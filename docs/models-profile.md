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

## A real model, and the OpenSCAD wall-time (slice_parts, Q dogfood, 2026-07-08)

`models/wall_screen/slice_parts.scad` — remindwall's slicer, one of the nastier real models: NESTED
`partition()` with dovetail/sawtooth cutpaths. Profiled the same way (release `models_worker`, 4 kHz),
39 477 samples ≈ **9.9 s of pure eval**. Note what `models_worker` measures: it stops at the `Geo` TREE
(`resolve_geometry_file`) and never lowers to Manifold — so this is the INTERPRETER describing the geometry,
before a single boolean runs.

The shape is the N.1 story, louder:
- **55.3% allocation/memory-traffic** — MORE alloc-bound than the N.1 corpus model.
- Hot subtree: `lc_for` 64% → `bind_module_scope` 56% → `eval_comprehension` 55% → `check_assert` 41% →
  `hoist_scope` 38%. partition's path generation is comprehension-and-module-call heavy, wrapped in BOSL2's
  per-function defensive asserts.
- The concentrated, attackable allocation is the **per-frame `Scope` BTreeMap**: `Frame::drop` 3.6% +
  `push_call` 3.0% + BTreeMap `IntoIter::dying` 2.6% + `VacantEntry::insert` 2.2% + `Scope::bind` 2.0% +
  the rest of the BTreeMap/child/call_frame machinery ≈ **~15% of all allocation**, all of it in
  allocate-a-BTreeMap-per-call, bind params, drop it. This is what N.2d (Vec-frame Scope) targets, and this
  real model JUSTIFIES it (it was parked as "re-measure whether the residual scope cost earns it" — yes).

  **N.2d LANDED (2026-07-08):** the per-frame `vars` map is now adaptive — a flat `Vec` (linear scan) for the
  small per-call/`let` frame, spilling to a `BTreeMap` past 16 entries for the thousand-constant island
  globals. slice_parts eval **8925 → 8517 ms (~4.6%)**, corpus **901/901** (up from 899 — the ~4.6% cleared
  the borderline spheroid + gaussian_rands timeouts). Modest because the map STRUCTURE was only part of it:
  the residual per-`bind` cost is the `String` KEY allocation (`name.into()` on every bind), unchanged here —
  that's N.2b (intern var names), the next scope lever.

  **N.2b LANDED (2026-07-08):** the bind-source identifiers (`Parameter.name`, `Assignment.name`, `Arg.name`)
  are now `Rc<str>` in the AST — allocated ONCE at parse, so the per-call param bind, the `lc_for` loop-var
  bind (the 64% path), and the `let` bind clone a refcount instead of a fresh `String`. slice_parts eval
  **8517 → ~8210 ms (~3.6%)**, corpus 901/901, clippy clean. Cumulative N.2d+N.2b: **8925 → 8210 ≈ 8%** off
  the interpreter. No global intern table (per-decl `Rc<str>` suffices for the alloc win; lookup stays a
  content compare — ptr-eq would need full interning, deferred). All numbers RELEASE (`cargo build --release`,
  opt-level 3); the `line-tables-only` debuginfo the profiler adds is symbols-only, zero runtime effect
  (verified: clean-release == debuginfo-release within noise). What's LEFT after N.2 is the O/P tier — the
  `check_assert` 41% + the comprehension NumList construction (a JIT target), not more scope polishing.

