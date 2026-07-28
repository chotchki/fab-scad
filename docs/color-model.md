# Color, end to end

**Verdict: `color()` SEMANTICS (which color wins) are oracle-matched. What kept breaking is the
UNCOLORED case — geometry a boolean pulled in from an unpainted operand — and it broke in the
display, in the export, and in the differential that was supposed to catch both.**

Written down because this has now been misdiagnosed twice: once as "the engine loses colors" (it
doesn't), and once as "verified against the oracle" using a command that ran the oracle on both
sides. Read the measurement section before trusting any comparison, including one of mine.

## The chain

`color()` → `Combinator::Color` → `GeoNode::Color` → `Solid::with_color` → **5 extra Manifold vertex
properties: RGBA plus a PAINTED flag** (J.2.9, masked in SY.2), which survive every boolean because
Manifold carries properties through union/difference/intersection (seam verts linear-interpolate,
exact for a uniform color).

From there it forks by consumer:

| consumer | carrier | shape |
| --- | --- | --- |
| 3MF export (`to_3mf_bytes`) | `<basematerials>` table | per-vertex, deduped to distinct colors |
| plate export (`Piece`) | `color: Option<[f32; 4]>` | per-piece |
| **display** (`WirePart`/`WirePiece`/`Rendered`/`Resliced`) | `colors: Option<Vec<Option<[f32; 4]>>>` | **per STL CORNER** |

The display carriers are SX. Before it they had no color field at all, so `part_material` returned a
constant `MODEL_GOLD` — every part gold, no matter what the model said.

**Why per-CORNER and not per-vertex.** STL is a triangle SOUP: it repeats a shared vertex once per
face. `vertex_colors()` is indexed like `to_indexed()` (deduped), so handing that array to a
renderer as a vertex attribute paints the wrong faces. `to_stl_with_colors` walks the same
`to_mesh_gl` in the same order and emits one rgba per emitted corner, which drops straight onto
Bevy's `ATTRIBUTE_COLOR` with no re-indexing. `to_stl_bytes` is a pure subset of that walk, pinned by
test, so the two can never drift into two STL writers.

**`None` is load-bearing, at BOTH levels.** The OUTER `None` means the solid is wholly uncolored
(MeshGL stride 3, no color property). The INNER `None` means one corner of a partially-colored solid
was never painted. Both say "substitute your own default"; neither means black.

**The unpainted case is the one that bites, and it needs an explicit MASK (SY.2).** A boolean's
output `num_prop` is `max(P, Q)`, so when a colored solid meets an uncolored one, Manifold pushes
`0.0` into every property slot the uncolored operand didn't have. Its faces come back as rgba
`(0,0,0,0)` — which by VALUE is exactly what `color("transparent")` produces, and a later boolean
barycentrically blends those zeros against real colors into partial values no exact-zero test can
catch. So `with_color` writes a fifth channel, a PAINTED flag of `1.0`; Manifold zero-fills `0.0` on
the unpainted side and a blended seam lands strictly between. `>= 0.5` is the test.

The kernel then refuses to invent a default, because "what does uncolored LOOK like" is a viewer
question with two different right answers: the viewport substitutes `MODEL_GOLD` (fab's own theme —
a fully uncolored model and the uncolored half of a mixed one then read identically), and the 3MF
export substitutes OpenSCAD's `#F9D72C` (a `.3mf` is an interchange artifact read by other people's
tools, where matching the ecosystem beats matching our theme).

Before the mask, the viewport painted those zeros opaque black and the 3MF exported
`displaycolor="#00000000"`, which three.js renders invisible — a published model with holes in it.

**sRGB → LINEAR at the display seam.** Bevy reads `ATTRIBUTE_COLOR` as linear; `color()` values and
the theme constants are sRGB. Skipping the conversion is invisible on primary colors, because 0 and
1 are fixed points of the transfer curve — which is precisely why `frame_upper`'s blue and white hid
it — and washes out every mid-tone.

## Outermost-color-wins is CORRECT — and here is how to actually check it

`color("red") { cube(); color("blue") cube(); }` gives ONE color, red. That is not a fab bug; it is
what OpenSCAD does.

