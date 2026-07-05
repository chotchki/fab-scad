# PLAN

PIVOTED 2026-07-04: scad-rs — a GPL Rust implementation of the OpenSCAD language over the
Manifold kernel (SPEC.md, in drafting). The workflow-tool plan this file used to hold is
archived + its SPEC lives on as SPEC_workflow.md; every non-G item is parked in Backlog, not
dead. Cardinal rule unchanged: nothing deleted before it's archived AND validated.

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->
## Phase I - scad-rs: evaluator core
  Meta - Cranelift is the NATIVE JIT rung (chotchki's find: VERY approachable, and it's determinism-friendly — no auto-FMA, transcendentals stay CALLS to our own math, so the fixed-accumulation doctrine survives). NOT a replacement for the interpreter: the wasm/browser target can't JIT in-sandbox (the bet's #1 differentiator needs ONE implementation everywhere), and the interpreter is the bit-identical baseline the JIT validates against (fast==slow extends to fast==JIT). Spiked at I.8 (one hot function, prove bit-identical); the JIT-vs-intrinsics PROMOTE decision lands at Phase L with data.
- [ ] I.1 - Value model full: enum + NumList fast path + interned strings + lazy ranges; fast==slow BITWISE property via the shared fixed 4-lane accumulation order
  - [x] I.1.1 - Heterogeneous List(Rc<[Value]>) alongside the NumList fast path: nested lists, indexing, eq/order per Value.cc
  - [x] I.1.2 - Lazy Range value (start/step/end): inclusive-end iteration, element cap + warning, range-as-value
  - [x] I.1.3 - Function values / closures (params + body + captured env) — the currency I.2's calls spend
  - [ ] I.1.4 - Interned strings (deterministic intern table) + string indexing / char access
  - [x] I.1.5 - Fixed 4-lane accumulation order + the fast==slow BITWISE proptest (NumList fast path == List slow path)
