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
added 2026-07-06.
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
  - [x] I.2.4 - Module-call machinery on the explicit stack: resolve user module + arg-bind + children eval → geometry tree
    - [x] I.2.4.1 - Loader: collect module defs (ModStore) through use/include, like functions
    - [x] I.2.4.2 - Ctx.modules + thread global through the statement side (module bodies = global.child + params, OpenSCAD hygiene)
    - [x] I.2.4.3 - Module-call arm: resolve user module + arg-bind (positional/named/default/$-args) + depth-guarded body eval → GeoNode
  - [x] I.2.5 - children() / $children late binding (refers to the call-site children, late-bound)
  - [x] I.2.6 - use/include LOADER: path resolution + include-splice + use-import (resolves H's zero-IO AST nodes; parser stays zero-IO)
  - [x] I.2.7 - Whole-scope variable binding — hoist top-level assignments, last-assignment-wins (OpenSCAD), not sequential
  - [x] I.2.8 - Differential vs the OpenSCAD oracle: use/include file-based cases (two-driver harness landed 04b8f1d)
- [ ] I.3 - Control flow + comprehensions + recursion bounded by memory — corner_brace-class deep recursion as the standing regression proof
  - [x] I.3.1 - let-expression `let(a=1,b=2) body` (ExprKind::Let): bind args left-to-right in a child scope, evaluate body there. Pure expression — deferred here from I.2.3.3. Reused by the comprehension `let`.
  - [x] I.3.2 - List comprehensions on the explicit stack: LcFor (iterate range/list), LcForC (C-style), LcEach (splice), LcIf/else (filter), lc-let — produce a List, nesting arbitrarily. Uses the I.1.2 range iterator; the element cap + warning ride here.
  - [x] I.3.3 - STATEMENT control flow (if/for producing geometry → the CSG tree) — GEOMETRY-COUPLED, deferred to sit with Phase J (needs transforms/booleans/multi-child union). The expression-level halves (I.3.1/I.3.2) land now; this is the statement half.
- [x] I.4 - Builtin function library (~80: math/list/string/type predicates), each landing with its semantics/ test
  - [x] I.4.1 - Math builtins: abs/sign, sin/cos/tan/asin/acos/atan/atan2 (DEGREES, reuse trig.rs), floor/ceil/round, ln/log/exp/pow/sqrt, min/max, norm/cross. Bug-for-bug func.cc. (rands is non-deterministic → deferred separately.)
  - [x] I.4.2 - List + string builtins: len, concat, str, chr, ord, lookup, search, reverse — the glue BOSL2 lives on.
  - [x] I.4.3 - Type-predicate builtins: is_undef, is_bool, is_num, is_string, is_list, is_function — + version/version_num. rands as a SEEDED deterministic builtin (or a loud defer if the seed threading isn't ready).
- [x] I.5 - undef propagation + warning/echo text bug-for-bug (string-equal vs oracle)
- [x] I.6 - tracing spans on the call path + aggregating benchmark layer; release builds compile it out; overhead measured
- [x] I.7 - Kani proofs: stack-machine push/pop discipline, range-iteration termination

- [x] I.8 - Cranelift JIT spike: after the interpreter core, JIT one hot numeric function, measure speedup vs interpreter, PROVE bit-identical (fast==JIT); bank the float-discipline recipe — de-risks the L JIT-vs-intrinsics decision
  - [x] I.8.1 - fab-jit crate scaffold: cranelift-jit deps, native-only, the single documented unsafe seam (fn-ptr call)
  - [x] I.8.2 - Expr → Cranelift IR compiler for the numeric subset, fixed left-to-right order matching the interpreter
  - [x] I.8.3 - Ops Cranelift lacks → external CALLS to our Rust math (% → a%b, ^ → a.powf(b)) — the determinism recipe
  - [x] I.8.4 - fast==JIT BITWISE differential (corpus + coeff-proptest) + the speedup benchmark
  - [x] I.8.5 - Bank the float-discipline recipe (doc) — feeds the Phase-L JIT-vs-intrinsics promote decision
- [ ] I.9 - fixing BOSL2 — evaluator bring-up (parse ✓ 56/56; short-circuit ✓; burn down the eval divergences)
  - [x] I.9.1 - Member access .x/.y/.z on vectors (ExprKind::Member) — deferred at I.1, now the next BOSL2 eval blocker
  - [x] I.9.2 - BOSL2 cyl → "Invalid transformation matrix" — a matrix helper (down/skew/up/multmatrix chain) diverges
  - [x] I.9.3 - BOSL2 cuboid → "Input to sum is non-numeric or inconsistent" — a list-build feeds sum() a non-numeric
  - [x] I.9.4 - BOSL2 sphere → "Bad arguments" — an arg-normalization assert fires (spherical primitive / attachable)
  - [x] I.9.5 - BOSL2 sphere/cyl/cuboid → "user-module recursion too deep" — unbounded recursion on the attachable path
  - [x] I.9.6 - BOSL2 attachable → `let(...) children()` used as a STATEMENT (module-form let)
## Phase J - scad-rs: geometry surface + cache
added 2026-07-05.
- [x] J.1 - Geometry backend trait; interface suite runs miri-on-mock AND ASAN-on-real-Manifold in CI (the split that replaced raw miri-on-FFI)
  - [x] J.1.1 - GeometryBackend trait + MockBackend + ManifoldBackend + the generic interface suite (both green under cargo test)
  - [x] J.1.2 - Run the interface suite under miri (mock) + ASAN (real Manifold) in CI — the split that replaces miri-on-FFI
- [x] J.2 - 3D: primitives, multmatrix, booleans through Manifold; polyhedron with oracle-matching validation semantics
  - [x] J.2.1 - GeoNode CSG tree + evaluator produces it: primitives→Leaf, transforms→Transform, implicit top-level Union
  - [x] J.2.2 - Boolean modules union/difference/intersection → the boolean GeoNodes over children
  - [x] J.2.3 - fab-scad tree-walker: GeoNode → Solid via GeometryBackend; rewire the FabLang differential driver through it
  - [x] J.2.6 - polyhedron() primitive + oracle-matching validation semantics
    - [x] J.2.6.1 - polyhedron(points,faces,convexity) → Mesh Leaf in fab-lang: raw verts + fan-triangulated n-gon faces (OpenSCAD tessellation), no backend needed
    - [x] J.2.6.2 - polyhedron validation bug-for-bug: out-of-range face index / <3-vertex face / non-manifold → OpenSCAD warn-and-render vs error
    - [x] J.2.6.3 - Differential: spheroid + a VNF shape vs oracle (boolean-residual / vertex-multiset)
  - [x] J.2.7 - Differential: CSG programs (transforms/booleans/multi-object/polyhedron) vs the oracle via boolean-residual
    - [x] J.2.7.1 - Harness: oracle-side re-import uses f32 MeshGL → boolean-result meshes fail; blocks the boolean differential
  - [x] J.2.8 - color() module → GeoNode::Color + Rgba vocab + CSS named-color table (BOSL2-critical)
  - [x] J.2.9 - Color propagation through Manifold (vertex props survive booleans) + oracle capture + differential
- [ ] J.3 - 2D subsystem on Clipper2: square/circle/polygon/offset/projection + linear/rotate_extrude bridging 2D→3D with tessellation parity
  - Comment: Is clipper2 the right library for this? could manifold do it?
  - [x] J.3.1 - DECISION + 2D backend seam: Manifold CrossSection for all 2D/hull/extrude/projection (zero new geometry deps — bundles Clipper2, the lib OpenSCAD 2021+ uses). GeoNode↔CrossSection; note in SPEC
  - [x] J.3.2 - 2D primitives square/circle/polygon → Shape2D node; circle uses our $fn fragment math for parity
    - [x] J.3.2.1 - J.3.2.1 - eval-wire: recognize 2D primitives + thread Geo{D2,D3} through the geometry pass
  - [x] J.3.3 - 2D booleans + offset over 2D children (CrossSection ops)
  - [x] J.3.4 - linear_extrude (height/twist/scale/slices) → 3D; tessellation parity MEASURED vs oracle (Manifold's if the metric tolerates, else our loft)
    - [x] J.3.4.1 - J.3.4.1 - twisted linear_extrude loft: match OpenSCAD's profile-resampling + slice interpolation
  - [x] J.3.5 - rotate_extrude (angle, $fn) → 3D; reuse the ring/segment math
  - [x] J.3.6 - projection(cut) 3D→2D via slice_to_cross_section
  - [x] J.3.7 - Differential: path/region-derived BOSL2 2D shapes vs oracle
- [ ] J.4 - hull; import() via our STL/3MF readers; text/minkowski/surface = LOUD deferred stubs (blow up, complain, never silently wrong)
  - Comment: Text could be handled by https://github.com/pop-os/cosmic-text . I'm still researching minkowski.
  - [x] J.4.1 - hull() → Manifold hull/batch_hull over children (2D + 3D); unblocks cuboid chamfer/rounding + masks
  - [ ] J.4.2 - import() via our STL/3MF readers (threemf/zip/quick-xml deps already present)
    - [>] J.4.2.1 - J.4.2.1 - import() eval + backend wiring (STL/3MF readers → Leaf)
    - [>] J.4.2.2 - J.4.2.2 - import() differential vs oracle (round-trip a known STL + 3MF)
  - [x] J.4.3 - text() LANDED via rustybuzz (shaping, the pure-Rust harfbuzz port — matches OpenSCAD's harfbuzz) + ttf-parser (glyph OUTLINES) over a BUNDLED Liberation Sans (OpenSCAD's default, SIL OFL, pinned at src/eval/fonts/). NOT cosmic-text — that rasterizes to pixels + does system-font lookup (fontconfig = non-deterministic, banned); we need vector contours from a pinned face. Pipeline: shape → per-glyph outline → $fn-flatten Béziers → placed/scaled contours → Shape2D::Polygon (even-odd fill, so glyph HOLES resolve for free). halign/valign/spacing/direction/script/language honored; `font=` accepts but ships one face (system fonts = a later opt-in). Deterministic (pure Rust + pinned font) + oracle-matchable (same glyphs as OpenSCAD → volume-residual). Validated: 'O' fills as a RING not a box; multi-glyph advance; empty→empty. Used across the models/ tree (part numbers, version stamps, labels) → unblocks L.3.
  - [x] J.4.4 - minkowski() LANDED via Manifold's NATIVE `minkowski_sum` (manifold3d 0.3.3 clean drop-in — same manifold-csg lineage, no migration; wraps Manifold C++ PR #666's tiered hull+union). `GeoNode::Minkowski` folds the binary sum with the empty-ANNIHILATOR rule (A⊕∅=∅); 2D LOUD-deferred to Clipper2 like 2D hull. Validated: box⊕box=summed box (1728 exact, oracle-free) + volume-residual for the rounding case; test_cyl clears → corpus 99.1%, 0 assertion / 0 unimplemented. Research + design writeup: docs/minkowski-design.md. (surface() stays a LOUD-deferred stub.)
  - [ ] J.4.5 - DETERMINISM: native geometry runs Manifold with TBB (`parallel` feature ON) = non-deterministic parallel reduction; wasm is single-threaded. Doctrine #36 needs bit-identical output cross-platform — build native with `parallel` OFF (`MANIFOLD_PAR=NONE`, matching wasm) + re-baseline, OR prove TBB reduction is deterministic. Surfaced by the minkowski research (manifold#666 CI: non-convex² broke Mac/Windows on non-CCW triangulation even with `deterministic=true`). Affects ALL geometry, not just minkowski.
- [ ] J.5 - Content-addressed CSG cache: node hash = subtree + resolved params + reaching $-context; in-memory tier + hit-rate counters (the on-disk tier stays a storage decision)

- [x] J.6 - Unify fab-scad's geom::V3 ([f64;3] orientation helpers) + printer-domain [f64;3] into fab_lang::Vec3
## Phase K - scad-rs: differential harness + semantics corpus
- [ ] K.1 - Harness v1: both engines, metric gate per model class, corpus tiers 1-3 wired in CI (OpenSCAD suite, BOSL2 tests, models/)
  - [x] K.1.1 - K.1.1 - BOSL2 test corpus tier: sweep the .scadtest suite through scad-rs
  - [ ] K.1.2 - K.1.2 - Perf tier: scad-rs vs OpenSCAD full-pipeline wall-time on geometry
- [ ] K.2 - semantics/ segmentation formalized: naming + provenance conventions; G.3/I tests migrated in
- [ ] K.3 - ChaCha8-seeded grammar-directed program generator v0; seed logged per run; one-command failure replay
- [ ] K.4 - Published artifacts per run: divergence report + the (initially empty) intrinsic matrix — the trend line starts before the intrinsics do

## Phase L - scad-rs: the BOSL2 gauntlet (exit gate for the bet)
  Meta - END-STATE EXECUTION MODEL (chotchki 2026-07-05): THREE tiers forming a bit-identity chain. web ships interpreter + intrinsics (optimized_functions); desktop adds the Cranelift JIT (the browser can't JIT in-sandbox). Because intrinsics == interpreter (fast==slow) AND JIT == interpreter (fast==JIT, proven at I.8), web output == desktop output ALWAYS — the JIT is pure SPEED on desktop, never a divergent mesh. So L is NOT "JIT vs intrinsics" as competitors, they're complementary LAYERS: intrinsics are hand-written + wasm-safe (the browser's whole perf story, the load-bearing + harder half — a bit-identical reimpl each, and BOSL2 is a big surface), the JIT auto-sweeps the numeric long tail on desktop only. L becomes a COVERAGE ALLOCATION — which hot functions get hand-intrinsified (everywhere) vs left to the JIT (desktop) vs the interpreter (everywhere, slow) — driven by the aggregate-corpus profiling (backlog #93). "Is BOSL2 special" is the same question: does the gauntlet cluster into a few hot intrinsics, or spread into a broad tail the JIT handles? (Custom perf overrides = the hand-intrinsified tier.)
  Implementation note, we should determine whether an intrinsic matches based on its original AST. That will help survive reformats or code comment changes.
added 2026-07-07.
- [x] L.1 - Pinned BOSL2 test suite through scad-rs; divergences triaged into named buckets
- [ ] L.2 - Burn-down: fixes land as semantics/ tests; expect this to expose evaluator gaps — that's the point
  - [x] L.2.1 - L.2.1 - Name the divergences: sharpen the generic clusters into a per-symbol worklist
  - [ ] L.2.2 - L.2.2 - Missing builtins: implement the functions the corpus names
  - [ ] L.2.3 - L.2.3 - Missing modules: the unknown-module tests
  - [ ] L.2.4 - L.2.4 - Builtin correctness bugs: named singletons that return the wrong value
  - [ ] L.2.5 - L.2.5 - Domain assert families: beziers, screw tables, polyhedra
  - [ ] L.2.6 - L.2.6 - The got==expected long tail: individual math divergences
  - [ ] L.2.7 - L.2.7 - Timeouts: the hull/region hangs
  - [x] L.2.8 - L.2.8 - Recursive function-literals (letrec): a closure must see its own binding
  - [x] L.2.8a - L.2.8a - Island-global bootstrapping: a top-level constant's fn call sees the constants hoisted so far (modular_hose +5)
  - [x] L.2.8b - L.2.8b - Empty-statement $children: a lone `;` is not a child (screw/attachable family +5)
  - [x] L.2.8c - L.2.8c - Seedless rands advances one per-eval stream (plane_intersection +2)
  - [x] L.2.8d - L.2.8d - Unary minus recurses into nested lists (-matrix element-wise; rot_inverse/rot_resample +4)
  - [x] L.2.8e - L.2.8e - C-style for binds init/update sequentially (skin distance + dependent-update DP idiom +7)
  - [x] L.2.8f - L.2.8f - `each` splices into a guard/loop operand (`each if(c) list`; nurbs_curve +4)
  - [x] L.2.8g - L.2.8g - str() renders nested function literals bare (OpenSCAD format; fnliterals f_1arg/f_2arg/f_3arg +2)
  - [x] L.2.8h - L.2.8h - a `let` in a vector is transparent (splices iff body does; trapezoid corner paths +3)
  - [x] L.2.8i - L.2.8i - fnliterals f_acos: acos/asin exact at nice angles (SNAP) — RESOLVED 2026-07-07. Root cause: macOS libm's acos(-0.5) is 2 ULP off the correctly-rounded 2π/3 → `to_degrees` gives 120.0000…01, failing BOSL2's exact-`==` f_acos. Rejected paths: the `(r/π)*180` rad2deg tweak (FALSIFIED — regressed test_glued_circles's arc discretization) and a correctly-rounded-libm crate (musl `libm` is ALSO off, differently — verified). FIX: snap acos/asin at the EXACT nice cosines/sines (`acos_degrees`/`asin_degrees`, the inverse analogue of our exact-quadrant sin/cos) to 0/30/45/60/90/120/135/150/180 — which IS glibc's correctly-rounded output there, so it's oracle-faithful AND deterministic (same every platform). A non-nice input (glued_circles' rounded near-√2/2 literal) still routes to libm untouched → no collateral. Determinism doctrine #36: SATISFIED for nice angles (bit-identical everywhere); the residual non-nice-angle platform-libm divergence is the general transcendental question, unchanged.
  - [x] L.2.8n - L.2.8n - is_num(NaN) is FALSE (was the "f_is_num" half of L.2.8i, NOT a math bug): OpenSCAD `func.cc` guards `type==NUMBER && !isnan`, so a NaN routes to is_nan/typeof "nan", never "number" → fnliterals f_is_num +1
  - [x] L.2.8j - L.2.8j - builtin args are POSITIONAL, name ignored: OpenSCAD builtins have no declared params, so the split-off named map dropped `search`'s `index_col_num`/`num_returns_per_match` → column search defaulted to col 0 → in_list("bar",…,idx=1) +1
  - [x] L.2.8k - L.2.8k - bool ordering + range structural equality: `false<true` (coerce 0/1) unblocks compare_vals; a range is SELF-equal even with a NaN step (`is_nan([0:NAN:INF])` false) → typeof "invalid" +2
  - [x] L.2.8l - L.2.8l - duplicate param name binds arg-over-default (OpenSCAD two-phase: ALL defaults, THEN args): BOSL2's rounding_edge_mask/fillet list `r` twice, so a single pass let the trailing undef clobber `r=2` → cleared the all_nonnegative assert (both then block on L.2.8m)
  - [x] L.2.8m - L.2.8m - module-body-LOCAL function/module definitions (nested-def hoisting + scoping): a `function`/`module` defined INSIDE a body is now hoisted into that body scope — functions as name-stamped closures CLOSING OVER the enclosing locals (`make_path` reads body `steps`/`ang`), modules onto a scope-local stack carrying their defining scope (so a nested module's body sees sibling nested funcs — `testvercmp`→`diversify`). Cleared 877→887 (+10): every nested-def "unknown function/module" — make_path (rounding_edge_mask, fillet), qrok, nullcheck, valid_lock/apply_lock, check_path_apply, testvercmp/diversify, ghost_if, corner_shape ×2. Unimplemented 13→3 (only `parent_module` builtin + minkowski left). v1 simplifications noted: nested defs share the var namespace (no var-vs-fn collision in real code) + module VISIBILITY is dynamically scoped (never resolves a wrong def since local names are unique).
  - [x] L.2.8o - L.2.8o - parent_module(n) / $parent_modules (L.2.2 missing builtin): the module-instantiation NAME stack — `call_user_module` pushes/pops the callee name, `parent_module(n)` reads `stack[len-1-n]` (0=self, 1=parent), `$parent_modules`=ancestor count. BOSL2's `deprecate()` echoes `parent_module(1)` → test_rounding_angled_edge_mask/_corner_mask +2 (887→888). With this the whole "unknown function/module" CLASS is cleared — unimplemented is JUST the deferred minkowski, so L.2.2 (missing builtins) + L.2.3 (missing modules) are effectively DONE.
  - [x] L.2.8p - L.2.8p - children() sees the CURRENT dynamic $-context (foundational): `children()` rendered the call-site children in the caller's LEXICAL scope but WITHOUT overlaying the $-vars in effect where `children()` is instantiated. $-vars are dynamically scoped, so BOSL2's `attachable()` (which sets `$parent_geom`/`$parent_parts` in its body right before `children()`) had those read back as undef by `parent()`/`desc_dist`/`parent_part` and the `ring_hook` orient → a zero-size default geom. Fix: overlay the current scope's specials onto the caller's lexical scope in `eval_children` (propagates transitively through forwarding `children()`). ONE fix cleared ALL 3 remaining assertions (parent_part, desc_dist, ring_hook) → the ASSERTION BUCKET IS NOW ZERO (890→891, 98.9%). Every correctness/math divergence resolved; only the deferred minkowski + the L.2.7 hull/region timeouts remain.
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
- **Aggregate corpus profiling: sum the I.6 tracing layer across the whole BOSL2/models corpus → hot-spot report** — added 2026-07-05.
- **Receipts ledger (docs/testing-cards.md): when a real bug lands, log which testing card caught it + why the rest missed — the ledger feeds the blog series, the proven-panic-free browser-safe claim and the FeOphant playbook. Start filling at K.1/L.2 when divergences flow.** — added 2026-07-05.
- **Warning text bug-for-bug vs the oracle — the deferred half of I.5 (Message::Warning channel exists, empty)** — added 2026-07-05.
- **Verify release builds actually emit SIMD/AVX for the lane-based dot + matrix accumulation** — added 2026-07-06.
- **Explicit-stack STATEMENT machine: convert eval_stmt/call_user_module from host-recursion to an explicit work-stack (like the expression machine), retiring MAX_MODULE_DEPTH's stack-fragility — 'Safari cliff structurally impossible' on the statement side. Deferred at I.9.6 (production-safe at 256; do at the I/J boundary)** — added 2026-07-06.
- **J.4.2.1 - import() eval + backend wiring (STL/3MF readers → Leaf)** — deferred from J.4.2.1 on 2026-07-06.
- **J.4.2.2 - import() differential vs oracle (round-trip a known STL + 3MF)** — deferred from J.4.2.2 on 2026-07-06.
- **surface() PNG heightmap load (deferred from M.5.2)** — added 2026-07-06.
