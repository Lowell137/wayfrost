use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, GrayImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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
        // Add space character at the end (index 503)
        char_dict.push(" ".to_string());

        Ok(Self {
            rec_session,
            char_dict,
        })
    }

    /// Recognizes a single horizontal line of text.
    pub fn recognize_line(&mut self, img: &DynamicImage) -> Result<(String, f32)> {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return Ok((String::new(), 0.0));
        }

        let target_h = 48;
        let aspect = w as f32 / h as f32;
        let target_w = (target_h as f32 * aspect).max(48.0).round() as u32;

        let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
        let rgb_img = resized.to_rgb8();

        let mut input_array = Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));
        for y in 0..target_h as usize {
            for x in 0..target_w as usize {
                let pixel = rgb_img.get_pixel(x as u32, y as u32);
                // Standard PP-OCR normalization: (val / 255.0 - 0.5) / 0.5
                input_array[[0, 0, y, x]] = (pixel[0] as f32 / 255.0 - 0.5) / 0.5;
                input_array[[0, 1, y, x]] = (pixel[1] as f32 / 255.0 - 0.5) / 0.5;
                input_array[[0, 2, y, x]] = (pixel[2] as f32 / 255.0 - 0.5) / 0.5;
            }
        }

        let input_tensor = Tensor::from_array(input_array)?;
        let outputs = self.rec_session.run(ort::inputs!["x" => input_tensor])?;

        // Output shape: [1, seq_len, 504]
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

            // Find argmax
            let mut max_idx = 0;
            let mut max_val = f32::NEG_INFINITY;
            for (idx, &val) in step_slice.iter().enumerate() {
                if val > max_val {
                    max_val = val;
                    max_idx = idx;
                }
            }

            // CTC decoding: 0 is blank
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

    /// Recognizes text from an arbitrary cropped region.
    /// If multi-line, slices by horizontal projection valleys and merges with newlines.
    pub fn recognize_image(&mut self, img: &DynamicImage) -> Result<String> {
        let (w, h) = img.dimensions();
        if w < 4 || h < 4 {
            return Ok(String::new());
        }

        // For small or single-line crops, recognize directly
        if h <= 64 {
            let (text, _) = self.recognize_line(img)?;
            return Ok(text.trim().to_string());
        }

        // Multi-line segmentation via row projection profile
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

        // If no clean lines detected, process entire image
        if lines.is_empty() {
            let (text, _) = self.recognize_line(img)?;
            return Ok(text.trim().to_string());
        }

        let mut results = Vec::new();
        for (start_y, end_y) in lines {
            let line_h = (end_y - start_y) as u32;
            let crop = image::imageops::crop_imm(img, 0, start_y as u32, w, line_h).to_image();
            let (text, conf) = self.recognize_line(&DynamicImage::ImageRgba8(crop))?;
            let trimmed = text.trim();
            if !trimmed.is_empty() && conf > -5.0 {
                results.push(trimmed.to_string());
            }
        }

        Ok(results.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::model_manager::ensure_models;
    use gtk4::cairo;

    #[test]
    fn test_multiline_ocr_recognition() -> Result<()> {
        let paths = ensure_models()?;
        let mut engine = OcrEngine::new(&paths.rec_model, &paths.dict_file)?;

        let width = 500;
        let height = 120;
        let mut surface = cairo::ImageSurface::create(cairo::Format::Rgb24, width, height)
            .map_err(|e| anyhow::anyhow!("Cairo error: {:?}", e))?;
        let cr = cairo::Context::new(&surface)
            .map_err(|e| anyhow::anyhow!("Cairo context error: {:?}", e))?;

        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().map_err(|e| anyhow::anyhow!("Paint error: {:?}", e))?;

        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(28.0);

        cr.move_to(20.0, 45.0);
        cr.show_text("Wayfrost Test Line 1")
            .map_err(|e| anyhow::anyhow!("Text error: {:?}", e))?;

        cr.move_to(20.0, 95.0);
        cr.show_text("Wayfrost Test Line 2")
            .map_err(|e| anyhow::anyhow!("Text error: {:?}", e))?;

        drop(cr);

        let data = surface.data().map_err(|e| anyhow::anyhow!("{:?}", e))?;
        let mut rgb_buffer = image::RgbImage::new(width as u32, height as u32);
        for y in 0..height as u32 {
            for x in 0..width as u32 {
                let offset = ((y * width as u32 + x) * 4) as usize;
                let b = data[offset];
                let g = data[offset + 1];
                let r = data[offset + 2];
                rgb_buffer.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
        drop(data);

        let img = DynamicImage::ImageRgb8(rgb_buffer);
        let recognized = engine.recognize_image(&img)?;

        println!("Multiline recognized text:\n{}", recognized);
        assert!(recognized.contains("Wayfrost") || recognized.contains("Line"));

        Ok(())
    }
}