**Two traps make this easy to "verify" without verifying anything, and an earlier draft of this file
fell into both.**

1. **`fab render` defaults to `--engine openscad`** and shells out to the OpenSCAD binary. So the
   obvious command — `fab render x.scad --out x.3mf` — compares the ORACLE WITH ITSELF and always
   agrees. Use `--engine scad-rs`, which since SY.1 honours the `.3mf` extension instead of writing
   STL bytes into a 3mf-named file.
2. **A 3MF material TABLE is not a list of used colors.** OpenSCAD always emits a `Default`
   `<base>` entry as the object-level `pindex` fallback, referenced by nothing. Grepping
   `displaycolor` counts it and manufactures a difference. Count the triangles' `p1` refs instead —
   which is what `differ.rs` does, comparing per-FACE colors.

Measured properly (`--engine scad-rs`, used materials only):

| case | oracle | fab |
| --- | --- | --- |
| siblings `color(red) cube; color(blue) cube` | red + blue | red + blue |
| nested `color(red){ cube; color(blue) cube }` | red (24/24 tris) | red |
| mixed `color(blue) cube; cube` | blue + `#F9D72C` | blue + `#F9D72C` |

The standing gate is `differ::compare_colors` — and note it normalizes BOTH legs symmetrically: the
oracle's colorscheme default is subtracted on its side, unpainted faces are skipped on ours. Its case
table carried only wholly-colored and wholly-uncolored programs until SY.5, which is exactly why the
mixed case shipped broken.

## The BOSL2 trap that looks like an engine bug

`color_this()` sets `$color`, and `attachable()` applies it via `_color($color)` → the real
`color()`. BOSL2's own docs warn: *"This works only with attachables and you cannot have any color()
modules above it in any parents, only recolor() or other color_this()."*

So a `color("white")` nested INSIDE a `color_this("blue")` attachable is flattened away by
outermost-wins — in fab and in OpenSCAD alike. `recolor()` is the sanctioned way to re-color inside
a `color_this` tree. And a `color_this` inside a module that is never instantiated colors nothing at
all, which is exactly as boring as it sounds and exactly what it looks like from the viewport.

## The "streaks" are the model's, not the engine's (SY.7)

`frame_upper.scad` renders its logo pocket as a mosaic — a field of default-colored triangles with
BLUE STREAKS running across it, fanned along the logo's SVG outline. It looks exactly like a color
bug, and it was reported as one.

It isn't. **OpenSCAD's own preview of the same file draws the same mosaic**, streaks and all
(`openscad --preview --viewall -o x.png models/wall_screen/frame_upper.scad`). The one thing fab had
wrong was painting that field BLACK instead of the default color — the zero-fill above. Once the
unpainted default lands, the two renderers agree.

What produces it is the model: a `tag("keep")` cuboid whose top face is EXACTLY coplanar with the
cover's, so the shared plane is retriangulated into sliver fans anchored on the logo outline, and
each resulting triangle is attributed to whichever operand its source face came from. Painted and
unpainted triangles interleave; the boundaries read as streaks. Nothing in the renderer can fix
that — the attribution is already baked into the vertex data, and it is the SAME attribution the
oracle makes.

The fix, if the look matters, is in the MODEL: offset that cuboid so the faces aren't coplanar, or
paint it. What was NOT worth doing is a coplanar tie-break in the kernel — it would have moved fab
AWAY from the oracle to chase a cosmetic artifact upstream also has.

Corollary worth keeping: a merged mixed-color model is a case where fab's 3MF is MORE faithful than
OpenSCAD's. The oracle's exporter is per-OBJECT, so a union of a painted and an unpainted solid
exports entirely in the painted color — the distinction its own preview draws is dropped. fab's
per-vertex table keeps it. Do not "fix" that divergence toward the oracle.

## Where the views disagree on purpose

The MODEL view shows model color. The **Orientation/Export views deliberately do NOT** — they paint
the HSL rainbow keyed by piece index (`print.rs`, +25° phase off the navy wedge so no piece sinks
into the background, 47° stride). That palette exists to tell N pieces apart, and model color would
destroy it the moment two pieces share one. `WirePiece` carries the color anyway: the wire stays
honest about the geometry, and the view decides what to draw.
