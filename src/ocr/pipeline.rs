use anyhow::{Context, Result};
use image::{imageops, DynamicImage, GenericImageView, GrayImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub struct OcrEngine {
    rec_session: Session,
    char_dict: Vec<String>,
}

impl OcrEngine {
    pub fn new(rec_model_path: &Path, dict_path: &Path) -> Result<Self> {
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
            rec_session,
            char_dict,
        })
    }

    /// Primary OCR entry point:
    /// Uses native Tesseract (tur+eng) if installed, with automatic dark-mode inversion and resolution upscaling.
    /// Falls back to embedded ONNX PaddleOCR if Tesseract is not available.
    pub fn recognize(&mut self, img: &DynamicImage, lang: &str) -> Result<String> {
        let (w, h) = img.dimensions();
        if w < 4 || h < 4 {
            return Ok(String::new());
        }

        // 1. Preprocess: Dark mode inversion + upscaling for crisp OCR
        let preprocessed = preprocess_for_ocr(img);

        // 2. Try native Tesseract first (fastest, full Turkish dictionary)
        if let Ok(text) = run_tesseract(&preprocessed, lang) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

        // 3. Fallback to ONNX neural net
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

    let mut child = Command::new("tesseract")
        .arg("stdin")
        .arg("stdout")
        .arg("-l")
        .arg(tesseract_lang)
        .arg("--psm")
        .arg("6")
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

    let (pw, ph) = processed.dimensions();
    let rgb = processed.to_rgb8();
    let mut ppm_bytes = format!("P6\n{} {}\n255\n", pw, ph).into_bytes();
    ppm_bytes.extend_from_slice(&rgb.into_raw());

    let tesseract_lang = match lang {
        "EN" => "eng",
        _ => "tur+eng",
    };

    let mut child = Command::new("tesseract")
        .arg("stdin")
        .arg("stdout")
        .arg("-l")
        .arg(tesseract_lang)
        .arg("tsv")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn tesseract tsv")?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&ppm_bytes);
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

            let conf = parts[10].parse::<f32>().unwrap_or(0.0);

            if level == "5" {
                let text = parts[11].trim().to_string();
                if !text.is_empty() && width >= 4.0 && height >= 6.0 && conf >= 15.0 {
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

    // Deduplicate overlapping boxes (NMS):
    // If two words overlap by > 50% of either word's area and their vertical centers are aligned,
    // keep the one with larger area or earlier detection.
    let mut clean_words: Vec<DetectedWord> = Vec::with_capacity(words.len());
    for w in words {
        let mut dup = false;
        let w_area = w.w * w.h;
        for existing in clean_words.iter() {
            let ox = (w.x + w.w).min(existing.x + existing.w) - w.x.max(existing.x);
            let oy = (w.y + w.h).min(existing.y + existing.h) - w.y.max(existing.y);
            if ox > 0.0 && oy > 0.0 {
                let inter_area = ox * oy;
                let min_area = w_area.min(existing.w * existing.h);
                if min_area > 0.0 && (inter_area / min_area) > 0.50 {
                    dup = true;
                    break;
                }
            }
        }
        if !dup {
            clean_words.push(w);
        }
    }
    let mut words = clean_words;

    // Sort words by visual reading order (top-to-bottom, left-to-right)
    words.sort_by(|a, b| {
        let cy_a = a.y + a.h / 2.0;
        let cy_b = b.y + b.h / 2.0;
        let line_h = a.h.max(b.h);
        if (cy_a - cy_b).abs() < line_h * 0.45 {
            a.x.total_cmp(&b.x)
        } else {
            cy_a.total_cmp(&cy_b)
        }
    });

    Ok(words)
}

/// Formats detected words into clean text using visual line and paragraph breaks
pub fn join_words(words: &[&DetectedWord]) -> String {
    if words.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            result.push_str(&w.text);
            continue;
        }
        let prev = words[i - 1];
        let prev_cy = prev.y + prev.h / 2.0;
        let curr_cy = w.y + w.h / 2.0;
        let line_h = prev.h.max(w.h);

        if (curr_cy - prev_cy).abs() < line_h * 0.50 {
            // Same visual line
            result.push(' ');
        } else {
            // New visual line: check for paragraph gap
            let y_gap = w.y - (prev.y + prev.h);
            if y_gap > line_h * 0.85 {
                result.push_str("\n\n");
            } else {
                result.push('\n');
            }
        }
        result.push_str(&w.text);
    }

    result
}

