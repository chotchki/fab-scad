# JIT rung D — dynamic-length lists (sink-return)

Status: **piece 1 landed** (`P.1.6f`, seedless-rands stream-weave). This doc is the design +
open decisions for **piece 2** (the loop + dynamic sink) — the part that needs review before
it's built, because it introduces the JIT's first control flow AND a new `unsafe` seam.

Read alongside `docs/jit-recipe.md` (the float-discipline recipe) and `jit/src/lib.rs` (the
compiler). Rungs A–C are the *scalarize* half of `P.1.6` (fixed-shape vectors, no memory);
rung D is the *sink-return* half (runtime-length lists).

---

## What piece 2 needs that the JIT has never done

1. **Loop codegen** — every rung so far emits STRAIGHT-LINE IR. A comprehension is a loop:
   Cranelift blocks (header / body / exit), a back-edge, and a loop-carried induction variable
   passed as a block param. First control flow in `fab-jit`.
2. **A dynamic sink** — the result length isn't known until the loop runs, so it can't be a
   fixed `out` buffer (rung C's sink). See Decision 1.
3. **Range-iteration bit-identity** — the loop bound + per-element value must match the
   interpreter's `RangeIter` (`lang/src/eval/value.rs`) EXACTLY. It's INDEX-BASED:
   `len = floor((end-start)/step) + 1` (0 if `step==0` / non-finite / wrong-direction),
   capped at `RANGE_MAX = 1e7`; `v[i] = start + (i as f64)*step`. Replicable in IR
   (`fcvt_from_uint` + `fmul` + `fadd`, `floor` via the math helper, a saturating
   `fcvt_to_uint` for `len`) — fiddly but mechanical.

---

## The scoping surprise: gaussian_rands is a PIPELINE, not a map

L.2.7 + the memory frame `gaussian_rands` (the 300k comprehension) as THE rung-D target. Its
real body (`libs/BOSL2/math.scad:1149`, dim==1 path) is:

```openscad
nums  = rands(0, 1, dim*n*2)                                    // (a) DYNAMIC-count rands
rdata = [for (i = count(dim*n,0,2)) sqrt(-2*ln(nums[i])) * cos(360*nums[i+1])]  // (b) loop INDEXING nums
        add_scalar(sqrt(cov) * rdata, mean)                     // (c) dynamic-list scale + add
```

This is NOT a self-contained map `[for(i=range) f(i)]`. It's a chain of dynamic-list ops:
- **(a)** a runtime-length `rands` → a materialized dynamic list `nums` (needs piece 2's
  loop-draw + a materialized list VALUE, not just a return).
- **(b)** a comprehension that INDEXES that materialized list (`nums[i]`, `nums[i+1]`) → another
  dynamic list `rdata`.
- **(c)** `sqrt(cov)*rdata` (scalar × dynamic list) + `add_scalar(_, mean)` (elementwise) →
  more dynamic lists.
(The dim>1 path is matrix math — `cholesky`, `transpose`, `list_to_matrix` — OUT of scope; a
matrix declines.)

So gaussian_rands needs **dynamic lists as first-class JIT values** — materialized, indexable,
mappable, scalable — not the minimal "body-is-a-comprehension → sink return." That's the crux
decision below.

---

## Decision 1 — the sink ABI (the main review item)

A runtime-length result can't ride rung C's fixed `out` buffer. Two ways:

- **A. push-helper** — a new `extern "C" fn jit_vec_push(sink: *mut Vec<f64>, v: f64)`; the loop
  body calls it per produced element, the caller owns the `Vec` and reads it after. GENERALIZES
  to filters (`LcIf`) and splices (`LcEach`) — the length is discovered as you go. This is the
  crate's **third `unsafe` seam** (after the fn-ptr call + `jit_rand_next`) — the thing to
  review. Shape:
  ```rust
  // fab-jit — mirrors jit_rand_next's confined+documented seam
  extern "C" fn jit_vec_push(sink: *mut Vec<f64>, v: f64) {
      // SAFETY: caller passes a live, exclusively-borrowed *mut Vec<f64> for the loop's duration.
      unsafe { &mut *sink }.push(v);
  }
  ```
  ABI grows a 5th param `*mut Vec<f64>` (like `out`/`rand`); the dispatch owns the `Vec`, reads
  it into a `NumList` after.
- **B. count-then-fill** — compute `len` up front (a range's `range_len`), allocate exactly,
  fill in a second pass. No new helper, but CAN'T do filters (a filtered length isn't known up
  front) and re-walks the bound. Dead-ends on the general comprehension.

**Recommend A** (push-helper). It's the only one that reaches `LcIf`/`LcEach`, and it composes
with the dynamic-list-value model Decision 3 needs anyway.

- Comment: Agree on A, we're going to need more miri tests once we get through this

## Decision 2 — how far the Lowered model grows (the real scope lever)

Rung A–C's `Lowered` is `{Num, Bool, Vec(fixed)}`. Piece 2 adds a runtime-length list. TWO sizes:

- **2a — comprehension only at a SINK position** (minimal). A function whose BODY is a
  comprehension (or whose return is one) → emit the loop → push to the sink → return a
  `NumList`. NO new `Lowered` variant; a dynamic list can't be an operand to anything else
  (can't be indexed, scaled, re-mapped). Delivers: simple pure maps —
  `[for(i=[0:n]) f(i)]` (bezier/path/table sampling). Does NOT reach gaussian_rands.
- **2b — dynamic lists as first-class values** (`Lowered::DynList` — a heap handle). Can be
  bound to a `let`, indexed in a loop, scaled, mapped, pushed. REACHES gaussian_rands' pipeline
  (a) + (b) + (c). Much larger: every op (`index`, `*`, `+`, `len`, another `for`) needs a
  dynamic path, plus lifetime/ownership of the intermediate `Vec`s inside one compiled function.

**This is the decision.** 2a is a bounded, low-risk win on a DIFFERENT (unmeasured) population
than gaussian_rands. 2b is the gaussian_rands lift and is a real project. My lean: **land 2a
first** (proves the loop + sink + range-iteration bit-identity on a small surface, delivers the
pure-map population), then scope 2b as its own phase with data on how many BOSL2 functions the
dynamic-list-pipeline shape actually unlocks (it may be a short list dominated by gaussian_rands
+ a few siblings — worth knowing before the lift).

- Comment: I have a strong feeling we need 2b. BOSL2 loves to compose lists and I see 2a as another we're not getting anywhere

## Decision 3 — the element-cap warning (solvable, just needs a nod)

A range/comprehension that exceeds `RANGE_MAX` emits a WARNING (I.3.2) — a message-stream side
effect the JIT can't produce, so an over-cap comprehension would diverge on I.5's
string-equal-vs-oracle. Fix: the JIT computes `len` first and **DECLINES at `len >= RANGE_MAX`**
(before the loop) → the interpreter handles the pathological case, warning and all. A
sub-1e7-element comprehension emits no warning on either side → matches. Clean, but confirm
that's the only cap (I believe the range's `RANGE_MAX` is the sole one; the comprehension has no
separate result cap).

- Comment: I'm good with that IIF we also know that with the decisions above we may still end up past this.

**RESOLVED — and the premise was wrong, in our favor.** Two findings from the code:
1. **We don't emit the warning yet.** The over-cap warning is the DEFERRED half of I.5
   (`Message::Warning` channel exists but is empty; `range_iter` caps `len` silently, `lc_for_c`
   just `break`s at `RANGE_MAX`). So TODAY there is NO message-stream side effect to diverge on —
   the decline-at-cap is NOT a correctness gate right now. When I.5's warning lands, we revisit to
   match the interpreter's exact text + fire-condition (and THAT's when a JIT decline-at-cap
   becomes load-bearing).
