//! SU.2 (sustainment): the intrinsic parity gate against the COMMITTED BOSL2 pin.
//!
//! Every registered intrinsic (and dep PIN) must fingerprint-match the `libs/BOSL2` submodule this repo
//! actually ships — 100% Matched or the build fails. This is the guard that makes drift LOUD at the
//! moment it enters the tree (a submodule bump, a stray edit under libs/, a stale registry reference),
//! instead of surfacing months later as a silent perf regression when the intrinsics quietly stop
//! dispatching. The nightly sustainment watcher runs the same audit against CANDIDATE upstream versions;
//! this test pins the baseline it diffs from. Skips cleanly when the submodule isn't checked out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::path::Path;

use fab_lang::IntrinsicMatrixStatus;

#[test]
fn committed_bosl2_pin_matches_every_intrinsic() {
    let bosl2 = Path::new(env!("CARGO_MANIFEST_DIR")).join("libs/BOSL2");
    if !bosl2.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 not checked out (git submodule update --init libs/BOSL2)");
        return;
    }
    let rows = fab_lang::intrinsic_matrix("include <std.scad>\n", &bosl2, &[]).expect("audit runs");
    assert!(!rows.is_empty(), "registry can't be empty");
    let off: Vec<_> = rows
        .iter()
        .filter(|r| r.status != IntrinsicMatrixStatus::Matched)
        .collect();
    assert!(
        off.is_empty(),
        "{} intrinsic(s) drifted from the committed BOSL2 pin — either libs/BOSL2 moved (update the \
         registry references) or a reference is stale:\n{off:#?}",
        off.len()
    );
}
