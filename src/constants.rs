//! Wayfrost projesi genelinde değişmeyen sabit değerler.
#![allow(dead_code)]

/// Uygulama kimliği (GTK & Desktop entry)
pub const APP_ID: &str = "org.wayfrost.Wayfrost";
pub const APP_NAME: &str = "Wayfrost";

/// Model indirme ve cache dizini
pub const CACHE_DIR_NAME: &str = "wayfrost";
pub const MODELS_SUBDIR: &str = "models";

/// PP-OCR ONNX Model URL'leri (Hafif Mobile Versiyonlar - ~12MB toplam)
pub const DET_MODEL_URL: &str = "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx";
pub const REC_MODEL_URL: &str = "https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/latin/rec.onnx";
pub const KEYS_DICT_URL: &str = "https://huggingface.co/monkt/paddleocr-onnx/raw/main/languages/latin/dict.txt";

/// Dosya isimleri
pub const DET_MODEL_FILENAME: &str = "ch_PP-OCRv4_det_infer.onnx";
pub const REC_MODEL_FILENAME: &str = "latin_rec.onnx";
pub const KEYS_DICT_FILENAME: &str = "latin_dict.txt";

/// Minimum seçilebilir piksel alanı (kazara tek tıklamada OCR tetiklenmesin)
pub const MIN_SELECTION_AREA_PIXELS: f64 = 64.0;
