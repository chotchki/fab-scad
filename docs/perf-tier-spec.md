# The perf tier — SPEC (what + why, for alignment)

Status: **DRAFT for alignment**, 2026-07-07. Not a plan — the *shape* of the perf work, the contracts it must
hold, and the open questions to settle before we phase it. The PLAN (phases M–P) is cut from this.

> **Positioning.** fab-scad is a DERIVATIVE work of OpenSCAD (the language + the reference behavior we test
> against), BOSL2 (the library we run), and Manifold (the geometry kernel). All credit to those projects — this
> exists to add a high-performance, web-capable execution layer on top of their language + library, and the
> parser / differential harness / perf tier are here for anyone, upstream included, to take. Everything below
> is ADDITIVE, never a knock on the originals.

## 0. The situation — we're already at parity, so this is an ADDITIVE speed layer, not a fix

The measurement that reframes everything (release, full pipeline `fab render` vs OpenSCAD render, best-of-3):

| workload | fab | openscad | ratio |
|---|---|---|---|
| geometry models (corner_brace, Underdesk, flat_spring, buttons) | 519–1868 ms | 478–1833 ms | **1.02–1.09×** |
| metaballs / isosurface voxel field | 1.9 s / 11.9 s | 1.8 s / 11.8 s | **1.0–1.01×** |
| gaussian_rands 300k (pure numeric comprehension) | 247 ms | 187 ms | **1.32×** |

We are at parity with OpenSCAD — geometry AND the numeric voxel field. The "6.5× too slow / 28% of models
time out" story was a DEBUG-BUILD artifact (release is ~6.5× faster than debug across the board). The only
real gap is the extreme pure-numeric comprehension, and it's 1.32×, not 6×.

Why that changes the whole framing: OpenSCAD's evaluator is a straightforward tree-walker — a deliberate,
simple design, no bytecode or JIT (nothing wrong with that; simplicity is a virtue in a reference
implementation). Our Rust tree-walker matches it per-op. So:

- **Parity is the FLOOR, not the ceiling.** There's no deficit to erase first — an execution-optimization tier
  (dispatch fast-paths, intrinsics, Cranelift) is purely additive on top of an already-competitive baseline.
- **The architecture already welcomes it.** scad-rs was designed for a JIT from the start (the I.8 spike,
  interpreter↔JIT bit-identity already proven); adding one to a C++ tree-walker would be a large undertaking,
  so this is a natural fit HERE — and, like the rest, available upstream if it's ever wanted.
- **The web target makes it LOAD-BEARING.** In the browser there's no native process to lean on — the
  interpreter + wasm-safe intrinsics ARE the entire perf story. That's the bet's #1 differentiator (ONE
  implementation everywhere), so the intrinsics tier isn't optional there, it's the product.

So the perf tier is a deliberate, additive investment — we build it because the web needs it and because the
architecture is ready for it, NOT because anything is broken today.

## 1. The execution model — three tiers, one bit-identity chain

Established at Phase L (chotchki, 2026-07-05); restated because everything below serves it:

- **Interpreter** — the baseline, EVERYWHERE (native + wasm). Correct by construction (it IS the semantics).
  Slow is fine; it's the floor and the oracle every faster tier validates against.
- **Intrinsics** — hand-written Rust reimplementations of hot functions, wasm-SAFE, EVERYWHERE. The browser's
  whole perf story. Bit-identical to interpreting the function (fast == slow).
