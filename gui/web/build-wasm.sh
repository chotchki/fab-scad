#!/usr/bin/env bash
# THE ONE BUILDER for the fab-gui web bundle: the app cdylib, BOTH geom worker variants, and the scad
# lib pack. Local dev, the CI boot gate and the release all call THIS — the release adds only
# compression, the manifest and the tarball on top.
#
# It is one script because it was three (W.3.5). `build-wasm.sh`, `ci.yml` and `release-web.yml` each
# carried their own copy of the same 60 lines, and copies drift: SZ.2 replaced the python lib-packer
# with `cargo run --bin pack_libs` and updated two of the three, leaving CI calling a script that no
# longer existed. Worse, CI's copy had never gained the threading flags at all, so the boot gate
# proved a SERIAL worker boots while the release shipped a THREADED one — the artifact under test was
# not the artifact published. Both are structural, and one script is the structural fix.
#
# Usage: build-wasm.sh [--dev] [--stage DIR] [--no-opt]
#   --dev        debug build of the APP (fast, large). The geom workers stay release — they are the
#                perf-critical kernel and a debug build of them is unusably slow.
#   --stage DIR  write the bundle here instead of gui/web (CI/release pass their staging dir).
#   --no-opt     skip wasm-opt. Size gates the RELEASE, not boot, so the CI gate skips a slow pass
#                that cannot change whether the thing runs.
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root

profile="release"; flag="--release"; dir="release"; stage="gui/web"; opt=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    # --dev also turns wasm-opt OFF, for every artifact — the old script gated all three passes on
    # profile==release, and `--dev` means "iterate fast", which a multi-minute -Oz over a 47 MB bevy
    # wasm is the opposite of.
    --dev)     profile="dev"; flag=""; dir="debug"; opt=0; shift ;;
    --stage)   stage="${2:?--stage needs a directory}"; shift 2 ;;
    --no-opt)  opt=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
mkdir -p "$stage"

# ONE definition of the geom worker's link flags, used by BOTH variants. The lean worker is the same
# kernel with a feature turned off; if it quietly lost the shared memory the failure would surface
# only at runtime, in a thread pool that never reports ready, in whichever half of the user base
# happened to route to it.
GEOM_RUSTFLAGS='-C link-arg=-zstack-size=16777216 -C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 -C link-arg=--import-memory -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base -C link-arg=--export=__heap_end'

# wasm-opt, then PROVE it wrote something. A crashed optimiser exits 0 having written a ZERO-BYTE
# file, and the caller installs it as the shipped kernel: binaryen v130 SIGBUSes on -O2/-Os/-Oz over
# the geom module (silently) and that is exactly how it fails. `set -e` catches a non-zero exit, not
# a zero-byte success.
run_wasm_opt() {  # <level> <file> <extra flags...>
  local level="$1" file="$2"; shift 2
  if [[ "$opt" != 1 ]] || ! command -v wasm-opt >/dev/null; then return 0; fi
  echo "wasm-opt $level $(basename "$(dirname "$file")")/$(basename "$file")…"
  wasm-opt "$level" "$@" -o "$file.opt" "$file"
  [[ -s "$file.opt" ]] || { echo "::error::wasm-opt produced an EMPTY $file"; exit 1; }
  mv "$file.opt" "$file"
}

# wasm-bindgen-rayon's workerHelpers.js does `import('../../..')` — a bundler/package.json assumption
# that resolves to the out-DIR, not a module. Raw `--target web` has no bundler, so point the
# sub-worker's dynamic import at the real entry; else every rayon sub-worker fails to load and
# initThreadPool HANGS (no error — the pool just never reports ready). `sed -i.bak` is portable
# across BSD (macOS) + GNU (CI).
patch_worker_helpers() {  # <out dir>
  for wh in "$1"/snippets/wasm-bindgen-rayon-*/src/workerHelpers.js; do
    [[ -f "$wh" ]] && sed -i.bak "s#import('\.\./\.\./\.\.')#import('../../../fab_geom.js')#" "$wh" && rm -f "$wh.bak"
  done
}

