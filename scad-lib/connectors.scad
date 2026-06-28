// connectors.scad — joints across a slicer cut plane, BOSL2 attachment-style. The joint is
// built around the cut plane at the origin (CENTER anchor = the cut plane), bolt/dowel axis
// +Z; reorient with the standard `orient`/`spin`/`anchor`.
//
// All-NEGATIVE: tag them "remove" and diff them out of the model BEFORE slicing — each piece
// keeps its own half (the slab clip splits the holes). Use chotchki's tagged idiom, and wrap
// in tag_scope() so the removes don't leak when this nests inside another diff:
//
//   slice([cut], axis=UP)
//     tag_scope() diff()
//       my_model() {                          // an attachable model
//         left(15)  tag("remove") bolt_joint("M3", through = upper_thickness);
//         right(15) tag("remove") pin_joint(d = 6);
//       };
//   // ...and print dowel(d = 6) on its own for each pin_joint, then glue.
//
// The only positive part is dowel() (the separately printed pin). Seeded from chotchki's
// projects (see the fastener-specs harvest): M3 default heat-set (5.0 × 6mm), garage_door
// boss params, $slop 0.1, teardrop ang 20, peg→socket +0.2.
include <BOSL2/std.scad>
include <BOSL2/screws.scad>

// Heat-set press-hole [diameter, depth] by screw size — harvested defaults.
function _insert_spec(screw) =
    screw == "M3" ? [5.0, 6] :
    screw == "M4" ? [6.0, 6] :
    screw == "M5" ? [7.0, 10] :
    assert(false, str("bolt_joint: unknown screw '", screw, "' (have M3/M4/M5)"));

// bolt_joint (DEFAULT) — negative volume of a heat-set + bolt joint, attachable on the cut
// plane (CENTER). +Z (bolt-access piece): clearance + socket-head counterbore (BOSL2
// screw_hole), outer face at z = `through` (set it to that piece's thickness for a flush
// head). -Z (insert piece): heat-set pocket + a chamfer lead-in at the cut face.
module bolt_joint(screw = "M3", through = 12, counterbore = 5, lead_in = 0.7,
                  anchor = CENTER, spin = 0, orient = UP) {
    spec = _insert_spec(screw);
    idia = spec[0];
    idepth = spec[1];
    size = [max(idia, 10), max(idia, 10), through + idepth];
    attachable(anchor, spin, orient, size = size) {
        union() {
            up(through) screw_hole(str(screw, ",", through), head = "socket",
                                   counterbore = counterbore, anchor = TOP, orient = DOWN);
            cyl(d = idia, h = idepth, anchor = TOP);                       // insert pocket
            cyl(d1 = idia, d2 = idia + 2 * lead_in, h = lead_in, anchor = TOP); // lead-in
        }
        children();
    }
}

// pin_joint — a teardrop socket each side of the cut for a separately printed, glued dowel().
// Attachable on the cut plane (CENTER). Teardrop (ang 20) prints support-free when the joint
// is horizontal; harmless when vertical.
module pin_joint(d = 6, depth = 8, slop = 0.2, ang = 20,
                 anchor = CENTER, spin = 0, orient = UP) {
    sd = d + 2 * slop;
    size = [sd * 1.4, sd, 2 * depth];
    attachable(anchor, spin, orient, size = size) {
        union() {
            teardrop(h = depth, d = sd, ang = ang, anchor = BOTTOM, orient = UP);
            teardrop(h = depth, d = sd, ang = ang, anchor = BOTTOM, orient = DOWN);
        }
        children();
    }
}

// dowel — the separately printed pin for pin_joint (glue in). Nominal `d`; the socket is
// d + 2*slop, so it drops in. Length spans both sockets. Attachable (delegates to teardrop).
module dowel(d = 6, len = 16, ang = 20, anchor = CENTER, spin = 0, orient = UP) {
    teardrop(h = len, d = d, ang = ang, anchor = anchor, spin = spin, orient = orient)
        children();
}
