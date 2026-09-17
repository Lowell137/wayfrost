use anyhow::{Context, Result};
use image::{imageops, DynamicImage, GenericImageView, GrayImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

static GLOBAL_OCR_ENGINE: Mutex<Option<OcrEngine>> = Mutex::new(None);

pub struct OcrEngine {
    det_session: Session,
    rec_session: Session,
    char_dict: Vec<String>,
}

impl OcrEngine {
    pub fn new(det_model_path: &Path, rec_model_path: &Path, dict_path: &Path) -> Result<Self> {
        let det_session = Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(det_model_path)
            .context("Failed to load detection model")?;

        let rec_session = Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(rec_model_path)
            .context("Failed to load recognition model")?;

        let file = File::open(dict_path).context("Failed to open dictionary file")?;
        let reader = BufReader::new(file);
        let mut char_dict = Vec::new();
        for line in reader.lines() {
            char_dict.push(line?);
        }
        char_dict.push(" ".to_string());

        Ok(Self {
            det_session,
            rec_session,
            char_dict,
        })
    }

    pub fn detect_text_boxes(&mut self, img: &DynamicImage) -> Result<Vec<(u32, u32, u32, u32)>> {
        let (orig_w, orig_h) = img.dimensions();
        if orig_w < 16 || orig_h < 16 {
            return Ok(vec![(0, 0, orig_w, orig_h)]);
        }

        let max_dim = 960.0f32;
        let scale = (max_dim / (orig_w.max(orig_h) as f32)).min(1.0);
        let target_w = (((orig_w as f32 * scale).round() as u32 / 32) * 32).max(32);
        let target_h = (((orig_h as f32 * scale).round() as u32 / 32) * 32).max(32);

        let resized = img.resize_exact(target_w, target_h, imageops::FilterType::Triangle);
        let rgb = resized.to_rgb8();

        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];

        let mut input_array = Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));
        for y in 0..target_h as usize {
            for x in 0..target_w as usize {
                let p = rgb.get_pixel(x as u32, y as u32);
                input_array[[0, 0, y, x]] = (p[0] as f32 / 255.0 - mean[0]) / std[0];
                input_array[[0, 1, y, x]] = (p[1] as f32 / 255.0 - mean[1]) / std[1];
                input_array[[0, 2, y, x]] = (p[2] as f32 / 255.0 - mean[2]) / std[2];
            }
        }

        let input_tensor = Tensor::from_array(input_array)?;
        let outputs = self.det_session.run(ort::inputs!["x" => input_tensor])?;
        let (_shape, data) = outputs["sigmoid_0.tmp_0"].try_extract_tensor::<f32>()?;

        let th = 0.30f32;
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

        boxes.sort_by(|a, b| {
            let line_a = a.1 / 16;
            let line_b = b.1 / 16;
            line_a.cmp(&line_b).then_with(|| a.0.cmp(&b.0))
        });

        Ok(boxes)
    }

    pub fn detect_and_recognize(&mut self, img: &DynamicImage) -> Result<Vec<DetectedWord>> {
        let (orig_w, orig_h) = img.dimensions();
        if orig_w < 4 || orig_h < 4 {
            return Ok(Vec::new());
        }

        let boxes = if orig_h <= 48 && orig_w <= 800 {
            vec![(0, (0, 0, orig_w, orig_h))]
        } else {
            let mut detected = self.detect_text_boxes(img)?;
            if detected.is_empty() {
                vec![(0, (0, 0, orig_w, orig_h))]
            } else {
                detected.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                let mut line_assignments = Vec::new();
                let mut current_line = 0usize;
                let mut last_y: Option<u32> = None;
                let mut last_h: Option<u32> = None;

                for &(bx, by, bw, bh) in &detected {
                    if let (Some(ly), Some(lh)) = (last_y, last_h) {
                        let diff = (by as i32 - ly as i32).abs();
                        let max_tol = (lh.min(bh) as i32) / 2;
                        if diff > max_tol.max(8) {
                            current_line += 1;
                            last_y = Some(by);
                            last_h = Some(bh);
                        }
                    } else {
                        last_y = Some(by);
                        last_h = Some(bh);
                    }
                    line_assignments.push((current_line, (bx, by, bw, bh)));
                }

                line_assignments.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| (a.1).0.cmp(&(b.1).0)));
                line_assignments
            }
        };

        let mut words = Vec::new();
        for (line_num, (bx, by, bw, bh)) in boxes {
            if bx + bw > orig_w || by + bh > orig_h || bw < 4 || bh < 4 {
                continue;
            }

            let crop = imageops::crop_imm(img, bx, by, bw, bh).to_image();
            let (text, score) = self.recognize_line(&DynamicImage::ImageRgba8(crop))?;
            let trimmed = text.trim();
            if trimmed.is_empty() || score < 0.25 {
                continue;
            }

            let word_tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if word_tokens.is_empty() {
                continue;
            }

            let total_chars: usize = word_tokens.iter().map(|w| w.chars().count()).sum();
            let total_chars = total_chars.max(1) as f64;
            let space_count = word_tokens.len().saturating_sub(1) as f64;
            let total_weight = total_chars + space_count * 0.5;

            let char_width = bw as f64 / total_weight;
            let space_width = char_width * 0.5;

            let mut current_x = bx as f64;
            for token in word_tokens {
                let w_len = token.chars().count() as f64;
                let w_width = (w_len * char_width).max(4.0);

                words.push(DetectedWord {
                    x: current_x,
                    y: by as f64,
                    w: w_width,
                    h: bh as f64,
                    text: token.to_string(),
                    line_num,
                    par_num: 0,
                    block_num: 0,
                });

                current_x += w_width + space_width;
            }
        }

        words.sort_by(|a, b| {
            (a.block_num, a.par_num, a.line_num)
                .cmp(&(b.block_num, b.par_num, b.line_num))
                .then_with(|| a.x.total_cmp(&b.x))
        });

        Ok(words)
    }

    /// Primary OCR entry point:
    /// Uses native embedded ONNX (ort) neural net first!
    /// Falls back to Tesseract if ONNX is unavailable.
    pub fn recognize(&mut self, img: &DynamicImage, lang: &str) -> Result<String> {
        let (w, h) = img.dimensions();
        if w < 4 || h < 4 {
            return Ok(String::new());
        }

        // 1. Try high-accuracy Tesseract (tessdata_best) first
        let preprocessed = preprocess_for_ocr(img);
        if let Ok(text) = run_tesseract(&preprocessed, lang) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

        // 2. Fallback to ONNX neural net
        if let Ok(words) = self.detect_and_recognize(img) {
            if !words.is_empty() {
                let sel_words: Vec<&DetectedWord> = words.iter().collect();
                let text = join_words(&sel_words);
                if !text.trim().is_empty() {
                    return Ok(text);
                }
            }
        }

        self.recognize_image(&preprocessed)
    }

    pub fn recognize_line(&mut self, img: &DynamicImage) -> Result<(String, f32)> {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return Ok((String::new(), 0.0));
        }

        let target_h = 48;
        let aspect = w as f32 / h as f32;
        let target_w = (target_h as f32 * aspect).max(48.0).round() as u32;

        let resized = img.resize_exact(target_w, target_h, imageops::FilterType::Triangle);
        let rgb_img = resized.to_rgb8();

        let mut input_array = Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));
        for y in 0..target_h as usize {
            for x in 0..target_w as usize {
                let pixel = rgb_img.get_pixel(x as u32, y as u32);
                input_array[[0, 0, y, x]] = (pixel[0] as f32 / 255.0 - 0.5) / 0.5;
                input_array[[0, 1, y, x]] = (pixel[1] as f32 / 255.0 - 0.5) / 0.5;
                input_array[[0, 2, y, x]] = (pixel[2] as f32 / 255.0 - 0.5) / 0.5;
            }
        }

        let input_tensor = Tensor::from_array(input_array)?;
        let outputs = self.rec_session.run(ort::inputs!["x" => input_tensor])?;

        let (shape, data) = outputs["fetch_name_0"].try_extract_tensor::<f32>()?;
        let seq_len = shape[1] as usize;
        let num_classes = shape[2] as usize;

        let mut recognized_text = String::new();
        let mut total_score = 0.0;
        let mut char_count = 0;
        let mut prev_idx = 0;

        for t in 0..seq_len {
            let offset = t * num_classes;
            let step_slice = &data[offset..offset + num_classes];

            let mut max_idx = 0;
            let mut max_val = f32::NEG_INFINITY;
            for (idx, &val) in step_slice.iter().enumerate() {
                if val > max_val {
                    max_val = val;
                    max_idx = idx;
                }
            }

            if max_idx != 0 && max_idx != prev_idx {
                let dict_idx = max_idx - 1;
                if dict_idx < self.char_dict.len() {
                    recognized_text.push_str(&self.char_dict[dict_idx]);
                    total_score += max_val;
                    char_count += 1;
                }
            }
            prev_idx = max_idx;
        }

        let avg_score = if char_count > 0 {
            total_score / char_count as f32
        } else {
            0.0
        };

        Ok((recognized_text, avg_score))
    }

    pub fn recognize_image(&mut self, img: &DynamicImage) -> Result<String> {
        let (w, h) = img.dimensions();
        if w < 4 || h < 4 {
            return Ok(String::new());
        }

        if h <= 64 {
            let (text, _) = self.recognize_line(img)?;
            return Ok(text.trim().to_string());
        }

        let gray: GrayImage = img.to_luma8();
        let mut row_scores = vec![0.0f32; h as usize];

        for y in 0..h as usize {
            let mut diff_sum = 0.0f32;
            for x in 1..w as usize {
                let p1 = gray.get_pixel(x as u32, y as u32)[0] as f32;
                let p0 = gray.get_pixel((x - 1) as u32, y as u32)[0] as f32;
                diff_sum += (p1 - p0).abs();
            }
            row_scores[y] = diff_sum / w as f32;
        }

        let avg_activity: f32 = row_scores.iter().sum::<f32>() / (h as f32).max(1.0);
        let threshold = (avg_activity * 0.35).max(1.0);

        let mut lines = Vec::new();
        let mut in_line = false;
        let mut line_start = 0;

        for (y, &score) in row_scores.iter().enumerate() {
            if score > threshold {
                if !in_line {
                    in_line = true;
                    line_start = y.saturating_sub(4);
                }
            } else if in_line {
                let line_end = (y + 4).min(h as usize);
                if line_end - line_start >= 12 {
                    lines.push((line_start, line_end));
                }
                in_line = false;
            }
        }

        if in_line {
            let line_end = h as usize;
            if line_end - line_start >= 12 {
                lines.push((line_start, line_end));
            }
        }

        if lines.is_empty() {
            let (text, _) = self.recognize_line(img)?;
            return Ok(text.trim().to_string());
        }

        let mut results = Vec::new();
        for (start_y, end_y) in lines {
            let line_h = (end_y - start_y) as u32;
            let crop = imageops::crop_imm(img, 0, start_y as u32, w, line_h).to_image();
            let (text, conf) = self.recognize_line(&DynamicImage::ImageRgba8(crop))?;
            let trimmed = text.trim();
            if !trimmed.is_empty() && conf > -5.0 {
                results.push(trimmed.to_string());
            }
        }

        Ok(results.join("\n"))
    }
}

