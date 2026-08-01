//! AR.26.4.1 — TWO LIBRARIES IN ONE REGISTRY, and the property that has to survive it: handing in a
//! second library may only ADD dispatch, never subtract it.
//!
//! This is the test the phase needed and did not have. fab-lang ships 85 function rows and 34 pins,
//! 66 and 33 of which name BOSL2 functions this crate also compiles — so a consumer loading both is
//! the FIRST real case of two libraries claiming one name, and it is the normal case rather than an
//! edge one. AR.26.2 measured what the old name-owned index did with it: 65 rows dropped and 591 of
//! 866 in permanent guard decline, because a row whose OWN library's dep had been evicted could
//! never anchor. Nothing failed. Nothing printed. Every differential in the tree went on passing,
//! because a native that declines and a native that fires compute the same answer — which is exactly
//! why the guard here is a COUNT and not a comparison of results.
//!
//! The three shapes are all measured against one program, in one process, so the only variable is
//! which row sets went in and in what order.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, wired_count};

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

fn have_bosl2() -> bool {
    fab_bosl2::transpiled() && libs().join("BOSL2/std.scad").exists()
}

/// Real BOSL2 geometry — enough of the library in scope that the whole `std.scad` closure loads and
/// the interesting rows get a chance to arm.
const SRC: &str = "include <BOSL2/std.scad>\ncuboid(10, rounding=2);\n";

/// Evaluate `SRC` against `registry` and report how many natives wired.
fn wired(registry: &Registry) -> usize {
    fab_lang::evaluate_geometry_with_registry(
        SRC,
        Path::new("."),
        &[libs()],
        registry,
        Config::default(),
    )
    .expect("renders");
    wired_count()
}

/// ACCUMULATION IS MONOTONE. Adding fab-lang's rows to a registry that already has BOSL2's — 66
/// contested function names and 33 contested pins — must not cost a single native, in either order.
///
/// Both orders, because the answer used to depend on it: the loser of a name was evicted, so which
/// library got evicted decided how much of the index survived. A test that only checked one order
/// would have passed on the good one.
#[test]
fn a_second_library_never_costs_dispatch() {
    if !have_bosl2() {
        return; // no submodule: nothing to accumulate
    }
    let bosl2 = fab_bosl2::Bosl2.rows();
    let hand = fab_lang::surface::Natives.rows();

    let alone = wired(&Registry::new().with(bosl2));
    assert!(
        alone > 800,
        "the differential is vacuous unless the library actually armed: {alone}"
    );
    let hand_first = wired(&Registry::new().with(hand).with(bosl2));
    let bosl2_first = wired(&Registry::new().with(bosl2).with(hand));

    assert!(
        hand_first >= alone && bosl2_first >= alone,
        "handing in a second library subtracted dispatch: BOSL2 alone {alone}, \
         fab-lang first {hand_first}, BOSL2 first {bosl2_first}"
    );
    assert_eq!(
        hand_first, bosl2_first,
        "which library was handed over first must not change how much wires"
    );
}

/// EVERY ROW INDEXES, both libraries at once. The counterpart to `faults()` being empty: a count
/// that matches the row slices proves nothing was skipped, where an empty fault list alone would
/// also be satisfied by a registry that never looked.
#[test]
fn both_libraries_index_without_a_fault() {
    if !have_bosl2() {
        return;
    }
    let bosl2 = fab_bosl2::Bosl2.rows();
    let hand = fab_lang::surface::Natives.rows();
    let reg = Registry::new().with(hand).with(bosl2);

    assert_eq!(reg.faults(), Vec::new(), "two libraries is not a fault");
    assert_eq!(
        reg.function_count(),
        hand.functions.len() + bosl2.functions.len(),
        "every row from both libraries is indexed"
    );
    assert_eq!(reg.pin_count(), hand.pins.len(), "pins are library-local");
    assert_eq!(
        reg.module_count(),
        hand.modules.len() + bosl2.modules.len(),
        "module names do not collide between the two — fab-lang ships only its POC set"
    );
}

/// AN ECLIPSED ROW IS SAFE BECAUSE ITS TWIN IS IDENTICAL — asserted against the real overlap rather
/// than a probe pair. Each of the 66 names both libraries declare must resolve to a row whose
/// reference fingerprints the SAME as the one it eclipsed; otherwise "the first match wins" would be
/// picking between two genuinely different functions, and the loser would be a missing native rather
/// than a redundant one.
///
/// It is also the strongest evidence available that fab-lang's hand-transcribed references are
/// faithful to the pinned library: 66 independent transcriptions, hashed by our own parser, all
/// agreeing with what the transpiler read out of the source.
#[test]
fn every_eclipsed_row_is_structurally_identical_to_the_one_that_won() {
    if !have_bosl2() {
        return;
    }
    let bosl2 = fab_bosl2::Bosl2.rows();
    let hand = fab_lang::surface::Natives.rows();
    let reg = Registry::new().with(hand).with(bosl2);

    let eclipsed = reg.eclipsed();
    assert!(
        !eclipsed.is_empty(),
        "the two libraries are supposed to overlap — an empty list means this gate stopped testing \
         anything"
    );
    for &(library, name) in &eclipsed {
        assert_eq!(
            library, "BOSL2",
            "fab-lang went in first, so BOSL2's copy is the one passed over"
        );
        // The live row for the name, and the eclipsed one, must hash alike. `reference_fp` answers
        // for the winner; finding the loser's reference means going back to the row slice.
        let winner = reg.reference_fp(name).expect("indexed");
        let loser = bosl2
            .functions
            .iter()
            .find(|e| e.name == name)
            .expect("the eclipsed row is BOSL2's");
        let probe = Registry::new().with(fab_lang::registry::Rows {
            name: "probe",
            functions: std::slice::from_ref(loser),
            ..fab_lang::registry::Rows::default()
        });
        assert_eq!(
            probe.reference_fp(name),
            Some(winner),
            "`{name}` was eclipsed by a row it is NOT structurally identical to — one of the two \
             libraries has drifted, and the eclipse is hiding it"
        );
    }
}
