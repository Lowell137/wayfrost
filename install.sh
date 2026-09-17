#!/usr/bin/env bash
set -e

# 1. Detect Distro & Install System Dependencies
if [ -f /etc/os-release ]; then
    . /etc/os-release
    OS=$ID
else
    OS="unknown"
fi

echo "Detected OS: $OS"

case "$OS" in
    arch|manjaro|cachyos)
        sudo pacman -S --needed --noconfirm rust cargo gtk4 libadwaita tesseract tesseract-data-tur tesseract-data-eng git openssl pkgconf
        ;;
    debian|ubuntu|pop|linuxmint)
        sudo apt update && sudo apt install -y cargo rustc libgtk-4-dev libadwaita-1-dev tesseract-ocr tesseract-ocr-tur tesseract-ocr-eng git libssl-dev pkg-config
        ;;
    fedora|rhel|centos)
        sudo dnf install -y cargo rust gtk4-devel libadwaita-devel tesseract tesseract-langpack-tur tesseract-langpack-eng git openssl-devel pkg-config
        ;;
    *)
        echo "Warning: Unsupported distro ($OS). Please ensure GTK4, Libadwaita, Tesseract, OpenSSL, and Rust are installed."
        ;;
esac

# 2. Ensure Cargo env & Rust
[[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"
if ! command -v cargo &> /dev/null; then
    echo "Installing Rust/Cargo..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "${HOME}/.cargo/env"
fi

# 3. Build & Install Binary
echo "Building Wayfrost..."
mkdir -p "${HOME}/.local/bin"
cargo build --release
cp target/release/wayfrost "${HOME}/.local/bin/wayfrost"
chmod +x "${HOME}/.local/bin/wayfrost"
echo "Binary installed to: ~/.local/bin/wayfrost"

# 4. Setup GNOME Shortcut
if command -v gsettings >/dev/null 2>&1; then
    KEY_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom-wayfrost/"
    SCHEMA="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding"
    MEDIA_KEYS="org.gnome.settings-daemon.plugins.media-keys"

    CURRENT=$(gsettings get ${MEDIA_KEYS} custom-keybindings)
    if [[ "${CURRENT}" != *"${KEY_PATH}"* ]]; then
        if [[ "${CURRENT}" == "@as []" || "${CURRENT}" == "[]" ]]; then
            NEW_LIST="['${KEY_PATH}']"
        else
            NEW_LIST="${CURRENT%]*}, '${KEY_PATH}']"
        fi
        gsettings set ${MEDIA_KEYS} custom-keybindings "${NEW_LIST}"
    fi

    gsettings set "${SCHEMA}:${KEY_PATH}" name "Wayfrost Text Extractor"
    gsettings set "${SCHEMA}:${KEY_PATH}" command "${HOME}/.local/bin/wayfrost"
    gsettings set "${SCHEMA}:${KEY_PATH}" binding "<Super><Shift>t"
    echo "GNOME shortcut created: Super+Shift+T"
fi

# 5. Install GNOME Shell Extension
if [ -d "$HOME/.local/share/gnome-shell" ] || command -v gnome-shell >/dev/null 2>&1; then
    EXT_DIR="${HOME}/.local/share/gnome-shell/extensions/wayfrost@lowell"
    mkdir -p "${EXT_DIR}"
    cp -r gnome-extension/* "${EXT_DIR}/"
    if command -v gnome-extensions >/dev/null 2>&1; then
        gnome-extensions enable wayfrost@lowell || true
        echo "GNOME extension (wayfrost@lowell) installed and enabled."
        echo "Note: If the extension doesn't register immediately, restart GNOME Shell (Press Alt+F2, type 'r', and press Enter; or log out and back in on Wayland)."
    fi
fi

echo "Installation complete!"