/// Preprocesses image for OCR:
/// - Detects if text is light on dark (dark mode) and inverts to dark on light
/// - Upscales low-resolution screen text for maximum character accuracy
pub fn preprocess_for_ocr(img: &DynamicImage) -> DynamicImage {
    let gray = img.to_luma8();
    let total_pixels = (gray.width() * gray.height()).max(1) as u64;
    let sum_luma: u64 = gray.pixels().map(|p| p[0] as u64).sum();
    let avg_luma = (sum_luma / total_pixels) as u8;

    // Invert if background is dark
    let mut processed = if avg_luma < 128 {
        let mut inverted = img.to_rgba8();
        for p in inverted.pixels_mut() {
            p[0] = 255 - p[0];
            p[1] = 255 - p[1];
            p[2] = 255 - p[2];
        }
        DynamicImage::ImageRgba8(inverted)
    } else {
        img.clone()
    };

    // Upscale small text so OCR can recognize individual character details
    let (w, h) = processed.dimensions();
    if h < 80 {
        let scale = (110.0 / h as f32).max(2.0);
        let new_w = (w as f32 * scale).round() as u32;
        let new_h = (h as f32 * scale).round() as u32;
        processed = processed.resize_exact(new_w, new_h, imageops::FilterType::Triangle);
    }

    processed
}