Two hypotheses this profile KILLED (record them so they don't get re-proposed):
- **The eval-memo cache is not the lever here.** A/B `FAB_EVAL_CACHE=1`: 8925 ms → 8530 ms, **4.5%** — nowhere
  near its 82-92% redundancy CEILING. The cache memoizes function VALUES; slice_parts' cost is module
  INSTANTIATION (geometry, not a memoizable value) + comprehension iteration + raw allocation. The cache pays
  for the function-heavy BOSL2 corpus, NOT for geometry-generating models. Stays opt-in.
- **Value-clone-on-lookup is NOT a deep copy.** `Scope::lookup_opt` does `value.clone()`, but `Value::List`
  is `Rc<[Value]>` (and `NumList`/`Str` too), so clone is an O(1) refcount bump. The 55% allocation is the
  Scope frames + result-Vec construction in comprehensions, not value copies.

### The OpenSCAD wall-time (an aside, but the motivating one)

Both to STL, both release/native, **both on the Manifold kernel** — OpenSCAD 2026.06.12's DEFAULT backend is
already Manifold (confirmed: `--backend Manifold` = 8.95 s, identical to the default; `--backend CGAL` did NOT
finish in 2 min). So this IS apples-to-apples on the kernel. The eval/render SPLIT (measured directly:
OpenSCAD eval-only = `.csg` export, which flattens the CSG tree WITHOUT rendering — the analog of our
`models_worker`):

| | eval only | render (≈full−eval) | full → STL | STL size |
|---|---|---|---|---|
| OpenSCAD (Manifold) | **~5.7 s** (.csg) | ~3.2 s | ~8.9 s | 26.5 MB |
| scad-rs (Manifold) | **~8.2 s** | ~1.4 s | ~9.6 s | 3.0 MB |

The CORRECTION that matters (an earlier draft of this doc ASSUMED OpenSCAD's eval was ~1–2 s and its render
dominated — WRONG, never measured): OpenSCAD's tree-walker eval is ~5.7 s. It is ALSO slow on this model.
We're ~1.4× behind on EVAL (8.2 vs 5.7), and ~2× AHEAD on RENDER (1.4 vs 3.2). The interpreter gap is ~30%,
NOT 8×.

Why ~1.4× on a non-JIT'd tree-walker? Almost certainly VALUE-BUFFER REUSE, not anything JIT-shaped.
OpenSCAD's `VectorType` is C++ move-semantics + COW: `a + b` with `a` a refcount-1 temporary MUTATES `a`'s
buffer in place, zero alloc. Our `Value::NumList` is an immutable `Rc<[f64]>` — every vector arithmetic op
allocates a FRESH buffer. That's the 55%-allocation signature, and N.2b/N.2d did NOT touch it (they cut
scope/name allocation, not ARITHMETIC-RESULT allocation). The untouched lever: COW-mutate a `NumList` when
`Rc::get_mut` shows it's unique — mirroring OpenSCAD. A concrete N.2-family change, NOT the O/P JIT tier.
So: JIT is for BEATING a tree-walker (the endgame); matching OpenSCAD needs ~30% eval, reachable by buffer
reuse.

**N.2e VERIFIED (ceiling-first, 2026-07-08) — and it FALSIFIED the buffer-reuse-closes-the-gap theory for
slice_parts.** Built the reuse (`zip_reuse`/`map_reuse` in `ops.rs`: COW-mutate a refcount-1 `Rc<[f64]>` in
place). A/B via `git stash`:
- slice_parts (our target): **~0%** (8210 → 8220, noise).
- a vector-arithmetic-heavy synthetic (`[for(i=…) a + b*i - a]`): **204 → 182 ms, ~11%** — reuse fires + works.

