//! The CSG geometry tree — fab-lang's geometry OUTPUT (J.2).
//!
//! fab-lang can't do booleans (that needs the Manifold kernel, and depending on it would be a cycle),
//! so the evaluator produces a TREE the downstream backend walks: leaves are tessellated meshes
//! (primitives), internal nodes are transforms + booleans. A single primitive is a bare [`GeoNode::Leaf`]
//! that [`crate::evaluate`] can still flatten to a [`Mesh`] with no backend; anything with a transform
//! or a boolean needs [`crate::evaluate_geometry`] + a backend (fab-scad's `GeometryBackend`, J.1).
//!
//! Transforms are 3×4 row-major affines (`multmatrix` form, `[m0..m11]`), applied as
//! `[m0·x+m1·y+m2·z+m3, m4·x+…+m7, m8·x+…+m11]`. Nested transforms compose as nested nodes (the backend
//! applies them outermost-last); the rotation math uses the exact-quadrant [`trig`](super::trig) so a
//! rotate is deterministic + bit-stable, same as the tessellation.

#![allow(
    clippy::many_single_char_names,
    reason = "rotation/reflection matrix math reads best in the standard xyz / c-s-t notation"
)]

use std::collections::BTreeMap;

use super::fragments::fragments;
use super::geo2d::{ExtrudeKind, Join2D, Shape2D};
use super::scope::Scope;
use super::trig::{cos_degrees, sin_degrees};
use super::value::Value;
use crate::Mesh;
use crate::geom::{Affine, Rgba};

/// A node in the CSG geometry tree.
#[derive(Debug, Clone, PartialEq)]
pub enum GeoNode {
    /// No geometry (an empty block, a degenerate primitive, a `for` that never ran).
    Empty,
    /// A tessellated primitive.
    Leaf(Mesh),
    /// An affine transform of a subtree (`translate`/`rotate`/`scale`/`mirror`/`multmatrix`).
    Transform {
        /// The affine.
        matrix: Affine,
        /// The transformed subtree.
        child: Box<GeoNode>,
    },
    /// Union of children (also the implicit union of multiple top-level objects + a block's children).
    Union(Vec<GeoNode>),
    /// `difference()` — the first child minus the rest.
    Difference(Vec<GeoNode>),
    /// `intersection()` — the common volume of all children.
    Intersection(Vec<GeoNode>),
    /// `hull()` — the convex hull of all children combined (N-ary, not a pairwise fold). Needs the
    /// backend (Manifold `batch_hull`); has no fab-lang mesh flattening (J.4.1).
    Hull(Vec<GeoNode>),
    /// `linear_extrude` / `rotate_extrude` — sweep a 2D [`Shape2D`] into 3D (the 2D→3D dimension
    /// bridge). Needs the backend (Manifold `extrude` / `revolve`); no fab-lang flattening (J.3.4/J.3.5).
    Extrude {
        /// Linear or rotational sweep, with its parameters.
        kind: ExtrudeKind,
        /// The 2D profile being swept.
        child: Box<Shape2D>,
    },
    /// `color()` over a subtree — sets its display color (BOSL2-critical). Geometry is UNCHANGED; the
    /// backend applies it as a Manifold vertex property (J.2.9). Outermost `color()` wins (OpenSCAD).
    Color {
        /// The RGBA color.
        color: Rgba,
        /// The colored subtree.
        child: Box<GeoNode>,
    },
}

/// Whether `name` is a built-in affine transform (dispatched to [`GeoNode::Transform`]).
pub(super) fn is_transform(name: &str) -> bool {
    matches!(
        name,
        "translate" | "rotate" | "scale" | "mirror" | "multmatrix"
    )
}

/// Whether `name` is a built-in CSG boolean (dispatched to the union/difference/intersection nodes).
pub(super) fn is_boolean(name: &str) -> bool {
    matches!(name, "union" | "difference" | "intersection")
}

