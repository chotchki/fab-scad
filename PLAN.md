# PLAN

PIVOTED 2026-07-04: scad-rs — a GPL Rust implementation of the OpenSCAD language over the
Manifold kernel (SPEC.md, in drafting). The workflow-tool plan this file used to hold is
archived + its SPEC lives on as SPEC_workflow.md; every non-G item is parked in Backlog, not
dead. Cardinal rule unchanged: nothing deleted before it's archived AND validated.

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->
## Phase G - scad-rs: the OpenSCAD language in Rust over Manifold
- [x] G.1 - Relicense + pivot mechanics: GPL-2.0-or-later (OpenSCAD's EXACT license, chosen for zero-friction upstreaming) across LICENSE + 4 crate manifests + README/NOTICE/web-bundle docs; SPEC.md → SPEC_workflow.md; PLAN restructured — all non-G work backlogged with provenance, phases 5/6/17/18/C archived
- [x] G.2 - SPEC.md round 1 (drafted WITH chotchki): mission + license stance, architecture (parser / explicit-stack evaluator / value model / Manifold+Clipper2 geometry mapping), the BOSL2 strategy rungs (fast evaluator → pin-verified intrinsics → JIT-if-earned), oracle + differential testing design, and the testing/formal-verification approach (proptest, differential fuzzing, Kani-scoped proofs, intrinsic equivalence protocol)
- [ ] G.3 - Tracer bullet: lang/ crate (winnow lexer/parser core: literals, exprs, module calls, $args) + explicit-stack evaluator skeleton + lower sphere()/cube() to kernel::Solid + differential harness v0 vs oracle CLI — verify the deterministic-output flag (spec Q7), run the strictest-metric experiment (sphere $fn=8 → high-poly), document the metric GATE per model class, first semantics/ tests land
- [ ] G.4 - Parser to full grammar: whole OpenSCAD language on winnow (modules, functions, comprehensions, let/each/assert/echo, ranges, string escapes, use/include) + bison-derived conformance tests + proptest print/parse roundtrip + cargo-fuzz target with a SCHEDULED CI fuzz job from this commit (fuzzing-as-infrastructure doctrine) + customizer annotations preserved in the AST
- [ ] G.5 - Evaluator core: enum values + NumList fast path (fixed 4-lane accumulation order, fast==slow bitwise property), lexical + dynamic $-scoping, children()/late binding, control flow, list comprehensions, recursion on the explicit stack, undef propagation bug-for-bug, tracing spans on the call path (compiled out in release) — Kani proofs on the stack machine discipline land here
- [ ] G.6 - Builtin geometry surface + lowering: primitives, multmatrix, booleans, polyhedron, hull, linear/rotate_extrude + offset + projection via Clipper2, import() through our STL/3MF readers, content-addressed CSG-node cache (in-memory tier) — text/minkowski/surface = LOUD deferred stubs; geometry-backend trait lands here (miri-on-mock + ASAN-on-Manifold interface tests)
- [ ] G.7 - semantics/ corpus + differential harness v1: segmented provenance-annotated semantics tests (oracle behavior + src/core line per decision), corpus tiers wired in CI (OpenSCAD test suite, BOSL2 tests, models/), ChaCha8-seeded grammar-directed program generator v0, seeds logged per run, trophy log started
- [ ] G.8 - The BOSL2 gauntlet (phase exit gate): run the pinned BOSL2 test suite + our models/ through scad-rs, burn down divergences to zero-or-documented; benchmark corpus captured via the tracing layer on every run (rung 2's data exists before rung 2 starts); exit = smoke corpus green end-to-end (teardrop/onion/screw_hole + corner_brace + Underdesk) — rungs 2/3 (intrinsics, JIT) phase as H from these numbers

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
