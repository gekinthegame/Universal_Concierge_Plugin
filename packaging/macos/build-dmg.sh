#!/bin/bash
# Assemble Concierge.app (from built Concierge binaries + a logo) and
# package it as a drag-to-Applications .dmg. macOS only (uses sips/iconutil/hdiutil).
#
#   packaging/macos/build-dmg.sh <plugin-binary> <logo.png> <out.dmg> [kernel-binary]
set -euo pipefail

BIN="${1:?usage: build-dmg.sh <plugin-binary> <logo.png> <out.dmg> [kernel-binary]}"
LOGO="${2:?missing logo png}"
OUT="${3:?missing output .dmg}"
KERNEL="${4:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"

APP="Concierge.app"
ICONSET="Concierge.iconset"
STAGE="dmg-stage"
rm -rf "$APP" "$ICONSET" "$STAGE" "$OUT"

# ── app bundle ───────────────────────────────────────────────────────────────
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$HERE/Info.plist" "$APP/Contents/Info.plist"
cp "$HERE/launcher.sh" "$APP/Contents/MacOS/Concierge"
chmod +x "$APP/Contents/MacOS/Concierge"
cp "$BIN" "$APP/Contents/MacOS/concierge-plugin"
chmod +x "$APP/Contents/MacOS/concierge-plugin"
if [ -n "$KERNEL" ]; then
  cp "$KERNEL" "$APP/Contents/MacOS/concierge-kernel"
  chmod +x "$APP/Contents/MacOS/concierge-kernel"
fi

# ── icon (from the logo) ─────────────────────────────────────────────────────
mkdir -p "$ICONSET"
gen() { sips -z "$1" "$1" "$LOGO" --out "$ICONSET/$2" >/dev/null; }
gen 16   icon_16x16.png
gen 32   icon_16x16@2x.png
gen 32   icon_32x32.png
gen 64   icon_32x32@2x.png
gen 128  icon_128x128.png
gen 256  icon_128x128@2x.png
gen 256  icon_256x256.png
gen 512  icon_256x256@2x.png
gen 512  icon_512x512.png
gen 1024 icon_512x512@2x.png
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

# ── .dmg (with an Applications symlink so the user can drag-to-install) ───────
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Universal Concierge" -srcfolder "$STAGE" -ov -format UDZO "$OUT"

rm -rf "$ICONSET" "$STAGE"
echo "built $OUT"
