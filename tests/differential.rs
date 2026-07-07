//! The two-driver differential suite (the recon-gen / quicksight pattern): the SAME `.scad` snippet
//! through EVERY engine, asserting they agree. fab-lang is the baseline; the OpenSCAD binary is the
//! oracle. When the binary is absent the oracle leg skips cleanly (the "optional not required" gate),
//! so this is a real gate on a dev box + CI-with-OpenSCAD, and a fast no-op without it.
//!
//! DISCIPLINE (why chotchki insisted on this shape): a test may reach an engine ONLY through a
//! `differ::Driver` — never the raw evaluator entrypoint or the OpenSCAD binary directly. That keeps a
//! case from quietly hitting one engine and skipping the differential. Enforced below by
//! `no_test_bypasses_a_driver` (a source meta-lint) + `both_drivers_run_when_the_oracle_is_present`
//! (the both-legs gate). Add a driver and every case starts checking it for free.

use std::path::PathBuf;

use fab_scad::differ::{Outcome, diff, diff_echo, diff_files, drivers};
use fab_scad::openscad::find_bin;

/// Assert a snippet's GEOMETRY agrees across every registered driver (panics on mismatch).
fn agree(scad: &str) {
    if let Err(why) = diff(scad) {
        panic!("differential divergence: {why}");
    }
}

/// Assert a snippet's boolean residual vs the oracle is under `max` — a RELAXED gate for the extrude
/// classes where Manifold's tessellation differs from OpenSCAD's by a small, resolution-vanishing phase
/// artifact: twisted `linear_extrude` (J.3.4.1) and PARTIAL `rotate_extrude` (J.3.5). The shape is right,
/// the residual bounded + documented; full revolutions and un-twisted extrudes hold the strict `agree`
/// gate. Skips cleanly when the oracle binary is absent, like `agree`.
fn agree_within(scad: &str, max: f64) {
    if let Err(why) = fab_scad::differ::diff_within(scad, max) {
        panic!("relaxed-tolerance differential divergence: {why}");
    }
}

/// Assert a snippet's ECHO output agrees across every driver — the I.5 string-equal gate. A `cube` is
/// appended so the ORACLE's render (which captures echo alongside a mesh EXPORT) succeeds; a
/// geometry-less program has nothing to export. The echo lines are identical either way.
fn agree_echo(scad: &str) {
    let with_geometry = format!("{scad}\ncube(1);");
    if let Err(why) = diff_echo(&with_geometry) {
        panic!("echo differential divergence: {why}");
    }
}