/// Resolve a `color()` module's evaluated args to an [`Rgba`], or `None` when the color is INVALID
/// (unknown name, non-string/non-vector `c`) — OpenSCAD leaves such a node's color at the `Color4f(-1,…)`
/// sentinel meaning "inherit", so the caller wraps NO color node. `c` (1st positional / `c=`) is a
/// name/hex STRING or an `[r, g, b(, a)]` vector; `alpha` (2nd positional / `alpha=`, when a number)
/// OVERRIDES the alpha, applied LAST — unclamped, exactly as OpenSCAD stores it.
pub(super) fn resolve_color(positional: &[Value], named: &BTreeMap<String, Value>) -> Option<Rgba> {
    let c = positional.first().or_else(|| named.get("c"))?;
    let mut rgba = match c {
        Value::Str(s) => Rgba::from_name(s).or_else(|| Rgba::from_hex(s))?,
        Value::NumList(xs) => {
            let ch = |i: usize, d: f64| xs.get(i).copied().unwrap_or(d);
            Rgba::new(ch(0, 0.0), ch(1, 0.0), ch(2, 0.0), ch(3, 1.0)) // short vector back-fills a = 1
        }
        _ => return None,
    };
    if let Some(Value::Num(a)) = positional.get(1).or_else(|| named.get("alpha")) {
        rgba.a = *a;
    }
    Some(rgba)
}

/// Resolve an `offset()` module's evaluated args to its lowering params `(delta, join, segments)`.
/// `r` (1st positional / `r=`) selects a ROUNDED offset, `$fn`-faceted by the SAME [`fragments`] calc as
/// `circle` (`segments` = a full-circle count; ignored by miter/bevel). A `delta=` (only when there's no
/// `r`) selects a MITERED offset, or a BEVELED one with `chamfer = true`. `r` BEATS `delta` (OpenSCAD —
/// verified vs 2026.06.12: `offset(r=2, delta=9)` renders as `r=2`). No usable arg → a zero (identity)
/// offset. Winding of the result is Clipper2's; `segments` for miter/bevel is unused so it's `0`.
pub(super) fn resolve_offset(
    positional: &[Value],
    named: &BTreeMap<String, Value>,
    scope: &Scope,
) -> (f64, Join2D, u32) {
    // `r` — positional 0, else named `r`. When present it wins: a rounded, $fn-faceted offset.
    if let Some(&Value::Num(r)) = positional.first().or_else(|| named.get("r")) {
        let (fn_, fa, fs) = scope.fn_fa_fs();
        return (r, Join2D::Round, fragments(r.abs(), fn_, fa, fs));
    }
    // `delta` — named only (positional 0 is `r`). `chamfer = true` bevels the corners, else they miter.
    if let Some(&Value::Num(delta)) = named.get("delta") {
        let join = if matches!(named.get("chamfer"), Some(Value::Bool(true))) {
            Join2D::Bevel
        } else {
            Join2D::Miter
        };
        return (delta, join, 0);
    }
    (0.0, Join2D::Miter, 0) // no r/delta → identity
}

/// Resolve a `linear_extrude()` module's evaluated args to an [`ExtrudeKind::Linear`]. `height` (1st
/// positional / `height=`) defaults to 100 (OpenSCAD's fallback for a missing/degenerate height); `twist`
/// (degrees, default 0), `scale` (scalar → `[s, s]`, or `[x, y]`, default `[1, 1]`), and `center` ride
/// their named args. `slices` is the twist subdivision: explicit if given, else OpenSCAD's `$fn`-driven
/// default ([`helix_slices`]) — 1 when there's no twist.
pub(super) fn resolve_linear_extrude(
    positional: &[Value],
    named: &BTreeMap<String, Value>,
    scope: &Scope,
) -> ExtrudeKind {
    let height = arg(positional, named, 0, "height")
        .and_then(as_num)
        .filter(|h| h.is_finite() && *h > 0.0)
        .unwrap_or(100.0);
    let twist = named.get("twist").and_then(as_num).unwrap_or(0.0);
    let scale = extrude_scale(named.get("scale"));
    let center = matches!(named.get("center"), Some(Value::Bool(true)));
    let (fn_, _, _) = scope.fn_fa_fs();
    let slices = match named.get("slices").and_then(as_num) {
        Some(s) if s >= 1.0 => whole_u32(s),
        _ => helix_slices(twist, fn_),
    };
    // The twist RESAMPLE count is `$fn` (the perimeter fragments OpenSCAD splits the outline into); 0 when
    // `$fn` is unset ($fa/$fs mode — the backend falls back to a default fragment length). Only used when
    // `twist != 0`.
    let facets = if fn_ >= 3.0 { whole_u32(fn_) } else { 0 };
    ExtrudeKind::Linear {
        height,
        twist,
        scale,
        slices,
        facets,
        center,
    }
}

