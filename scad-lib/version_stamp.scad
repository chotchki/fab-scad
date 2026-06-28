// version_stamp.scad — emboss/deboss a short label (version, name) onto a part.
// Pure OpenSCAD, no library deps, so it composes anywhere and survives any BOSL2 bump.
//
// The module makes ONLY the text solid, centered at the origin, lying in XY and extruded
// +Z by `depth`. You position/rotate it onto a face, then union (raised) or difference
// (recessed) it with your part:
//
//   // recessed (prints support-free — the text floor is the part surface):
//   difference() { my_part(); up(face_z) version_stamp("v3"); }
//
//   // raised:
//   union()      { my_part(); up(face_z) version_stamp("v3"); }
module version_stamp(label, size = 6, depth = 0.6, font = "Liberation Sans:style=Bold",
                     halign = "center", valign = "center") {
    linear_extrude(height = max(depth, 0.01))
        text(is_string(label) ? label : str(label),
             size = size, font = font, halign = halign, valign = valign);
}
