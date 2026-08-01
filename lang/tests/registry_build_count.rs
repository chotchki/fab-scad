//! AR.26.1 — the anti-regression the design note cannot be: NO EVALUATION BUILDS A REGISTRY.
//!
//! A registry costs a full library parse the first time it is ASKED anything — every reference
//! parsed and fingerprinted, because rows carry SOURCE rather than a hash, which is what keeps the
//! gate hash ours rather than the row author's. Fine exactly once per registry. A refactor that
//! moved construction inside an eval loop would pay it per run and nothing behavioural would notice:
//! the answers stay identical, only the clock moves. So the invariant gets a number.
//!
//! The counter tracks row-set HAND-OVERS rather than finished registries — `Registry::with` itself
//! is nearly free, and the number is the one signal available before the lazy indexes decide whether
//! to do any work at all. Zero new hand-overs means no evaluation started building anything.
//!
//! ITS OWN FILE, deliberately. `build_count` is a process-global counter, and `cargo test` runs one
//! binary's tests as THREADS in one process — a sibling test that built a registry would land inside
//! the delta window and the failure would look like a real regression. Nothing else lives here.
//!
//! DELTA, not an absolute. Asserting `== 2` would be asserting how many row sets `Registry::builtin`
//! happens to accumulate, so AR.26.3 adding a third library would fail it for the right reason with
//! a misleading message. Zero-new-builds is the actual invariant and it survives.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use fab_lang::registry;

#[test]
fn no_ordinary_evaluation_builds_a_registry() {
    // Prime the builtin instance FIRST so its own construction is outside the window.
    let primed = registry::Registry::builtin();
    assert!(
        primed.function_count() > 0,
        "the shipped registry must be non-empty, or this test proves nothing"
    );
    let before = registry::build_count();

    // Three different public doors, three different programs — including one that instantiates a
    // user MODULE, since the module index is the lazy half and the easiest one to rebuild by
    // accident.
    fab_lang::evaluate("cube(1);").expect("mesh entry");
    fab_lang::evaluate_geometry("sphere(2);").expect("geometry entry");
    fab_lang::evaluate_geometry("module m(s) { cube(s); }\nm(3);\nm(4);")
        .expect("module-instantiating program");

    assert_eq!(
        registry::build_count(),
        before,
        "an evaluation built a registry — construction has leaked into the eval path"
    );
}
