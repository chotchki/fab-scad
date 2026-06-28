#!/bin/bash
# Characterize the 2^N slicing blowup (PLAN 4.1). See docs/slicing-blowup.md.
#
# Stacks the projects' real BOSL2 partition() configs to depth N around an inline leaf
# (standing in for the frozen import() STL) and times each render through `fab render`.
# The ratio column is each render over the previous; converging to ~2.0 is the 2^N
# signature. A series stops once a render crosses CAP seconds.
#
# Usage:  cargo build && bash docs/blowup_bench.sh

set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${FAB:-$ROOT/target/debug/fab}"
CAP=25   # seconds; stop a series past this

[ -x "$BIN" ] || { echo "fab binary not found at $BIN — run \`cargo build\` first (or set FAB)"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
BENCH="$TMP/bench.scad"
OUT="$TMP/bench_out.stl"
cd "$ROOT"   # so `fab` finds the root and injects OPENSCADPATH

gen() {  # $1=size  $2=cutsize  $3=gap  $4=depth
  cat > "$BENCH" <<EOF
include <BOSL2/std.scad>
\$fn = 32;
module slice_part() {
    partition(cutpath="jigsaw", size=$1, spin=90, cutsize=$2, gap=$3, spread=30)
        children();
}
module leaf() { cuboid([120, 60, 40], rounding=4, \$fn=16); }   // stands in for import(...stl)
// partition() instantiates children() TWICE, so leaf() runs 2^n times.
module stack(n) { if (n <= 0) leaf(); else slice_part() stack(n - 1); }
stack($4);
EOF
}

series() {  # $1=label  $2=size  $3=cutsize  $4=gap  $5=maxN
  echo "=== $1  (config: size=$2 cutsize=$3 gap=$4) ==="
  printf "%3s  %10s  %8s  %s\n" "N" "leaves" "secs" "ratio"
  prev=
  for ((N=0; N<=$5; N++)); do
    gen "$2" "$3" "$4" "$N"
    /usr/bin/time -p "$BIN" render "$BENCH" --out "$OUT" >/dev/null 2>"$TMP/timing.txt"
    real=$(awk '/^real/{print $2}' "$TMP/timing.txt")
    leaves=$(( 2 ** N ))
    if [ -n "$prev" ]; then
      ratio=$(awk -v a="$real" -v b="$prev" 'BEGIN{ if(b>0) printf "%.2fx", a/b; else print "-" }')
    else
      ratio="-"
    fi
    printf "%3d  %10d  %8s  %s\n" "$N" "$leaves" "$real" "$ratio"
    prev="$real"
    [ "$(awk -v r="$real" -v c="$CAP" 'BEGIN{print (r>c)?1:0}')" = "1" ] && { echo "    (stopped: >$CAP s)"; break; }
  done
  echo
}

# Real configs: window_light_blocker/slice_part() and shoe_holder/simpler_holder.scad.
series "window_light_blocker" "[300,300,60]" "[20,15]" "5"  10
series "shoe_holder"          "[300,300,60]" "[14,10]" "20"  9
echo "done — see docs/slicing-blowup.md"