So slice_parts' allocation is NOT vector arithmetic — it's comprehension RESULT-LIST building (`build_vector`
→ `Rc<[Value]>`) + scope. A result list is genuine OUTPUT; you can't reuse a buffer for a list you're
constructing fresh. The slice_parts eval gap to OpenSCAD is interpreter per-element overhead on
comprehension/list-building + `check_assert` (41%), which is the O/P INTRINSIC/JIT domain, NOT allocation
micro-opts. N.2e was KEPT anyway (bit-identical, corpus 901/901, do-no-harm, a real ~11% for the
matrix/transform/point-heavy BOSL2 code that's common elsewhere) — but it is explicitly NOT the slice_parts
lever. This is the empirical close of the N.2 allocation tier: the remaining eval cost is interpretation, not
allocation.

CONFOUND, still honest: the engines emit ~8× different mesh complexity (26.5 vs 3.0 MB), so if OpenSCAD's
finer tessellation is generated during EVAL (not just render), it produces more per second than 1.4×
suggests. The `.csg` is 12.4 MB (a big tree) — a tree-size comparison to our Geo tree is the follow-up to
make the eval race fully clean.

## Redundancy — would an eval-memo cache pay? (measured 2026-07-08)

N.1 says WHERE the time goes (allocation). This asks a different question: how much of that work is
REDUNDANT — the same function evaluated with the same inputs over and over, which a content-addressed cache
(J.5 / P.2) would skip entirely. The `FAB_REDUNDANCY=1` probe (`redundancy.rs`) keys every user-function call
two ways and counts repeats — the theoretical hit-rate CEILING a perfect cache could reach:

- `(fn, args)` — ignores the reaching `$`-context, so it MERGES keys a correct cache would keep apart → a
  strict UPPER bound (a correct cache can only do worse).
- `(fn, args, ALL reaching $-vars)` — BOSL2 sets ~42 `$`-vars and a loop `$idx` changes per iteration, so this
  OVER-specifies (a real cache keys on only the `$`-vars a fn actually READS) → a LOWER bound.

The true ceiling is bracketed between them. Across five real slow models:

| model | fn calls | redundancy (lower..upper) | avg key size | top-10 keys absorb |
|---|---|---|---|---|
| corner_brace | 149 K | **92.5 .. 96.8%** | 22 elems | 51% of calls |
| garage_door | 3.36 M | **89.2 .. 99.2%** | 11 elems | 61% |
| under_sink_guide | 157 K | **84.1 .. 90.1%** | 936 elems | 25% |
| pill_holder_smaller | 3.03 M | **90.4 .. 92.2%** | 53 elems | 43% |
| keyboard_tent | 774 K | **92.6 .. 97.6%** | 48 elems | 39% |

**Every model is ≥84% redundant even on the pessimistic bound.** BOSL2's call graph re-derives the same
sub-results constantly (defensive re-validation + shared helpers down deep call chains), so a memo cache could
eliminate the large majority of function-call evaluation — and the 57% allocation that rides on it. And the
CONCENTRATION is extreme: 10 keys absorb 25–61% of MILLIONS of calls, so a small bounded (LRU) cache captures
most of the win — you don't need to remember everything.

This is a BIGGER lever than making each call cheaper (N.2b's ~19% scope): N.2b shaves the cost of a call, the
cache DELETES 84%+ of the calls. They compose — the residual misses still want a cheap scope, and interned
names (N.2b) make the cache KEY cheap to hash — but the cache is the headline.

### The correctness fence (why it's not free)

The measurement counts ALL repeats; a CORRECT cache can only memoize a call whose result is a pure function of
its key. Three fences:
- **Impure subtrees bypass.** `rands()` advances a stream and `echo`/`assert(msg)` emit ordered side effects —
  a call whose subtree touches those can't be served from cache (it'd freeze the RNG / drop the echo). In these
  models that's a small discount (`rands` barely registered in the N.1 profile), but the cache MUST detect and
  skip it, not memoize blindly.
- **The key must be COMPLETE.** Args + every `$`-var the body transitively reads. Miss one and you serve a
  stale result for a different context — silent geometry corruption, the worst failure. The gap between the
  84% lower and 96% upper bound IS this: precise read-set tracking closes it; the safe fallback (all reaching
  `$`-vars) sits at the lower bound and is still ≥84%.
- **Big keys.** `under_sink_guide` averages 936 Value-elements/key (path/region args) — hashing that per
  lookup isn't free, so the cache should either cap key size or accept that huge-arg calls pay their hash. The
  work behind those keys is heavy though (list math), so it likely still nets out ahead.

Reproduce with `FAB_REDUNDANCY=1 target/release/models_worker <model.scad> libs scad-lib`.

## Per-function call profile — aiming the intrinsics (O.2, 2026-07-08)

The redundancy probe answers "how much repeats" but keys on the body POINTER, so it can't name WHICH functions
to make cheap. The N.1 sampler can't either — every BOSL2 function evaluates through the SAME eval loop, so
`samply` sees `lc_for`/`check_assert`/`bind_module_scope` (our Rust symbols) and CANNOT separate `is_vector`
from `is_path`. So O.2 needed its own instrument: `FAB_PROFILE_FNS=1` (`fnprofile.rs`), a per-NAME call counter
hooked at dispatch — user functions, builtins, modules, counted separately. On `slice_parts` (2.92 M
user-function calls):

| rank | user function | calls | % of fn calls |
|---|---|---|---|
| 1 | **is_finite** | 1.01 M | **34.6%** |
| 2 | **is_nan** | 621 K | **21.3%** |
| 3 | last | 280 K | 9.6% |
| 4 | is_vector | 187 K | 6.4% |
| 5 | default | 74 K | 2.5% |

Two functions are **56% of every user-function call in the model**, and they're the workhorses of BOSL2's input
validation — every `assert(is_finite(…))` / `is_vector(…)` guard bottoms out here. Under them the builtins tell
the same story: `is_num` 1.12 M, `len` 889 K, `is_list` 816 K, `is_undef` 795 K — the type-predicate soup the
N.1 profile predicted, now named. And it ties back to `check_assert` being 41% of eval self-time: asserts
evaluate their condition through a nested eval loop, so that 41% IS these predicates.

### Why intrinsics, and NOT the cache/tagging the redundancy section teased

`slice_parts` measures **96.8 .. 99.8% redundant** — the highest ceiling in the table — yet the N.2c eval-memo
cache delivered ~0% here. That looks like a contradiction until you read the avg key size: **12.4 Value-elems
hashed per call**. To SERVE `is_nan(x)` from a cache you hash the key (~12 elems), probe a map, clone the
result — and `is_nan` is ONE comparison. The memo bookkeeping COSTS MORE THAN THE COMPUTE. That's the general
law for cheap leaf predicates: memoization (a cache, or tagging the value with "already validated") only pays
when the work behind the key dwarfs the key's hash, and a type predicate is the opposite extreme. The intrinsic
sidesteps it — it makes each call cheap with ZERO lookup, no key, no eviction, no `Rc`-aliasing fence.

### The A/B (ceiling-first, same machine, min of 5)

`is_nan` = `f64::is_nan` on a number (non-numbers route through the real `!=` so `[nan]!=[nan]` stays TRUE —
the element-wise corner a naive scalar check gets wrong); `is_finite` = `f64::is_finite`, which ALSO erases its
`is_num`/`is_nan`/`0*x` sub-calls (the interpreted body dispatches them; the native body doesn't). Both WIRE
against the shipped BOSL2 (confirmed via `FAB_EXPLAIN`), both proven bit-identical to interpreting their
verbatim reference — `is_finite` through the new dependency-aware oracle (interpret the reference WITH `is_nan`
defined). Corpus 901/901 unchanged.

| build | slice_parts eval (min of 5) | Δ |
|---|---|---|
| baseline (`is_def`/`is_str` only) | 8253 ms | — |
| + `is_nan` + `is_finite` | 7216 ms | −12.6% |
| + `last` + `default` | 6740 ms | −5.7% (incremental) |
| + `_is_liststr` + `point3d` | 6559 ms | −4.7% (incremental) |

**Cumulative −1694 ms, ~20.5%** off the pre-O.2 baseline, from six hand-written intrinsics. Each is proven
bit-identical to interpreting its verbatim reference (`bit_eq` compares `f64` by `to_bits` so a returned `NaN`
matches a `NaN` — `==` gets that backwards — and `±0` stay distinct) and WIREs against the shipped BOSL2.

`point3d` is the first intrinsic with an inline `assert()`, which forced (and got) the mechanism's next piece:
the `Intrinsic` ABI widened from `fn(&[Value]) -> Value` to a fallible `fn(&[Value]) -> Result<Value>`, so a
native impl RAISES exactly where the interpreted body's assert would (the harness matches on "both errored" —
the assert message is a diagnostic locator, not output). This is load-bearing for the rest of the profile:
`point3d`, `reverse`, `is_vector`, `is_matrix` — nearly every BOSL2 function validates inputs with an inline
assert. The dependency-aware harness also grew a fix — it now binds parameter DEFAULTS for unprovided args
(the real call path does), without which a short call like `point3d(p)` ran the oracle with `fill` unbound.

This confirms the intrinsic thesis over the memo thesis outright: same 99.8%-redundant workload, the cache got
~0%, the intrinsics got 20.5%. Next is `is_vector` (6.4%) — the hardest remaining single target: 5 params, a
comprehension, a `norm`/`_EPSILON` clause, and an `all_nonzero` sub-call that RECURSIVELY calls `is_vector`.
Below it the profile tapers into ≤2.2% functions (`_list_pattern`, `reverse`, `is_consistent`, `approx`,
`is_matrix`) — the point the per-call lever starts to stall and the JIT tier (P.1) earns its turn. Reproduce
the profile with `FAB_PROFILE_FNS=1 target/release/models_worker <model.scad> libs scad-lib`.

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

## O.4 — the eval-bound tail, per-NAME wall time (2026-07-16)

Post-BU/P.2 the perf sweep's remaining 30-second models are ≥85% EVALUATOR (window_air_cover: 36s of a 38s
wall). The existing deep-profile leg structurally couldn't see them — it drills into the top COMPLETERS of a
10s sweep, and the intrinsics tier's whole point is the models that blow 10s. Two additions close the gap:

