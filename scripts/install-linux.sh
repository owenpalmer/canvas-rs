#!/usr/bin/env bash
# Install canvas-rs for this user, from an unpacked release: the files in ~/.local/share/canvas-rs,
# a launcher entry (with a "Sample data" action), and canvas-app / canvas-mcp / canvas-check in
# ~/.local/bin. Run it again to update; delete those to uninstall.
set -euo pipefail
src="$(cd "$(dirname "$0")" && pwd)"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
dest="$data/canvas-rs"
if [ "$src" != "$dest" ]; then
  rm -rf "$dest"
  mkdir -p "$dest"
  cp -r "$src"/. "$dest"/
fi
mkdir -p "$data/applications" "$HOME/.local/bin"
for b in canvas-app canvas-mcp canvas-check; do
  # leave alone a same-named command that isn't ours
  if [ ! -e "$HOME/.local/bin/$b" ] || [ -L "$HOME/.local/bin/$b" ]; then
    ln -sfn "$dest/$b" "$HOME/.local/bin/$b"
  else
    echo "note: $HOME/.local/bin/$b exists and isn't a link; left it alone"
  fi
done
cat > "$data/applications/canvas-rs.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Canvas
Comment=Your Canvas courses: fast, and readable offline
Exec=$dest/canvas-app
Icon=$dest/canvas-rs.png
Terminal=false
Categories=Education;
Keywords=canvas;lms;courses;school;anki;
StartupWMClass=canvas-desktop
Actions=demo;

[Desktop Action demo]
Name=Sample data (demo)
Exec=$dest/canvas-app --demo
DESKTOP
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" 2>/dev/null || true
echo "Installed canvas-rs in $dest (launcher: Canvas)."
