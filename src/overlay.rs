use anyhow::Result;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Box, Button, CssProvider, DrawingArea, EventControllerKey, GestureDrag, Image,
    Orientation, Overlay, Picture, Popover, ScrolledWindow, Separator, TextView,
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
        let x = self.start_x.min(self.current_x);
        let y = self.start_y.min(self.current_y);
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
    // 1. Capture screen
    let screen_img = match capture::capture_screen() {
        Ok(img) => Some(Rc::new(img)),
        Err(e) => {
            log::warn!("Could not capture screen at startup: {e}");
            None
        }
    };

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Wayfrost Overlay")
        .decorated(false)
        .resizable(false)
        .build();

    window.add_css_class("overlay-window");
    window.set_default_size(1024, 768);
    window.fullscreen();

    // OCR Engine holder
    let ocr_engine: Rc<RefCell<Option<OcrEngine>>> = Rc::new(RefCell::new(None));

    // Selection state
    let selection = Rc::new(RefCell::new(SelectionState::default()));

    // Overlay root container
    let root_overlay = Overlay::new();

    // Background picture (frozen screenshot)
    if let Some(ref img) = screen_img {
        let rgba = img.to_rgba8();
        let width = rgba.width() as i32;
        let height = rgba.height() as i32;
        let stride = (width * 4) as usize;
        let bytes = glib::Bytes::from(rgba.as_raw());
        let texture = gdk::MemoryTexture::new(
            width,
            height,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            stride,
        );
        let picture = Picture::for_paintable(&texture);
        picture.set_can_shrink(true);
        root_overlay.set_child(Some(&picture));
    } else {
        let fallback_box = Box::new(Orientation::Vertical, 0);
        fallback_box.add_css_class("dark-fallback");
        root_overlay.set_child(Some(&fallback_box));
    }

    // Drawing area for selection rectangle and scrim
    let drawing_area = DrawingArea::new();
    drawing_area.set_can_target(true);

    {
        let selection = Rc::clone(&selection);
        drawing_area.set_draw_func(move |_, cr, width, height| {
            let s = selection.borrow();
            let w = width as f64;
            let h = height as f64;

            if let Some((sx, sy, sw, sh)) = s.normalized() {
                // Dim 4 regions around the selection
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);

                // Top
                cr.rectangle(0.0, 0.0, w, sy);
                cr.fill().unwrap();
                // Bottom
                cr.rectangle(0.0, sy + sh, w, (h - (sy + sh)).max(0.0));
                cr.fill().unwrap();
                // Left
                cr.rectangle(0.0, sy, sx, sh);
                cr.fill().unwrap();
                // Right
                cr.rectangle(sx + sw, sy, (w - (sx + sw)).max(0.0), sh);
                cr.fill().unwrap();

                // Highlight inside selection
                cr.set_source_rgba(0.2, 0.6, 1.0, 0.08);
                cr.rectangle(sx, sy, sw, sh);
                cr.fill().unwrap();

                // Selection border
                cr.set_source_rgba(0.25, 0.65, 1.0, 0.95);
                cr.set_line_width(2.0);
                cr.rectangle(sx, sy, sw, sh);
                cr.stroke().unwrap();
            } else {
                // Entire screen scrim
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
                cr.rectangle(0.0, 0.0, w, h);
                cr.fill().unwrap();
            }
        });
    }

    // Gesture Drag for selection
    let gesture_drag = GestureDrag::new();
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        gesture_drag.connect_drag_begin(move |_, x, y| {
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
        gesture_drag.connect_drag_end(move |gesture, offset_x, offset_y| {
            let mut s = selection.borrow_mut();
            if let Some((start_x, start_y)) = gesture.start_point() {
                s.current_x = start_x + offset_x;
                s.current_y = start_y + offset_y;
                s.active = false;
                s.completed = true;
                da.queue_draw();
            }
        });
    }
    drawing_area.add_controller(gesture_drag);
    root_overlay.add_overlay(&drawing_area);

    // Floating Action Bar (Pill shape)
    let action_bar = Box::new(Orientation::Horizontal, 6);
    action_bar.add_css_class("floating-pill");
    action_bar.set_halign(gtk::Align::Center);

    // Helper closure to perform OCR on current selection
    let perform_ocr = {
        let screen_img = screen_img.clone();
        let selection = Rc::clone(&selection);
        let ocr_engine = Rc::clone(&ocr_engine);
        let da = drawing_area.clone();

        move || -> Result<String> {
            let Some(ref img) = screen_img else {
                anyhow::bail!("No screen image available");
            };

            // Lazily ensure and initialize OCR engine
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
                // If no selection, use full screen
                (0.0, 0.0, da_w.max(1.0), da_h.max(1.0))
            };

            let scale_x = img_w as f64 / da_w.max(1.0);
            let scale_y = img_h as f64 / da_h.max(1.0);

            let crop_x = ((sel_x * scale_x).round() as u32).min(img_w.saturating_sub(1));
            let crop_y = ((sel_y * scale_y).round() as u32).min(img_h.saturating_sub(1));
            let crop_w = ((sel_w * scale_x).round() as u32).min(img_w - crop_x).max(1);
            let crop_h = ((sel_h * scale_y).round() as u32).min(img_h - crop_y).max(1);

            let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
            let mut engine_ref = ocr_engine.borrow_mut();
            let engine = engine_ref.as_mut().unwrap();
            let text = engine.recognize_image(&DynamicImage::ImageRgba8(crop))?;
            Ok(text)
        }
    };

    // 1. Copy Button
    let btn_copy = create_symbolic_button("edit-copy-symbolic", "Metni Kopyala (Enter)");
    {
        let window_weak = window.downgrade();
        let perform_ocr = perform_ocr.clone();
        btn_copy.connect_clicked(move |_| {
            match perform_ocr() {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let _ = clipboard::copy_to_clipboard(trimmed);
                        clipboard::send_notification("Wayfrost", &format!("Kopyalandı: {trimmed}"));
                    }
                    if let Some(win) = window_weak.upgrade() {
                        win.close();
                    }
                }
                Err(e) => {
                    log::error!("OCR failed: {e}");
                }
            }
        });
    }

    // 2. Select All Button
    let btn_select_all = create_symbolic_button("edit-select-all-symbolic", "Tümünü Seç");
    {
        let selection = Rc::clone(&selection);
        let da = drawing_area.clone();
        btn_select_all.connect_clicked(move |_| {
            let mut s = selection.borrow_mut();
            s.start_x = 0.0;
            s.start_y = 0.0;
            s.current_x = da.width() as f64;
            s.current_y = da.height() as f64;
            s.active = false;
            s.completed = true;
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
    let lang_label = gtk::Label::new(Some("TR"));
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
        let popover = popover.clone();
        btn_tr.connect_clicked(move |_| {
            lang_label.set_text("TR");
            lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: Türkçe - TR)"));
            popover.popdown();
        });
    }

    {
        let lang_label = lang_label.clone();
        let lang_button = lang_button.clone();
        let popover = popover.clone();
        btn_en.connect_clicked(move |_| {
            lang_label.set_text("EN");
            lang_button.set_tooltip_text(Some("Dil Seçimi (Aktif: English - EN)"));
            popover.popdown();
        });
    }

    popover_vbox.append(&btn_tr);
    popover_vbox.append(&btn_en);
    popover.set_child(Some(&popover_vbox));

    // 4. Edit Text Button
    let btn_edit = create_symbolic_button("document-edit-symbolic", "Metni Düzenle");
    {
        let window = window.clone();
        let perform_ocr = perform_ocr.clone();
        btn_edit.connect_clicked(move |_| {
            let extracted = perform_ocr().unwrap_or_default();

            // Dialog window to view and edit recognized text
            let edit_win = gtk::Window::builder()
                .title("Wayfrost — Metin Düzenle")
                .transient_for(&window)
                .modal(true)
                .default_width(520)
                .default_height(340)
                .build();
            edit_win.add_css_class("edit-dialog");

            let vbox = Box::new(Orientation::Vertical, 12);
            vbox.set_margin_top(16);
            vbox.set_margin_bottom(16);
            vbox.set_margin_start(16);
            vbox.set_margin_end(16);

            let scrolled = ScrolledWindow::builder()
                .vexpand(true)
                .hexpand(true)
                .build();
            let text_view = TextView::new();
            text_view.set_wrap_mode(gtk::WrapMode::Word);
            text_view.buffer().set_text(&extracted);
            scrolled.set_child(Some(&text_view));
            vbox.append(&scrolled);

            let btn_row = Box::new(Orientation::Horizontal, 8);
            btn_row.set_halign(gtk::Align::End);

            let btn_copy_close = Button::with_label("Kopyala & Kapat");
            btn_copy_close.add_css_class("suggested-action");

            let edit_win_weak = edit_win.downgrade();
            let main_win_weak = window.downgrade();
            let text_buffer = text_view.buffer();
            btn_copy_close.connect_clicked(move |_| {
                let start = text_buffer.start_iter();
                let end = text_buffer.end_iter();
                let content = text_buffer.text(&start, &end, true).to_string();
                let _ = clipboard::copy_to_clipboard(&content);
                clipboard::send_notification("Wayfrost", "Metin kopyalandı");

                if let Some(ew) = edit_win_weak.upgrade() {
                    ew.close();
                }
                if let Some(mw) = main_win_weak.upgrade() {
                    mw.close();
                }
            });

            btn_row.append(&btn_copy_close);
            vbox.append(&btn_row);
            edit_win.set_child(Some(&vbox));
            edit_win.present();
        });
    }

    // Separator before close
    let separator = Separator::new(Orientation::Vertical);
    separator.add_css_class("pill-separator");

    // 5. Close / Cancel Button
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

    // Pack buttons into pill
    action_bar.append(&btn_copy);
    action_bar.append(&btn_select_all);
    action_bar.append(&lang_button);
    action_bar.append(&btn_edit);
    action_bar.append(&separator);
    action_bar.append(&btn_close);

    // Floating clamp container at bottom center
    let clamp = adw::Clamp::builder()
        .maximum_size(520)
        .tightening_threshold(400)
        .child(&action_bar)
        .build();

    let bottom_box = Box::new(Orientation::Vertical, 0);
    bottom_box.set_valign(gtk::Align::End);
    bottom_box.set_halign(gtk::Align::Fill);
    bottom_box.set_margin_bottom(36);
    bottom_box.append(&clamp);

    root_overlay.add_overlay(&bottom_box);
    window.set_child(Some(&root_overlay));

    // CSS Styling
    let css_provider = CssProvider::new();
    css_provider.load_from_data(
        "
        window.overlay-window {
            background-color: transparent;
        }

        .floating-pill {
            background: rgba(24, 24, 26, 0.90);
            border: 1px solid rgba(255, 255, 255, 0.14);
            border-radius: 9999px;
            padding: 6px 10px;
            box-shadow: 0 16px 40px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.05);
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
            transition: background-color 150ms cubic-bezier(0.2, 0, 0, 1), transform 100ms ease;
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
            transition: background-color 120ms ease;
        }

        .popover-item:hover {
            background: rgba(255, 255, 255, 0.12);
        }

        .edit-dialog {
            background-color: #1e1e20;
            color: #f2f2f7;
            border-radius: 16px;
        }

        .edit-dialog textview {
            background-color: #28282b;
            color: #f2f2f7;
            border-radius: 8px;
            padding: 12px;
            font-size: 14px;
        }
        ",
    );

    gtk4::style_context_add_provider_for_display(
        &gtk4::prelude::RootExt::display(&window),
        &css_provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Keyboard shortcuts (Escape = close, Enter = OCR copy)
    let key_controller = EventControllerKey::new();
    {
        let window_weak = window.downgrade();
        let perform_ocr = perform_ocr;
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                if let Some(win) = window_weak.upgrade() {
                    win.close();
                    return glib::Propagation::Stop;
                }
            } else if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
                match perform_ocr() {
                    Ok(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let _ = clipboard::copy_to_clipboard(trimmed);
                            clipboard::send_notification("Wayfrost", &format!("Kopyalandı: {trimmed}"));
                        }
                        if let Some(win) = window_weak.upgrade() {
                            win.close();
                            return glib::Propagation::Stop;
                        }
                    }
                    Err(e) => {
                        log::error!("OCR failed: {e}");
                    }
                }
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(key_controller);

    window.present();
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
