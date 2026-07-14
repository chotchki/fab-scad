#!/usr/bin/env bash
# Render icon.svg → a macOS .icns (all iconset sizes), committed as packaging/macos/fab-scad.icns.
# Headless Chrome renders the SVG (sips can't rasterize SVG); sips downscales the 1024 master into the
# .iconset; iconutil packs it. Re-run after editing icon.svg.
set -euo pipefail
cd "$(dirname "$0")"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# SVG → 1024 PNG, transparent outside the rounded square.
"$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --default-background-color=00000000 --window-size=1024,1024 \
  --screenshot="$work/icon-1024.png" "file://$PWD/icon.svg" >/dev/null 2>&1

set="$work/fab-scad.iconset"; mkdir -p "$set"
gen() { sips -z "$2" "$2" "$work/icon-1024.png" --out "$set/$1" >/dev/null; }
gen icon_16x16.png 16;      gen icon_16x16@2x.png 32
gen icon_32x32.png 32;      gen icon_32x32@2x.png 64
gen icon_128x128.png 128;   gen icon_128x128@2x.png 256
gen icon_256x256.png 256;   gen icon_256x256@2x.png 512
gen icon_512x512.png 512;   gen icon_512x512@2x.png 1024

iconutil -c icns "$set" -o fab-scad.icns
echo "wrote packaging/macos/fab-scad.icns ($(du -h fab-scad.icns | cut -f1))"
