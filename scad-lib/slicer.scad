// slicer.scad — linear slab slicing. Splits a model into printable pieces in O(N), killing
// the O(2^N) blowup of nested BOSL2 partition() (see ../docs/slicing-blowup.md).
//
// BOSL2-style: an operator on children (like partition()/distribute()), not an attachable
// shape — attachable() would shadow children(), and children() IS the model to cut. Each
// piece is the source intersected with ONE slab, so the child is evaluated once per piece
// (N+1 evaluations for N cuts, not 2^N).
//
// Per piece it exposes the cut context as $-vars so the connector layer (4.4–4.6) can place
// joints on the cut faces:
//   $idx        piece index (0-based)
//   $slice_n    number of pieces
//   $slice_axis cut-axis unit vector (RIGHT / BACK / UP)
//   $slice_lo   coordinate of the cut plane below this piece (undef at the model's low end)
//   $slice_hi   coordinate of the cut plane above this piece (undef at the model's high end)
//
// `cuts`  : coordinates along `axis`, ASCENDING, where the model is split.
// `axis`  : RIGHT / BACK / UP (or 0 / 1 / 2). Default RIGHT (X).
// `size`  : slab extent; MUST exceed the model's bounding box on every axis (default 500mm).
// `spread`: fan pieces out along the cut axis by this much each (0 = leave assembled).
// `only`  : render just one piece by index (undef = all). Hook for per-piece render (6.1).
//
//   slice([-10, 20]) my_model();              // 3 pieces, assembled
//   slice([0], axis=UP, spread=40) my_model(); // cut on Z, fanned out
//   slice([0], only=1) my_model();             // upper piece only
include <BOSL2/std.scad>

// Cut planes padded with the outer ±size/2 sentinels — the N+1 slab boundaries.
function slice_boundaries(cuts, size = 500) = concat([-size / 2], cuts, [size / 2]);

// Pieces produced by `cuts` (N cuts -> N+1 pieces).
function slice_count(cuts) = len(cuts) + 1;

// True when `v` is non-decreasing — the cuts-must-ascend / within-bounds contract.
function _ascending(v, i = 0) =
    i >= len(v) - 1 ? true : (v[i] <= v[i + 1] && _ascending(v, i + 1));

// Map a direction vector or 0/1/2 to an axis index.
function _axis_index(axis) =
    is_vector(axis) ? (axis.x != 0 ? 0 : axis.y != 0 ? 1 : 2) : axis;

module slice(cuts, axis = RIGHT, size = 500, spread = 0, only = undef) {
    req_children($children);
    ai = _axis_index(axis);
    assert(ai == 0 || ai == 1 || ai == 2, "slice(): axis must be RIGHT/BACK/UP or 0/1/2");
    unit = [for (a = [0:2]) a == ai ? 1 : 0];
    bounds = slice_boundaries(cuts, size);
    assert(_ascending(bounds), "slice(): cuts must be ascending and within ±size/2");
    n = len(bounds) - 1;
    for (i = [0 : n - 1]) {
        if (is_undef(only) || only == i) {
            lo = bounds[i];
            hi = bounds[i + 1];
            dims = [for (a = [0:2]) a == ai ? hi - lo : size];
            center = [for (a = [0:2]) a == ai ? (lo + hi) / 2 : 0];
            $idx = i;
            $slice_n = n;
            $slice_axis = unit;
            $slice_lo = i == 0 ? undef : lo;       // cut plane below (undef at the model end)
            $slice_hi = i == n - 1 ? undef : hi;   // cut plane above
            move(unit * (i * spread)) intersection() {
                children();
                move(center) cuboid(dims);
            }
        }
    }
}
