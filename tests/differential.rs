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
