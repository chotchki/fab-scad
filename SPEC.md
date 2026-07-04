# SPEC: scad-rs — the OpenSCAD language in Rust over Manifold

Round 1, 2026-07-04. Drafting live; `[OPEN]` marks questions we haven't settled. The workflow
tool's spec moved to SPEC_workflow.md — it keeps working and its backlog waits in PLAN.md.

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
                  (params = AST)    caching per node
```

- **Parser:** port of OpenSCAD's bison grammar (we're GPL — read it, translate it). Rust
  recursive-descent or LALR via grammar-faithful translation. Lossless-enough AST to power
  the customizer (param extraction with comments/annotations) and future tooling.
- **Evaluator:** the hard 90%. Tree-walker, EXPLICIT STACK (no host recursion — the Safari
  class of failure becomes structurally impossible), lexical+dynamic scoping exactly as
  OpenSCAD does it ($-variables are dynamically scoped; `children()` is late-bound), value
  model with FAST PATHS: contiguous `Vec<f64>` for numeric lists (BOSL2 is 90% numeric list
  math — OpenSCAD's boxed-variant Values are the reason BOSL2 is slow there), interned
  strings, ranges as lazy triples. Undef-propagation semantics preserved bug-for-bug where
  models depend on them. [OPEN] value NaN-boxing vs enum — measure before committing.
- **Builtin geometry surface (deliberately small):** polyhedron, primitives, multmatrix,
  union/difference/intersection, hull, linear_extrude/rotate_extrude (2D via Clipper2 crate),
  offset, projection. `import()` = our existing STL/3MF readers. DEFERRED: text() (fonts —
  the whole freetype/harfbuzz/fontconfig tree), minkowski (Manifold lacks it; OpenSCAD's
  manifold-backend minkowski still calls CGAL — rare in our corpus), surface().
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
   swaps in native Rust (vnf_vertex_array, affine ops, path math on Clipper2). NumPy-vs-pure-
   Python, for scad. The pin makes it SOUND: an intrinsic activates only after proving
   equivalence against the userland original over the differential harness, re-proven at
   every BOSL2 pin bump; failures fall back to interpretation. BOSL2 remains the source of
   truth — intrinsics are theorem-checked shortcuts, not forks.
3. **JIT, if rungs 1-2 leave anything on the table.** scad→wasm emission for hot monomorphic
   numeric functions (browser: emit bytes + WebAssembly.instantiate; native: cranelift).
   Gated on MEASURED need — rung 1's constant factor plus rung 2's algorithmic swaps may
   saturate. [OPEN] don't design this until the profiler says so.

## Oracle + corpus

The oracle is stock OpenSCAD running stock BOSL2 — natively via the CLI we've always wrapped
(CI installs it today), no custom build required. Corpus, in escalating order:
1. OpenSCAD's own test suite (GPL, ours to use directly now).
2. BOSL2's test suite (tests/ in the pinned submodule).
3. Our models/ tree (~60 real projects — corner_brace.scad is the Safari-killer poster child).
4. Generated programs (differential fuzzing, below).

## Testing + verification (the section to work through)

Layered, cheapest-first; each layer catches what the previous can't:

- **Differential testing (the workhorse).** Same source → scad-rs and oracle → compare.
  Comparison is the subtle part [OPEN — discuss]: meshes aren't byte-comparable (triangulation
  order, float jitter). Candidate metrics, strictest-first: exact vertex-multiset match after
  canonical sort+quantize; volume + surface area + Euler characteristic within epsilon;
  boolean-difference residual volume ≈ 0 (compute `A-B` and `B-A` in Manifold — the honest
  "same solid" test and we already own the machinery). Echo/console output compares EXACTLY
  (BOSL2 asserts print — string equality is free fidelity signal).
- **Property-based (proptest).** Parser: print(parse(s)) roundtrips; parse never panics.
  Evaluator invariants: scope push/pop balance, explicit-stack depth == semantic depth,
  numeric-list fast path == boxed slow path on random inputs (the fast paths get tested
  AGAINST OUR OWN slow path — an internal differential).
- **Differential fuzzing.** Generate well-typed-ish scad programs (grammar-directed
  generation, not byte fuzzing), run both engines, diff. This is where evaluator semantics
  bugs that no hand-written test imagines get caught. cargo-fuzz for the parser proper
  (bytes → no panic, no hang).
- **Intrinsic equivalence protocol (rung 2's gate).** Per intrinsic: proptest inputs drawn
  from the function's real domain + every call site's argument shapes observed in corpus
  runs; equivalence vs the interpreted BOSL2 original at the CURRENT pin; CI re-runs the
  whole protocol on any pin bump; failure = intrinsic silently disabled + report, never
  wrong geometry.
- **Proof-grade spots (Kani or similar), scoped tight [OPEN — how far do we want to go?].**
  Candidates where a proof buys real safety: value-representation invariants (if we NaN-box),
  the explicit-stack machine's push/pop discipline (no underflow/type confusion), range
  iteration termination, the quantizer used by mesh comparison. NOT candidates: whole-
  evaluator correctness (that's what the differential net is for — formal spec of OpenSCAD
  semantics doesn't exist and writing one IS the reimplementation).
- **[OPEN] executable semantics as documentation?** Every semantics decision we make
  (scoping corner, undef case, $fn resolution order) lands as a named test citing the
  oracle behavior + the src/core line it was ported from. The test suite BECOMES the
  OpenSCAD language spec that upstream never wrote. Decide: do we structure these as a
  separate `semantics/` corpus with provenance annotations from day one?

## Non-goals (round 1)

- text(), surface(), minkowski — deferred, not refused; corpus determines urgency.
- The preview/CSG-cache/OpenCSG rendering path — we render meshes, full stop.
- Language extensions. scad-rs runs OpenSCAD, it doesn't improve it. (Upstreamability cuts
  both ways: divergence would kill it.)

## Open questions (gathered)

1. Mesh-equality metric for the differential harness — which tier is the GATE?
2. Value representation: NaN-box vs enum-with-fast-Vec — benchmark first?
3. Proof scope: Kani on the stack machine + value invariants, or skip proofs round 1?
4. semantics/ corpus with src/core provenance annotations from day one?
5. Grammar: hand recursive-descent (better errors, customizer-friendly) vs LALR-faithful
   translation of the bison file (fidelity by construction)?
6. Where does scad-rs live: in-tree module (fab_scad::lang) vs sibling crate in the workspace?
