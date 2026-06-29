// connectors.scad — joints across a slicer cut plane, BOSL2 attachment-style. The joint is built
// around the cut plane at the origin (CENTER anchor = the cut plane), bolt axis +Z; reorient with
// the standard `orient`/`spin`/`anchor`.
//
// Two joiners: the heat-set-insert + bolt (`bolt_joint`, removable), and the support-free `onion`
// (glue-free, no separate part — applied per-piece by the slicer). The onion REPLACED the old
// pin/dowel (a glued removable peg); bolt_joint stays. bolt_joint is all-NEGATIVE — tag it "remove"
// and diff it out before slicing so each piece keeps its half; wrap removes in tag_scope() so they
// don't leak when nested in another diff:
//
//   slice([cut], axis=UP)
//     tag_scope() diff()
//       my_model()                            // an attachable model
//         left(15) tag("remove") bolt_joint("M3", through = upper_thickness);
//
// Seeded from chotchki's projects (see the fastener-specs harvest): M3 default heat-set (5.0 × 6mm),
// garage_door boss params, $slop 0.1, onion peg→socket +0.2.
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
//
// `teardrop`: print support-free when the joint axis ends up HORIZONTAL on the bed — the
// clearance becomes a teardrop (BOSL2) and the insert pocket a teardrop too (point toward the
// joint axis's +Y, i.e. up once the piece is laid down). Drive it per piece with
// needs_teardrop($slice_axis, print_up) — the piece's print orientation decides it (4.5).
module bolt_joint(screw = "M3", through = 12, counterbore = 5, lead_in = 0.7,
                  teardrop = false, anchor = CENTER, spin = 0, orient = UP) {
    spec = _insert_spec(screw);
    idia = spec[0];
    idepth = spec[1];
    size = [max(idia, 10), max(idia, 10), through + idepth];
    attachable(anchor, spin, orient, size = size) {
        union() {
            up(through) screw_hole(str(screw, ",", through), head = "socket",
                                   counterbore = counterbore, teardrop = teardrop,
                                   anchor = TOP, orient = DOWN);
            if (teardrop) {
                // teardrop pocket: bore along -Z, point up (+Y of the teardrop) for support-free
                down(idepth) teardrop(h = idepth, d = idia, ang = 45, orient = DOWN, anchor = TOP);
            } else {
                cyl(d = idia, h = idepth, anchor = TOP);                       // insert pocket
                cyl(d1 = idia, d2 = idia + 2 * lead_in, h = lead_in, anchor = TOP); // lead-in
            }
        }
        children();
    }
}

// True when a joint along `axis` would print with an overhang (axis more than ~45° off the
// piece's build-up direction), so its holes need teardrop. Feed bolt_joint's `teardrop`.
function needs_teardrop(axis, up = UP) =
    abs(unit(axis) * unit(up)) < 0.71;   // |cos| < cos(45°) => axis is closer to horizontal

// auto-place a connector across a cut face: a cols×rows grid inset from the edges, centered
// on the cut plane. `face` = [w, h], the cut-plane footprint. Shared across connector types —
// children() is any connector (bolt_joint, …). Manual override: skip this and position the
// connector yourself.
//
//   cuboid([80,50,30]) connector_grid(face=[80,50], cols=3) tag("remove") bolt_joint("M3");
module connector_grid(face, cols = 2, rows = 1, inset = 15) {
    w = max(face[0] - 2 * inset, 0);
    h = max(face[1] - 2 * inset, 0);
    for (i = [0 : cols - 1], j = [0 : rows - 1]) {
        x = cols == 1 ? 0 : -w / 2 + w * i / (cols - 1);   // centered; no /0 for a single col/row
        y = rows == 1 ? 0 : -h / 2 + h * j / (rows - 1);
        translate([x, y, 0]) children();
    }
}

// ── onion joint (support-free, no separate part) ──────────────────────────────────────────
// A BOSL2 onion() centred on the cut plane (sphere equator at the cut, cap +Z). Unlike the bolt
// these are NOT both-sides negatives: the slicer applies them PER PIECE — UNION the peg into the
// lower piece, DIFF the socket from the upper piece — so one half grows a bump and the other a
// matching socket. Orient +Z along EACH piece's build-up (4.5 / #40) and it prints support-free:
// the exposed bump (upper hemisphere + cap) only narrows going up, and the socket's ceiling is the
// cap. `d` is the joint diameter; auto-sized from the cut's cross-section where placed (#41).

// onion_peg — MALE half. UNION into the lower piece: lower hemisphere merges into the piece, the
// upper hemisphere + cap stand proud as the self-supporting bump.
module onion_peg(d = 10, ang = 45, anchor = CENTER, spin = 0, orient = UP) {
    r = d / 2;
    attachable(anchor, spin, orient, r = r) {
        onion(r = r, ang = ang);
        children();
    }
}

// onion_socket — FEMALE half: the same onion grown by `slop`. tag("remove") and DIFF from the
// upper piece; the peg drops in. Cap-up so the cavity ceiling self-supports (opens downward).
module onion_socket(d = 10, slop = 0.2, ang = 45, anchor = CENTER, spin = 0, orient = UP) {
    r = d / 2 + slop;
    attachable(anchor, spin, orient, r = r) {
        onion(r = r, ang = ang);
        children();
    }
}
