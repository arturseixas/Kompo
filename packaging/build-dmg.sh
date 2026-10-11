#!/usr/bin/env bash
# Builds Kompo.dmg on macOS (needs ffmpeg from Homebrew, dylibbundler).
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c "import json,sys;print(next(p['version'] for p in json.load(sys.stdin)['packages'] if p['name']=='kompo-app'))")
cargo build --release -p kompo-app
APP=target/Kompo.app
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp target/release/kompo "$APP/Contents/MacOS/kompo"
sed "s/@VERSION@/$VERSION/" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
command -v dylibbundler >/dev/null || brew install dylibbundler
dylibbundler -od -b -x "$APP/Contents/MacOS/kompo" -d "$APP/Contents/Frameworks" -p @executable_path/../Frameworks/
codesign --force --deep --sign "${CODESIGN_IDENTITY:--}" "$APP"
hdiutil create -volname Kompo -srcfolder "$APP" -ov -format UDZO target/Kompo.dmg
echo "target/Kompo.dmg"
