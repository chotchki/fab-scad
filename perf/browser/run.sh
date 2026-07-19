#!/usr/bin/env bash
# W.3.17 driver: stage the model subset, serve perf/browser under COOP/COEP, boot headless Chrome at
# harness.html, wait for the harness to POST results server-side (console routing is unreliable), then
# print the table + leave results.json. Prereqs: build-fab-wasm.sh (fab worker) + the W.3.17.1 OpenSCAD
# recovery (vendor/openscad.{js,wasm} + osc-libs.json). Usage: run.sh [port=8791] [timeout_s=1200].
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root
BD="perf/browser"
PORT="${1:-8791}"
TIMEOUT="${2:-1200}"
RESULT="$BD/results.json"

# name|path — light win → heavy win → an OpenSCAD-wasm stack-overflow → an OpenSCAD-native-TIMEOUT.
# (bowtie/second_approach is omitted: it import()s an external SVG asset the bench doesn't mount, so
# BOTH engines fail on it — it stays the honest loss in the NATIVE table, not this browser one.)
MODELS='corner_brace|models/cart_brace/corner_brace.scad
ashtray|models/ashtray/ashtray.scad
angled_laptop_holder|models/Underdesk/angled_laptop_holder.scad
garage_door|models/garage_door/garage_door.scad
pill_holder|models/pill_holder/pill_holder.scad
traced_holder|models/controller_charger_holder/traced_holder.scad'

for f in "$BD/vendor/geom/fab_geom.js" "$BD/vendor/openscad.wasm" "$BD/vendor/openscad.js" \
         "$BD/vendor/fab-libs.json" "$BD/vendor/osc-libs.json"; do
  [[ -f "$f" ]] || { echo "MISSING $f — run $BD/build-fab-wasm.sh + the W.3.17.1 recovery first" >&2; exit 1; }
done

mkdir -p "$BD/vendor/models"
# ONLY=<substr> restricts the run to matching model names (fast smoke test before the long heavy run).
MODELS="$MODELS" python3 - "$BD" <<'PY'
import json, os, shutil, sys
bd = sys.argv[1]
only = os.environ.get("ONLY", "")
rows = []
for line in os.environ["MODELS"].strip().splitlines():
    name, path = line.split("|")
    if only and only not in name:
        continue
    shutil.copyfile(path, os.path.join(bd, "vendor/models", name + ".scad"))
    rows.append({"name": name, "url": "vendor/models/%s.scad" % name})
json.dump(rows, open(os.path.join(bd, "vendor/models.json"), "w"))
print("staged", len(rows), "models")
PY

CHROME="${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
if [[ ! -x "$CHROME" ]]; then
  for c in google-chrome google-chrome-stable chromium chromium-browser; do
    command -v "$c" >/dev/null 2>&1 && CHROME="$c" && break
  done
fi
[[ -x "$CHROME" || -n "$(command -v "$CHROME" 2>/dev/null)" ]] || { echo "no Chrome (set \$CHROME_BIN)" >&2; exit 1; }

CONSOLE="$(mktemp)"; PROFILE="$(mktemp -d)"; SRV_PID=""; CHROME_PID=""
cleanup() {
  [[ -n "$CHROME_PID" ]] && kill "$CHROME_PID" 2>/dev/null || true
  [[ -n "$SRV_PID" ]] && kill "$SRV_PID" 2>/dev/null || true
  rm -rf "$PROFILE" "$CONSOLE" 2>/dev/null || true
}
trap cleanup EXIT

rm -f "$RESULT"
python3 "$BD/bench-server.py" "$BD" "$PORT" "$RESULT" >/dev/null 2>&1 &
SRV_PID=$!
sleep 1

URL="http://127.0.0.1:${PORT}/harness.html"
echo "bench: $CHROME (headless) -> $URL   (timeout ${TIMEOUT}s)"
"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage --enable-logging=stderr --v=1 \
  --user-data-dir="$PROFILE" "$URL" 2>"$CONSOLE" &
CHROME_PID=$!

done=""
for _ in $(seq 1 $((TIMEOUT / 2))); do
  sleep 2
  [[ -s "$RESULT" ]] && { done=1; break; }
done

echo "--- console (fab/PERF/WARN lines) ---"
grep -aE "PERF |WARN:|fab worker ready|loaded [0-9]|initThreadPool" "$CONSOLE" | tail -25 || true
echo "-------------------------------------"

if [[ -z "$done" ]]; then
  echo "::error:: bench did not report in ${TIMEOUT}s"; tail -30 "$CONSOLE" || true; exit 1
fi

echo "results -> $RESULT"
python3 - "$RESULT" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
if d.get("fatal"):
    print("FATAL:", d["fatal"]); sys.exit(1)
print(f"\nthreads={d['threads']} crossOriginIsolated={d['xoi']}")
print(f"{'model':<26}{'fab ms':>9}{'osc ms':>9}{'osc rt':>9}{'o/f':>7}  status")
for r in d["rows"]:
    fab = r['fab_ms'] if r['fab_ms'] is not None else r['fab_status']
    osc = r['osc_ms'] if r['osc_ms'] is not None else r['osc_status']
    rt = r['osc_rt_ms'] if r.get('osc_rt_ms') else '-'
    ratio = f"{r['ratio']}x" if r['ratio'] else '-'
    st = []
    if r['fab_status'] != 'ok': st.append('fab:' + r['fab_status'] + (f"({r['fab_err'][:40]})" if r.get('fab_err') else ''))
    if r['osc_status'] != 'ok': st.append('osc:' + r['osc_status'] + (f"({r['osc_err'][:40]})" if r.get('osc_err') else ''))
    print(f"{r['model']:<26}{str(fab):>9}{str(osc):>9}{str(rt):>9}{ratio:>7}  {' '.join(st)}")
PY
