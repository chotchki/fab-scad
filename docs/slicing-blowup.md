# The 2^N slicing blowup (and why linear slicing kills it)

The motivating bug for the whole slicer effort (PLAN 4.1). Splitting a model into N+1
printable pieces with nested `partition()` costs `O(2^N)` — so a 7-cut part renders the
underlying model 128 times, and the only way the old projects coped was freezing the model
to an STL and `import()`-ing it (the crutch SPEC 6.6 wants gone). This documents the
diagnosis, the measured numbers, and the fix.

## Diagnosis — nested `partition()` instantiates its child twice

BOSL2's `partition()` (`libs/BOSL2/partitions.scad:879`) cuts its child along a path and
lays out BOTH halves:

```openscad
move(vec)  { intersection() { children(); partition_mask(...); } }              // half A
move(-vec) { intersection() { children(); partition_mask(..., inverse=true); } } // half B
```

`children()` appears TWICE. That's fine for one cut. But the multi-cut idiom in the old
designs stacks the calls — and in OpenSCAD `mod_a() mod_b() target()` *nests*, so each
`slice_part()` becomes the child of the one above it:

```openscad
// window_light_blocker_slice.scad — 7 stacked slice_part() = 7 nested partition()
slice_part() right(d) slice_part() right(d) ... slice_part() import("...stl");
```

N nested `partition()` calls → the leaf is evaluated `2^N` times, and there are `2^k`
mask-intersections at level `k`. Total geometry work doubles with every cut you add.

## Evidence — measured on the real configs

`scripts/blowup_bench.sh` stacks the projects' actual `partition()` configs (jigsaw cutpath,
real cutsize/gap) to depth N around an inline leaf standing in for the frozen STL, and
times each render through `fab render` (Manifold backend). The ratio is each render over
the previous — the `2^N` signature is that ratio converging to **2.0** (each added cut
doubles the work):

```
window_light_blocker (cutsize=[20,15], gap=5)        shoe_holder (cutsize=[14,10], gap=20)
  N    leaves    secs   ratio                          N    leaves    secs   ratio
  0         1    0.18   -                               0         1    0.17   -
  3         8    0.36   1.24x                           3         8    0.34   1.42x
  5        32    1.01   1.71x                           5        32    0.90   1.70x
  6        64    1.82   1.80x                           6        64    1.67   1.86x
  7       128    3.49   1.92x                           7       128    3.10   1.86x
  8       256    6.89   1.97x                           8       256    6.08   1.96x
  9       512   13.61   1.98x                           9       512   12.08   1.99x
 10      1024   27.35   2.01x
```

Both projects converge to ~2.0x per cut. The sub-2.0 ratios at low N are just the ~0.18 s
fixed cost (OpenSCAD startup + Manifold init) diluting the exponential term until it
dominates. 27 s at N=10 for a part that renders in 0.18 s uncut — that's the tax.

The crutch follows directly: `window_light_blocker_slice.scad` carries 7 cuts = 128×, so
the model was frozen to `window_light_blocker_half_slice_v2.stl` and `import()`-ed to dodge
re-evaluating it. The blowup is *why* `import()` is in these files at all.

## The fix — linear slicing (PLAN 4.2)

Don't nest. Each piece is the source intersected with ONE slab (the region between cut
plane `i` and `i+1`):

```
piece_i = source ∩ slab(i, i+1)
```

N cuts → N+1 pieces → N+1 intersections, the source evaluated ONCE per piece. Linear, not
exponential:

```
cuts N      nested 2^N      linear N+1      speedup
   3              8              4            2.0x
   5             32              6            5.3x
   7            128              8           16.0x
  10           1024             11           93.1x
```

At the real 7-cut scale that's a 16x cut in evaluations, and it only diverges further. It
also makes the `import()` crutch unnecessary (SPEC 6.6): the live model is cheap enough to
re-render, so pieces can stay a function of source — no frozen intermediate to drift out of
sync.

`scad-lib/slicer.scad` implements this (4.2): `slice(cuts, axis, size, spread)` emits
`piece_i = children() ∩ slab_i`, the child evaluated once per piece. Re-running the
benchmark with it (same leaf, cuts along X) flattens the curve completely:

```
N (cuts)   pieces   linear slice()   nested 2^N (measured)
    0          1        0.27 s          0.17 s
    5          6        0.32 s          0.98 s
   10         11        0.33 s         26.8 s
   20         21        0.43 s          ~2^20 — intractable
```

Where nested doubles with every cut, the slab slicer holds flat — ~8 ms per added piece
against the ~0.27 s fixed render cost. N=20 (21 pieces) renders in 0.43 s; the nested form
can't get there at all. This replaces the nested-`slice_part()` idiom.

## Reproduce

```sh
bash scripts/blowup_bench.sh   # ~2 min; renders nested stacks to depth 10 and times them
```
