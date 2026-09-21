# Wayfrost Flatpak packaging

This folder is **only** for Flatpak packaging. `src/`, `Cargo.toml`, `install.sh`, and `gnome-extension/` are **never modified** — the `sed` commands under `build-commands` patch only the copy inside `build-dir`, so the upstream source stays clean.

## File map

```
flatpak/
├── io.github.Lowell137.Wayfrost.yml   ← main manifest
├── modules/
│   ├── leptonica.json                 ← tesseract dependency
│   ├── tesseract.json                 ← OCR CLI
│   ├── tessdata.json                  ← eng + tur traineddata
│   └── onnxruntime.json               ← ONNX runtime (dynamic link)
├── icons/
│   ├── io.github.Lowell137.Wayfrost.svg   ← canonical vector icon
│   └── hicolor/<size>/apps/*.png          ← rendered PNGs
├── metainfo/
│   └── org.wayfrost.Wayfrost.metainfo.xml
├── build.sh                           ← build + install + run in one command
└── .gitignore
```

## Building

### On the target VM (Fedora Workstation)

```bash
sudo dnf install -y flatpak flatpak-builder
cd ~/Projects/wayfrost/flatpak
./build.sh
```

**Expected first error: sha256 placeholders.** The `"PLACEHOLDER_..."` fields in `modules/*.json` are filled in for you — flatpak-builder reports the real hash, e.g.:

```
Fetched content has wrong checksum: expected PLACEHOLDER_..., actual e5263fba9c7e...
```

Copy each hash into the matching line and re-run. There are four across three modules (leptonica, tesseract, eng, tur). Takes about five minutes.

### On a build host whose libvips lacks JPEG-XL (e.g. CachyOS)

`appstreamcli compose` fails at the very end (`E: media-worker-error` / `filters-but-no-output`) because the host libvips can't decode the icon/screenshot. The binary is already built by then, so finish/export/bundle manually:

```bash
flatpak-builder --ccache --disable-rofiles-fuse --force-clean \
  --keep-build-dirs --repo=repo build-dir io.github.Lowell137.Wayfrost.yml
# (fails at appstream — safe to ignore; build-dir/files is complete)
flatpak build-finish --command=wayfrost build-dir \
  --socket=wayland --socket=fallback-x11 --device=dri --share=network \
  --talk-name=org.freedesktop.portal.Desktop --talk-name=org.wayfrost.Capture \
  --filesystem=~/.cache/wayfrost:create --filesystem=xdg-pictures
flatpak build-export repo build-dir
flatpak build-bundle repo wayfrost.flatpak io.github.Lowell137.Wayfrost
```

Note: `build-finish` auto-detects the command from `/app/bin` and may pick `tesseract` (two executables ship) — always pass `--command=wayfrost`, or fix `command=` in `build-dir/metadata` before exporting.

## Test flow (in the VM)

```bash
flatpak run io.github.Lowell137.Wayfrost
```

Expected:
- The overlay opens. The `Super+Shift+T` global shortcut **won't work** from a Flatpak install (it can't write gsettings) — add it manually under GNOME → Keyboard → Custom Shortcuts, pointing at `flatpak run io.github.Lowell137.Wayfrost`.
- Drag a selection → Tesseract OCR puts the text on the clipboard.
- **The ONNX backend downloads its models on first run** into `~/.cache/wayfrost/models/` inside the sandbox — that's fine (`--filesystem=~/.cache/wayfrost:create`).
- Capture chain: silent XDG Desktop Portal (`interactive:false`) is the primary path on GNOME — no region picker, no extension. It falls back to grim / the extension / gnome-screenshot / ImageMagick only if the portal is unavailable.

## Known gaps (before a Flathub PR)

1. **Metainfo screenshot.** The `<screenshots>` block points at `docs/screenshot.png`, which currently 404s. Add a real screenshot to the repo before submitting.
2. **Icon is now a proper vector** (`icons/io.github.Lowell137.Wayfrost.svg`, rendered to all hicolor sizes). Keep the SVG as the source of truth.
3. **Metainfo `<release>` date** should match the real tag date.

## Troubleshooting

**Tesseract not found** → run the sandbox check at the bottom of `build.sh`.

**Code signing / permission error** → drop into the sandbox with `flatpak run --command=sh io.github.Lowell137.Wayfrost` and inspect `ls /app/bin/`.