/// Runs native tesseract with stdin/stdout pipe
pub fn run_tesseract(img: &DynamicImage, lang: &str) -> Result<String> {
    let mut png_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )?;

    let tesseract_lang = match lang {
        "EN" => "eng",
        _ => "tur+eng",
    };

    let mut cmd = Command::new("tesseract");
    cmd.arg("stdin").arg("stdout");

    if let Ok(models_dir) = crate::ocr::model_manager::get_models_dir() {
        let tessdata_dir = models_dir.join("tessdata");
        if tessdata_dir.join("tur.traineddata").exists() {
            cmd.arg("--tessdata-dir").arg(tessdata_dir);
        }
    }

    let mut child = cmd
        .arg("-l")
        .arg(tesseract_lang)
        .arg("--psm")
        .arg("6")
        .env("OMP_THREAD_LIMIT", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn tesseract")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&png_bytes)?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("tesseract exited with error");
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(text)
}

#[derive(Clone, Debug, Default)]
pub struct DetectedWord {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub text: String,
    pub line_num: usize,
    pub par_num: usize,
    pub block_num: usize,
}

pub fn run_tesseract_tsv(img: &DynamicImage, lang: &str) -> Result<Vec<DetectedWord>> {
    let gray = img.to_luma8();
    let total_pixels = (gray.width() * gray.height()).max(1) as u64;
    let sum_luma: u64 = gray.pixels().map(|p| p[0] as u64).sum();
    let avg_luma = (sum_luma / total_pixels) as u8;

    let mut processed = if avg_luma < 128 {
        let mut inverted = img.to_rgba8();
        for p in inverted.pixels_mut() {
            p[0] = 255 - p[0];
            p[1] = 255 - p[1];
            p[2] = 255 - p[2];
        }
        DynamicImage::ImageRgba8(inverted)
    } else {
        img.clone()
    };

    let (orig_w, orig_h) = processed.dimensions();
    let mut scale = 1.0f64;
    if orig_h < 80 && orig_h > 0 {
        scale = (110.0 / orig_h as f64).max(2.0);
        let new_w = (orig_w as f64 * scale).round() as u32;
        let new_h = (orig_h as f64 * scale).round() as u32;
        processed = processed.resize_exact(new_w, new_h, imageops::FilterType::Triangle);
    }

    let mut png_bytes = Vec::new();
    processed.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )?;

    let tesseract_lang = match lang {
        "EN" => "eng",
        _ => "tur+eng",
    };

    let mut cmd = Command::new("tesseract");
    cmd.arg("stdin").arg("stdout");

    if let Ok(models_dir) = crate::ocr::model_manager::get_models_dir() {
        let tessdata_dir = models_dir.join("tessdata");
        if tessdata_dir.join("tur.traineddata").exists() {
            cmd.arg("--tessdata-dir").arg(tessdata_dir);
        }
    }

    let mut child = cmd
        .arg("-l")
        .arg(tesseract_lang)
        .arg("--psm")
        .arg("6")
        .arg("tsv")
        .env("OMP_THREAD_LIMIT", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn tesseract tsv")?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&png_bytes);
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("tesseract tsv exited with error");
    }

    let tsv_str = String::from_utf8_lossy(&output.stdout);
    let mut words = Vec::new();

    for line in tsv_str.lines().skip(1) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 12 {
            let level = parts[0];
            let block_num = parts[2].parse::<usize>().unwrap_or(0);
            let par_num = parts[3].parse::<usize>().unwrap_or(0);
            let line_num = parts[4].parse::<usize>().unwrap_or(0);
            let left = parts[6].parse::<f64>().unwrap_or(0.0) / scale;
            let top = parts[7].parse::<f64>().unwrap_or(0.0) / scale;
            let width = parts[8].parse::<f64>().unwrap_or(0.0) / scale;
            let height = parts[9].parse::<f64>().unwrap_or(0.0) / scale;

            if level == "5" {
                let text = parts[11].trim().to_string();
                if !text.is_empty() {
                    words.push(DetectedWord {
                        x: left,
                        y: top,
                        w: width,
                        h: height,
                        text,
                        line_num,
                        par_num,
                        block_num,
                    });
                }
            }
        }
    }

    // Spatially cluster words into lines using visual vertical centers and overlap
    words.sort_by(|a, b| {
        let cy_a = a.y + a.h / 2.0;
        let cy_b = b.y + b.h / 2.0;
        cy_a.total_cmp(&cy_b).then_with(|| a.x.total_cmp(&b.x))
    });

    let mut line_clusters: Vec<Vec<DetectedWord>> = Vec::new();
    for w in words {
        let cy = w.y + w.h / 2.0;
        let mut found_line = false;
        for line in &mut line_clusters {
            let line_cy = line.iter().map(|item| item.y + item.h / 2.0).sum::<f64>() / line.len() as f64;
            let line_h = line.iter().map(|item| item.h).sum::<f64>() / line.len() as f64;
            if (cy - line_cy).abs() < line_h * 0.5 {
                line.push(w.clone());
                found_line = true;
                break;
            }
        }
        if !found_line {
            line_clusters.push(vec![w]);
        }
    }

    line_clusters.sort_by(|a, b| {
        let cy_a = a.iter().map(|w| w.y + w.h / 2.0).sum::<f64>() / a.len() as f64;
        let cy_b = b.iter().map(|w| w.y + w.h / 2.0).sum::<f64>() / b.len() as f64;
        cy_a.total_cmp(&cy_b)
    });

    let mut final_words = Vec::new();
    let mut current_par = 0usize;
    let mut prev_line_bottom: Option<f64> = None;
    let mut prev_line_h: Option<f64> = None;

    for (line_idx, line) in line_clusters.iter_mut().enumerate() {
        line.sort_by(|a, b| a.x.total_cmp(&b.x));

        let line_top = line.iter().map(|w| w.y).fold(f64::INFINITY, f64::min);
        let line_bottom = line.iter().map(|w| w.y + w.h).fold(f64::NEG_INFINITY, f64::max);
        let line_h = (line_bottom - line_top).max(1.0);

        if let (Some(pb), Some(ph)) = (prev_line_bottom, prev_line_h) {
            let gap = line_top - pb;
            if gap > ph * 0.65 {
                current_par += 1;
            }
        }

        for i in 0..line.len().saturating_sub(1) {
            let next_x = line[i + 1].x;
            if line[i].x + line[i].w >= next_x {
                line[i].w = (next_x - 1.0 - line[i].x).max(1.0);
            }
        }

        for w in line {
            w.block_num = 0;
            w.par_num = current_par;
            w.line_num = line_idx;
            final_words.push(w.clone());
        }

        prev_line_bottom = Some(line_bottom);
        prev_line_h = Some(line_h);
    }

    Ok(final_words)
}

