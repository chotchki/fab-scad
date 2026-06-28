// part_number.scad — stamp a piece's index so a sliced set reassembles in order.
// Thin wrapper over version_stamp (same text3d-based, attachable look).
include <version_stamp.scad>

module part_number(n, size = 8, depth = 0.6, prefix = "", font = "Liberation Sans:style=Bold",
                   anchor = CENTER, spin = 0, orient = UP) {
    version_stamp(str(prefix, n), size = size, depth = depth, font = font,
                  anchor = anchor, spin = spin, orient = orient)
        children();
}
