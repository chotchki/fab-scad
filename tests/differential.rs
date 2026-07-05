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

use fab_scad::differ::{diff, diff_files, drivers};
use fab_scad::openscad::find_bin;

/// Assert a snippet agrees across every registered driver (panics with the divergence on mismatch).
fn agree(scad: &str) {
    if let Err(why) = diff(scad) {
        panic!("differential divergence: {why}");
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
