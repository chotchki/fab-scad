#!/usr/bin/env bash
# W.3.17: build fab's geom worker wasm for the browser perf head-to-head — THREADED (par) + the `bench`
# export (render_scad_stl). Mirrors gui/web/build-wasm.sh's threaded-worker steps exactly (shared-memory
# link-args + the workerHelpers.js import patch, both REQUIRED for wasm-bindgen-rayon), but adds
# --features bench and lands in perf/browser/vendor/geom (this bench, not the shipped bundle). Needs
# nightly + rust-src + wasm-bindgen-cli in lockstep (0.2.126).
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root
out="perf/browser/vendor/geom"

echo "building fab-geom (wasm32, release, par+bench, threaded build-std)…"
RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base -C link-arg=--export=__heap_end' \
  cargo +nightly build -p fab-geom --release --target wasm32-unknown-unknown \
  --features par,bench -Z build-std=panic_abort,std

mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-name fab_geom --out-dir "$out" \
  "target/wasm32-unknown-unknown/release/fab_geom.wasm"

# wasm-bindgen-rayon's sub-worker does import('../../..') — point it at the real entry or initThreadPool HANGS.
for wh in "$out"/snippets/wasm-bindgen-rayon-*/src/workerHelpers.js; do
  [[ -f "$wh" ]] && sed -i.bak "s#import('\.\./\.\./\.\.')#import('../../../fab_geom.js')#" "$wh" && rm -f "$wh.bak"
done

if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-threads --enable-reference-types --enable-bulk-memory \
    -o "$out/fab_geom_bg.wasm" "$out/fab_geom_bg.wasm"
fi

# fab's include pack (BOSL2 + scad-lib), keys like BOSL2/std.scad — reuse the shipped one.
python3 packaging/web/pack_scad_libs.py perf/browser/vendor/fab-libs.json
echo "built -> $out/fab_geom_bg.wasm ($(du -h "$out/fab_geom_bg.wasm" | cut -f1)) + vendor/fab-libs.json"
