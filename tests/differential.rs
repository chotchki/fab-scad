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

use fab_scad::differ::{diff, drivers};
use fab_scad::openscad::find_bin;

/// Assert a snippet agrees across every registered driver (panics with the divergence on mismatch).
fn agree(scad: &str) {
    if let Err(why) = diff(scad) {
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
