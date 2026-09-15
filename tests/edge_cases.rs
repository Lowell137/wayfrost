use anyhow::Result;
use image::{DynamicImage, RgbImage};
use wayfrost::ocr::model_manager::ensure_models;
use wayfrost::ocr::pipeline::OcrEngine;

#[test]
fn test_zero_and_tiny_crops() -> Result<()> {
    let paths = ensure_models()?;
    let mut engine = OcrEngine::new(&paths.rec_model, &paths.dict_file)?;

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
fn test_screenshot_37() {
    let img_res = image::open("/home/lowell/.gemini/antigravity/brain/a849d87f-9151-4ca4-8e04-5696711787b2/.user_uploaded/media_1789515472013.png");
    if let Ok(img) = img_res {
        let crop = image::imageops::crop_imm(&img, 216, 185, 500, 410).to_image();
        let dyn_crop = image::DynamicImage::ImageRgba8(crop);

        let words = wayfrost::ocr::pipeline::run_tesseract_tsv(&dyn_crop, "TR").unwrap();
        println!(">>> Crop words detected: {}", words.len());
        let refs: Vec<&wayfrost::ocr::pipeline::DetectedWord> = words.iter().collect();
        let text = wayfrost::ocr::pipeline::join_words(&refs);
        println!(">>> Extracted text:\n{}", text);
    }
}
