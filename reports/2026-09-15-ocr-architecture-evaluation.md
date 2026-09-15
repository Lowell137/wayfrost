# OCR Engine Evaluation for Turkish & Multilingual Wayland Tool

Tarih: 2026-09-15  
Yazar: Echo (Oh My Pi)  
Hedef: Windows 11 Text Extractor alternatifi için en uygun OCR motorunun seçimi.

---

## 1. Karşılaştırılan Motorlar

### A. Tesseract (v5 LSTM)
- **Avantaj:** Küçük boyut, hızlı CPU, sistem paketi olarak kurulu (`/usr/bin/tesseract`).
- **Dezavantaj:** Ekran görüntülerinde ve düşük kontrastta Türkçe diacritic (`ı/i`, `ğ`, `ş`, `ç`) karakterleri gürültü sanarak siler veya yanlış çevirir. Katı binarizasyon gerektirir.
- **Karar:** Yetersiz, ana motor olamaz.

### B. EasyOCR
- **Avantaj:** PyTorch tabanlı, CRAFT deteksiyonu iyi, doğal sahne metinlerinde başarılı.
- **Dezavantaj:** Ağır PyTorch bağımlılığı (~1GB+ disk), CPU'da yavaş inference (~1.5-3 sn), Türkçe özel karakterlerde ara sıra cedilla atlama.
- **Karar:** Bağımlılık boyutu nedeniyle tekil binary dağıtımına uygun değil.

### C. PaddleOCR (PP-OCRv4) — **SEÇİLEN MOTOR**
- **Avantaj:**
  - Standardize Türkçe doküman benchmarklarında (OCRTurk) en düşük NED (Normalized Edit Distance) hatası.
  - Noktalı/noktasız `I/İ/ı/i` ayrımını doğru yapar.
  - ONNX export formatı resmi olarak desteklenir.
  - Mobile modeli sadece ~22 MB (Det: 4.5MB, Rec: 16MB, Cls: 1.4MB).
  - CPU'da ~150-250ms, GPU'da (RTX 4060) ~15ms inference.
- **Karar:** ONNX Runtime (`ort` crate) ile saf Rust içerisinde sıfır Python bağımlılığıyla çalıştırılacak.

---

## 2. Model Detayları ve İndirme Kaynakları
- **Detection:** `ch_PP-OCRv4_det_infer.onnx`
- **Recognition:** `latin_PP-OCRv4_rec_infer.onnx` (Türkçe Latin alfabesi bu modeldedir)
- **Karakter Sözlüğü:** `ppocr_keys_latin.txt`
