// Tests for connectors.scad. Run headless; asserts fail loudly and exit nonzero:
//   OPENSCADPATH="$PWD/libs:$PWD/scad-lib" openscad -o /dev/null.csg scad-lib/tests/test_connectors.scad
include <connectors.scad>

module test_insert_spec() {
    assert(_insert_spec("M3") == [5.0, 6]);   // the default, from 15+ projects
    assert(_insert_spec("M4") == [6.0, 6]);
    assert(_insert_spec("M5") == [7.0, 10]);
}
test_insert_spec();

// Each module instantiates and produces geometry without error (headless render check).
bolt_joint("M3", through = 12);
right(25) pin_joint(d = 6, depth = 8);
right(50) dowel(d = 6, len = 16);

// Tagged + attachable path (chotchki's idiom): attach a joint to a face and diff it out.
back(40) tag_scope() diff()
    cuboid([30, 30, 20])
        attach(TOP) tag("remove") bolt_joint("M3", through = 10);

module test_needs_teardrop() {
    assert(needs_teardrop(UP) == false);     // vertical bore prints support-free
    assert(needs_teardrop(RIGHT) == true);   // horizontal bore overhangs -> teardrop
    assert(needs_teardrop(BACK) == true);
}
test_needs_teardrop();
