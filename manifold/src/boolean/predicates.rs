//! The symbolic-perturbation predicates — the robustness model, ported VERBATIM from `shared.h` /
//! `utils.h` / `boolean3.cpp`.
//!
//! Manifold has NO exact arithmetic (no Shewchuk, no rationals). Robustness comes from plain `f64` plus
//! an operation-dependent SYMBOLIC PERTURBATION: when two coordinates are exactly equal, a tracked
//! `dir` sign (a sum of face-normal components) breaks the tie consistently. A "more correct"
//! (exact-predicate) version makes DIFFERENT choices at those ties and breaks the C++ differential — so
//! every line here is a faithful transliteration, not an improvement.
//!
//! Two disciplines are load-bearing and enforced:
//! - **NO FMA.** Every `a*b + c` is a separate rounded multiply then add (Manifold builds
//!   `-ffp-contract=off`). `f64::mul_add` is clippy-banned crate-wide; Rust never auto-contracts.
//! - **exact `==` IS the algorithm.** [`shadows`] compares raw doubles with `==` on purpose (that's
//!   the perturbation trigger), so `clippy::float_cmp` is allowed for this module only.
//!
//! There's no independent oracle for these (they're not exported from the C++ crate, and they ARE the
//! algorithm — nothing to cross-check against the way `mathf` cross-checks `libm`). The differential is
//! the end-to-end boolean-residual at GATE-A; the unit tests here pin the load-bearing SEMANTICS (the
//! literal `4` and `<=` in `ccw`, the magnitude-choice in `interpolate`/`intersect`, `shadows`'
//! tie-break and NaN→false, the sign-flip in the axis projection).

#![allow(
    // The exact float `==` in `shadows` is the symbolic-perturbation trigger, not an accidental
    // equality test — the whole robustness model rests on it.
    clippy::float_cmp
)]

use crate::linalg::{Box3, Vec2, Vec3, Vec4};

/// The kernel's base precision (`utils.h` `kPrecision`) — the relative epsilon floor, scaled by the
/// bounding box in [`max_epsilon`].
pub const K_PRECISION: f64 = 1e-12;

