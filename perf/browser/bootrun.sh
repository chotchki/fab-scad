#!/usr/bin/env bash
# W.3.17 bonus: fab-gui COLD boot-to-first-render via the app's own ?model= link — the whole cold start a
# user feels (app wasm load + Bevy boot + libs fetch + geom-worker spawn + first render), not render-only.
# Drives the SHIPPED gui/web bundle (no rebuild) in headless Chrome, reads the boot->render span from the
# page's own console timestamps. The 8MB app download is instant on localhost, so this is the warm-network
# floor; a real first visit adds the download over the wire on top. Usage: bootrun.sh [model.scad] [port].
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root
MODEL="${1:-models/cart_brace/corner_brace.scad}"
PORT="${2:-8795}"
DIR="gui/web"

[[ -f "$DIR/index.html" && -f "$DIR/fab_gui.js" ]] || { echo "no app bundle at $DIR (build: gui/web/build-wasm.sh)" >&2; exit 1; }
cp "$MODEL" "$DIR/bench-model.scad"

CHROME="${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
[[ -x "$CHROME" ]] || for c in google-chrome chromium; do command -v "$c" >/dev/null 2>&1 && CHROME="$c" && break; done

CONSOLE="$(mktemp)"; PROFILE="$(mktemp -d)"; SRV=""; CH=""
cleanup() {
  [[ -n "$CH" ]] && kill "$CH" 2>/dev/null || true
  [[ -n "$SRV" ]] && kill "$SRV" 2>/dev/null || true
  rm -rf "$PROFILE" "$CONSOLE" "$DIR/bench-model.scad" 2>/dev/null || true
}
trap cleanup EXIT

python3 perf/browser/bench-server.py "$DIR" "$PORT" /tmp/_boot_unused.json >/dev/null 2>&1 &
SRV=$!; sleep 1

URL="http://127.0.0.1:${PORT}/index.html?model=bench-model.scad"
echo "boot: $(basename "$MODEL") -> $URL"
# The app renders a 3D canvas, so it needs (software) WebGL2 — SwiftShader, like the save-back e2e.
"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage --enable-logging=stderr --v=1 \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader-webgl \
  --user-data-dir="$PROFILE" --window-size=1000,700 "$URL" 2>"$CONSOLE" &
CH=$!

for _ in $(seq 1 60); do sleep 1; grep -aq "render complete: [1-9]" "$CONSOLE" && break; done

python3 - "$CONSOLE" <<'PY'
import re, sys
lines = open(sys.argv[1], errors="replace").read().splitlines()
def ts(l):
    m = re.search(r'/(\d{2})(\d{2})(\d{2})\.(\d+):', l)   # HHMMSS.micros
    return ((int(m.group(1))*60+int(m.group(2)))*60+int(m.group(3))) + int(m.group(4))/1e6 if m else None
console = [(ts(l), l) for l in lines if 'CONSOLE' in l and ts(l) is not None]
comp = [t for t, l in console if 'render complete' in l]
if console and comp:
    boot0 = console[0][0]
    print(f"cold boot -> first render: {comp[0]-boot0:.2f}s  (first app-console line -> 'render complete', localhost)")
else:
    print("no render-complete captured — see console tail below")
PY
echo "--- boot console (fab lines) ---"
grep -aE "fab-gui|render complete|panicked|RuntimeError|could not grow" "$CONSOLE" | tail -12
