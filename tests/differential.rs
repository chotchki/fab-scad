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

use fab_scad::differ::{diff, diff_echo, diff_files, drivers};
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

#[test]
fn whole_scope_hoisting_matches_the_oracle() {
    // I.2.7: geometry reflects the HOISTED value, so a hoisting bug renders a different solid.
    agree("sphere(x, $fn = 8); x = 5;"); // read-before-assign → sphere(5)
    agree("x = 1; sphere(x, $fn = 8); x = 9;"); // last-assignment-wins → sphere(9)
    agree("n = 1; n = n + 4; sphere(n, $fn = 8);"); // self-ref gotcha → sphere(undef)
    agree("b = 5; a = b; sphere(a, $fn = 8);"); // backward ref → sphere(5)
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
