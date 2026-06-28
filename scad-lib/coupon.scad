// coupon.scad — a printable tolerance-test strip for tuning connector slop before a full
// print. A row of small blocks, each with one test hole at a different slop and the slop
// value debossed beside it. Print it, find the block where the dowel/insert seats snugly,
// and feed that slop back as the connector default. (Modeled on models/slop_test.)
//
//   slop_coupon(type="pin",    d=6);       // dowel sockets at 0…0.25 slop
//   slop_coupon(type="insert", screw="M3"); // heat-set pockets at idia + 0…0.25
//
// `fab coupon` writes a driver that calls this and renders it.
include <BOSL2/std.scad>
include <connectors.scad>
include <version_stamp.scad>

module slop_coupon(type = "pin", d = 6, screw = "M3", depth = 8,
                   slops = [0, 0.05, 0.10, 0.15, 0.20, 0.25],
                   block = [18, 22, 10], gap = 4) {
    assert(type == "pin" || type == "insert", "slop_coupon: type must be \"pin\" or \"insert\"");
    base = type == "insert" ? _insert_spec(screw)[0] : d;   // hole diameter at slop 0
    for (i = [0 : len(slops) - 1]) {
        s = slops[i];
        right(i * (block.x + gap))
        tag_scope() diff()
            cuboid(block, anchor = BOTTOM, rounding = 1.5, edges = "Z") {
                // test hole, opening on the top face toward the back of the block
                position(TOP) back(block.y / 4) tag("remove") {
                    if (type == "pin")
                        teardrop(h = depth, d = base + s, ang = 45, anchor = TOP, orient = DOWN);
                    else
                        cyl(d = base + s, h = depth, anchor = TOP);
                }
                // slop value debossed on the top, toward the front
                position(TOP) fwd(block.y / 4) tag("remove")
                    version_stamp(str(s), size = 4.5, depth = 0.6, anchor = TOP);
            }
    }
}
