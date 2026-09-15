use anyhow::Result;
use gtk4 as gtk;
use gtk4::cairo;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DrawingArea, EventControllerKey, GestureDrag, Image, Label,
    Orientation, Overlay, Popover, ScrolledWindow, Separator, TextView, WrapMode,
};
use image::{imageops, DynamicImage, GenericImageView};
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;

use crate::capture;
use crate::clipboard;
use crate::ocr::model_manager;
use crate::ocr::pipeline::OcrEngine;

#[derive(Default, Clone, Copy, Debug)]
pub struct SelectionState {
    pub start_x: f64,
    pub start_y: f64,
    pub current_x: f64,
    pub current_y: f64,
    pub active: bool,
    pub completed: bool,
}

impl SelectionState {
    pub fn normalized(&self) -> Option<(f64, f64, f64, f64)> {
        if !self.active && !self.completed {
            return None;
        }
        let x = self.start_x.min(self.current_x).max(0.0);
        let y = self.start_y.min(self.current_y).max(0.0);
        let w = (self.start_x - self.current_x).abs();
        let h = (self.start_y - self.current_y).abs();
        if w >= 4.0 && h >= 4.0 {
            Some((x, y, w, h))
        } else {
            None
        }
    }
}

pub fn build_overlay_window(app: &adw::Application) {
    // 1. Capture screen at startup
    let screen_img = match capture::capture_screen() {
        Ok(img) => Some(Rc::new(img)),
        Err(e) => {
            log::warn!("Could not capture screen at startup: {e}");
            None
        }
    };

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Wayfrost")
        .decorated(false)
        .resizable(false)
        .build();

    window.add_css_class("overlay-window");
    window.set_cursor_from_name(Some("crosshair"));

    // Convert screenshot to Cairo ImageSurface for exact 1:1 painting
    let background_surface = screen_img.as_ref().and_then(|img| {
        image_to_cairo_surface(img).ok().map(Rc::new)
    });

    let ocr_engine: Rc<RefCell<Option<OcrEngine>>> = Rc::new(RefCell::new(None));
    let selection = Rc::new(RefCell::new(SelectionState::default()));
    let active_lang = Rc::new(RefCell::new("TR".to_string()));

    let root_overlay = Overlay::new();

    // Fullscreen DrawingArea: paints background image 1:1 + dim scrim + live selection highlight
    let drawing_area = DrawingArea::new();
    drawing_area.set_can_target(true);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);

    {
        let selection = Rc::clone(&selection);
        let surface = background_surface.clone();

        drawing_area.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;

            // 1. Paint background screenshot 1:1
            if let Some(ref surf) = surface {
                let surf_w = surf.width() as f64;
                let surf_h = surf.height() as f64;
                let scale_x = w / surf_w.max(1.0);
                let scale_y = h / surf_h.max(1.0);

                let _ = cr.save();
                cr.scale(scale_x, scale_y);
                let _ = cr.set_source_surface(&**surf, 0.0, 0.0);
                let _ = cr.paint();
                let _ = cr.restore();
            } else {
                cr.set_source_rgb(0.08, 0.08, 0.1);
                cr.rectangle(0.0, 0.0, w, h);
                let _ = cr.fill();
            }

            let s = selection.borrow();
            if let Some((sx, sy, sw, sh)) = s.normalized() {
                // Dim 4 unselected regions around the box
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);

                // Top
                cr.rectangle(0.0, 0.0, w, sy);
                let _ = cr.fill();
                // Bottom
                cr.rectangle(0.0, sy + sh, w, (h - (sy + sh)).max(0.0));
                let _ = cr.fill();
                // Left
                cr.rectangle(0.0, sy, sx, sh);
                let _ = cr.fill();
                // Right
                cr.rectangle(sx + sw, sy, (w - (sx + sw)).max(0.0), sh);
                let _ = cr.fill();

                // Live translucent blue text selection highlight
                cr.set_source_rgba(0.0, 0.47, 0.84, 0.35);
                cr.rectangle(sx, sy, sw, sh);
                let _ = cr.fill();

                // Crisp border
                cr.set_source_rgba(0.24, 0.60, 0.98, 0.95);
                cr.set_line_width(2.0);
                cr.rectangle(sx, sy, sw, sh);
                let _ = cr.stroke();
            } else {
                // Subtle scrim before any selection
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
                cr.rectangle(0.0, 0.0, w, h);
                let _ = cr.fill();
            }
        });
    }
    root_overlay.set_child(Some(&drawing_area));

    // --- In-Place Selectable Text Box (Appears directly on the selected coordinates) ---
    let in_place_textview = TextView::new();
    in_place_textview.set_wrap_mode(WrapMode::Word);
    in_place_textview.set_editable(false);
    in_place_textview.set_cursor_visible(true);
    in_place_textview.add_css_class("in-place-textview");

    let in_place_scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .max_content_height(500)
        .child(&in_place_textview)
        .build();

    let in_place_box = Box::new(Orientation::Vertical, 0);
    in_place_box.add_css_class("in-place-box");
    in_place_box.set_halign(Align::Start);
    in_place_box.set_valign(Align::Start);
    in_place_box.set_visible(false);
    in_place_box.append(&in_place_scrolled);
    root_overlay.add_overlay(&in_place_box);

    // --- Helper function: Execute OCR on selected coordinates ---
    let execute_ocr = {
        let screen_img = screen_img.clone();
        let selection = Rc::clone(&selection);
        let ocr_engine = Rc::clone(&ocr_engine);
        let active_lang = Rc::clone(&active_lang);
        let da = drawing_area.clone();

        move || -> Result<String> {
            let Some(ref img) = screen_img else {
                anyhow::bail!("No screen image captured");
            };

            if ocr_engine.borrow().is_none() {
                let paths = model_manager::ensure_models()?;
                let engine = OcrEngine::new(&paths.rec_model, &paths.dict_file)?;
                *ocr_engine.borrow_mut() = Some(engine);
            }

            let (img_w, img_h) = img.dimensions();
            let da_w = da.width() as f64;
            let da_h = da.height() as f64;

            let (sel_x, sel_y, sel_w, sel_h) = if let Some(rect) = selection.borrow().normalized() {
                rect
            } else {
                (0.0, 0.0, da_w.max(1.0), da_h.max(1.0))
            };

            let scale_x = img_w as f64 / da_w.max(1.0);
            let scale_y = img_h as f64 / da_h.max(1.0);

            let crop_x = ((sel_x * scale_x).round() as u32).min(img_w.saturating_sub(1));
            let crop_y = ((sel_y * scale_y).round() as u32).min(img_h.saturating_sub(1));
            let crop_w = ((sel_w * scale_x).round() as u32).min(img_w - crop_x).max(1);
            let crop_h = ((sel_h * scale_y).round() as u32).min(img_h - crop_y).max(1);

            let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
            let lang = active_lang.borrow().clone();

            let mut engine_ref = ocr_engine.borrow_mut();
            let engine = engine_ref.as_mut().unwrap();
            let text = engine.recognize(&DynamicImage::ImageRgba8(crop), &lang)?;
            Ok(text)
        }
    };

    // --- Helper function: Copy text (only selected words, or all) and finish ---
    let copy_selection_and_finish = {
        let text_view = in_place_textview.clone();
        let window_weak = window.downgrade();

        move || {
            let buffer = text_view.buffer();
            let text_to_copy = if buffer.has_selection() {
                let (start, end) = buffer.selection_bounds().unwrap();
                buffer.text(&start, &end, true).to_string()
            } else {
                let start = buffer.start_iter();
                let end = buffer.end_iter();
                buffer.text(&start, &end, true).to_string()
            };

            let trimmed = text_to_copy.trim();
            if !trimmed.is_empty() {
                let _ = clipboard::copy_to_clipboard(trimmed);
                let preview = if trimmed.len() > 60 {
                    format!("{}...", &trimmed[..60])
                } else {
                    trimmed.to_string()
                };
                clipboard::send_notification("Wayfrost — Kopyalandı", &preview);

                if let Some(win) = window_weak.upgrade() {
                    win.close();
                }
            } else {
                clipboard::send_notification("Wayfrost", "Kopyalanacak metin bulunamadı");
            }
        }
    };

    // Connect Enter/Ctrl+C key on the in-place text view
    {
        let tv_key = EventControllerKey::new();
        let copy_fn = copy_selection_and_finish.clone();
        tv_key.connect_key_pressed(move |_, keyval, _, state| {
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && (keyval == gdk::Key::c || keyval == gdk::Key::C)
            {
                copy_fn();
                return glib::Propagation::Stop;
            }
            if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
                copy_fn();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        in_place_textview.add_controller(tv_key);
    }

    // --- Mouse Drag Gestures for Live Screen Selection ---
    let gesture_drag = GestureDrag::new();
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let in_place_box = in_place_box.clone();

        gesture_drag.connect_drag_begin(move |_, x, y| {
            in_place_box.set_visible(false);
            let mut s = selection.borrow_mut();
            s.start_x = x;
            s.start_y = y;
            s.current_x = x;
            s.current_y = y;
            s.active = true;
            s.completed = false;
            da.queue_draw();
        });
    }

    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();

        gesture_drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let mut s = selection.borrow_mut();
            if let Some((start_x, start_y)) = gesture.start_point() {
                s.current_x = start_x + offset_x;
                s.current_y = start_y + offset_y;
                da.queue_draw();
            }
        });
    }

    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let in_place_box = in_place_box.clone();
        let in_place_textview = in_place_textview.clone();
        let execute_ocr = execute_ocr.clone();

        gesture_drag.connect_drag_end(move |gesture, offset_x, offset_y| {
            let (has_selection, rect) = {
                let mut s = selection.borrow_mut();
                if let Some((start_x, start_y)) = gesture.start_point() {
                    s.current_x = start_x + offset_x;
                    s.current_y = start_y + offset_y;
                    s.active = false;
                    s.completed = true;
                    da.queue_draw();
                }
                (s.normalized().is_some(), s.normalized())
            }; // <-- mutable borrow dropped here

            if has_selection {
                if let Some((sx, sy, sw, _sh)) = rect {
                    if let Ok(text) = execute_ocr() {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let buffer = in_place_textview.buffer();
                            buffer.set_text(trimmed);

                            // Set position & size right over the selected area!
                            in_place_box.set_margin_start(sx as i32);
                            in_place_box.set_margin_top(sy as i32);
                            let target_w = (sw as i32).max(180);
                            in_place_box.set_size_request(target_w, -1);

                            in_place_box.set_visible(true);
                            in_place_textview.grab_focus();
                        } else {
                            clipboard::send_notification("Wayfrost", "Seçilen alanda metin bulunamadı");
                        }
                    }
                }
            }
        });
    }
    drawing_area.add_controller(gesture_drag);

    // --- Bottom Floating Pill Bar ---
    let action_bar = Box::new(Orientation::Horizontal, 6);
    action_bar.add_css_class("floating-pill");
    action_bar.set_halign(Align::Center);

    // 1. Copy Selection button
    let btn_copy_bar = create_symbolic_button("edit-copy-symbolic", "Seçilen Metni Kopyala (Ctrl+C / Enter)");
    {
        let copy_fn = copy_selection_and_finish.clone();
        btn_copy_bar.connect_clicked(move |_| {
            copy_fn();
        });
    }

    // 2. Select All Button
    let btn_select_all = create_symbolic_button("edit-select-all-symbolic", "Tüm Ekranı Seç");
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let in_place_box = in_place_box.clone();
        let in_place_textview = in_place_textview.clone();
        let execute_ocr = execute_ocr.clone();

        btn_select_all.connect_clicked(move |_| {
            {
                let mut s = selection.borrow_mut();
                s.start_x = 0.0;
                s.start_y = 0.0;
                s.current_x = da.width() as f64;
                s.current_y = da.height() as f64;
                s.active = false;
                s.completed = true;
            }
            da.queue_draw();

            if let Ok(text) = execute_ocr() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let buffer = in_place_textview.buffer();
                    buffer.set_text(trimmed);
                    in_place_box.set_margin_start(40);
                    in_place_box.set_margin_top(40);
                    in_place_box.set_size_request((da.width() - 80).max(200), (da.height() - 140).max(100));
                    in_place_box.set_visible(true);
                    in_place_textview.grab_focus();
                }
            }
        });
    }

    // 3. Language Switcher (Popover: TR / EN)
    let lang_button = Button::new();
    lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: Türkçe - TR)"));
    lang_button.add_css_class("pill-btn");

    let lang_box = Box::new(Orientation::Horizontal, 4);
    let lang_icon = Image::from_icon_name("preferences-desktop-locale-symbolic");
    lang_icon.set_pixel_size(18);
    let lang_label = Label::new(Some("TR"));
    lang_label.add_css_class("lang-badge");
    lang_box.append(&lang_icon);
    lang_box.append(&lang_label);
    lang_button.set_child(Some(&lang_box));

    let popover = Popover::new();
    popover.add_css_class("lang-popover");
    popover.set_parent(&lang_button);

    let popover_clone = popover.clone();
    lang_button.connect_clicked(move |_| {
        popover_clone.popup();
    });

    let popover_vbox = Box::new(Orientation::Vertical, 4);
    popover_vbox.set_margin_top(6);
    popover_vbox.set_margin_bottom(6);
    popover_vbox.set_margin_start(6);
    popover_vbox.set_margin_end(6);

    let btn_tr = Button::with_label("🇹🇷  Türkçe (TR)");
    btn_tr.add_css_class("popover-item");
    let btn_en = Button::with_label("🇬🇧  English (EN)");
    btn_en.add_css_class("popover-item");

    {
        let lang_label = lang_label.clone();
        let lang_button = lang_button.clone();
        let active_lang = Rc::clone(&active_lang);
        let popover = popover.clone();
        btn_tr.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "TR".to_string();
            lang_label.set_text("TR");
            lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: Türkçe - TR)"));
            popover.popdown();
        });
    }

    {
        let lang_label = lang_label.clone();
        let lang_button = lang_button.clone();
        let active_lang = Rc::clone(&active_lang);
        let popover = popover.clone();
        btn_en.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "EN".to_string();
            lang_label.set_text("EN");
            lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: English - EN)"));
            popover.popdown();
        });
    }

    popover_vbox.append(&btn_tr);
    popover_vbox.append(&btn_en);
    popover.set_child(Some(&popover_vbox));

    // Separator before close
    let separator = Separator::new(Orientation::Vertical);
    separator.add_css_class("pill-separator");

    // Close button
    let btn_close = create_symbolic_button("window-close-symbolic", "Kapat (Esc)");
    btn_close.add_css_class("destructive");
    {
        let window_weak = window.downgrade();
        btn_close.connect_clicked(move |_| {
            if let Some(win) = window_weak.upgrade() {
                win.close();
            }
        });
    }

    // Pack into bottom pill bar
    action_bar.append(&btn_copy_bar);
    action_bar.append(&btn_select_all);
    action_bar.append(&lang_button);
    action_bar.append(&separator);
    action_bar.append(&btn_close);

    let bar_clamp = adw::Clamp::builder()
        .maximum_size(480)
        .tightening_threshold(380)
        .child(&action_bar)
        .build();

    let bottom_box = Box::new(Orientation::Vertical, 0);
    bottom_box.set_valign(Align::End);
    bottom_box.set_halign(Align::Fill);
    bottom_box.set_margin_bottom(28);
    bottom_box.append(&bar_clamp);

    root_overlay.add_overlay(&bottom_box);
    window.set_child(Some(&root_overlay));

    // --- CSS Styles ---
    let css_provider = CssProvider::new();
    css_provider.load_from_data(
        "
        window.overlay-window {
            background-color: black;
        }

        /* In-place text box right over the selection */
        .in-place-box {
            background: rgba(18, 18, 22, 0.90);
            border: 2px solid #3584e4;
            border-radius: 6px;
            box-shadow: 0 8px 32px rgba(0, 0, 0, 0.7);
            backdrop-filter: blur(12px);
        }

        .in-place-textview {
            background: transparent;
            color: #ffffff;
            font-size: 14px;
            font-weight: 500;
            line-height: 1.4;
            padding: 4px 8px;
        }

        .in-place-textview:focus {
            outline: none;
        }

        .floating-pill {
            background: rgba(22, 22, 24, 0.92);
            border: 1px solid rgba(255, 255, 255, 0.16);
            border-radius: 9999px;
            padding: 6px 10px;
            box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7);
            backdrop-filter: blur(24px);
        }

        .floating-pill button,
        .floating-pill .pill-btn {
            background: transparent;
            border: none;
            border-radius: 9999px;
            min-width: 40px;
            min-height: 40px;
            padding: 6px 10px;
            color: #f2f2f7;
            font-size: 13px;
            font-weight: 600;
            transition: background-color 150ms ease, transform 100ms ease;
        }

        .floating-pill button:hover,
        .floating-pill .pill-btn:hover {
            background: rgba(255, 255, 255, 0.14);
        }

        .floating-pill button:active,
        .floating-pill .pill-btn:active {
            background: rgba(255, 255, 255, 0.24);
            transform: scale(0.96);
        }

        .lang-badge {
            font-size: 11px;
            font-weight: 700;
            color: #64b5f6;
            margin-left: 2px;
        }

        .pill-separator {
            background-color: rgba(255, 255, 255, 0.16);
            margin: 6px 4px;
            min-width: 1px;
        }

        .floating-pill button.destructive:hover {
            background: rgba(239, 68, 68, 0.28);
            color: #fca5a5;
        }

        .lang-popover contents {
            background: rgba(30, 30, 32, 0.96);
            border: 1px solid rgba(255, 255, 255, 0.12);
            border-radius: 14px;
            box-shadow: 0 12px 32px rgba(0, 0, 0, 0.6);
            padding: 4px;
        }

        .popover-item {
            background: transparent;
            border: none;
            border-radius: 8px;
            padding: 8px 14px;
            color: #f2f2f7;
            font-size: 13px;
            font-weight: 500;
            text-align: left;
        }

        .popover-item:hover {
            background: rgba(255, 255, 255, 0.12);
        }
        ",
    );

    gtk4::style_context_add_provider_for_display(
        &gtk4::prelude::RootExt::display(&window),
        &css_provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // --- Global Key Controller: Escape = close, Enter / Ctrl+C = copy and exit ---
    let key_controller = EventControllerKey::new();
    {
        let window_weak = window.downgrade();
        let copy_fn = copy_selection_and_finish.clone();

        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == gdk::Key::Escape {
                if let Some(win) = window_weak.upgrade() {
                    win.close();
                    return glib::Propagation::Stop;
                }
            }

            // Ctrl+C: copy selected text
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && (keyval == gdk::Key::c || keyval == gdk::Key::C)
            {
                copy_fn();
                return glib::Propagation::Stop;
            }

            // Enter: copy selected text
            if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
                copy_fn();
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
    }
    window.add_controller(key_controller);

    window.set_default_size(1920, 1080);
    window.fullscreen();
    window.present();

    {
        let window_weak = window.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(win) = window_weak.upgrade() {
                win.fullscreen();
            }
        });
    }
}

fn create_symbolic_button(icon_name: &str, tooltip: &str) -> Button {
    let btn = Button::new();
    btn.add_css_class("pill-btn");
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(18);
    btn.set_child(Some(&icon));
    btn.set_tooltip_text(Some(tooltip));
    btn
}

fn image_to_cairo_surface(img: &DynamicImage) -> Result<cairo::ImageSurface> {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();
    let mut surface = cairo::ImageSurface::create(cairo::Format::Rgb24, w as i32, h as i32)
        .map_err(|e| anyhow::anyhow!("Cairo surface error: {:?}", e))?;

    {
        let mut data = surface.data().map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let raw = rgba.as_raw();
        for (dst, src) in data.chunks_exact_mut(4).zip(raw.chunks_exact(4)) {
            dst[0] = src[2]; // Blue
            dst[1] = src[1]; // Green
            dst[2] = src[0]; // Red
            dst[3] = 255;    // Alpha
        }
    }
    surface.mark_dirty();
    Ok(surface)
}
