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

/// Assert a snippet's WARNING output agrees across every driver, in order (AN.13's second string-equal
/// channel). Same `cube` trick as [`agree_echo`] so the oracle's export succeeds.
fn agree_warnings(scad: &str) {
    let with_geometry = format!("{scad}\ncube(1);");
    if let Err(why) = fab_scad::differ::diff_warnings(&with_geometry) {
        panic!("warning divergence: {why}");
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
fn color_on_2d_matches_the_oracle() {
    // color() on a 2D shape TAGS the color (Shape2D::Color, for the GUI) but leaves the geometry untouched —
    // so the extruded/compared solid matches the oracle. Surfaced by the BOSL2 example corpus (L.3.7): it was
    // the single biggest bucket (343×), BOSL2's 2D examples using `color("red") <marker>` position dots.
    agree("linear_extrude(2) color(\"red\") square(10);");
    agree("linear_extrude(2) color(\"blue\") circle(5);");
    agree(
        "linear_extrude(1) { color(\"red\") circle(10); color(\"green\") translate([25, 0]) square(15); }",
    );
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
    agree_bosl2_body(
        "diff() cuboid([40, 25, 80]) { tag(\"remove\") left(5) cuboid([10, 10, 90]); }",
    );
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
fn resize_matches_the_oracle() {
    // L.5.1: resize() scales the child so its bbox hits `newsize` per axis — the factors come from the
    // BUILT child's measured extent (GeoNode::Resize, resolved in the backend), so it's pose-sensitive and
    // a wrong scale/axis blows the residual. A `0` axis is kept; an `auto` `0` axis scales proportionally.
    agree("resize([10, 20, 30]) cube(1);"); // full per-axis target
    agree("resize([20, 0, 0]) cube([10, 10, 10]);"); // x→20, y/z KEPT at 10
    agree("resize([20, 0, 0], auto = true) cube([10, 10, 10]);"); // auto: all axes ×2 → [20,20,20]
    agree("resize([0, 15, 0], auto = [true, false, true]) cube([10, 10, 10]);"); // y sized; x,z auto-proportional
    agree("resize([30, 20, 10]) sphere(5, $fn = 24);"); // non-uniform scale of a curved solid → ellipsoid
    agree("resize([12, 0, 0]) cylinder(h = 10, r = 3, $fn = 24);"); // keep h, widen r
}

#[test]
fn render_is_identity_vs_the_oracle() {
    // L.5.1: render() forces a nef/CGAL evaluation in OpenSCAD but is semantically identity — every fab
    // node is already an exact manifold, so render() just groups its children. Both engines' render() = the
    // child, so they agree; this proves fab's render() doesn't drop or alter geometry (the `convexity` hint
    // is inert).
    agree("render() cube(10);");
    agree(
        "render(convexity = 4) difference() { cube(10); translate([5, 5, 5]) sphere(6, $fn = 24); }",
    );
    agree("render() translate([5, 0, 0]) sphere(5, $fn = 24);");
    agree("difference() { cube(10); render() translate([5, 5, 5]) cube(8); }"); // render nested under a boolean
}

#[test]
fn child_block_assignments_are_not_children_but_bind() {
    // L.5.2: a child-block `assignment` is NOT a child — it counts toward neither `$children` nor a
    // `children(i)` index — but its binding IS in scope for every geometry child. This is the BOSL2logo
    // pattern (an `xdistribute()` block interleaving `sbez = …;` among the shapes, read by a sibling
    // `path_sweep(bezpath_curve(sbez))`), reduced to a hand distributor so the divergence is unambiguous.
    let dist = "module dist() { for (i = [0:$children-1]) translate([i*20, 0, 0]) children(i); }\n";
    // interleaved locals the geometry children READ; $children must be 2 (the two cubes), each seeing s / t.
    agree(&format!(
        "{dist}dist() {{ s = 6; cube(s); t = 8; cube([t, 3, 3]); }}"
    ));
    // $children arithmetic must EXCLUDE the middle assignment (3 stmts, 2 geometry children).
    agree(&format!(
        "{dist}dist() {{ cube(3); a = 4; sphere(a, $fn = 24); }}"
    ));
    // children() (all) must bind a leading local too.
    agree("module wrap() { children(); }\nwrap() { r = 5; sphere(r, $fn = 24); }");
    // a trailing assignment after the only child is a no-op (1 child), and the child still renders.
    agree(&format!("{dist}dist() {{ cube(4); unused = 99; }}"));
}

#[test]
fn nested_mutually_recursive_functions_match_the_oracle() {
    // L.5.4: `function`s defined in one BODY scope are mutually visible regardless of textual order (a
    // sibling defined LATER, and mutual recursion) — OpenSCAD's letrec-group semantics. A wrong resolution
    // would leave the call `undef` → a mis-sized/positioned primitive → the residual blows, so driving
    // geometry from the result validates the group binding against the real binary.
    // forward reference: g (first) calls h (defined 1 line BELOW it) — the `_gather_contiguous_edges` pattern.
    agree(
        "module m() { function g(n) = h(n) + 1; function h(n) = n * 2; translate([g(3), 0, 0]) cube(2); } m();",
    );
    // mutual recursion: even/odd ping-pong between two body functions.
    agree(
        "module m() { function ev(n) = n == 0 ? true : od(n - 1); function od(n) = n == 0 ? false : ev(n - 1); translate([ev(6) ? 8 : 0, 0, 0]) cube(2); } m();",
    );
    // a body function reading an ENCLOSING local (the L.2.8m capture) still works alongside the group.
    agree("module m() { k = 4; function scaled(n) = n * k; cube(scaled(3)); } m();"); // cube(12)
    // self-recursion (the self_name path) next to a sibling call — both must resolve.
    agree(
        "module m() { function fact(n) = n <= 1 ? 1 : n * fact(n - 1); function twice(n) = fact(n) * 2; cube(twice(3)); } m();",
    ); // cube(12)
}

#[test]
fn failed_assert_exports_pre_assert_geometry_like_the_oracle() {
    // L.5.8: a failed assert is NOT fatal — OpenSCAD prints the ERROR but still exports the top-level
    // geometry accumulated BEFORE the failing statement, and fab now matches. The differential compares the
    // PARTIAL solid both engines produce (a wrong boundary → residual/genus mismatch, so this is a real gate).
    agree("cube(10); assert(false); translate([20, 0, 0]) cube(5);"); // both → cube(10) only (cube(5) unreached)
    agree("cube(8); assert(1 > 2); sphere(5, $fn = 16);"); // stops at the assert → cube(8) only
    // an assert INSIDE an instantiated module halts the same way — the geometry before the call survives.
    agree("cube(8); module m() { assert(false); cube(3); } m(); cube(4);"); // → cube(8) only
    // a PASSING assert is transparent (both render the full thing).
    agree("assert(1 < 2); cube(6); assert(true, \"msg\") sphere(4, $fn = 16);");
    // NOTE: the pre-geometry case (`assert(false); cube(10);`) is deliberately NOT here — OpenSCAD writes an
    // EMPTY stl, which the oracle's mesh reader reports as `rejected` while fab reports `empty`; both produce
    // no geometry, so it's a harness empty-vs-rejected classification edge, not a divergence in the render.
}

#[test]
fn unknown_module_warns_and_continues_like_the_oracle() {
    // L.5.7: an unknown module instantiation renders NOTHING for that node (warn-and-continue), and the
    // REST of the program renders — bit-for-bit what OpenSCAD does ("Ignoring unknown module 'X'", exit 0).
    // This is the behavior a corpus naming a newer-BOSL2 module (hulling/force_tags) or a typo relies on.
    agree("cube(10); nonexistent_module_xyz();"); // sibling unknown → dropped, cube stays
    agree("difference() { cube(10); unknown_cut_module(); }"); // unknown boolean operand → no-op
    agree("cube(5); mystery_mod() { sphere(3, $fn = 16); }"); // unknown module WITH children → dropped whole
    agree("cube(5); translate([20, 0, 0]) also_unknown();"); // unknown under a transform → the transform is empty
}

#[test]
fn unknown_function_is_undef_like_the_oracle() {
    // L.5.7: an unknown function call evaluates to `undef` (warn-and-continue), matching OpenSCAD's
    // "Ignoring unknown function 'X'". A primitive sized by that undef renders nothing on BOTH engines, and
    // the sibling geometry still renders — so they agree.
    agree("cube(10); sphere(r = no_such_fn_abc(), $fn = 16);"); // sphere(undef) → empty both sides
    agree("cube(10); translate([20, 0, 0]) cube(mystery_size_fn(4));"); // cube(undef) under a transform
}

#[test]
fn missing_import_warns_and_continues_like_the_oracle() {
    // L.5.7: a missing import()/surface() file → warn + EMPTY mesh, the rest of the model renders — OpenSCAD's
    // "Can't open import file '…'", warn-and-render-without-it. Exercised through the FILE path (the loader's
    // needs fixpoint, where the tolerance lives), so it's a real gate vs the oracle on a dev box.
    agree_graph(
        "missing_import",
        &[(
            "root.scad",
            "cube(10);\ntranslate([20, 0, 0]) import(\"definitely_absent_part_xyz.stl\");",
        )],
        "root.scad",
        &[],
    );
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
    // KNOWN DIVERGENCE (M.7.3, fab strictly better): this region is an even-odd XOR of two
    // OVERLAPPING squares — the overlap is a hole touching the outline at two corner points.
    // OpenSCAD REJECTS the vertex-touching extrude outright; pre-flip, Clipper2 + C++ Extrude fell
    // over the same way (both sides "rejected" read as agreement). Our i_overlay splits the pinch
    // into clean contours and the extrude yields the analytically-exact solid (area 36+36−2·9 = 54),
    // so assert fab's CORRECT result instead of parity with a rejection.
    {
        let bosl2 = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/BOSL2");
        if bosl2.join("std.scad").exists() {
            let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bosl2_diff");
            std::fs::create_dir_all(&base).unwrap();
            let root = base.join("region_xor_overlapping_squares.scad");
            std::fs::write(
                &root,
                "include <std.scad>\nlinear_extrude(1) region([square(6), move([3, 3], square(6))]);\n",
            )
            .unwrap();
            let Outcome::Solid(solid) = drivers()[0].eval_file(&root, &[bosl2]) else {
                panic!("fab must render the even-odd region as a solid");
            };
            assert!(
                (solid.volume() - 54.0).abs() < 1e-9,
                "even-odd region volume {} != 54",
                solid.volume()
            );
            assert!(solid.is_manifold());
        }
    }
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

#[test]
fn duplicate_binding_rules_match_the_oracle() {
    // The CONTRACT half of the jit_diff fuzz trophy: a repeated name inside ONE `let` is FIRST-wins
    // upstream, but a repeated PARAMETER is LAST-wins — two OPPOSITE rules that look like one problem,
    // which is exactly how the JIT got it wrong (a `BTreeMap` insert is right for params, backwards for
    // `let`). These cases nail the rules to the real binary so neither can drift. Scope note: the driver
    // evaluates with `jit: false`, so this is the INTERPRETER vs the oracle — the JIT-tier half of the
    // trophy is pinned separately by `fast_eq_jit::duplicate_let_binding_declines`.
    agree_echo("echo(let(a = 1, a = 2) a);"); // first wins → 1
    agree_echo("echo([for (i = [0:0]) let(a = 1, a = 2) a]);"); // same rule in a comprehension
    agree_echo("function g(a, a) = a; echo(g(1, 2));"); // params: LAST wins → 2
    agree_echo("echo(let(a = 1) let(a = 2) a);"); // ordinary shadowing is untouched → 2
    // AN.9: a POSITIONAL binding binds NOTHING. `_` is a legal identifier, so parking the value under a
    // synthetic `_` clobbered a user's own — these read the OUTER `_`, and the `for` still iterates.
    agree_echo("_ = 42; echo(let(99) _);"); // → 42, not 99
    agree_echo("_ = 42; echo(let(1, 2) _);"); // → 42
    agree_echo("_ = 1; echo([for ([7, 8]) _]);"); // → [1,1]: iterates twice, binds nothing
    agree_echo("_ = 1; echo([for (i = [0:1], [5, 6]) i]);"); // → [0,0,1,1]
    // AN.7: the STATEMENT `let` obeys the same first-wins rule as the expression one.
    agree_echo("let(a = 1, a = 2) echo(a);"); // → 1
    agree_echo("let($fn = 3, $fn = 5) echo($fn);"); // → 3, the rule covers $-vars
    agree_echo("let(a = 1, a = 2, a = 3) { echo(a); }"); // → 1, a braced child block
    // Statement-level `for` is NOT first-wins — the generators nest, so the inner rebinds per iteration.
    agree_echo("for (i = [0:1], i = [8, 9]) echo(i);"); // → 8,9,8,9
    // AN.8: the C-style clause lists too. Last-wins here broke the LOOP, not just a value — a bad init
    // made the condition false immediately (empty), a bad update overshot and terminated after one pass.
    agree_echo("echo([for (i = 0, i = 10; i < 3; i = i + 1) i]);"); // → [0,1,2], not []
    agree_echo("echo([for (a = 0; a < 3; a = a + 1, a = a + 100) a]);"); // → [0,1,2], not [0]
    agree_echo("_ = 1; echo([for (i = 0; i < 2; i = i + 1, 99) [i, _]]);"); // → [[0,1],[1,1]]
    agree_echo("_ = 1; echo([for (i = 0, 77; i < 2; i = i + 1) [i, _]]);"); // → [[0,1],[1,1]]
    // AN.6: duplicate PARAMS are asymmetric — args set in order (last wins), then each still-unset
    // param takes its default in declaration order (FIRST wins). Both halves used to be last-wins.
    agree_echo("function g(a = 5, a, a = 9) = a; echo(g());"); // → 5, not 9
    agree_echo("function g(a = 7, a = 5) = a; echo(g());"); // → 7
    agree_echo("function g(a, b, a = 9) = [a, b]; echo(g(b = 2));"); // → [undef,2], not [9,2]
    agree_echo("function g(a, a) = a; echo(g(1, 2));"); // → 2, args stay last-wins
    agree_echo("function g(a, a = 5) = a; echo(g(1));"); // → 1, an arg beats a later default
    agree_echo("function g(a = 1, a = 2) = a; echo(g(a = 9));"); // → 9
    // MODULES bind on a separate path with the same rule — including the BOSL2-shaped case that
    // motivated the original two-phase comment (a param listed twice, once defaultless).
    agree_echo("module m(l, r, ang = 90, d, r = 0) { echo(r); } m(l = 1);"); // → undef, not 0
    agree_echo("module m(a = 5, a, a = 9) { echo(a); } m();"); // → 5
    agree_echo("module m(a, a = 5) { echo(a); } m(1);"); // → 1, an arg still beats a default
}

/// AN.10 — the INTRINSIC tier's version of the shadowing question. BOSL2's `is_vector` declares a
/// parameter named `all_nonzero` AND calls a FUNCTION named `all_nonzero`; per AD.1 a local holding a
/// function value wins in call position, so passing one redirects that inner call. The native impl
/// hardcodes the real function, so it answered as though the parameter weren't there.
#[test]
fn a_parameter_shadowing_an_intrinsics_own_dep_matches_the_oracle() {
    const BOSL2_SHAPED: &str = "\
_EPSILON = 1e-9;
function is_nan(x) = (x!=x);
function is_finite(x) = is_num(x) && !is_nan(0*x);
function all_nonzero(x, eps=_EPSILON) =
    is_finite(x)? abs(x)>eps :
    is_vector(x) && [for (xx=x) if(abs(xx)<eps) 1] == [];
function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) =
    is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0]
    && (is_undef(length) || (assert(is_num(length))len(v)==length))
    && (is_undef(zero) || ((norm(v) >= eps) == !zero))
    && (!all_nonzero || all_nonzero(v)) ;
";
    // A function value in the shadowing slot — both spellings, named and positional.
    agree_echo(&format!(
        "{BOSL2_SHAPED}echo(is_vector([1, 2], all_nonzero = function(x) false));"
    ));
    agree_echo(&format!(
        "{BOSL2_SHAPED}echo(is_vector([1, 2], undef, undef, function(x) false));"
    ));
    // The ordinary bool spellings must keep working — whatever the fix, it can't break these.
    agree_echo(&format!(
        "{BOSL2_SHAPED}echo(is_vector([1, 2], all_nonzero = true), is_vector([1, 0], all_nonzero = true));"
    ));
    agree_echo(&format!(
        "{BOSL2_SHAPED}echo(is_vector([1, 2]), is_vector([]));"
    ));
}

#[test]
fn duplicate_binding_warnings_match_the_oracle() {
    // AN.12/AN.13: the VALUE channel is blind to a whole class of divergence — these cases all agree on
    // what they compute and differ only in what they SAY. `agree_echo` would pass every one of them
    // even with the diagnostics deleted, which is exactly how they went missing. Scoped on purpose: our
    // warning coverage isn't upstream-complete, so this gate names the rules it owns rather than
    // sweeping every warning in the language.
    agree_warnings("echo(let(a = 1, a = 2) a);"); // Ignoring duplicate variable assignment "a" = 2
    agree_warnings("let(a = 1, a = 2, a = 3) { echo(a); }"); // twice, in order
    agree_warnings("_ = 42; echo(let(99) _);"); // Assignment without variable name 99
    agree_warnings("_ = 42; echo(let(1, 2) _);"); // twice
    agree_warnings("let(7) echo(1);"); // the STATEMENT form warns too
    agree_warnings("echo([for (i = 0, i = 10; i < 3; i = i + 1) i]);"); // the C-style init clause
    agree_warnings("echo([for (a = 0; a < 3; a = a + 1, a = a + 100) a]);"); // update: one per iteration
    agree_warnings("_ = 1; echo([for (i = 0; i < 2; i = i + 1, 99) [i, _]]);");
    agree_warnings("echo([for (i = [0:0]) let(a = 1, a = 2) a]);"); // the comprehension `let`
    // AN.14, the CALL-SITE four. Note the third and fourth: the same "a named arg landed on a taken
    // slot" event gets a DIFFERENT message depending on how the slot was taken, which is why the
    // matching walk tracks positional-vs-named rather than just filled-or-not.
    agree_warnings("function f(a, b) = [a, b]; echo(f(a = 1, a = 2));"); // supplied more than once
    agree_warnings("function f(a, b) = [a, b]; echo(f(1, a = 2));"); // overrides positional argument
    agree_warnings("function f(a, b) = [a, b]; echo(f(1, 2, 3));"); // Too many unnamed arguments
    // Overflow warns ONCE PER CALL, not once per surplus arg — the single-extra case above passed
    // even while the walk emitted per-argument, so it took a two-extra call to expose it.
    agree_warnings("module m(x) { echo(x); } m(1, 2, 3);");
    agree_warnings("module m(x) { echo(x); } m(1, 2, 3, 4, 5);");
    agree_warnings("function f(a) = a; echo(f(1, 2, 3, 4));");
    // …and it keeps its POSITION among the named-arg diagnostics: whichever event the argument list
    // reaches first is printed first, so these two orders are opposites.
    agree_warnings("module m(a) { echo(a); } m(1, x = 9, 2, 3);"); // not-specified, THEN overflow
    agree_warnings("module m(a) { echo(a); } m(1, 2, x = 9);"); // overflow, THEN not-specified
    agree_warnings("function g() = 1; echo(g(x = 1));"); // not specified as parameter
    agree_warnings("function g() = 1; echo(g($x = 1));"); // a $-arg is EXEMPT — no warning at all
    agree_warnings("function f(a, b) = [a, b]; echo(f(a = 1, 2));"); // no warning: 2 takes b
    agree_warnings("module m(p) { echo(p); } m(q = 5);"); // modules warn on the same rules
    agree_warnings("module m(p) { echo(p); } m(p = 1, p = 2);");
    // The reduced trophy body itself: the two readings differ far past a ULP (-2.0034 vs exactly -2).
    agree_echo(
        "function sq(s) = let(x = min(0.998, s), r = 1 + x*(sqrt(2)-1),
                              x = min(1.998, s), r = 1 + x*(sqrt(2)-1))
                          log(0.5) / log(r);
         echo(sq(1));",
    );
}

#[test]
fn a_parameter_shadowed_by_a_literal_warns_like_the_oracle() {
    // AN.15.2. Upstream fires this when a module body assigns one of its OWN parameter names to a
    // statically-decidable value — the argument the caller passed is dead before the body reads it.
    agree_warnings("module m(x = 1) { x = 5; echo(x); } m();");
    agree_warnings("module m(x) { x = 5; echo(x); } m();"); // a defaultless param counts too
    agree_warnings("module m(x = 1) { { x = 5; } echo(x); } m();"); // a bare block FOLDS IN
    agree_warnings("module m(x = 1) { x = 5; echo(x); } m(); m();"); // once per INSTANTIATION, not per module
    // Order is the body's FIRST-occurrence order, not the parameter declaration order.
    agree_warnings("module m(x = 1, y = 2) { y = 6; x = 5; echo(x, y); } m();");
    // The two silences that pin what "a literal" means. A binary op never qualifies however constant
    // it looks, and the check reads the HOISTED (last-wins) expression — so which of the two
    // assignments comes last decides it, and only one of these two programs warns.
    agree_warnings("y = 1; module m(x = 0) { x = y + 1; echo(x); } m();"); // silent: not a literal
    agree_warnings("y = 1; module m(x = 0) { x = y + 1; x = 5; echo(x); } m();"); // warns: last IS
    agree_warnings("y = 1; module m(x = 0) { x = 5; x = y + 1; echo(x); } m();"); // silent: last is NOT
    // Unary ops forward to their operand; a vector/range needs EVERY element literal.
    agree_warnings("module m(x = 0) { x = -5; echo(x); } m();");
    agree_warnings("module m(x = 0) { x = [1, 2]; echo(x); } m();");
    agree_warnings("y = 1; module m(x = 0) { x = [1, y]; echo(x); } m();"); // silent
    // Two scopes that do NOT collide: an `if` branch is its own, and a FUNCTION has no such warning.
    agree_warnings("module m(x = 1) { if (true) { x = 5; echo(x); } } m();"); // silent
    agree_warnings("function f(x = 1) = let(x = 5) x; echo(f());"); // silent
    // Against the AN.14 call-site diagnostics: those come FIRST, so this pins the interleaving of two
    // independently-emitted warning families, not just each one's presence.
    agree_warnings("module m(x) { x = 5; echo(x); } m(1, 2, 3);");
    agree_warnings("module m(x) { x = 5; echo(x); } m(q = 9);");
}

#[test]
fn an_overwritten_assignment_warns_like_the_oracle() {
    // AN.15.1. A STATIC pass, which these cases are chosen to prove rather than assume: the message
    // names the FIRST assignment's line, so a real line table has to reach the emission point.
    agree_warnings("a = 1;\nb = 2;\na = 3;\necho(a);");
    agree_warnings("a = 1;\na = 2;\na = 3;\necho(a);"); // twice, BOTH citing line 1
    agree_warnings("module m() { a = 1;\na = 3; }"); // an UNCALLED body — eval could never see this
    agree_warnings("echo(\"first\");\na = 1;\na = 2;"); // lands AHEAD of an echo on line 1
    // Emission order is the OVERWRITING site's, across scopes — `b` (line 2) before `a` (line 3),
    // though `a`'s scope is the outer one. A scope-at-a-time walk would print these backwards.
    agree_warnings("a = 1;\nmodule m() { b = 1; b = 2; }\na = 2;");
    // Scope boundaries: an `if` branch and a module body are their own, a bare block is NOT.
    agree_warnings("a = 1;\nif (true) { a = 2; echo(a); }\necho(a);"); // silent
    agree_warnings("a = 1;\nmodule m() { a = 2; }\necho(a);"); // silent
    agree_warnings("a = 1;\n{ a = 2; }\necho(a);"); // warns — the block folds in
    agree_warnings("if (true) { a = 1; a = 2; echo(a); }"); // each nested scope scanned on its own
    agree_warnings("translate([0, 0, 0]) { a = 1; a = 2; cube(a); }"); // children are a scope too
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
    std::fs::write(
        base.join("bump.dat"),
        "0 0 0 0\n0 5 5 0\n0 5 5 0\n0 0 0 0\n",
    )
    .unwrap();

    for (name, body) in [
        ("plain.scad", "surface(file=\"bump.dat\");\n"),
        (
            "centered.scad",
            "surface(file=\"bump.dat\", center=true);\n",
        ),
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
        &[(
            "model.scad",
            "use <nonexistent.scad>\ncube([10, 20, 30]);\n",
        )],
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

/// AC.2: 2D minkowski through the FULL stack (eval → Shape2D::Minkowski → kernel tiered sum →
/// extrude). Volumes ORACLE-PINNED (OpenSCAD 2026.06.12 rendered the same programs): square⊕circle
/// = 35.121…, concave-L⊕unit-square = exactly 21 (the closed-form smear).
#[test]
fn minkowski_2d_volumes_match_the_oracle() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("mink2d");
    std::fs::create_dir_all(&base).unwrap();
    let fab = &drivers()[0];

    let round = base.join("round.scad");
    std::fs::write(
        &round,
        "linear_extrude(1) minkowski() { square(4); circle(1, $fn = 32); }\n",
    )
    .unwrap();
    let Outcome::Solid(solid) = fab.eval_file(&round, &[]) else {
        panic!("square+circle minkowski must render");
    };
    assert!(
        (solid.volume() - 35.121).abs() < 1e-3,
        "rounded-square volume {} != oracle 35.121",
        solid.volume()
    );
    assert!(solid.is_manifold());

    let concave = base.join("concave.scad");
    std::fs::write(
        &concave,
        "p = [[0,0],[4,0],[4,2],[2,2],[2,4],[0,4]];\n\
         linear_extrude(1) minkowski() { polygon(p); square(1); }\n",
    )
    .unwrap();
    let Outcome::Solid(solid) = fab.eval_file(&concave, &[]) else {
        panic!("concave minkowski must render");
    };
    assert!(
        (solid.volume() - 21.0).abs() < 1e-9,
        "L-smear volume {} != 21",
        solid.volume()
    );
    assert!(solid.is_manifold());
}
