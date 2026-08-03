// SZ.9.2/9.3 fixture — the model both the browser and the desktop render, so their geometry can be
// compared instead of assumed equal.
//
// Three jobs, and every line here is doing one of them:
//
//   1. `include <BOSL2/std.scad>` puts a `BOSL2/`-prefixed key in the include closure, which is what
//      `wanted_variant` routes on. Without it the app picks the LEAN worker and the gate quietly
//      stops testing the 3.85 MB artifact it was written for.
//   2. `rounding` + `$fn` drive real BOSL2 maths — trig, path offsetting, VNF assembly — so the
//      transpiled band has work to dispatch and the comparison has floats that could actually
//      disagree between wasm's libm and the system's. A plain `cube()` would match trivially and
//      prove nothing about the platforms.
//   3. It stays SMALL. This renders under software WebGL on a CI runner, twice (once in the browser,
//      once natively), inside the job's timeout.
//
// UNCOLORED on purpose: color is the save path's OTHER arm and `e2e-save.sh` already covers it. Here
// it would only add a per-vertex color array to diff, which is not what this gate is about.
include <BOSL2/std.scad>

$fn = 32;

difference() {
    cuboid([40, 30, 12], rounding = 3, edges = "Z");
    up(2) cyl(h = 20, d = 10);
    xcopies(26) down(4) cyl(h = 12, d = 4);
}
