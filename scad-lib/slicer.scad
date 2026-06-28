// slicer.scad — linear slab slicing. Splits a model into printable pieces in O(N), killing
// the O(2^N) blowup of nested BOSL2 partition() (see ../docs/slicing-blowup.md).
// Pure OpenSCAD, no library deps.
//
// Each piece is the source intersected with ONE slab (the region between two cut planes),
// so the child is evaluated once per piece — N cuts => N+1 pieces => N+1 evaluations, not 2^N.
//
// `cuts`  : coordinates along `axis`, in ASCENDING order, where the model is split.
// `axis`  : 0 = X, 1 = Y, 2 = Z (default X).
// `size`  : slab extent; MUST exceed the model's bounding box on every axis (default 500mm).
// `spread`: fan the pieces out along the cut axis by this much each (0 = leave assembled).
// `only`  : render just one piece by index (undef = all). Hook for per-piece render (6.1).
//
//   slice([-10, 20]) my_model();             // 3 pieces, assembled in place
//   slice([-10, 20], spread=40) my_model();  // same 3 pieces, fanned out for inspection
//   slice([0], axis=2, only=1) my_model();   // upper half only, cut on Z

// Cut planes padded with the outer ±size/2 sentinels — the N+1 slab boundaries.
function slice_boundaries(cuts, size = 500) = concat([-size / 2], cuts, [size / 2]);

// Pieces produced by `cuts` (N cuts -> N+1 pieces).
function slice_count(cuts) = len(cuts) + 1;

// True when `v` is non-decreasing — the cuts-must-ascend / within-bounds contract.
function _ascending(v, i = 0) =
    i >= len(v) - 1 ? true : (v[i] <= v[i + 1] && _ascending(v, i + 1));

module slice(cuts, axis = 0, size = 500, spread = 0, only = undef) {
    assert(axis == 0 || axis == 1 || axis == 2, "slice(): axis must be 0 (X), 1 (Y) or 2 (Z)");
    bounds = slice_boundaries(cuts, size);
    assert(_ascending(bounds), "slice(): cuts must be ascending and within ±size/2");
    for (i = [0 : len(bounds) - 2]) {
        if (is_undef(only) || only == i) {
            lo = bounds[i];
            hi = bounds[i + 1];
            dims = [for (a = [0:2]) a == axis ? hi - lo : size];
            center = [for (a = [0:2]) a == axis ? (lo + hi) / 2 : 0];
            offset = [for (a = [0:2]) a == axis ? i * spread : 0];
            translate(offset) intersection() {
                children();
                translate(center) cube(dims, center = true);
            }
        }
    }
}
