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

# The geometry Worker (W.3.6): the kernel-only fab-geom wasm — PURE-RUST fab-manifold (the C++
# manifold3d/wasm-cxx-shim was CUT at M.7.4), so NO LLVM/C++ toolchain. Built SERIAL (always --release,
# the perf-critical kernel): the THREADED build (fab-manifold `par` over wasm-bindgen-rayon: nightly
# `-Z build-std` + atomics) compiles but is runtime-BROKEN in-browser — its wasm Memory isn't actually
# shared, so `initThreadPool`'s postMessage of that Memory to the rayon sub-workers throws
# DataCloneError and NO geometry renders (W.5.9 headless-e2e finding). Re-enabling threading + fixing the
# shared-memory setup + measuring the speedup is W.6.2; the serial worker is the working shippable state.
# geom-worker.js tolerates BOTH builds (namespace import + best-effort pool spin-up), so no page-side
# change is needed when threading returns.
echo "building fab-geom worker (fab-manifold, serial — threading parked at W.6.2)…"
cargo build -p fab-geom --release --target wasm32-unknown-unknown
mkdir -p gui/web/geom
wasm-bindgen --target web --no-typescript --out-name fab_geom --out-dir gui/web/geom \
  "target/wasm32-unknown-unknown/release/fab_geom.wasm"
if [[ "$profile" == "release" ]] && command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-reference-types --enable-bulk-memory \
    -o gui/web/geom/fab_geom_bg.wasm gui/web/geom/fab_geom_bg.wasm
fi
cp packaging/web/geom-worker.js gui/web/geom/

# The scad LIB PACK (W.3.6 Stage 2): BOSL2 + scad-lib + the web demo, one JSON the app fetches once
# and computes each model's include closure from. Served at the bundle root.
python3 packaging/web/pack_scad_libs.py gui/web/libs.json

sz=$(du -h gui/web/fab_gui_bg.wasm | cut -f1)
gsz=$(du -h gui/web/geom/fab_geom_bg.wasm | cut -f1)
echo "built -> gui/web/fab_gui.js + fab_gui_bg.wasm ($sz) + geom/fab_geom_bg.wasm ($gsz)"
# dev-server.py sets COOP/COEP (harmless for the serial worker; REQUIRED once threading returns at
# W.6.2 — SharedArrayBuffer). For the save-back round-trip e2e instead: packaging/web/e2e-save.sh gui/web
echo "serve:  python3 packaging/web/dev-server.py gui/web 8080   # -> http://127.0.0.1:8080"
