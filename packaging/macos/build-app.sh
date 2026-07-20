#!/usr/bin/env bash
# W.2.3: build the desktop fab-gui `.app` for LOCAL dogfooding — release binaries → cargo-packager →
# ad-hoc codesign, so it launches on THIS machine without the Apple Developer-ID + notarization bill.
# That paid, distributable path stays W.2.2's call; this makes a runnable-here app in one command, not
# a shippable one. macOS + arm64/x86 host only. Icons come from Packager.toml (regenerate via make-icon.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root

[[ "$(uname)" == "Darwin" ]] || { echo "macOS only (this bundles a .app)"; exit 1; }
command -v cargo-packager >/dev/null 2>&1 || {
  echo "need cargo-packager — install it once: cargo install cargo-packager --locked" >&2
  exit 1
}

echo "1/3 building release binaries (fab-gui app + fab CLI)…"
cargo build --release --workspace --bins

echo "2/3 packaging the .app (skipping the DMG — that's for distribution)…"
cargo packager --release --formats app

APP="target/packager/fab-scad.app"
[[ -d "$APP" ]] || { echo "no .app at $APP — did cargo-packager change its out-dir?" >&2; exit 1; }

# Ad-hoc sign (-s -): a valid but anonymous signature so macOS lets it run locally. NOT Developer-ID /
# notarized — `spctl` will still reject it for GATEKEEPER-distribution (W.2.2's $99/yr + rcodesign call).
# Clearing the quarantine xattr keeps a locally-built app from nagging on first open.
echo "3/3 ad-hoc signing…"
codesign --deep --force --sign - "$APP"
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
codesign --verify --verbose=2 "$APP" 2>&1 | tail -1 || true

echo
echo "built + ad-hoc signed → $APP"
echo "run it:  open '$APP'    (or drag it to /Applications)"
