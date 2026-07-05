# PLAN

PIVOTED 2026-07-04: scad-rs — a GPL Rust implementation of the OpenSCAD language over the
Manifold kernel (SPEC.md, in drafting). The workflow-tool plan this file used to hold is
archived + its SPEC lives on as SPEC_workflow.md; every non-G item is parked in Backlog, not
dead. Cardinal rule unchanged: nothing deleted before it's archived AND validated.

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->
## Phase H - scad-rs: the whole grammar
- [ ] H.1 - Grammar inventory: bison file → conformance checklist doc (every production accounted for)
  - [x] H.1.1 - grammar-inventory.md: every parser.y production + lexer.l rule → {AST node, parser fn, status, conformance anchor}; the matrix H.5's suite derives from
  - [x] H.1.2 - Lexer completeness audit vs lexer.l: confirm hex/float/escapes/unicode/$-idents/digit-idents/EOT/operators all covered; document the DELIBERATE divergences (comments preserved, zero file-IO in the lexer)
- [ ] H.2 - Statements/items (parse-only): module def, function def, if/else, use/include → AST — the 4 genuinely-new constructs; for/intersection_for/let/each/assert/echo ALREADY parse as module calls (their semantics are I.2/I.3)
  - [x] H.2.1 - Parameter type + params-list parser (id | id=default, trailing comma) — shared by module def, function def, and the function-literal expr
  - [x] H.2.2 - Module def: `module id(params) statement` → StmtKind::ModuleDef (body is one statement, usually a block)
  - [x] H.2.3 - Function def: `function id(params) = expr;` → StmtKind::FunctionDef
  - [x] H.2.4 - if/else in the module_instantiation path: dangling-else (%prec NO_ELSE), else-if chains, works in child position for free (translate() if(x) cube();)
  - [x] H.2.5 - use/include → AST nodes (parse-only, zero-IO); resolution/splice is I.2's loader; the evaluator stays LOUD-deferred on these nodes until then
  - [x] H.2.6 - Conformance nicety: child_statements ⊂ inner_input (module/function DEFS illegal inside a module-call child block) — tighten block() or consciously defer
- [ ] H.3 - Expressions complete: list comprehensions (every form), ranges, function literals, ternary, string escapes/unicode
  - [x] H.3.1 - Extend the non-recursive Drop + MAX_DEPTH guards for every new recursive node (the Safari-cliff discipline — do the pattern once, here)
  - [x] H.3.2 - List-comprehension elements: LcFor, LcForC (C-style for(init;cond;next)), LcEach, LcLet, LcIf/else, parenthesized _p, arbitrary nesting
  - [x] H.3.3 - Function-literal expr: `function(params) expr` → ExprKind::FunctionLiteral
  - [x] H.3.4 - let-expression: `let(args) expr` → ExprKind::Let
  - [x] H.3.5 - assert/echo expressions with OPTIONAL trailing expr (expr_or_empty): assert(args) expr?, echo(args) expr?
  - [x] H.3.6 - Ranges + string-escape/unicode: audit + pin with tests (already implemented in G.3.3 + the lexer — confirm, don't rebuild)
- [ ] H.4 - Customizer annotations survive: parameter comments/groups/ranges in the AST (lossless-enough)
  - [x] H.4.1 - Customizer annotation model: group / description / widget-constraint (range, step, dropdown k:v, string maxlen) types in the AST
  - [x] H.4.2 - Trivia-association pass: walk Lexed::all, bind trailing line-comment + active group header to each top-level assignment (top-of-file scope, per OpenSCAD)
  - [x] H.4.3 - Constraint mini-grammar parser: [min:max], [min:step:max], [v,…], [k:label,…], [maxlen]; group headers incl. [Hidden]/[Global]
  - [x] H.4.4 - Customizer lossless-enough roundtrip test: annotations survive parse → (edit a value) → emit
- [ ] H.5 - proptest print/parse roundtrip + the bison-derived conformance suite green
  - [x] H.5.1 - Pretty-printer: AST → canonical OpenSCAD source (Display over the whole AST) — the missing prerequisite for the roundtrip property
  - [x] H.5.2 - proptest strategy over the AST + print→parse→assert-equal property (structural eq modulo spans)
  - [x] H.5.3 - Bison-derived conformance suite: one+ example per production from grammar-inventory.md, all green — fills the doc's H.5.3 anchor holes
  - [ ] H.5.4 - cargo-mutants gate on the parser (backlog #37) — prove the tests CATCH bugs, kill survivors
- [ ] H.6 - cargo-fuzz target + SCHEDULED CI fuzz job + persisted/minimized corpus + trophy log (fuzz-from-first-commit doctrine starts here, not later)

  - [x] H.6.1 - cargo-fuzz target: parse(arbitrary bytes) never panics/hangs/OOMs — wire the fuzz crate + the parse harness
  - [x] H.6.2 - Fuzz seed corpus: extend the lexer seed set to the parser + a structure-aware corpus from H.5's generator
  - [x] H.6.3 - Scheduled CI fuzz job + persisted/minimized corpus artifact (the fuzz-from-first-commit doctrine)
  - [x] H.6.4 - TROPHIES.md doctrine: every fuzz-found bug logged + regression-pinned as a test
## Phase I - scad-rs: evaluator core
Meta - I've been looking into cranelift and it looks like it would be VERY approachable for us leverage for execution instead of going the interpreter route
- [ ] I.1 - Value model full: enum + NumList fast path + interned strings + lazy ranges; fast==slow BITWISE property via the shared fixed 4-lane accumulation order
- [ ] I.2 - Scoping engine: lexical envs, dynamic $-variables, children()/late binding, module+function call machinery on the explicit stack; + the use/include LOADER (file resolution + include-splice + use-import — parser stays zero-IO, this is where H's use/include AST nodes get resolved)
- [ ] I.3 - Control flow + comprehensions + recursion bounded by memory — corner_brace-class deep recursion as the standing regression proof
- [ ] I.4 - Builtin function library (~80: math/list/string/type predicates), each landing with its semantics/ test
- [ ] I.5 - undef propagation + warning/echo text bug-for-bug (string-equal vs oracle)
- [ ] I.6 - tracing spans on the call path + aggregating benchmark layer; release builds compile it out; overhead measured
- [ ] I.7 - Kani proofs: stack-machine push/pop discipline, range-iteration termination

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