- **`FAB_PROFILE_FNS` grew a per-user-fn CLOCK** (self + outermost-inclusive time, not just call counts). User
  fns can't be span-timed — bodies evaluate on the explicit task stack, no host recursion — so the dispatch
  site opens a window and a `Task::FnTimeReturn` (pushed like `TraceReturn`) closes it when the return value
  lands; strict LIFO makes a SHADOW STACK sound. SELF time subtracts timed user-fn callees ONLY — builtin
  sub-evals stay with the caller, which is exactly what a hand intrinsic erases, so the self column IS the
  reclaimable number. Outermost-inclusive is what deleting the function entirely would reclaim (recursion
  doesn't double-book).
- **`models_profile_targets`** — a targeted harness leg (`FAB_PROFILE_TARGETS=a.scad,b.scad`, default = the
  BU.7 eval-bound four) that deep-profiles NAMED models however slow they are, per-model tables.

Probe overhead ~1.7× on wall (window_air_cover 38→63s) — SHARES are the signal, the perf harness owns honest
walls. Sanity anchor: the 37.0s of booked self time ≈ the model's 36s un-probed eval wall.

### The cross-model worklist (four models, 83.2s total user-fn self)

| function | self (s) | calls | models | note |
|---|---|---|---|---|
| `_tri_class` | 12.4 | 3.9M | wac, pill | earcut CW/CCW classifier — cross+norm+sign, tiny body |
| `_region_region_intersections` | 9.7 | 6 | shoe | MONSTER body (comprehension loops) — deferred, JIT-tier shape |
| `is_vector` | 8.8 | 2.4M | all four | THE type predicate; the registry header's own "next step" |
| `approx` | 5.9 | 2.2M | all four | tolerance compare |
| `_bt_search` | 5.2 | 800k | shoe, webcam | recursive binary search |
| `_point_dist` | 4.9 | 1.8k | shoe | inside the region monster — deferred with it |
| `_none_inside` | 4.8 | 1.6M | wac | earcut ear test, recursive w/ early exit (deps: select, _tri_class, _pt_in_tri) |
| `is_consistent`+`_list_pattern`+`same_shape` | 4.7 | 1.3M | all four | the shape-check bundle |
| `_find_anchor` | 3.6 | 900 | webcam, pill | attachable anchor resolution — big body, deferred |
| `sum`+`_sum` | 3.1 | 830k | webcam, pill | recursive accumulate |
| `_apply` | 2.2 | 145k | all four | affine matrix × points |
| `_group_sort_by_index` | 2.0 | 20k | webcam | sorting machinery — deferred |
| `rot` | 1.4 | 132k | wac | big dispatch body — deferred |
| `vector_angle` | 1.2 | 176k | wac, webcam | |
| `unit` | 1.1 | 500k | wac, webcam, pill | assert + v/norm(v) |
| `is_matrix` | 0.9 | 417k | wac, pill | |
| `posmod` | 0.9 | 710k | wac, shoe, pill | (x%m+m)%m + assert |
| `idx` | 0.8 | 355k | wac, shoe, pill | range builder |
| `in_list` | 0.6 | 180k | webcam, pill | |
| `constrain`, `force_list`, `num_defined` | 0.9 | 900k | mixed | trivial leaves |

The intrinsic-able bands sum to ~53s of the 83s; the four deferred monsters (`_region_region_intersections`,
`_point_dist`, `_find_anchor`, `_group_sort_by_index`, plus `rot`) hold ~22s and are the NEXT cut — their
bodies are big enough that hand-transliteration stops being obviously-correct, which is the JIT tier's case
(P.1.6 list ABI) or a second, carefully-harnessed intrinsic pass.

Confirmations along the way: the O.2 tier FIRES on these models (17.9M intrinsic dispatches across the four —
`is_finite` + `select` dominate, no DRIFT), and the builtin tables show what the worklist bottoms out in
(window_air_cover: 43M builtin calls — `norm` 10.3M, `is_undef` 6.1M, `len` 5.7M, `cross` 4.2M) — per-call
eval-loop dispatch that a native body erases wholesale.

### The `_EPSILON` gate — new mechanism the band needs first

Nearly every target defaults `eps=_EPSILON` — an IDENT default. The fingerprint proves the FUNCTION source
matches, but not that `_EPSILON` still evaluates to 1e-9 in the fn's home scope (a user override would make a
hardcoded constant silently wrong — doctrine violation). So `Entry` grows a wire-time const guard: named
constants + expected bits, checked against the home scope at `build_intrinsics`; mismatch → the entry doesn't
wire (worst case stays "missed speedup, never a wrong answer").

