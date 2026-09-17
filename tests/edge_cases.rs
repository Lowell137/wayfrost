use anyhow::Result;
use image::{DynamicImage, RgbImage};
use wayfrost::ocr::model_manager::ensure_models;
use wayfrost::ocr::pipeline::OcrEngine;

#[test]
fn test_zero_and_tiny_crops() -> Result<()> {
    let paths = ensure_models()?;
    let mut engine = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file)?;

    // 0x0 image
    let empty_img = DynamicImage::ImageRgb8(RgbImage::new(0, 0));
    let (text, score) = engine.recognize_line(&empty_img)?;
    assert_eq!(text, "");
    assert_eq!(score, 0.0);

    // 1x1 image
    let tiny_img = DynamicImage::ImageRgb8(RgbImage::new(1, 1));
    let text = engine.recognize_image(&tiny_img)?;
    assert_eq!(text, "");

    // 3x3 image
    let tiny_img2 = DynamicImage::ImageRgb8(RgbImage::new(3, 3));
    let text = engine.recognize_image(&tiny_img2)?;
    assert_eq!(text, "");

    Ok(())
}

#[test]
fn test_turkish_char_set_in_dict() -> Result<()> {
    let paths = ensure_models()?;
    let dict_content = std::fs::read_to_string(&paths.dict_file)?;

    let required_chars = ['ç', 'ğ', 'ı', 'İ', 'ö', 'ş', 'ü', 'Ç', 'Ğ', 'Ö', 'Ş', 'Ü'];
    for c in required_chars {
        assert!(
            dict_content.contains(c),
            "Character '{}' must exist in dictionary",
            c
        );
    }
    Ok(())
}

#[test]
fn test_synth_tesseract() -> Result<()> {
    if let Ok(img) = image::open("/tmp/synth_test.png") {
        let words = wayfrost::ocr::pipeline::extract_words_onnx_or_fallback(&img, "TR")?;
        println!(">>> extract_words_onnx_or_fallback words count: {}", words.len());
        let refs: Vec<&_> = words.iter().collect();
        let text = wayfrost::ocr::pipeline::join_words(&refs);
        println!(">>> Output on synth test:\n{}", text);
    }
    Ok(())
}

#[test]
fn test_capture_now() {
    if let Ok(img) = wayfrost::capture::capture_screen() {
        println!(">>> Captured screen: {}x{}", img.width(), img.height());
        let _ = img.save("/tmp/captured_live.png");
    } else {
        println!(">>> Capture failed!");
    }
}

#[test]
fn test_onnx_rec() -> Result<()> {
    let paths = ensure_models()?;
    let mut engine = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file)?;

    if let Ok(img) = image::open("/tmp/clean_52.png") {
        let crop = image::imageops::crop_imm(&img, 394, 258, 904, 383).to_image();
        let dyn_crop = DynamicImage::ImageRgba8(crop);
        let res = engine.recognize_image(&dyn_crop)?;
        println!(">>> ONNX OCR recognize_image result:\n{}", res);
    }
    Ok(())
}

#[test]
fn test_det_session_meta() -> Result<()> {
    let paths = ensure_models()?;
    let mut det_session = ort::session::Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(&paths.det_model)?;
    for (i, input) in det_session.inputs().iter().enumerate() {
        println!("Det Input {}: name={}", i, input.name());
    }
    for (i, output) in det_session.outputs().iter().enumerate() {
        println!("Det Output {}: name={}", i, output.name());
    }

    // Test running inference with a dummy 960x960 image
    let h = 960usize;
    let w = 960usize;
    let input_array = ndarray::Array4::<f32>::zeros((1, 3, h, w));
    let input_tensor = ort::value::Tensor::from_array(input_array)?;
    let outputs = det_session.run(ort::inputs!["x" => input_tensor])?;
    let mut rec_session = ort::session::Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(&paths.rec_model)?;
    for (i, input) in rec_session.inputs().iter().enumerate() {
        println!("Rec Input {}: name={}", i, input.name());
    }
    for (i, output) in rec_session.outputs().iter().enumerate() {
        println!("Rec Output {}: name={}", i, output.name());
    }
    let dict_lines = std::fs::read_to_string(&paths.dict_file)?.lines().count();
    println!("Dict lines count: {}", dict_lines);

    // Dummy inference to see shape
    let dummy_in = ndarray::Array4::<f32>::zeros((1, 3, 48, 320));
    let tensor = ort::value::Tensor::from_array(dummy_in)?;
    let rec_out = rec_session.run(ort::inputs!["x" => tensor])?;
    let (rshape, _) = rec_out["fetch_name_0"].try_extract_tensor::<f32>()?;
    println!("Rec Output shape: {:?}", rshape);
    Ok(())
}