- [ ] I.2 - Scoping engine: lexical envs, dynamic $-variables, children()/late binding, module+function call machinery on the explicit stack; + the use/include LOADER (file resolution + include-splice + use-import — parser stays zero-IO, this is where H's use/include AST nodes get resolved)
  - [x] I.2.1 - Lexical env chain (vars) + frame repr — DECISION: Rc<Frame> chain (correctness-first, single-threaded, the browser can't thread anyway; closures capture ONE Rc clone; $-scoping walks the chain). The frame-arena is a profiled I.6 opt, not now. PARALLELISM (captured 2026-07-04): it's not tree-vs-stack, it's a TREE OF STACK-MACHINES — fork independent units, each a sequential deterministic stack machine, join in FIXED order. Task-parallelism lives in the geometry DAG (6.1) + a parallel-comprehension MAP driver (fan iterations, assemble BY INDEX), NOT a rebuilt evaluator. Rc→Arc is the 3rd axis: parallel comprehensions need Send values+env (Arc taxes the sequential fast path for a benefit only they collect) — defer to a profiled I.6/intrinsics call; the swap is mechanical but crate-wide, internal to Value. Any parallelism MUST preserve a fixed reduction order (the 4-lane accumulation IS that) + buffered echo/warning order (else I.5's string-equal-vs-oracle breaks).
  - [x] I.2.2 - Dynamic $-variables: down-the-call-tree propagation + per-call override + the reaching-$-context
  - [x] I.2.3 - Function-call machinery ON THE EXPLICIT STACK: resolve + arg-match (positional/named/default) + body eval + return, no host recursion
    - [x] I.2.3.1 - Per-task scope + eval-context (function store) plumbing — Task carries its Scope so a call's body evals in the callee's scope while the caller's continuation waits; thread a Ctx (name→&'prog FunctionDef) through eval. Refactor only, all tests stay green.
    - [x] I.2.3.2 - User function calls on the explicit stack: resolve name→FunctionDef, arg-match (positional/named/default), push body eval in the call frame, return the value — no host recursion. The corner_brace-class deep-recursion (f(n)=f(n-1), 100k deep) proof lands here.
    - [x] I.2.3.3 - Function-literal VALUES / closures: Value::Function (params + body + captured Rc<Frame> env), function(x)body evaluates to it, calling a function value reuses I.2.3.2's machinery. Folds in I.1.3 (#70).
  - [ ] I.2.4 - Module-call machinery on the explicit stack: resolve user module + arg-bind + children eval → geometry tree
  - [ ] I.2.5 - children() / $children late binding (refers to the call-site children, late-bound)
  - [ ] I.2.6 - use/include LOADER: path resolution + include-splice + use-import (resolves H's zero-IO AST nodes; parser stays zero-IO)
- [ ] I.3 - Control flow + comprehensions + recursion bounded by memory — corner_brace-class deep recursion as the standing regression proof
  - [x] I.3.1 - let-expression `let(a=1,b=2) body` (ExprKind::Let): bind args left-to-right in a child scope, evaluate body there. Pure expression — deferred here from I.2.3.3. Reused by the comprehension `let`.
  - [x] I.3.2 - List comprehensions on the explicit stack: LcFor (iterate range/list), LcForC (C-style), LcEach (splice), LcIf/else (filter), lc-let — produce a List, nesting arbitrarily. Uses the I.1.2 range iterator; the element cap + warning ride here.
  - [ ] I.3.3 - STATEMENT control flow (if/for producing geometry → the CSG tree) — GEOMETRY-COUPLED, deferred to sit with Phase J (needs transforms/booleans/multi-child union). The expression-level halves (I.3.1/I.3.2) land now; this is the statement half.
- [x] I.4 - Builtin function library (~80: math/list/string/type predicates), each landing with its semantics/ test
  - [x] I.4.1 - Math builtins: abs/sign, sin/cos/tan/asin/acos/atan/atan2 (DEGREES, reuse trig.rs), floor/ceil/round, ln/log/exp/pow/sqrt, min/max, norm/cross. Bug-for-bug func.cc. (rands is non-deterministic → deferred separately.)
  - [x] I.4.2 - List + string builtins: len, concat, str, chr, ord, lookup, search, reverse — the glue BOSL2 lives on.
  - [x] I.4.3 - Type-predicate builtins: is_undef, is_bool, is_num, is_string, is_list, is_function — + version/version_num. rands as a SEEDED deterministic builtin (or a loud defer if the seed threading isn't ready).
- [ ] I.5 - undef propagation + warning/echo text bug-for-bug (string-equal vs oracle)
- [ ] I.6 - tracing spans on the call path + aggregating benchmark layer; release builds compile it out; overhead measured
- [ ] I.7 - Kani proofs: stack-machine push/pop discipline, range-iteration termination

- [ ] I.8 - Cranelift JIT spike: after the interpreter core, JIT one hot numeric function, measure speedup vs interpreter, PROVE bit-identical (fast==JIT); bank the float-discipline recipe — de-risks the L JIT-vs-intrinsics decision
## Phase J - scad-rs: geometry surface + cache
- [ ] J.1 - Geometry backend trait; interface suite runs miri-on-mock AND ASAN-on-real-Manifold in CI (the split that replaced raw miri-on-FFI)
- [ ] J.2 - 3D: primitives, multmatrix, booleans through Manifold; polyhedron with oracle-matching validation semantics
- [ ] J.3 - 2D subsystem on Clipper2: square/circle/polygon/offset/projection + linear/rotate_extrude bridging 2D→3D with tessellation parity
- [ ] J.4 - hull; import() via our STL/3MF readers; text/minkowski/surface = LOUD deferred stubs (blow up, complain, never silently wrong)
- [ ] J.5 - Content-addressed CSG cache: node hash = subtree + resolved params + reaching $-context; in-memory tier + hit-rate counters (the on-disk tier stays a storage decision)

## Phase K - scad-rs: differential harness + semantics corpus
- [ ] K.1 - Harness v1: both engines, metric gate per model class, corpus tiers 1-3 wired in CI (OpenSCAD suite, BOSL2 tests, models/)
- [ ] K.2 - semantics/ segmentation formalized: naming + provenance conventions; G.3/I tests migrated in
- [ ] K.3 - ChaCha8-seeded grammar-directed program generator v0; seed logged per run; one-command failure replay
- [ ] K.4 - Published artifacts per run: divergence report + the (initially empty) intrinsic matrix — the trend line starts before the intrinsics do

## Phase L - scad-rs: the BOSL2 gauntlet (exit gate for the bet)
  Meta - This is where we should evaluate if BOSL2 is special or if it would be wiser to move towards a JIT approach for seen user defined OpenSCAD functions (with the ability to override to custom performance implementations)
- [ ] L.1 - Pinned BOSL2 test suite through scad-rs; divergences triaged into named buckets
- [ ] L.2 - Burn-down: fixes land as semantics/ tests; expect this to expose evaluator gaps — that's the point
- [ ] L.3 - models/ tree end-to-end (teardrop/onion/screw_hole, corner_brace, Underdesk); benchmark corpus captured via the tracing layer on every run
- [ ] L.4 - Exit review: divergences zero-or-documented, perf-vs-oracle published; rung 2/3 (intrinsics, JIT) phase cut FROM THIS DATA

## Backlog (not yet phased)

Parked 2026-07-04 for the scad-rs pivot — the workflow tool works and stays in service; these resume when G stabilizes:

- **Phase 7 - Web + publish (whole phase parked):** 7.1 STL decimation for the Three.js viewer (poly budget); 7.2 cover image + description bundle matching hotchkiss.io content model; 7.3 API-key auth + publish endpoint on hotchkiss.io (passkeys stay for humans); 7.4 `fab publish`: one project live on hotchkiss.io/projects
- **Phase 8 - Pilot migration (whole phase parked):** 8.1 confirm pilots (shoe_holder, keyboard_tent, nail_polish_holder); 8.2 migrate each + minimal project.toml, dogfood the fields; 8.3 scad fixes + validate output version + parity vs archived; 8.4 prune redundant old versions LOCALLY then publish; 8.5 retro into template/manifest/tool
- **Phase 9 - Reorg convention (whole phase parked):** 9.1 lock the folder convention (libs/scad-lib/models submodules, excluded outputs, NAS archive); 9.2 triage remaining ~59 projects into a migration backlog; 9.3 migrate opportunistically
- **5.1 / 5.3 parent validation gates** — children all done; the parent tick awaits a deliberate exit validation pass (deferred 2026-07-04)
- **6.1 render engine** (parallel targets + thumbnails + N/M progress) — the DAG engine; scad-rs makes it deeply instrumentable, revisit after G
- **6.4 embedded magnets** (split around cavities + pause-at-layer)
- **6.5 Bambu 3mf settings embedding** (adopt only if clean)
- **6.6 demote import() crutch** to freeze-source-once; DAG resolver as fallback
- **17.6 GUI auto-on-open rotate-to-fit** — deferred from 17.6 on 2026-07-04
- **18.9 crates.io publish** — now as GPL-2.0-or-later post-relicense; dry-run was clean at 103KiB — deferred from 18.9 on 2026-07-04
- **Showcase→slicer deep-link: project page hands its published STL into the slicer special page (same-origin fetch, COEP-safe) — publish-side wiring + slicer URL param** *(restored 2026-07-04; bridge dropped it during the sweep)*
- **Resume the native channel: dispatch release-native.yml (mac DMG + Windows NSIS artifacts), fill winget InstallerSha256 from the release, decide the signing purchases (docs/packaging.md)** *(restored 2026-07-04)*
- **Colored 3mf EXPORT: assemblies export per-part pieces as separate objects with extruder mapping (distinct color → Bambu AMS slot; extend bambu::Placed + model_settings extruder) — the other half of A.9's color carry-through** *(restored 2026-07-04)*
- **fab-web wire-size stretch: ≤7 MiB brotli needs build-std and/or naga stripping — feature surgery EXHAUSTED; NOTE the egui research: eframe+wgpu analog ships 1.7 MB brotli, the full-bevy-exit is the real answer** *(restored 2026-07-04)*
- B.6 - Customizer stretch: expose the .scad's top-level params in the panel, tweak → worker re-render *(deferred from phase `B` on 2026-07-03; scad-rs makes this an AST walk — fold into G when the evaluator lands)*
- **Safari deep-recursion fix, the real one: build openscad wasm ourselves (openscad-wasm docker recipe) with -sSTACK_SIZE=8MB+, test corner_brace.scad under JSC via safaridriver; if the bigger baked stack fixes it, swap the pin to our build; if not, it's JSC engine frames and upstream/WebKit territory** — added 2026-07-04; scad-rs kills this class by construction (explicit-stack evaluator), so only worth doing if G drags
- **GUI/UX next-major (from SPEC_workflow.md "Next Major Spec"):** tabbed guided flow (Config/Loading/Planes/Plates), bevy_egui shell replacing feathers (research: GREEN — tracks bevy within ~44h, wasm first-class, picking designed-in), URL-param/TOML config unification (PlanConfig schema), fab-gui retirement decision, DAG per-target animations aligned with 6.1
- **fab owns $fn: inject draft/final quality + strip `$fn = $preview ? …` from all scad model files** — added 2026-06-28
- document gui startup
- gui remembers last folder it was used against
- **CI covers only fab-scad (the sole workspace default member) — fab-geom/fab-gui/fab-web get no clippy/test in CI. Flip the shared ci.yml steps to --workspace once the lang implementation phases settle, and fix whatever those crates are currently hiding. (fab-lang already has its own explicit lane.)** — added 2026-07-04.
- **Tri-OS CI matrix (linux/mac/windows) — the PROOF of the determinism doctrine's "bit-identical, every platform" claim (cross-OS float-order/hasher divergence surfaces as a mismatch). Add to the fab-lang lane first (cheap, pure-Rust); the Manifold-C++ crates need the toolchain per runner, so fold those in when the differential harness lands. WILL do.** — added 2026-07-04.
- **cargo-mutants mutation gates on the parser + evaluator — proves the tests CATCH bugs, not just run (kills survivors that fuzzing/proptest miss; complements the fuzzer). Wire at the H.5 / I test phases.** — added 2026-07-04.
- **Enable clippy::allow_attributes on fab-lang (prefer #[expect] over #[allow] so a suppression fails once it's no longer needed) — the stricter sibling of allow_attributes_without_reason. Turn on once the suppression set stabilizes.** — added 2026-07-04.
- **Migrate fab-scad/fab-geom/fab-gui/fab-web from edition 2021 to 2024 (fab-lang is already 2024). Mechanical via `cargo fix --edition` per crate + verify each. Do this when we're done working in lang/ — not before (avoid churning the established crates mid-lexer).** — added 2026-07-04.
- **Evaluate make_mut copy-on-write (or an im-style persistent vector) for the NumList list-BUILD path — a BOSL2 VNF-math perf optimization. v0 uses immutable Rc<[f64]> (read/memory-optimal). Profile-driven at I.1 / the intrinsics work: measure whether BOSL2's concat/comprehension append-accumulation benefits vs the read-path cost. Internal to the Value enum, non-breaking to swap.** — added 2026-07-04.
- **Longer-term: re-evaluate adopting more of Manifold's NATIVE primitives/operations vs our OpenSCAD-matching tessellation, once scad-rs is fully implemented and the differential harness (K) has data. Manifold-native avoids our tessellation but DIVERGES from OpenSCAD's mesh (different vertex algorithm) — only wise where the metric tolerates it or we deliberately accept non-byte-exact output for perf. Revisit alongside the geometry backend (J.1) + intrinsics (rung 2).** — added 2026-07-04.
