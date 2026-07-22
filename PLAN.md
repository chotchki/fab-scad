<!-- plan-bridge:phase-high-water=SU -->
# PLAN

PIVOTED 2026-07-04: scad-rs — a GPL Rust implementation of the OpenSCAD language over the
Manifold kernel (SPEC.md, in drafting). The workflow-tool plan this file used to hold is
archived + its SPEC lives on as SPEC_workflow.md; every non-G item is parked in Backlog, not
dead. Cardinal rule unchanged: nothing deleted before it's archived AND validated.

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->
## Phase K - scad-rs: differential harness + semantics corpus
- [ ] K.1 - Harness v1: both engines, metric gate per model class, corpus tiers 1-3 wired in CI (OpenSCAD suite, BOSL2 tests, models/)
  - [x] K.1.1 - K.1.1 - BOSL2 test corpus tier: sweep the .scadtest suite through scad-rs
  - [x] K.1.2 - K.1.2 - Perf tier: scad-rs vs OpenSCAD full-pipeline wall-time on geometry
- [ ] K.2 - semantics/ segmentation formalized: naming + provenance conventions; G.3/I tests migrated in
- [x] K.3 - ChaCha8-seeded grammar-directed program generator v0; seed logged per run; one-command failure replay
- [ ] K.4 - Published artifacts per run: divergence report + the (initially empty) intrinsic matrix — the trend line starts before the intrinsics do

