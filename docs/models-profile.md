# The `models/` tree profile — where the interpreter actually spends its time

Status: **first capture** (L.3), 2026-07-07; **release re-profile with an unbiased sampler** added N.1,
2026-07-08 (the section directly below — it CORRECTS the tracing-era ms story and is the one to trust for
"where does the time go"). This is the profiling artifact the JIT/intrinsics tier (rung 2/3, phase L.4) gets
cut FROM — the baseline trend line, taken before a single intrinsic exists. Regenerate it any time; the
numbers are meant to move.

## Release sampling — the honest wall-time picture (N.1, 2026-07-08)

The L.3 numbers below came from the tracing layer, which counts calls faithfully but INFLATES per-builtin ms
(its own `Instant`+mutex per span dwarfs a 0.7 µs predicate — the doc's own caveat said so). N.1 answers the
ms question the right way: an EXTERNAL sampler (`samply` at 2 kHz) that never touches the code, on a RELEASE
build, symbolicated against the binary. It overturns two things.

**First correction — the interpreter is MUCH faster on release than the L.3 headline implied.** That headline
("28% of parts can't evaluate in 10 s", "`corner_brace` takes 7.4 s") was a DEBUG measurement, taken before
the L.2.7 scope fix landed. On release `corner_brace` evaluates in **311 ms** — a 24× gap, all build profile —
and isn't a slow model at all anymore. The honest release histogram (106 top-level models, 10 s budget):

| build | rendered | TIMEOUT (>10 s) | error |
|---|---|---|---|
| debug (L.3) | 59 (53%) | 31 (28%) | 21 (19%) |
| **release (N.1)** | **63 (59%)** | **19 (18%)** | **24 (23%)** |

So it's 18% of real parts that blow 10 s on release, not 28% — still the number rung 2/3 has to move, but a
smaller hill than the debug figure sold. (The +3 errors are the assert-passthrough fix reaching DEEPER into
models that used to time out, surfacing unvendored `.stl`/`.svg` assets — visibility, not regression.)

**Second correction, the big one — it is NOT builtin dispatch. It's ALLOCATION.** Sampling the three slowest
release COMPLETERS (`pill_holder_combined_tray` 9.5 s, `under_sink_guide` 6 s, `garage_door` 4.5 s — different
shapes, one comprehension-heavy, one boolean-heavy) gives a dead-consistent breakdown:

| self-time bucket | pill_holder | under_sink | garage_door |
|---|---|---|---|
| `libsystem_malloc` (allocator) | 34.3% | 32.6% | 32.0% |
| `libsystem_platform` (memmove/memset) | 15.9% | 17.5% | 16.6% |
| **all allocation / memory traffic** | **58%** | **57%** | **57%** |
| `builtins::apply` + `is_builtin` (DISPATCH) | 0.9% | ~0.8% | ~0.8% |

**~57% of the interpreter's wall-time is `malloc`/`free`/`memmove` — and builtin dispatch, the thing the tracing
profile fingered and the thing PLAN N.2 was written to fix, is under 1%.** The tracing layer wasn't lying, it
was answering a different question: `is_num` really IS called 2.4 M times, but the FUNCTION is a single enum-tag
match that costs nothing — the cost is the machinery every call drags with it (arg `Vec`s, `Value` clones,
per-call scope frames), and that machinery is allocation. Count ≠ cost. This is the whole reason N.1 is a
separate task from the L.3 tracing pass.

### Where the allocation comes from (charged to the nearest semantic caller)

| % of all samples | site | what allocates |
|---|---|---|
| ~26% | `eval_with_global` | the central dispatch loop: per-node arg `Vec`s, `Task` pushes, the `split_off` for builtin args, per-arg `scope.clone()` |
| ~4.7% | `Scope::lookup_opt` | (inlining slop — `Value::clone` is an `Rc` bump, no copy; the real cost here is the deep dynamic-`$`-chain WALK, below) |
| ~3.2% | `ValueList::drop` / `Value::drop` | tearing down list values |
| ~3.2% | `Frame::drop` | dropping a call/`let` scope frame — its two `BTreeMap`s go node-by-node |
| ~2.7% | `push_call` | building the per-call argument-source list |
| ~2.1% | `Scope::bind` + `BTreeMap` insert/`VacantEntry` | binding a param allocates a `BTreeMap` node (+ COWs the frame if shared) |

Roll the Scope machinery up — `lookup_opt` + `bind` + `Frame::drop` + the `BTreeMap` insert/iter/`dying_next`/
`child`/`call_frame` entries — and it's **~19% of total time in the scope data structure alone.** The Scope is
an `Rc<Frame>` chain where each frame carries two `BTreeMap<String, Value>` (lexical `vars` + dynamic
`specials`). Every call/`let`/comprehension allocates a frame; every `bind` allocates a String-keyed node;
every lookup walks the chain doing `BTreeMap::get` on `&str` keys — that's the **1.4% `memcmp`** sitting in the
self list, string-comparing `"$fn"` and friends. And the L.2.7 fix (dynamic-`$`-chain by reference) that killed
the per-call copy has a tail: the chain is now DEEP, so every `$fn`/`$fa` read BOSL2 does per circle/arc walks
it to the root — `under_sink_guide`'s 6.3% `lookup_opt` is that walk.

### The other thing the inclusive view shows — BOSL2's defensive-assert tax

`check_assert` is **~39% INCLUSIVE** on `pill_holder`. That's not a bug, it's BOSL2: every library function
re-validates its args (`assert(is_finite(x))`, `assert(is_vector(axis))`), and evaluating those condition
EXPRESSIONS is real work. One slice of it IS a bug though — `check_assert` pretty-prints the condition back to
source (`print_expr` → the `write_expr` at ~1.5% of allocation) to build the `[assert(…)]` failure locator, and
it does so on EVERY assert INCLUDING the ones that pass with tracing off, then throws the string away. Guarding
that behind `trace::on() || !passed` is free and removes ~1.5–2%.

### What this means for N.2 / the rung-2/3 plan

The lever the profile actually points at, in priority:

1. **Kill per-call allocation.** The arg `split_off` `Vec` (one heap alloc per builtin call), the per-arg
   `scope.clone()`, the `Task`/`values` stack growth in `eval_with_global`. This is the 26% site.
2. **Re-represent the Scope frame.** Two `BTreeMap<String, Value>` per frame is the ~19%. A frame usually holds
   a HANDFUL of bindings — a small `Vec<(key, Value)>` (linear scan beats `BTreeMap` at that size AND drops
   cheaper), and/or INTERNED variable names so keys are integers not Strings (kills the `memcmp` and the String
   allocation in `bind`, and ties straight into backlog I.1.4). Determinism is preserved either way — insertion
   order is deterministic, and interning is a deterministic table.
3. **The assert formatting freebie** above (~2%, one guard).

Builtin-dispatch fast-pathing — N.2 as originally written — is a <1% change and should be re-scoped or dropped.
`norm`/`cross` and the transcendentals remain a rung-2 intrinsic target for CORRECTNESS-parity speed, but they
are not why the interpreter is slow. Reproduce any of this with `bash scripts/profile-model.sh <model.scad>`.

### The caveat that survives

Function-level attribution is solid; the LINE-level (`file:line`) didn't resolve — `line-tables-only` debuginfo
plus aggressive inlining defeats `atos`'s line lookup, so the allocation "sites" are charged to the nearest
named function, not the exact source line. For pinning the precise `.clone()`/`.split_off()` to fix, a
`debug=2` build or a heaptrack-style allocation profiler is the follow-up. The MAGNITUDES (57% allocation,
<1% dispatch, ~19% scope) are inlining-independent and are the signal.

## The harness

`tests/models_harness.rs` (`#[ignore]`, run on demand) sweeps every TOP-LEVEL model under `models/` —
chotchki's real printed parts, not synthetic tests — and does two things:

- **PROFILE.** Each model evaluates in an isolated `models_worker` SUBPROCESS with a 10 s watchdog. The
  subprocess is the point: the interpreter is slow enough on heavy BOSL2 geometry that a big fraction of the
  tree blows the budget, and a subprocess gets KILLED on timeout (reclaiming the core) where an in-process
  thread would leak and thrash — an early in-process version accumulated 30+ leaked threads and crawled. The
  sweep is a 12-wide pool (independent processes parallelize freely), so ~10 min of serial timeout-waiting
  collapses to ~2.5 min. Then the N slowest COMPLETERS are re-run in-process under a tracing layer that times
  each builtin/module BY NAME — the per-function breakdown. We profile the slow completers, not the timeouts,
  because a killed process's spans never close, and the completers exercise the same hot paths anyway.

- **COMPARE** (opt-in, `MODELS_COMPARE=1`). Rendered models vs the OpenSCAD oracle, boolean-residual. Off by
  default — an oracle render per model is minutes over the tree — and a divergence is DATA, not a failure.

```
cargo test -p fab-scad --test models_harness -- --ignored --nocapture models_profile_and_compare
```

A "top-level model" is a `.scad` no other file `include`s/`use`s — that subtraction drops the libraries and
data files (measurements.scad, monitor.scad, the 712k-element `height_map_*` blobs) that aren't models at
all, leaving 111 of the 140 files.

## The headline — the interpreter can't finish half the tree

111 top-level models, 10 s budget each:

| outcome | count | share |
|---|---|---|
| rendered | 59 | 53% |
| **TIMEOUT (>10 s)** | **31** | **28%** |
| error (evaluator gap) | 21 | 19% |

Of the 59 that DO render, the timing histogram is bottom-heavy toward slow: 13 of them take 3–10 s, only 2
come in under 100 ms. These are not enormous parts — `corner_brace` (a bracket) takes 7.4 s to EVALUATE, and
that's before a single boolean is realized (the harness times the evaluator, not the geometry kernel: it
stops at the `Geo` tree). 28% of real parts can't be evaluated in ten seconds. That is the number rung 2/3
has to move, and it's why the bet needs the JIT tier at all — passing the BOSL2 corpus proved correctness;
this proves it isn't yet FAST enough to use.

