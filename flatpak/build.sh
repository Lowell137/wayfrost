#!/usr/bin/env bash
# Local build + install + run. Requires flatpak-builder.
# Fedora WS 44: sudo dnf install flatpak flatpak-builder nodejs
set -euo pipefail
cd "$(dirname "$0")"

APP_ID=io.github.Lowell137.Wayfrost

# Ensure Flathub remote is present (user scope, no sudo).
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo

# GNOME 48 runtime/SDK.
flatpak install -y --user flathub org.gnome.Platform//48 org.gnome.Sdk//48

# Build, then install locally to user dir.
flatpak-builder \
  --user \
  --install-deps-from=flathub \
  --force-clean \
  --repo=repo \
  build-dir \
  "${APP_ID}.yml"

flatpak install -y --user ./repo "${APP_ID}"

echo
echo "Built and installed. Test with:"
echo "  flatpak run ${APP_ID}"
echo
echo "If OCR fails to spawn 'tesseract', enter the sandbox and check:"
echo "  flatpak run --command=sh ${APP_ID} -c 'which tesseract && tesseract --version'"
