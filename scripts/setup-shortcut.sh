#!/usr/bin/env bash
set -e

# Source cargo if present
[[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"

if ! command -v cargo &> /dev/null; then
    echo "Hata: Rust/Cargo bulunamadı. Lütfen https://rustup.rs adresinden Rust kurun."
    exit 1
fi

WAYFROST_BIN="${HOME}/.local/bin/wayfrost"
# 1. Build release binary and install to ~/.local/bin
mkdir -p "${HOME}/.local/bin"
cargo build --release
cp target/release/wayfrost "${WAYFROST_BIN}"
chmod +x "${WAYFROST_BIN}"

echo "Wayfrost binary installed to: ${WAYFROST_BIN}"

# 2. Setup GNOME Shortcut if gsettings is available
if command -v gsettings >/dev/null 2>&1; then
    KEY_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom-wayfrost/"
    SCHEMA="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding"
    MEDIA_KEYS="org.gnome.settings-daemon.plugins.media-keys"

    # Fetch current custom keybindings list
    CURRENT=$(gsettings get ${MEDIA_KEYS} custom-keybindings)
    if [[ "${CURRENT}" != *"${KEY_PATH}"* ]]; then
        if [[ "${CURRENT}" == "@as []" || "${CURRENT}" == "[]" ]]; then
            NEW_LIST="['${KEY_PATH}']"
        else
            NEW_LIST="${CURRENT%]*}, '${KEY_PATH}']"
        fi
        gsettings set ${MEDIA_KEYS} custom-keybindings "${NEW_LIST}"
    fi

    # Set shortcut attributes
    gsettings set "${SCHEMA}:${KEY_PATH}" name "Wayfrost Text Extractor"
    gsettings set "${SCHEMA}:${KEY_PATH}" command "${WAYFROST_BIN}"
    gsettings set "${SCHEMA}:${KEY_PATH}" binding "<Super><Shift>t"

    echo "GNOME shortcut created: Super+Shift+T -> ${WAYFROST_BIN}"
else
    echo "gsettings not found. Add a custom shortcut manually in your desktop settings for: ${WAYFROST_BIN}"
fi