/// Materialize a `use`/`include` FILE GRAPH under a fresh temp subdir, then assert its `root` file
/// agrees across every driver (`libs` are subdirs of the graph, joined into the oracle's OPENSCADPATH).
fn agree_graph(subdir: &str, files: &[(&str, &str)], root: &str, libs: &[&str]) {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("differential")
        .join(subdir);
    for (rel, contents) in files {
        let path = base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    let lib_paths: Vec<PathBuf> = libs.iter().map(|l| base.join(l)).collect();
    if let Err(why) = diff_files(&base.join(root), &lib_paths) {
        panic!("differential divergence: {why}");
    }
}

/// Assert a BOSL2 geometry `body` renders the same as the oracle. `include <std.scad>` is BOSL2's
/// REQUIRED form — its attachable system reads file-level constants + `$`-context from the caller scope,
/// which only `include` splices in (`use` does not). Skips cleanly when the `libs/BOSL2` submodule isn't
/// checked out (or the oracle binary is absent, in `diff_files`), so it's a real gate on a dev box + a
/// no-op elsewhere.
fn agree_bosl2_body(body: &str) {
    let bosl2 = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/BOSL2");
    if !bosl2.join("std.scad").exists() {
        return; // submodule not checked out — nothing to compare against
    }
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bosl2_diff");
    std::fs::create_dir_all(&base).unwrap();
    let safe: String = body
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(60)
        .collect();
    let root = base.join(format!("{safe}.scad"));
    std::fs::write(&root, format!("include <std.scad>\n{body};\n")).unwrap();
    if let Err(why) = diff_files(&root, &[bosl2]) {
        panic!("BOSL2 differential divergence: {why}");
    }
}

/// A BOSL2 2D `shape` (J.3.7), bridged to a unit-height solid via `linear_extrude(1)` so the
/// boolean-residual differential compares it (volume == area).
fn agree_bosl2(shape: &str) {
    agree_bosl2_body(&format!("linear_extrude(1) {shape}"));
}

/// A BOSL2 3D `shape` — an attachable solid or a VNF (J.2.6.3) — compared as-is (no extrude wrap).
fn agree_bosl2_solid(shape: &str) {
    agree_bosl2_body(shape);
}

#[test]
fn whole_scope_hoisting_matches_the_oracle() {
    // I.2.7: geometry reflects the HOISTED value, so a hoisting bug renders a different solid.
    agree("sphere(x, $fn = 8); x = 5;"); // read-before-assign → sphere(5)
    agree("x = 1; sphere(x, $fn = 8); x = 9;"); // last-assignment-wins → sphere(9)
    agree("n = 1; n = n + 4; sphere(n, $fn = 8);"); // self-ref gotcha → sphere(undef)
    agree("b = 5; a = b; sphere(a, $fn = 8);"); // backward ref → sphere(5)
}

#[test]
fn instantiation_modifiers_match_the_oracle() {
    // The `* ! % #` modifiers (parsed into `Modifiers`, honored in `eval_stmt`). Surfaced by the L.3 models
    // sweep: `*`-parked variants were rendering as REAL geometry, the top divergence-vs-oracle cause.
    // `*` disable + `%` background drop a subtree from the exported mesh entirely:
    agree("cube(10); *sphere(20);");
    agree("cube(10); %sphere(20);");
    // `#` highlight is a preview decoration with no effect on exported geometry:
    agree("cube(10); #translate([20, 0, 0]) sphere(5, $fn = 16);");
    // `!` root renders ONLY its subtree — ancestors (the outer translate) + siblings (the sphere) discarded,
    // but the `!`-node's OWN transform is kept (this needs the backend, so it lives here not in fab-lang):
    agree("translate([50, 0, 0]) !cube(10); sphere(20);"); // cube at origin, sphere gone
    agree("!translate([5, 0, 0]) cube(10); sphere(20);"); // own translate kept → cube at [5,15]
    agree("difference() { cube(30); !translate([5, 5, 5]) cube(10); }"); // ancestor difference dropped
}

#[test]
fn text_size_matches_the_oracle() {
    // text() glyph SIZE — the 100/72 DPI scale (L.3.6): OpenSCAD renders text through FreeType at 72 DPI
    // while treating `size` as 100-unit, so glyphs are 100/72 larger than the naive size/units_per_em; we
    // matched it. The bbox now agrees exactly with the oracle; the small RESIDUAL is Bézier curve-flattening
    // granularity (not size), so a RELAXED gate. Same bundled Liberation Sans both sides (default font).
    agree_within(
        "linear_extrude(2) text(\"AB\", size = 10, halign = \"center\", valign = \"center\");",
        6e-2,
    );
}

#[test]
fn revolved_vnf_shapes_match_the_oracle() {
    // The polyhedron/VNF leaf WELDS exact-coincident vertices (kernel `from_indexed`). A revolved VNF
    // duplicates its 360° closure ring (section N == section 0 as distinct indices), which reads as a
    // non-manifold OPEN seam → the whole leaf drops to empty without the weld. Surfaced by the L.3 sweep as
    // the dominant divergence (L.3.4): chamfered/rounded `cyl` + `teardrop` rendered NOTHING.
    agree_bosl2_body("cyl(d = 10, l = 20, chamfer = 1)");
    agree_bosl2_body("cyl(d = 10, l = 20, rounding = 2)");
    agree_bosl2_body("teardrop(d = 8, l = 12)");
    agree_bosl2_body("rotate_sweep([[1, 0], [3, 0], [3, 5], [1, 5]], 360)"); // a bare revolved profile
}

#[test]
fn assert_echo_passthrough_matches_the_oracle() {
    // `assert`/`echo` are passthrough — child geometry renders after the check/emit. Surfaced by the L.3 sweep:
    // BOSL2's `left()`/`fwd()` guard their `translate() children()` with a semicolon-less `assert`, so the
    // geometry is the assert's CHILD — dropping it rendered EMPTY.
    agree("assert(true) translate([5, 0, 0]) cube(10);");
    agree("echo(\"x\") cube(10);");
    agree_bosl2_body("left(5) cube([10, 10, 10])"); // the real trigger — a bare BOSL2 named transform
    agree_bosl2_body("diff() cuboid([40, 25, 80]) { tag(\"remove\") left(5) cuboid([10, 10, 90]); }");
}

#[test]
fn primitives_and_expressions_match_the_oracle() {
    agree("sphere(10, $fn = 32);");
    agree("cube([10, 20, 30]);");
    agree("cylinder(h = 10, r1 = 5, r2 = 2, $fn = 16);");
    agree("r = 3 + 4; sphere(r, $fn = 16);"); // expression value flows to geometry
    agree("sphere(max(3, 7), $fn = 16);"); // builtin value
}

#[test]
fn polyhedron_and_vnf_match_the_oracle() {
    // J.2.6.3: polyhedron() (with the winding fixed, J.2.6 — faces wound clockwise-from-outside get
    // reversed to Manifold's CCW) + BOSL2 VNF/attachable solids, vs the oracle by boolean residual.
    // Plain polyhedron primitives (no BOSL2) → the strict agree() gate:
    agree(
        "polyhedron(points = [[0, 0, 0], [10, 0, 0], [10, 10, 0], [0, 10, 0], [5, 5, 8]], \
         faces = [[0, 1, 2, 3], [0, 4, 1], [1, 4, 2], [2, 4, 3], [3, 4, 0]]);",
    ); // a square pyramid — a QUAD base face + 4 triangular sides
    agree(
        "polyhedron(points = [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], \
         faces = [[0, 1, 2], [0, 3, 1], [1, 3, 2], [2, 3, 0]]);",
    ); // a tetrahedron
    // BOSL2 VNF + attachable solids (include-based) — the shapes the polyhedron/VNF path drives:
    agree_bosl2_solid("spheroid(r = 5, $fn = 16)"); // a VNF sphere
    agree_bosl2_solid("cyl(h = 10, r = 4, $fn = 24)"); // an attachable cylinder
    agree_bosl2_solid("prismoid(size1 = [6, 6], size2 = [3, 3], h = 5)"); // a VNF prismoid
    agree_bosl2_solid("vnf_polyhedron(cube([4, 4, 4]))"); // a VNF fed straight to vnf_polyhedron
}

#[test]
fn transforms_match_the_oracle() {
    // J.2: transforms lower to GeoNode::Transform, walked through the Manifold backend. The
    // boolean-residual metric is tessellation-independent but POSE-sensitive — a wrong rotation order
    // or matrix would put the solid in the wrong place and blow the residual, so this validates the
    // 3x4 affine math (translate/rotate/scale/mirror/multmatrix) against the real binary.
    agree("translate([5, 0, 0]) cube(10);");
    agree("translate([1, 2, 3]) sphere(5, $fn = 24);");
    agree("rotate([0, 0, 45]) cube(10);"); // Euler about Z
    agree("rotate([90, 0, 0]) cylinder(h = 10, r = 3, $fn = 24);"); // about X, non-centered
    agree("rotate(30) cube([10, 2, 2]);"); // scalar rotate about Z
    agree("rotate(a = 90, v = [1, 1, 0]) cube([8, 2, 2]);"); // angle-axis
    agree("scale([2, 1, 0.5]) cube(10);");
    agree("mirror([1, 0, 0]) translate([5, 0, 0]) cube(4);"); // nested transform
    agree("multmatrix([[1, 0, 0, 5], [0, 1, 0, 2], [0, 0, 1, 0], [0, 0, 0, 1]]) cube(3);");
}

#[test]
fn booleans_and_multi_object_match_the_oracle() {
    // Now that the oracle-side re-import is f64-pure (MeshGL64, J.2.7.1), boolean-RESULT meshes read
    // back cleanly — including a DISJOINT multi-object union (a 2-component mesh) that f32 rejected.
    agree("cube(10); translate([20, 0, 0]) sphere(5, $fn = 24);"); // disjoint implicit union
    agree("cube(10); translate([5, 0, 0]) sphere(6, $fn = 24);"); // overlapping implicit union
    agree("union() { cube(10); translate([5, 5, 5]) sphere(6, $fn = 24); }");
    agree("difference() { cube(10); translate([5, 5, 5]) sphere(6, $fn = 24); }");
    agree("intersection() { cube(10); sphere(7, $fn = 24); }");
    agree("difference() { cube(10); cube(5); }"); // first minus the rest
    agree("translate([2, 0, 0]) difference() { cube(10); sphere(6, $fn = 24); }"); // transform of a boolean
    agree("for (i = [0:2]) translate([i * 12, 0, 0]) cube(5);"); // for-loop union → the oracle
    agree("{ cube(4); translate([6, 0, 0]) cube(4); }"); // a bare block groups + unions
}

#[test]
fn dimension_mixing_that_resolves_to_3d_matches_the_oracle() {
    // J.3.2.1: 2D/3D mixing where 3D WINS — the first non-null child fixes the dimension and the
    // mismatched 2D children drop, so the surviving 3D solid must match the oracle (which drops the same
    // ones). This is the subset the 3D differential can compare LIVE today; the 2D-winning cases become
    // live cases once linear_extrude bridges them to a solid (J.3.4). The WARNING text isn't compared
    // here — that's the warning-differential channel (#94); GEOMETRY agreement is what this pins.
    agree("cube(2); circle(5);"); // 3D first → the cube; the 2D circle dropped
    agree("union() { cube(2); circle(5); }"); // same under an explicit union
    agree("difference() { { } cube(4, center = true); }"); // an empty {} block drops out → the cube
}

#[test]
fn linear_extrude_matches_the_oracle() {
    // J.3.4: the UN-TWISTED sweep — prism + per-axis scale — lowers through Manifold's extrude and
    // matches the oracle by boolean residual under the strict 1e-3 gate. (Twist rides its own relaxed-
    // tolerance test below, J.3.4.1.)
    agree("linear_extrude(5) square(4);");
    agree("linear_extrude(5, center = true) square(4);");
    agree("linear_extrude(10, scale = 2) square(4, center = true);"); // frustum
    agree("linear_extrude(10, scale = [2, 0.5]) square(4, center = true);"); // anisotropic
    agree("linear_extrude(3) circle(4, $fn = 32);"); // a curved profile
}

#[test]
fn twisted_linear_extrude_matches_the_oracle() {
    // J.3.4.1: the twist loft — negate the sign (Manifold spins the OPPOSITE way from OpenSCAD) + resample
    // each profile edge into `round(edge/perimeter · $fn)` segments (OpenSCAD's exact rule). The SHAPE
    // matches; a small per-slice tessellation-phase residual remains that VANISHES with resolution.
    //
    // ACCEPTED, DOCUMENTED divergence (chotchki's call): rectilinear profiles at reasonable $fn agree
    // within 2% (0.4–1.5% measured), pinned here. Curved / low-$fn profiles carry a larger BUT bounded
    // residual — measured worst ~6% at $fn=16, ~4% for a twisted circle — a known edge that shrinks as
    // $fn climbs; the exact slice-phase match stays open in J.3.4.1. `agree_within` leans on the relative
    // residual for this class, `agree`'s hard 1e-3 gate is unchanged for everything else.
    let t = 0.02;
    agree_within(
        "linear_extrude(10, twist = 90, $fn = 32) square(4, center = true);",
        t,
    );
    agree_within(
        "linear_extrude(10, twist = 45, $fn = 32) square([4, 2], center = true);",
        t,
    );
    agree_within(
        "linear_extrude(8, twist = -90, $fn = 32) square([5, 3], center = true);",
        t,
    ); // negative
    agree_within(
        "linear_extrude(10, twist = 180, $fn = 64) square(6, center = true);",
        t,
    );
}

#[test]
fn rotate_extrude_matches_the_oracle() {
    // J.3.5: revolve a 2D profile about +Z. FULL revolutions (the common case) match OpenSCAD under the
    // STRICT 1e-3 gate — the segment count (`$fn`, else `$fa`/`$fs` on the profile's max radius) and the
    // ring/segment tessellation line up exactly, including the `$fn`-unset default. Profile placement
    // (X = radius, Y = height) and the axis both check out via the boolean residual.
    agree("rotate_extrude($fn = 64) translate([10, 0]) square([2, 3]);"); // a square ring
    agree("rotate_extrude($fn = 6) translate([10, 0]) square([2, 3]);"); // coarse → a hex sweep
    agree("rotate_extrude($fn = 64) translate([10, 0]) circle(2, $fn = 32);"); // a torus
    agree("rotate_extrude() translate([8, 0]) circle(2);"); // $fn unset → $fa/$fs from the ring radius
    agree("rotate_extrude($fn = 48) polygon([[4, 0], [7, 0], [7, 2], [5, 5], [4, 3]]);"); // a profile poly
}

#[test]
fn partial_rotate_extrude_matches_the_oracle() {
    // J.3.5: a PARTIAL revolution (angle < 360) leaves two end caps and a proportional arc. Same family
    // as the twist (J.3.4.1) — Manifold's arc tessellation vs OpenSCAD's differs by a small, resolution-
    // vanishing phase artifact (0.2–2% measured, converging as $fn climbs), an ACCEPTED, DOCUMENTED
    // divergence behind the relaxed per-class tolerance; full revolutions stay on the strict gate above.
    let t = 0.025;
    agree_within(
        "rotate_extrude(angle = 90, $fn = 64) translate([10, 0]) square([2, 3]);",
        t,
    );
    agree_within(
        "rotate_extrude(angle = 180, $fn = 64) translate([10, 0]) square([2, 3]);",
        t,
    );
    agree_within(
        "rotate_extrude(angle = 270, $fn = 48) translate([10, 0]) square([2, 3]);",
        t,
    );
    agree_within(
        "rotate_extrude(angle = 45, $fn = 32) translate([10, 0]) circle(2, $fn = 24);",
        t,
    );
}

#[test]
fn projection_matches_the_oracle() {
    // J.3.6: the 3D→2D bridge, the inverse of the extrudes. `cut = false` is the shadow (the whole solid
    // flattened onto XY); `cut = true` slices at z = 0. A bare 2D result compares trivially on the 3D
    // axis (both empty), so each case re-extrudes the projection to a unit-height solid whose VOLUME is
    // the projected AREA — the existing boolean-residual differential then re-runs OpenSCAD on it. All
    // pass the STRICT 1e-3 gate (no phase artifact — a projection is exact, not a swept tessellation).
    agree("linear_extrude(1) projection() sphere(5, $fn = 32);"); // shadow of a sphere → a disk
    agree("linear_extrude(1) projection() translate([0, 0, 3]) cube(6, center = true);"); // lifted cube
    agree("linear_extrude(1) projection() cylinder(h = 10, r = 4, $fn = 24);"); // shadow of a cylinder
    agree("linear_extrude(1) projection(cut = true) sphere(5, $fn = 32);"); // equatorial slice
    agree("linear_extrude(1) projection(cut = true) cube(6, center = true);"); // a square slice
    agree(
        "linear_extrude(1) projection(cut = true) rotate([30, 0, 0]) cylinder(h = 10, r = 4, center = true, $fn = 32);",
    ); // a tilted-cylinder slice → an ellipse-ish section
}

#[test]
fn bosl2_2d_shapes_match_the_oracle() {
    // J.3.7: real BOSL2 path/region-derived 2D shapes through the WHOLE 2D stack — attachable modules,
    // path math, polygon, offset, region booleans — against the oracle. This is what the use-scope fix
    // (a `use`d/`include`d function reads its file's constants) + the even-odd polygon fill (a BOSL2 path
    // winds clockwise; even-odd fills it, `Positive` dropped it to empty) unlocked together.
    // MODULE forms — attachable shapes:
    agree_bosl2("rect([6, 4])");
    agree_bosl2("rect([6, 4], rounding = 1)"); // offset-derived rounded corners
    agree_bosl2("star(n = 5, r = 6, ir = 3)"); // a clockwise path → even-odd fill
    agree_bosl2("hexagon(d = 8)");
    agree_bosl2("regular_ngon(n = 7, r = 5)");
    agree_bosl2("ellipse(d = [8, 5])");
    agree_bosl2("teardrop2d(r = 5)");
    agree_bosl2("glued_circles(d = 6, spread = 8)"); // a region — two circles + a connector
    agree_bosl2("supershape(m1 = 6, n1 = 1, r = 5)"); // a superformula path
    // FUNCTION forms → polygon(path): the shape called AS A FUNCTION returns its path, fed to polygon().
    // These asserted on `undef` constants before the use-scope fix.
    agree_bosl2("polygon(star(n = 5, r = 6, ir = 3))");
    agree_bosl2("polygon(circle(r = 5, $fn = 7))");
    agree_bosl2("polygon(hexagon(d = 8))");
    agree_bosl2("polygon(path2d(square(5)))");
    // 2D booleans + offset over BOSL2 shapes (regions):
    agree_bosl2("difference() { rect([10, 8], rounding = 2); circle(d = 4); }");
    agree_bosl2("offset(r = 1) star(n = 6, r = 5, ir = 2.5)");
    agree_bosl2("region([square(6), move([3, 3], square(6))])");
}

#[test]
fn extrude_brings_the_2d_catches_live() {
    // The J.3.2.1/J.3.3 2D catches were pinned as unit tests with oracle-derived LITERALS. linear_extrude
    // bridges them to a 3D solid whose VOLUME is the 2D area, so the EXISTING boolean-residual differential
    // now re-runs OpenSCAD on them — the frozen literals become live oracle comparisons.
    agree("linear_extrude(1) offset(r = 2, $fn = 64) square(5);"); // rounded offset
    agree("linear_extrude(1) offset(delta = 2) square(5);"); // mitered
    agree("linear_extrude(1) offset(delta = 2, chamfer = true) square(5);"); // chamfer = jtSquare (the bug)
    agree("linear_extrude(1) offset(-1) square(5);"); // shrink
    agree("linear_extrude(1) difference() { square(4); translate([2, 2]) square(4); }"); // 2D difference
    agree("linear_extrude(1) intersection() { square(4); translate([2, 2]) square(4); }"); // 2D intersection
    agree("linear_extrude(1) { square(4); translate([2, 2]) square(4); }"); // 2D implicit union
    agree("linear_extrude(1) polygon([[0, 0], [4, 0], [2, 3]]);"); // polygon primitive
    agree("linear_extrude(1) circle(5, $fn = 6);"); // circle $fn parity, extruded
    agree("linear_extrude(1) translate([3, 4]) scale([2, 3]) square(1);"); // 2D transform chain
}

#[test]
fn use_include_loader_matches_the_oracle() {
    // The loader's core semantics, validated against the real binary (constant-returning functions, so
    // we stay clear of the known use-imported-fn-sees-root-scope gap). Single-object, no cycle/diamond
    // (those are our LOUD defers — a deliberate oracle divergence covered self-consistently in
    // lang/tests/loader_corpus.rs).
    //
    // include splices a var into the shared scope → geometry sees it:
    agree_graph(
        "inc_var",
        &[
            ("consts.scad", "size = 7;\n"),
            (
                "root.scad",
                "include <consts.scad>\nsphere(size, $fn = 24);\n",
            ),
        ],
        "root.scad",
        &[],
    );
    // use imports a function → feeds geometry:
    agree_graph(
        "use_fn",
        &[
            ("lib.scad", "function r() = 8;\n"),
            ("root.scad", "use <lib.scad>\nsphere(r(), $fn = 24);\n"),
        ],
        "root.scad",
        &[],
    );
    // last-USE-wins: two libs define r(), the later use wins (b → 5, not a → 8):
    agree_graph(
        "use_order",
        &[
            ("a.scad", "function r() = 8;\n"),
            ("b.scad", "function r() = 5;\n"),
            (
                "root.scad",
                "use <a.scad>\nuse <b.scad>\nsphere(r(), $fn = 24);\n",
            ),
        ],
        "root.scad",
        &[],
    );
    // local def BEATS the used def (position-independent):
    agree_graph(
        "local_wins",
        &[
            ("lib.scad", "function r() = 8;\n"),
            (
                "root.scad",
                "use <lib.scad>\nfunction r() = 3;\nsphere(r(), $fn = 24);\n",
            ),
        ],
        "root.scad",
        &[],
    );
    // library-path resolution: the lib lives under libs/, reachable only via OPENSCADPATH:
    agree_graph(
        "lib_path",
        &[
            ("libs/pathlib.scad", "function pr() = 6;\n"),
            ("root.scad", "use <pathlib.scad>\nsphere(pr(), $fn = 24);\n"),
        ],
        "root.scad",
        &["libs"],
    );
}

#[test]
fn echo_output_matches_the_oracle() {
    // I.5: the number formatter + quoting + named-arg rendering, validated against the real binary's
    // ECHO: console line-for-line (not just against my probes).
    agree_echo("echo(9); echo(9.5); echo(-42);"); // integers + short decimals
    agree_echo("echo(1 / 3, 2 / 3, 10 / 3);"); // 6-sig-fig rounding
    agree_echo("echo(1e6, 1e7, 1e21, 1e-6, 1e-5, 1e-4);"); // scientific crossover, both ends
    agree_echo("echo(\"hi\", a = 5, [1, 2, 3]);"); // quoting + named args + a list
    agree_echo("echo(true, false, undef);");
    agree_echo("echo(1 / 0, -1 / 0, 0 / 0);"); // inf / -inf / nan
    agree_echo("echo([1.5, \"a\", true, undef]);"); // heterogeneous list
}

// ─────────────────────── enforcement (the discipline, AS tests) ──────────────────────────────────

#[test]
fn both_drivers_run_when_the_oracle_is_present() {
    // The both-legs gate: when OpenSCAD is installed, drivers() MUST include it — otherwise every
    // agree() case would be a vacuous fab-lang-only pass with the oracle silently dropped.
    let names: Vec<_> = drivers().iter().map(|d| d.name()).collect();
    assert!(names.contains(&"fab-lang"), "fab-lang is always a driver");
    if find_bin().is_some() {
        assert!(
            names.contains(&"openscad"),
            "OpenSCAD is installed but not a registered driver — the oracle leg would silently skip"
        );
    } else {
        eprintln!("note: OpenSCAD not found — oracle leg skipped (the optional-not-required gate)");
    }
}

#[test]
fn import_stl_matches_the_oracle() {
    // M.6: validate M.5.1's STL import against the oracle. Generate the fixture from a KNOWN-valid cube
    // (Solid::cube → STL bytes) so both engines import a real manifold — a hand-wound soup could make BOTH
    // reject and "agree" falsely (the trap chotchki flagged). Our reader dedups the soup back; OpenSCAD
    // welds its own way; the boolean-residual / vertex-multiset metric tolerates the tessellation route.
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("differential")
        .join("import_stl");
    std::fs::create_dir_all(&base).unwrap();
    let cube_stl = fab_scad::kernel::Solid::cube(10.0, 10.0, 10.0, false).to_stl_bytes();
    std::fs::write(base.join("cube.stl"), cube_stl).unwrap();
    let root = base.join("model.scad");
    std::fs::write(&root, "import(\"cube.stl\");\n").unwrap();

    // Guard the false-positive: our leg must produce a REAL solid, not a rejection that would trivially
    // "agree" with an oracle rejection. (The FabLang driver is always first — the pure-Rust baseline.)
    let fab = drivers().into_iter().next().unwrap();
    assert!(
        matches!(fab.eval_file(&root, &[]), Outcome::Solid(_)),
        "import(cube.stl) must lower to a real solid, not a rejection"
    );

    if let Err(why) = diff_files(&root, &[]) {
        panic!("import STL differential divergence: {why}");
    }
}

#[test]
fn surface_dat_matches_the_oracle() {
    // M.5.2: a DAT heightmap through surface(), both engines, boolean-residual. Our tessellation
    // (cell-center fan on top + grid-mirror base + walls) must be the SAME solid as OpenSCAD's surface.cc.
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("differential")
        .join("surface_dat");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("bump.dat"), "0 0 0 0\n0 5 5 0\n0 5 5 0\n0 0 0 0\n").unwrap();

    for (name, body) in [
        ("plain.scad", "surface(file=\"bump.dat\");\n"),
        ("centered.scad", "surface(file=\"bump.dat\", center=true);\n"),
    ] {
        let root = base.join(name);
        std::fs::write(&root, body).unwrap();
        // Guard the both-rejected false-pass: our surface must lower to a real solid.
        let fab = drivers().into_iter().next().unwrap();
        assert!(
            matches!(fab.eval_file(&root, &[]), Outcome::Solid(_)),
            "{name}: surface must lower to a real solid, not a rejection"
        );
        if let Err(why) = diff_files(&root, &[]) {
            panic!("surface DAT differential divergence ({name}): {why}");
        }
    }
}

