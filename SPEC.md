# SPEC: scad-rs — the OpenSCAD language in Rust over Manifold

Round 2, 2026-07-04 (round 1 + chotchki's inline comments absorbed as decisions). `[OPEN]`
marks what's still unsettled. The workflow tool's spec moved to SPEC_workflow.md — it keeps
working and its backlog waits in PLAN.md.

## Mission

Execute the OpenSCAD language — REAL OpenSCAD, BOSL2-grade OpenSCAD — natively in the fab
stack: one Rust implementation feeding the Manifold kernel we already embed, running
identically on desktop and in a single wasm32-unknown-unknown module in the browser. Not a
new language, not a dialect: stock `.scad` in, the same mesh OpenSCAD produces out, fast
enough that the reactive GUI re-renders on keystroke.

Why this beats wrapping the official wasm (what we ship today):
- ONE module everywhere — no emscripten split (research verdict: winit killed emscripten in
  2019 with no plans back; the two-module seam is permanent otherwise), no 10.7 MB lazy fetch.
- The Safari cliff dies BY CONSTRUCTION — explicit-stack evaluator, recursion bounded by
  memory, not by whichever JS engine's frame budget you happen to run under.
- The customizer becomes an AST walk we own; the DAG render engine gets per-node progress,
  cancellation + caching because evaluation happens in OUR process.
- BOSL2 gets FAST (see strategy below) without forking a line of it.

## License (DECIDED)

fab-scad is **GPL-2.0-or-later — OpenSCAD's exact license, on purpose.** A reimplementation
derives its correctness from the OpenSCAD community's accumulated semantics, tests and docs;
extracting that value and licensing around it would be legal and wrong. Matching their
license byte-for-byte has two payoffs: anything we build can flow UPSTREAM with zero
relicensing friction if they find value in it, and we can port from `src/core` directly —
read the bison grammar, read the evaluator, translate truthfully — instead of clean-room
guessing. The ethics choice IS the engineering advantage. (`scad-lib` stays MIT; BOSL2 is
BSD-2, compatible.)

Nuance (researched 2026-07-04): Apache-2.0 deps (Manifold — the same one OpenSCAD links)
are GPLv2-incompatible but flow into GPLv3, so every DISTRIBUTED build operates under the
grant's v3 option. Grant = 2-or-later for upstream's sake; effective rules = v3. The README
says this out loud. This is also why OpenSCAD+Manifold is legal at all — the or-later
escape, not community respect.

## What the research established (2026-07-04, 8-agent verified)

- **No linkable OpenSCAD exists.** `OpenSCADLibInternal` is a test-only artifact (never
  installed, no public headers, issue #193 open since 2012, 64 src/core commits/yr, no
  stable release since 2021.01). Vendored-fork C++ binding = permanent maintenance debt.
- **OpenSCAD's own backend is now Manifold** — the same Manifold in `kernel::Solid`. The
  only thing we lack is the language front-end. That's the gap scad-rs fills; the geometry
  engine question is ALREADY ANSWERED in our tree.
- **Single-module via emscripten is dead** (winit/cpal). A Rust front-end on
  wasm32-unknown-unknown is the only unified-build path.

## Architecture sketch

```
.scad source ──parse──▶ AST ──eval──▶ CSG node tree ──lower──▶ kernel::Solid (Manifold)
                         │                │                       + Clipper2 (2D subsystem)
                  customizer walk   explicit stack,
                  (params = AST)    content-addressed cache
```

- **Parser: winnow (DECIDED).** Hand-written combinator parser on winnow (nom's successor;
  chotchki has deep nom history, winnow is where that lineage is going). The bison grammar
  in src/core is the CONFORMANCE REFERENCE, not the implementation strategy — we generate
  grammar-conformance tests from it rather than translating LALR mechanics. Winnow buys us
  error quality + streaming + a customizer-friendly lossless-enough AST (params with
  comments/annotations survive) — AND built-in observability: every named parser is wrapped
  in winnow's `trace()` combinator from the first one written (the `debug` feature prints
  the attempt/backtrack tree; without it, zero cost). Same compile-out-like-a-logger
  doctrine as the evaluator's tracing spans — parse decisions and evaluation each observable
  from their idiomatic tool, both free in release.
  **Winnow-NATIVE error discipline (decided) — lean on the nom heritage, don't reinvent it:**
  `ContextError` + `StrContext::Label`/`Expected` on EVERY named production (errors name the
  construct and what was expected, never bare "parse error"); `cut_err` at commit points
  (past `module ident (` there is no backtracking — the error points AT the problem, not at
  some outer alternative that was never viable); `Located` input so AST nodes + diagnostics
  carry spans natively. The only part we own: RENDERING — winnow's context stack + spans →
  caret-style terminal output, built once, and the same structured diagnostic feeds the
  GUI/customizer later. No bespoke error types, no external parser-error framework.