pub fn join_words(words: &[&DetectedWord]) -> String {
    if words.is_empty() {
        return String::new();
    }

    let mut sorted: Vec<&DetectedWord> = words.to_vec();
    sorted.sort_by(|a, b| {
        let cy_a = a.y + a.h / 2.0;
        let cy_b = b.y + b.h / 2.0;
        cy_a.total_cmp(&cy_b).then_with(|| a.x.total_cmp(&b.x))
    });

    let mut line_clusters: Vec<Vec<&DetectedWord>> = Vec::new();
    for w in sorted {
        let cy = w.y + w.h / 2.0;
        let mut found_line = false;
        for line in &mut line_clusters {
            let line_cy = line.iter().map(|item| item.y + item.h / 2.0).sum::<f64>() / line.len() as f64;
            let line_h = line.iter().map(|item| item.h).sum::<f64>() / line.len() as f64;
            if (cy - line_cy).abs() < line_h * 0.45 {
                line.push(w);
                found_line = true;
                break;
            }
        }
        if !found_line {
            line_clusters.push(vec![w]);
        }
    }

    line_clusters.sort_by(|a, b| {
        let cy_a = a.iter().map(|w| w.y + w.h / 2.0).sum::<f64>() / a.len() as f64;
        let cy_b = b.iter().map(|w| w.y + w.h / 2.0).sum::<f64>() / b.len() as f64;
        cy_a.total_cmp(&cy_b)
    });

    let mut result = String::new();
    let mut prev_bottom: Option<f64> = None;
    let mut prev_h: Option<f64> = None;

    for line in line_clusters.iter_mut() {
        line.sort_by(|a, b| a.x.total_cmp(&b.x));

        let line_top = line.iter().map(|w| w.y).fold(f64::INFINITY, f64::min);
        let line_bottom = line.iter().map(|w| w.y + w.h).fold(f64::NEG_INFINITY, f64::max);
        let line_h = (line_bottom - line_top).max(1.0);

        if let (Some(pb), Some(ph)) = (prev_bottom, prev_h) {
            let gap = line_top - pb;
            if gap > ph * 0.65 {
                result.push_str("\n\n");
            } else {
                result.push('\n');
            }
        }

        for (j, w) in line.iter().enumerate() {
            if j > 0 {
                result.push(' ');
            }
            result.push_str(&w.text);
        }

        prev_bottom = Some(line_bottom);
        prev_h = Some(line_h);
    }

    result
}

