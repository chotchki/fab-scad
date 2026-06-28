// version_stamp.scad — emboss/deboss a short label (version, name) onto a part. Attachable
// wrapper over BOSL2 text3d(), centered so it straddles the surface — union to raise,
// tag("remove") + diff() to recess (same attach idiom as family_logo):
//
//   cuboid([40,20,8]) attach(TOP, BOTTOM) version_stamp("v3");                         // raised
//   tag_scope() diff() cuboid([40,20,8])
//     attach(TOP, BOTTOM, inside=true) tag("remove") version_stamp("v3", depth=0.8);   // recessed
//
// We wrap text3d in our own attachable so the two-anchor attach() works (text3d on its own
// rejects it). CENTER — what stamping uses — is exact; the box width is estimated from the
// label length, so only the edge/corner anchors are approximate.
include <BOSL2/std.scad>

module version_stamp(label, size = 6, depth = 0.6, font = "Liberation Sans:style=Bold",
                     anchor = CENTER, spin = 0, orient = UP) {
    txt = is_string(label) ? label : str(label);
    box = [max(len(txt) * size * 0.62, size), size, depth];
    attachable(anchor, spin, orient, size = box) {
        text3d(txt, h = max(depth, 0.01), size = size, font = font, center = true);
        children();
    }
}