## Phase L - scad-rs: the BOSL2 gauntlet (exit gate for the bet)
  Meta - END-STATE EXECUTION MODEL (chotchki 2026-07-05): THREE tiers forming a bit-identity chain. web ships interpreter + intrinsics (optimized_functions); desktop adds the Cranelift JIT (the browser can't JIT in-sandbox). Because intrinsics == interpreter (fast==slow) AND JIT == interpreter (fast==JIT, proven at I.8), web output == desktop output ALWAYS — the JIT is pure SPEED on desktop, never a divergent mesh. So L is NOT "JIT vs intrinsics" as competitors, they're complementary LAYERS: intrinsics are hand-written + wasm-safe (the browser's whole perf story, the load-bearing + harder half — a bit-identical reimpl each, and BOSL2 is a big surface), the JIT auto-sweeps the numeric long tail on desktop only. L becomes a COVERAGE ALLOCATION — which hot functions get hand-intrinsified (everywhere) vs left to the JIT (desktop) vs the interpreter (everywhere, slow) — driven by the aggregate-corpus profiling (backlog #93). "Is BOSL2 special" is the same question: does the gauntlet cluster into a few hot intrinsics, or spread into a broad tail the JIT handles? (Custom perf overrides = the hand-intrinsified tier.)
  Implementation note, we should determine whether an intrinsic matches based on its original AST. That will help survive reformats or code comment changes.
added 2026-07-07.
- [x] L.1 - Pinned BOSL2 test suite through scad-rs; divergences triaged into named buckets
- [x] L.2 - Burn-down: fixes land as semantics/ tests; expect this to expose evaluator gaps — that's the point
  - [x] L.2.1 - L.2.1 - Name the divergences: sharpen the generic clusters into a per-symbol worklist
  - [x] L.2.2 - L.2.2 - Missing builtins: implement the functions the corpus names
  - [x] L.2.3 - L.2.3 - Missing modules: the unknown-module tests
  - [x] L.2.4 - L.2.4 - Builtin correctness bugs: named singletons that return the wrong value
  - [x] L.2.5 - L.2.5 - Domain assert families: beziers, screw tables, polyhedra
  - [x] L.2.6 - L.2.6 - The got==expected long tail: individual math divergences
  - [x] L.2.7 - L.2.7 - Timeouts: 6 of 8 CLEARED (893→899, 99.8%) by a FOUNDATIONAL scope perf fix — NOT hull/region hangs but per-call $-context COPYING. Every user function/module call copied the caller's reaching $-context into the call scope (`caller.specials()` → O(#$-vars)); BOSL2 sets 42 top-level $-vars, so call-heavy geometry paid 42 clones+inserts PER CALL. Fix: split the DYNAMIC $-chain from the LEXICAL chain in Scope — a call frame inherits the caller's $-context BY REFERENCE (`dynamic_parent`), O(1) call setup; iterative `Frame::Drop` keeps deep recursion heap-bounded (the dynamic chain is now deep). Cleared gears×3, circle_3points, exclusive_or, rot, vnf_area. gaussian_rands 52s→~12s. Remaining 2: gaussian_rands (borderline — passes solo, times out under the parallel sweep; a JIT/intrinsics target — 300k-element sqrt/ln/cos comprehension, per chotchki) + spheroid (investigate).
  - [x] L.2.7a - L.2.7a - spheroid timeout: investigate the last non-JIT timeout (high-$fn sphere geometry). gaussian_rands is deferred to the JIT/intrinsics tier (rung 2/3) — the numeric-comprehension hot path it exemplifies is exactly what optimized_functions/Cranelift target.
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
- [x] L.3 - models/ tree end-to-end (teardrop/onion/screw_hole, corner_brace, Underdesk); benchmark corpus captured via the tracing layer on every run
  - [x] L.3.1 - L.3.1 - models-surfaced evaluator gaps: resize/render modules + attachable×3
  - [x] L.3.2 - L.3.2 - `* ! % #` instantiation modifiers honored in eval (the #1 divergence)
  - [x] L.3.3 - L.3.3 - assert/echo are passthrough: render child geometry (BOSL2 left/fwd fix)
  - [x] L.3.4 - L.3.4 - BOSL2 `sweep()`/VNF returns empty → chamfer/rounding/teardrop/rotate_sweep render nothing (14/19 divergences)
  - [x] L.3.5 - L.3.5 - Manifold version parity: coincident-face genus divergences (ours 3.5.x vs OpenSCAD 3.4.1)
  - [x] L.3.6 - L.3.6 - text() 100/72 DPI scale (was rendering glyphs 0.72× too small)
  - [x] L.3.8 - L.3.8 - color() on 2D geometry tags the color (Shape2D::Color) — the 343× BOSL2-example bucket
- [x] L.4 - Exit review: divergences zero-or-documented, perf-vs-oracle published; rung 2/3 (intrinsics, JIT) phase cut FROM THIS DATA
- [ ] L.5 - L.5 - Evaluator-gap closure: BOSL2 examples + models/ render clean (the perf-blog gate)
  - [x] L.5.1 - L.5.1 - render() + resize() builtin modules
  - [x] L.5.2 - L.5.2 - children(i) interleaved-assignment child-scope + $children fix
  - [x] L.5.3 - L.5.3 - seed viewport specials $vpr/$vpt/$vpd/$vpf
  - [x] L.5.4 - L.5.4 - resolve BOSL2 std-chain symbols (hulling, _gather_contiguous_edges_r)
  - [x] L.5.5 - L.5.5 - unified.scad fab-specific assert (OpenSCAD renders clean)
  - [ ] L.5.6 - L.5.6 - BOSL2 examples corpus in the harness; perf-vs-oracle (mine + BOSL2) published
  - [x] L.5.7 - L.5.7 - warn-and-continue on missing resources (match OpenSCAD)
  - [x] L.5.8 - L.5.8 - assert failure exports pre-assert geometry (match OpenSCAD)
## Phase R - R - Generator + success-function search (perf + correctness fitness)
- [x] R.1 - R.1 - v0 perf success function: generate → rank programs by deterministic eval-cost → worst-case report
  - [x] R.1.1 - R.1.1 - surface a deterministic eval-cost metric (eval_steps) from fab_lang — a metered-eval entry that returns (result, steps)
  - [x] R.1.2 - R.1.2 - scad-gen: capture per-program eval-cost into the manifest (cost field), rank, expose a worst-case list
  - [x] R.1.3 - R.1.3 - perf report artifact (eval-cost histogram + top-N worst-case seeds) + a smoke test
- [ ] R.2 - R.2 - correctness differential: scad-rs vs OpenSCAD reference (success = divergence) — values/echo first, geometry gated on J.4.5 determinism
- [ ] R.3 - R.3 - v1 closed loop: score-guided search (evolve seeds/grammar-choices toward high-scoring inputs) — sampling → guided search
## Phase S - S - Cross-platform determinism (J.4.5): Manifold parallel determinism holds same-platform; verify cross-platform, then re-baseline
- [x] S.1 - S.1 - test A: native run-to-run determinism (MANIFOLD_PAR=ON, boolean-heavy models) — PASSED: 18 renders / 2 models, bit-identical every time
- [x] S.2 - S.2 - test B: native MANIFOLD_PAR=ON vs a PAR=OFF rebuild — confirm parallel ≡ serial output bit-for-bit
- [ ] S.3 - S.3 - test C: native (arm64) vs wasm cross-platform check — DEFERRED (needs a headless wasm mesh harness that doesn't exist yet; wasm is browser-only wasm32-uu). Predicted outcome: polyhedra match, curved primitives diverge on libm → collapses into libm-transcendental-divergence (fix = libm crate), NOT a Manifold issue
- [x] S.4 - S.4 - RESOLVED by the pure-Rust kernel (2026-07-19) — the C++ S.4 died at M.7.4. The reopened non-determinism was a C++-Manifold-CORE defect (atomic-slot races in disjoint-write assembly + a non-total-order `EdgePos` comparator, `boolean_result.cpp:197`), UNREACHABLE from outside the kernel — owning it in Rust is exactly what let us design the class out (the payoff W.3.9 predicted). `fab_manifold::par` is determinism-BY-CONSTRUCTION: rayon is clippy-banned outside par.rs (one door), a `CommutativeAssociative` compile-gate (non-associative float reduce WON'T COMPILE → `reduce_serial` Kahan), index-order `map_collect` (the serial/par crossover moves, never a byte), total-order sorts (`morton.then(idx)` — the M.4 tiebreak flag, landed), `SortGeometry` canonicalizes on POSITION before output so `mesh_id`/`tri_ref` are never emitted (the global `MESH_ID_COUNTER` atomic can't reach bytes; `build_geo_parts` is sequential regardless). VERIFIED two ways: (a) EMPIRICAL — garage_door + window_light_blocker + pill_holder + ashtray all bit-identical run-to-run, par on 16 cores (`tests/determinism_render.rs`, kept as the standing regression guard — determinism-by-construction is only as good as the proof no future edit opens a SECOND parallelism door); (b) AUDIT — a 5-lens adversarial Workflow (unordered-iteration / parallel-reduce / sort-tiebreak / global-atomic / float) × per-finding skeptical verify found 0 surviving over 6 candidates. Hardened the ONE non-total-order comparator the audit surfaced — `Solid::components()` (`bbox-min.then(num_tri)` → self-contained: both bbox corners + num_tri + num_vert + volume, all `total_cmp`) so a future PARALLEL `decompose()` can't reintroduce it; output-neutral on real models (no ties). Doctrine #36 holds same-platform run-to-run; cross-platform (native vs wasm libm) is S.3, a separate axis.
## Phase V - V - Multi-part parallelism (per-part render/slice/pack on independent worker threads; Solids stay thread-local, mesh data crosses)
- [ ] V.1 - V.1 - per-part parallelism: render/slice/print-layout each part on its own worker thread
## Phase Y - Y - Verification hardening: 100%-Rust re-derivation — shrink the unsafe surface, aim each tier where it uniquely covers
- [x] Y.1 - Y.1 - Recon: the verification-tier map (Workflow, wide)
- [x] Y.2 - Y.2 - Shrink the unsafe surface (delete-before-test)
- [x] Y.3 - Y.3 - Resurrect the lang fuzz campaign
- [ ] Y.4 - Y.4 - Re-aim ASan at the JIT (its unique target)
- [ ] Y.5 - Y.5 - Widen miri to the kernel unsafe
- [>] Y.6 - Y.6 - TSan / race detection for surviving Send/Sync + S.4
- [x] Y.7 - Y.7 - Fuzz the geometry-lowering seam (new target)
- [x] Y.8 - Y.8 - Audit + wire the kernel fuzz coverage
- [ ] Y.9 - Y.9 - Extend kernel fuzz coverage (csg_tree random-op + new op targets)

## Phase AA - Grammar gaps from the sustainment census: the 12-file parse bucket goes green
<!-- Source: the SU.7 bootstrap census (issue #1, 523/576) — the parse bucket, root-caused 2026-07-22:
     (1) modifiers on `if` — CONFIRMED by local probe: all four `* ! % #` fail on `if` specifically, while
         for/let/module-calls take them fine (L.3.2 built EVAL semantics only; the parser never accepted
         `if` as a modifiable instantiation — in OpenSCAD's grammar `if` IS a module instantiation);
     (2) issue1890 family — unterminated block comment / string / `include <` / `use <` at EOF: OpenSCAD
         warns-and-recovers, our lexer hard-errors;
     (3) linenumber.scad — whitespace-hostile include/use (`include\n< line 6 >`, spaces in paths, two
         directives on one line): their lexer takes everything to `>` literally;
     (4) issue4172 — deeply-nested vector literal (their stack-exhaust test): parser depth;
     (5) nbsp-latin1 — non-UTF-8 source, a doctrine call not a bug.
     OUT OF SCOPE (stay on the census worklist in issue #1): 10 assertion (semantics), 8 timeout (perf),
     18 load (text()/fonts + MCAD), 5 unimplemented (2D minkowski = J.3 follow-up). -->
- [x] AA.1 - Modifiers on `if` DONE: `StmtKind::If` grew a `Modifiers` field; `module_instantiation`'s prefix loop routes `if` to `ifelse_statement` (with the flags + true span start — `if` IS an instantiation per parser.y:271); eval mirrors `dispatch_module` EXACTLY (`!`→CaptureRoot, `*`/`%`→drop geometry AND side effects — the condition never evaluates, pinned by test; `#` render no-op); printer shares one `write_modifiers`. ORACLE-VERIFIED all four against OpenSCAD (incl. `!if` dropping the ancestor transform). Gates: 6 conformance parses (stacked + spaced mods), 5 geometry_corpus eval tests, **the 5 census modifier files sweep 5/5 clean**. Fuzzer (chotchki's add): gen's `if` arm now emits `* # % *# %*` prefixes at 20% (+ rare `!` at 2% — root-capture starves other statements' coverage), so the corner stays exercised
- [x] AA.2 - issue1890 DONE — and the ORACLE REWROTE the premise: upstream does NOT warn-and-recover on all four. Unterminated COMMENT and STRING are hard parse errors there too ("Unterminated comment"/"Unterminated string") — our rejection already matched, now pinned as verdict-parity tests. Unterminated `include <`/`use <` at EOF: upstream consumes to EOF and silently DISCARDS (per-newline warnings only, no resolution attempt) — our lexer now emits the token with the swallowed path (`opt('>')` replaces the cut_err); the loader's tolerated can't-open is the one observable delta (a warning where they have a different warning, same empty contribution). Census: 4 parse → 2 parse (correct rejections) + 2 load. Gates: conformance swallow-to-EOF test (1 stmt, sphere is path content), rejected() parity tests, lexer_corpus updated (its old unterminated-use/include-must-error assertions were pre-oracle)
- [x] AA.3 - Literal-to-`>` include/use paths DONE: inside `<…>` the ONLY terminator is now `>` (was: tab/cr/lf hard-errored). ORACLE-PINNED first: upstream parses linenumber.scad, warns "new lines in include<> is not defined - behavior may change" per newline, and keeps the LAST segment as the filename — since upstream declares the munging UNDEFINED, we keep the RAW slice instead (zero-copy; the can't-open warning shows exactly what the source said) — a documented divergence on an explicitly-undefined construct. Spaces-in-paths + `use <a> include <b>` same-line already worked (is_use_ws matched flex). linenumber.scad: parse→**parses clean**, now buckets `load` (its libraries deliberately don't exist — the oracle warns identically); 4 conformance parses pin the shapes
- [ ] AA.4 - Parser depth (issue4172): the deeply-nested vector literal parses without stack death — iterative expression spine or an explicit budget ≥ theirs (the M-phase heap-bounded doctrine, parser edition); fuzz already covers the lexer, add a depth-shaped corpus seed
- [ ] AA.5 - nbsp/Latin-1 doctrine call: match OpenSCAD's byte-lenient reads (lossy-decode at the fs seam, like the SU sweep does) or document the accepted divergence in docs/sustainment.md — DECIDE, implement or record, either closes the file
- [ ] AA.6 - Exit gate (bar CORRECTED by AA.2's oracle findings): the parse bucket must equal the VERDICT-PARITY set — files upstream ALSO rejects: issue1890-comment + issue1890-string (+ nbsp-latin1 pending the AA.5 call). Everything else parses. Local full-census re-sweep proves it; every fix is a pinned test; the next sustain nightly's report reflects the new baseline

## Backlog (not yet phased)

- **Evaluate the M.3.1 spectral-norm SHORTCUT (chotchki, 2026-07-14).** `Mat3::spectral_norm` uses deterministic power iteration on MᵀM (32 iters + IEEE sqrt) instead of porting Manifold's iterative Jacobi SVD (`svd.h`, ~304 LOC). Justified because `SpectralNorm` is used ONLY for `epsilon *= SpectralNorm` (a tolerance invisible to a transform's output geometry — positions/tris/normals are exact). REVISIT if: (a) a compound-op differential (`transform(x).union(y)`) fails on an epsilon-driven near-degenerate merge tracing to a spectral-norm ULP divergence vs C++, or (b) the M.6 native≡wasm bit-for-bit corpus sweep flags it. Neither bites ⇒ shortcut was worth it (~300 LOC of Jacobi SVD avoided); if it bites ⇒ port `svd.h` verbatim. (Task #4 logged; bridge id-collided with K.2 so tracked here.)
- **manifold-rs BUILDER REFACTOR — ergonomics pass once the port is proven (chotchki, 2026-07-14: "plan a second phase once we get a working port done, refactor into builders, so we know we have a good foundation").** Sequence AFTER the robustness core stabilizes (≥R2/M.2, when difference/intersection + edge_op + the nasty corpus land and the boolean assembly stops changing). Struct-group the many-arg assembly helpers (`append_partial/new/whole_edges` 6–9 args, `size_output` 10, `add_new_edge_verts` 8) into a `ResultAssembly` builder owning the shared mutable state (out/face_halfedges/face_ptr_r/whole_halfedge_*); helpers become methods. Deferred on PURPOSE: the C++ uses long-arg FREE fns for exactly these (only `DuplicateVerts`/`CountVerts` are functor structs), so grouping diverges from the transliteration — do it on a GREEN, validated baseline, not while the port is still being proven against the reference. Typed-index misuse-resistance (M.1.3.1) already shipped; this is the arg-COUNT half.
- **manifold-rs — reimplement the Manifold kernel in Rust (someday, its own multi-month phase)** — surfaced 2026-07-13 chasing the W.3.9 TBB yak. MEASURED: Manifold's core is ~13.3K non-comment LOC — SMALLER than fab-lang (~16.6K, the OpenSCAD evaluator we already reimplemented) — so a Rust port is the SAME class of bet as scad-rs, one layer down, NOT CGAL-scale. The hard part is a ~4K robustness core (boolean3/boolean_result/edge_op/face_op/impl/polygon: exact predicates + manifold-preserving topology surgery; a 95%-right port is worthless — fails exactly where naive booleans fail). DE-RISKER (chotchki spotted): Manifold ships an excellent test suite (~9K/338 cases) — a Google-fuzztest STRUCTURE-AWARE fuzzer (random CSG trees of transformed cubes + `intermediateChecks` manifold-invariant, ports to Rust `proptest`), manifold-invariant property tests (IsManifold/Volume/genus), a nasty-model corpus (self_intersect/Havocglass/Cray) — so port the TESTS first → inherit what "robust" means, with a DOUBLE oracle (bit-differential vs the C++ kernel + invariant properties; a better setup than scad-rs-vs-OpenSCAD had). Also needs Clipper2 (2D boolean) for cross-sections. Payoff: drop the C++ kernel ENTIRELY (no wasm-cxx-shim / -fno-exceptions / SharedArrayBuffer, own the parallelism + full determinism, one language). Greenlight AFTER W.3 ships + a scoped SPEC bounding which Manifold surface fab-scad actually exercises. The W.3.9 "own the TBB backend" spike is recon toward it. See memory [[manifold-rs-feasibility]].

- **Interlocking cut profiles for THIN-walled parts (dovetail/finger/jigsaw)** — surfaced 2026-07-13 dogfooding window_light_blocker whole-mode. The GUI slicer makes FLAT planar cuts + onion/bolt connectors, but a thin frame wall can't fit an onion (needs a sphere of material) OR a bolt (needs a screw hole + head clearance) — so a cut through a thin member has no way to join. The maker answer is to make the CUT ITSELF interlock: a 2D join profile (dovetail/finger/jigsaw) swept in the cut plane, so the interlock lives in-plane and needs no thickness (exactly what the model's own `partition_mask(cutpath="jigsaw")` export mode does). Feature: give GUI cuts an optional interlocking profile (sized to the local wall, both halves stay manifold), auto-picking dovetail for thin cross-sections and reserving onion/bolt for thick structural joins. OPEN design: GUI-generates-its-own-profile vs recognize/reuse the author's partition joints; auto thin→dovetail / thick→onion vs a per-cut choice. Related [[connector-design-intent]].

- **Presliced parts → separate top-level Parts (Option B)** — deferred from U.3.19. We shipped Option A (skip auto-slice when every component fits, keeping the `part = top-level source statement` invariant; presliced blob = 1 part · N pieces). B would promote each disconnected connected-component to its OWN Part (Part 1..N × 1 pc) — more literal "separate parts", but the investigation flagged real costs: breaks `part_names` provenance (N components vs 1 module name → count-mismatch nulls all names), runs union-find on EVERY model, and over-splits a legitimately-multi-body single part (base+lid you meant to join with GUI connectors). Revisit only if dogfooding shows the 1-part·N-pcs representation is genuinely worse than an N-parts tree.

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
- **Colored 3mf EXPORT: assemblies export per-part pieces as separate objects with extruder mapping (distinct color → Bambu AMS slot; extend bambu::Placed + model_settings extruder) — the other half of A.9's color carry-through** *(restored 2026-07-04)*
- B.6 - Customizer stretch: expose the .scad's top-level params in the panel, tweak → worker re-render *(deferred from phase `B` on 2026-07-03; scad-rs makes this an AST walk — fold into G when the evaluator lands)*
- **Safari deep-recursion fix, the real one: build openscad wasm ourselves (openscad-wasm docker recipe) with -sSTACK_SIZE=8MB+, test corner_brace.scad under JSC via safaridriver; if the bigger baked stack fixes it, swap the pin to our build; if not, it's JSC engine frames and upstream/WebKit territory** — added 2026-07-04; scad-rs kills this class by construction (explicit-stack evaluator), so only worth doing if G drags
- **GUI/UX next-major (from SPEC_workflow.md "Next Major Spec") — residue after the egui flip:** tabbed guided flow (Config/Loading/Planes/Plates) + DAG per-target animations aligned with 6.1. (The bevy_egui-replacing-feathers headline SHIPPED in Phase U; config unification landed as the `fab:config` line-comment in W.3.8/W.3.14; the fab-gui retirement question resolved into the W.3 one-codebase consolidation.)
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
- **SVG import: stroke-only (open/unfilled) paths — Clipper-offset by stroke-width/2** — added 2026-07-08.
- **SVG import: per-element even-odd grouping (vs pooled) — union elements with nonzero** — added 2026-07-08.
- **scad-rs import() base-dir is per-run, not per-containing-file (OpenSCAD divergence)** — added 2026-07-08.
- **Fast-path named args: map named→positional before intrinsic/JIT dispatch** — added 2026-07-09.
- **JIT-in-WASM may be viable — revisit the desktop-only-JIT assumption** — added 2026-07-09.
- **gui module imports: tighten `use crate::*` globs (U.4 refactor artifact) to explicit imports, then prune the uniform pub(crate) to the real cross-module surface** — added 2026-07-12.
- **Evaluate grcov swap: line-level coverage exclusion (GRCOV_EXCL_LINE) → parser/lexer gate back to 100%** — added 2026-07-12.
- **Y.6 - TSan / race detection for surviving Send/Sync + S.4** — deferred from Y.6 on 2026-07-19.

<!-- W.3.29 handoff (autonomous session, .6+.1 done; .2/.3/.4 need a live-dogfood session) -->
<!--
W.3.29.2/.4 blueprint (coverless first) — mirror the save-back, it does 90% of this:
  - Transport: generalize gui/src/web_host.rs::upload_multipart → take a METHOD + return (status, body).
    Add a form-urlencoded helper (URLSearchParams body + Accept: application/json) for the page endpoints,
    and reuse fetch_text/GET for the existence check. Parse the POST /media response `ref` with js_sys::JSON
    (serde_json is dev-only) keyed on contract::MEDIA_REF_FIELD.
  - Flow (new gui/src/publish_web.rs, wasm): dialog.confirmed → spawn a Task (save_action is the template):
    bake editor buffer → RenderWhole(full) + SaveMeshes on the wasm GeomPool → files=[source.scad, low.3mf,
    high.3mf] → POST contract::media_url → ref; (plate as a separate item if staged); markdown =
    contract::compose_markdown; slug = contract::slugify; GET contract::page_url (exists?) → POST
    contract::create_page_url if new → PUT contract::page_url (contract::PAGE_* fields). Cover: NONE for now.
  - Button: a wasm "Publish" on the Export tab (panel.rs), DISTINCT from the "Update" save-back. It opens
    the W.3.29.6 dialog (register publish_dialog on wasm too — drop its native-only gate). Loud on 401/403
    (not logged in as admin) via web_host's existing status mapping.
  - Auth: the ambient same-origin session cookie (RequestCredentials::SameOrigin) — no key, no Settings gear.
W.3.29.3 (cover on wasm): DEFERRED. save_to_disk is native-only; needs a render-target→PNG-bytes readback
  (Bevy Screenshot observer → image bytes → encode) that I couldn't verify headlessly. Coverless publishes
  fine (the site renders its own thumbnail, like `fab publish`). Add the cover once the coverless path
  dogfoods green, so the readback is the only new variable.
-->
