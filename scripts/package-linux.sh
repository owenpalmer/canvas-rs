#!/usr/bin/env bash
# Package a release build for Linux: dist/canvas-rs-linux-x64.tar.gz with the app, the MCP server,
# PDFium (lib/), the icon and install.sh. Run `cargo build --release --workspace` first.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f vendor/pdfium/linux-x64/lib/libpdfium.so ] || scripts/fetch-pdfium.sh linux-x64
out=dist/canvas-rs
rm -rf "$out"
mkdir -p "$out/lib" "$out/licenses"
cp target/release/canvas-app target/release/canvas-mcp target/release/canvas-check "$out/"
cp vendor/pdfium/linux-x64/lib/libpdfium.so "$out/lib/"
"$out/canvas-app" --write-icon "$PWD/$out/canvas-rs.png"
cp scripts/install-linux.sh "$out/install.sh"
cp README.md "$out/"
cp vendor/pdfium/linux-x64/LICENSE "$out/licenses/PDFium-LICENSE.txt"
cp assets/fonts/Inter-LICENSE.txt "$out/licenses/"
cp assets/fonts/katex/LICENSE "$out/licenses/KaTeX-LICENSE.txt"
tar -C dist -czf dist/canvas-rs-linux-x64.tar.gz canvas-rs
echo "dist/canvas-rs-linux-x64.tar.gz"
