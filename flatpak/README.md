# Wayfrost Flatpak packaging

Bu klasör **yalnızca** flatpak paketlemesi için. `src/`, `Cargo.toml`, `install.sh`, `gnome-extension/` — hiçbiri **değiştirilmez**. Build sırasında `build-commands` altındaki `sed` komutları sadece build-dir içindeki kopyayı patch'ler, upstream temiz kalır.

## Dosya haritası

```
flatpak/
├── io.github.Lowell137.Wayfrost.yml   ← ana manifest
├── modules/
│   ├── leptonica.json                 ← tesseract bağımlılığı
│   ├── tesseract.json                 ← OCR CLI
│   └── tessdata.json                  ← eng + tur traineddata
├── icons/
│   └── org.wayfrost.Wayfrost.svg      ← placeholder ikon (beğenmezsen değiştir)
├── metainfo/
│   └── org.wayfrost.Wayfrost.metainfo.xml
├── build.sh                           ← tek komutla build + install + run
└── .gitignore
```

## İlk build (Fedora WS 44 VM)

```bash
sudo dnf install -y flatpak flatpak-builder
cd ~/Projects/wayfrost/flatpak
./build.sh
```

### Beklenecek ilk hata: sha256 placeholder'ları

`modules/*.json` içinde `"PLACEHOLDER_..."` yazan alanları flatpak-builder sana **gerçek hash'i verir**, örneğin:

```
Error: Failed to download source: ... 
Fetched content has wrong checksum: expected PLACEHOLDER_..., actual e5263fba9c7e...
```

O hash'i kopyala, ilgili satıra yapıştır, tekrar çalıştır. Üç module için toplamda 4 hash var (leptonica, tesseract, eng, tur). 5 dakika sürer.

## Test akışı (VM'de)

```bash
flatpak run io.github.Lowell137.Wayfrost
```

Beklenen:
- Overlay açılır (Super+Shift+T **çalışmaz**, flatpak kurulumu gsettings yazamıyor — elle GNOME → Keyboard → Custom Shortcuts ile ekle)
- Seçim yap → tesseract OCR metni clipboard'a koyar
- **ONNX backend ilk seferde HuggingFace'den model indirir** — `~/.cache/wayfrost/models/` sandbox içine düşer, sorun değil (`--filesystem=~/.cache/wayfrost:create` verdik)
- Capture chain: portal fallback ile çalışır (grim/gnome-screenshot/import sandbox'ta yok, extension yoksa anında sessizce portal'a düşer)

## Bilinen eksikler (Flathub PR öncesi yapılacaklar)

1. **ONNX runtime build-time download**: local'de çalışır çünkü sandbox build network açık. Flathub CI'da yasak → `modules/onnxruntime.json` eklenmeli, Cargo.toml'u build-time sed ile `default-features = false, features = ["load-dynamic"]` yapılmalı.
2. **Icon**: şu an placeholder. Kendin çiz veya birine çizdir (Flathub review ikon kalitesine bakıyor).
3. **Metainfo**: `<release>` tarihini gerçek tag tarihine ayarla.

## Sorun giderme

**Tesseract bulunamıyor** → build.sh'ın son satırındaki sandbox kontrolünü çalıştır.

**Kod imzası / permission hatası** → `flatpak run --command=sh io.github.Lowell137.Wayfrost` ile shell'e düş, `ls /app/bin/` kontrol et.