/// A value as a plain number, else `None`.
fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        _ => None,
    }
}

/// `linear_extrude`'s `scale`: a scalar → uniform `[s, s]`, an `[x, y]` list → per-axis, anything else →
/// `[1, 1]` (no scaling).
fn extrude_scale(v: Option<&Value>) -> [f64; 2] {
    match v {
        Some(Value::Num(s)) => [*s, *s],
        Some(Value::NumList(xs)) => [
            xs.first().copied().unwrap_or(1.0),
            xs.get(1).copied().unwrap_or(1.0),
        ],
        _ => [1.0, 1.0],
    }
}

/// The default twist-subdivision count — OpenSCAD's `Calc::get_helix_slices` for the `$fn > 0` case:
/// `max(1, |twist| · $fn / 360)`. No twist → a single band (a straight prism). The `$fa`/`$fs` fallback
/// (when `$fn == 0`) is deferred; a twisted extrude in the wild sets `$fn`.
fn helix_slices(twist: f64, fn_: f64) -> u32 {
    if twist == 0.0 {
        return 1;
    }
    whole_u32((twist.abs() * fn_ / 360.0).max(1.0))
}

/// A validated `slices` count → its `u32`, saturating at `u32::MAX`. Callers guarantee a FINITE value
/// `≥ 1` (an explicit `slices >= 1.0`, or [`helix_slices`]' `.max(1.0)`), so there's no sub-1/NaN guard.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: only a finite value ≥ 1 reaches the cast; it saturates at u32::MAX"
)]
fn whole_u32(s: f64) -> u32 {
    if s >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        s as u32
    }
}

/// The 3×4 affine for a transform module, from its EVALUATED arguments. Unknown/degenerate args fall
/// back to identity (OpenSCAD treats a malformed transform as a no-op rather than an error).
pub(super) fn transform_matrix(
    name: &str,
    positional: &[Value],
    named: &BTreeMap<String, Value>,
) -> Affine {
    Affine::row_major(match name {
        "translate" => translate(vec3(arg(positional, named, 0, "v"))),
        "scale" => scale(scale_factor(arg(positional, named, 0, "v"))),
        "mirror" => mirror(vec3(arg(positional, named, 0, "v"))),
        "multmatrix" => multmatrix(arg(positional, named, 0, "m")),
        "rotate" => rotate(
            arg(positional, named, 0, "a"),
            positional.get(1).or_else(|| named.get("v")),
        ),
        _ => IDENTITY,
    })
}

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// The positional arg at `i`, falling back to the named arg `name` (OpenSCAD arg-matching).
fn arg<'a>(
    positional: &'a [Value],
    named: &'a BTreeMap<String, Value>,
    i: usize,
    name: &str,
) -> Option<&'a Value> {
    positional.get(i).or_else(|| named.get(name))
}

/// A value as an `[x, y, z]` vector: a list takes its first three (missing → 0); anything else → zero.
fn vec3(v: Option<&Value>) -> [f64; 3] {
    match v {
        Some(Value::NumList(xs)) => [
            xs.first().copied().unwrap_or(0.0),
            xs.get(1).copied().unwrap_or(0.0),
            xs.get(2).copied().unwrap_or(0.0),
        ],
        _ => [0.0, 0.0, 0.0],
    }
}

/// A scale factor: a list is per-axis, a scalar is uniform (`scale(2)` = `scale([2,2,2])`), else identity.
fn scale_factor(v: Option<&Value>) -> [f64; 3] {
    match v {
        Some(Value::Num(s)) => [*s, *s, *s],
        Some(Value::NumList(xs)) => [
            xs.first().copied().unwrap_or(1.0),
            xs.get(1).copied().unwrap_or(1.0),
            xs.get(2).copied().unwrap_or(1.0),
        ],
        _ => [1.0, 1.0, 1.0],
    }
}

fn translate([x, y, z]: [f64; 3]) -> [f64; 12] {
    [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z]
}

fn scale([x, y, z]: [f64; 3]) -> [f64; 12] {
    [x, 0.0, 0.0, 0.0, 0.0, y, 0.0, 0.0, 0.0, 0.0, z, 0.0]
}

