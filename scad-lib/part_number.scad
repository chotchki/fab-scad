// part_number.scad — stamp a piece's index so a sliced set reassembles in order.
// Thin wrapper over version_stamp for a consistent look across pieces.
include <version_stamp.scad>

module part_number(n, size = 8, depth = 0.6, prefix = "", font = "Liberation Sans:style=Bold") {
    version_stamp(str(prefix, n), size = size, depth = depth, font = font);
}
