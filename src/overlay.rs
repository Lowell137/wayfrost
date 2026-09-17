use anyhow::Result;
use gtk4 as gtk;
use gtk4::cairo;
use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DrawingArea, EventControllerKey, EventControllerMotion,
    GestureDrag, Image, Orientation, Overlay, Popover, Separator,
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
    window.set_cursor_from_name(Some("crosshair"));

    let root_overlay = Overlay::new();
    window.set_child(Some(&root_overlay));

    // AI Mode Switcher Header
    let header_box = gtk::Box::new(Orientation::Horizontal, 6);
    header_box.set_halign(Align::End);
    header_box.set_valign(Align::Start);
    header_box.set_margin_top(12);
    header_box.set_margin_end(12);
    
    let ai_button = gtk::Button::builder()
        .label("AI Mode")
        .icon_name("system-run-symbolic")
        .build();
    ai_button.connect_clicked(move |_| {
        println!("AI Modal needed here");
    });


    window.add_css_class("overlay-window");
    window.set_cursor_from_name(Some("crosshair"));

    // State management: Persistent selection frame & text selection
    let locked_region: Rc<RefCell<Option<(f64, f64, f64, f64)>>> = Rc::new(RefCell::new(None));
    let framing_drag: Rc<RefCell<Option<((f64, f64), (f64, f64))>>> = Rc::new(RefCell::new(None));
    let text_drag: Rc<RefCell<Option<((f64, f64), (f64, f64))>>> = Rc::new(RefCell::new(None));
    let active_lang = Rc::new(RefCell::new("TR".to_string()));
    let cached_text: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Live Text: detected words with bounding boxes (exact 1:1 screen coordinates)
    let all_words: Rc<RefCell<Vec<DetectedWord>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_indices: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let root_overlay = Overlay::new();

    let (img_w, img_h) = screen_img
        .as_ref()
        .map(|img| img.dimensions())
        .unwrap_or((1920, 1080));

    // --- GPU BACKGROUND: Upload screenshot ONCE to GPU via GdkMemoryTexture + GtkPicture ---
    // Previously: Cairo was re-uploading 8MB every mouse-move frame → stutter.
    // Now: texture lives on GPU, only overlay quads are re-drawn by Cairo.
    if let Some(ref img_arc) = screen_img {
        let rgba = img_arc.to_rgba8();
        let (w, h) = img_arc.dimensions();
        let stride = w as usize * 4;
        let bytes = glib::Bytes::from(rgba.as_raw().as_slice());
        let texture = gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            stride,
        );
        let bg_picture = gtk::Picture::for_paintable(&texture);
        bg_picture.set_content_fit(gtk::ContentFit::Fill);
        bg_picture.set_hexpand(true);
        bg_picture.set_vexpand(true);
        bg_picture.set_can_target(false);
        root_overlay.set_child(Some(&bg_picture));
    } else {
        let fallback = DrawingArea::new();
        fallback.set_hexpand(true);
        fallback.set_vexpand(true);
        fallback.set_can_target(false);
        fallback.set_draw_func(|_, cr, w, h| {
            cr.set_source_rgb(0.08, 0.08, 0.1);
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            let _ = cr.fill();
        });
        root_overlay.set_child(Some(&fallback));
    }

    // Overlay DrawingArea: only draws dimming quads + selection frame + word boxes
    let drawing_area = DrawingArea::new();
    drawing_area.set_can_target(true);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);
    root_overlay.add_overlay(&drawing_area);

    // Cache accent color ONCE at startup to avoid querying GSettings D-Bus on every frame
    let (ar, ag, ab, _) = get_accent_color(&drawing_area);

    // Floating Copy Button (Circular pill, only edit-copy-symbolic)
    let floating_copy_btn = Button::new();
    floating_copy_btn.add_css_class("floating-copy-btn");
    floating_copy_btn.set_cursor_from_name(Some("pointer"));
    let copy_icon = Image::from_icon_name("edit-copy-symbolic");
    copy_icon.set_pixel_size(20);
    floating_copy_btn.set_child(Some(&copy_icon));
    floating_copy_btn.set_tooltip_text(Some("Copy"));
    floating_copy_btn.set_halign(Align::Start);
    floating_copy_btn.set_valign(Align::Start);
    floating_copy_btn.set_visible(false);

    let copy_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);

        drawing_area.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;
            let (iw, ih) = (img_w as f64, img_h as f64);
            let scale_x = w / iw.max(1.0);
            let scale_y = h / ih.max(1.0);

            let _ = cr.save();
            cr.scale(scale_x, scale_y);

            // Background rendered by GtkPicture (GPU). Only overlay drawn here.

            let f_drag = *framing_drag.borrow();
            let locked = *locked_region.borrow();
            let words = all_words.borrow();
            let selected = selected_indices.borrow();

            // Framing Drag or Persistent Locked Region Dimming
            if let Some((start, curr)) = f_drag {
                let sx = start.0.min(curr.0);
                let sy = start.1.min(curr.1);
                let sw = (start.0 - curr.0).abs();
                let sh = (start.1 - curr.1).abs();

                cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
                cr.rectangle(0.0, 0.0, iw, sy);
                cr.rectangle(0.0, sy + sh, iw, (ih - (sy + sh)).max(0.0));
                cr.rectangle(0.0, sy, sx, sh);
                cr.rectangle(sx + sw, sy, (iw - (sx + sw)).max(0.0), sh);
                let _ = cr.fill();

                cr.set_source_rgba(ar, ag, ab, 0.85);
                cr.set_line_width(1.5);
                cr.rectangle(sx, sy, sw, sh);
                let _ = cr.stroke();
            } else if let Some((rx, ry, rw, rh)) = locked {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.38);
                cr.rectangle(0.0, 0.0, iw, ry);
                cr.rectangle(0.0, ry + rh, iw, (ih - (ry + rh)).max(0.0));
                cr.rectangle(0.0, ry, rx, rh);
                cr.rectangle(rx + rw, ry, (iw - (rx + rw)).max(0.0), rh);
                let _ = cr.fill();

                cr.set_source_rgba(ar, ag, ab, 0.90);
                cr.set_line_width(1.5);
                cr.rectangle(rx, ry, rw, rh);
                let _ = cr.stroke();

                let corner_len = 14.0f64.min(rw / 4.0).min(rh / 4.0);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.90);
                cr.set_line_width(2.0);
                cr.move_to(rx, ry + corner_len); cr.line_to(rx, ry); cr.line_to(rx + corner_len, ry);
                let _ = cr.stroke();
                cr.move_to(rx + rw - corner_len, ry); cr.line_to(rx + rw, ry); cr.line_to(rx + rw, ry + corner_len);
                let _ = cr.stroke();
                cr.move_to(rx, ry + rh - corner_len); cr.line_to(rx, ry + rh); cr.line_to(rx + corner_len, ry + rh);
                let _ = cr.stroke();
                cr.move_to(rx + rw - corner_len, ry + rh); cr.line_to(rx + rw, ry + rh); cr.line_to(rx + rw, ry + rh - corner_len);
                let _ = cr.stroke();
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.22);
                cr.rectangle(0.0, 0.0, iw, ih);
                let _ = cr.fill();
            }

            // 3. Native-style continuous text selection highlight (like browser / text editor)
            if !selected.is_empty() {
                let mut sel_words: Vec<&DetectedWord> = selected
                    .iter()
                    .filter_map(|&i| words.get(i))
                    .collect();
                sel_words.sort_by(|a, b| {
                    (a.par_num, a.line_num)
                        .cmp(&(b.par_num, b.line_num))
                        .then_with(|| a.x.total_cmp(&b.x))
                });

                let mut line_groups: Vec<Vec<&DetectedWord>> = Vec::new();
                for w in sel_words {
                    if let Some(last_group) = line_groups.last_mut() {
                        let prev = last_group.last().unwrap();
                        let same_tsv_line = w.par_num == prev.par_num
                            && w.line_num == prev.line_num;
                        let vert_overlap = (w.y - prev.y).abs() < (w.h.min(prev.h) * 0.6);

                        if same_tsv_line || vert_overlap {
                            last_group.push(w);
                            continue;
                        }
                    }
                    line_groups.push(vec![w]);
                }

                cr.set_source_rgba(ar, ag, ab, 0.55);
                for group in line_groups {
                    let min_x = group.iter().map(|w| w.x).fold(f64::INFINITY, f64::min);
                    let max_x = group.iter().map(|w| w.x + w.w).fold(f64::NEG_INFINITY, f64::max);
                    let min_y = group.iter().map(|w| w.y).fold(f64::INFINITY, f64::min);
                    let max_y = group.iter().map(|w| w.y + w.h).fold(f64::NEG_INFINITY, f64::max);

                    let pad_x = 2.5;
                    let pad_y = 1.5;
                    let rx = min_x - pad_x;
                    let ry = min_y - pad_y;
                    let rw = (max_x - min_x) + pad_x * 2.0;
                    let rh = (max_y - min_y) + pad_y * 2.0;

                    let radius = 3.0f64.min(rw / 2.0).min(rh / 2.0);
                    draw_rounded_rect(cr, rx, ry, rw, rh, radius);
                    let _ = cr.fill();
                }
            }

            let _ = cr.restore();
        });
    }

    // 2. Background task: extract word bounding boxes across full screen (in 1:1 pixel coords)
    let (tx, rx) = std::sync::mpsc::channel::<Vec<DetectedWord>>();
    let rx = Rc::new(RefCell::new(rx));
    {
        let all_words = Rc::clone(&all_words);
        let da = drawing_area.clone();
        let rx = Rc::clone(&rx);

        glib::timeout_add_local(std::time::Duration::from_millis(25), move || {
            if let Ok(words) = rx.borrow_mut().try_recv() {
                *all_words.borrow_mut() = words;
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
            if let Ok(words) = pipeline::extract_words_onnx_or_fallback(&img, &lang) {
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
                        let crop_x = (rx.round() as u32).min(img_w.saturating_sub(1));
                        let crop_y = (ry.round() as u32).min(img_h.saturating_sub(1));
                        let crop_w = (rw.round() as u32).min(img_w - crop_x).max(1);
                        let crop_h = (rh.round() as u32).min(img_h - crop_y).max(1);

                        let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
                        let lang = active_lang.borrow().clone();
                        let crop_dyn = DynamicImage::ImageRgba8(crop);
                        if let Ok(crop_words) = pipeline::extract_words_onnx_or_fallback(&crop_dyn, &lang) {
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
                clipboard::send_notification("Wayfrost — Copied", &preview);

                if let Some(win) = window_weak.upgrade() {
                    win.close();
                }
            } else {
                clipboard::send_notification("Wayfrost", "No text found in selected area");
            }
        }
    };

    // --- Helper function: Schedule floating tooltip at mouse release offset in 0.20s (200ms) ---
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

            let mut btn_x = mouse_x + 8.0;
            let mut btn_y = mouse_y - 36.0;

            if btn_x + btn_size > da_w - 12.0 {
                btn_x = (mouse_x - btn_size - 8.0).max(12.0);
            }
            if btn_y < 12.0 {
                btn_y = (mouse_y + 16.0).min(da_h - btn_size - 12.0);
            }

            let final_x = btn_x.clamp(12.0, (da_w - btn_size - 12.0).max(12.0));
            let final_y = btn_y.clamp(12.0, (da_h - btn_size - 12.0).max(12.0));

            // 0.20s (200ms) response time as requested
            let source_id = glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                copy_timer_clone.borrow_mut().take();
                btn_clone.set_margin_start(final_x as i32);
                btn_clone.set_margin_top(final_y as i32);
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

            let da_w = da.width().max(1) as f64;
            let da_h = da.height().max(1) as f64;
            let to_img_x = img_w as f64 / da_w;
            let to_img_y = img_h as f64 / da_h;

            let img_x = start_x * to_img_x;
            let img_y = start_y * to_img_y;

            let current_locked = *locked_region.borrow();
            let is_inside_locked = if let Some((rx, ry, rw, rh)) = current_locked {
                img_x >= rx && img_x <= rx + rw && img_y >= ry && img_y <= ry + rh
            } else {
                false
            };

            if is_inside_locked {
                // Dragging INSIDE the already-framed region -> Text Selection
                *text_drag.borrow_mut() = Some(((img_x, img_y), (img_x, img_y)));
                *framing_drag.borrow_mut() = None;
                selected_indices.borrow_mut().clear();
                *cached_text.borrow_mut() = None;
            } else {
                // Dragging OUTSIDE or when no region framed -> Frame a new region first
                *locked_region.borrow_mut() = None;
                *text_drag.borrow_mut() = None;
                *framing_drag.borrow_mut() = Some(((img_x, img_y), (img_x, img_y)));
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
            let da_w = da.width().max(1) as f64;
            let da_h = da.height().max(1) as f64;
            let to_img_x = img_w as f64 / da_w;
            let to_img_y = img_h as f64 / da_h;
            let off_x = offset_x * to_img_x;
            let off_y = offset_y * to_img_y;

            let t_start_opt = text_drag.borrow().map(|(start, _)| start);
            if let Some(start) = t_start_opt {
                let current = (start.0 + off_x, start.1 + off_y);
                *text_drag.borrow_mut() = Some((start, current));

                let words = all_words.borrow();
                let locked = *locked_region.borrow();
                let new_sel = select_words_in_flow(&words, start, current, locked);
                drop(words);

                if *selected_indices.borrow() != new_sel {
                    *selected_indices.borrow_mut() = new_sel;
                    da.queue_draw();
                }
            } else {
                let f_start_opt = framing_drag.borrow().map(|(start, _)| start);
                if let Some(start) = f_start_opt {
                    let current = (start.0 + off_x, start.1 + off_y);
                    *framing_drag.borrow_mut() = Some((start, current));
                    da.queue_draw();
                }
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
            let da_w = da.width().max(1) as f64;
            let da_h = da.height().max(1) as f64;
            let to_img_x = img_w as f64 / da_w;
            let to_img_y = img_h as f64 / da_h;
            let off_x = offset_x * to_img_x;
            let off_y = offset_y * to_img_y;

            if let Some((start, _)) = framing_drag.borrow_mut().take() {
                let current = (start.0 + off_x, start.1 + off_y);
                let sx = start.0.min(current.0);
                let sy = start.1.min(current.1);
                let sw = (start.0 - current.0).abs();
                let sh = (start.1 - current.1).abs();

                if sw >= 8.0 && sh >= 8.0 {
                    // PERSISTENT SELECTION RECTANGLE!
                    *locked_region.borrow_mut() = Some((sx, sy, sw, sh));

                    if let Some(ref img) = screen_img {
                        let (img_w, img_h) = img.dimensions();
                        let crop_x = (sx.round() as u32).min(img_w.saturating_sub(1));
                        let crop_y = (sy.round() as u32).min(img_h.saturating_sub(1));
                        let crop_w = (sw.round() as u32).min(img_w - crop_x).max(1);
                        let crop_h = (sh.round() as u32).min(img_h - crop_y).max(1);

                        let crop = imageops::crop_imm(img.as_ref(), crop_x, crop_y, crop_w, crop_h).to_image();
                        let lang = active_lang.borrow().clone();
                        let crop_dyn = DynamicImage::ImageRgba8(crop);

                        if let Ok(mut crop_words) = pipeline::extract_words_onnx_or_fallback(&crop_dyn, &lang) {
                            for w in &mut crop_words {
                                w.x += crop_x as f64;
                                w.y += crop_y as f64;
                            }
                            let mut words_mut = all_words.borrow_mut();
                            words_mut.retain(|w| w.x + w.w < sx || w.x > sx + sw || w.y + w.h < sy || w.y > sy + sh);
                            words_mut.extend(crop_words);
                            words_mut.sort_by(|a, b| {
                                (a.par_num, a.line_num)
                                    .cmp(&(b.par_num, b.line_num))
                                    .then_with(|| a.x.total_cmp(&b.x))
                            });
                        }
                    }

                    // Region is now framed cleanly: ready for user to drag-select text inside it
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                } else {
                    // Click outside without drag -> clear persistent region
                    *locked_region.borrow_mut() = None;
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                }
                da.queue_draw();
            } else if let Some((start, _)) = text_drag.borrow_mut().take() {
                let current = (start.0 + off_x, start.1 + off_y);
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
                    } else {
                        selected_indices.borrow_mut().clear();
                    }
                } else {
                    let words = all_words.borrow();
                    let locked = *locked_region.borrow();
                    let sel = select_words_in_flow(&words, start, current, locked);
                    drop(words);
                    *selected_indices.borrow_mut() = sel;
                }

                // Cache selected text
                let has_sel = {
                    let words = all_words.borrow();
                    let selected = selected_indices.borrow();
                    let sel_words: Vec<&DetectedWord> =
                        selected.iter().filter_map(|&i| words.get(i)).collect();
                    if !sel_words.is_empty() {
                        *cached_text.borrow_mut() = Some(pipeline::join_words(&sel_words));
                        true
                    } else {
                        *cached_text.borrow_mut() = None;
                        false
                    }
                };

                if has_sel {
                    schedule_tooltip(current.0 / to_img_x, current.1 / to_img_y);
                }
                da.queue_draw();
            }
        });
    }
    drawing_area.add_controller(gesture_drag);

    // --- Right-click drag: ALWAYS starts a new framing selection (ignores locked region) ---
    let gesture_rdrag = GestureDrag::new();
    gesture_rdrag.set_button(3);
    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let text_drag = Rc::clone(&text_drag);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let copy_timer = Rc::clone(&copy_timer);
        let floating_copy_btn = floating_copy_btn.clone();
        let da = drawing_area.clone();

        gesture_rdrag.connect_drag_begin(move |_, start_x, start_y| {
            if let Some(src) = copy_timer.borrow_mut().take() { src.remove(); }
            floating_copy_btn.set_visible(false);
            let to_img_x = img_w as f64 / da.width().max(1) as f64;
            let to_img_y = img_h as f64 / da.height().max(1) as f64;
            *locked_region.borrow_mut() = None;
            *text_drag.borrow_mut() = None;
            *framing_drag.borrow_mut() = Some(((start_x * to_img_x, start_y * to_img_y), (start_x * to_img_x, start_y * to_img_y)));
            selected_indices.borrow_mut().clear();
            *cached_text.borrow_mut() = None;
            da.queue_draw();
        });
    }
    {
        let framing_drag = Rc::clone(&framing_drag);
        let da = drawing_area.clone();

        gesture_rdrag.connect_drag_update(move |_, offset_x, offset_y| {
            let to_img_x = img_w as f64 / da.width().max(1) as f64;
            let to_img_y = img_h as f64 / da.height().max(1) as f64;
            let f_start_opt = framing_drag.borrow().map(|(start, _)| start);
            if let Some(start) = f_start_opt {
                let current = (start.0 + offset_x * to_img_x, start.1 + offset_y * to_img_y);
                *framing_drag.borrow_mut() = Some((start, current));
                da.queue_draw();
            }
        });
    }
    {
        let locked_region = Rc::clone(&locked_region);
        let framing_drag = Rc::clone(&framing_drag);
        let all_words = Rc::clone(&all_words);
        let selected_indices = Rc::clone(&selected_indices);
        let cached_text = Rc::clone(&cached_text);
        let screen_img = screen_img.clone();
        let active_lang = Rc::clone(&active_lang);
        let da = drawing_area.clone();

        gesture_rdrag.connect_drag_end(move |_, offset_x, offset_y| {
            let to_img_x = img_w as f64 / da.width().max(1) as f64;
            let to_img_y = img_h as f64 / da.height().max(1) as f64;
            if let Some((start, _)) = framing_drag.borrow_mut().take() {
                let current = (start.0 + offset_x * to_img_x, start.1 + offset_y * to_img_y);
                let sx = start.0.min(current.0);
                let sy = start.1.min(current.1);
                let sw = (start.0 - current.0).abs();
                let sh = (start.1 - current.1).abs();
                if sw >= 8.0 && sh >= 8.0 {
                    *locked_region.borrow_mut() = Some((sx, sy, sw, sh));

                    if let Some(ref img) = screen_img {
                        let (iw, ih) = img.dimensions();
                        let cx = (sx.round() as u32).min(iw.saturating_sub(1));
                        let cy = (sy.round() as u32).min(ih.saturating_sub(1));
                        let cw = (sw.round() as u32).min(iw - cx).max(1);
                        let ch = (sh.round() as u32).min(ih - cy).max(1);
                        let crop = imageops::crop_imm(img.as_ref(), cx, cy, cw, ch).to_image();
                        let lang = active_lang.borrow().clone();
                        let crop_dyn = DynamicImage::ImageRgba8(crop);
                        if let Ok(mut crop_words) = pipeline::extract_words_onnx_or_fallback(&crop_dyn, &lang) {
                            for w in &mut crop_words {
                                w.x += cx as f64;
                                w.y += cy as f64;
                            }
                            let mut words_mut = all_words.borrow_mut();
                            words_mut.retain(|w| w.x + w.w < sx || w.x > sx + sw || w.y + w.h < sy || w.y > sy + sh);
                            words_mut.extend(crop_words);
                            words_mut.sort_by(|a, b| {
                                (a.par_num, a.line_num)
                                    .cmp(&(b.par_num, b.line_num))
                                    .then_with(|| a.x.total_cmp(&b.x))
                            });
                        }
                    }
                    // Region is now framed cleanly: ready for user to drag-select text inside it
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                } else {
                    *locked_region.borrow_mut() = None;
                    selected_indices.borrow_mut().clear();
                    *cached_text.borrow_mut() = None;
                }
                da.queue_draw();
            }
        });
    }
    drawing_area.add_controller(gesture_rdrag);

    // --- Fast Cursor Motion: Only call Wayland IPC set_cursor_from_name when state actually changes! ---
    let current_cursor: Rc<RefCell<&'static str>> = Rc::new(RefCell::new("crosshair"));
    let motion = EventControllerMotion::new();
    {
        let all_words = Rc::clone(&all_words);
        let text_drag = Rc::clone(&text_drag);
        let locked_region = Rc::clone(&locked_region);
        let current_cursor = Rc::clone(&current_cursor);
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
                let desired_cursor = if is_over_word || is_text_dragging {
                    "text"
                } else {
                    "crosshair"
                };
                if *current_cursor.borrow() != desired_cursor {
                    *current_cursor.borrow_mut() = desired_cursor;
                    win.set_cursor_from_name(Some(desired_cursor));
                }
            }
        });
    }
    drawing_area.add_controller(motion);

    // --- Bottom Floating Pill Toolbar: PURE SYMBOLIC ICONS (No text labels), 24px/28px icon sizes ---
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

    // 3. Reset Selection Button (Pure symbolic icon: view-refresh-symbolic, enlarged to 28px)
    let btn_reset_region =
        create_symbolic_button_sized("view-refresh-symbolic", "Seçimi Sıfırla (Esc)", 28);
    btn_reset_region.add_css_class("btn-reset");
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
                    if let Ok(words) = pipeline::extract_words_onnx_or_fallback(&img, "TR") {
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
                    if let Ok(words) = pipeline::extract_words_onnx_or_fallback(&img, "EN") {
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

    // --- CSS Styles: Optimized for zero rendering lag and crisp icons ---
    let css_provider = CssProvider::new();
    css_provider.load_from_data(
        "
        window.overlay-window {
            background-color: black;
        }

        .floating-pill {
            background: rgba(22, 22, 24, 0.95);
            border: 1px solid rgba(255, 255, 255, 0.16);
            border-radius: 9999px;
            padding: 6px 12px;
            box-shadow: 0 10px 30px rgba(0, 0, 0, 0.6);
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
            transition: background-color 120ms ease, transform 80ms ease;
        }

        .floating-pill button image,
        .floating-pill .pill-btn image {
            -gtk-icon-size: 24px;
            min-width: 24px;
            min-height: 24px;
        }

        .floating-pill button.btn-reset image {
            -gtk-icon-size: 28px;
            min-width: 28px;
            min-height: 28px;
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

fn create_symbolic_button_sized(icon_name: &str, tooltip: &str, size: i32) -> Button {
    let btn = Button::new();
    btn.add_css_class("pill-btn");
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(size);
    btn.set_child(Some(&icon));
    btn.set_tooltip_text(Some(tooltip));
    btn
}

fn create_symbolic_button(icon_name: &str, tooltip: &str) -> Button {
    create_symbolic_button_sized(icon_name, tooltip, 24)
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

fn draw_rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    if r <= 0.0 {
        cr.rectangle(x, y, w, h);
        return;
    }
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
    cr.close_path();
}

fn select_words_in_flow(
    words: &[DetectedWord],
    start: (f64, f64),
    current: (f64, f64),
    locked_region: Option<(f64, f64, f64, f64)>,
) -> Vec<usize> {
    if words.is_empty() {
        return Vec::new();
    }

    let (p_first, p_last) = {
        let dy = current.1 - start.1;
        if dy.abs() > 10.0 {
            if dy > 0.0 {
                (start, current)
            } else {
                (current, start)
            }
        } else if start.0 <= current.0 {
            (start, current)
        } else {
            (current, start)
        }
    };

    let mut selected = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if let Some((rx, ry, rw, rh)) = locked_region {
            if w.x + w.w < rx || w.x > rx + rw || w.y + w.h < ry || w.y > ry + rh {
                continue;
            }
        }

        let line_top = w.y - 4.0;
        let line_bottom = w.y + w.h + 4.0;

        let after_first = if p_first.1 < line_top {
            true
        } else if p_first.1 > line_bottom {
            false
        } else {
            w.x + w.w >= p_first.0
        };

        if !after_first {
            continue;
        }

        let before_last = if p_last.1 > line_bottom {
            true
        } else if p_last.1 < line_top {
            false
        } else {
            w.x <= p_last.0
        };

        if before_last {
            selected.push(i);
        }
    }

    selected
}