## The finding — it's type predicates, not transcendental math

> **Superseded on the COST question by N.1 (top of file).** The call COUNTS below are right and still useful
> (`is_num` really is called 2.4 M times), but the implied conclusion "so predicate dispatch is the hot spot"
> does NOT survive the unbiased sampler: dispatch is <1% of wall-time, allocation is 57%. Read this section as
> "what the corpus calls a lot", not "where the time goes".

Here's the part that overturns the working assumption. I expected the intrinsic worklist to be trig and
`sqrt` — the classic "the interpreter is slow at math, hand-write the kernels" story. It is NOT. Deep-profiling
the 8 slowest completers, ranked by call count (the reliable signal — see the caveat):

| builtin | calls (8 models) | note |
|---|---|---|
| `is_num` | 2,381,950 | type predicate |
| `is_undef` | 2,210,666 | type predicate |
| `is_list` | 2,103,766 | type predicate |
| `len` | 1,788,996 | list length |
| `concat` | 412,700 | list build |
| `norm` | 439,068 | vector length |
| `cross` | 212,498 | vector cross |
| `is_string` | 274,786 | type predicate |
| `sin` / `cos` | ~145k each | the "obvious" targets |
| `sqrt` | 678 | ~nothing |
| `acos` | 8,134 | ~nothing |