2. **The cap is asymmetric — you're right that we sail past it.** A RANGE caps at `RANGE_MAX`
   (`range_iter`), but LIST iteration is UNCAPPED (`iter_values` returns every element) and neither
   caps nor warns. So a composed pipeline (`[for(x = big_list) …]`, `concat`, `each`) produces
   intermediate lists LONGER than `RANGE_MAX` freely — and the interpreter is fine with it.
So the JIT decline is a **memory/perf BUDGET**, not a `RANGE_MAX` tie: decline (→ interpret) when a
materialized dynamic list would exceed a working budget (a few M elements — an 80 MB `Vec<f64>` at
1e7). A real LOOP (not the scalarize rungs' UNROLL) handles large counts without IR bloat, so the
budget is about working memory + not out-computing the interpreter, and it composes with your
"lists get big" point: the budget guards intermediate lists, wherever they blow up in the chain.

---

## Bit-identity references (must match EXACTLY)

- **Range iteration** — `lang/src/eval/value.rs::range_iter`/`range_len`/`RangeIter::next`
  (index-based, the formula above). Route `floor` through the math helper (== interpreter's
  `.floor()`); the `len` saturating-cast needs care (huge/negative/NaN → the documented 0-or-cap).
- **`lc_for`** — `lang/src/eval/mod.rs::lc_for` (the bit-identity ref for the loop): it iterates
  `iter_values(&iterable)` in order, binding the loop var per iteration (`scope.child()`), and
  `out.extend` in index order. The JIT loop must produce elements in the SAME order (it does —
  a loop pushes in index order).
- **Seedless rands in the body** — already handled (piece 1): `jit_rand_next` advances the woven
  stream in the loop's iteration order, matching the interpreter's per-element draw order. So a
  comprehension body that DRAWS composes for free — but see (a) above: a DYNAMIC-count `rands`
  (gaussian_rands' `nums`) is itself a loop-draw into a sink, which is piece 2, not piece 1.

---

## Code anchors (where piece 2 slots into `jit/src/lib.rs`)

- **`define_one`** — the ABI signature (`ctx.func.signature.params`) grows a 5th `*mut Vec<f64>`
  sink param; a block param `sink_ptr` joins `params_ptr`/`raised_ptr`/`out_ptr`/`rand_ptr`.
- **The return match** in `define_one` (the `Lowered::{Num,Bool,Vec}` arms) grows a
  comprehension/dyn-list arm (2a: detect the body is a comprehension → emit the loop → push →
  return `Ret::DynVec`; the dispatch reads the `Vec` into a `NumList`).
- **`compile_expr`** — a new `ExprKind::LcFor` arm (single-binding, range-or-vector iterable) is
  the minimal 2a. `LcForC`/`LcEach`/`LcIf` + multi-binding decline (`kind_name` "comprehension"
  already names them). For 2b, a `Lowered::DynList` variant threads through `Index`/`Binary`/etc.
- **`CompiledFn`/`call_numeric`** — `Ret` grows a dyn-vector variant; the dispatch allocates the
  `Vec<f64>` sink, passes its pointer, reads it back (like rung C's `out`, but growable).
- **New helper** — `jit_vec_push` registered in `new_module` + declared in `declare_helpers`
  (Decision 1), the third unsafe seam.
- **`NumericJit::call_numeric`** — no signature change (the sink is internal to the JIT, like
  `out`); the dynamic `Vec` is allocated inside `call_numeric` and wrapped as `JitOutcome::Vec`.

---

## DECISIONS (chotchki, locked)

1. **Sink ABI → push-helper (A).** Plus a MIRI suite for the new unsafe seam(s) once the shape
   settles (the crate already splits miri-on-mock / ASAN-on-real; the JIT's `unsafe` seams —
   fn-ptr call, `jit_rand_next`, now `jit_vec_push` — want miri coverage of the pointer contracts).
2. **Model → 2b (first-class dynamic lists).** Rationale: BOSL2 composes lists everywhere; 2a
   (comprehension-only-at-a-sink) would be another rung-B — technically green, coverage-flat.
   Go straight to `Lowered::DynList` as a real operand.
3. **Cap → memory/perf BUDGET, not a `RANGE_MAX` tie** (see Decision 3 RESOLVED). No warning
   divergence today; revisit when I.5's warning lands.

## Execution roadmap — 2b as a rung sequence

Each rung is a bit-identical, differential-guarded commit (the P.1.6a–f cadence), building toward
gaussian_rands (dim==1). `Lowered::DynList` = a heap-`Vec<f64>` handle materialized by a loop into
the push-helper sink; the budget guards its size.

- **2b.1 — the machinery** (biggest NEW risk: first control flow). `jit_vec_push` seam + the ABI
  sink param + Cranelift loop codegen (header/body/exit blocks, induction var as a block param) +
  range-iteration bit-identity (`range_len`/`range_iter` in IR, `floor` via the math helper,
  saturating `len` cast) + the budget decline. Deliverable: `[for (i = range) pure_scalar_body]`
  as a function body → a materialized `DynList` return, differentialed vs `lc_for`.
- **2b.2 — DynList as an operand.** `Lowered::DynList` threads through `Index` (a DYNAMIC index
  `dyn[i]` — a bounds-checked load, `undef`-on-miss matching the interpreter → since undef can't be
  represented, an out-of-range index is where a decline or a guard lands), `len(dyn)`, and
  iterating a DynList (`[for (x = dyn) …]`). This is gaussian_rands' `nums[i]`/`nums[i+1]`.
- **2b.3 — dynamic-count rands → DynList.** `rands(min,max,COUNT)` with a runtime count = a
  loop-draw (piece 1's `jit_rand_next` inside 2b.1's loop) into the sink. gaussian_rands' `nums`.
- **2b.4 — dynamic-list arithmetic.** scalar×DynList, DynList±DynList, `add_scalar` — each a loop
  (or a map) into a fresh sink, matching `ops`/`map_reuse` order. gaussian_rands' `sqrt(cov)*rdata`
  + `add_scalar(_,mean)`.
- **2b.5 — gaussian_rands (dim==1) end-to-end** differential over the corpus; the dim>1 matrix path
  stays declined (matrix ops out of scope).
- **2b.N — filters/splices** (`LcIf`/`LcEach`), C-style (`LcForC`), nested/multi-binding `for` —
  the push-helper makes these incremental; add as the corpus demands.
- **miri pass** — the crate's miri lane exercises the pointer contracts of all three seams (per
  Decision 1) once 2b.1–2b.3 land the new ones.

Start at **2b.1** (the machinery is the load-bearing risk; everything else is operand plumbing on
top of it).

## 2c — the matrix path (FIXED nesting)

A separate branch from 2b's dynamic lists: nested vectors whose shape is COMPILE-TIME known — a
`[[a,b],[c,d]]` literal, a matrix constructor, a matrix ARG. The surprise on opening it up: it was
already half-supported. `Lowered::Vec` nests for free (it holds `Vec<Lowered>`, and a `Lowered` can
be a `Vec`), the `Vector` literal arm builds the nested value, the `Index` arm returns `elems.nth(i)`
which IS the nested row, and `len(fixed vec)` is the row count (a `ConstNum` since 2b.4). Only three
seams DECLINED on nesting: the RETURN flatten, the ARITHMETIC, and the ARG side. So 2c is a short
grind, not a rewrite.

- **2c.1 — nested (matrix) RETURNS** (done, P.1.6m). The return flatten recurses: leaves stored
  row-major into the sink, a `VShape` tree recording the nesting so the dispatch rebuilds the
  interpreter's nested `Value` (`Ret::Nested{shape,leaves}` + `JitOutcome::Nested(Value)`). Bit-
  identical BY CONSTRUCTION — `rebuild_nested` applies `build_vector`'s exact rule (all-`Num`
  children → `NumList`, else `List`) at each level. The corpus differential grew a `Nested` arm
  backed by `value_bits_eq` (a `to_bits`-strict, `NumList`/`List`-agnostic structural compare, so a
  `NaN` matrix must match bit-for-bit). Unblocked the matrix CONSTRUCTORS (rotation/affine mats from
  trig). `CompiledFn` went `Copy`→`Clone` to hold the `Rc<VShape>` (an O(1) refcount-bump clone).
  Coverage 73→82 scalar, 25→28 vec / 520→576 triples.
- **2c.2 — matrix ARGS + elementwise/scale arithmetic.** `ArgShape` becomes a TREE (`Vec(Vec<ArgShape>)`),
  `shape_and_flatten` + param-load recurse, and `vec_elementwise`/`vec_scale`/`vec_div` recurse into
  nested `Vec` elements (declining on element shape-mismatch). With 2c.1 returns + the existing index
  arm this is transpose/submatrix/reshape + `mat±mat` / `mat*scalar`. A total-leaf cap (`MAX_FLAT_ARG`)
  bounds the unroll.
- **2c.2b — matrix PRODUCT** (deferred). OpenSCAD `*` on `mat*mat` / `mat*vec` / `vec*mat` is LINEAR
  ALGEBRA, not elementwise — a sum-of-products whose REDUCTION ORDER must match `ops.rs` bit-for-bit.
  These already decline safely today (`vec_dot`'s `.num()` on a row `Err`s), so 2c.2 leaves the
  `(Mul,Vec,Vec)` routing alone; the product is its own careful rung.
- **2c.3 — DYNAMIC vec-of-vec** (deferred, the 2b/2c crossover). A comprehension/`rands` producing a
  runtime-length list of VECTORS needs a list-of-lists arena (2b's `JitArena` grows a nested tier).
  This is gaussian_rands' matrix branch — the last piece of that end-to-end.

## 2c.3 — the ConstUndef fold (len of a non-vector) — DONE

`len of a non-vector` was the top blocker after 2c.1 (364). In a SCALAR specialization we STATICALLY
know the arg is a scalar, so `len(scalar)` is a compile-time-known `undef` — `len(x) == N` folds to
`false` and PRUNES, exactly like the ConstBool/ConstNum folds. `Lowered::ConstUndef` (a compile-time
undef): `len(non-list)` → `ConstUndef`; `is_undef(ConstUndef)` → `true`, the other type-predicates →
`false`; comparisons fold to the interpreter's EXACT undef semantics (`ops.rs`: `==`/`!=` give a bool
via `Value::eq`, so `undef==undef` true and `undef==other` false; an ORDERED `<`/`>` is `undef`
because undef is non-orderable — NOT `false`, the one I'd have guessed wrong); `const_truthy` →
`Some(false)` (undef is falsy, so `undef ? … : …` prunes to the else); `!undef` folds to `true`. A
`ConstUndef` that must become a RUNTIME number (`len(x)+1`) or a RETURN value DECLINES (no undef in
`JitOutcome`) — it only folds in predicates/comparisons/ternary-conditions.

The honest result: `len of a non-vector` is GONE from the histogram (all 364 now compile past the
len), but net coverage is only +2 scalar / +1 vec (83→85 / 28→29) — because the histogram is
FIRST-blocker, so those 364 mostly redistribute to their NEXT blocker rather than fully compiling.
The fold IS the win (the ceiling's cleared); the reclaim is downstream-limited. New ceiling: `call`
(285) and — tellingly — `index of a non-vector` (146→226), which is the SAME undef story (`scalar[i]`
→ `undef` per `ops::index`). So the cheap follow-on is extending `ConstUndef` to Index (and `.x`/`.y`
member access on a non-vector), with ONE nuance the `len` case dodges: still COMPILE the index expr
for its eval-order side effects (a nested `rands` advances the stream) before folding the result to
`ConstUndef`, or the seedless-`rands` weave desyncs.

## Known determinism edge — bail-after-partial-draw (for the hardening pass)

A JIT that BAILS (`raised` → the dispatch's `None`) *after* it has already drawn some seedless
`rands` leaves the shared stream ADVANCED by those partial draws; the interpreter then re-runs the
function from that advanced state, so its draws are shifted. The RESULT is still bit-identical — a
bail is always an error/decline path (assert-fail, out-of-range index → `undef`, over-budget), and
`undef`/error propagates regardless of the random values — but a program that CONTINUES past that
error using later randoms could see shifted subsequent draws. It does NOT bite the real cases: the
budget bail is checked BEFORE any draw; gaussian_rands' indices are in-range and its asserts pass;
BOSL2's compiled functions don't draw-then-bail (the corpus differential confirms). The clean fix —
snapshot the `RandStream` before a DRAWING function's call and restore it on `None` (cheap because
only drawing functions, a compile-time-known flag, pay the ~2.5 KB clone) — is deferred to the
determinism-hardening pass alongside the miri work. Flagging it here so it isn't lost.