/// Reflection across the plane through the origin with normal `n` (OpenSCAD `mirror`): `I − 2·n̂·n̂ᵀ`.
/// A zero normal → identity.
fn mirror([x, y, z]: [f64; 3]) -> [f64; 12] {
    let len2 = x * x + y * y + z * z;
    if len2 == 0.0 {
        return IDENTITY;
    }
    let len = len2.sqrt();
    let (nx, ny, nz) = (x / len, y / len, z / len);
    [
        1.0 - 2.0 * nx * nx,
        -2.0 * nx * ny,
        -2.0 * nx * nz,
        0.0,
        -2.0 * ny * nx,
        1.0 - 2.0 * ny * ny,
        -2.0 * ny * nz,
        0.0,
        -2.0 * nz * nx,
        -2.0 * nz * ny,
        1.0 - 2.0 * nz * nz,
        0.0,
    ]
}

/// `multmatrix(m)`: the caller passes a 4×4 (or 4×3) row-major matrix as a list of rows; take the first
/// three rows' first four columns. Malformed → identity.
fn multmatrix(v: Option<&Value>) -> [f64; 12] {
    let Some(Value::List(rows)) = v else {
        return IDENTITY;
    };
    let mut m = IDENTITY;
    for r in 0..3 {
        let Some(Value::NumList(row)) = rows.get(r) else {
            return IDENTITY;
        };
        for c in 0..4 {
            m[r * 4 + c] = row
                .get(c)
                .copied()
                .unwrap_or(if r == c { 1.0 } else { 0.0 });
        }
    }
    m
}

/// `rotate`: `rotate(a)` (scalar → about +Z), `rotate([x,y,z])` (Euler, applied X then Y then Z, i.e.
/// `Rz·Ry·Rx`), or `rotate(a, v)` (angle `a` about axis `v`, Rodrigues). Uses exact-quadrant trig.
fn rotate(a: Option<&Value>, axis: Option<&Value>) -> [f64; 12] {
    match (a, axis) {
        (Some(Value::Num(angle)), Some(Value::NumList(_))) => angle_axis(*angle, vec3(axis)),
        (Some(Value::Num(angle)), _) => euler([0.0, 0.0, *angle]), // scalar → about Z
        (Some(Value::NumList(_)), _) => euler(vec3(a)),
        _ => IDENTITY,
    }
}

/// Euler `Rz(c)·Ry(b)·Rx(a)` for `[a, b, c]` in degrees.
fn euler([a, b, c]: [f64; 3]) -> [f64; 12] {
    let (ca, sa) = (cos_degrees(a), sin_degrees(a));
    let (cb, sb) = (cos_degrees(b), sin_degrees(b));
    let (cc, sc) = (cos_degrees(c), sin_degrees(c));
    [
        cc * cb,
        cc * sb * sa - sc * ca,
        cc * sb * ca + sc * sa,
        0.0,
        sc * cb,
        sc * sb * sa + cc * ca,
        sc * sb * ca - cc * sa,
        0.0,
        -sb,
        cb * sa,
        cb * ca,
        0.0,
    ]
}

/// Rotation by `angle` degrees about unit-normalized `axis` (Rodrigues). Zero axis → identity.
fn angle_axis(angle: f64, [x, y, z]: [f64; 3]) -> [f64; 12] {
    let len2 = x * x + y * y + z * z;
    if len2 == 0.0 {
        return IDENTITY;
    }
    let len = len2.sqrt();
    let (ux, uy, uz) = (x / len, y / len, z / len);
    let c = cos_degrees(angle);
    let s = sin_degrees(angle);
    let t = 1.0 - c;
    [
        t * ux * ux + c,
        t * ux * uy - s * uz,
        t * ux * uz + s * uy,
        0.0,
        t * ux * uy + s * uz,
        t * uy * uy + c,
        t * uy * uz - s * ux,
        0.0,
        t * ux * uz - s * uy,
        t * uy * uz + s * ux,
        t * uz * uz + c,
        0.0,
    ]
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "translate/scale/mirror/multmatrix matrices are EXACT literals; rotate uses approx()"
)]
mod tests {
    use std::collections::BTreeMap;

    use super::{IDENTITY, is_boolean, is_transform, transform_matrix};
    use crate::Value;

