#!/usr/bin/env bash
# Builds Kompo-x86_64.AppImage (needs linuxdeploy; downloaded when absent).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p kompo-app
APPDIR=target/AppDir
rm -rf "$APPDIR"; mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/applications"
cp target/release/kompo "$APPDIR/usr/bin/"
cp packaging/icon.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/kompo.svg"
cp packaging/linux/kompo.desktop "$APPDIR/usr/share/applications/"
LD=${LINUXDEPLOY:-target/linuxdeploy-x86_64.AppImage}
if [ ! -x "$LD" ]; then
  curl -fL -o "$LD" https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
  chmod +x "$LD"
fi
# bundles ffmpeg and the other shared libraries; the LGPL libs stay replaceable (dynamic linking)
APPIMAGE_EXTRACT_AND_RUN=1 OUTPUT=Kompo-x86_64.AppImage "$LD" --appdir "$APPDIR" \
  --desktop-file "$APPDIR/usr/share/applications/kompo.desktop" \
  --icon-file "$APPDIR/usr/share/icons/hicolor/scalable/apps/kompo.svg" --output appimage
