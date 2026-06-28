# scad-lib

My shared OpenSCAD modules (MIT) — the reusable parts every design re-needs, in one place.
Resolved via `OPENSCADPATH` (see [`../docs/openscad-libraries.md`](../docs/openscad-libraries.md)),
so `include <version_stamp.scad>` works from any project.

- `version_stamp.scad` — emboss/deboss a version/label onto a part.
- `part_number.scad` — stamp a piece index so a sliced set reassembles in order.
- _(coming, Phase 4)_ the linear slicer + connector library.

Pure-OpenSCAD where possible (no forced BOSL2 dep) so they compose anywhere and survive a
BOSL2 bump. Refined by dogfooding on real projects.
