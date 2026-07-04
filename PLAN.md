# PLAN

3D print workflow, backup, and showcase. Derived from SPEC.md (round 2). Infra-first:
foundation → prove on 2-3 pilots → migrate the rest. Cardinal rule: nothing is deleted
before it's archived AND validated. Driver: LOW mental burden (focus a project, reuse
shared parts, generate output).

<!--
Driven by `claude-plan-bridge` (FORMATv2). Hand-authored; run
`claude-plan-bridge baseline` after a rewrite to resync the state file.
-->

## Phase 5 - Slicer / workflow GUI (EARLY; dogfood the OpenSCAD wrap)
- [ ] 5.1 - GUI MVP: load model, set cut planes, click a face to place pegs/connectors, preview piece-vs-bed + orientation
  - [x] 5.1.1 - Sim-interaction test harness: scripted input → real systems → screenshot
  - [x] 5.1.2 - Multi-cut + per-cut axis: set cut lines, rotate/pick the plane
  - [x] 5.1.3 - Face-pick connector placement: click model → drop bolt/pin on the cut
    - [x] 5.1.3.1 - Manual face-pick: click model → drop a connector on the nearest cut (build first)
    - [x] 5.1.3.2 - BOSL2 onion connector (support-free), replacing pin/dowel
    - [x] 5.1.3.3 - Per-piece print-orientation UI → derive connector orientation
    - [x] 5.1.3.4 - Cross-section-driven auto-size + auto-place connectors
    - [x] 5.1.3.5 - Per-cut 2D cross-section connector editor: button on a cut → see its profile → pick connectors on it
