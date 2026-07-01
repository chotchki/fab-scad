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
  - [ ] 5.3.9 - Bolt: bound through-depth + wire teardrop shaft in slicer
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
## Phase 11 - Track C: in-process Manifold geometry kernel
- [ ] 11.1 - geo module scaffold: manifold3d dep + typed Solid wrapper
- [ ] 11.2 - STL import: weld to a valid manifold Solid
- [ ] 11.3 - Export: Solid to binary STL + multi-object 3mf
- [ ] 11.4 - In-process slab slicer: piece by multi-index
- [ ] 11.5 - Slicer parity harness vs OpenSCAD
- [ ] 11.6 - Connector solids in Rust: onion + bolt clearance
- [ ] 11.7 - Apply connectors per piece, floater-free by construction
- [ ] 11.8 - Connector parity vs the scad path
- [ ] 11.9 - fab render/slice: in-process kernel path with OpenSCAD fallback
- [ ] 11.10 - GUI reactive DAG on the cached base mesh
- [ ] 11.11 - Corpus parity + latency validation; demote scad codegen; docs

## Backlog (not yet phased)

- **fab owns $fn: inject draft/final quality + strip `$fn = $preview ? …` from all scad model files** — added 2026-06-28.