# One geom worker variant: build, bind, patch, optimise. The two differ ONLY in cargo features —
# `--no-default-features` drops the `libraries` feature and with it the transpiled band.
build_geom() {  # <out subdir> <cargo feature flags...>
  local out="$stage/$1"; shift
  mkdir -p "$out"
  # `${FAB_GEOM_TOOLCHAIN-+nightly}`: local runs keep `+nightly` (the worker needs -Zbuild-std),
  # but CI sets the var EMPTY so this build uses the workflow's dtolnay toolchain — a hardcoded
  # `+nightly` here resolves to the LATEST nightly behind the workflow's back, which re-broke
  # boot-gate the day ci.yml pinned nightly-2026-08-12 (rustup synced a fresh nightly without
  # rust-src and would have hit the same bevy E0034 the pin exists to dodge). No-colon expansion
  # on purpose: empty means "use the default toolchain", only UNSET falls back to +nightly.
  RUSTFLAGS="$GEOM_RUSTFLAGS" cargo ${FAB_GEOM_TOOLCHAIN-+nightly} build -p fab-geom --release \
    --target wasm32-unknown-unknown "$@" -Z build-std=panic_abort,std
  wasm-bindgen --target web --no-typescript --out-name fab_geom --out-dir "$out" \
    "target/wasm32-unknown-unknown/release/fab_geom.wasm"
  patch_worker_helpers "$out"
  # --enable-threads preserves the atomics/shared-memory ops through the opt pass (else wasm-opt
  # strips them and the shared memory is invalid). -O1, NOT -Oz: see run_wasm_opt.
  run_wasm_opt -O1 "$out/fab_geom_bg.wasm" --enable-threads --enable-reference-types --enable-bulk-memory
  cp packaging/web/geom-worker.js "$out/"
}

# --- the app: cdylib -> wasm-bindgen -> $stage ------------------------------------------------------
# Kernel-free (the geometry runs in the worker), so pure-Rust wasm with no C++/LLVM toolchain. Needs
# wasm-bindgen-cli in lockstep with the wasm-bindgen crate.
echo "building fab-gui cdylib (wasm32, $profile)…"
cargo build --target wasm32-unknown-unknown -p fab-gui --lib $flag
wasm-bindgen --target web --no-typescript --out-dir "$stage" --out-name fab_gui \
  "target/wasm32-unknown-unknown/$dir/fab_gui.wasm"
# bevy's unopt release wasm is ~110MB; -Oz strips + crushes it to ~20MB. Skipped on --dev/--no-opt.
# The explicit feature flags defend against any binaryen too old to read the module's feature
# section — release-web.yml carried them and this script did not, so consolidating on the version
# WITHOUT them would have been a quiet downgrade of the shipped build.
run_wasm_opt -Oz "$stage/fab_gui_bg.wasm" --enable-reference-types --enable-bulk-memory

# --- the geometry workers (W.3.6 -> W.6), BOTH variants ---------------------------------------------
# THREADED: fab-manifold's `par` over wasm-bindgen-rayon runs the boolean kernel on a rayon pool,
# which needs nightly + rust-src to rebuild std with atomics, and a cross-origin-isolated (COOP/COEP)
# page for SharedArrayBuffer. Serve with packaging/web/dev-server.py, NOT plain http.server.
echo "building fab-geom FULL worker (transpiled band, threaded)…"
build_geom geom --features par
# SZ.4 — the LEAN worker beside it: same kernel, same evaluator, no transpiled band (1.3 MB brotli
# against the full 5.4). The app routes to it when a model's include closure names no banded
# library, so a `cube(10);` visitor never fetches 4.1 MB of natives they cannot call into.
echo "building fab-geom LEAN worker (no transpiled band)…"
build_geom geom-lean --no-default-features --features par

# --- the scad LIB PACK (W.3.6 Stage 2) --------------------------------------------------------------
# BOSL2 + MCAD + machineblocks + scad-lib + the web demo, one JSON the app fetches once and computes
# each model's include closure from. Built from `import::libraries()` — the SAME declaration the
# transpiled band is registered from, so the browser's source and the build's rows cannot drift.
cargo run --quiet --bin pack_libs -- "$stage/libs.json"

# BOTH variants have to be present or the app breaks on whichever half of models routes to the
# missing one — and it breaks by loading a worker that never comes up, not by erroring.
for v in geom geom-lean; do
  [[ -s "$stage/$v/fab_geom_bg.wasm" ]] || { echo "::error::$v/fab_geom_bg.wasm missing or empty"; exit 1; }
  [[ -s "$stage/$v/geom-worker.js" ]]   || { echo "::error::$v/geom-worker.js missing"; exit 1; }
done

echo "built -> $stage"
echo "  app:        $(du -h "$stage/fab_gui_bg.wasm" | cut -f1)"
echo "  geom full:  $(du -h "$stage/geom/fab_geom_bg.wasm" | cut -f1)"
echo "  geom lean:  $(du -h "$stage/geom-lean/fab_geom_bg.wasm" | cut -f1)"
echo "  libs.json:  $(du -h "$stage/libs.json" | cut -f1)"
# MUST serve with COOP/COEP: the threaded geom worker needs SharedArrayBuffer (cross-origin
# isolation) or initThreadPool can't create the shared memory. dev-server.py sets both; plain
# http.server does NOT. For the save-back round-trip e2e: packaging/web/e2e-save.sh $stage
echo "serve:  python3 packaging/web/dev-server.py $stage 8080   # COOP/COEP on -> http://127.0.0.1:8080"