- **Evaluator:** the hard 90%. Tree-walker, EXPLICIT STACK (no host recursion — the Safari
  class of failure becomes structurally impossible), lexical+dynamic scoping exactly as
  OpenSCAD does it ($-variables are dynamically scoped; `children()` is late-bound), value
  model with FAST PATHS: contiguous `Vec<f64>` for numeric lists (BOSL2 is 90% numeric list
  math — OpenSCAD's boxed-variant Values are the reason BOSL2 is slow there), interned
  strings, ranges as lazy triples. Undef-propagation semantics preserved bug-for-bug where
  models depend on them. **Value representation (DECIDED): plain enum + fast-path variants,
  NaN-boxing REJECTED.** Grounds: SIMD lives in the `NumList(Vec<f64>)` fast path (present
  either way — LLVM auto-vectorizes contiguous f64, incl. wasm SIMD128); NaN-boxing gives
  ZERO SIMD upside (per-element tag checks kill vectorization, and NaN canonicalization
  taxes even pure-number loops), has no maintained production crate (the small ones predate
  strict provenance), and buys a mandatory Kani proof burden. The enum keeps exhaustive
  match, miri, Kani and the determinism doctrine all cheap. Ecosystem precedent agrees: Boa
  ships the enum; starlark-rust's tagged-pointer + frozen-arena design is the prior art
  worth READING (immutable values, like ours) without adopting its unsafe.
- **Caching is a first-class design input, not a retrofit.** OpenSCAD has visibly struggled
  to bolt a good cache onto the Manifold backend from outside. We design for it from node
  one: every CSG node gets a CONTENT HASH (subtree structure + resolved params + $-context
  that reaches it), evaluation is pure node-in/geometry-out, and the cache is
  hash → Manifold result — in-memory first, the on-disk tier drops in later because the keys
  are already content-addressed. This is the same discipline as fab's 6.2 incremental
  rebuild, pushed down to the language level. The DAG engine's per-node progress/cancel
  hangs off the same node identity.
- **Builtin geometry surface (deliberately small):** polyhedron, primitives, multmatrix,
  union/difference/intersection, hull, linear_extrude/rotate_extrude (2D via Clipper2 crate),
  offset, projection. `import()` = our existing STL/3MF readers. DEFERRED: text() (fonts —
  the whole freetype/harfbuzz/fontconfig tree), surface(), and minkowski — which Manifold
  lacks and OpenSCAD's own manifold backend still farms to CGAL. Deferred = BLOW UP AND
  COMPLAIN LOUDLY, never silently wrong; if corpus pressure ever demands minkowski, we do
  our own implementation over Manifold hulls (decided direction, unscheduled).
- **Module boundary:** scad-rs lives behind the SAME geomsg seam the workers use today — a
  `Render{source, params}` request. The official-wasm worker remains wired as a FALLBACK
  during cutover; per-model, whichever engine is trusted answers.

## The BOSL2 strategy (no divergence, ever)

BOSL2 stays byte-identical to upstream — usable in stock OpenSCAD at all times. Three rungs,
each independently shippable:

1. **Fast evaluator.** Contiguous numeric lists + explicit stack + no boxed-variant churn.
   Expected 10-50× on BOSL2's VNF-building workload before any cleverness. This alone likely
   makes keystroke-reactive rendering real for typical parts.
2. **Pin-verified intrinsics.** The runtime recognizes pinned-BOSL2 functions by name and
   swaps in native implementations. Two HARD constraints (decided):
   - An intrinsic is PURE RUST or a MANIFOLD CALL — nothing else. No new native deps ride in
     through the intrinsic door; the wasm build stays one clean module.
   - **Benchmark data is captured and KEPT for every corpus run** — per-call timings at
     BOSL2-function granularity. The point isn't just "what's hot": BOSL2 pays a permanent
     tax working within stock OpenSCAD's limits (no mutation, no real arrays, userland VNF
     math), so the WIN often lives at a HIGHER call level than the obvious leaf — e.g. one
     native `skin()`/`attachable` path can beat perfectly-optimized leaves it would have
     called. The timing corpus is what finds those grain boundaries; intrinsic selection is
     data-driven, not vibes-driven.
   The pin makes it SOUND: an intrinsic activates only after proving equivalence against the
   userland original over the differential harness, re-proven at every BOSL2 pin bump;
   failures fall back to interpretation. BOSL2 remains the source of truth — intrinsics are
   theorem-checked shortcuts, not forks.
