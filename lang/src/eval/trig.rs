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

/// `M_SQRT3` = √3, upstream's literal (`degree_trig.h`: `1.73205080756887719318`).
const SQRT3: f64 = 1.732_050_807_568_877_2;

/// `M_SQRT1_3` = √(1/3) = √3/3, upstream's literal (`degree_trig.h`: `0.57735026918962573106`).
const SQRT1_3: f64 = 0.577_350_269_189_625_7;

/// `deg2rad(x) = x * M_DEG2RAD`, with `M_DEG2RAD = PI/180`.
fn deg2rad(x: f64) -> f64 {
    x * (PI / 180.0)
}

/// `acos(x)` in DEGREES with upstream's GENERAL whole-degree snap (`degree_trig.cc`): compute
/// `rad2deg(acos(x))`, round to the nearest whole degree, and return the WHOLE degree iff
/// `cos_degrees(whole) == x` — i.e. any input that is EXACTLY the cosine of an integer angle
/// round-trips to that integer (the trig-tests inverse sweep pins every integer angle). This
/// SUBSUMES the old L.2.8i nice-angle table (`cos_degrees(120) == -0.5` exactly via the quadrant
/// table, so `acos(-0.5)` is exactly `120`, not `120.00000000000001`); a non-exact input
/// (`0.707107`, a rounded literal) fails the round-trip check and keeps the libm value, so
/// near-but-not-exact geometry (the `glued_circles` arc) stays untouched. Deterministic.
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "the exact `==` round-trip check IS upstream's snap"
)]
pub(crate) fn acos_degrees(x: f64) -> f64 {
    let degs = x.acos().to_degrees();
    let whole = degs.round();
    if cos_degrees(whole) == x { whole } else { degs }
}

/// `asin(x)` in DEGREES — the [`acos_degrees`] companion, same whole-degree round-trip snap.
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "the exact `==` round-trip check IS upstream's snap"
)]
pub(crate) fn asin_degrees(x: f64) -> f64 {
    let degs = x.asin().to_degrees();
    let whole = degs.round();
    if sin_degrees(whole) == x { whole } else { degs }
}

/// `atan(x)` in DEGREES — same whole-degree round-trip snap against [`tan_degrees`].
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "the exact `==` round-trip check IS upstream's snap"
)]
pub(crate) fn atan_degrees(x: f64) -> f64 {
    let degs = x.atan().to_degrees();
    let whole = degs.round();
    if tan_degrees(whole) == x { whole } else { degs }
}

/// `atan2(y, x)` in DEGREES — upstream snaps to a whole degree within `3e-14` (a tolerance, not a
/// round-trip: `tan` can't distinguish the quadrants `atan2` resolves).
pub(crate) fn atan2_degrees(y: f64, x: f64) -> f64 {
    let degs = y.atan2(x).to_degrees();
    let whole = degs.round();
    if (degs - whole).abs() < 3.0e-14 {
        whole
    } else {
        degs
    }
}

/// Reduce an angle to `[0, 360)` (non-finite → `NaN`, matching the effective flex behavior).
/// `TRIG_HUGE_VAL` (`degree_trig.cc`): past `2²⁶ · 360 · 2²⁶` the 52-bit mantissa can't resolve a
/// revolution — reduction is meaningless, upstream returns NaN.
const TRIG_HUGE_VAL: f64 = 67_108_864.0 * 360.0 * 67_108_864.0; // (1<<26)·360·(1<<26), exact in f64

fn reduce_360(x: f64) -> f64 {
    if (0.0..360.0).contains(&x) {
        x
    } else if x.is_finite() && x.abs() < TRIG_HUGE_VAL {
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

/// `tan` of an angle in DEGREES — upstream's DEDICATED case split (`degree_trig.cc`
/// `tan_degrees`), NOT the sin/cos quotient: the signed zero and signed infinity hang off the raw
/// half-turn count (`tan(-180)` is `-0`, `tan(-90)` is `-inf` — AH.2.9, the trig-tests golden),
/// which a quotient of folded sin/cos can't recover. The `TRIG_HUGE_VAL` guard applies here too
/// (defined in `degree_trig.cc` — the .h grep that called it dead was wrong). Parity via f64 `%`
/// (fmod): past 2⁵³ half-turns parity is unknowable either way — upstream's `(int)floor` is UB
/// there.
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "exact `==` on the special angles IS OpenSCAD's exact-quadrant algorithm"
)]
pub(crate) fn tan_degrees(x: f64) -> f64 {
    if x.is_finite() && x.abs() >= TRIG_HUGE_VAL {
        return f64::NAN; // total loss of accuracy — upstream's guard
    }
    let cycles = (x / 180.0).floor();
    let mut x = if (0.0..180.0).contains(&x) {
        x
    } else {
        x - 180.0 * cycles
    };
    let oppose = x > 90.0;
    if oppose {
        x = 180.0 - x;
    }
    let even = cycles % 2.0 == 0.0;
    let y = if x == 0.0 {
        if even { 0.0 } else { -0.0 }
    } else if x == 30.0 {
        SQRT1_3
    } else if x == 45.0 {
        1.0
    } else if x == 60.0 {
        SQRT3
    } else if x == 90.0 {
        if even {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }
    } else {
        deg2rad(x).tan()
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
    use super::{SQRT3_4, acos_degrees, asin_degrees, cos_degrees, sin_degrees};
    use std::f64::consts::FRAC_1_SQRT_2;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} vs {b}");
    }

    #[test]
    fn acos_asin_snap_exact_at_nice_angles() {
        // the whole nice-cosine table lands EXACTLY (libm gives 120.0000…01 for acos(-0.5)) — BOSL2's
        // exact-`==` f_acos needs this. A regression to `.to_degrees()` fails these.
        assert_eq!(acos_degrees(1.0), 0.0);
        assert_eq!(acos_degrees(SQRT3_4), 30.0);
        assert_eq!(acos_degrees(FRAC_1_SQRT_2), 45.0);
        assert_eq!(acos_degrees(0.5), 60.0);
        assert_eq!(acos_degrees(0.0), 90.0);
        assert_eq!(acos_degrees(-0.5), 120.0);
        assert_eq!(acos_degrees(-1.0), 180.0);
        assert_eq!(asin_degrees(0.5), 30.0);
        assert_eq!(asin_degrees(1.0), 90.0);
        assert_eq!(asin_degrees(-0.5), -30.0);
        // a NON-nice input stays on libm, bit-identical — so geometry sampling arbitrary angles (the
        // glued_circles arc, which feeds acos a rounded near-√2/2 literal) is not perturbed by the snap.
        assert_eq!(acos_degrees(-0.6), (-0.6_f64).acos().to_degrees());
        assert_eq!(asin_degrees(0.3), (0.3_f64).asin().to_degrees());
        assert!(acos_degrees(2.0).is_nan()); // out of domain → NaN, like libm
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
