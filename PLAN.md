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
- [ ] 5.2 - Emit the slicing spec that scad-lib/fab consume; round-trip it through `fab render`
- [ ] 5.3 - Grow into a friendly workflow front-end (cut the verb-memorization tax)

## Phase 6 - fab render + output (3mf, magnets, Bambu)
- [ ] 6.1 - fab render: pieces as independent parallel jobs across cores; per-piece thumbnails
- [ ] 6.2 - Incremental rebuild: skip pieces whose inputs are unchanged (content hash)
- [ ] 6.3 - Multipart 3mf: export pieces as SEPARATE objects on a plate (lazy-union); verify separation
- [ ] 6.4 - Embedded magnets: clean split around cavities + pause-at-layer in the 3mf
- [ ] 6.5 - Investigate Bambu 3mf settings embedding (plate/material/pause) for one-click print; adopt only if clean
- [ ] 6.6 - Demote import() crutch to optional freeze-source-once; DAG resolver as fallback only

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