/// Clusters word indices into visual lines ordered top-to-bottom,
/// and left-to-right within each line.
pub fn cluster_words_into_lines(words: &[DetectedWord], candidates: &[usize]) -> Vec<Vec<usize>> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut sorted_indices = candidates.to_vec();
    sorted_indices.sort_by(|&a, &b| {
        let cy_a = words[a].y + words[a].h / 2.0;
        let cy_b = words[b].y + words[b].h / 2.0;
        cy_a.total_cmp(&cy_b)
    });

    struct LineGroup {
        y_min: f64,
        y_max: f64,
        cy: f64,
        members: Vec<usize>,
    }

    let mut lines: Vec<LineGroup> = Vec::new();

    for idx in sorted_indices {
        let w = &words[idx];
        let cy = w.y + w.h / 2.0;
        let h = w.h;

        let mut placed = false;
        for line in lines.iter_mut() {
            let line_h = line.y_max - line.y_min;
            let threshold = h.max(line_h) * 0.55;
            if (cy - line.cy).abs() < threshold {
                line.members.push(idx);
                line.y_min = line.y_min.min(w.y);
                line.y_max = line.y_max.max(w.y + w.h);
                let count = line.members.len() as f64;
                line.cy = line.members.iter().map(|&i| words[i].y + words[i].h / 2.0).sum::<f64>() / count;
                placed = true;
                break;
            }
        }

        if !placed {
            lines.push(LineGroup {
                y_min: w.y,
                y_max: w.y + w.h,
                cy,
                members: vec![idx],
            });
        }
    }

    lines.sort_by(|a, b| a.y_min.total_cmp(&b.y_min));

    let mut result = Vec::with_capacity(lines.len());
    for mut line in lines {
        line.members.sort_by(|&a, &b| words[a].x.total_cmp(&words[b].x));
        result.push(line.members);
    }

    result
}

fn get_word_at_point(
    x: f64,
    y: f64,
    lines: &[Vec<usize>],
    words: &[DetectedWord],
) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }

    // 1. Find line closest to y
    let mut best_li = 0;
    let mut best_dist_y = f64::INFINITY;

    for (li, line) in lines.iter().enumerate() {
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        for &idx in line {
            let w = &words[idx];
            y_min = y_min.min(w.y);
            y_max = y_max.max(w.y + w.h);
        }

        if y >= y_min && y <= y_max {
            best_li = li;
            break;
        }

        let dist = if y < y_min { y_min - y } else { y - y_max };
        if dist < best_dist_y {
            best_dist_y = dist;
            best_li = li;
        }
    }

    let line = &lines[best_li];
    if line.is_empty() {
        return (best_li, 0);
    }

    // 2. Find word on that line closest to x
    let mut best_wi = 0;
    let mut best_dist_x = f64::INFINITY;

    for (wi, &idx) in line.iter().enumerate() {
        let w = &words[idx];
        if x >= w.x && x <= w.x + w.w {
            return (best_li, wi);
        }
        let dist = if x < w.x { w.x - x } else { x - (w.x + w.w) };
        if dist < best_dist_x {
            best_dist_x = dist;
            best_wi = wi;
        }
    }

    (best_li, best_wi)
}

/// Selects words in natural reading stream order between start_pt and curr_pt.
pub fn select_words_stream(
    start_pt: (f64, f64),
    curr_pt: (f64, f64),
    lines: &[Vec<usize>],
    words: &[DetectedWord],
) -> Vec<usize> {
    if lines.is_empty() {
        return Vec::new();
    }

    let (s_li, s_wi) = get_word_at_point(start_pt.0, start_pt.1, lines, words);
    let (c_li, c_wi) = get_word_at_point(curr_pt.0, curr_pt.1, lines, words);

    let forward = s_li < c_li || (s_li == c_li && s_wi <= c_wi);

    let (from_li, from_wi, to_li, to_wi) = if forward {
        (s_li, s_wi, c_li, c_wi)
    } else {
        (c_li, c_wi, s_li, s_wi)
    };

    let mut selected = Vec::new();
    for li in from_li..=to_li {
        let line = &lines[li];
        let start_w = if li == from_li { from_wi } else { 0 };
        let end_w = if li == to_li { to_wi } else { line.len() - 1 };

        for wi in start_w..=end_w {
            if let Some(&idx) = line.get(wi) {
                selected.push(idx);
            }
        }
    }

    selected
}

/// Selects words swept by a drag gesture from start to current in reading order.
pub fn select_in_drag(
    start: (f64, f64),
    current: (f64, f64),
    words: &[DetectedWord],
    candidates: &[usize],
) -> Vec<usize> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let min_x = start.0.min(current.0);
    let max_x = start.0.max(current.0);
    let min_y = start.1.min(current.1) - 4.0;
    let max_y = start.1.max(current.1) + 4.0;

    let in_rect: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&i| {
            let w = &words[i];
            !(w.x + w.w < min_x || w.x > max_x || w.y + w.h < min_y || w.y > max_y)
        })
        .collect();

    let lines = cluster_words_into_lines(words, &in_rect);
    lines.into_iter().flatten().collect()
}

