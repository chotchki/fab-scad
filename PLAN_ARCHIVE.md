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

---

## 2026-07-01

## Phase 10 - Manifold in-process kernel — spike (go/no-go)
- [x] 10.1 - Vet a Rust Manifold binding: build + boolean + STL export
- [x] 10.2 - Import the real Underdesk STL into Manifold; check robustness
- [x] 10.3 - In-process slab slice: parity + latency vs OpenSCAD
- [x] 10.4 - Multi-object 3mf export from Manifold meshes
- [x] 10.5 - Go/no-go writeup + scope Track C or park

---

## 2026-07-01

## Phase 11 - Track C: in-process Manifold geometry kernel
- [x] 11.1 - kernel module scaffold: manifold3d dep + typed Solid wrapper
- [x] 11.2 - STL import: weld to a valid manifold Solid
- [x] 11.3 - Export: Solid to binary STL + multi-object 3mf
- [x] 11.4 - In-process slab slicer: piece by multi-index
- [x] 11.5 - Slicer parity harness vs OpenSCAD
- [x] 11.6 - Connector solids in Rust: onion + bolt clearance
- [x] 11.7 - Apply connectors per piece, floater-free by construction
- [x] 11.8 - Connector parity vs the scad path
- [x] 11.9 - fab render/slice: in-process kernel path with OpenSCAD fallback
- [x] 11.10 - GUI reactive DAG on the cached base mesh
- [x] 11.11 - Corpus parity + latency validation; demote scad codegen; docs
- [x] 11.12 - Port the print-orientation preview to the kernel

---

## 2026-07-01

## Phase 12 - Bambu multi-plate export
- [x] 12.1 - fab_scad::bambu writer — multi-plate project .3mf
- [x] 12.2 - 2D bin-packer — oriented footprints to fewest bed plates
- [x] 12.3 - export_plates orchestration — orient, seat, pack, place, write
- [x] 12.4 - GUI "Export plates" action in the print view
- [x] 12.5 - Round-trip test + Bambu Studio validation

---

## 2026-07-01

## Phase 13 - Auto-slice & orient
- [x] 13.1 - auto_orient: stability tie-break — largest face down when overhang allows
- [x] 13.2 - auto_slice: bbox partition into bed-fit cells (equal division, overflowing axes only)
- [x] 13.3 - GUI Auto-slice button — seed cuts from model bbox + printer bed

---

## 2026-07-02

## Phase 14 - fab make: the one-shot auto pipeline
- [x] 14.1 - fab_scad::auto — plan() chaining auto_slice + connector auto-place (lib extraction)
- [x] 14.2 - GUI: auto-on-open for unsliced too-big models + Auto button on auto::plan
- [x] 14.3 - CLI: fab make <model> — render, plan, slice, orient, export a Bambu project
- [x] 14.4 - fab make end-to-end dogfood + docs
- [x] 14.5 - Adaptive onion placement — a few alignment guides, spread + scaled to face

---

## 2026-07-02

## Phase 15 - Publish to hotchkiss.io
- [x] 15.1 - Manifest [publish] section + fab_scad::publish client (auth, slugify, upload, page CRUD, retry)
- [x] 15.2 - publish orchestration: gather artifacts (cover, low-fn viewer STL, full STL, 3mf), compose markdown, idempotent create+update
- [x] 15.3 - fab publish CLI command — API key (env/flag) + base URL, render artifacts, publish
- [x] 15.4 - GUI Publish button
- [x] 15.5 - Publish end-to-end dogfood against hotchkiss-io + docs

---

## 2026-07-02

## Phase 16 - Bolt: bound through-depth + teardrop shaft
- [x] 16.1 - Bolt: bound through-depth to the above-slab thickness (kernel slice_solid)
- [x] 16.2 - Bolt: teardrop the shaft + counterbore for support-free horizontal holes (build-up aimed)
- [x] 16.3 - Bolt: tests (through-depth spans slab, teardrop self-supports) + scad bolt_joint parity

