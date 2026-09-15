# Wayfrost — Project Backlog

## Phase 1: Proje İskeleti ve Ekran Yakalama
- [x] `Cargo.toml` bağımlılıklarının yapılandırılması (`gtk4`, `ashpd`, `image`, `ort`, vb.)
- [x] `src/constants.rs` sabitler dosyasının oluşturulması (model URL'leri, varsayılan yollar)
- [x] `src/capture.rs`: Wayland ekran görüntüsü alma modülü (XDG Portal / grim fallback)

## Phase 2: GTK4 Freeze & Selection Overlay
- [x] `src/overlay.rs`: Tam ekran dondurulmuş görsel penceresi ve Yüzer Araç Çubuğu (Floating Pill Bar, Adwaita symbolic ikonlar, CSS blur/translucent styling, ESC çıkış davranışı)
- [x] Mouse drag ile dinamik seçim dikdörtgeni çizimi (GestureDrag + Cairo dim scrim)
- [x] Seçilen koordinatlardan görsel kırpma (`image::crop_imm`)

## Phase 3: ONNX OCR Motoru (PaddleOCR PP-OCRv5 Latin/Multilingual)
- [x] `src/ocr/model_manager.rs`: İlk çalıştırmada ONNX modellerini indirme ve cache'leme (`~/.local/share/wayfrost/models`)
- [x] `src/ocr/pipeline.rs`: PP-OCR CTC greedy decoding ve Türkçe/Latin karakterleri tanıma
- [x] `src/ocr/pipeline.rs`: Çok satırlı (multi-line) yatay projeksiyon tabanlı satır segmentasyonu ve birleştirme
- [x] CPU multi-threading (4 thread) ile yüksek hızlı inferans (~0.10s)

## Phase 4: Pano & Bildirim & Kısayol
- [x] `src/clipboard.rs`: Metni Wayland panosuna (`wl-copy`) kopyalama
- [x] `src/clipboard.rs`: Masaüstü bildirimi (`notify-send`: "Metin kopyalandı")
- [x] `scripts/setup-shortcut.sh`: GNOME Custom Shortcut tanımlama betiği (`Super+Shift+T`)
- [x] `org.wayfrost.Wayfrost.desktop`: Linux masaüstü entegrasyon dosyası

## Phase 5: Doğrulama & Edge Case Testleri
- [x] Boş/1 piksellik seçim kontrolü (`tests/edge_cases.rs`)
- [x] Türkçe karakter test seti (`ç, ğ, ı, İ, ö, ş, ü`, `tests/edge_cases.rs`)
- [x] Çok satırlı metin okuma testi (`tests` & `src/ocr/pipeline.rs`)
- [x] Release derlemesi ve `~/.local/bin/wayfrost` kurulumu
