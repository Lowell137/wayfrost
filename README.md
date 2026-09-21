# Wayfrost

Fast, lightweight, native text extraction for GNOME and Wayland — grab any region of your screen and the text inside it lands on your clipboard (like Apple Live Text or PowerToys Text Extractor on Windows).


Wayfrost Screenshot  <img width="1920" height="1080" alt="Wayfrost selection overlay" src="https://github.com/user-attachments/assets/45ddcd7c-651f-461e-8e8b-13b1138e7123" />

---

## Features

- **Silent capture, no setup** — on GNOME/Wayland it grabs the screen through the XDG Desktop Portal with no region-picker dialog and no shell extension required.
- **Full Turkish and English OCR** — `ç, ğ, ı, ö, ş, ü` and their uppercase variants are recognized correctly.
- **Correct reading order** — 2D spatial line clustering keeps multi-column text from scrambling.
- **Two OCR engines** — bundled Tesseract (`tessdata_best`) and a PP-OCR ONNX model.
- **Private by design** — runs fully offline; nothing leaves your machine.
- **Native GTK4 / Libadwaita UI.**

## Install

### Flatpak (recommended)

Grab the bundle from the [latest release](https://github.com/Lowell137/wayfrost/releases) and install it:

```bash
flatpak install -y ./wayfrost.flatpak
flatpak run io.github.Lowell137.Wayfrost
```

### From source

Clone the repository and run the automated installation script:

```bash
git clone https://github.com/Lowell137/wayfrost.git
cd wayfrost
chmod +x install.sh
./install.sh
```

The script installs the required dependencies (GTK4, Libadwaita, Tesseract, Rust) for your distribution (Arch, Debian/Ubuntu, Fedora), compiles the release binary to `~/.local/bin/wayfrost`, and sets up the `<Super><Shift>T` shortcut.

## Usage

1. Press `<Super><Shift>T` (or launch the app) — your screen freezes.
2. Drag over the text you want.
3. The extracted text is copied to your clipboard automatically.

## How capture works

Wayfrost tries several backends in order and uses the first that succeeds:

1. `grim` (wlroots compositors)
2. **XDG Desktop Portal, silent** (`interactive:false`) — the default on GNOME. A transparent, focused helper surface is presented so the portal grabs the real desktop with no picker and no extension.
3. Optional companion GNOME Shell extension (instant, silent)
4. `gnome-screenshot`
5. ImageMagick `import` (X11 / XWayland / VMs)

## License

MIT