#[test]
fn a_missing_use_warns_and_renders_like_the_oracle() {
    // M.6.1: a missing use/include is warn-and-RENDER (exit 0) in BOTH engines — the reference drops to
    // nothing (no statements, no defs) and the rest of the program renders. cube uses no def from the
    // missing lib, so the render is well-defined; both engines must land the same cube.
    agree_graph(
        "missing_use",
        &[("model.scad", "use <nonexistent.scad>\ncube([10, 20, 30]);\n")],
        "model.scad",
        &[],
    );
}

#[test]
fn no_test_bypasses_a_driver() {
    // The no-leak meta-lint (the recon-gen no-playwright-leak analog): this suite may touch an engine
    // ONLY through a differ::Driver. Scanning our own source, the raw engine entrypoints must not
    // appear — so a case can't quietly hit one engine and skip the differential. Patterns are built by
    // concatenation so this check never matches its OWN source.
    let src = include_str!("differential.rs");
    let forbidden = [
        ["fab_lang", "::evaluate"].concat(), // the evaluator: must go through the FabLang driver
        ["oracle", "::run"].concat(), // the oracle runner: must go through the OpenScad driver
        ["Openscad", "::discover"].concat(), // no direct oracle-runner construction
    ];
    for pat in &forbidden {
        assert!(
            !src.contains(pat.as_str()),
            "differential.rs reaches an engine directly ({pat}) — route it through a differ::Driver"
        );
    }
}
