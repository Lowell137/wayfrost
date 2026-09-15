use anyhow::Result;
use gtk4 as gtk;
use gtk4::cairo;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DrawingArea, EventControllerKey, GestureDrag, Image, Label,
    Orientation, Overlay, Popover, Separator,
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
    let current_ocr_text = Rc::new(RefCell::new(String::new()));

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

                // Live translucent blue text selection highlight (Windows 11 Snipping Tool look)
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

    // --- Helper function: Copy current recognized text and close ---
    let copy_selected_text = {
        let current_ocr_text = Rc::clone(&current_ocr_text);
        let window_weak = window.downgrade();

        move || {
            let text = current_ocr_text.borrow().trim().to_string();
            if !text.is_empty() {
                let _ = clipboard::copy_to_clipboard(&text);
                let preview = if text.len() > 60 {
                    format!("{}...", &text[..60])
                } else {
                    text
                };
                clipboard::send_notification("Wayfrost — Kopyalandı", &preview);

                if let Some(win) = window_weak.upgrade() {
                    win.close();
                }
            }
        }
    };

    // --- Helper function: Copy all screen text and close ---
    let copy_all_text = {
        let screen_img = screen_img.clone();
        let ocr_engine = Rc::clone(&ocr_engine);
        let active_lang = Rc::clone(&active_lang);
        let window_weak = window.downgrade();

        move || {
            let Some(ref img) = screen_img else { return; };
            if ocr_engine.borrow().is_none() {
                if let Ok(paths) = model_manager::ensure_models() {
                    if let Ok(engine) = OcrEngine::new(&paths.rec_model, &paths.dict_file) {
                        *ocr_engine.borrow_mut() = Some(engine);
                    }
                }
            }
            let lang = active_lang.borrow().clone();
            let mut engine_ref = ocr_engine.borrow_mut();
            if let Some(ref mut engine) = *engine_ref {
                if let Ok(text) = engine.recognize(img.as_ref(), &lang) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let _ = clipboard::copy_to_clipboard(trimmed);
                        clipboard::send_notification("Wayfrost — Tüm Metin Kopyalandı", trimmed);
                        if let Some(win) = window_weak.upgrade() {
                            win.close();
                        }
                    }
                }
            }
        }
    };

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

    // --- Windows 11 Snipping Tool Style Floating Context Menu (Anchored to Selection) ---
    let context_menu = Box::new(Orientation::Vertical, 2);
    context_menu.add_css_class("win11-context-menu");

    let menu_preview = Label::new(None);
    menu_preview.add_css_class("win11-menu-preview");
    menu_preview.set_halign(Align::Start);
    menu_preview.set_max_width_chars(32);
    menu_preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    menu_preview.set_visible(false);
    context_menu.append(&menu_preview);

    let btn_copy_menu = create_menu_item("edit-copy-symbolic", "Metni Kopyala", "Ctrl+C");
    {
        let copy_fn = copy_selected_text.clone();
        btn_copy_menu.connect_clicked(move |_| {
            copy_fn();
        });
    }

    let btn_select_all_menu = create_menu_item("edit-select-all-symbolic", "Tümünü Kopyala", "Ctrl+A");
    {
        let copy_all_fn = copy_all_text.clone();
        btn_select_all_menu.connect_clicked(move |_| {
            copy_all_fn();
        });
    }

    let btn_close_menu = create_menu_item("window-close-symbolic", "Kapat", "Esc");
    {
        let window_weak = window.downgrade();
        btn_close_menu.connect_clicked(move |_| {
            if let Some(win) = window_weak.upgrade() {
                win.close();
            }
        });
    }

    context_menu.append(&btn_copy_menu);
    context_menu.append(&btn_select_all_menu);
    context_menu.append(&btn_close_menu);

    let menu_overlay_box = Box::new(Orientation::Vertical, 0);
    menu_overlay_box.set_valign(Align::Start);
    menu_overlay_box.set_halign(Align::Start);
    menu_overlay_box.set_visible(false);
    menu_overlay_box.append(&context_menu);
    root_overlay.add_overlay(&menu_overlay_box);

    // --- Mouse Drag Gestures for Live Screen Selection ---
    let gesture_drag = GestureDrag::new();
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let menu_overlay_box = menu_overlay_box.clone();

        gesture_drag.connect_drag_begin(move |_, x, y| {
            menu_overlay_box.set_visible(false);
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
        let menu_overlay_box = menu_overlay_box.clone();
        let menu_preview = menu_preview.clone();
        let current_ocr_text = Rc::clone(&current_ocr_text);
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
            };

            if has_selection {
                if let Some((sx, sy, sw, sh)) = rect {
                    let win_w = da.width() as f64;
                    let win_h = da.height() as f64;
                    let menu_w = 240.0;
                    let menu_h = 125.0;

                    // Position directly at the bottom-right corner of the selection
                    let pos_x = if sx + sw + menu_w + 12.0 <= win_w {
                        sx + sw + 4.0
                    } else if sx + sw - menu_w >= 12.0 {
                        sx + sw - menu_w
                    } else {
                        sx.clamp(12.0, (win_w - menu_w - 12.0).max(12.0))
                    };

                    let pos_y = if sy + sh + menu_h + 12.0 <= win_h {
                        sy + sh + 6.0
                    } else if sy - menu_h - 6.0 >= 12.0 {
                        sy - menu_h - 6.0
                    } else {
                        (sy + sh - menu_h).clamp(12.0, (win_h - menu_h - 12.0).max(12.0))
                    };

                    menu_overlay_box.set_margin_start(pos_x as i32);
                    menu_overlay_box.set_margin_top(pos_y as i32);
                    menu_overlay_box.set_visible(true);
                }

                match execute_ocr() {
                    Ok(text) => {
                        let trimmed = text.trim().to_string();
                        if !trimmed.is_empty() {
                            let preview = if trimmed.len() > 30 {
                                format!("\"{}...\"", &trimmed[..28])
                            } else {
                                format!("\"{}\"", trimmed)
                            };
                            menu_preview.set_text(&preview);
                            menu_preview.set_visible(true);
                        } else {
                            menu_preview.set_text("Metin bulunamadı");
                            menu_preview.set_visible(true);
                        }
                        *current_ocr_text.borrow_mut() = trimmed;
                    }
                    Err(e) => {
                        log::error!("OCR error: {e}");
                    }
                }
            }
        });
    }
    drawing_area.add_controller(gesture_drag);

    // --- Top Center Floating Bar (Windows 11 Snipping Tool Style) ---
    let topbar = Box::new(Orientation::Horizontal, 8);
    topbar.add_css_class("win11-topbar");
    topbar.set_halign(Align::Center);

    // 1. Copy All Text button with icon + label
    let btn_copy_all = Button::builder()
        .icon_name("edit-copy-symbolic")
        .label("Tüm Metni Kopyala")
        .tooltip_text("Ekrandaki tüm metni panoya kopyalar (Ctrl+A)")
        .build();
    btn_copy_all.add_css_class("win11-topbar-btn");
    {
        let copy_all_fn = copy_all_text.clone();
        btn_copy_all.connect_clicked(move |_| {
            copy_all_fn();
        });
    }

    // 2. Language Switcher (Popover: TR / EN)
    let lang_button = Button::new();
    lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: Türkçe - TR)"));
    lang_button.add_css_class("win11-topbar-btn");

    let lang_box = Box::new(Orientation::Horizontal, 4);
    let lang_icon = Image::from_icon_name("preferences-desktop-locale-symbolic");
    lang_icon.set_pixel_size(16);
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

    // Separator
    let separator = Separator::new(Orientation::Vertical);
    separator.add_css_class("topbar-separator");

    // Close button
    let btn_close = Button::from_icon_name("window-close-symbolic");
    btn_close.set_tooltip_text(Some("Kapat (Esc)"));
    btn_close.add_css_class("win11-topbar-btn");
    btn_close.add_css_class("destructive");
    {
        let window_weak = window.downgrade();
        btn_close.connect_clicked(move |_| {
            if let Some(win) = window_weak.upgrade() {
                win.close();
            }
        });
    }

    // Pack into topbar
    topbar.append(&btn_copy_all);
    topbar.append(&lang_button);
    topbar.append(&separator);
    topbar.append(&btn_close);

    let top_box = Box::new(Orientation::Vertical, 0);
    top_box.set_valign(Align::Start);
    top_box.set_halign(Align::Center);
    top_box.set_margin_top(18);
    top_box.append(&topbar);

    root_overlay.add_overlay(&top_box);
    window.set_child(Some(&root_overlay));

    // --- CSS Styles ---
    let css_provider = CssProvider::new();
    css_provider.load_from_data(
        "
        window.overlay-window {
            background-color: black;
        }

        /* Windows 11 Snipping Tool Top Bar */
        .win11-topbar {
            background: rgba(30, 30, 34, 0.94);
            border: 1px solid rgba(255, 255, 255, 0.16);
            border-radius: 9999px;
            padding: 5px 10px;
            box-shadow: 0 16px 40px rgba(0, 0, 0, 0.65);
            backdrop-filter: blur(24px);
        }

        .win11-topbar-btn {
            background: transparent;
            border: none;
            border-radius: 9999px;
            padding: 6px 14px;
            color: #f2f2f7;
            font-size: 13px;
            font-weight: 500;
            transition: background-color 120ms ease;
        }

        .win11-topbar-btn:hover {
            background: rgba(255, 255, 255, 0.14);
            color: #ffffff;
        }

        .win11-topbar-btn.destructive:hover {
            background: rgba(239, 68, 68, 0.28);
            color: #fca5a5;
        }

        .lang-badge {
            font-size: 11px;
            font-weight: 700;
            color: #64b5f6;
            margin-left: 2px;
        }

        .topbar-separator {
            background-color: rgba(255, 255, 255, 0.16);
            margin: 4px 2px;
            min-width: 1px;
        }

        /* Windows 11 Snipping Tool Floating Context Menu */
        .win11-context-menu {
            background: rgba(32, 32, 36, 0.96);
            border: 1px solid rgba(255, 255, 255, 0.16);
            border-radius: 8px;
            padding: 5px;
            box-shadow: 0 16px 44px rgba(0, 0, 0, 0.75), 0 0 0 1px rgba(255, 255, 255, 0.06);
            backdrop-filter: blur(24px);
            min-width: 220px;
        }

        .win11-menu-preview {
            color: #93c5fd;
            font-size: 11px;
            font-weight: 500;
            padding: 4px 10px 6px 10px;
            border-bottom: 1px solid rgba(255, 255, 255, 0.10);
            margin-bottom: 3px;
        }

        .win11-menu-item {
            background: transparent;
            border: none;
            border-radius: 6px;
            padding: 7px 12px;
            color: #f2f2f7;
            font-size: 13px;
            font-weight: 500;
            transition: background-color 100ms ease;
        }

        .win11-menu-item:hover,
        .win11-menu-item:focus {
            background: rgba(255, 255, 255, 0.14);
            color: #ffffff;
        }

        .win11-shortcut {
            color: #9e9ea6;
            font-size: 11px;
            font-weight: 400;
            margin-left: 18px;
        }

        .lang-popover contents {
            background: rgba(30, 30, 32, 0.96);
            border: 1px solid rgba(255, 255, 255, 0.12);
            border-radius: 12px;
            box-shadow: 0 12px 32px rgba(0, 0, 0, 0.6);
            padding: 4px;
        }

        .popover-item {
            background: transparent;
            border: none;
            border-radius: 6px;
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

    // --- Key Controller: Escape = close, Ctrl+C / Enter = copy selected, Ctrl+A = copy all ---
    let key_controller = EventControllerKey::new();
    {
        let window_weak = window.downgrade();
        let copy_selected = copy_selected_text.clone();
        let copy_all = copy_all_text.clone();

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
                copy_selected();
                return glib::Propagation::Stop;
            }

            // Ctrl+A: copy all screen text
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && (keyval == gdk::Key::a || keyval == gdk::Key::A)
            {
                copy_all();
                return glib::Propagation::Stop;
            }

            // Enter: copy selected text
            if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
                copy_selected();
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

fn create_menu_item(icon_name: &str, label_text: &str, shortcut_text: &str) -> Button {
    let btn = Button::new();
    btn.add_css_class("win11-menu-item");

    let row = Box::new(Orientation::Horizontal, 8);
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);

    let label = Label::new(Some(label_text));
    label.set_hexpand(true);
    label.set_halign(Align::Start);

    let shortcut = Label::new(Some(shortcut_text));
    shortcut.add_css_class("win11-shortcut");
    shortcut.set_halign(Align::End);

    row.append(&icon);
    row.append(&label);
    row.append(&shortcut);

    btn.set_child(Some(&row));
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
