# SVG (2D vector) import — design

`import("logo.svg")` → a 2D `Shape2D::Polygon`, so a stamped family logo (the driving use case) lands as
real geometry you can `linear_extrude`. This is the text() playbook applied to vector art: parse → walk →
flatten Béziers → even-odd polygon. It shipped as Phase Q.4.

## Why this needed a structural change first

The whole import fixpoint was `Mesh`-typed END TO END — `FileTable`, the `mesh_reader` callback,
`Ctx::request_file`, the `import` dispatch. A `.stl` is 3D, but a `.svg` is 2D, and the ONLY place the
2D/3D decision lived was the impure reader, which could only hand back a `Mesh`. So SVG had nowhere to land.

Q.4.1 widened the seam with a dimension-tagged payload:

```rust
pub enum Imported {
    Mesh(Mesh),            // stl / 3mf / off / dat / png  → GeoNode::Leaf
    Contours(Vec<Contour>) // svg / dxf                    → Shape2D::Polygon
}
```

`FileTable`'s value, the reader's return, `request_file`, and the `import`/`surface` dispatch all carry it;
`import` branches `leaf3`/`poly2` on the tag. `Imported` lives in fab-lang core — wasm-clean, no
kernel/native gate — so the async wasm host (fab-web) inherits the same widened seam for free.

### The placeholder-dimension trap

The needs fixpoint runs the program, surfaces the `import` path it couldn't satisfy, the caller reads the
file, then re-runs. On the FIRST pass the file isn't in the table yet, so `request_file` returns an EMPTY
placeholder and records the need. That placeholder's DIMENSION is load-bearing: a `.svg` inside a 2D context
(`linear_extrude() import("x.svg")`) must present as 2D on pass 1, or the run dimension-errors on the mixed
2D/3D tree and aborts BEFORE the need ever surfaces — the fixpoint would never close. So the placeholder is
peeked from the file EXTENSION (`Imported::empty_for`): `.svg`/`.dxf` → empty 2D, everything else → empty 3D.
Cheap, pure, available on pass 1 (the extension is in the literal path string).

## Where the parser lives

usvg (the parse-only half of resvg) is the SVG engine. It NORMALIZES the whole document — resolves
viewBox/units/transforms, expands the shape elements (`rect`/`circle`/`ellipse`/`line`/`polyline`/`polygon`)
and `use`/`defs`/inline-styles into a flat tree of Bézier paths. That's the hard 90% of SVG, done. The
reader (`src/svg.rs`, fab-scad native side) only owns the last mile: walk the filled paths, flatten each
Bézier at a fixed segment count, and map every vertex through the OpenSCAD affine.

usvg is gated on the `kernel` feature and built `default-features = false` — that drops the `text` feature
(fontdb + system-font lookup, which is non-deterministic and BANNED by determinism doctrine #36). SVG `<text>`
then imports empty, which is exactly what OpenSCAD's own SVG importer does with text anyway. Critically usvg
does NOT go into fab-lang: the pure evaluator stays IO-free and wasm-clean; the bytes arrive through the
impure reader seam, native only. wasm SVG import is a later host-side concern.

## The oracle contract (nailed empirically, then confirmed against source)

Verified against OpenSCAD 2026.06.12 and `src/io/import_svg.cc` (three release lines). Two facts drive
everything, and both were measured against the oracle before a line of parser code went in — a rect
`x=10 y=20 w=30 h=40` in a `100×100` viewBox, extruded, bounding-box read back:

- **Scale = 25.4 / 72.** OpenSCAD's SVG default dpi is **72**, NOT the 96 the wikibook claims (96 is only the
  divisor for the explicit `px` UNIT). We set usvg's dpi to 72 too, then convert px→mm by `25.4/72`. That one
  factor CANCELS usvg's mm→px for physical-unit widths (`width="100mm"` → identity mm) AND applies the 72-dpi
  rule for unitless / viewBox-only documents. One constant, both regimes, exact. (Measured: unitless rect
  `w=30` → `10.583 mm = 30·25.4/72` ✓; mm-unit rect `w=30` → `30.000 mm` ✓.)