3. **JIT — a real destination, not a vestige.** scad→wasm emission for hot monomorphic
   numeric functions (browser: emit bytes + WebAssembly.instantiate; native: cranelift).
   Still gated on measured need for SHIPPING it — but this rung is also deliberately a
   learning vehicle for the coming feophant work, so design notes and spikes here are
   first-class even before the profiler demands them.

## Oracle + corpus

The oracle is stock OpenSCAD running stock BOSL2 — natively via the CLI we've always wrapped
(CI installs it today), no custom build required. **Use OpenSCAD's deterministic/predictable
output mode in the harness** (sorted/reproducible export ordering) — it collapses a chunk of
the mesh-comparison problem at the source. (Exact flag + coverage to verify in G.3; the
float-jitter problem remains real regardless.)

Corpus, in escalating order:
1. OpenSCAD's own test suite (GPL, ours to use directly now).
2. BOSL2's test suite (tests/ in the pinned submodule).
3. Our models/ tree (~60 real projects — corner_brace.scad is the Safari-killer poster child).
4. Generated programs (differential fuzzing, below).

## Determinism doctrine (decided — imported from quicksight)

ANY randomness, anywhere, derives from a single seeded PRNG so the initial seed determines
it all — test reproduction is trivial by construction. Two layers, because Rust adds teeth:

- **Harness determinism:** the grammar-directed program generator, proptest strategies,
  corpus sampling, shuffling — all draw from ONE seed per run, logged in every CI artifact
  and failure report. The PRNG is ALGORITHM-PINNED (`ChaCha8Rng`), never `StdRng` — StdRng's
  algorithm may change between rand releases, which turns "reproducible" into "reproducible
  until cargo update". Failure = seed + generator version = exact replay. (proptest's
  persisted regressions and libFuzzer's crash artifacts already conform — the input IS the
  seed there.)
- **Engine determinism:** same source → BIT-IDENTICAL output, every run, every platform we
  ship. Concretely: no HashMap iteration order anywhere it can leak into output (echo order,
  tessellation, traversal) — BTreeMap/IndexMap at any order-visible surface; no
  time/address/thread-id dependence in evaluation. This isn't just hygiene: the
  content-addressed cache is UNSOUND without it (same key must mean same value), and the
  differential harness needs the engine side as reproducible as the generator side.
- **Float accumulation order is FIXED, everywhere (decided with the enum call):** strict
  IEEE means reduction order is semantics. Two rules: (1) fast path and slow path use the
  SAME fixed chunked accumulation order, so the "fast == slow bitwise" property holds by
  construction; (2) chunk width is a CONSTANT (4 lanes) regardless of hardware — wider SIMD
  processes fixed-width chunks, so native AVX, wasm SIMD128 and plain scalar all produce
  identical bits. Lane width is a throughput knob, never a semantics knob.

## Testing + verification

Layered, cheapest-first; each layer catches what the previous can't:

- **Lints:** clippy at -D warnings from day one (already house rule), and the lint set
  RATCHETS — we keep tightening (pedantic picks, unsafe_op_in_unsafe_fn, missing_docs on
  public surface) as the codebase matures. Loosening is a reviewed decision, never drift.
- **Unit tests:** table stakes, written with the code (house rule).
- **Interface tests at the Manifold boundary — soundness split (adjusted from the miri
  comment):** miri cannot execute foreign code, so it can't watch real FFI calls. The goal
  stands; the mechanism splits: (a) the geometry backend sits behind a trait; interface
  tests run against a PURE-RUST MOCK under MIRI, which checks all our unsafe/ownership
  handling up to the boundary; (b) the SAME interface tests re-run against real Manifold
  under AddressSanitizer/LeakSanitizer in a CI job, which is the tool class that actually
  sees across FFI. Between miri-on-mock and ASAN-on-real, the boundary is covered from both
  sides.
- **Differential testing (the workhorse).** Same source → scad-rs and oracle → compare.
  Mesh equality is subtle (triangulation order, float jitter — even with deterministic
  output mode). Candidate metrics, strictest-first: exact vertex-multiset match after
  canonical sort+quantize; volume + surface area + Euler characteristic within epsilon;
  boolean-difference residual volume ≈ 0 (`A−B` and `B−A` in Manifold — the honest "same
  solid" test, machinery we already own). Echo/console output compares EXACTLY (BOSL2
  asserts print — string equality is free fidelity signal).
  **The G.3 MVP is exactly this question made small (decided):** low-poly sphere first —
  what is the STRICTEST comparison that passes `sphere($fn=8)` against the oracle? Then
  scale $fn up and watch which tiers survive. The metric GATE is chosen empirically from
  that experiment, per model class (polyhedral = stricter tier, curved = residual tier).
