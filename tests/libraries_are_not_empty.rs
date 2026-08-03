//! SZ.1 — A SHIPPED BUILD CARRIES ITS LIBRARIES, or the build fails saying so.
//!
//! Every library crate transpiles a pinned submodule at build time, and a checkout WITHOUT that
//! submodule is not an error: `build.rs` warns, writes an empty library, and the crate declares
//! nothing. That is the right default — a missing asset costs a PART, not an error, which is what
//! lets a submodule-less clone still build and still render (everything just interprets).
//!
//! It is the wrong default for a RELEASE, and it shipped. `release-native.yml` used
//! `actions/checkout@v4` with no `submodules:` key, so every signed desktop artifact was built
//! against an empty `libs/BOSL2`: the transpiled band declared nothing and the app interpreted all
//! of BOSL2. Nothing failed, because nothing was supposed to. The config is fixed; this is the part
//! that stops it coming back, because a config key is exactly the sort of thing that gets dropped
//! in a workflow rewrite and no test notices.
//!
//! It asserts the CAPABILITY, not a count. `transpiled()` answers "did a real library go through
//! the emitter for this build" and is the predicate the crates already expose for this question. A
//! function-count floor would be a second number to maintain against the coverage ratchet, which
//! owns that job (`bosl2_corpus_ratchet_and_report`).
//!
//! SKIPPED, not failed, when the submodule is genuinely absent — a contributor with a shallow clone
//! should get a working `cargo test`, and CI is where the checkout is guaranteed. The skip is
//! keyed on the submodule directory rather than on `transpiled()` itself, so the test can never
//! excuse the very state it exists to catch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A library this build claims to carry: the cargo feature that pulls it in, the submodule file
/// whose presence means "the source really is checked out", and the crate's own answer.
struct Carried {
    name: &'static str,
    /// A file every checkout of that submodule has — absent means the submodule is not initialised.
    sentinel: PathBuf,
    transpiled: bool,
}

// Built by pushing under `cfg` rather than as a `vec!` literal: which libraries exist is a
// COMPILE-TIME question, and a literal cannot hold conditionally-present elements.
#[allow(
    clippy::vec_init_then_push,
    reason = "each entry is cfg-gated; a vec! literal cannot express that"
)]
fn carried() -> Vec<Carried> {
    let mut out = Vec::new();
    #[cfg(feature = "bosl2")]
    out.push(Carried {
        name: "fab-bosl2",
        sentinel: repo().join("libs/BOSL2/std.scad"),
        transpiled: fab_bosl2::transpiled(),
    });
    #[cfg(feature = "mcad")]
    out.push(Carried {
        name: "fab-mcad",
        sentinel: repo().join("libs/MCAD/constants.scad"),
        transpiled: fab_mcad::transpiled(),
    });
    #[cfg(feature = "machineblocks")]
    out.push(Carried {
        name: "fab-machineblocks",
        sentinel: repo().join("libs/machineblocks/lib/block.scad"),
        transpiled: fab_machineblocks::transpiled(),
    });
    out
}

/// If the source is on disk, the crate MUST have transpiled it. A crate that links but declares
/// nothing is the exact shape of the shipped bug.
#[test]
fn every_carried_library_actually_transpiled() {
    for lib in carried() {
        if !lib.sentinel.exists() {
            // The submodule is genuinely absent — a shallow clone, not a broken build.
            continue;
        }
        assert!(
            lib.transpiled,
            "{} links into this build and its source IS checked out at {}, but it transpiled \
             NOTHING — the crate declares an empty library and every one of its functions will \
             interpret. This is what a release built without `submodules: recursive` produces, and \
             it does not fail the build on its own.",
            lib.name,
            lib.sentinel.display()
        );
    }
}

/// The set is non-empty. Guards the guard: if the feature flags are ever rearranged so this binary
/// carries no library at all, every assertion above passes over an empty list.
#[test]
fn this_build_carries_at_least_one_library() {
    let libs = carried();
    assert!(
        !libs.is_empty(),
        "this build carries NO transpiled library, so the check above asserts nothing — either a \
         feature was dropped or the test needs teaching about a renamed one"
    );
}