- **JIT** (Cranelift) — auto-compiles the numeric long tail, DESKTOP only (the browser can't JIT in-sandbox).
  Bit-identical to the interpreter (fast == JIT, proven at I.8).

The invariant that makes three tiers safe: **intrinsics == interpreter AND JIT == interpreter ⇒ web output ==
desktop output, ALWAYS.** A faster tier is pure speed, never a divergent mesh. This is non-negotiable and
gates every optimization below.

## 2. The resilience contract — "just gets slower, never wrong"

The crux, and the thing chotchki cares most about: a fast path must NEVER produce a different answer than the
interpreter, and when BOSL2 (or any library) updates a function we've intrinsified, the fast path must
DEGRADE GRACEFULLY — fall back to interpreting the new body, just slower. Never a silent wrong answer.

Two levels, DIFFERENT risk profiles — keep them separate:

### 2a. Our-builtin fast-paths (Phase N) — ZERO library-drift risk

`is_num`, `len`, `norm`, `concat`, … are OURS. They can't drift out from under us. Fast-pathing their
DISPATCH (skip the per-call arg-Vec allocation + the name→match, direct-call the hot unary predicates) is a
pure interpreter-internal optimization — always correct, no fingerprinting needed. This is the safe first
move and the immediate win against the gaussian_rands 1.32×.

### 2b. Library-function intrinsics (Phase O) — resilience by AST FINGERPRINT

Replacing a hot BOSL2 *function* (say a bezier evaluator, a path-math inner loop) with a Rust intrinsic is
where drift bites. The mechanism that makes it safe:

- **Key the intrinsic to a normalized AST FINGERPRINT of the target function**, not its name (the "match on
  original AST" note). At library-load time, fingerprint each user function's parsed AST; if it matches a
  registered intrinsic's fingerprint, INSTALL the fast path. No match → interpret.
- **A BOSL2 update that changes the body changes the AST changes the fingerprint ⇒ automatic, silent
  fallback** to the (slower) interpreter on the new body. A cosmetic reformat (whitespace, comments) does NOT
  change the AST, so the intrinsic SURVIVES it — exactly the property we want.
- **v1 fingerprint**: structural hash of the AST (operators, literals, call-names, control-flow shape) with
  local-var names included. A local RENAME breaks the match → falls back to slow → still correct. (v2:
  canonicalize local names — De Bruijn / positional — so a rename keeps the fast path. A refinement, not a
  correctness need.)
- **fast == slow, CI-enforced**: every intrinsic ships a test that runs the intrinsic AND the interpreted
  PINNED body over shared inputs and asserts BIT-IDENTICAL `Value`. The fingerprint guarantees we only ever
  substitute the intrinsic for that exact pinned body; the test guarantees the substitution is sound. Airtight:
  same body → fingerprint match → proven-equal intrinsic; changed body → no match → interpret.

The failure mode we're buying out: an intrinsic silently diverging after a `git submodule update` of BOSL2.
The fingerprint makes that IMPOSSIBLE — a changed function simply stops being intrinsified.

## 3. The foundation FIRST — heap-bounded eval (Phase M)

Do the last recursion removal BEFORE the fast-paths, while the interpreter is stable. What remains on the
HOST stack today (expression eval is already explicit-stack; Frame `Drop` is already iterative):

- **Deep `GeoNode` / `Shape2D` / `Value` tree `Drop`** — a recursive module builds a runtime-deep result tree
  (NOT bounded by the parser's `MAX_DEPTH`, which only caps source nesting); dropping it recurses. This is the
  exact thing that forced the 1 GiB-stack hacks in `models_worker` / `diff_repro` / the harnesses.
- **Geometry-tree eval assembly** (`eval_nodes` / `eval_stmt`) — assess whether it recurses at runtime past
  what the explicit-stack CALLS already bound, or if only `Drop` is the real exposure.

Why first, not later:
- **wasm-lethal.** Browser stacks are tiny; a stack-overflow class can't ship in the bet's #1 differentiator.
- **kills the 1 GiB hacks** — the harnesses stop reserving a gig per eval thread.
- **clean base for the fast-paths** — build them on the FINAL iterative structure, not rework them after.

Start with iterative `Drop` (contained, mirrors the Frame `Drop` already done), then measure whether the
eval-assembly needs the same treatment.

## 4. Caching — two tiers, keep them apart

- **CSG / geometry cache** (Phase P or parallel; the J.5 idea): key = hash(`subtree AST` + `resolved params` +
  `reaching $-context`) → realized mesh. Deterministic, clean, and the BIG interactive win — the GUI
  re-renders only changed subtrees, and BOSL2's repeated identical sub-geometry hits the cache. SHARES the
  AST+context hasher with §2b's fingerprint registry — build the hasher once, use it twice.
- **Value memoization** (pure-function results, keyed on args + reaching $-context): attacks the ~10M
  redundant predicate/`len` calls the models-profile flagged, DIRECTLY. But it has real subtleties —
  $-context sensitivity, `rands`/`$t` non-determinism must be excluded from the cacheable set. Trickier;
  DEFER behind the CSG cache and the dispatch fast-path (which may make it unnecessary).

## 5. Proposed phases (cut from the above — names/letters TBD with chotchki)

1. **Phase M — heap-bounded eval.** Iterative `Drop` for `GeoNode`/`Shape2D`/`Value`; assess + fix eval
   recursion. Exit: no host recursion in the geometry pipeline, harnesses drop the 1 GiB stack, a
   deep-recursive-module stress test passes on a default stack (and a small wasm stack).
2. **Phase N — interpreter fast-paths.** Fast-path builtin dispatch (unary predicates, `len`, `concat`);
   reduce per-call allocation. Exit: gaussian_rands closes toward 1.0×, corpus + differential green, no
   measurable regression on geometry models.
3. **Phase O — the intrinsics tier.** AST-fingerprint registry + auto-fallback + fast==slow harness; the
   first N hand-intrinsified BOSL2 functions, chosen from release-profile data. Exit: each intrinsic proven
   bit-identical, a BOSL2-update simulation (mutate a body) demonstrably falls back to the interpreter.
4. **Phase P — the Cranelift JIT + CSG cache.** JIT the numeric long tail (desktop), bit-identical to the
   interpreter; the content-addressed CSG cache. Exit: JIT==interpreter on the corpus, cache hit-rate
   counters, the GUI re-render path measured.

## 6. Open questions to settle before phasing

- **Fingerprint normalization depth** — exact AST (rename breaks → safe fallback) vs canonicalized locals
  (rename survives). v1 exact; is v2 worth it, or do BOSL2 functions rename rarely enough to not matter?
- **Where the intrinsic registry lives** — fab-lang (wasm-safe, so intrinsics ship to the browser) vs a
  native-only crate. Intrinsics must be wasm-safe ⇒ fab-lang, pure-Rust, no OS.
- **Profile on RELEASE, not debug** — the models-profile.md hot-list (predicates dominate) was DEBUG +
  tracing-inflated. Re-profile a representative slow model in release (a real sampling profiler) before
  picking the first intrinsics — the debug hot-list may not be the release hot-list.
- **Is the dispatch fast-path enough?** If Phase N closes gaussian_rands to ~1.05×, do we even NEED library
  intrinsics for parity — or is Phase O purely about EXCEEDING OpenSCAD (the moat) + the web?
- **CSG cache invalidation** — the $-context is part of the key; confirm every geometry-affecting $-var is in
  the reaching-context hash (miss one and the cache serves a stale mesh).