    /// A number list value.
    fn nl(xs: &[f64]) -> Value {
        Value::num_list(xs.to_vec())
    }
    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }
    /// `transform_matrix` as a row-major `[f64; 12]`, for the exact-literal asserts.
    fn tm(name: &str, positional: &[Value], named: &BTreeMap<String, Value>) -> [f64; 12] {
        transform_matrix(name, positional, named).as_row_major()
    }

    #[test]
    fn predicates() {
        for t in ["translate", "rotate", "scale", "mirror", "multmatrix"] {
            assert!(is_transform(t));
        }
        assert!(!is_transform("cube") && !is_transform("union"));
        for b in ["union", "difference", "intersection"] {
            assert!(is_boolean(b));
        }
        assert!(!is_boolean("translate"));
    }

    #[test]
    fn translate_scale_mirror() {
        let none = BTreeMap::new();
        assert_eq!(
            tm("translate", &[nl(&[1.0, 2.0, 3.0])], &none),
            [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0, 3.0]
        );
        assert_eq!(tm("translate", &[nl(&[1.0, 2.0])], &none)[11], 0.0); // short → pad z
        assert_eq!(tm("translate", &[Value::Num(9.0)], &none), IDENTITY); // scalar → zero vec
        assert_eq!(
            tm("scale", &[nl(&[2.0, 3.0, 4.0])], &none),
            [2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 4.0, 0.0]
        );
        assert_eq!(tm("scale", &[Value::Num(5.0)], &none)[0], 5.0); // uniform
        assert_eq!(tm("scale", &[nl(&[2.0])], &none)[5], 1.0); // short → pad 1
        assert_eq!(tm("scale", &[Value::Bool(true)], &none), IDENTITY); // non-numeric
        assert_eq!(tm("mirror", &[nl(&[1.0, 0.0, 0.0])], &none)[0], -1.0); // reflect x
        assert_eq!(tm("mirror", &[nl(&[0.0, 0.0, 0.0])], &none), IDENTITY); // zero normal
    }

    #[test]
    fn multmatrix_passthrough_and_malformed() {
        let none = BTreeMap::new();
        let m = Value::list(vec![
            nl(&[1.0, 0.0, 0.0, 7.0]),
            nl(&[0.0, 1.0, 0.0, 8.0]),
            nl(&[0.0, 0.0, 1.0, 9.0]),
            nl(&[0.0, 0.0, 0.0, 1.0]),
        ]);
        assert_eq!(
            tm("multmatrix", &[m], &none),
            [1.0, 0.0, 0.0, 7.0, 0.0, 1.0, 0.0, 8.0, 0.0, 0.0, 1.0, 9.0]
        );
        // short row → padded from identity; not-a-list arg → identity; a non-list row → identity.
        assert_eq!(
            tm(
                "multmatrix",
                &[Value::list(vec![nl(&[2.0]), nl(&[]), nl(&[])])],
                &none
            ),
            [2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        );
        assert_eq!(tm("multmatrix", &[Value::Num(1.0)], &none), IDENTITY);
        let bad = Value::list(vec![nl(&[1.0]), Value::Num(2.0), nl(&[3.0])]);
        assert_eq!(tm("multmatrix", &[bad], &none), IDENTITY);
    }

    #[test]
    fn rotate_scalar_euler_and_axis() {
        let none = BTreeMap::new();
        // scalar 90° about +Z: col0 = (cos, sin) = (0, 1).
        let rz = tm("rotate", &[Value::Num(90.0)], &none);
        approx(rz[0], 0.0);
        approx(rz[4], 1.0);
        approx(rz[1], -1.0);
        // euler [90,0,0] about X: the y-axis maps toward +z.
        let rx = tm("rotate", &[nl(&[90.0, 0.0, 0.0])], &none);
        approx(rx[5], 0.0);
        approx(rx[9], 1.0);
        // angle-axis 90° about a NON-unit z axis (exercises normalization) == scalar 90.
        let aa = tm("rotate", &[Value::Num(90.0), nl(&[0.0, 0.0, 2.0])], &none);
        approx(aa[0], 0.0);
        approx(aa[4], 1.0);
        assert_eq!(
            tm("rotate", &[Value::Num(90.0), nl(&[0.0, 0.0, 0.0])], &none),
            IDENTITY // zero axis
        );
        assert_eq!(tm("rotate", &[], &none), IDENTITY); // no args
        // named fallback: rotate(a = 90).
        let mut named = BTreeMap::new();
        named.insert("a".to_string(), Value::Num(90.0));
        approx(tm("rotate", &[], &named)[4], 1.0);
        // an unrecognized transform name → identity.
        assert_eq!(tm("bogus", &[], &none), IDENTITY);
    }
}
