//! M.3 — invariants of the explicit-stack GEOMETRY driver (`docs/m3-explicit-eval-spec.md` §DECISION). The
//! whole test suite ALSO runs through the driver by default (it's the default eval path — `FAB_GEO_DRIVER=0`
//! forces the recursive path), so every existing geometry test doubles as a transparency check; these pin the
//! driver-SPECIFIC properties the 4-lens design review flagged, arm by arm as the dispatch converts.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test: unwrap/panic ARE the assertions"
)]

use fab_lang::{Geo, GeoNode, evaluate_geometry};

fn d3(src: &str) -> GeoNode {
    match evaluate_geometry(src).unwrap() {
        Geo::D3(n) => n,
        Geo::D2(s) => panic!("expected a 3D result for {src:?}, got 2D: {s:?}"),
    }
}

/// A2 + INVARIANT 1 — mark-drain arity: a `Collect` drains EXACTLY what its block pushed (0-or-1 per child
/// statement), never a static statement count. A block preceded by a sibling proves the drain doesn't reach
/// BELOW its mark and steal the sibling's `Geo` — the count-based bug the review caught (a block of 4 statements
/// pushes only 1 node, so a "pop 4" would steal 3 from the parent frame → silent CSG corruption).
#[test]
fn block_mark_drain_takes_only_its_own_children() {
    // 4 statements, exactly ONE of which produces geometry: `x = 1` hoists, `*cube(1)` is disabled, `echo` is a
    // side effect, `sphere` is the lone real child.
    match d3("cube(5); { x = 1; *cube(1); echo(\"h\"); sphere(2, $fn = 8); }") {
        GeoNode::Union(ref kids) => {
            assert_eq!(kids.len(), 2, "the cube sibling + the block's single sphere")
        }
        other => panic!("expected a top-level Union of 2, got {other:?}"),
    }
    // ...and the block itself collapses to exactly that one real child — bit-identical to the child bare.
    assert_eq!(
        d3("{ x = 1; *cube(1); echo(\"h\"); sphere(2, $fn = 8); }"),
        d3("sphere(2, $fn = 8);"),
    );
}
