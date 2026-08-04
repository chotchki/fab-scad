#!/usr/bin/env bash
# SZ.9.2 + SZ.9.3 — THE FULL WORKER RUNS, AND ITS GEOMETRY MATCHES THE DESKTOP'S.
#
# Two holes, one browser boot, because the same model closes both.
#
# SZ.9.2 — NOTHING HAD EVER BOOTED THE FULL WORKER. The sourceless demo is `include <fabdemo.scad>`
# and the save e2e's fixture is `color("red") cube(20)`; neither names a banded library, so
# `wanted_variant` routes both LEAN. Every browser gate in the repo was exercising the 1.3 MB
# worker. The 5.4 MB one — all 1361 transpiled natives, the entire point of phases AR and SZ — was
# shipping on the strength of "it compiles".
#
# SZ.9.3 — AND NOTHING COMPARED THE TWO PLATFORMS' GEOMETRY. `handle_with_store` is one function, so
# the browser and the desktop ought to agree trivially. What differs is the TARGET: wasm gets Rust's
# own libm for the transcendentals, rayon runs over wasm-bindgen-rayon, and wasm-opt rewrites the
# module after rustc is done with it. Any of those can move a vertex and none of them says so. A
# BOSL2 model is made of trig, which is exactly where a libm difference would show.
#
# The flow: serve `e2e-parity-model.scad` through the stub as a media item, let the app render and
# save it back, keep the uploaded mesh, and diff it against the desktop's answer to the SAME request.
#
# Usage: e2e-web-native-parity.sh [bundle_dir=gui/web] [port=8897] [timeout_s=240]
#   Build the bundle first: gui/web/build-wasm.sh
#   Override the browser with $CHROME_BIN; otherwise Chrome/Chromium is autodetected (Linux + mac).
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root
REPO="$PWD"

DIR="${1:-gui/web}"
PORT="${2:-8897}"
TIMEOUT="${3:-240}"
REF="0198e2e5a4e70000000000000000beef"   # a plausible 32-hex UUIDv7-shaped media_ref
MODEL="packaging/web/e2e-parity-model.scad"
CONSOLE="$(mktemp)"
PROFILE="$(mktemp -d)"
DUMP="$(mktemp -d)"
RECORD="$(mktemp)"

if [[ ! -f "$DIR/fab_gui.js" ]]; then
  echo "::error::no bundle at $DIR (need fab_gui.js). Build it: gui/web/build-wasm.sh" >&2
  exit 1
fi
# The full worker has to BE in the bundle. Checked up front so a missing artifact reports as itself
# rather than as a render that never finished.
if [[ ! -s "$DIR/geom/fab_geom_bg.wasm" ]]; then
  echo "::error::$DIR/geom/fab_geom_bg.wasm missing or empty — the full worker is not in the bundle" >&2
  exit 1
fi
# BOSL2 has to be checked out: the fixture includes it, the native render resolves it off disk, and
# the browser resolves it out of libs.json. A shallow clone would fail this in three confusing ways.
if [[ ! -f "libs/BOSL2/std.scad" ]]; then
  echo "::error::libs/BOSL2 is not checked out — git submodule update --init libs/BOSL2" >&2
  exit 1
fi

# --- locate a browser (CI: google-chrome; mac dev: the .app binary) ---------------------------------
CHROME="${CHROME_BIN:-}"
if [[ -z "$CHROME" ]]; then
  for c in google-chrome google-chrome-stable chromium chromium-browser; do
    command -v "$c" >/dev/null 2>&1 && CHROME="$c" && break
  done
fi
if [[ -z "$CHROME" && -x "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" ]]; then
  CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
fi
if [[ -z "$CHROME" ]]; then
  echo "::error::no Chrome/Chromium found (set \$CHROME_BIN)" >&2
  exit 1
fi

