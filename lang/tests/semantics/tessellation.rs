//! # Semantics: primitive tessellation (sphere / cube / cylinder)
//!
//! Provenance: OpenSCAD `src/core/primitives.cc` — `SphereNode` / `CubeNode` / `CylinderNode`
//! `createGeometry`.
//! Oracle: G.3.7 differential sweep — vertex AND triangle counts matched the oracle EXACTLY across
//! `$fn` 8→256, boolean residual ~5e-7 (the SAME solid).

use fab_lang::evaluate;

/// FACT: `sphere` builds `ceil($fn/2)` rings of `$fn` vertices each — `$fn=8 → 4×8 = 32`; an odd
/// `$fn` still rounds the ring count up (`$fn=7 → 4×7`).
#[test]
fn sphere_ring_structure() {
    assert_eq!(evaluate("sphere(1, $fn=8);").unwrap().vert_count(), 32);
    assert_eq!(evaluate("sphere(1, $fn=7);").unwrap().vert_count(), 4 * 7);
}

/// FACT: `cube(size)` is the 8 corners — non-centered spans `[0,0,0]..=size`; `center=true` is `±size/2`.
#[test]
fn cube_is_eight_corners() {
    let m = evaluate("cube([2,3,4]);").unwrap();
    assert_eq!(m.vert_count(), 8);
    assert_eq!(m.verts[0], [0.0, 0.0, 0.0]);
    assert_eq!(m.verts[6], [2.0, 3.0, 4.0]);
    assert_eq!(
        evaluate("cube(2, center=true);").unwrap().verts[0],
        [-1.0, -1.0, -1.0]
    );
}

/// FACT: a zero radius collapses that ring to a single apex vertex — `cylinder(r2=0)` is a cone
/// (`$fn` ring + 1 apex = `$fn+1`), vs the full two-ring `2·$fn`.
#[test]
fn cylinder_zero_radius_is_an_apex() {
    assert_eq!(
        evaluate("cylinder(h=10, r=5, $fn=8);")
            .unwrap()
            .vert_count(),
        16
    ); // two rings
    assert_eq!(
        evaluate("cylinder(h=10, r1=5, r2=0, $fn=8);")
            .unwrap()
            .vert_count(),
        9 // ring (8) + apex (1)
    );
}

/// FACT: diameter beats radius (`d = 2r`), and positional args bind to the primitive's parameter
/// order — `sphere(d=10)` and `sphere(5)` are the same 5-radius sphere.
#[test]
fn diameter_and_positional_binding() {
    let by_d = evaluate("sphere(d=10, $fn=8);").unwrap();
    let by_pos = evaluate("sphere(5, $fn=8);").unwrap();
    assert_eq!(by_d.verts, by_pos.verts);
}
