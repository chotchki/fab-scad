//! AR.37.2 — the acceptance suite, pointed at the SECOND library.
//!
//! `bosl2/tests/surface_diff.rs` is the same differential aimed at BOSL2, and running it there is
//! not the same test as running it here. The emitter was developed against BOSL2 for a whole phase;
//! every construct it handles, it handles because a BOSL2 function needed it. A gate over BOSL2
//! therefore proves the emitter is self-consistent, and says nothing about whether the subset it
//! implements is a general OpenSCAD subset or a BOSL2-shaped one.
//!
//! MCAD is the control. Never a target, ships WITH OpenSCAD, and shaped the other way round — 167
//! modules to 39 functions, where BOSL2 is 414 to 1329. AR.37.1 already got one real bug out of it
//! that BOSL2 structurally could not have produced (a leading digit is a legal OpenSCAD identifier
//! and an illegal Rust one, so `3dtri_draw` failed to compile). This is the standing version of
//! that probe.
//!
//! THE ORACLE IS THE INTERPRETER (`Config::intrinsics = false`) against the SAME registry, so the
//! only variable is which tier ran — the AR.2 contract. Comparison is over `Message::render`, the
//! whole console, because a tier that computes the right number while swallowing a warning is still
//! a divergence and no mesh comparison would see it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_gen::{NativeSurface, Profile, generate_against};
use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, Message};

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

/// A checkout without the submodule SKIPS rather than fails — the crate is designed to declare
/// nothing in that case (`transpiled()` answers false), and a test that fails on a missing optional
/// asset trains people to ignore it.
fn have_mcad() -> bool {
    fab_mcad::transpiled() && libs().join("MCAD/constants.scad").exists()
}

/// One seed's verdict, as a string, so a divergence prints what actually differed.
enum Leg {
    Ok(Vec<String>),
    Err(String),
}

fn run(src: &str, registry: &Registry, intrinsics: bool) -> Leg {
    let config = Config {
        intrinsics,
        ..Config::default()
    };
    match fab_lang::evaluate_geometry_with_registry(
        src,
        Path::new("."),
        &[libs()],
        registry,
        config,
    ) {
        Ok((_geo, msgs)) => Leg::Ok(msgs.iter().map(Message::render).collect()),
        Err(e) => Leg::Err(e.to_string()),
    }
}

/// Compare both tiers on one seed. Returns `Err(explanation)` on a divergence.
///
/// BOTH-ERRED-IDENTICALLY IS A PASS: a generated program can legitimately fail (a library `assert`,
/// an eval budget), and with arbitrary arguments driven into 39 callables plenty of them will. What
/// is not a pass is the two tiers disagreeing about WHETHER it fails.
fn diff_seed(seed: u32, registry: &Registry, surface: &NativeSurface) -> Result<(), String> {
    let src = generate_against(seed, Profile::AB, surface);
    match (run(&src, registry, false), run(&src, registry, true)) {
        (Leg::Ok(interp), Leg::Ok(native)) if interp == native => Ok(()),
        (Leg::Ok(interp), Leg::Ok(native)) => Err(format!(
            "seed {seed}: interp != NATIVE console\n  interp: {interp:?}\n  native: {native:?}\n{src}"
        )),
        (Leg::Err(a), Leg::Err(b)) if a == b => Ok(()),
        (Leg::Err(a), Leg::Err(b)) => Err(format!(
            "seed {seed}: both tiers erred, differently\n  interp: {a}\n  native: {b}\n{src}"
        )),
        (a, b) => Err(format!(
            "seed {seed}: one tier errored and the other did not (interp_ok={}, native_ok={})\n{src}",
            matches!(a, Leg::Ok(_)),
            matches!(b, Leg::Ok(_))
        )),
    }
}

/// The surface a consumer that loaded MCAD would generate against, and the registry that consumer
/// would evaluate with.
fn harness() -> (Registry, NativeSurface) {
    (
        Registry::new().with(fab_mcad::Mcad.rows()),
        NativeSurface::from_library(&fab_mcad::Mcad),
    )
}

/// THE GATE. Same budget as BOSL2's fast lane — each seed costs two full evaluations, which is
/// where the time goes.
#[test]
fn the_transpiled_band_agrees_with_the_interpreter_over_generated_programs() {
    if !have_mcad() {
        return;
    }
    let (registry, surface) = harness();
    let mut failures = Vec::new();
    for seed in 0..24 {
        if let Err(why) = diff_seed(seed, &registry, &surface) {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 24 seeds diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The DEEP lane — same comparison, far more seeds. `--ignored`, because the fast gate above is
/// what protects an ordinary commit.
#[test]
#[ignore = "deep lane: run after touching the emitter"]
fn the_transpiled_band_agrees_over_many_generated_programs() {
    if !have_mcad() {
        return;
    }
    let (registry, surface) = harness();
    let mut failures = Vec::new();
    for seed in 0..500 {
        if let Err(why) = diff_seed(seed, &registry, &surface) {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 500 seeds diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// NON-VACUITY, and this one earns its keep more here than it does over BOSL2.
///
/// Every assertion above passes trivially if the generator emits programs that call nothing, or if
/// the registry arms nothing — and a second library is exactly where that would happen silently,
/// because a wiring mistake looks identical to a clean run. So: the surface must be non-empty, the
/// registry must actually hold rows, and a generated program must contain a call to a name MCAD
/// declares rather than only builtins.
#[test]
fn the_gate_is_not_vacuous() {
    if !have_mcad() {
        return;
    }
    let (registry, surface) = harness();
    assert!(
        !fab_mcad::Mcad.callables().is_empty(),
        "MCAD declares nothing — the surface is empty and every seed above is a no-op"
    );
    assert!(
        !fab_mcad::Mcad.rows().functions.is_empty(),
        "MCAD compiled no functions — the native tier is empty and both legs are the interpreter"
    );
    let _ = &registry;

    let names: Vec<&str> = fab_mcad::Mcad.callables().iter().map(|d| d.name).collect();
    let mut called = 0_usize;
    for seed in 0..24 {
        let src = generate_against(seed, Profile::AB, &surface);
        if names.iter().any(|n| src.contains(n)) {
            called += 1;
        }
    }
    assert!(
        called > 0,
        "no generated program named a single MCAD callable — the gate compares two interpreters"
    );
}

/// The leading-digit names, pinned as their own case (AR.37.1).
///
/// `3dtri_*`, `8bit_polyfont` and `12ptStar` are why MCAD's band did not compile at first, and the
/// fix lives in the emitter's ident mapping rather than anywhere near MCAD. If it regresses, this
/// crate stops building — which is a loud failure and a confusing one, so the expectation is
/// written down where the confusion would land.
#[test]
fn the_leading_digit_names_are_declared() {
    if !have_mcad() {
        return;
    }
    let declared: Vec<&str> = fab_mcad::Mcad.callables().iter().map(|d| d.name).collect();
    assert!(
        declared
            .iter()
            .any(|n| n.starts_with(|c: char| c.is_ascii_digit())),
        "MCAD is supposed to declare at least one digit-leading name — if upstream renamed them \
         all, this crate stopped testing the thing it was added to test: {declared:?}"
    );
}
