#!/usr/bin/env bash
# Download PDFium (bblanchon/pdfium-binaries) into vendor/pdfium/<platform>, where the app looks
# for it when run from the source tree. Usage: scripts/fetch-pdfium.sh [linux-x64|win-x64]
set -euo pipefail
VERSION="chromium/8066"
PLATFORM="${1:-linux-x64}"
DEST="$(cd "$(dirname "$0")/.." && pwd)/vendor/pdfium/$PLATFORM"
URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${VERSION/\//%2F}/pdfium-$PLATFORM.tgz"
mkdir -p "$DEST"
curl -fsSL "$URL" | tar -xz -C "$DEST"
echo "PDFium $VERSION ($PLATFORM) in $DEST"