pub fn init_global_ocr() {
    let mut guard = match GLOBAL_OCR_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.is_none() {
        if let Ok(paths) = crate::ocr::model_manager::ensure_models() {
            if let Ok(engine) = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file) {
                *guard = Some(engine);
                log::info!("Global ONNX OCR engine initialized successfully");
            }
        }
    }
}

pub fn extract_words_onnx_or_fallback(img: &DynamicImage, lang: &str) -> Result<Vec<DetectedWord>> {
    // 1. High-accuracy Tesseract with tessdata_best (99%+ accuracy on Turkish and English)
    if let Ok(words) = run_tesseract_tsv(img, lang) {
        if !words.is_empty() {
            return Ok(words);
        }
    }

    // 2. Fallback to ONNX neural net
    {
        let mut guard = match GLOBAL_OCR_ENGINE.lock() {
            Ok(g) => g,
            Err(_) => return Ok(Vec::new()),
        };
        if guard.is_none() {
            if let Ok(paths) = crate::ocr::model_manager::ensure_models() {
                if let Ok(engine) = OcrEngine::new(&paths.det_model, &paths.rec_model, &paths.dict_file) {
                    *guard = Some(engine);
                }
            }
        }

        if let Some(engine) = guard.as_mut() {
            if let Ok(words) = engine.detect_and_recognize(img) {
                if !words.is_empty() {
                    return Ok(words);
                }
            }
        }
    }

    Ok(Vec::new())
}

