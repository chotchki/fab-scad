#!/bin/bash
# Release SAMPLING profile of one model's evaluation (PLAN N.1). See docs/models-profile.md.
#
# The tracing layer (tests/models_harness.rs) counts calls but INFLATES per-builtin ms — its
# own Instant+mutex per span dwarfs a 0.7us predicate. An external sampler doesn't touch the
# code, so its wall-time is honest. This runs the release `models_worker` (built WITH line-table
# debuginfo so frames symbolicate) under `samply`, then aggregates the capture into self/
# inclusive/allocation tables via scripts/profile-analyze.py.
#
# Needs: samply (`cargo install samply`) + atos (Xcode CLT). macOS/arm64 — the __TEXT base and
# `-arch arm64` in the analyzer are Apple-silicon specific.
#
# Usage:  bash scripts/profile-model.sh models/pill_holder/pill_holder_combined_tray.scad
#         RATE=4000 bash scripts/profile-model.sh <model.scad>   # bump the sample rate

set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODEL="${1:?usage: profile-model.sh <model.scad>}"
RATE="${RATE:-2000}"
OUT="${OUT:-/tmp/fab-profile.json.gz}"
BIN="$ROOT/target/release/models_worker"
cd "$ROOT"

command -v samply >/dev/null || { echo "samply not found — \`cargo install samply\`"; exit 1; }

# Line-table debuginfo on the release build (env-only, nothing committed) so atos can name frames.
echo "building release models_worker (+line-tables debuginfo)…"
CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release --bin models_worker >/dev/null

# The library search path the harness uses: BOSL2 under libs/, plus scad-lib/.
echo "sampling $MODEL at ${RATE}Hz…"
samply record --save-only --rate "$RATE" -o "$OUT" -- \
  "$BIN" "$MODEL" "$ROOT/libs" "$ROOT/scad-lib"

python3 "$ROOT/scripts/profile-analyze.py" "$OUT" "$BIN"
echo
echo "profile saved at $OUT — \`samply load $OUT\` for the flamegraph UI"
