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

# The geometry Worker (W.3.6): the kernel-only fab-geom wasm (Manifold via wasm-cxx-shim) in its own
# Web Worker. Needs LLVM-21 for the C++ build — prepend it (override with LLVM_BIN=… on other setups).
LLVM_BIN="${LLVM_BIN:-/opt/homebrew/opt/llvm@21/bin}"
echo "building fab-geom worker (manifold-wasm, LLVM-21)…"
PATH="$LLVM_BIN:$PATH" cargo build -p fab-geom --release --target wasm32-unknown-unknown
mkdir -p gui/web/geom
wasm-bindgen --target web --no-typescript --out-name fab_geom --out-dir gui/web/geom \
  "target/wasm32-unknown-unknown/$dir/fab_geom.wasm"
if [[ "$profile" == "release" ]] && command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-reference-types --enable-bulk-memory \
    -o gui/web/geom/fab_geom_bg.wasm gui/web/geom/fab_geom_bg.wasm
fi
cp packaging/web/geom-worker.js gui/web/geom/

sz=$(du -h gui/web/fab_gui_bg.wasm | cut -f1)
gsz=$(du -h gui/web/geom/fab_geom_bg.wasm | cut -f1)
echo "built -> gui/web/fab_gui.js + fab_gui_bg.wasm ($sz) + geom/fab_geom_bg.wasm ($gsz)"
echo "serve:  python3 -m http.server --directory gui/web 8080   # then open http://localhost:8080"