/// C++ `std::max`/`la::max` for scalars: `a < b ? b : a`, bit-for-bit — INCLUDING the NaN tie-break
/// (returns `a` when `a` is NaN, `b` when only `b` is NaN), which Rust's `f64::max` inverts. Used
/// wherever a ported line called `std::max`, so no `std::max` site can diverge on a poisoned input.
#[inline]
pub fn fmax(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

/// The epsilon for a mesh of the given bounding box (`shared.h` `MaxEpsilon`): `max(min_epsilon,
/// kPrecision · bBox.Scale())`, clamped to `-1` if non-finite (an empty/degenerate box).
#[inline]
pub fn max_epsilon(min_epsilon: f64, b_box: Box3) -> f64 {
    let epsilon = fmax(min_epsilon, K_PRECISION * b_box.scale());
    if epsilon.is_finite() { epsilon } else { -1.0 }
}

/// `la::normalize` guarded against a zero/degenerate input (`shared.h` `SafeNormalize`): normalize,
/// and if the result's `x` went non-finite (length was 0), fall back to the zero vector. Only `.x` is
/// checked, verbatim.
#[inline]
pub fn safe_normalize(v: Vec3) -> Vec3 {
    let n = v.normalize();
    if n.x.is_finite() { n } else { Vec3::ZERO }
}

/// `withSign` (`shared.h`): `v` when `pos`, else `-v`. Under a union (`expandP = true`) the caller
/// passes `pos = true`, so the perturbation adds `+normal`.
#[inline]
pub fn with_sign(pos: bool, v: f64) -> f64 {
    if pos { v } else { -v }
}

/// `p < q` with symbolic perturbation (`shared.h` `Shadows`): when `p == q` EXACTLY, the sign of `dir`
/// decides (`dir < 0` ⇒ shadows). NaN in either operand yields `false` (both `==` and `<` are false) —
/// the deliberate "no shadow" answer. This is the single most-consulted tie-break in the boolean.
#[inline]
pub fn shadows(p: f64, q: f64, dir: f64) -> bool {
    if p == q { dir < 0.0 } else { p < q }
}

/// Interpolate the `(y, z)` of segment `aL→aR` at abscissa `x` (`shared.h` `Interpolate`). The
/// `(x − aL)` vs `(x − aR)` choice takes the smaller magnitude to keep FP error low near either
/// endpoint; a non-finite `lambda`/slope degenerates to `aL`'s `(y, z)`.
#[inline]
pub fn interpolate(a_l: Vec3, a_r: Vec3, x: f64) -> Vec2 {
    let dx_l = x - a_l.x;
    let dx_r = x - a_r.x;
    debug_assert!(dx_l * dx_r <= 0.0, "Boolean manifold error: not in domain");
    let use_l = dx_l.abs() < dx_r.abs();
    let d_lr = a_r - a_l;
    let lambda = (if use_l { dx_l } else { dx_r }) / d_lr.x;
    if !lambda.is_finite() || !d_lr.y.is_finite() || !d_lr.z.is_finite() {
        return Vec2::new(a_l.y, a_l.z);
    }
    let y = lambda * d_lr.y + (if use_l { a_l.y } else { a_r.y });
    let z = lambda * d_lr.z + (if use_l { a_l.z } else { a_r.z });
    Vec2::new(y, z)
}

/// Intersect edge `aL→aR` with edge `bL→bR` in the shadow projection (`boolean3.cpp` `Intersect`),
/// returning `(x, y, z_a, z_b)` — the crossing's abscissa, ordinate, and the two z-heights. Every
/// magnitude-choice (`useL`, `useA`) matches the source; a non-finite `lambda` degenerates to `0`.
#[inline]
pub fn intersect(a_l: Vec3, a_r: Vec3, b_l: Vec3, b_r: Vec3) -> Vec4 {
    let dy_l = b_l.y - a_l.y;
    let dy_r = b_r.y - a_r.y;
    debug_assert!(
        dy_l * dy_r <= 0.0,
        "Boolean manifold error: no intersection"
    );
    let use_l = dy_l.abs() < dy_r.abs();
    let dx = a_r.x - a_l.x;
    let mut lambda = (if use_l { dy_l } else { dy_r }) / (dy_l - dy_r);
    if !lambda.is_finite() {
        lambda = 0.0;
    }
    let x = lambda * dx + (if use_l { a_l.x } else { a_r.x });
    let a_dy = a_r.y - a_l.y;
    let b_dy = b_r.y - b_l.y;
    let use_a = a_dy.abs() < b_dy.abs();
    let y = lambda * (if use_a { a_dy } else { b_dy })
        + if use_l {
            if use_a { a_l.y } else { b_l.y }
        } else if use_a {
            a_r.y
        } else {
            b_r.y
        };
    let z = lambda * (a_r.z - a_l.z) + (if use_l { a_l.z } else { a_r.z });
    let w = lambda * (b_r.z - b_l.z) + (if use_l { b_l.z } else { b_r.z });
    Vec4::new(x, y, z, w)
}

/// `a.x·b.y − a.y·b.x` (`polygon.cpp` `determinant2x2`) — the 2×2 determinant / 2D cross magnitude,
/// spelled out (MSVC wouldn't fold `la::determinant(mat2(a,b))`), so no FMA sneaks in.
#[inline]
pub fn determinant2x2(a: Vec2, b: Vec2) -> f64 {
    a.x * b.y - a.y * b.x
}

/// Winding of `p0→p1→p2` within `tol` (`utils.h` `CCW`): `+1` CCW, `-1` CW, `0` collinear. The
/// collinearity test `area²·4 ≤ base²·tol²` is load-bearing to the last token — the literal `4`, the
/// `≤`, and the SQUARED form (no `sqrt`, no division) all keep it exact and FMA-free.
#[inline]
pub fn ccw(p0: Vec2, p1: Vec2, p2: Vec2, tol: f64) -> i32 {
    let v1 = p1 - p0;
    let v2 = p2 - p0;
    let area = v1.x * v2.y - v1.y * v2.x;
    let base2 = fmax(v1.dot(v1), v2.dot(v2));
    if area * area * 4.0 <= base2 * tol * tol {
        0
    } else if area > 0.0 {
        1
    } else {
        -1
    }
}

/// The closest axis-aligned projection to a face normal (`shared.h` `GetAxisAlignedProjection`),
/// returned as its two output rows so `apply` reproduces `mat2x3 · vec3` exactly. Projecting along an
/// axis instead of the true normal avoids introducing any rounding error into the shadow coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Projection {
    /// First output row (`transpose`'s row 0 = the source `mat3x2`'s column 0, sign-flipped when the
    /// dominant normal component is negative).
    pub r0: Vec3,
    /// Second output row.
    pub r1: Vec3,
}

