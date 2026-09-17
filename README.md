# Wayfrost

Wayland ve GNOME ortamları için geliştirilmiş, hızlı ve hafif ekran metin seçme aracı (Apple Live Text / PowerToys Text Extractor benzeri).

![Wayfrost Ekran Görüntüsü](docs/screenshot.png)

## Özellikler

- Türkçe ve İngilizce tam karakter desteği (`ç, ğ, ı, ö, ş, ü` ve büyük harfler).
- 2D uzaysal satır kümeleme ile bozulmayan, doğru okuma sırası.
- GTK4 ve Libadwaita tabanlı şık arayüz.
- Tesseract `tessdata_best` ve ONNX model desteği.

## Kurulum

Depoyu klonlayın ve kurulum scriptini çalıştırın:

```bash
git clone https://github.com/Lowell137/wayfrost.git
cd wayfrost
./scripts/setup-shortcut.sh
```

Bu script binary dosyasını `~/.local/bin/wayfrost` konumuna kurar ve `<Super><Shift>t` kısayolunu otomatik tanımlar.
