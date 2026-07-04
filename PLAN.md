# PLAN

PIVOTED 2026-07-04: scad-rs — a GPL Rust implementation of the OpenSCAD language over the
Manifold kernel (SPEC.md, in drafting). The workflow-tool plan this file used to hold is
archived + its SPEC lives on as SPEC_workflow.md; every non-G item is parked in Backlog, not
dead. Cardinal rule unchanged: nothing deleted before it's archived AND validated.

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->
## Phase G - scad-rs bootstrap: pivot + spec + tracer bullet
- [x] G.1 - Relicense + pivot mechanics: GPL-2.0-or-later (OpenSCAD's EXACT license, chosen for zero-friction upstreaming) across LICENSE + 4 crate manifests + README/NOTICE/web-bundle docs; SPEC.md → SPEC_workflow.md; PLAN restructured — all non-G work backlogged with provenance, phases 5/6/17/18/C archived
- [x] G.2 - SPEC.md rounds 1-2 (drafted WITH chotchki): mission + license stance, architecture, BOSL2 rungs, determinism doctrine, testing/verification layers — all open questions resolved or scheduled (winnow, enum values, Kani-low-level, semantics/ segmented, lang/ sibling, tracing full-trace)
- [ ] G.3 - Tracer bullet: sphere-vs-oracle end to end, metric gate chosen from data
  - [ ] G.3.1 - lang/ crate scaffold: workspace sibling, error type, tracing dep (compiled-out default), clippy-pedantic baseline, CI lane (fmt/clippy/test)
  - [ ] G.3.2 - winnow lexer: tokens, numbers/strings/identifiers, comments PRESERVED (customizer needs them later); every named parser wrapped in winnow trace() from day one (debug-feature-gated, zero cost off); lexer fuzz seed corpus started
  - [ ] G.3.3 - parser core: expression precedence, module instantiation, argument lists incl. $-args; AST with source spans (LocatingSlice + .with_span()); winnow-native errors from production one — StrContext label+expected everywhere, cut_err at commit points, caret rendering from the context stack
  - [ ] G.3.4 - evaluator skeleton: explicit-stack machine over the subset; Value v0 (Num/Bool/Str/NumList/Undef); $fn/$fa/$fs resolution
  - [ ] G.3.5 - lower sphere()/cube()/cylinder() to kernel::Solid — tessellation EXACTLY matching src/core primitives (ring/segment math ported, provenance noted)
  - [ ] G.3.6 - oracle runner: drive the openscad CLI, capture mesh + echo; VERIFY the deterministic-output flag (spec Q7) — what it sorts, what it doesn't
  - [ ] G.3.7 - metric experiment: implement the comparison tiers (quantized vertex-multiset, vol/area/Euler, boolean residual); sphere $fn=8→256 matrix; DOCUMENT the gate per model class back into SPEC.md
  - [ ] G.3.8 - first semantics/ tests land (provenance-annotated from G.3.5's port)

## Phase H - scad-rs: the whole grammar
- [ ] H.1 - Grammar inventory: bison file → conformance checklist doc (every production accounted for)
- [ ] H.2 - Statements/items: assignments, module defs, function defs, use/include resolution, if/else, for/intersection_for, let/each, assert/echo
- [ ] H.3 - Expressions complete: list comprehensions (every form), ranges, function literals, ternary, string escapes/unicode
- [ ] H.4 - Customizer annotations survive: parameter comments/groups/ranges in the AST (lossless-enough)
- [ ] H.5 - proptest print/parse roundtrip + the bison-derived conformance suite green
- [ ] H.6 - cargo-fuzz target + SCHEDULED CI fuzz job + persisted/minimized corpus + trophy log (fuzz-from-first-commit doctrine starts here, not later)

## Phase I - scad-rs: evaluator core
- [ ] I.1 - Value model full: enum + NumList fast path + interned strings + lazy ranges; fast==slow BITWISE property via the shared fixed 4-lane accumulation order
- [ ] I.2 - Scoping engine: lexical envs, dynamic $-variables, children()/late binding, module+function call machinery on the explicit stack
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
