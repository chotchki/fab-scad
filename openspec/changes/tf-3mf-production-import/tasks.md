# Tasks

## TF. 3MF import reads Production-extension files (Bambu Studio / MakerWorld)

- [ ] TF.0 Phase exit: every box below ticked; every ci.yml job green on the landing commit (boot-gate builds the wasm geometry worker, which compiles this reader); no out-of-CI e2e lane touches 3MF import (`e2e-save.sh` exercises save-back, not import), so none to run; the `import/3mf` spec synced into `openspec/specs/` at archive, which makes it the owning doc
- [ ] TF.1 `zip` declares `features = ["deflate"]` in its own right (`Cargo.toml`), and its comment stops saying "Stored only". Verify with `cargo tree -e features -i zip`, which shows fab-scad itself requesting `deflate`, not only threemf
- [ ] TF.2 The package reader, built beside the old read path: `_rels/.rels` start-part resolution with the `3D/3dmodel.model` fallback, and part names normalized (leading `/` stripped, ASCII case-insensitive). Each part is parsed lazily and memoized, streamed with `read_event_into` on LOCAL names into objects (mesh, components with `p:path`, `pid`/`pindex`), build items and basematerials; metadata is ignored. Unit tests in the same box, with synthetic fixtures only:
  - metadata before `<resources>` AND after `</build>` (the reported error, so it must fail on today's reader)
  - start part at `/3D/main.model` named by `.rels`
  - no `.rels`
  - DEFLATE-compressed entries
  - a transform without 12 values, and a non-numeric coordinate, each erroring with the element named
- [ ] TF.3 Resolution: objects looked up by `(part, id)`; a component path defaults to its own part; only the root part's build seeds it; parent∘child through the existing `compose`/`apply`, with a comment citing upstream `import_3mf_v2.cc:135` (0,0,1 default) and `:145` (`cm * m`) and pointing at the spec; colors resolve in the declaring part. Unit tests in the same box:
  - the spec's A–D cubes asserting the spec values, with the oracle's values in comments
  - a cross-file component
  - a sub-model build that adds nothing
  - a missing part, a missing object, an out-of-range triangle index and a component cycle, each erroring with what is missing named
  - `geomsvc` `analyze` returning the cross-file object (the GUI path)
- [ ] TF.4 Prove parity, then switch. Before touching `parse_3mf`, run the old and new readers side by side on every `.3mf` under `models/` that the old reader can read, and require identical `Object3mf` output (vertices, triangles, colors). Record the files and the result in this box. Then switch `parse_3mf` to the new reader and delete the `threemf::read` path and `read_materials`. The three existing tests pass unchanged, and `cargo nextest run --workspace` + `cargo test --workspace --doc` run green, unpiped into a file that gets read
- [ ] TF.5 Real files by hand (never committed: MakerWorld's Standard Digital File License, and `models/` is its own repo):
  - `fab render --engine scad-rs models/shower_holder/soap_holder.scad` imports the soap dish: 17104 triangles, bbox [78.87, 87.933, 0]–[177.13, 168.067, 65.599], and no "Can't open import file" warning
  - `fab render --check` on it shows the spec's recorded +1.0 mm Z divergence and nothing else
  - `models/wall_screen/slice_parts-plates.3mf` (Bambu, 13 model parts) yields all of its objects
  - the desktop app opens the soap dish (offscreen `--screenshot`)
- [ ] TF.6 Docs: no `docs/*.md` owns mesh import (`svg-import-design.md` is SVG only), so the spec is the owner. `src/threemf_in.rs`'s module doc describes what it reads and points at the spec for the divergence. README needs nothing, because it doesn't describe 3MF import. Verify the module doc against the final code
- [ ] TF.7 Release: ships in `fab`, the desktop app and the web worker on the next `v*` tag (v1.4.1, or riding TE's v1.5.0), only on chotchki's word. hotchkiss.io's editor gets it when its pin moves