Eight models make **~10 million type-predicate + `len` calls** and 678 `sqrt` calls. The transcendental math
I'd have intrinsified first is statistical noise. The cost is BOSL2's defensive-typing discipline — every
library function re-validates its arguments (`is_num(x)`, `is_list(path)`, `is_vector(axis)`, `is_undef(opt)`)
on every call — multiplied by how call-heavy that library is. The predicates themselves are trivial (inspect a
`Value`'s enum tag); the cost is paying the full builtin-dispatch path 10 million times to do it.

## What this means for L.4 (rung 2/3)

The lever is NOT a math-kernel library. It's, in rough priority:

1. **Builtin-call dispatch overhead.** ~10M calls to functions that do O(1) work means per-call setup — arg
   flattening into a `Vec`, name lookup, the dispatch match — dominates. Fast-pathing the unary predicates and
   `len`/`concat` to skip the general arg-marshalling path is the highest-leverage, lowest-risk change, and it
   needs no JIT.
2. **Call VOLUME.** BOSL2 re-validates the same arguments through call chains. A content-addressed value/CSG
   cache (J.5) or predicate memoization attacks the 10M count itself, not the per-call cost.
3. **The JIT proper (Cranelift).** Inlining these predicates into compiled function bodies so they never hit
   the dispatch path at all — the endgame, and now we know the first thing it must inline is `is_num`, not `sin`.

`norm`/`cross` (~650k vector-op calls) are the one genuinely math-flavored target, and they operate on list
`Value`s — an intrinsic there is a real win, but a distant third behind dispatch overhead.

### Caveat — trust the call counts, not the milliseconds

The absolute ms the profiler reports are INFLATED by the profiler: it wraps every builtin span in two
`Instant::now()` calls plus a map update, and for a 0.7 µs predicate that instrumentation is a large fraction
of the measured time — so the ms over-attribute to the highest-frequency builtins. The CALL COUNTS are
instrumentation-independent and are the signal. They point the same direction the ms do, only cleaner.

## The error worklist — 21 evaluator gaps

Clustered, these are cheap and real:

- **Unimplemented builtins** — `resize` (scale-to-bbox), `render` (the CGAL-force no-op we can passthrough).
  Small, mechanical.
- **`unknown module attachable` ×3** (all in `wall_screen/unused/`) — attachable IS a BOSL2 module, so these
  either don't `include <BOSL2/std.scad>` or hit a resolution edge. Investigate; may be one bug behind three.
- **Missing asset files** — `.stl`/`.3mf` imports whose meshes aren't checked into the repo (`Connector - Foot.stl`,
  `ChainLink.stl`, `shelf_nologo.3mf`). Harness/data issue, not an evaluator bug.