- **Property-based (proptest).** Parser: print(parse(s)) roundtrips; parse never panics.
  Evaluator invariants: scope push/pop balance, explicit-stack depth == semantic depth,
  numeric-list fast path == boxed slow path on random inputs (the fast paths get tested
  AGAINST OUR OWN slow path — an internal differential).
- **Fuzzing — we live and die here (decided), so it's INFRASTRUCTURE, not a chore:**
  grammar-directed program generation feeding the differential harness (evaluator semantics
  bugs no hand-written test imagines), cargo-fuzz on the parser (bytes → no panic, no hang),
  corpora persisted + minimized in-repo, a scheduled CI fuzz job from the first parser
  commit, and a trophy log (every fuzzer-found bug becomes a named regression test).
- **Intrinsic equivalence protocol (rung 2's gate).** Per intrinsic: proptest inputs drawn
  from the function's real domain + every call site's argument shapes observed in corpus
  runs; equivalence vs the interpreted BOSL2 original at the CURRENT pin; CI re-runs the
  whole protocol on any pin bump; failure = intrinsic silently disabled + report, never
  wrong geometry. **The matrix is PUBLISHED with every test run (decided):** per-intrinsic
  status (active/disabled/why) + equivalence stats + the benchmark deltas, as a CI artifact
  from day one — trend line over time, not a point-in-time claim.
- **Proof-grade spots (Kani), scoped to LOW-LEVEL components (decided):** the explicit-stack
  machine's push/pop discipline (no underflow, no type confusion), value-representation
  invariants (mandatory if we NaN-box), range-iteration termination, the mesh-comparison
  quantizer. NOT whole-evaluator correctness — that's the differential net's job; a formal
  OpenSCAD semantics doesn't exist and writing one IS the reimplementation.
- **semantics/ — a SEGMENTED executable-spec corpus (decided):** every ported semantics
  decision (scoping corner, undef case, $fn resolution order) lands as a named test in its
  own `semantics/` tree, annotated with the oracle-observed behavior AND the src/core
  provenance it was translated from. The suite becomes the OpenSCAD language spec upstream
  never wrote — and the most upstreamable artifact this project can produce, in exactly the
  license they can take.

## Non-goals (round 1)

- text(), surface(), minkowski — deferred loudly, not refused; corpus determines urgency
  (minkowski direction if demanded: our own, over Manifold hulls).
- The preview/CSG-cache/OpenCSG rendering path — we render meshes, full stop.
- Language extensions. scad-rs runs OpenSCAD, it doesn't improve it. (Upstreamability cuts
  both ways: divergence would kill it.)

## Open questions (remaining)

1. ~~Mesh-equality metric~~ → RESOLVED as G.3's empirical MVP: strictest-passing tier on
   low-poly sphere, scaled up, gate chosen per model class.
2. ~~Value representation~~ → RESOLVED: enum + fast-path variants (NumList etc.).
   NaN-boxing rejected on SIMD (no upside, tag-check + canonicalization tax), crate reality
   (nothing maintained post-strict-provenance) and proof burden. Revisiting requires a
   profiler mandate AND the Kani value-repr proofs landing in the same PR — the door is
   open, the toll is posted.
3. ~~Proof scope~~ → RESOLVED: Kani on low-level components only (stack machine, value
   invariants, quantizer, range termination).
4. ~~semantics/ corpus~~ → RESOLVED: yes, segmented, provenance-annotated, from day one.
5. ~~Grammar strategy~~ → RESOLVED: winnow hand-parser; bison grammar as conformance
   reference generating tests.
6. ~~Crate location~~ → RESOLVED: `lang/` workspace sibling (geom/ pattern) — light
   kernel-only consumers, clean fuzz target.
7. Deterministic output mode: confirm the exact OpenSCAD flag + what it does/doesn't sort —
   NEEDS TESTING, owned by G.3's harness work.
8. ~~Timing capture~~ → RESOLVED: FULL TRACE via the `tracing` crate, treated exactly like
   logging — spans on the evaluator's function-call path (name = scad function), a custom
   aggregating layer turns spans into the per-call benchmark corpus, and release builds
   compile it out entirely (`release_max_level_off` / feature gate). Dual use for free:
   the same spans ARE the evaluator's structured debugging story.