- [x] 5.2 - Emit the slicing spec that scad-lib/fab consume; round-trip it through `fab render`
- [ ] 5.3 - Grow into a friendly workflow front-end (cut the verb-memorization tax)
  - [x] 5.3.1 - Directory/file picker: rfd "Open" button → choose a project dir or .scad (retire CLI-arg-only entry)
  - [x] 5.3.2 - File-list side panel: FileList resource (Vec<PathBuf> + active); click a row to switch (SceneCfg.source stays the scalar active pointer — lower blast radius)
  - [x] 5.3.3 - File-watch: mtime-poll on the active .scad → auto re-render on save (open-file only; include-graph gap → 6.6)
  - [x] 5.3.4 - Panel UX pass (dogfooding): full-focus mode-aware panel (View / Connectors / Print — hide controls that don't apply, in-mode Done); scroll-bound the file list + orbit yields over the panel; fix cross-section Y-flip (OpenSCAD negates SVG Y → auto-place scattered connectors below the model)

  - [x] 5.3.5 - Connector type picker (onion/bolt) in GUI editor
  - [x] 5.3.6 - Split view: dock panel, inset 3D camera viewport
  - [x] 5.3.7 - Onion feasibility follows build axis, not cut axis
  - [x] 5.3.8 - Bound onions to teardrop tip, not sphere radius
  - [x] 5.3.9 - Bolt: bound through-depth + wire teardrop shaft in slicer
  - [x] 5.3.10 - Seat the loaded model on the bed (Z-floor)
- [x] 5.4 - Track B: reactive render DAG (dogfood-driven)
  - [x] 5.4.1 - Include/use dependency resolver (fab_scad::deps)
  - [x] 5.4.2 - Include-graph file-watch (closes 5.3.3 gap)
  - [x] 5.4.3 - Reactive auto-reslice: no Re-slice button
  - [x] 5.4.4 - Loading pulse while recomputing
## Phase 6 - fab render + output (3mf, magnets, Bambu)
- [ ] 6.1 - Render engine: enumerate targets → parallel (rayon) render → report; a "target" is any .scad→out unit (pieces/parts/projects collapse to target sets); per-target thumbnail + N/M progress
- [x] 6.2 - Incremental rebuild: skip pieces whose inputs are unchanged (content hash)
- [x] 6.3 - Multipart 3mf: export pieces as SEPARATE objects on a plate (lazy-union); verify separation
- [ ] 6.4 - Embedded magnets: clean split around cavities + pause-at-layer in the 3mf
- [ ] 6.5 - Investigate Bambu 3mf settings embedding (plate/material/pause) for one-click print; adopt only if clean
- [ ] 6.6 - Demote import() crutch to optional freeze-source-once; DAG resolver as fallback only
- [x] 6.7 - Smoke oracle: a render "passes" iff OpenSCAD exits 0 AND mesh face-count > 0 (fast, no goldens; parity-vs-archived deferred to 8.4)
- [x] 6.8 - `fab render --all [PATH]`: walk the tree, enumerate every .scad, parallel smoke-render, print a pass/fail summary (the correctness sweep; needs no manifests)
- [x] 6.9 - Wire `.fab/focus`: `fab render` with no arg renders the focused project's parts (needs one minimal project.toml; scaffold from the existing template)

## Phase 7 - Web + publish
- [ ] 7.1 - STL decimation for the Three.js viewer (poly budget)
- [ ] 7.2 - Build cover image + description bundle matching hotchkiss.io content model
- [ ] 7.3 - Add API-key auth + publish endpoint to hotchkiss.io (passkeys stay for humans)
- [ ] 7.4 - `fab publish`: get one project live on hotchkiss.io/projects

## Phase 8 - Pilot migration (2-3 showcase projects; dogfood the schema)
- [ ] 8.1 - Confirm pilots (shoe_holder, keyboard_tent, nail_polish_holder)
- [ ] 8.2 - Migrate each into the new structure + minimal project.toml; DOGFOOD what fields are really needed
- [ ] 8.3 - Apply scad fixes; validate the correct output version; render via fab and confirm parity with archived output
- [ ] 8.4 - Prune redundant old versions LOCALLY (safe: archived + validated); publish to website
- [ ] 8.5 - Retro: fold pilot lessons back into template/manifest/tool

## Phase 9 - Reorg convention + incremental migration
- [ ] 9.1 - Lock the fab-scad-owned folder convention (libs/scad-lib/models submodules, excluded outputs, NAS archive)
- [ ] 9.2 - Triage remaining ~59 projects (mine / third-party / downloaded / dead) into a migration backlog
- [ ] 9.3 - Migrate remaining projects opportunistically (backlog)
## Phase 17 - auto-slice/pack v2: kernel cross-sections + rotate-to-fit
- [x] 17.1 - Kernel cross-section: Solid::cross_section(axis, at) via slice_at_z (drop the OpenSCAD spawn)
- [x] 17.2 - Swap auto::plan + GUI connector editor onto the kernel cross-section (keep OpenSCAD as parity oracle)
- [x] 17.3 - Rotate-to-fit auto-slice: score candidate rotations (incl 45°) by piece count, pick fewest
- [x] 17.4 - Wire rotate-to-fit into auto::plan / fab make / GUI auto-on-open
- [x] 17.5 - Phase 17 tests + parity (kernel vs OpenSCAD cross-section; rotate-to-fit reduces pieces) + dogfood
- [ ] 17.6 - GUI auto-on-open rotate-to-fit: re-orient loaded model + thread rotation through reslice/export
## Phase 18 - Deployment spike: DMG/winget vs wasm on hotchkiss.io
- [x] 18.1 - Native: cargo-packager multi-binary config → local unsigned .app + DMG; Bevy asset-path fix; app launches from /Applications
- [x] 18.2 - Native: drafted then PARKED (web-first, 2026-07-03) — release-native.yml kept, manual-dispatch only; winget manifests drafted; signing bill in docs/packaging.md; resume via backlog
- [x] 18.3 - Wasm: `native` feature seam — lib compiles on wasm32-unknown-unknown with openscad/publish/reqwest gated off (pure modules + STL bytes green)
- [x] 18.4 - Wasm kernel gate (GO/NO-GO): manifold-csg `unstable-wasm-uu` — Solid boolean + slice_at_z under wasm-bindgen in a browser; npm-bridge fallback assessment if no
- [x] 18.5 - Wasm GUI gate: fab-gui via bevy_cli on wasm — feathers render (bevy#22620: WebGL2 vs WebGPU), mesh_picking, rfd pick_file→bytes
- [x] 18.6 - Wasm hosting gate: hotchkiss-io special page serving the fab wasm bundle as a build-time artifact (full-page document, NOT an iframe) — COOP/COEP on the app document, precompressed bytes, wasm out of CompressionLayer; crossOriginIsolated proven
- [x] 18.7 - Decision memo → SPEC.md: pick primary mode; web = standalone client-only auto-slicer, zero server-side outputs (decided 2026-07-03; STL-upload-first, openscad-wasm stretch); spawn the build-out phase
- [x] 18.8 - fab-web bundle contract: GitHub-release artifact (tar.gz: ES-module glue + wasm + br/gz + manifest.json; tailwind-style pinned fetch) — contract doc + spike bundle handed to the 18.6 gate
- [ ] 18.9 - crates.io channel: claim the free `fab-scad` name — fix package contents (exclude models/spikes/docs), `cargo publish --dry-run` clean, then publish 0.1.0 (cargo install = third distribution channel, source-build tradeoff documented)
## Phase C - fab-web beta feedback
- [x] C.1 - Busy pulse + staged sync work: animated "rendering {name} (OpenSCAD)" while the worker runs; "slicing…"/"packing…" labels armed 2 frames ahead so they PAINT before the main-thread block; all completions clear to a real status (the desktop loading-pulse standard, ported)
- [x] C.2 - Geometry worker (fab-geom): a second SMALL wasm (kernel-only, no bevy, ~1 MB) in its own web worker runs weld/plan/slice/export over mesh-bytes postMessage — the !Send Solid contract as designed; makes the C.1 slice/export labels a LIVE pulse instead of a painted-then-frozen one (A.8 measured 5-10 s block on a 119k-tri part)
- [ ] C.3 - Printer selection: preset cycle button (A1 mini / P1-X1 / MK4 / Ender 3 / Voron 350) + localStorage persistence (fab-web.bed) — no hardcoded 256³; changing printer re-plans the loaded part in the background (reactive standard, live pulse); ?bed= deep-link still wins at startup
- [ ] C.4 - Adversarial review of C.2/C.3 (40-agent workflow: 4 lenses × 2-skeptic verify) + fixes: pick/render polls queue behind in-flight geometry (single-flight bypass = crossed worker replies), id-matched persistent worker transport + onerror (404'd worker script errored visibly instead of eternal pulse), Part.raw commits only on Analyze success, worker-init retry, ?bed= clamp, queued printer clicks

## Backlog (not yet phased)

- **fab owns $fn: inject draft/final quality + strip `$fn = $preview ? …` from all scad model files** — added 2026-06-28.
- **Showcase→slicer deep-link: project page hands its published STL into the slicer special page (same-origin fetch, COEP-safe) — publish-side wiring + slicer URL param** — added 2026-07-03.
- **Resume the native channel: dispatch release-native.yml (mac DMG + Windows NSIS artifacts), fill winget InstallerSha256 from the release, decide the signing purchases (docs/packaging.md)** — added 2026-07-03.
- **Colored 3mf EXPORT: assemblies export per-part pieces as separate objects with extruder mapping (distinct color → Bambu AMS slot; extend bambu::Placed + model_settings extruder) — the other half of A.9's color carry-through** — added 2026-07-03.
- **fab-web wire-size stretch: below 8.5 → ≤7 MiB brotli needs build-std (opt-size std, panic=abort) and/or naga shader stripping — feature-level surgery is EXHAUSTED (measured: meta-group trim ~1 MB, granular assembly ~0.2 MB; the weight is bevy_render/wgpu/naga)** — added 2026-07-03.
- B.6 - Customizer stretch: expose the .scad's top-level params in the panel, tweak → worker re-render (defer if B.1-B.5 drag) *(deferred from phase `B` on 2026-07-03)*
