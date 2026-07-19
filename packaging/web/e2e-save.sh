#!/usr/bin/env bash
# W.5.9 save-back BROWSER e2e: boot a PREBUILT fab-gui bundle in headless Chrome against the stub
# hotchkiss.io (e2e-stub-server.py), auto-fire the Save via the `?e2e=save` hook, and assert the stub
# received the 3-variant PUT with the session cookie. No DOM/canvas click — egui buttons are canvas
# pixels with no accessibility node on wasm, so the app self-drives via the URL param and we grep the
# console + read the stub's record (the same console-grep shape as the release-web.yml boot gate).
#
# Usage: e2e-save.sh [bundle_dir=gui/web] [port=8788] [timeout_s=180]
#   Build the bundle first: gui/web/build-wasm.sh   (CI passes its staged dir instead).
#   Override the browser with $CHROME_BIN; the script otherwise autodetects Chrome/Chromium (Linux + mac).
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root

DIR="${1:-gui/web}"
PORT="${2:-8788}"
TIMEOUT="${3:-180}"
REF="0198e2e5a4e70000000000000000cafe"   # a plausible 32-hex UUIDv7-shaped media_ref
CONSOLE="$(mktemp)"
PROFILE="$(mktemp -d)"

if [[ ! -f "$DIR/fab_gui.js" || ! -f "$DIR/index.html" ]]; then
  echo "::error::no bundle at $DIR (need fab_gui.js + index.html). Build it: gui/web/build-wasm.sh" >&2
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
  rm -rf "$PROFILE" "$CONSOLE"
}
trap cleanup EXIT

# --- serve the bundle + the stub site ---------------------------------------------------------------
python3 packaging/web/e2e-stub-server.py "$DIR" "$PORT" >/dev/null 2>&1 &
STUB_PID=$!
sleep 1

# The deep-link the site emits: ?model=/media/<ref>?format=scad (URL-encoded), plus the e2e hook.
URL="http://127.0.0.1:${PORT}/index.html?model=%2Fmedia%2F${REF}%3Fformat%3Dscad&e2e=save"
echo "e2e: $CHROME (headless) -> $URL"

# SwiftShader software WebGL2 (no GPU on CI). `--v=1` routes the PAGE's console.* to stderr (headless
# Chrome drops it otherwise). REAL wall-clock poll, not --virtual-time-budget: the save renders through
# the geom WORKER and virtual time races real worker threads.
"$CHROME" --headless=new --no-sandbox --enable-logging=stderr --v=1 \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader-webgl \
  --user-data-dir="$PROFILE" --window-size=1000,700 \
  "$URL" 2>"$CONSOLE" &
CHROME_PID=$!

# --- wait for the PUT to land (or a failure) --------------------------------------------------------
# The stub RECORDING the PUT is the ground-truth success signal — platform-independent, no reliance on
# console routing. The console grep is only a fast-fail for a panic/crash so we don't burn the full timeout.
ok=""
fail=""
for _ in $(seq 1 $((TIMEOUT / 2))); do
  sleep 2
  if curl -s "http://127.0.0.1:${PORT}/__e2e/state" | grep -q '"method": "PUT"'; then ok=1; break; fi
  if grep -aqE "save failed|RuntimeError|panicked|could not grow|model fetch failed" "$CONSOLE"; then fail=1; break; fi
done

echo "--- console tail ---"; grep -aE "fab-gui (render complete|e2e|init)|saved to|save failed|worker:|RuntimeError|panicked" "$CONSOLE" | tail -20 || true
echo "--------------------"

if [[ -n "$fail" || -z "$ok" ]]; then
  echo "::error::save round-trip did not complete in ${TIMEOUT}s"; tail -30 "$CONSOLE" || true
  exit 1
fi

# --- assert the stub actually received the 3-variant PUT with the cookie -----------------------------
STATE="$(curl -s "http://127.0.0.1:${PORT}/__e2e/state")"
echo "stub record: $STATE"
python3 - "$STATE" <<'PY'
import json, sys
s = json.loads(sys.argv[1] or "{}")
errs = []
if s.get("method") != "PUT": errs.append(f"method={s.get('method')} (want PUT)")
if s.get("count") != 3: errs.append(f"count={s.get('count')} (want 3)")
if not s.get("has_scad"): errs.append("no .scad variant")
if not s.get("mesh_ext_match"): errs.append("low/high mesh formats differ (roundtrip rule)")
if not s.get("cookie_present"): errs.append("session cookie did not ride the PUT")
if errs:
    print("::error::stub assertions failed: " + "; ".join(errs)); sys.exit(1)
print(f"OK: PUT {s['ref']}/variants — 3 files {[p['ext'] for p in s['parts']]}, cookie rode")
PY
echo "e2e save round-trip: OK"