- **Real asserts** — `assert(false)`, `assert(is_finite(l))`, `assert(is_vector(axis))` — these fire in OUR
  evaluator; whether they'd fire in OpenSCAD too is a compare-leg question (a genuine divergence vs a faithful
  reproduction of the model's own guard).
- **`mb_resolve_crop`** — from the `machineblocks` external lib, not vendored. Out of scope.

## The compare leg — the correctness baseline

`MODELS_COMPARE=1` runs the rendered models against the oracle (boolean-residual, genus, shape-class). The
first baseline was sobering — the BOSL2 corpus at 99.8% proved the LANGUAGE is right, but only 14% of real
rendered parts MATCHED OpenSCAD's mesh. Two root-cause fixes (below) moved it hard:

| pass | render / timeout / error | match / diverge (of rendered) | match rate |
|---|---|---|---|
| baseline | 59 / 31 / 21 | 8 / 51 | 14% |
| + `* ! %` modifiers | 79 / 17 / 10 | 15 / 64 | 19% |
| + `assert`/`echo` passthrough | 54 / 29 / 23 | 35 / 19 | 65% |
| + revolved-VNF weld | 54 / 29 / 23 | **42 / 12** | **78%** |

Exact matches went **8 → 42** (5.25×), divergences **51 → 12** — three foundational fixes, all surfaced by the
harness, none visible in the 99.8%-passing BOSL2 corpus. The remaining 12: 4 small residuals (≤9e-2, likely
tessellation-phase, maybe acceptable), 5 genus mismatches (honeycomb 29-vs-60, Underdesk-laptop -17-vs-3, …),
3 "empty vs REJECTED" (the oracle itself errors — weak signal, may not be real divergences). Note the coverage
TRADE in the middle rows: fewer models
fully render (54 vs 79) because the assert fix makes `left`/`fwd`/etc. do their REAL geometry — which is
slower (more 10 s timeouts, the profile's whole point) and reaches DEEPER, exposing gaps the empty
short-circuit used to hide (missing `.stl`/`.3mf`/`.svg` assets not vendored, 2D SVG import [deferred],
`resize`/`render` [unimplemented]). That's visibility, not regression — the interpreter is now correct-but-slow
where it was fast-but-empty, which is exactly the state the JIT/intrinsics tier exists to fix.

The ORIGINAL 51 divergences, categorized (historical — most cleared by the two fixes):

| category | count | reading |
|---|---|---|
| topology (genus mismatch) | 20 | our mesh has different holes/handles — real geometry bugs, `$fn`-immune |
| volume (residual > 1e-3) | 13 | wrong volume; several residuals >1.0 = a wildly different shape, not tessellation |
| empty vs rejected | 7 | we produce empty, the oracle errors — both "failed", weak signal |
| solid vs rejected | 6 | WE render, the ORACLE rejects — likely oracle render-timeout on heavy parts; verify before trusting |
| empty vs solid | 5 | we produce NOTHING, the oracle produces the part — missing geometry (silent unimplemented → empty) |

### `$preview` is NOT the cause (checked, because it looks like it should be)

The obvious suspect: `$fn = $preview ? 15 : 100` (89 of 111 models use `$preview`). It's exonerated. scad-rs
leaves `$preview` undef, OpenSCAD's CLI render sets it `false` — and BOTH `undef ? 15 : 100` and
`false ? 15 : 100` take the `:100` branch, so `$fn` is 100 either way. Statistically: `$preview` appears in
80% of ALL top-level models and 84% of the diverged ones — the same rate, so it's not the discriminator. And
the 20 genus mismatches are topological, immune to `$fn` regardless. Deleting the line moves nothing.

### Root cause of the topology class: the `* ! %` modifiers were IGNORED (fixed 2026-07-07)

The first thread pulled — I'd guessed a rounding/tag-diff geometry bug — went somewhere better. Reducing
`corner_brace` (genus −42 vs 0) with the new `diff_repro` tool: its actual rendered block AGREES with the
oracle in isolation. The −42 came from the FOUR `*`-disabled sibling blocks (`*bottom_curved()`, `*table()`,
two `*diff()…`). `*` is OpenSCAD's DISABLE modifier — "render nothing" — and the reduction `cube(10);
*sphere(20);` diverged with residual 0.97: **scad-rs was rendering the `*`-disabled subtree as real geometry.**

The parser recorded `modifiers.disable` (and `.root`/`.background`/`.highlight`); `eval_stmt` never READ them.
So `*` (disable), `%` (background — a preview ghost F6-render/STL-export omits), and `!` (root — "render only
this") all rendered as if bare. `*` is the dominant way chotchki parks variants — `*alternate();` — so it hit
a large slice of the tree. The fix (`eval_stmt` honors the modifiers): `*`/`%` drop the subtree from output;
`!` diverts its geometry into a program-global root override so only `!`-tagged subtrees render (ancestors +
siblings discarded — semantics verified against the oracle: `translate([50,0,0]) !cube()` renders the cube at
the ORIGIN). `#` highlight is a preview no-op. Regression-tested in fab-lang `geometry_corpus` + oracle-tested
in fab-scad `differential`. corner_brace went genus −42 → residual 0.558 — the topology garbage is GONE; the
residual 0.558 is a SEPARATE, smaller geometry issue in its rendered block, the next thread.

### Root cause of a missing-geometry class: `assert`/`echo` dropped their child (fixed 2026-07-07)

The second thread, from `corner_brace`'s leftover residual 0.558. Reducing with `diff_repro` (now printing each
engine's volume + genus): the removal in a `diff()` wasn't happening — our volume was the FULL keep, OpenSCAD's
had the piece carved out. It narrowed to `left(5) cube(...)` rendering EMPTY in our engine while `up(5)` worked.
The definitions are identical BOSL2 transforms — except `left`/`right`/`fwd`/`back` guard their body with

```
assert(is_finite(x), "Invalid number")   // <- NO semicolon
translate([-x,0,0]) children();
```

and `up`/`down` put a semicolon after the `assert`. With no semicolon, `translate(…) children()` is the
assert's CHILD (OpenSCAD's `assert(cond) <statement>` guard form). Our `assert` arm ran the check but DROPPED
`mi.children` — so the geometry vanished. `assert`/`echo` are PASSTHROUGH modules; the fix renders their child
after the side effect. Because `left`/`right`/`fwd`/`back` are ubiquitous, this was the single biggest
missing-geometry (empty-vs-solid) cause. `corner_brace` now AGREES with the oracle outright. Regression-tested
in fab-lang `geometry_corpus` + fab-scad `differential` (incl. the bare BOSL2 `left(5) cube()` trigger).

