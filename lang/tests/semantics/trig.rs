//! # Semantics: exact-quadrant degree trig
//!
//! Provenance: OpenSCAD `src/utils/degree_trig.cc` `sin_degrees` / `cos_degrees`.
//! Oracle: verified bit-for-bit — the nightly's `sphere($fn=8)` OFF export carries `-0` at θ=180,
//! reproduced exactly (G.3.6 manual check; G.3.7 residual ~5e-7).
//!
//! OpenSCAD folds the angle into `[0,90]` and hardcodes the values at 0/30/45/60/90 instead of
//! calling `libm` on radians, so `sin(180°)` is EXACTLY `0.0` (with sign), not `1.2e-16`. The
//! internal `trig` module has direct unit tests; here we pin the USER-OBSERVABLE consequence — a
//! `$fn=4` sphere lands vertices exactly on the quadrant angles, so their coordinates are exact.

use fab_lang::evaluate;

/// FACT: `cos_degrees(90) == 0` exactly — the θ=90/270 vertices of a `$fn=4` sphere have x EXACTLY
/// 0, no `6e-17` dust that a naive `cos(θ·π/180)` would leave.
#[test]
fn cos_at_90_is_exact_zero() {
    let m = evaluate("sphere(1, $fn=4);").unwrap();
    // $fn=4 → θ ∈ {0,90,180,270}; θ=90 and θ=270 give x == 0 across both rings → ≥ 2 exact zeros.
    let x_exact_zeros = m.verts.iter().filter(|v| v[0] == 0.0).count();
    assert!(
        x_exact_zeros >= 2,
        "θ=90/270 must give x == 0 exactly; got {x_exact_zeros}"
    );
}

/// FACT: `sin_degrees(180) == -0.0` — a NEGATIVE zero. The exact-quadrant fold reproduces the SIGN,
/// which the oracle also carries in its export; the θ=180 vertex's y is negative zero, not +0.
#[test]
fn sin_at_180_is_negative_zero() {
    let m = evaluate("sphere(1, $fn=4);").unwrap();
    let v180 = m.verts.iter().find(|v| v[0] < 0.0).expect("θ=180 vertex");
    assert_eq!(v180[1], 0.0);
    assert!(
        v180[1].is_sign_negative(),
        "y at θ=180 must be -0.0, got {}",
        v180[1]
    );
}
