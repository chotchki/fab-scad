# Phase TF - 3MF import reads Production-extension files (Bambu Studio / MakerWorld)

## Why

A Bambu Studio or MakerWorld `.3mf` doesn't import. `import("Soap_Dish-Without_Hook.3mf")` (models/shower_holder) warns "Can't open import file … duplicate field `metadata`" and the model renders without it, and the GUI can't open the file either. It's not one bad file: Bambu writes every 3MF in the Production-extension shape (the root model points at its meshes in `3D/Objects/*.model`), so this is the format most real-world 3MFs arrive in.

## What Changes

- Root cause is three defects stacked, and the first hides the other two (diagnosed and oracle-probed 2026-10-04):
  1. threemf 0.8.0 deserializes `<metadata>` as a serde `Vec`, and quick-xml can't collect a list split into two runs. Bambu writes metadata before `<resources>` AND after `</build>`, hence "duplicate field".
  2. threemf has no field for a component's `p:path` and returns the `.model` files unnamed, so `threemf_in::collect` looks for the component's object in the root file and fails "missing object 1".
  3. Only the root model's `<build>` may instantiate. Sub-model builds are empty by spec, but nothing enforces it: today every file's build is walked.
- The READ path leaves threemf for a quick-xml event reader on local element names, the approach `read_materials` already uses for basematerials. It indexes every `.model` by its path, finds the root through `_rels/.rels`, resolves components through `p:path` and instantiates only the root build. threemf stays for writing (`Solid::write_3mf`).
- Placement follows the 3MF spec: component transforms compose parent∘child, and an absent or identity transform adds no offset. That DIVERGES from the OpenSCAD 2026.06.12 oracle on purpose (chotchki, 2026-10-04: accept the divergence, no upstream report). Upstream's `import_3mf_v2.cc:135` gives a component without a transform (lib3mf reports an explicit identity as none) a (0,0,1) translation, and `:145` composes child after parent. Probed on the binary, the soap dish lands 1.0 mm high and a rotated item over a translated component moves along the wrong axis. The spec records the divergence, so a `fab render --check` hit reads as known instead of as a regression.
- An unresolvable `p:path`, object id or out-of-range triangle index still fails the import (warn-and-render-without, as today), and the message names the missing part.
- `zip` declares its own `deflate` feature. Every entry in a Bambu 3MF is DEFLATE, and today that support arrives only through threemf's dependency, so moving off threemf without it would break every compressed file.

## Capabilities

### New Capabilities

- `import/3mf`: what `import()` and the GUI do with a `.3mf`: which build items become geometry, how components and transforms place it (the oracle divergence included), object colors, and how an unresolvable reference fails.

### Modified Capabilities

None. `openspec/specs/` is empty; this is fab-scad's first spec.

## Impact

- `src/threemf_in.rs`: the read path is rewritten; `parse_3mf` and `Object3mf` keep their signatures, so no caller changes: `src/import.rs` (`read_import_bytes` :74, `threemf_mesh` :296), `src/geomsvc.rs` (`analyze` :171), `src/bin/mesh_diff.rs:74`.
- `Cargo.toml`: `zip` gains `features = ["deflate"]`. No new crates: quick-xml and zip are already `kernel` deps, and miniz_oxide is already in both the native and wasm graphs.
- Tests: synthetic fixtures only. The soap dish ships under MakerWorld's Standard Digital File License and never enters this repo; it is verified by hand.
- Owning doc: none. No `docs/*.md` covers mesh import (`svg-import-design.md` is SVG only), so the new `import/3mf` spec becomes the owner when it syncs at archive.
- Release reach: CI on every push. The reader is in the `fab` CLI, the desktop app AND the wasm geometry worker (`geom` builds `fab-scad` with `kernel`), so a `v*` tag ships it everywhere at once; hotchkiss.io's editor gets it only when its pin moves (v1.3.2 today). No one-way door: nothing we write changes shape, and `latest.json` and the signing key are untouched.