### Root cause of the DOMINANT class: revolved VNFs weren't welded (fixed 2026-07-07)

The biggest of the three, from the post-fix compare (14 of the 19 remaining divergences). `dowels` rendered
empty; reduced to `cyl(chamfer=1)` → empty (plain `cyl` fine). BOSL2 builds a chamfered/rounded `cyl` — and
`teardrop`, and anything via `rotate_sweep` — as a VNF that it renders with `vnf_polyhedron`. Chased it down
with `echo_repro` (new — dumps our echo so intermediates diff against OpenSCAD's): our `sweep`/`rotate_sweep`
returns a VNF **bit-identical to the oracle's** (68 verts, 128 tris, same indices). So the value math is
right — the bug was in RENDERING it. Our `from_indexed` (the polyhedron/VNF → Manifold leaf) did NO vertex
weld, but a 360°-revolved VNF DUPLICATES its closure-seam ring (section N == section 0 as distinct indices,
bit-for-bit equal: `v[0]==v[64]`, 68 verts → 64 unique). Manifold reads the un-welded duplicate as an OPEN
seam (non-manifold) → the whole leaf drops to empty. OpenSCAD's `polyhedron()` welds; we didn't.

Fix: `from_indexed` welds exact-bit-coincident verts (dropping any tri that collapses to degenerate), like
`from_stl_bytes` already did for STL import. Exact bits, not a tolerance — a 3mf's shared topology has no
exact dups (no-op) and a boolean-RESULT mesh's NEAR-coincident seam verts differ in the low bits so they stay
distinct (J.2.7.1 preserved). `dowels`, `wire_holder2` (was missing all 12 bolt holes — the teardrop cutters
rendered empty so nothing subtracted), and the whole chamfer/rounding/teardrop/`rotate_sweep` class now AGREE.

### The other levers

- **Excluding dead experiments.** 9 of 51 live in `unused/` or are named `_test` / `_slice` / `second_approach`
  / `slop_test` — abandoned or intermediate, not target parts. The harness now drops the `unused/` dir (like
  `out/`); the rest are a model-cleanup call. That's the real win from a cleanup pass — NOT deleting `$preview`.
- **Verifying the 6 "solid vs rejected".** The oracle REJECTED where we rendered; if that's the oracle's render
  timeout firing on heavy parts (not a geometric disagreement), those aren't real divergences and shouldn't
  count against the baseline.
- **A fresh compare pass** post-modifier-fix re-baselines the number — every model whose ONLY divergence was
  `*`-block spurious geometry now agrees.
