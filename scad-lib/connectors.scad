// connectors.scad — joints across a slicer cut plane, BOSL2-based. Cut plane is z=0, the
// joint axis is +Z by default (set `orient` for a cut on another axis).
//
// All-NEGATIVE: subtract these from the model BEFORE slicing, and each piece keeps its own
// half (the slab clip splits the holes correctly). The only positive part is the separately
// printed dowel() for pin_joint.
//
//   slice([cut], axis=UP) diff() {
//       my_model();
//       up(cut) left(15)  bolt_joint("M3", through = upper_thickness);
//       up(cut) right(15) pin_joint(d = 6);
//   }
//   // ...and print dowel(d = 6) on its own for each pin_joint, then glue.
//
// Seeded from chotchki's projects (see the fastener-specs harvest): M3 the default heat-set
// (5.0 × 6mm), boss params from garage_door, $slop 0.1, teardrop ang 20, peg→socket +0.2.
include <BOSL2/std.scad>
include <BOSL2/screws.scad>

// Heat-set press-hole [diameter, depth] by screw size — harvested defaults.
function _insert_spec(screw) =
    screw == "M3" ? [5.0, 6] :
    screw == "M4" ? [6.0, 6] :
    screw == "M5" ? [7.0, 10] :
    assert(false, str("bolt_joint: unknown screw '", screw, "' (have M3/M4/M5)"));

// bolt_joint (DEFAULT) — negative volume of a heat-set + bolt joint.
//   +Z (bolt-access piece): clearance + socket-head counterbore (BOSL2 screw_hole), outer
//      face at z = `through` (so set `through` = that piece's thickness for a flush head).
//   -Z (insert piece): heat-set pocket + a chamfer lead-in at the cut face.
module bolt_joint(screw = "M3", through = 12, counterbore = 5, lead_in = 0.7, orient = UP) {
    spec = _insert_spec(screw);
    idia = spec[0];
    idepth = spec[1];
    rot(from = UP, to = orient) {
        // bolt side: clearance hole of length `through` with the head recess at the far end.
        up(through) screw_hole(str(screw, ",", through), head = "socket",
                               counterbore = counterbore, anchor = TOP, orient = DOWN);
        // insert side: pocket + chamfer lead-in (wider at the cut face).
        cyl(d = idia, h = idepth, anchor = TOP);
        cyl(d1 = idia, d2 = idia + 2 * lead_in, h = lead_in, anchor = TOP);
    }
}

// pin_joint — a teardrop socket each side of the cut for a separately printed, glued dowel().
// Teardrop (ang 20) prints support-free when the joint is horizontal; harmless when vertical.
module pin_joint(d = 6, depth = 8, slop = 0.2, ang = 20, orient = UP) {
    sd = d + 2 * slop;
    rot(from = UP, to = orient) {
        teardrop(h = depth, d = sd, ang = ang, anchor = BOTTOM, orient = UP);
        teardrop(h = depth, d = sd, ang = ang, anchor = BOTTOM, orient = DOWN);
    }
}

// dowel — the separately printed pin for pin_joint (glue in). Nominal `d`; the socket is
// d + 2*slop, so this drops in. Length spans both sockets.
module dowel(d = 6, len = 16, ang = 20, anchor = CENTER, spin = 0, orient = UP) {
    teardrop(h = len, d = d, ang = ang, anchor = anchor, spin = spin, orient = orient);
}
