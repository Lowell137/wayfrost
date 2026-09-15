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
use std::sync::Arc;

use crate::capture;
use crate::clipboard;
use crate::ocr::pipeline::{self, DetectedWord};

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
        Ok(img) => Some(Arc::new(img)),
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

    let selection = Rc::new(RefCell::new(SelectionState::default()));
    let active_lang = Rc::new(RefCell::new("TR".to_string()));
    let cached_text: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Live Text: detected words with bounding boxes
    let all_words: Rc<RefCell<Vec<DetectedWord>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let root_overlay = Overlay::new();

    // Fullscreen DrawingArea: paints background image 1:1 + live word-level highlight
    let drawing_area = DrawingArea::new();
    drawing_area.set_can_target(true);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);

    {
        let selection = Rc::clone(&selection);
        let surface = background_surface.clone();
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);

        drawing_area.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;

            // 1. Paint original screenshot 1:1 with crystal clear quality
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
            let words = all_words.borrow();
            let selected = selected_indices.borrow();

            // 2. Dim background outside drag area while user is dragging
            if let Some((sx, sy, sw, sh)) = s.normalized() {
                if s.active {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
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

                    // Subtle white drag rectangle border
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.75);
                    cr.set_line_width(1.5);
                    cr.rectangle(sx, sy, sw, sh);
                    let _ = cr.stroke();
                }
            } else if selected.is_empty() {
                // Subtle scrim before any selection
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.20);
                cr.rectangle(0.0, 0.0, w, h);
                let _ = cr.fill();
            }

            // 3. Apple/Windows LIVE TEXT: highlight selected words directly on the image!
            for &idx in selected.iter() {
                if let Some(word) = words.get(idx) {
                    // Translucent blue highlighter over the exact word on the original image
                    cr.set_source_rgba(0.18, 0.52, 0.95, 0.40);
                    cr.rectangle(word.x - 1.0, word.y - 1.0, word.w + 2.0, word.h + 2.0);
                    let _ = cr.fill();

                    // Crisp subtle border
                    cr.set_source_rgba(0.28, 0.62, 0.98, 0.90);
                    cr.set_line_width(1.0);
                    cr.rectangle(word.x - 1.0, word.y - 1.0, word.w + 2.0, word.h + 2.0);
                    let _ = cr.stroke();
                }
            }
        });
    }
    root_overlay.set_child(Some(&drawing_area));

    // 2. Background task: extract word bounding boxes across full screen
    let (tx, rx) = std::sync::mpsc::channel::<Vec<DetectedWord>>();
    let rx = Rc::new(RefCell::new(rx));
    {
        let all_words = Rc::clone(&all_words);
        let da = drawing_area.clone();
        let screen_img = screen_img.clone();
        let rx = Rc::clone(&rx);

        glib::timeout_add_local(std::time::Duration::from_millis(25), move || {
            if let Ok(words) = rx.borrow_mut().try_recv() {
                let da_w = da.width() as f64;
                let da_h = da.height() as f64;
                let (img_w, img_h) = if let Some(ref img) = screen_img {
                    img.dimensions()
                } else {
                    (1920, 1080)
                };
                let scale_x = da_w / img_w.max(1) as f64;
                let scale_y = da_h / img_h.max(1) as f64;

                let mut scaled_words = words;
                if (scale_x - 1.0).abs() > 0.001 || (scale_y - 1.0).abs() > 0.001 {
                    for w in &mut scaled_words {
                        w.x *= scale_x;
                        w.y *= scale_y;
                        w.w *= scale_x;
                        w.h *= scale_y;
                    }
                }

                *all_words.borrow_mut() = scaled_words;
                da.queue_draw();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    if let Some(ref img_arc) = screen_img {
        let img = Arc::clone(img_arc);
        let lang = active_lang.borrow().clone();

        std::thread::spawn(move || {
            if let Ok(words) = pipeline::run_tesseract_tsv(&img, &lang) {
                let _ = tx.send(words);
            }
        });
    }

    // --- Helper function: Copy highlighted text and finish ---
    let copy_selection_and_finish = {
        let cached_text = Rc::clone(&cached_text);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let window_weak = window.downgrade();

        move || {
            let mut text = cached_text.borrow().clone().unwrap_or_default();
            if text.trim().is_empty() {
                let words = all_words.borrow();
                let selected = selected_indices.borrow();
                let sel_words: Vec<&DetectedWord> = selected.iter().filter_map(|&i| words.get(i)).collect();
                if !sel_words.is_empty() {
                    text = pipeline::join_words(&sel_words);
                }
            }

            let trimmed = text.trim();
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
                clipboard::send_notification("Wayfrost", "Seçilen alanda metin bulunamadı");
            }
        }
    };

    // --- Mouse Drag Gestures: Select text live directly on the screen ---
    let gesture_drag = GestureDrag::new();
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let cached_text = Rc::clone(&cached_text);
        let selected_indices = Rc::clone(&selected_indices);

        gesture_drag.connect_drag_begin(move |_, x, y| {
            *cached_text.borrow_mut() = None;
            *selected_indices.borrow_mut() = Vec::new();
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
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);

        gesture_drag.connect_drag_update(move |gesture, offset_x, offset_y| {
            let mut s = selection.borrow_mut();
            if let Some((start_x, start_y)) = gesture.start_point() {
                s.current_x = start_x + offset_x;
                s.current_y = start_y + offset_y;

                if let Some((sx, sy, sw, sh)) = s.normalized() {
                    let words = all_words.borrow();
                    let mut new_sel = Vec::new();
                    let (sx2, sy2) = (sx + sw, sy + sh);
                    for (i, w) in words.iter().enumerate() {
                        let (wx2, wy2) = (w.x + w.w, w.y + w.h);
                        if !(wx2 < sx || w.x > sx2 || wy2 < sy || w.y > sy2) {
                            new_sel.push(i);
                        }
                    }
                    *selected_indices.borrow_mut() = new_sel;
                }
                da.queue_draw();
            }
        });
    }

    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let screen_img = screen_img.clone();
        let active_lang = Rc::clone(&active_lang);

        gesture_drag.connect_drag_end(move |gesture, offset_x, offset_y| {
            let rect = {
                let mut s = selection.borrow_mut();
                if let Some((start_x, start_y)) = gesture.start_point() {
                    s.current_x = start_x + offset_x;
                    s.current_y = start_y + offset_y;
                    s.active = false;
                    s.completed = true;
                    da.queue_draw();
                }
                s.normalized()
            };

            if let Some((sx, sy, sw, sh)) = rect {
                let words_empty = all_words.borrow().is_empty();

                // If full screen words are not ready yet, run quick crop TSV in 30ms!
                if words_empty {
                    if let Some(ref img) = screen_img {
                        let (img_w, img_h) = img.dimensions();
                        let da_w = da.width() as f64;
                        let da_h = da.height() as f64;
                        let scale_x = img_w as f64 / da_w.max(1.0);
                        let scale_y = img_h as f64 / da_h.max(1.0);

                        let crop_x = ((sx * scale_x).round() as u32).min(img_w.saturating_sub(1));
                        let crop_y = ((sy * scale_y).round() as u32).min(img_h.saturating_sub(1));
                        let crop_w = ((sw * scale_x).round() as u32).min(img_w - crop_x).max(1);
                        let crop_h = ((sh * scale_y).round() as u32).min(img_h - crop_y).max(1);

                        let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
                        let lang = active_lang.borrow().clone();

                        if let Ok(mut crop_words) = pipeline::run_tesseract_tsv(&DynamicImage::ImageRgba8(crop), &lang) {
                            for w in &mut crop_words {
                                w.x = (w.x + crop_x as f64) / scale_x;
                                w.y = (w.y + crop_y as f64) / scale_y;
                                w.w = w.w / scale_x;
                                w.h = w.h / scale_y;
                            }
                            let mut words = all_words.borrow_mut();
                            let start_idx = words.len();
                            let count = crop_words.len();
                            words.extend(crop_words);
                            *selected_indices.borrow_mut() = (start_idx..start_idx + count).collect();
                            da.queue_draw();
                        }
                    }
                }

                // Cache the joined string of the selected words
                let words = all_words.borrow();
                let selected = selected_indices.borrow();
                let sel_words: Vec<&DetectedWord> = selected.iter().filter_map(|&i| words.get(i)).collect();
                if !sel_words.is_empty() {
                    *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
                }
            }
        });
    }
    drawing_area.add_controller(gesture_drag);

    // --- Bottom Floating Pill Bar ---
    let action_bar = Box::new(Orientation::Horizontal, 6);
    action_bar.add_css_class("floating-pill");
    action_bar.set_halign(Align::Center);

    // 1. Copy Highlighted Text button
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
        let da = drawing_area.clone();
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);

        btn_select_all.connect_clicked(move |_| {
            let words = all_words.borrow();
            let all_idx: Vec<usize> = (0..words.len()).collect();
            let sel_words: Vec<&DetectedWord> = words.iter().collect();
            if !sel_words.is_empty() {
                *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
            }
            drop(words);
            *selected_indices.borrow_mut() = all_idx;
            da.queue_draw();
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
        let cached_text = Rc::clone(&cached_text);
        let popover = popover.clone();
        btn_tr.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "TR".to_string();
            *cached_text.borrow_mut() = None;
            lang_label.set_text("TR");
            lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: Türkçe - TR)"));
            popover.popdown();
        });
    }

    {
        let lang_label = lang_label.clone();
        let lang_button = lang_button.clone();
        let active_lang = Rc::clone(&active_lang);
        let cached_text = Rc::clone(&cached_text);
        let popover = popover.clone();
        btn_en.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "EN".to_string();
            *cached_text.borrow_mut() = None;
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

    // --- Key Controller: Escape = close, Ctrl+C / Enter = copy and exit ---
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
