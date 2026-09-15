# Wayfrost — Agent Execution Protocol

Wayfrost, Linux Wayland ortamı (GNOME ve genel Wayland) için Windows 11 Text Extractor benzeri ekran dondurmalı, yüksek doğruluklu Türkçe + çok dilli OCR aracıdır.

## 1. Mimari Prensipler & Kısıtlamalar
- **Dil:** Saf Rust. Python çalışma zamanı (runtime) bağımlılığı YOK.
- **Ekran Yakalama:** XDG Desktop Portal (`ashpd`) veya `grim` (Wayland standart).
- **Overlay UI:** GTK4 / `gtk4-rs` — ekranı dondurur, şeffaf seçim alanı sunar.
- **OCR Motoru:** ONNX Runtime (`ort` crate) üzerinden PaddleOCR PP-OCRv4 ONNX modelleri.
- **Donanım:** CPU varsayılan (hafif mobil model), CUDA/TensorRT algılanırsa GPU hızlandırma.
- **Pano:** `wl-clipboard-rs` veya `wl-copy`.

## 2. "Bitti" (Done) Tanımı
Bir özellik veya görev şu şartlar sağlanmadan "bitti" sayılamaz:
1. **Gerçek Görsel Testi:** Türkçe diacritic içeren gerçek bir ekran görüntüsünde (`ç, ğ, ı, İ, ö, ş, ü`) metin kayıpsız tanınmalı.
2. **Kopya Doğrulaması:** Tanınan metin `wl-paste` ile panodan okunabilir olmalı.
3. **Wayland Temiz Çıkış:** Overlay kapatıldığında veya `ESC` basıldığında GTK penceresi arkada zombi process bırakmamalı.
4. **Boş Test Yasağı:** Sadece geçsin diye yazılan tautolojik testler (`assert_eq!(2, 2)`) kabul edilmez; sınır durumları (boş seçim, ters seçim, düşük çözünürlük) test edilmelidir.

## 3. Kod Düzeni
- Sabitler (URL'ler, model adları, varsayılan yollar) `src/constants.rs` içinde tutulur.
- Büyük benchmark veya mimari analizler `reports/` içine yazılır, context şişirilmez.
- Kalan işler ve oturum durumu her zaman `Backlog.md` üzerinde güncellenir.

## 4. Ponytail Disiplini (Lazy Senior Dev)
- **The Ladder:** 1. YAGNI -> 2. Proje içi yeniden kullanım -> 3. Rust stdlib -> 4. Platform native -> 5. Mevcut bağımlılık -> 6. Tek satır -> 7. Minimum çalışan kod.
- **Sıfır Şişirme (No Bloat):** Tek bir implementasyonu olan gereksiz trait/interface'ler, spekülatif mimari katmanları ve "ileride lazım olur" scaffolding'i yasaktır.
- **Kısa ve Net Çıktı:** Açıklama koddan uzun olmayacak. Gereksiz uzun açıklamalar yerine kod ve neyin atlandığı / ne zaman ekleneceği yazılır.
