// Smoke test for coupon.scad — both coupon types render without error. Run headless:
//   OPENSCADPATH="$PWD/libs:$PWD/scad-lib" openscad -o /dev/null.csg scad-lib/tests/test_coupon.scad
include <coupon.scad>

slop_coupon(type = "pin", d = 6, slops = [0, 0.1, 0.2]);
back(50) slop_coupon(type = "insert", screw = "M3", slops = [0, 0.1, 0.2]);