- **Y-flip about the document height.** SVG's Y points down, OpenSCAD's up. usvg's canvas is `[0, size.height()]`
  px, so `y_mm = (size.height() − y_px) · 25.4/72` reproduces OpenSCAD's flip. `center` defaults to `false`, so
  the viewBox bottom-left sits at the mm origin. (Measured: SVG y∈[20,60] → mm y∈[40,80] both engines ✓.)

Two more, from the source read:

- **Even-odd fill is FAITHFUL, not a compromise.** OpenSCAD IGNORES the SVG `fill-rule` attribute entirely and
  always resolves holes by even-odd nesting (it even force-reverses rings to CCW first). Our `Shape2D::Polygon`
  is even-odd. So a glyph-like "O" fills as a ring, matching the oracle exactly — the same free-holes property
  text() rides on.
- **20 segments per Bézier.** OpenSCAD's SVG default when `$fn` is unset. We flatten at a fixed 20 — and
  empirically 20 MINIMIZES the residual (bumping to 40/60 made FamilyLogo's residual slightly WORSE, because
  our flatten converges to the true area while OpenSCAD stays at its own 20-approx).

Because both the abs-transform and the scale/flip are affine, and an affine commutes with uniform-`t` Bézier
sampling, we map control points to mm FIRST then flatten — identical result, simpler code.

## Validation

`fab render --engine scad-rs --check` (our differ, against OpenSCAD) across the SVG corpus:

- **8/8 machineblocks icon patterns AGREE** within residual tolerance (filled paths with holes — the logo
  shape).
- **FamilyLogo** (the driver): bbox `203.73×178.75` vs OpenSCAD `203.70×178.75` — identical origin, identical
  Y-extent, X within **0.03 mm** on a 204 mm logo; volume residual **0.07%** (a genuine tiny usvg-vs-libsvg
  shape-extraction difference, not scale/flip — it's stable and matchable, the text()-class result). Genus 1,
  the one hole resolved correctly.
- **drawing.svg, cutout.svg** (Inkscape CAD): volume + bbox IDENTICAL to the oracle; genus off by one — the
  known Manifold-version genus divergence (L.3.5 family), affects all geometry, NOT SVG-specific.
- **remindwall `frame_upper.scad`** — the original blocker — now renders FULLY (vol 1.01e6, genus 133), logo
  stamp and all. This was the last thing stopping remindwall.

## v1 simplifications (documented gaps, revisit when a real model's residual demands it)

- **FILLED-CLOSED paths only.** OpenSCAD STROKES open/unfilled paths (Clipper-offset by `stroke-width/2`); we
  skip `fill:none` paths. Logos are filled contours, so this only bites CAD-outline SVGs — the 3 keyboard_tent
  paths (all `fill="none"`) import empty, and `template_paths.svg` (guide lines + fills) diverges.
- **`$fn`/`$fa`/`$fs` ignored** (fixed 20/Bézier). The reader runs OUTSIDE eval, with no call-site fragment
  vars. Matches OpenSCAD's default; a knob is a later lift into the eval seam (would need a Bézier-carrying
  intermediate payload so the flatten happens at the `import` dispatch, not in the reader).
- **Pooled even-odd across an element's subpaths.** OpenSCAD does per-ELEMENT even-odd then unions elements
  (nonzero); we pool ALL a path's subpaths into one even-odd `Polygon`. Identical for non-overlapping art
  (every clean logo); differs only for overlapping same-color fills (`template_paths.svg`'s spurious holes).
- **Explicit `px`-unit width/height** scale at 72 dpi here vs OpenSCAD's hardcoded 96 for that ONE unit — a
  ~1.36× residual for the rare px-SIZED SVG. viewBox-only and mm (the common cases) are exact.
- **DXF** stays LOUD-deferred — SVG is the wired 2D path; DXF is its own reader when a model needs it.

## Determinism

Pure-Rust f64 math + usvg's deterministic geometry normalization + a fixed segment count = bit-identical
output cross-platform (doctrine #36), same as text(). The one determinism hazard in usvg — font/text layout —
is compiled out. Oracle FIDELITY (matching OpenSCAD) is the separate, softer bar, met via volume-residual as
above.
