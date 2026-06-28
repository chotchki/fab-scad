## 2026-06-28

## Phase 0 - Safety net (no deletions until this is green)
- [x] 0.1 - Confirm NAS reachable + capacity; pick cold-archive root (hotchkiss.io:/Volumes/NAS/3d_print/_cold_archive)
- [x] 0.2 - Full immutable snapshot of current ~7.6 G to NAS (done manually)
- [x] 0.3 - Verify the manual archive is complete + intact (checksums); record an inventory of what's where


## Phase 1 - Git backup (fab-scad MIT tool repo + scad-models designs repo)
- [x] 1.1 - Author .gitignore (out/, target/, BOSL2.wiki, downloaded models, .DS_Store)
- [x] 1.2 - git init / first commit of source into scad-models (git@github.com:chotchki/scad-models.git)
- [x] 1.3 - Push both repos (fab-scad MIT done; scad-models stays PRIVATE for now); verify a fresh clone is small/fast
- [x] 1.4 - Apply CC BY-NC-SA 4.0 to scad-models (LICENSE + headers + README terms); then decide public
- [x] 1.5 - Root README (workflow overview, how the pieces fit)


## Phase 2 - fab-scad skeleton + BOSL2 pin + standardization + shared SCAD lib
- [x] 2.1 - Stand up fab-scad superproject skeleton (clone repo, layout: libs/ scad-lib/ printers.toml, MIT)
- [x] 2.2 - Pin BOSL2 submodule under fab-scad/libs to latest tag (v2.0.746); track tagged releases, bump deliberately
- [x] 2.3 - Canonical include mechanism (OPENSCADPATH up into fab-scad so BOSL2/std.scad resolves); document it
- [x] 2.4 - Standardize include paths across projects to canonical <BOSL2/...> form (scripted)
- [x] 2.5 - Inventory pass: which projects fail to compile at the pin via .csg eval (report only)
- [x] 2.6 - Pin other third-party libs as submodules (gridfinity_extended, machineblocks)
- [x] 2.7 - Stand up scad-lib in fab-scad (MIT): version-stamp + part-numbering modules


---

## 2026-06-28

## Phase 3 - fab foundation (workflow layer + OpenSCAD wrap spike)
- [x] 3.1 - fab repo + CI + clap skeleton; `fab doctor` (openscad/manifold/NAS/pins/submodules)
- [x] 3.2 - Dogfood the OpenSCAD integration pattern: headless render, preview, geometry I/O (the wrap fab + GUI build on)
- [x] 3.3 - `fab focus <project>` active-project context (no name on every command)
- [x] 3.4 - Parse a MINIMAL project.toml (name/title/part); schema grows by dogfooding
- [x] 3.5 - `fab new <name>` scaffolds from the template
- [x] 3.6 - Wire scad-models + libs + scad-lib as pinned submodules under fab-scad


---

## 2026-06-28

## Phase 4 - Linear slicing (SCAD lib; kill the 2^N blowup)
- [x] 4.1 - Characterize blowup: confirm nested partition() ~2^N; quantify on window_light_blocker + shoe_holder
- [x] 4.2 - Linear slicer module in scad-lib: planar slab cuts, piece = source ∩ slab (child once per piece)
- [x] 4.3 - Cut minimization: fit by rotation/diagonal against printer bed before cutting (printers.toml)
- [x] 4.4 - Connector lib: heat-set insert + M bolt (default) and teardrop pin + glue; reuse BOSL2 screw_hole/nut_trap; harvest insert specs
- [x] 4.5 - Per-piece print orientation drives connector orientation so all features print support-free
- [x] 4.6 - Auto-place connectors on a face + manual override (shared across connector types)
- [x] 4.7 - Test coupons: emit a sample joint to tune slop before full prints
- [x] 4.8 - Dimensional-integrity check: reassembled pieces == original within tolerance (no shrink)

- [x] 4.9 - Family logo stamp module (scad-lib, BOSL2 attachable)

