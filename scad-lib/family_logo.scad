// family_logo.scad — stamp the family logo onto a part, BOSL2 attachment-style.
//
// NOTE: the CODE here is MIT like the rest of scad-lib, but the bundled FamilyLogo.svg is
// chotchki's family mark — NOT covered by the MIT grant, all rights reserved. The module
// imports it relative to ITSELF (OpenSCAD resolves import() against the module file), so the
// default works from any project; pass `svg=` to stamp a different mark.
//
// Attachable, so you place it like any BOSL2 shape:
//   raised:
//     diff() cuboid([60,60,10]) attach(TOP, BOTTOM) family_logo(width=40);
//   recessed (tag it for removal):
//     diff() cuboid([60,60,10])
//       attach(TOP, BOTTOM, inside=true) tag("remove") family_logo(width=40, depth=1);
//
// `bevel` drafts the base outward (wide at the bottom) so a raised stamp prints support-free
// and a recess gets a chamfered lip — mirrors the original logo_emboss offset draft.
include <BOSL2/std.scad>

// FamilyLogo.svg viewBox (w, h) — its native units, used to scale by `width` and to size
// the attachable bounding box.
_FAMILY_LOGO_VIEWBOX = [577.53, 506.68];

module family_logo(width = 40, depth = 0.6, bevel = 0, svg = "FamilyLogo.svg",
                   anchor = CENTER, spin = 0, orient = UP) {
    s = width / _FAMILY_LOGO_VIEWBOX.x;
    size = [width + 2 * bevel, _FAMILY_LOGO_VIEWBOX.y * s + 2 * bevel, depth];
    attachable(anchor, spin, orient, size = size) {
        down(depth / 2)
        if (bevel <= 0) {
            linear_extrude(depth) _family_logo_2d(s, svg);
        } else {
            steps = max(2, ceil(depth / 0.2));
            for (k = [0 : steps - 1])
                up(k * depth / steps)
                    linear_extrude(depth / steps + 0.01)
                        offset(r = bevel * (1 - k / steps))
                            _family_logo_2d(s, svg);
        }
        children();
    }
}

// The 2D logo, scaled and centered at the origin.
module _family_logo_2d(s, svg) {
    scale(s) import(svg, center = true);
}
