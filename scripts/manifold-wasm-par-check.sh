#!/usr/bin/env bash
# M.6.1 compile gate: fab-manifold's `par` feature on BROWSER wasm (wasm32-unknown-unknown), i.e.
# rayon over wasm-bindgen-rayon's Web-Worker pool. Shared-memory wasm needs std rebuilt with
# atomics, hence nightly -Zbuild-std; the final LINK (and initThreadPool wiring) is the app's build
# (fab-gui, W phase) — this proves the kernel side compiles.
#
# Run from the repo root: ./scripts/manifold-wasm-par-check.sh
set -euo pipefail
cd "$(dirname "$0")/../manifold"

RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals' \
  cargo +nightly build --release \
  --target wasm32-unknown-unknown \
  --features par \
  -Z build-std=panic_abort,std

echo "manifold wasm+par: compile gate GREEN (wasm32-unknown-unknown, +atomics, build-std)"
