# The auto pipeline: model in → printable plates out

The thesis of the whole tool, in one sentence: drop in a model too big to print, get back a
ready-to-slice **Bambu multi-plate project** — sliced to fit the bed, jointed so the pieces align,
oriented to print without support, packed onto the fewest plates. One command headless, or one open
in the GUI.

```
fab make Underdesk.scad --printer H2D -o plates.3mf
```

## The stages

All of it is ONE pure lib (`fab_scad::auto`), called by both front-ends — so the CLI and the GUI
produce the same plan, there is no logic mirrored per skin.

1. **auto-slice** (`auto_slice`) — partition the model's bbox so every cell fits the bed. For each
   axis the model overflows, cut into `ceil(extent / bed)` EQUAL slabs (equal so there are no
   slivers), and cut ONLY the overflowing axes (single-axis when just one dimension is too big — no
   needless grid intersections).
2. **auto-connect** (`plan` → `cross_section::auto_place`) — seed onions on each cut's cross-section.
   Onions are ALIGNMENT guides, not structure, so the placement is a GEODESIC covering: it floods the
   connected material (grid BFS) out to `ONION_SPACING` and drops a guide wherever a stretch would
   otherwise go unpinned. Distance follows the SHAPE, so each rail gets pinned along its own length
   instead of being deemed aligned by a straight-line-near onion on the next rail over. Corner
   clearance keeps guides off cut intersections, axial-cap keeps them out of thin slabs, and an onion
   the slicer can't print support-free downgrades to a bolt.
3. **auto-orient** (`auto_orient::best_up`) — pick each piece's build-up: minimize unsupported
   overhang FIRST (the never-trim intent), then among near-tied orientations lay the LARGEST face on
   the bed (most stable, best adhesion, lowest print — a slab lies flat instead of standing tall).
4. **pack** (`pack`) — bin-pack the oriented footprints onto the fewest bed-sized plates (FFDH shelf,
   90° rotation), by bounding box.
5. **export** (`bambu`) — write the multi-plate project (see `docs/` research notes for the format
   gotchas: the `Application=BambuStudio-` recognition gate, and POSITIONAL plate binding — pieces
   are placed on Bambu's global plate grid so the importer bins them right).

## Two front-ends, one brain

- **`fab make`** — headless one-shot. `auto::make` renders the base once (OpenSCAD front-door), runs
  the pipeline in-process (Manifold kernel), writes the `.3mf`. Scriptable, zero interaction. When
  the defaults aren't right, the escape hatch is: open the same model in the GUI and nudge.
- **GUI auto-on-open** — open a model that OVERFLOWS the bed and it auto-plans itself (slice +
  connect) as a SEED, then the reactive loop reslices and you refine any cut/onion/orientation by
  hand. Fires ONCE per source; a model that fits the bed is left alone. It's auto-seed, human-refine
  — never a black box, matching the reactive standard (no rebuild button, background rebuild).

## Honest limits (v1)

Documented, not hidden — each is a known place the greedy heuristic leaves quality on the table:

- **bbox slicing** — cuts the whole grid even through empty space (empty cells drop downstream), and
  places cuts on the even grid without dodging thin features or spots a connector can't seat. No
  rotate-to-fit either: a piece that WOULD fit the bed if spun still gets cut. Conservative, safe,
  occasionally over-cuts.
- **bbox packing** — pieces pack by their bounding box, so an L-bracket won't nest into another's
  concave corner. `fill_ratio` reports how tight it got; true polygon nesting is the upgrade if
  plates run too empty.
- **onion coverage is 4-connected** — the geodesic flood uses grid Manhattan distance, which
  overestimates true distance slightly, so it errs toward MORE pins (extra alignment, never a gap).

None of these block the flow — they're the v2 backlog. The v1 gets you from a too-big model to a
printable, jointed, packed project in a second and a half.