impl Projection {
    /// Project a 3D point to the 2D shadow plane: `(r0·p, r1·p)` = `mat2x3 · p`.
    #[inline]
    pub fn apply(self, p: Vec3) -> Vec2 {
        Vec2::new(self.r0.dot(p), self.r1.dot(p))
    }
}

/// Build the [`Projection`] for a face normal — drop the axis the normal is most aligned with, keeping
/// the other two (and flip the first kept axis when the dominant normal component is negative, to
/// preserve orientation). The strict `>` comparisons and branch order are verbatim.
#[inline]
pub fn get_axis_aligned_projection(normal: Vec3) -> Projection {
    let abs_normal = normal.cabs();
    // `(r0, r1)` are the rows of the returned `mat2x3` = the columns of the source `mat3x2`.
    let (mut r0, r1, xyz_max) = if abs_normal.z > abs_normal.x && abs_normal.z > abs_normal.y {
        (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), normal.z)
    } else if abs_normal.y > abs_normal.x {
        (Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.0), normal.y)
    } else {
        (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), normal.x)
    };
    if xyz_max < 0.0 {
        r0 = -r0;
    }
    Projection { r0, r1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadows_tie_break_and_nan() {
        // Strict order when unequal — dir is ignored.
        assert!(shadows(1.0, 2.0, 5.0));
        assert!(!shadows(2.0, 1.0, -5.0));
        // Exact tie: dir < 0 shadows, dir >= 0 does not (the ==0 boundary is NOT a shadow).
        assert!(shadows(3.0, 3.0, -0.001));
        assert!(!shadows(3.0, 3.0, 0.0));
        assert!(!shadows(3.0, 3.0, 0.001));
        // NaN in either operand ⇒ false (the deliberate "no shadow").
        assert!(!shadows(f64::NAN, 1.0, -1.0));
        assert!(!shadows(1.0, f64::NAN, -1.0));
    }

    #[test]
    fn with_sign_flips() {
        assert_eq!(with_sign(true, 4.0), 4.0);
        assert_eq!(with_sign(false, 4.0), -4.0);
        assert_eq!(with_sign(false, -4.0), 4.0);
    }

    #[test]
    fn interpolate_endpoints_and_midpoint() {
        let a_l = Vec3::new(0.0, 10.0, 100.0);
        let a_r = Vec3::new(2.0, 20.0, 200.0);
        // At each endpoint the interpolation returns that endpoint's (y, z) exactly.
        assert_eq!(interpolate(a_l, a_r, 0.0), Vec2::new(10.0, 100.0));
        assert_eq!(interpolate(a_l, a_r, 2.0), Vec2::new(20.0, 200.0));
        // Midpoint.
        assert_eq!(interpolate(a_l, a_r, 1.0), Vec2::new(15.0, 150.0));
        // Degenerate segment (aL.x == aR.x, x on the line) ⇒ non-finite lambda ⇒ falls back to aL.yz.
        let v = Vec3::new(5.0, 1.0, 2.0);
        let w = Vec3::new(5.0, 9.0, 9.0);
        assert_eq!(interpolate(v, w, 5.0), Vec2::new(1.0, 2.0));
    }

    #[test]
    fn intersect_crossing_x_axis() {
        // aL→aR along y = x - 1 (from (0,-1) up to (2,1)); bL→bR along y = 1 - x (from (0,1) down to
        // (2,-1)). They cross at (1, 0). z-heights are carried linearly (both flat at 0 here).
        let a_l = Vec3::new(0.0, -1.0, 0.0);
        let a_r = Vec3::new(2.0, 1.0, 0.0);
        let b_l = Vec3::new(0.0, 1.0, 0.0);
        let b_r = Vec3::new(2.0, -1.0, 0.0);
        let xyzz = intersect(a_l, a_r, b_l, b_r);
        assert_eq!(xyzz.x, 1.0);
        assert_eq!(xyzz.y, 0.0);
        assert_eq!(xyzz.z, 0.0);
        assert_eq!(xyzz.w, 0.0);
    }

    #[test]
    fn ccw_sign_and_collinear() {
        let o = Vec2::new(0.0, 0.0);
        let e = Vec2::new(1.0, 0.0);
        assert_eq!(ccw(o, e, Vec2::new(0.0, 1.0), 0.0), 1); // left turn
        assert_eq!(ccw(o, e, Vec2::new(0.0, -1.0), 0.0), -1); // right turn
        assert_eq!(ccw(o, e, Vec2::new(2.0, 0.0), 0.0), 0); // exactly collinear
        // A near-collinear point is snapped to 0 by tolerance: area = 1e-4, base = 1, so
        // area²·4 = 4e-8 ≤ tol² = 1e-6 at tol = 1e-3.
        assert_eq!(ccw(o, e, Vec2::new(0.5, 1e-4), 1e-3), 0);
        // ...but with a tighter tolerance the same point is a genuine left turn.
        assert_eq!(ccw(o, e, Vec2::new(0.5, 1e-4), 1e-9), 1);
    }

    #[test]
    fn determinant2x2_is_2d_cross() {
        assert_eq!(
            determinant2x2(Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)),
            1.0
        );
        assert_eq!(
            determinant2x2(Vec2::new(0.0, 1.0), Vec2::new(1.0, 0.0)),
            -1.0
        );
        assert_eq!(
            determinant2x2(Vec2::new(3.0, 2.0), Vec2::new(1.0, 4.0)),
            10.0
        );
    }

    #[test]
    fn axis_projection_drops_dominant_axis() {
        // +Z-dominant normal: keep (x, y) as-is.
        let pz = get_axis_aligned_projection(Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(pz.apply(Vec3::new(3.0, 4.0, 9.0)), Vec2::new(3.0, 4.0));
        // −Z-dominant: xyzMax < 0 flips the first kept axis ⇒ (−x, y).
        let nz = get_axis_aligned_projection(Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(nz.apply(Vec3::new(3.0, 4.0, 9.0)), Vec2::new(-3.0, 4.0));
        // +Y-dominant: keep (z, x).
        let py = get_axis_aligned_projection(Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(py.apply(Vec3::new(3.0, 9.0, 5.0)), Vec2::new(5.0, 3.0));
        // +X-dominant (the else branch): keep (y, z).
        let px = get_axis_aligned_projection(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(px.apply(Vec3::new(9.0, 3.0, 5.0)), Vec2::new(3.0, 5.0));
    }

    #[test]
    fn safe_normalize_zero_falls_back() {
        assert_eq!(
            safe_normalize(Vec3::new(3.0, 4.0, 0.0)),
            Vec3::new(0.6, 0.8, 0.0)
        );
        assert_eq!(safe_normalize(Vec3::ZERO), Vec3::ZERO);
    }

    #[test]
    fn max_epsilon_scales_and_clamps() {
        // A unit-ish box: epsilon = kPrecision · Scale.
        let b = Box3::from_points(Vec3::ZERO, Vec3::splat(2.0));
        assert_eq!(max_epsilon(-1.0, b), K_PRECISION * 2.0);
        // A larger min_epsilon wins.
        assert_eq!(max_epsilon(0.5, b), 0.5);
        // Empty (inverted-infinity) box ⇒ Scale = +inf ⇒ non-finite ⇒ clamped to -1.
        assert_eq!(max_epsilon(-1.0, Box3::default()), -1.0);
    }

    #[test]
    fn fmax_matches_cpp_nan_order() {
        assert_eq!(fmax(1.0, 2.0), 2.0);
        assert_eq!(fmax(2.0, 1.0), 2.0);
        // C++ std::max(a, b) = a < b ? b : a: a NaN first operand is returned (a<b is false).
        assert!(fmax(f64::NAN, 1.0).is_nan());
        // A NaN second operand is NOT returned (a < NaN is false ⇒ returns a).
        assert_eq!(fmax(1.0, f64::NAN), 1.0);
    }
}
