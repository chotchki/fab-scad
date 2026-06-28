// Tests for slicer.scad. Run headless; asserts fail loudly and exit nonzero:
//   OPENSCADPATH="$PWD/libs:$PWD/scad-lib" openscad -o /dev/null.csg scad-lib/tests/test_slicer.scad
include <slicer.scad>

module test_slice_boundaries() {
    assert(slice_boundaries([], 100) == [-50, 50]);
    assert(slice_boundaries([-10, 20], 100) == [-50, -10, 20, 50]);
    assert(slice_boundaries([0], 8) == [-4, 0, 4]);
}
test_slice_boundaries();

module test_slice_count() {
    assert(slice_count([]) == 1);          // no cuts -> one piece
    assert(slice_count([0]) == 2);
    assert(slice_count([-10, 0, 20]) == 4); // N cuts -> N+1 pieces
}
test_slice_count();

module test_ascending() {
    assert(_ascending([-50, -10, 20, 50]) == true);
    assert(_ascending([0]) == true);
    assert(_ascending([]) == true);
    assert(_ascending([-50, 20, -10, 50]) == false);  // unsorted cuts
    assert(_ascending([-50, 600, 50]) == false);       // cut past +size/2
}
test_ascending();

// Marker geometry so headless render produces output (no "nothing to render" noise).
cube(0.01);
