#!/usr/bin/env sh
# Install the Linux package built by tools/package.sh for the current user: the launcher and
# the program to ~/.local/bin (fflocal, ffl-app), a menu entry and icon. FFLocal then starts
# from the application menu (the launcher window first, then the program).
set -eu
cd "$(dirname "$0")/.."
[ -f dist/linux/fflocal ] && [ -f dist/linux/ffl-app ] || { echo "run tools/package.sh first"; exit 1; }
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
mkdir -p "$bin" "$apps" "$icons"
install -m 755 dist/linux/fflocal "$bin/fflocal"
install -m 755 dist/linux/ffl-app "$bin/ffl-app"
install -m 644 dist/linux/icon.png "$icons/fflocal.png"
sed "s|^Exec=.*|Exec=$bin/fflocal|" dist/linux/fflocal.desktop > "$apps/fflocal.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$apps" || true
echo "installed: $bin/fflocal (launcher), $bin/ffl-app and $apps/fflocal.desktop"
