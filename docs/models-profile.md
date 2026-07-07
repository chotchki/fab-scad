# The `models/` tree profile — where the interpreter actually spends its time

Status: **first capture** (L.3), 2026-07-07. This is the profiling artifact the JIT/intrinsics tier (rung
2/3, phase L.4) gets cut FROM — the baseline trend line, taken before a single intrinsic exists. Regenerate
it any time with the harness below; the numbers are meant to move.

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

`MODELS_COMPARE=1` ran the 59 rendered models against the oracle (boolean-residual, genus, shape-class).
Baseline 2026-07-07: **8 match / 51 diverge.** That's the sobering counterpart to the profile — the BOSL2
corpus at 99.8% proved the LANGUAGE is right, but only 14% of real rendered parts MATCH OpenSCAD's mesh. The
corpus is assert-based unit tests; a real part stacks features the corpus never exercises together.

The 51, categorized:

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

### The other levers

- **Excluding dead experiments.** 9 of 51 live in `unused/` or are named `_test` / `_slice` / `second_approach`
  / `slop_test` — abandoned or intermediate, not target parts. The harness now drops the `unused/` dir (like
  `out/`); the rest are a model-cleanup call. That's the real win from a cleanup pass — NOT deleting `$preview`.
- **Verifying the 6 "solid vs rejected".** The oracle REJECTED where we rendered; if that's the oracle's render
  timeout firing on heavy parts (not a geometric disagreement), those aren't real divergences and shouldn't
  count against the baseline.
- **A fresh compare pass** post-modifier-fix re-baselines the number — every model whose ONLY divergence was
  `*`-block spurious geometry now agrees.
