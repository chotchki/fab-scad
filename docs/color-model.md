# Color, end to end

**Verdict: fab's color semantics are ORACLE-EXACT, and always were. What was broken was the
display wire.** Recording this because "color isn't being respected" is a natural bug report to
file against the ENGINE, and the engine is the one place it isn't worth looking.

## The chain

`color()` → `Combinator::Color` → `GeoNode::Color` → `Solid::with_color` → **4 extra Manifold vertex
properties (RGBA)**, which survive every boolean because Manifold carries properties through
union/difference/intersection (seam verts linear-interpolate, exact for a uniform color). That's
J.2.9, and it has worked since.

From there it forks by consumer:

| consumer | carrier | shape |
| --- | --- | --- |
| 3MF export (`to_3mf_bytes`) | `<basematerials>` table | per-vertex, deduped to distinct colors |
| plate export (`Piece`) | `color: Option<[f32; 4]>` | per-piece |
| **display** (`WirePart`/`WirePiece`/`Rendered`/`Resliced`) | `colors: Option<Vec<[f32; 4]>>` | **per STL CORNER** |

The display carriers are SX. Before it they had no color field at all, so `part_material` returned a
constant `MODEL_GOLD` — every part gold, no matter what the model said.

**Why per-CORNER and not per-vertex.** STL is a triangle SOUP: it repeats a shared vertex once per
face. `vertex_colors()` is indexed like `to_indexed()` (deduped), so handing that array to a
renderer as a vertex attribute paints the wrong faces. `to_stl_with_colors` walks the same
`to_mesh_gl` in the same order and emits one rgba per emitted corner, which drops straight onto
Bevy's `ATTRIBUTE_COLOR` with no re-indexing. `to_stl_bytes` is a pure subset of that walk, pinned by
test, so the two can never drift into two STL writers.

**`None` is load-bearing.** An uncolored solid (MeshGL stride 3, no color property) yields `None`,
and the viewer paints its own default. Without that an uncolored model would come out transparent
black. So: a fully uncolored model keeps fab's `MODEL_GOLD`; a MIXED model shows OpenSCAD's own
`#F9D72C` default for its uncolored half, because the kernel assigns that once any color property
exists. fab's look survives for plain models, faithful-to-OpenSCAD kicks in exactly when the model
has opinions.

## Outermost-color-wins is CORRECT — measured, not assumed

`color("red") { cube(); color("blue") cube(); }` gives ONE color, red. That is not a fab bug; it is
what OpenSCAD does. Verified by exporting through the OpenSCAD binary and diffing the 3MF color
tables:

| case | oracle | fab |
| --- | --- | --- |
| siblings `color(red) cube; color(blue) cube` | red + blue + default | identical |
| nested `color(red){ cube; color(blue) cube }` | red + default | identical |
| `models/wall_screen/frame_upper.scad` | `#0000FF` + `#F9D72C` | identical |

Re-run that comparison before believing any future "fab loses a color" report. The backend comment
at `GeoNode::Color` says outermost wins; that's the enclosing node's color op overwriting the inner
one, and it matches.

## The BOSL2 trap that looks like an engine bug

`color_this()` sets `$color`, and `attachable()` applies it via `_color($color)` → the real
`color()`. BOSL2's own docs warn: *"This works only with attachables and you cannot have any color()
modules above it in any parents, only recolor() or other color_this()."*

So a `color("white")` nested INSIDE a `color_this("blue")` attachable is flattened away by
outermost-wins — in fab and in OpenSCAD alike. `recolor()` is the sanctioned way to re-color inside
a `color_this` tree. And a `color_this` inside a module that is never instantiated colors nothing at
all, which is exactly as boring as it sounds and exactly what it looks like from the viewport.

## Where the views disagree on purpose

The MODEL view shows model color. The **Orientation/Export views deliberately do NOT** — they paint
the HSL rainbow keyed by piece index (`print.rs`, +25° phase off the navy wedge so no piece sinks
into the background, 47° stride). That palette exists to tell N pieces apart, and model color would
destroy it the moment two pieces share one. `WirePiece` carries the color anyway: the wire stays
honest about the geometry, and the view decides what to draw.