#[test]
fn test_full_onnx_pipeline() -> Result<()> {
    use image::GenericImageView;
    let paths = ensure_models()?;
    let mut det_session = ort::session::Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(4)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(&paths.det_model)?;

    let mut engine = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file)?;

    if let Ok(img) = image::open("/tmp/clean_52.png") {
        let (orig_w, orig_h) = img.dimensions();
        // Resize to multiple of 32, max dimension <= 960
        let max_dim = 960.0f32;
        let scale = (max_dim / (orig_w.max(orig_h) as f32)).min(1.0);
        let target_w = (((orig_w as f32 * scale).round() as u32 / 32) * 32).max(32);
        let target_h = (((orig_h as f32 * scale).round() as u32 / 32) * 32).max(32);

        let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
        let rgb = resized.to_rgb8();

        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];

        let mut input_array = ndarray::Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));
        for y in 0..target_h as usize {
            for x in 0..target_w as usize {
                let p = rgb.get_pixel(x as u32, y as u32);
                input_array[[0, 0, y, x]] = (p[0] as f32 / 255.0 - mean[0]) / std[0];
                input_array[[0, 1, y, x]] = (p[1] as f32 / 255.0 - mean[1]) / std[1];
                input_array[[0, 2, y, x]] = (p[2] as f32 / 255.0 - mean[2]) / std[2];
            }
        }

        let input_tensor = ort::value::Tensor::from_array(input_array)?;
        let start_time = std::time::Instant::now();
        let outputs = det_session.run(ort::inputs!["x" => input_tensor])?;
        let (_shape, data) = outputs["sigmoid_0.tmp_0"].try_extract_tensor::<f32>()?;
        println!(">>> Det time: {:?}", start_time.elapsed());

        // Simple connected components over threshold 0.3
        let th = 0.3f32;
        let mut visited = vec![false; (target_h * target_w) as usize];
        let mut boxes = Vec::new();

        let scale_x = orig_w as f64 / target_w as f64;
        let scale_y = orig_h as f64 / target_h as f64;

        for y in 0..target_h as usize {
            for x in 0..target_w as usize {
                let idx = y * target_w as usize + x;
                if visited[idx] || data[idx] < th {
                    continue;
                }

                // BFS flood-fill
                let mut queue = std::collections::VecDeque::new();
                queue.push_back((x, y));
                visited[idx] = true;

                let mut min_x = x;
                let mut max_x = x;
                let mut min_y = y;
                let mut max_y = y;
                let mut count = 0;

                while let Some((cx, cy)) = queue.pop_front() {
                    count += 1;
                    min_x = min_x.min(cx);
                    max_x = max_x.max(cx);
                    min_y = min_y.min(cy);
                    max_y = max_y.max(cy);

                    for (dx, dy) in &[(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        let nx = cx as isize + dx;
                        let ny = cy as isize + dy;
                        if nx >= 0 && nx < target_w as isize && ny >= 0 && ny < target_h as isize {
                            let n_idx = ny as usize * target_w as usize + nx as usize;
                            if !visited[n_idx] && data[n_idx] >= th {
                                visited[n_idx] = true;
                                queue.push_back((nx as usize, ny as usize));
                            }
                        }
                    }
                }

                if count >= 16 && (max_x - min_x) >= 6 && (max_y - min_y) >= 6 {
                    // Expand slightly
                    let pad = 2;
                    let bx = min_x.saturating_sub(pad);
                    let by = min_y.saturating_sub(pad);
                    let bw = (max_x - min_x + 1 + pad * 2).min(target_w as usize - bx);
                    let bh = (max_y - min_y + 1 + pad * 2).min(target_h as usize - by);

                    let ox = (bx as f64 * scale_x).round() as u32;
                    let oy = (by as f64 * scale_y).round() as u32;
                    let ow = (bw as f64 * scale_x).round() as u32;
                    let oh = (bh as f64 * scale_y).round() as u32;

                    boxes.push((ox, oy, ow, oh));
                }
            }
        }

        // Sort boxes in reading order
        boxes.sort_by(|a, b| {
            let line_a = a.1 / 16;
            let line_b = b.1 / 16;
            line_a.cmp(&line_b).then_with(|| a.0.cmp(&b.0))
        });

        println!(">>> Detected {} text boxes", boxes.len());
        let rec_start = std::time::Instant::now();
        for (i, &(bx, by, bw, bh)) in boxes.iter().take(20).enumerate() {
            if bx + bw <= orig_w && by + bh <= orig_h {
                let crop = image::imageops::crop_imm(&img, bx, by, bw, bh).to_image();
                let (text, score) = engine.recognize_line(&DynamicImage::ImageRgba8(crop))?;
                if !text.is_empty() {
                    println!("Box {}: (x={}, y={}, w={}, h={}) [{:.2}] -> '{}'", i, bx, by, bw, bh, score, text);
                }
            }
        }
        println!(">>> Rec time for 20 boxes: {:?}", rec_start.elapsed());
    }
    Ok(())
}

#[test]
fn test_onnx_on_turkish_screen() -> Result<()> {
    let paths = ensure_models()?;
    let mut engine = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file)?;

    if let Ok(img) = image::open("/tmp/test_turkish_screen.png") {
        let res = engine.recognize_image(&img)?;
        println!(">>> ONNX result on turkish screen:\n{}", res);
    }
    Ok(())
}


