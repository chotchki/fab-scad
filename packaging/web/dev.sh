#!/usr/bin/env bash
# Local fab-web loop: build -> bindgen -> stage the contract dir -> serve with prod headers.
# The release pipeline (release-web.yml) is for SHIPPING; this is the seconds-scale iterate loop.
#
#   ./packaging/web/dev.sh               # build + stage + serve http://127.0.0.1:8787/
#   ./packaging/web/dev.sh --stage-only  # refresh target/fab-web/stage and exit — the dir a
#                                        # hotchkiss-io local override consumes instead of the
#                                        # GitHub release (see docs/web-bundle.md)
#   PORT=9000 ./packaging/web/dev.sh     # different port
#
# Differences vs the released artifact, on purpose: no wasm-opt (slow, size-only) and fast
# compression levels — the file SET still matches the contract so consumers don't special-case.
set -euo pipefail
cd "$(dirname "$0")/../.."

PKG=${FAB_WEB_PKG:-fab-web} # workspace member; underscored name below is the cdylib artifact
cargo build -p "$PKG" --release --target wasm32-unknown-unknown
WASM="target/wasm32-unknown-unknown/release/${PKG//-/_}.wasm"

STAGE=target/fab-web/stage
rm -rf "$STAGE" && mkdir -p "$STAGE" # stale stage files once masked a broken loader — always clean
wasm-bindgen --target web --no-typescript --out-name fab_web --out-dir "$STAGE" "$WASM"
# wasm-opt PARITY with CI (the v0.1.0 lesson: the one transform dev skipped was the one that
# broke prod). Explicit feature flags defend against binaryens too old to read the feature section.
if command -v wasm-opt >/dev/null; then
    wasm-opt -Oz --enable-reference-types --enable-bulk-memory \
        -o "$STAGE/fab_web_bg.wasm.opt" "$STAGE/fab_web_bg.wasm"
    mv "$STAGE/fab_web_bg.wasm.opt" "$STAGE/fab_web_bg.wasm"
else
    echo "WARN: wasm-opt not installed (brew install binaryen) — stage skips the CI transform"
fi
cp packaging/web/index.reference.html "$STAGE/index.html"

# The OpenSCAD side-module (Phase B): the PINNED official wasm build + our worker glue + the
# lib pack (BOSL2 @ the submodule pin + scad-lib @ this commit). All lazily fetched by the app,
# so STL-only users never pay for them. GPL module ships UNMODIFIED — see web-bundle.md.
OSC_VER="2026.07.01"
OSC_SHA="10e0937dd2627116a1f7ab6fb1ed75762b66572f21f4e5054c7dc00526cdd1ff"
OSC_CACHE="target/openscad-cache"
if [ ! -f "$OSC_CACHE/openscad.wasm" ]; then
    mkdir -p "$OSC_CACHE"
    curl -sL -o "$OSC_CACHE/osc.zip" "https://files.openscad.org/snapshots/OpenSCAD-$OSC_VER-WebAssembly-web.zip"
    echo "$OSC_SHA  $OSC_CACHE/osc.zip" | shasum -a 256 -c - >/dev/null
    unzip -o -q "$OSC_CACHE/osc.zip" -d "$OSC_CACHE"
fi
mkdir -p "$STAGE/openscad"
cp "$OSC_CACHE/openscad.js" "$OSC_CACHE/openscad.wasm" "$STAGE/openscad/"
cp packaging/web/openscad-worker.js packaging/web/OPENSCAD-NOTICE.txt "$STAGE/openscad/"
python3 packaging/web/pack_libs.py "$STAGE/openscad/libs.json"
# Contract-complete variants at dev-grade compression (brotli -q5 ~seconds vs -q11 ~minutes).
if command -v brotli >/dev/null; then brotli -q 5 -kf "$STAGE/fab_web_bg.wasm"; fi
gzip -6 -kf "$STAGE/fab_web_bg.wasm"
python3 - "$STAGE" <<'EOF'
import hashlib, json, os, sys
stage = sys.argv[1]
sha = {}
for root, _, names in os.walk(stage):
    for n in sorted(names):
        rel = os.path.relpath(os.path.join(root, n), stage)
        if rel in ("manifest.json", "index.html"):
            continue
        sha[rel] = hashlib.sha256(open(os.path.join(root, n), "rb").read()).hexdigest()
json.dump({"version": "dev", "entry": "fab_web.js", "wasm": "fab_web_bg.wasm", "sha256": sha},
          open(os.path.join(stage, "manifest.json"), "w"), indent=1)
EOF
echo "staged -> $STAGE"

[ "${1:-}" = "--stage-only" ] && exit 0
exec python3 packaging/web/dev-server.py "$STAGE" "${PORT:-8787}"
