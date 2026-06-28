# scad-lib

My shared OpenSCAD modules (MIT) — the reusable parts every design re-needs, in one place.
Resolved via `OPENSCADPATH` (see [`../docs/openscad-libraries.md`](../docs/openscad-libraries.md)),
so `include <version_stamp.scad>` works from any project.

- `version_stamp.scad` — emboss/deboss a version/label onto a part.
- `part_number.scad` — stamp a piece index so a sliced set reassembles in order.
- `slicer.scad` — linear slab slicing: piece = source ∩ slab, O(N) not O(2^N). See [`../docs/slicing-blowup.md`](../docs/slicing-blowup.md).
- `family_logo.scad` — stamp the family mark onto a part (`attach(TOP, BOTTOM) family_logo()`). Code MIT; the bundled `FamilyLogo.svg` is chotchki's mark, all rights reserved.
- `connectors.scad` — joints across a slicer cut: `bolt_joint` (heat-set + bolt, the default) + `pin_joint` (teardrop dowel + glue) + `dowel`. All-negative (slices cleanly); reuses BOSL2 `screw_hole`.

BOSL2 attachment-style (`include <BOSL2/std.scad>`): stamps are `attachable()` so callers
position them with `attach()`; the slicer is a BOSL2 operator on children, like `partition()`.
Refined by dogfooding on real projects.