## O.5 — the first intrinsics campaign, closed out (2026-07-16)

Four bands landed off the O.4 worklist, all fingerprint-gated + guard-checked (O.5.1 const guards for the
`_EPSILON` bakes, O.5.2 dep pins + builtin-shadow checks — the gate extended one hop), every native proven
bit-identical by a fast==slow battery and WIRED/ARMED against the vendored BOSL2 under `FAB_EXPLAIN`:

- **O.5.2 shape/predicate** (11): `_list_pattern`, `same_shape`, `is_consistent`, `num_defined`,
  `force_list`, `approx`, `posmod`, `idx`, `all_nonzero`, `is_vector`, `is_matrix`.
- **O.5.3 earcut** (3): `_tri_class`, `_is_at_left`, `_none_inside` — window_air_cover's core.
- **O.5.4 aggregate/affine** (7): `sum`, `_sum`, `unit`, `is_2d_transform`, `_apply`, `_bt_search`,
  `vector_angle`.

### The scoreboard (worker wall, kernel included)

| model | pre-O.5 | post-O.5 | Δ | interpreted user-fn self |
|---|---|---|---|---|
| window_air_cover | ~38s (36s eval) | **13.9s** | −63% | 37.0s → 6.8s |
| shoe_holder | 17.9s | **11.0s** | −39% | 23.0s → 17.4s |
| webcam_holder | 12.2s | **7.6s** | −38% | 15.7s → 8.9s |
| pill_holder | 8.0s | **4.0s** | −50% | 7.6s → 3.2s |

