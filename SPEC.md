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
                         │                │                       + CrossSection (2D, Manifold)
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
  some outer alternative that was never viable); `LocatingSlice` as the input stream — every
  production gets `.with_span()` byte ranges for free, so AST nodes + diagnostics carry
  spans natively. The only part we own: RENDERING — winnow's context stack + spans →
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
- **2D subsystem (DECIDED J.3.1) — Manifold `CrossSection`, and a strongly-typed 2D tree.**
  TWO decisions. First, the library: NOT a separate `clipper2` crate — the `manifold-csg`
  binding we ALREADY link ships `CrossSection` (2D booleans + `offset`), `extrude`/`revolve`
  (2D→3D), and `slice_to_cross_section` (projection, 3D→2D), and it bundles Clipper2
  INTERNALLY — the same library OpenSCAD 2021+ uses for its 2D. So the whole 2D surface (plus
  hull, already landed) rides one dependency, and 2D results align with the oracle by
  construction. Zero new geometry deps for the core. Second, the SHAPE: 2D and 3D are
  DIFFERENT TYPES (a region isn't a solid; mixing them is an OpenSCAD *warning*), so we encode
  dimension in the TYPE SYSTEM, not as a runtime property. A separate `Shape2D` tree runs
  parallel to `GeoNode`, mutually recursive across the dimension bridges — `GeoNode::Extrude`
  holds a `Shape2D` (2D→3D), `Shape2D::Projection` holds a `GeoNode` (3D→2D); eval yields a
  dimension-tagged `Geo { D2, D3 }`, each sub-tree homogeneous. The backend grows a SECOND
  associated type for the 2D handle (`CrossSection`), so no method body is ever
  dimension-polymorphic. Why typed over OpenSCAD's own dimension-mixed tree: the mixing
  warning forces eval to track dimension REGARDLESS, so the "one untyped tree" is a mirage —
  and worse, the warning would have to fire in the backend, which has no message channel. The
  typed tree makes well-formed input (all of BOSL2) impossible to mis-lower at COMPILE time,
  keeps the backend two clean types, and puts the mixing warning in eval where its source
  location belongs. Strong typing is testing-before-the-test. The eval-wire (J.3.2.1) realized
  this: primitives + transforms + booleans build `Shape2D` nodes, and the mixing rule is pinned
  BUG-FOR-BUG against OpenSCAD 2026.06.12 — the FIRST non-null child fixes a group's dimension
  (a present-but-empty `cube(0)` counts; only a truly-absent `{}`/never-run-`for` is neutral),
  each mismatched child is dropped with `Ignoring {n}D child object for {m}D operation`, and
  `Mixing 2D and 3D objects is not supported` fires once per operation. Tessellation parity for
  the extrudes is MEASURED against the oracle, not assumed — Manifold's `extrude`/`revolve` if
  the metric tolerates, our own loft if not. OUTCOME (J.3.4): un-twisted `linear_extrude` (prism +
  scale) passes the strict 1e-3 boolean-residual gate on Manifold's extrude. TWIST needed two fixes —
  Manifold spins the OPPOSITE way (we negate the sign) and OpenSCAD resamples the profile perimeter to
  `$fn` points before sweeping (each edge → `round(edge/perimeter·$fn)` segments, reproduced) — after
  which the SHAPE matches; a small per-slice tessellation-phase remainder (~1-2% at typical `$fn`,
  larger for curved/low-`$fn`) is an ACCEPTED, DOCUMENTED divergence behind a relaxed per-class residual
  tolerance (the exact slice-phase match is J.3.4.1, revisited only if it compounds). OUTCOME (J.3.5):
  `rotate_extrude` FULL revolutions pass the same strict gate — segment count (`$fn`, else `$fa`/`$fs`
  on the profile's max radius) + ring tessellation line up, no direction/phase quirk. PARTIAL angles
  (< 360) carry the twist's same small, converging arc-phase residual (0.2-2%), the same relaxed-
  tolerance treatment. OUTCOME (J.3.6): `projection` (the inverse bridge — `cut=false` shadow / `cut=true`
  z=0 slice) is EXACT: no swept tessellation, so shadow + cut cases (incl. a tilted-cylinder section) all
  pass the strict gate. OUTCOME (J.3.7): 16 real BOSL2 path/region-derived 2D shapes (attachable modules,
  path math, polygon, offset, region booleans) match the oracle strictly. Two fixes unlocked it: (1) the
  USE-SCOPE fix — a `use`d/`include`d function/module reads its own file's top-level constants (per-island
  constant scope), so BOSL2's function-form shapes stop asserting on `undef`; and (2) EVEN-ODD polygon
  fill — `polygon()` fills by nesting not winding (a BOSL2 path winds CW; `Positive` fill dropped it to
  empty). BOSL2 is loaded via `include <std.scad>` (attachable needs the file constants + `$`-context in
  the caller scope, which only `include` splices in).
- **Builtin geometry surface (deliberately small):** polyhedron, primitives, multmatrix,
  union/difference/intersection, hull, linear_extrude/rotate_extrude (2D via Manifold
  `CrossSection`, above), offset, projection. `import()` = our existing STL/3MF readers. DEFERRED: text() (fonts —
  the whole freetype/harfbuzz/fontconfig tree), surface(), and minkowski — which Manifold
  lacks and OpenSCAD's own manifold backend still farms to CGAL. Deferred = BLOW UP AND
  COMPLAIN LOUDLY, never silently wrong; if corpus pressure ever demands minkowski, we do
  our own implementation over Manifold hulls (decided direction, unscheduled).
- **Pure source-provider — fab-lang does ZERO IO, the caller fulfills a needs fixpoint (DECIDED,
  Phase M).** The language crate never touches the filesystem; every external source enters as
  DATA the caller supplies. Evaluation returns a `Resolution { Complete { geo, messages } |
  Incomplete { needs } }`, and a need is a `SourceNeed { Scad { from_dir, raw } | File { raw } }`
  — the impure caller reads the named sources, adds them to a table, and RE-RUNS until the graph
  closes. Two discovery phases, grounded in oracle tests (2026.06.12): `use`/`include` is STATIC
  (literal `<...>` tokens, top-level only — a variable path `use <x>` looks for a file literally
  named "x") so the loader closes it with a PARSE-TIME fixpoint (M.2); `import`/`surface` paths are
  DYNAMIC (runtime expressions) so they're found only by EXECUTING — a mesh path the caller's
  `FileTable` (raw → `Mesh`) lacks records a `File` need and substitutes an EMPTY placeholder so the
  run KEEPS GOING and surfaces the rest (a mesh rarely gates control flow → usually one more round,
  not one-per-file). Returning Incomplete instead of a sync reader-callback is the load-bearing
  choice: the browser reads files ASYNCHRONOUSLY and a sync callback can't await — Incomplete lets
  the async shell await between rounds. This is also why the coverage math gets EASIER (the pure
  core has no IO branches to cover) while the calling gets harder — which is fine, fab-scad's IO
  shell (M.4) is the ONE place `std::fs` lives, and it's the same seam an async wasm shell slots
  into. The determinism doctrine's "same input → bit-identical output" stays honest because the
  input is now EXPLICIT (the source + file tables + library paths), with no hidden `OPENSCADPATH`
  or ambient disk read reaching the evaluator.
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
(CI installs it today), no custom build required. **Determinism (G.3.6 RESOLVED):** the export
is byte-identical run-to-run with NO flag — there is no "sort the output" mode to enable, and
vertex/face order is GENERATION order (ring-major, same as scad-rs), not canonicalized. So the
harness compares vertices as a MULTISET, order-independent. The float-jitter AND export-
quantization problems (the oracle only writes OFF at ~6 digits, STL at f32) remain real
regardless — they set the metric FLOOR at ~1e-6, so exact-f64 through a file is off the table.

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
- **Coverage — 100%, but ONLY for the parser/lexer (deliberate exception, not a blanket rule):**
  the `fab-lang` CI lane gates `cargo llvm-cov --fail-under-lines 100 --fail-under-functions 100`. In
  a tokenizer an unexercised branch is a silent mis-tokenization, so the gap IS the bug — worth the
  friction here, nowhere else. LINE + FUNCTION only; region is NOT gated (infallible-in-context `?`
  error arms and test-side `matches!`/`assert` failure arms are structurally unreachable, so 100%
  regions would outlaw idiomatic `?`/`matches!`). Defensive branches get covered by testing the
  public decode helpers directly; genuinely-dead branches get DELETED, not excluded.
- **Interface tests at the Manifold boundary — soundness split (adjusted from the miri
  comment):** miri cannot execute foreign code, so it can't watch real FFI calls. The goal
  stands; the mechanism splits: (a) the geometry backend sits behind a trait; interface
  tests run against a PURE-RUST MOCK under MIRI, which checks all our unsafe/ownership
  handling up to the boundary; (b) the SAME interface tests re-run against real Manifold
  under AddressSanitizer/LeakSanitizer in a CI job, which is the tool class that actually
  sees across FFI. Between miri-on-mock and ASAN-on-real, the boundary is covered from both
  sides.
- **Differential testing (the workhorse).** Same source → scad-rs and oracle → compare.
  Mesh equality is subtle (triangulation order, float jitter, and the oracle EXPORT quantizes),
  so the gate is tolerance-based. **G.3.7 ran the experiment (sphere r=10, `$fn` 8→256 vs the
  nightly oracle) and RESOLVED the gate per model class:**
    - **Boolean-difference residual** — `vol((A−B) ∪ (B−A)) / vol(A)` in Manifold — is the gate
      for CURVED solids: triangulation-independent, "same solid" by construction, and it held at
      ~5e-7 flat across the whole sweep (that ~1e-6 floor IS the export precision — scad-rs is as
      close as a file round-trip permits). Backstop: volume + surface area within ~1e-7 relative,
      genus EXACT (0/0 every rung). Gate threshold: residual < 1e-5.
    - **Vertex-multiset** (canonical quantize) is a LOW-resolution / POLYHEDRAL gate only. It
      passed at `$fn`≤32 (eps 1e-5) but the quantization-boundary straddle makes it fragile as
      vertices densify (`$fn`≥64 read "none" while every other tier stayed tiny — a grid artifact,
      NOT divergence). Exact/polyhedral classes can use it; curved classes gate on residual.
    - Bonus: vertex AND triangle counts matched the oracle EXACTLY at every rung (32→32768 verts,
      60→65532 tris) — the tessellation port is faithful, not merely close. The exact-quadrant
      trig even reproduces the oracle's `-0.0` at θ=180.
  Echo/console output compares EXACTLY (BOSL2 asserts print — string equality is free fidelity).
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

1. ~~Mesh-equality metric~~ → RESOLVED (G.3.7 sphere sweep vs the nightly oracle): boolean-
   residual gate (< 1e-5) for curved classes, vertex-multiset for polyhedral, with a vol/area/
   genus backstop. Sphere held at ~5e-7 residual across `$fn` 8→256; see Testing + verification.
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
