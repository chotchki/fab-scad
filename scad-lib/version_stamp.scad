// version_stamp.scad — emboss/deboss a short label (version, name) onto a part, BOSL2
// attachment-style. attach() it onto a face, centered there; the geometry straddles the
// surface (centered on Z) so you union to raise, or tag("remove")+diff() to recess:
//
//   diff() cuboid([40,20,8]) attach(TOP, BOTTOM) version_stamp("v3");                  // raised
//   diff() cuboid([40,20,8])
//     attach(TOP, BOTTOM, inside=true) tag("remove") version_stamp("v3", depth=0.8);   // recessed
//
// Text has no queryable bounding box, so the attachable extent is ESTIMATED from the label
// length: the CENTER anchor (what stamping uses) is exact; edge/corner anchors approximate.
include <BOSL2/std.scad>

module version_stamp(label, size = 6, depth = 0.6, font = "Liberation Sans:style=Bold",
                     halign = "center", valign = "center",
                     anchor = CENTER, spin = 0, orient = UP) {
    txt = is_string(label) ? label : str(label);
    est = [max(len(txt) * size * 0.62, size), size, depth];   // approx extent for anchors
    attachable(anchor, spin, orient, size = est) {
        linear_extrude(height = max(depth, 0.01), center = true)
            text(txt, size = size, font = font, halign = halign, valign = valign);
        children();
    }
}