STUB_PID=""; CHROME_PID=""
cleanup() {
  [[ -n "$CHROME_PID" ]] && kill "$CHROME_PID" 2>/dev/null || true
  [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null || true
  # `|| true`: Chrome's lingering child procs can re-touch the profile dir, so `rm -rf` may hit a
  # transient "Directory not empty" — that must NOT flip a passing run to a failure via the EXIT trap.
  rm -rf "$PROFILE" "$CONSOLE" "$DUMP" "$RECORD" 2>/dev/null || true
}
trap cleanup EXIT

# The stub serves the fixture as the media item AND keeps every uploaded part (--dump), which is the
# only way the mesh the browser computed gets out of the browser.
python3 packaging/web/e2e-stub-server.py "$DIR" "$PORT" \
  --model "$MODEL" --dump "$DUMP" --record "$RECORD" >/dev/null 2>&1 &
STUB_PID=$!
sleep 1

[[ -f "$DIR/index.html" ]] || cp gui/web/index.html "$DIR/index.html"
# The deep-link the site emits, plus the e2e hook that fires Save once the render lands.
URL="http://127.0.0.1:${PORT}/index.html?model=%2Fmedia%2F${REF}%3Fformat%3Dscad&e2e=save"
echo "e2e(parity): $CHROME (headless) -> $URL"

# SwiftShader software WebGL2 (no GPU on CI). `--v=1` routes the PAGE's console.* to stderr (headless
# Chrome drops it otherwise). REAL wall-clock poll, not --virtual-time-budget: the render goes through
# the geom WORKER and virtual time races real worker threads (the fab-web A-phase lesson).
"$CHROME" --headless=new --no-sandbox --enable-logging=stderr --v=1 \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader-webgl \
  --user-data-dir="$PROFILE" --window-size=1000,700 \
  "$URL" 2>"$CONSOLE" &
CHROME_PID=$!

# The dumped mesh is the ground-truth success signal — no reliance on console routing. The console
# grep is a fast-fail so a panic doesn't burn the whole timeout.
ok=""
for _ in $(seq 1 $((TIMEOUT / 2))); do
  sleep 2
  [[ -s "$DUMP/high.3mf" ]] && ok=1 && break
  grep -aqE "save failed|RuntimeError|panicked|could not grow|model fetch failed" "$CONSOLE" && break
done

echo "--- console tail ---"
grep -aE "fab-gui (render complete|geom worker|e2e|init)|saved to|save failed|worker:|RuntimeError|panicked" "$CONSOLE" | tail -20 || true
echo "--------------------"

# --- 1. SZ.9.2: THE ROUTER PICKED THE FULL WORKER ---------------------------------------------------
# Asserted BEFORE the save outcome, and independently of it: a lean worker renders this model
# perfectly well (interpreting BOSL2 instead of dispatching it), so a green save would prove nothing
# about the artifact under test. Checking it first also means a save-pipeline flake still reports
# whether the full worker came up.
if ! grep -aq "fab-gui geom worker: full" "$CONSOLE"; then
  echo "::error::the app did NOT route to the full worker — this gate ran against the lean one"
  grep -a "fab-gui geom worker" "$CONSOLE" | head -3 || echo "(no worker-variant line at all)"
  tail -30 "$CONSOLE"
  exit 1
fi
echo "SZ.9.2 OK: the full geom worker loaded and served the render"

if grep -aqE "RuntimeError|panicked|could not grow" "$CONSOLE"; then
  echo "::error::the FULL geom worker crashed in the browser"
  grep -aE "RuntimeError|panicked|could not grow" "$CONSOLE" | head -5
  exit 1
fi
if [[ -z "$ok" ]]; then
  echo "::error::the browser never uploaded a mesh in ${TIMEOUT}s (no $DUMP/high.3mf)"
  ls -la "$DUMP" || true
  tail -30 "$CONSOLE"
  exit 1
fi

# --- 2. SZ.9.3: THE GEOMETRY MATCHES ----------------------------------------------------------------
# `--against` renders the SAME model natively through the browser's exact request shape
# (RenderWhole/Final/preview=false -> SaveMeshes/budget=None -> the `high` part), so the only variable
# left between the two meshes is the target. Anything else — a CLI render with its own defaults —
# would fold a settings difference into a platform comparison.
kill "$CHROME_PID" 2>/dev/null || true
echo "browser mesh: $(du -h "$DUMP/high.3mf" | cut -f1)"
cargo run --quiet --bin mesh_diff -- "$DUMP/high.3mf" --against "$MODEL" --root "$REPO"

echo "web/native parity: OK (full worker booted + browser and desktop agree on the geometry)"
