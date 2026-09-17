# Wayfrost

Wayland ve GNOME ortamları için geliştirilmiş, hızlı ve hafif ekran metin seçme aracı (Apple Live Text / PowerToys Text Extractor benzeri).

![Wayfrost Ekran Görüntüsü](docs/screenshot.png)

## Özellikler

- Türkçe ve İngilizce tam karakter desteği (`ç, ğ, ı, ö, ş, ü` ve büyük harfler).
- 2D uzaysal satır kümeleme ile bozulmayan, doğru okuma sırası.
- GTK4 ve Libadwaita tabanlı şık arayüz.
- Tesseract `tessdata_best` ve ONNX model desteği.

## Otomatik Kurulum

Arch Linux, Debian, Ubuntu ve Fedora destekleyen tek komutluk kurulum scripti:

```bash
git clone https://github.com/Lowell137/wayfrost.git
cd wayfrost
chmod +x install.sh
./install.sh
```

Bu script eksik paketleri (GTK4, Libadwaita, Tesseract, Rust) dağıtınıza göre otomatik kurar, binary dosyasını `~/.local/bin/wayfrost` altına derler ve `<Super><Shift>t` kısayolunu tanımlar.
