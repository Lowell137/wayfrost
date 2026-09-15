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
fn test_stream_text_selection_multi_line() {
    use wayfrost::ocr::pipeline::{cluster_words_into_lines, select_words_stream, join_words, DetectedWord};

    let sample_words = vec![
        DetectedWord { x: 20.0, y: 20.0, w: 30.0, h: 15.0, text: "Line0_Word0".into(), ..Default::default() },
        DetectedWord { x: 60.0, y: 20.0, w: 30.0, h: 15.0, text: "Line0_Word1".into(), ..Default::default() },
        DetectedWord { x: 100.0, y: 20.0, w: 30.0, h: 15.0, text: "Line0_Word2".into(), ..Default::default() },
        DetectedWord { x: 20.0, y: 45.0, w: 30.0, h: 15.0, text: "Line1_Word0".into(), ..Default::default() },
        DetectedWord { x: 60.0, y: 45.0, w: 30.0, h: 15.0, text: "Line1_Word1".into(), ..Default::default() },
        DetectedWord { x: 100.0, y: 45.0, w: 30.0, h: 15.0, text: "Line1_Word2".into(), ..Default::default() },
        DetectedWord { x: 20.0, y: 70.0, w: 30.0, h: 15.0, text: "Line2_Word0".into(), ..Default::default() },
        DetectedWord { x: 60.0, y: 70.0, w: 30.0, h: 15.0, text: "Line2_Word1".into(), ..Default::default() },
    ];

    let candidates: Vec<usize> = (0..sample_words.len()).collect();
    let lines = cluster_words_into_lines(&sample_words, &candidates);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].len(), 3);
    assert_eq!(lines[1].len(), 3);
    assert_eq!(lines[2].len(), 2);

    // Forward drag from Line0_Word1 to Line2_Word0:
    // Should select: Line0_Word1, Line0_Word2, Line1_Word0, Line1_Word1, Line1_Word2, Line2_Word0
    let sel = select_words_stream((70.0, 25.0), (30.0, 75.0), &lines, &sample_words);
    let sel_texts: Vec<&str> = sel.iter().map(|&i| sample_words[i].text.as_str()).collect();
    assert_eq!(
        sel_texts,
        vec!["Line0_Word1", "Line0_Word2", "Line1_Word0", "Line1_Word1", "Line1_Word2", "Line2_Word0"]
    );

    // Backward drag from Line2_Word0 up to Line0_Word1:
    let sel_back = select_words_stream((30.0, 75.0), (70.0, 25.0), &lines, &sample_words);
    let sel_back_texts: Vec<&str> = sel_back.iter().map(|&i| sample_words[i].text.as_str()).collect();
    assert_eq!(sel_texts, sel_back_texts);

    // Single line selection:
    let sel_line = select_words_stream((25.0, 48.0), (110.0, 48.0), &lines, &sample_words);
    let sel_line_texts: Vec<&str> = sel_line.iter().map(|&i| sample_words[i].text.as_str()).collect();
    assert_eq!(sel_line_texts, vec!["Line1_Word0", "Line1_Word1", "Line1_Word2"]);

    // Test join_words formatting:
    let refs: Vec<&DetectedWord> = sel.iter().map(|&i| &sample_words[i]).collect();
    let joined = join_words(&refs);
    assert_eq!(
        joined,
        "Line0_Word1 Line0_Word2\nLine1_Word0 Line1_Word1 Line1_Word2\nLine2_Word0"
    );
}

#[test]
fn test_select_in_drag_direct() {
    use wayfrost::ocr::pipeline::{select_in_drag, DetectedWord};

    let sample_words = vec![
        DetectedWord { x: 50.0, y: 100.0, w: 40.0, h: 20.0, text: "Direct".into(), ..Default::default() },
        DetectedWord { x: 100.0, y: 100.0, w: 40.0, h: 20.0, text: "Text".into(), ..Default::default() },
        DetectedWord { x: 150.0, y: 100.0, w: 60.0, h: 20.0, text: "Selection".into(), ..Default::default() },
    ];
    let candidates = vec![0, 1, 2];

    // Drag sweeping over "Direct" and "Text"
    let sel = select_in_drag((45.0, 95.0), (142.0, 115.0), &sample_words, &candidates);
    assert_eq!(sel, vec![0, 1]);

    // Drag in empty space: returns empty
    let empty_sel = select_in_drag((500.0, 500.0), (600.0, 600.0), &sample_words, &candidates);
    assert!(empty_sel.is_empty());
}

