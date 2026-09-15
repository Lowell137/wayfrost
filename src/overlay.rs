use anyhow::Result;
use gtk4 as gtk;
use gtk4::cairo;
use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DrawingArea, EventControllerKey, EventControllerMotion,
    GestureDrag, Image, Orientation, Overlay, Picture, Popover, Separator,
};
use image::{imageops, DynamicImage, GenericImageView};
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::capture;
use crate::clipboard;
use crate::ocr::pipeline::{self, DetectedWord};

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

    // State management: Persistent selection frame & text selection
    let locked_region: Rc<RefCell<Option<(f64, f64, f64, f64)>>> = Rc::new(RefCell::new(None));
    let framing_drag: Rc<RefCell<Option<((f64, f64), (f64, f64))>>> = Rc::new(RefCell::new(None));
    let text_drag: Rc<RefCell<Option<((f64, f64), (f64, f64))>>> = Rc::new(RefCell::new(None));
    let active_lang = Rc::new(RefCell::new("TR".to_string()));
    let cached_text: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Live Text: detected words with bounding boxes
    let all_words: Rc<RefCell<Vec<DetectedWord>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let root_overlay = Overlay::new();

    // 1. GPU-accelerated background picture (avoids 100% CPU Cairo redraw on every mouse frame!)
    let bg_picture = Picture::new();
    bg_picture.set_hexpand(true);
    bg_picture.set_vexpand(true);
    bg_picture.set_can_shrink(true);

    if let Some(ref img) = screen_img {
        let (w, h) = img.dimensions();
        let rgba = img.to_rgba8();
        let bytes = glib::Bytes::from_owned(rgba.into_raw());
        let texture = gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            (w * 4) as usize,
        );
        bg_picture.set_paintable(Some(&texture));
    }
    root_overlay.set_child(Some(&bg_picture));

    // Fullscreen transparent DrawingArea: overlays dimming, borders, and delicate word borders
    let drawing_area = DrawingArea::new();
    drawing_area.set_can_target(true);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);

    // Floating Copy Button (Circular pill, only edit-copy-symbolic)
    let floating_copy_btn = Button::new();
    floating_copy_btn.add_css_class("floating-copy-btn");
    floating_copy_btn.set_cursor_from_name(Some("pointer"));
    let copy_icon = Image::from_icon_name("edit-copy-symbolic");
    copy_icon.set_pixel_size(20);
    floating_copy_btn.set_child(Some(&copy_icon));
    floating_copy_btn.set_tooltip_text(Some("Kopyala"));
    floating_copy_btn.set_halign(Align::Start);
    floating_copy_btn.set_valign(Align::Start);
    floating_copy_btn.set_visible(false);

    let copy_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let text_drag = Rc::clone(&text_drag);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let da_for_style = drawing_area.clone();

        drawing_area.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;

            let f_drag = *framing_drag.borrow();
            let locked = *locked_region.borrow();
            let t_drag = *text_drag.borrow();
            let words = all_words.borrow();
            let selected = selected_indices.borrow();

            let (ar, ag, ab, _) = get_accent_color(&da_for_style);

            // 1. Framing Drag or Persistent Locked Region Dimming
            if let Some((start, curr)) = f_drag {
                let sx = start.0.min(curr.0);
                let sy = start.1.min(curr.1);
                let sw = (start.0 - curr.0).abs();
                let sh = (start.1 - curr.1).abs();

                // Dim unselected area
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
                cr.rectangle(0.0, 0.0, w, sy);
                cr.rectangle(0.0, sy + sh, w, (h - (sy + sh)).max(0.0));
                cr.rectangle(0.0, sy, sx, sh);
                cr.rectangle(sx + sw, sy, (w - (sx + sw)).max(0.0), sh);
                let _ = cr.fill();

                // Crisp accent frame border during active drag
                cr.set_source_rgba(ar, ag, ab, 0.85);
                cr.set_line_width(1.5);
                cr.rectangle(sx, sy, sw, sh);
                let _ = cr.stroke();
            } else if let Some((rx, ry, rw, rh)) = locked {
                // PERSISTENT SELECTION RECTANGLE!
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.38);
                cr.rectangle(0.0, 0.0, w, ry);
                cr.rectangle(0.0, ry + rh, w, (h - (ry + rh)).max(0.0));
                cr.rectangle(0.0, ry, rx, rh);
                cr.rectangle(rx + rw, ry, (w - (rx + rw)).max(0.0), rh);
                let _ = cr.fill();

                // Vibrant Libadwaita accent frame border
                cr.set_source_rgba(ar, ag, ab, 0.90);
                cr.set_line_width(1.5);
                cr.rectangle(rx, ry, rw, rh);
                let _ = cr.stroke();

                // Sleek white corner marks
                let corner_len = 14.0f64.min(rw / 4.0).min(rh / 4.0);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.90);
                cr.set_line_width(2.0);
                // Top-Left
                cr.move_to(rx, ry + corner_len);
                cr.line_to(rx, ry);
                cr.line_to(rx + corner_len, ry);
                let _ = cr.stroke();
                // Top-Right
                cr.move_to(rx + rw - corner_len, ry);
                cr.line_to(rx + rw, ry);
                cr.line_to(rx + rw, ry + corner_len);
                let _ = cr.stroke();
                // Bottom-Left
                cr.move_to(rx, ry + rh - corner_len);
                cr.line_to(rx, ry + rh);
                cr.line_to(rx + corner_len, ry + rh);
                let _ = cr.stroke();
                // Bottom-Right
                cr.move_to(rx + rw - corner_len, ry + rh);
                cr.line_to(rx + rw, ry + rh);
                cr.line_to(rx + rw, ry + rh - corner_len);
                let _ = cr.stroke();
            } else {
                // Initial subtle scrim before region is selected
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.22);
                cr.rectangle(0.0, 0.0, w, h);
                let _ = cr.fill();
            }

            // 2. Text Drag Selection rectangle inside frame
            if let Some((start, curr)) = t_drag {
                let tx1 = start.0.min(curr.0);
                let ty1 = start.1.min(curr.1);
                let tw = (start.0 - curr.0).abs();
                let th = (start.1 - curr.1).abs();

                cr.set_source_rgba(ar, ag, ab, 0.12);
                cr.rectangle(tx1, ty1, tw, th);
                let _ = cr.fill();

                cr.set_source_rgba(ar, ag, ab, 0.70);
                cr.set_line_width(1.0);
                cr.rectangle(tx1, ty1, tw, th);
                let _ = cr.stroke();
            }

            // 3. Selected word boundaries:
            // Background is 100% TRANSPARENT (NO background fill), only 1.0px @accent_color stroke
            for &idx in selected.iter() {
                if let Some(word) = words.get(idx) {
                    cr.set_source_rgba(ar, ag, ab, 0.95);
                    cr.set_line_width(1.0);
                    cr.rectangle(word.x - 0.5, word.y - 0.5, word.w + 1.0, word.h + 1.0);
                    let _ = cr.stroke();
                }
            }
        });
    }
    root_overlay.add_overlay(&drawing_area);

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
        let locked_region = Rc::clone(&locked_region);
        let screen_img = screen_img.clone();
        let active_lang = Rc::clone(&active_lang);
        let drawing_area = drawing_area.clone();
        let window_weak = window.downgrade();

        move || {
            let mut text = cached_text.borrow().clone().unwrap_or_default();
            if text.trim().is_empty() {
                let words = all_words.borrow();
                let selected = selected_indices.borrow();
                let sel_words: Vec<&DetectedWord> = selected.iter().filter_map(|&i| words.get(i)).collect();
                if !sel_words.is_empty() {
                    text = pipeline::join_words(&sel_words);
                } else if let Some((rx, ry, rw, rh)) = *locked_region.borrow() {
                    // Copy all words inside persistent selection frame
                    let region_words: Vec<&DetectedWord> = words
                        .iter()
                        .filter(|w| !(w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh))
                        .collect();
                    if !region_words.is_empty() {
                        text = pipeline::join_words(&region_words);
                    } else if let Some(ref img) = screen_img {
                        let (img_w, img_h) = img.dimensions();
                        let da_w = drawing_area.width() as f64;
                        let da_h = drawing_area.height() as f64;
                        let scale_x = img_w as f64 / da_w.max(1.0);
                        let scale_y = img_h as f64 / da_h.max(1.0);

                        let crop_x = ((rx * scale_x).round() as u32).min(img_w.saturating_sub(1));
                        let crop_y = ((ry * scale_y).round() as u32).min(img_h.saturating_sub(1));
                        let crop_w = ((rw * scale_x).round() as u32).min(img_w - crop_x).max(1);
                        let crop_h = ((rh * scale_y).round() as u32).min(img_h - crop_y).max(1);

                        let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
                        let lang = active_lang.borrow().clone();
                        let crop_dyn = DynamicImage::ImageRgba8(crop);
                        if let Ok(crop_words) = pipeline::run_tesseract_tsv(&crop_dyn, &lang) {
                            let refs: Vec<&DetectedWord> = crop_words.iter().collect();
                            text = pipeline::join_words(&refs);
                        }
                    }
                } else if !words.is_empty() {
                    text = pipeline::join_words(&words.iter().collect::<Vec<_>>());
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

    // --- Helper function: Schedule floating tooltip at mouse release offset ---
    let schedule_copy_tooltip = {
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();
        let da = drawing_area.clone();

        move |mouse_x: f64, mouse_y: f64| {
            if let Some(source) = copy_timer.borrow_mut().take() {
                source.remove();
            }
            floating_copy_btn.set_visible(false);

            let btn_clone = floating_copy_btn.clone();
            let copy_timer_clone = Rc::clone(&copy_timer);
            let da_w = da.width() as f64;
            let da_h = da.height() as f64;
            let btn_size = 40.0;

            let btn_x = (mouse_x - btn_size / 2.0).max(12.0).min(da_w - btn_size - 12.0);
            let btn_y = if mouse_y - btn_size - 12.0 >= 12.0 {
                mouse_y - btn_size - 12.0
            } else {
                mouse_y + 16.0
            }
            .max(12.0)
            .min(da_h - btn_size - 12.0);

            let source_id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                copy_timer_clone.borrow_mut().take();
                btn_clone.set_margin_start(btn_x as i32);
                btn_clone.set_margin_top(btn_y as i32);
                btn_clone.set_visible(true);
            });
            *copy_timer.borrow_mut() = Some(source_id);
        }
    };

    // --- Mouse Drag Gestures: Persistent Selection Rectangle + Text Selection ---
    let gesture_drag = GestureDrag::new();
    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let text_drag = Rc::clone(&text_drag);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();
        let da = drawing_area.clone();

        gesture_drag.connect_drag_begin(move |_, start_x, start_y| {
            if let Some(source) = copy_timer.borrow_mut().take() {
                source.remove();
            }
            floating_copy_btn.set_visible(false);

            let current_locked = *locked_region.borrow();
            let is_inside_locked = if let Some((rx, ry, rw, rh)) = current_locked {
                start_x >= rx && start_x <= rx + rw && start_y >= ry && start_y <= ry + rh
            } else {
                false
            };

            if is_inside_locked {
                // Dragging INSIDE the persistent selection rectangle -> Text Selection
                *text_drag.borrow_mut() = Some(((start_x, start_y), (start_x, start_y)));
                *framing_drag.borrow_mut() = None;
                selected_indices.borrow_mut().clear();
                *cached_text.borrow_mut() = None;
            } else {
                // Dragging OUTSIDE or when no region locked -> New Selection Rectangle framing
                *locked_region.borrow_mut() = None;
                *text_drag.borrow_mut() = None;
                *framing_drag.borrow_mut() = Some(((start_x, start_y), (start_x, start_y)));
                selected_indices.borrow_mut().clear();
                *cached_text.borrow_mut() = None;
            }
            da.queue_draw();
        });
    }

    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let text_drag = Rc::clone(&text_drag);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let da = drawing_area.clone();

        gesture_drag.connect_drag_update(move |_, offset_x, offset_y| {
            if let Some((start, _)) = *text_drag.borrow() {
                let current = (start.0 + offset_x, start.1 + offset_y);
                *text_drag.borrow_mut() = Some((start, current));

                let min_x = start.0.min(current.0);
                let max_x = start.0.max(current.0);
                let min_y = start.1.min(current.1) - 4.0;
                let max_y = start.1.max(current.1) + 4.0;

                let words = all_words.borrow();
                let locked = *locked_region.borrow();
                let mut new_sel = Vec::new();
                for (i, w) in words.iter().enumerate() {
                    if let Some((rx, ry, rw, rh)) = locked {
                        if w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh {
                            continue;
                        }
                    }
                    let (wx2, wy2) = (w.x + w.w, w.y + w.h);
                    if !(wx2 < min_x || w.x > max_x || wy2 < min_y || w.y > max_y) {
                        new_sel.push(i);
                    }
                }
                drop(words);
                *selected_indices.borrow_mut() = new_sel;
                da.queue_draw();
            } else if let Some((start, _)) = *framing_drag.borrow() {
                let current = (start.0 + offset_x, start.1 + offset_y);
                *framing_drag.borrow_mut() = Some((start, current));
                da.queue_draw();
            }
        });
    }

    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let text_drag = Rc::clone(&text_drag);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let screen_img = screen_img.clone();
        let active_lang = Rc::clone(&active_lang);
        let da = drawing_area.clone();
        let schedule_tooltip = schedule_copy_tooltip.clone();

        gesture_drag.connect_drag_end(move |_, offset_x, offset_y| {
            if let Some((start, _)) = framing_drag.borrow_mut().take() {
                let current = (start.0 + offset_x, start.1 + offset_y);
                let sx = start.0.min(current.0);
                let sy = start.1.min(current.1);
                let sw = (start.0 - current.0).abs();
                let sh = (start.1 - current.1).abs();

                if sw >= 8.0 && sh >= 8.0 {
                    // PERSISTENT SELECTION RECTANGLE!
                    *locked_region.borrow_mut() = Some((sx, sy, sw, sh));

                    // Ensure words within this region are recognized
                    let has_words_in_region = {
                        let words = all_words.borrow();
                        words
                            .iter()
                            .any(|w| !(w.x + w.w < sx || w.x > sx + sw || w.y + w.h < sy || w.y > sy + sh))
                    };

                    if !has_words_in_region {
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
                            let crop_dyn = DynamicImage::ImageRgba8(crop);

                            if let Ok(mut crop_words) = pipeline::run_tesseract_tsv(&crop_dyn, &lang) {
                                let mut words = all_words.borrow_mut();
                                for mut w in crop_words.drain(..) {
                                    w.x = (w.x + crop_x as f64) / scale_x;
                                    w.y = (w.y + crop_y as f64) / scale_y;
                                    w.w = w.w / scale_x;
                                    w.h = w.h / scale_y;
                                    words.push(w);
                                }
                            }
                        }
                    }

                    // Schedule 400ms floating copy tooltip directly at mouse release coordinates!
                    schedule_tooltip(current.0, current.1);
                } else {
                    // Click outside without drag -> clear persistent region
                    *locked_region.borrow_mut() = None;
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                }
                da.queue_draw();
            } else if let Some((start, _)) = text_drag.borrow_mut().take() {
                let current = (start.0 + offset_x, start.1 + offset_y);
                let sw = (start.0 - current.0).abs();
                let sh = (start.1 - current.1).abs();

                if sw < 4.0 && sh < 4.0 {
                    // Single click inside region: select single word under cursor
                    let words = all_words.borrow();
                    let mut clicked_idx = None;
                    for (i, w) in words.iter().enumerate() {
                        if start.0 >= w.x - 2.0
                            && start.0 <= w.x + w.w + 2.0
                            && start.1 >= w.y - 2.0
                            && start.1 <= w.y + w.h + 2.0
                        {
                            clicked_idx = Some(i);
                            break;
                        }
                    }
                    if let Some(idx) = clicked_idx {
                        *selected_indices.borrow_mut() = vec![idx];
                    }
                }

                // Cache selected text
                {
                    let words = all_words.borrow();
                    let selected = selected_indices.borrow();
                    let sel_words: Vec<&DetectedWord> =
                        selected.iter().filter_map(|&i| words.get(i)).collect();
                    if !sel_words.is_empty() {
                        *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
                    }
                }

                // Schedule 400ms floating copy tooltip directly at mouse release coordinates!
                schedule_tooltip(current.0, current.1);
                da.queue_draw();
            }
        });
    }
    drawing_area.add_controller(gesture_drag);

    // --- Cursor Motion: "crosshair" in free space / framing, "text" over detected word bounding boxes ---
    let motion = EventControllerMotion::new();
    {
        let all_words = Rc::clone(&all_words);
        let text_drag = Rc::clone(&text_drag);
        let locked_region = Rc::clone(&locked_region);
        let window_weak = window.downgrade();
        motion.connect_motion(move |_, x, y| {
            if let Some(win) = window_weak.upgrade() {
                let is_over_word = {
                    let words = all_words.borrow();
                    let locked = *locked_region.borrow();
                    words.iter().any(|w| {
                        if let Some((rx, ry, rw, rh)) = locked {
                            if w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh {
                                return false;
                            }
                        }
                        x >= w.x - 2.0 && x <= w.x + w.w + 2.0 && y >= w.y - 2.0 && y <= w.y + w.h + 2.0
                    })
                };
                let is_text_dragging = text_drag.borrow().is_some();
                if is_over_word || is_text_dragging {
                    win.set_cursor_from_name(Some("text"));
                } else {
                    win.set_cursor_from_name(Some("crosshair"));
                }
            }
        });
    }
    drawing_area.add_controller(motion);

    // --- Bottom Floating Pill Toolbar: PURE SYMBOLIC ICONS (No text labels), 24px icon sizes ---
    let action_bar = Box::new(Orientation::Horizontal, 8);
    action_bar.add_css_class("floating-pill");
    action_bar.set_halign(Align::Center);

    // 1. Copy All Button (Pure symbolic icon: edit-copy-symbolic, 24px)
    let btn_copy_bar = create_symbolic_button("edit-copy-symbolic", "Tümünü Kopyala (Enter)");
    {
        let copy_fn = copy_selection_and_finish.clone();
        btn_copy_bar.connect_clicked(move |_| {
            copy_fn();
        });
    }

    // 2. Select All Button (Pure symbolic icon: edit-select-all-symbolic, 24px)
    let btn_select_all = create_symbolic_button("edit-select-all-symbolic", "Tüm Metni Seç (Ctrl+A)");
    {
        let da = drawing_area.clone();
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let locked_region = Rc::clone(&locked_region);
        let cached_text = Rc::clone(&cached_text);
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();

        btn_select_all.connect_clicked(move |_| {
            if let Some(source) = copy_timer.borrow_mut().take() {
                source.remove();
            }
            floating_copy_btn.set_visible(false);

            let words = all_words.borrow();
            let locked = *locked_region.borrow();
            let all_idx: Vec<usize> = if let Some((rx, ry, rw, rh)) = locked {
                (0..words.len())
                    .filter(|&i| {
                        let w = &words[i];
                        !(w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh)
                    })
                    .collect()
            } else {
                (0..words.len()).collect()
            };

            let sel_words: Vec<&DetectedWord> = all_idx.iter().filter_map(|&i| words.get(i)).collect();
            if !sel_words.is_empty() {
                *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
            }
            drop(words);
            *selected_indices.borrow_mut() = all_idx;
            da.queue_draw();
        });
    }

    // 3. Reset Selection Button (Pure symbolic icon: view-refresh-symbolic, 24px)
    let btn_reset_region = create_symbolic_button("view-refresh-symbolic", "Seçimi Sıfırla (Esc)");
    {
        let da = drawing_area.clone();
        let locked_region = Rc::clone(&locked_region);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();

        btn_reset_region.connect_clicked(move |_| {
            if let Some(source) = copy_timer.borrow_mut().take() {
                source.remove();
            }
            floating_copy_btn.set_visible(false);
            *locked_region.borrow_mut() = None;
            selected_indices.borrow_mut().clear();
            *cached_text.borrow_mut() = None;
            da.queue_draw();
        });
    }

    // 4. Floating Copy Button Click Handler
    {
        let copy_fn = copy_selection_and_finish.clone();
        floating_copy_btn.connect_clicked(move |_| {
            copy_fn();
        });
    }

    // 5. Language Switcher (Pure symbolic icon: preferences-desktop-locale-symbolic, 24px)
    let lang_button =
        create_symbolic_button("preferences-desktop-locale-symbolic", "Dil Seçimi (Aktif: TR)");

    let popover = Popover::new();
    popover.add_css_class("lang-popover");
    let pop_box = Box::new(Orientation::Vertical, 4);

    let opt_tr = Button::with_label("Türkçe (TR)");
    opt_tr.add_css_class("popover-item");
    let opt_en = Button::with_label("English (EN)");
    opt_en.add_css_class("popover-item");

    pop_box.append(&opt_tr);
    pop_box.append(&opt_en);
    popover.set_child(Some(&pop_box));
    popover.set_parent(&lang_button);

    lang_button.connect_clicked({
        let pop = popover.clone();
        move |_| pop.popup()
    });

    let (tx_lang, rx_lang) = std::sync::mpsc::channel::<Vec<DetectedWord>>();
    let rx_lang = Rc::new(RefCell::new(rx_lang));
    {
        let all_words = Rc::clone(&all_words);
        let da = drawing_area.clone();
        let rx_lang = Rc::clone(&rx_lang);
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if let Ok(words) = rx_lang.borrow_mut().try_recv() {
                *all_words.borrow_mut() = words;
                da.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let active_lang = Rc::clone(&active_lang);
        let lang_btn_clone = lang_button.clone();
        let pop = popover.clone();
        let screen_img = screen_img.clone();
        let tx = tx_lang.clone();

        opt_tr.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "TR".to_string();
            lang_btn_clone.set_tooltip_text(Some("Dil Seçimi (Aktif: TR)"));
            pop.popdown();

            if let Some(ref img_arc) = screen_img {
                let img = Arc::clone(img_arc);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    if let Ok(words) = pipeline::run_tesseract_tsv(&img, "TR") {
                        let _ = tx.send(words);
                    }
                });
            }
        });
    }

    {
        let active_lang = Rc::clone(&active_lang);
        let lang_btn_clone = lang_button.clone();
        let pop = popover.clone();
        let screen_img = screen_img.clone();
        let tx = tx_lang.clone();

        opt_en.connect_clicked(move |_| {
            *active_lang.borrow_mut() = "EN".to_string();
            lang_btn_clone.set_tooltip_text(Some("Dil Seçimi (Aktif: EN)"));
            pop.popdown();

            if let Some(ref img_arc) = screen_img {
                let img = Arc::clone(img_arc);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    if let Ok(words) = pipeline::run_tesseract_tsv(&img, "EN") {
                        let _ = tx.send(words);
                    }
                });
            }
        });
    }

    // 6. Close Button (Pure symbolic icon: window-close-symbolic, 24px)
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

    let sep1 = Separator::new(Orientation::Vertical);
    sep1.add_css_class("pill-separator");
    let sep2 = Separator::new(Orientation::Vertical);
    sep2.add_css_class("pill-separator");

    action_bar.append(&btn_copy_bar);
    action_bar.append(&sep1);
    action_bar.append(&btn_select_all);
    action_bar.append(&btn_reset_region);
    action_bar.append(&lang_button);
    action_bar.append(&sep2);
    action_bar.append(&btn_close);

    let bar_clamp = adw::Clamp::builder()
        .maximum_size(360)
        .tightening_threshold(280)
        .child(&action_bar)
        .build();

    let bottom_box = Box::new(Orientation::Vertical, 0);
    bottom_box.set_valign(Align::End);
    bottom_box.set_halign(Align::Fill);
    bottom_box.set_margin_bottom(28);
    bottom_box.append(&bar_clamp);

    root_overlay.add_overlay(&floating_copy_btn);
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
            padding: 6px 12px;
            box-shadow: 0 16px 40px rgba(0, 0, 0, 0.7);
            backdrop-filter: blur(24px);
        }

        .floating-pill button,
        .floating-pill .pill-btn {
            background: transparent;
            border: none;
            border-radius: 9999px;
            min-width: 44px;
            min-height: 44px;
            padding: 8px;
            color: #f2f2f7;
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
        }

        .popover-item:hover {
            background: rgba(255, 255, 255, 0.12);
        }

        .floating-copy-btn {
            background-color: @accent_bg_color;
            background-image: none;
            color: @accent_fg_color;
            border-radius: 9999px;
            min-width: 40px;
            max-width: 40px;
            min-height: 40px;
            max-height: 40px;
            padding: 0;
            border: 1px solid rgba(255, 255, 255, 0.28);
            box-shadow: 0 4px 18px rgba(0, 0, 0, 0.55);
            transition: transform 120ms ease, background-color 150ms ease;
            outline: none;
        }

        .floating-copy-btn:hover {
            transform: scale(1.10);
            box-shadow: 0 6px 24px rgba(0, 0, 0, 0.70);
        }

        .floating-copy-btn:active {
            transform: scale(0.94);
        }
        ",
    );

    gtk4::style_context_add_provider_for_display(
        &gtk4::prelude::RootExt::display(&window),
        &css_provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // --- Key Controller: Escape = close/deselect, Ctrl+C / Enter = copy, Ctrl+A = select all ---
    let key_controller = EventControllerKey::new();
    {
        let window_weak = window.downgrade();
        let copy_fn = copy_selection_and_finish.clone();
        let selected_indices = Rc::clone(&selected_indices);
        let locked_region = Rc::clone(&locked_region);
        let cached_text = Rc::clone(&cached_text);
        let all_words = Rc::clone(&all_words);
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();
        let da = drawing_area.clone();

        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == gdk::Key::Escape {
                if let Some(source) = copy_timer.borrow_mut().take() {
                    source.remove();
                }
                floating_copy_btn.set_visible(false);

                let has_sel = !selected_indices.borrow().is_empty();
                if has_sel {
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                    da.queue_draw();
                    return glib::Propagation::Stop;
                }

                let has_locked = locked_region.borrow().is_some();
                if has_locked {
                    *locked_region.borrow_mut() = None;
                    *cached_text.borrow_mut() = None;
                    da.queue_draw();
                    return glib::Propagation::Stop;
                }

                if let Some(win) = window_weak.upgrade() {
                    win.close();
                    return glib::Propagation::Stop;
                }
            }

            // Ctrl+A: select all words in locked region (or screen)
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && (keyval == gdk::Key::a || keyval == gdk::Key::A)
            {
                if let Some(source) = copy_timer.borrow_mut().take() {
                    source.remove();
                }
                floating_copy_btn.set_visible(false);

                let words = all_words.borrow();
                let locked = *locked_region.borrow();
                let all_sel: Vec<usize> = if let Some((rx, ry, rw, rh)) = locked {
                    (0..words.len())
                        .filter(|&i| {
                            let w = &words[i];
                            !(w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh)
                        })
                        .collect()
                } else {
                    (0..words.len()).collect()
                };

                let sel_words: Vec<&DetectedWord> =
                    all_sel.iter().filter_map(|&i| words.get(i)).collect();
                if !sel_words.is_empty() {
                    *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
                }
                drop(words);
                *selected_indices.borrow_mut() = all_sel;
                da.queue_draw();
                return glib::Propagation::Stop;
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

fn get_accent_color(widget: &DrawingArea) -> (f64, f64, f64, f64) {
    let ctx = widget.style_context();
    if let Some(rgba) = ctx.lookup_color("accent_color").or_else(|| ctx.lookup_color("accent_bg_color")) {
        return (
            rgba.red() as f64,
            rgba.green() as f64,
            rgba.blue() as f64,
            rgba.alpha() as f64,
        );
    }

    if let Some(source) = gio::SettingsSchemaSource::default() {
        if source.lookup("org.gnome.desktop.interface", true).is_some() {
            let settings = gio::Settings::new("org.gnome.desktop.interface");
            let accent = settings.string("accent-color");
            return match accent.as_str() {
                "teal" => (0.13, 0.56, 0.64, 1.0),
                "green" => (0.23, 0.58, 0.29, 1.0),
                "yellow" => (0.78, 0.53, 0.0, 1.0),
                "orange" => (0.93, 0.36, 0.0, 1.0),
                "red" => (0.90, 0.18, 0.26, 1.0),
                "pink" => (0.84, 0.38, 0.60, 1.0),
                "purple" => (0.57, 0.25, 0.67, 1.0),
                "slate" => (0.44, 0.51, 0.59, 1.0),
                _ => (0.208, 0.518, 0.894, 1.0), // blue
            };
        }
    }

    // Fallback default Libadwaita Blue: #3584e4
    (0.208, 0.518, 0.894, 1.0)
}

fn create_symbolic_button(icon_name: &str, tooltip: &str) -> Button {
    let btn = Button::new();
    btn.add_css_class("pill-btn");
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(24);
    btn.set_child(Some(&icon));
    btn.set_tooltip_text(Some(tooltip));
    btn
}

#[allow(dead_code)]
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
