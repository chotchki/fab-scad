//! AR.37.2 — the acceptance suite, pointed at the THIRD library.
//!
//! `mcad/tests/surface_diff.rs` is the generality CONTROL and explains why a second library is a
//! different KIND of test rather than more of the same one. This is the BREADTH: machineblocks is a
//! third-party parametric-brick library with no connection to BOSL2 or to MCAD, written in whatever
//! OpenSCAD style its author preferred. Two libraries agreeing could still be a coincidence of
//! style; three is starting to be evidence.
//!
//! It reads `libs/machineblocks/lib` only — the repository ships 529 `.scad` files and roughly 500
//! are generated part variants under `examples/` and `templates/`.
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
        .join("libs/machineblocks/lib")
}

/// A checkout without the submodule SKIPS rather than fails — the crate is designed to declare
/// nothing in that case (`transpiled()` answers false), and a test that fails on a missing optional
/// asset trains people to ignore it.
fn have_machineblocks() -> bool {
    fab_machineblocks::transpiled() && libs().join("block.scad").exists()
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
/// an eval budget), and with arbitrary arguments driven into 57 callables plenty of them will. What
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

/// The surface a consumer that loaded machineblocks would generate against, and the registry that consumer
/// would evaluate with.
fn harness() -> (Registry, NativeSurface) {
    (
        Registry::new().with(fab_machineblocks::MachineBlocks.rows()),
        NativeSurface::from_library(&fab_machineblocks::MachineBlocks),
    )
}

/// THE GATE. Same budget as BOSL2's fast lane — each seed costs two full evaluations, which is
/// where the time goes.
#[test]
fn the_transpiled_band_agrees_with_the_interpreter_over_generated_programs() {
    if !have_machineblocks() {
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
    if !have_machineblocks() {
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

/// NON-VACUITY — the same guard MCAD carries, for the same reason.
///
/// Every assertion above passes trivially if the generator emits programs that call nothing, or if
/// the registry arms nothing — and a second library is exactly where that would happen silently,
/// because a wiring mistake looks identical to a clean run. So: the surface must be non-empty, the
/// registry must actually hold rows, and a generated program must contain a call to a name machineblocks
/// declares rather than only builtins.
#[test]
fn the_gate_is_not_vacuous() {
    if !have_machineblocks() {
        return;
    }
    let (registry, surface) = harness();
    assert!(
        !fab_machineblocks::MachineBlocks.callables().is_empty(),
        "machineblocks declares nothing — the surface is empty and every seed above is a no-op"
    );
    assert!(
        !fab_machineblocks::MachineBlocks.rows().functions.is_empty(),
        "machineblocks compiled no functions — the native tier is empty and both legs are the interpreter"
    );
    let _ = &registry;

    let names: Vec<&str> = fab_machineblocks::MachineBlocks
        .callables()
        .iter()
        .map(|d| d.name)
        .collect();
    let mut called = 0_usize;
    for seed in 0..24 {
        let src = generate_against(seed, Profile::AB, &surface);
        if names.iter().any(|n| src.contains(n)) {
            called += 1;
        }
    }
    assert!(
        called > 0,
        "no generated program named a single machineblocks callable — the gate compares two interpreters"
    );
}
