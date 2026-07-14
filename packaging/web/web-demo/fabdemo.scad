// A tiny self-contained lib the fab-gui web boot INCLUDEs (W.3.6 Stage 2) — proves the
// fetch->closure->worker include path with pure CSG, no BOSL2, so the browser smoke is fast + doesn't
// hinge on scad-rs's BOSL2 coverage.

// A rounded-ish bracket: a slab, a raised boss with a bore, a chamfered lightening slot.
module fab_bracket(w = 60, d = 40, h = 22, bore = 12) {
    difference() {
        union() {
            cube([w, d, h / 2], center = true);
            translate([0, 0, h / 4]) cube([w * 0.55, d * 0.6, h / 2], center = true);
        }
        translate([0, 0, 2]) cylinder(h = h, r = bore / 2, center = true);
        translate([-w / 4, 0, -h / 4]) rotate([0, 45, 0]) cube([h, d + 2, h], center = true);
    }
}