83.2s of interpreted user-fn self across the four → 36.3s. Corpus 901/901 throughout; all four golden lanes
(m7 + m6, serial + par) green — the intrinsics are A/B-invisible, as the doctrine demands.

### The residual worklist (the NEXT cut)

- **`_region_region_intersections` + `_point_dist` — 14.2s, shoe_holder's whole story now (54%+28% of its
  residual).** Six calls, comprehension-heavy monster body: hand-transliteration stops being
  obviously-correct at this size. This is the JIT tier's case (P.1.6 list ABI) or a second, very carefully
  harnessed intrinsic pass.
- **`_find_anchor` (3.8s, webcam+pill)** — same class, attachable anchor resolution.
- **`_group_sort_by_index` (2.0s)**, **`_vnf_centroid` (2.3s, webcam+pill)**, **`rot` (1.3s, wac)** — medium
  bodies, plausible band 5.
- **The NAMED-ARG gap**: `is_vector`/`unit` still burn ~1.2s interpreted in wac alone because BOSL2 calls
  them with named args (`zero=`, `error=`) and the v1 intrinsic ABI only routes all-positional calls. A
  named→positional rebind at dispatch (names are known at the call site) would extend every existing
  intrinsic to those calls — likely the cheapest s/loc in the residual.
- Deferred small fry: `in_list` (0.5s, needs the all-hits retry tail + search), `is_path`, `constrain`,
  `_get_ear` (0.6s self), `vector_axis`, `affine3d_rot_from_to`, `apply` (the wrapper over `_apply`).

### The K.1.2 sweep verdict (run-1784241178, baseline re-frozen)

Aggregate fab wall on the common set **124.0s → 94.9s** (oracle flat at 250.1s): the fab-vs-OpenSCAD ratio
moves **2.02× → 2.63×**, median per-model **2.69× → 3.34×**. 28 models improved ≥20% — the intrinsics lift
everything BOSL2-heavy, not just the four targets (traced_holder −71%, kirby_holder −59%, shower_holder
−57%, the whole pill_holder family −21..−41%). Zero regressions. Two new rescues: `silverwear.scad` and
`window_air_cover.scad` go fab-TIMEOUT → SOLID while the oracle still times out — they move to the
fab-renders-where-OpenSCAD-can't column (8→10).
