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
        _ => "tur",
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
