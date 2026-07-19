#!/usr/bin/env bash
# fab-gui wasm SMOKE build (W.3.5): cdylib -> wasm-bindgen -> gui/web/. Kernel-free (empty scene until
# the W.3.6 Worker), so pure-Rust wasm — NO C++/LLVM toolchain. Needs wasm-bindgen-cli in lockstep
# with the wasm-bindgen crate (0.2.126). Pass `--dev` for a fast (large) debug build.
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root

profile="release"; flag="--release"; dir="release"
if [[ "${1:-}" == "--dev" ]]; then profile="dev"; flag=""; dir="debug"; fi

echo "building fab-gui cdylib (wasm32, $profile)…"
cargo build --target wasm32-unknown-unknown -p fab-gui --lib $flag

echo "wasm-bindgen…"
wasm-bindgen --target web --no-typescript \
  --out-dir gui/web --out-name fab_gui \
  "target/wasm32-unknown-unknown/$dir/fab_gui.wasm"

# Size pass: bevy's unopt release wasm is ~110MB; wasm-opt -Oz strips + crushes it to ~20MB (browser-
# friendly). Skipped on --dev (fast iteration) and if binaryen is absent. Pin: binaryen v130 (apt's
# older builds corrupt wasm-bindgen's externref table).
if [[ "$profile" == "release" ]] && command -v wasm-opt >/dev/null; then
  echo "wasm-opt -Oz…"
  wasm-opt -Oz -o gui/web/fab_gui_bg.wasm gui/web/fab_gui_bg.wasm
fi

# The geometry Worker (W.3.6 → W.6): the kernel-only fab-geom wasm — PURE-RUST fab-manifold (C++ cut at
# M.7.4, no LLVM). Built THREADED (always --release, the perf-critical kernel): fab-manifold `par` over
# wasm-bindgen-rayon runs the boolean kernel on a rayon pool. Needs nightly + rust-src + a cross-origin-
# isolated (COOP/COEP) page for SharedArrayBuffer. TWO W.6.2 fixes (both REQUIRED — validated by the W.5.9
# headless e2e):
#  (1) SHARED MEMORY link-args: --shared-memory + --max-memory + --import-memory make wasm-ld emit an
#      IMPORTED SHARED memory. Without them the memory is non-shared → initThreadPool's postMessage of the
#      Memory to the rayon sub-workers throws DataCloneError → no geometry renders. The __tls_*/__heap_*
#      exports are what wasm-bindgen's thread transform needs (it errors "failed to find __heap_base" else).
#  (2) The workerHelpers.js `import('../../..')` PATCH below.
echo "building fab-geom worker (fab-manifold, threaded: nightly build-std + shared memory)…"
RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base -C link-arg=--export=__heap_end' \
  cargo +nightly build -p fab-geom --release --target wasm32-unknown-unknown \
  --features par -Z build-std=panic_abort,std
mkdir -p gui/web/geom
wasm-bindgen --target web --no-typescript --out-name fab_geom --out-dir gui/web/geom \
  "target/wasm32-unknown-unknown/release/fab_geom.wasm"
# wasm-bindgen-rayon's workerHelpers.js does `import('../../..')` — a bundler/package.json assumption that
# resolves to the out-DIR, not a module. Raw `--target web` has no bundler, so point the sub-worker's
# dynamic import at the real entry; else every rayon sub-worker fails to load and initThreadPool HANGS
# (no error — the pool just never reports ready). `sed -i.bak` is portable across BSD (macOS) + GNU (CI).
for wh in gui/web/geom/snippets/wasm-bindgen-rayon-*/src/workerHelpers.js; do
  [[ -f "$wh" ]] && sed -i.bak "s#import('\.\./\.\./\.\.')#import('../../../fab_geom.js')#" "$wh" && rm -f "$wh.bak"
done
if [[ "$profile" == "release" ]] && command -v wasm-opt >/dev/null; then
  # --enable-threads preserves the atomics/shared-memory ops through the opt pass.
  wasm-opt -Oz --enable-threads --enable-reference-types --enable-bulk-memory \
    -o gui/web/geom/fab_geom_bg.wasm gui/web/geom/fab_geom_bg.wasm
fi
cp packaging/web/geom-worker.js gui/web/geom/

# The scad LIB PACK (W.3.6 Stage 2): BOSL2 + scad-lib + the web demo, one JSON the app fetches once
# and computes each model's include closure from. Served at the bundle root.
python3 packaging/web/pack_scad_libs.py gui/web/libs.json

sz=$(du -h gui/web/fab_gui_bg.wasm | cut -f1)
gsz=$(du -h gui/web/geom/fab_geom_bg.wasm | cut -f1)
echo "built -> gui/web/fab_gui.js + fab_gui_bg.wasm ($sz) + geom/fab_geom_bg.wasm ($gsz)"
# MUST serve with COOP/COEP: the threaded geom worker needs SharedArrayBuffer (cross-origin isolation)
# or initThreadPool can't create the shared memory. dev-server.py sets both; plain http.server does NOT.
# For the save-back round-trip e2e instead: packaging/web/e2e-save.sh gui/web
echo "serve:  python3 packaging/web/dev-server.py gui/web 8080   # COOP/COEP on -> http://127.0.0.1:8080"
