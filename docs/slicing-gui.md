# The slicing GUI — Phase 5 design

Scoping pass before any code (PLAN Phase 5). The slicer + connector engine (Phase 4) is the
scriptable half; this is the half that kills the actual headache — the SPATIAL work. Picking
cut planes and clicking faces to drop connectors is miserable as CLI flags, and "remember the
verbs" is its own tax. So the GUI is the primary UX, not a capstone — and building it is how
we dogfood the one genuine unknown: how `fab` drives OpenSCAD for a live preview.

## What it must do

1. **Load a model** — a project's source `.scad`, rendered to geometry we can show + pick.
2. **Set cut planes** — drag the slab cuts the slicer consumes (axis + positions), see the
   pieces update.
3. **Place connectors** — click a cut face, drop a `bolt_joint`/`pin_joint` (auto-grid or by
   hand), per the Phase-4 lib.
4. **Preview piece-vs-bed + orientation** — each piece against the H2D/H2C bed (reuse `fab
   plan`: fits? needs a cut? rotate?), and its print orientation (support-free? reuse
   `needs_teardrop`).
5. **Emit the slicing spec** — write what we chose so `fab` can reproduce it headlessly, and
   round-trip it through `fab render` (5.2).
6. **Grow into a workflow front-end** — focus/new/render/publish without memorizing verbs (5.3).

## The unknown, decided: render to mesh, GUI owns the viewport

OpenSCAD has no API to drive its preview, so don't try. The clean seam — and the one the SPEC
flagged to discover — is a **geometry round-trip**:

- `fab` renders the model (and each sliced piece) to STL via the existing wrap.
- The **GUI loads the mesh and draws it in its own 3D viewport** — camera orbit, and
  ray-cast picking for "click this face."
- Cut planes + connector markers are **GUI overlays**, cheap to drag without re-rendering.
- On a committed edit, the GUI re-invokes `fab render` (incremental, content-hashed per 6.2)
  and swaps the mesh. OpenSCAD stays the geometry engine; the GUI never reaches into it.

This is why the wrap (3.2) and incremental render (6.x) matter: the GUI is just another `fab
render` caller. It also means the viewport tech is independent of OpenSCAD — we own it.

## Framework: Bevy (Rust)

`fab` is Rust and so is the toolkit; the GUI should be too — one binary, shared types (the
slicing spec, printer beds, connector specs already live in Rust/SCAD, nothing to mirror into
a JS layer). **Bevy** owns the 3D: load the STL, orbit the camera, ray-pick a face to drop a
connector (ECS + its picking are a fit here, not overkill).

Two payoffs past "it's Rust":

- **The web stays Rusty too.** Bevy compiles to WASM, so the SAME viewer code can serve the
  public site viewer — no separate Three.js/JS stack, no spec types re-written in TS. That
  folds 7.1 INTO this work instead of duplicating it (7.1's "Three.js viewer" framing gets
  revisited as Bevy/WASM).
- **bsn.** Bevy 0.19's scene notation gives a declarative way to describe the viewport scene
  and UI — a direction worth leaning into rather than hand-wiring widgets.

UI panels (cut/connector controls): **bsn + Feather** — Bevy's native scene-notation UI plus
its widget set (chotchki's call; we'll see how it goes when 5.1 lands). `bevy_egui` stays the
fallback if Feather is too green. Either way it doesn't gate the architecture.

Rejected: **Tauri + web/Three.js** (splits the stack into JS, mirrors the spec types in TS).
Plain **egui + three-d** was the other Rust option — lighter, but loses the web-unification and
the room to grow that Bevy buys.

## The GUI ↔ fab contract: the slicing spec

The GUI doesn't cut geometry — it edits a **spec**, and `fab` turns the spec into the same
`slice()`/connector SCAD the Phase-4 lib runs. That keeps the GUI and the headless path
identical (5.2's round-trip: GUI → spec → `fab render` → STL, reproducing the preview).

Where it lives: **grow `project.toml`** (the manifest was always meant to grow by dogfooding;
slicing config is a known field). Roughly:

```toml
[slicing]
printer = "H2D"

[[slicing.cut]]
axis = "x"          # x|y|z
at   = -10          # mm, model coords

[[slicing.connector]]
piece = 1           # which piece / cut face
type  = "bolt"      # bolt|pin
screw = "M3"
at    = [0, 12]     # position on the cut face
```

How `fab` applies it: generate a driver `.scad` (exactly like `fab coupon` already does) that
`include`s the source + `slicer.scad`/`connectors.scad` and emits
`slice(cuts) diff(){ source(); <connectors> }` from the spec, then render it. So `fab render`
of a spec'd project Just Works, and the GUI previews by calling that same path.

## Build sequence (proposed — reorders the leaves)

Build the **headless engine before the GUI**, because the spec round-trip is testable without
any window and is the thing the GUI stands on:

1. **5.2 first — the spec + `fab` driver generation.** Define `[slicing]` in the manifest,
   generate the slicer driver `.scad`, render it. Fully testable headless (assert the spec
   round-trips: spec → render → expected pieces). No GUI yet.
2. **5.1 — the Bevy MVP.** Load the model mesh, drag cut planes, click a face to place a
   connector, preview piece-vs-bed (via `fab plan`) + orientation. It reads/writes the 5.2
   spec and previews by calling `fab render`.
3. **5.3 — the workflow front-end.** Fold focus/new/render/publish into the same window so the
   verbs stop being a memory tax.

## Open questions / risks

- **Picking quality** — Bevy ray-mesh picking on a dense STL: fast enough? (Decimate the
  preview mesh — 7.1's decimation may land early here.)
- **WASM bundle size** — Bevy's web build is hefty, so on the site (7.1) the viewer is
  **lazy-loaded, not shipped on page load**: only on pages that have a model, and only after
  the visitor asks to interact. Show the static cover image first (7.2), then swap in the
  Bevy/WASM viewer on click. The heavy bundle is opt-in, so normal page loads stay fast.
- **Cut-plane coords** — the GUI thinks in model space; the spec stores model-space `at`; the
  slicer is centered-origin. Pin the coordinate convention in 5.2 so there's one source of truth.
- **Per-piece orientation** — 4.5 left "which lay" to here; the GUI is where you actually choose
  it and see the support-free consequence. That closes the 4.5 loop.
- **Live-ness** — start with re-render-on-commit (a button / drag-release), not every frame;
  revisit if it feels laggy.
