#!/usr/bin/env sh
# Build the release programs and assemble a double-clickable package for this OS in dist/.
#   Linux: dist/linux/ (fflocal = the launcher, ffl-app = the program, fflocal.desktop, icon)
#          — tools/install-desktop.sh installs it
#   macOS: dist/macos/FFLocal.app (the launcher is the bundle's executable, ffl-app beside it)
# Needs a Rust toolchain (https://rustup.rs) and, on Linux, the usual Bevy build deps
# (alsa, udev, wayland/x11 headers). The games are not needed to build.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p ffl-app -p ffl-launcher
case "$(uname -s)" in
  Darwin)
    app=dist/macos/FFLocal.app
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp target/release/fflocal "$app/Contents/MacOS/fflocal"
    cp target/release/ffl-app "$app/Contents/MacOS/ffl-app"
    strip "$app/Contents/MacOS/fflocal" "$app/Contents/MacOS/ffl-app" 2>/dev/null || true
    cp packaging/Info.plist "$app/Contents/"
    cp packaging/icon.png "$app/Contents/Resources/icon.png"
    echo "built $app (open it, or drag it to Applications)"
    ;;
  *)
    out=dist/linux
    rm -rf "$out"
    mkdir -p "$out"
    cp target/release/fflocal "$out/fflocal"
    cp target/release/ffl-app "$out/ffl-app"
    # The release profile keeps line tables for traces; the package does not need them.
    strip "$out/fflocal" "$out/ffl-app" 2>/dev/null || true
    cp packaging/fflocal.desktop packaging/icon.png "$out/"
    echo "built $out/ (double-click fflocal, or run tools/install-desktop.sh to add FFLocal to the application menu)"
    ;;
esac
