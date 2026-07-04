//! Exact-quadrant degree trig — ported verbatim from OpenSCAD `src/utils/degree_trig.cc`.
//!
//! OpenSCAD does NOT call `libm` on radians for its geometry: it folds the angle into `[0,90]` and
//! hardcodes the exact values at 0/30/45/60/90 (so `sin_degrees(180) == 0.0` exactly, not `1.2e-16`).
//! Replicating this is required for byte-for-byte vertex parity with the oracle — a naive
//! `f64::sin(θ·π/180)` diverges in the low bits and fails the strict mesh-comparison tier.
//!
//! **[VERIFY at G.3.6/G.3.7]** the last-bit parity of the `libm` fallback path (`sin`/`cos` on the
//! non-special angles) depends on `M_DEG2RAD` being `PI/180` and the platform libm matching — the
//! metric experiment resolves which comparison tier this survives.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

/// `M_SQRT3_4` = √3/2 (the `f64` nearest to `sqrt(3.0)/2.0`, matching OpenSCAD's constant).
const SQRT3_4: f64 = 0.866_025_403_784_438_6;

/// `deg2rad(x) = x * M_DEG2RAD`, with `M_DEG2RAD = PI/180`.
fn deg2rad(x: f64) -> f64 {
    x * (PI / 180.0)
}

/// Reduce an angle to `[0, 360)` (non-finite → `NaN`, matching the effective flex behavior).
fn reduce_360(x: f64) -> f64 {
    if (0.0..360.0).contains(&x) {
        x
    } else if x.is_finite() {
        x - 360.0 * (x / 360.0).floor()
    } else {
        f64::NAN
    }
}

/// `sin` of an angle in DEGREES, OpenSCAD's exact-quadrant way (`degree_trig.cc`).
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "exact `==` on the special angles IS OpenSCAD's exact-quadrant algorithm"
)]
pub(crate) fn sin_degrees(x: f64) -> f64 {
    let mut x = reduce_360(x);
    let oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
    }
    let y = if x < 45.0 {
        if x == 30.0 { 0.5 } else { deg2rad(x).sin() }
    } else if x == 45.0 {
        FRAC_1_SQRT_2
    } else if x == 60.0 {
        SQRT3_4
    } else {
        deg2rad(90.0 - x).cos()
    };
    if oppose { -y } else { y }
}

/// `cos` of an angle in DEGREES, OpenSCAD's exact-quadrant way (`degree_trig.cc`).
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "exact `==` on the special angles IS OpenSCAD's exact-quadrant algorithm"
)]
pub(crate) fn cos_degrees(x: f64) -> f64 {
    let mut x = reduce_360(x);
    let mut oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
        oppose = !oppose;
    }
    let y = if x > 45.0 {
        if x == 60.0 {
            0.5
        } else {
            deg2rad(90.0 - x).sin()
        }
    } else if x == 45.0 {
        FRAC_1_SQRT_2
    } else if x == 30.0 {
        SQRT3_4
    } else {
        deg2rad(x).cos()
    };
    if oppose { -y } else { y }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact-value assertions on the special angles are the whole point of this module"
)]
mod tests {
    use super::{SQRT3_4, cos_degrees, sin_degrees};
    use std::f64::consts::FRAC_1_SQRT_2;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} vs {b}");
    }

    #[test]
    fn sin_branches() {
        assert_eq!(sin_degrees(0.0), 0.0); // x<45, else → sin(0)
        assert_eq!(sin_degrees(30.0), 0.5); // x==30
        assert_eq!(sin_degrees(45.0), FRAC_1_SQRT_2); // x==45
        assert_eq!(sin_degrees(60.0), SQRT3_4); // x==60
        assert_eq!(sin_degrees(90.0), 1.0); // else → cos(0)
        assert_eq!(sin_degrees(180.0), 0.0); // oppose, x=0 (−0.0 == 0.0)
        assert_eq!(sin_degrees(270.0), -1.0); // oppose → −cos(0)
        assert_eq!(sin_degrees(120.0), SQRT3_4); // x>90 fold → 60
        assert_eq!(sin_degrees(720.0), 0.0); // reduce → 0
        assert!(sin_degrees(f64::NAN).is_nan()); // non-finite → NaN
        approx(sin_degrees(22.5), 22.5_f64.to_radians().sin()); // x<45 else (libm)
        approx(sin_degrees(70.0), 70.0_f64.to_radians().sin()); // else → cos(20)
    }

    #[test]
    fn cos_branches() {
        assert_eq!(cos_degrees(0.0), 1.0); // x<45, else → cos(0)
        assert_eq!(cos_degrees(30.0), SQRT3_4); // x==30
        assert_eq!(cos_degrees(45.0), FRAC_1_SQRT_2); // x==45
        assert_eq!(cos_degrees(60.0), 0.5); // x>45, x==60
        assert_eq!(cos_degrees(90.0), 0.0); // x>45, else → sin(0)
        assert_eq!(cos_degrees(180.0), -1.0); // oppose → −cos(0)
        assert_eq!(cos_degrees(120.0), -0.5); // x>90 fold + flip → −0.5
        assert_eq!(cos_degrees(720.0), 1.0); // reduce → cos(0)
        assert!(cos_degrees(f64::NAN).is_nan());
        approx(cos_degrees(70.0), 70.0_f64.to_radians().cos()); // x>45 else (libm)
    }
}
