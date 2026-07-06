//! Geometry primitives — the typed coordinate vocabulary (newtypes over bare `f64` arrays, chotchki's
//! standing preference). Everything is `f64`: the precision floor is the tessellation + the OFF export,
//! never an f32 downcast (the kernel imports through Manifold's `MeshGL64`).
//!
//! - [`Vec3`] — a 3D coordinate. A point OR a direction; OpenSCAD conflates them, so one type serves.
//! - [`Tri`] — a triangle, three vertex indices into a mesh's vertex list.
//! - [`Affine`] — a 3×4 ROW-MAJOR affine transform (OpenSCAD `multmatrix` order).
//!
//! `Rgba` (the `color()` model, BOSL2-critical) joins this vocabulary at J.2.8, wired to a real use.

use std::ops::{Add, Index, Mul, Neg, Sub};

/// A 3D coordinate — a point or a vector (OpenSCAD doesn't distinguish).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    /// x coordinate.
    pub x: f64,
    /// y coordinate.
    pub y: f64,
    /// z coordinate.
    pub z: f64,
}

impl Vec3 {
    /// The origin / zero vector.
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);

    /// A coordinate from its components.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3 { x, y, z }
    }

    /// From an `[x, y, z]` array (the FFI / literal boundary).
    #[must_use]
    pub const fn from_array([x, y, z]: [f64; 3]) -> Self {
        Vec3 { x, y, z }
    }

    /// To an `[x, y, z]` array (the FFI / export boundary).
    #[must_use]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    /// Dot product.
    #[must_use]
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    /// Cross product.
    #[must_use]
    pub fn cross(self, o: Self) -> Self {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    /// Euclidean length.
    #[must_use]
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    /// Unit vector in the same direction. A ~zero vector is returned unchanged (no NaN).
    #[must_use]
    pub fn normalize(self) -> Vec3 {
        let n = self.length();
        if n < 1e-12 { self } else { self * (1.0 / n) }
    }

    /// Angle to `other` in DEGREES (`0..=180`). Zero-length inputs clamp cleanly.
    #[must_use]
    pub fn angle_deg(self, other: Vec3) -> f64 {
        self.normalize()
            .dot(other.normalize())
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f64> for Vec3 {
    type Output = Vec3;
    /// Scale by a scalar.
    fn mul(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}
impl Index<usize> for Vec3 {
    type Output = f64;
    /// Component access: `0`→x, `1`→y, `2`→z. Panics out of range, like slice indexing.
    #[allow(
        clippy::panic,
        reason = "an out-of-range component index is a bug — panics by contract, exactly as [f64; 3] does"
    )]
    fn index(&self, i: usize) -> &f64 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            _ => panic!("Vec3 index {i} out of range (0..3)"),
        }
    }
}

/// A 2D coordinate — a point in the XY plane (the 2D subsystem's currency, J.3). A point OR a
/// direction, same as [`Vec3`]: OpenSCAD conflates them. Contours (rings of these) build a
/// [`Shape2D::Polygon`](crate::Shape2D).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    /// x coordinate.
    pub x: f64,
    /// y coordinate.
    pub y: f64,
}

impl Vec2 {
    /// A coordinate from its components.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Vec2 { x, y }
    }

    /// From an `[x, y]` array (the value / literal boundary).
    #[must_use]
    pub const fn from_array([x, y]: [f64; 2]) -> Self {
        Vec2 { x, y }
    }

    /// To an `[x, y]` array (the FFI boundary — Manifold's `CrossSection` speaks `[f64; 2]`).
    #[must_use]
    pub const fn to_array(self) -> [f64; 2] {
        [self.x, self.y]
    }
}

/// A triangle — three vertex indices into a mesh's vertex list, in winding order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tri(pub [u32; 3]);

impl Tri {
    /// A triangle from its three vertex indices.
    #[must_use]
    pub const fn new(a: u32, b: u32, c: u32) -> Self {
        Tri([a, b, c])
    }

    /// The three vertex indices.
    #[must_use]
    pub const fn indices(self) -> [u32; 3] {
        self.0
    }
}

/// A 3×4 ROW-MAJOR affine transform `[m0..m11]`, applied to a point as
/// `[m0·x + m1·y + m2·z + m3, m4·x + … + m7, m8·x + … + m11]` (OpenSCAD `multmatrix` order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine(pub [f64; 12]);

impl Affine {
    /// The identity transform.
    pub const IDENTITY: Affine =
        Affine([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    /// From a 3×4 row-major `[m0..m11]`.
    #[must_use]
    pub const fn row_major(m: [f64; 12]) -> Self {
        Affine(m)
    }

    /// From a COLUMN-major `[m0..m11]` (Manifold's layout) — the exact inverse of [`to_column_major`],
    /// so a call site holding column-major data wraps it byte-for-byte instead of transposing by hand.
    ///
    /// [`to_column_major`]: Affine::to_column_major
    #[must_use]
    pub const fn from_column_major(c: [f64; 12]) -> Self {
        Affine([
            c[0], c[3], c[6], c[9], // row 0
            c[1], c[4], c[7], c[10], // row 1
            c[2], c[5], c[8], c[11], // row 2
        ])
    }

    /// The row-major `[m0..m11]`.
    #[must_use]
    pub const fn as_row_major(&self) -> [f64; 12] {
        self.0
    }

    /// The COLUMN-major `[m0..m11]` — the layout Manifold's `transform` wants (transpose the 3×4).
    #[must_use]
    pub const fn to_column_major(&self) -> [f64; 12] {
        let m = &self.0;
        [
            m[0], m[4], m[8], // column 0
            m[1], m[5], m[9], // column 1
            m[2], m[6], m[10], // column 2
            m[3], m[7], m[11], // translation
        ]
    }

    /// Apply the affine to a coordinate.
    #[must_use]
    pub fn apply(&self, v: Vec3) -> Vec3 {
        let m = &self.0;
        Vec3::new(
            m[0] * v.x + m[1] * v.y + m[2] * v.z + m[3],
            m[4] * v.x + m[5] * v.y + m[6] * v.z + m[7],
            m[8] * v.x + m[9] * v.y + m[10] * v.z + m[11],
        )
    }
}

/// A 2×3 ROW-MAJOR affine transform `[a, b, c, d, e, f]`, applied to a 2D point as
/// `[a·x + b·y + c, d·x + e·y + f]` — the 2D analogue of [`Affine`], for [`Shape2D`](crate::Shape2D)
/// transforms (translate / rotate / scale / mirror on a 2D shape).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2(pub [f64; 6]);

impl Affine2 {
    /// The identity transform.
    pub const IDENTITY: Affine2 = Affine2([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    /// From a 2×3 row-major `[a, b, c, d, e, f]`.
    #[must_use]
    pub const fn row_major(m: [f64; 6]) -> Self {
        Affine2(m)
    }

    /// The row-major `[a, b, c, d, e, f]`.
    #[must_use]
    pub const fn as_row_major(&self) -> [f64; 6] {
        self.0
    }

    /// The COLUMN-major `[a, d, b, e, c, f]` — the layout Manifold's 2D `transform` wants, three
    /// `(x, y)` column pairs (the two basis columns then the translation).
    #[must_use]
    pub const fn to_column_major(&self) -> [f64; 6] {
        let m = &self.0;
        [
            m[0], m[3], // column 0
            m[1], m[4], // column 1
            m[2], m[5], // translation
        ]
    }

    /// Apply the affine to a 2D coordinate.
    #[must_use]
    pub fn apply(&self, v: Vec2) -> Vec2 {
        let m = &self.0;
        Vec2::new(
            m[0] * v.x + m[1] * v.y + m[2],
            m[3] * v.x + m[4] * v.y + m[5],
        )
    }
}

/// A 3-axis SIZE — width / depth / height, same units as [`Vec3`] but a MEASUREMENT, not a point:
/// always-nonnegative magnitudes, and the ops say so (componentwise fit + volume, never dot / cross /
/// normalize). Distinct from `Vec3` on purpose — a bed size can't be passed where a point is wanted,
/// and a point can't be passed where a size is: the type IS the check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dims {
    /// Extent along x.
    pub x: f64,
    /// Extent along y.
    pub y: f64,
    /// Extent along z.
    pub z: f64,
}

impl Dims {
    /// A size from its three axis extents.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Dims { x, y, z }
    }

    /// From an `[x, y, z]` array.
    #[must_use]
    pub const fn from_array([x, y, z]: [f64; 3]) -> Self {
        Dims { x, y, z }
    }

    /// To an `[x, y, z]` array.
    #[must_use]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    /// The extent of the AABB between two corner points — `|max − min|` per axis (order-independent).
    #[must_use]
    pub fn from_extent(a: Vec3, b: Vec3) -> Self {
        Dims::new((b.x - a.x).abs(), (b.y - a.y).abs(), (b.z - a.z).abs())
    }

    /// Whether this size fits within `bed` on every axis (componentwise `≤`).
    #[must_use]
    pub fn fits_within(self, bed: Dims) -> bool {
        self.x <= bed.x && self.y <= bed.y && self.z <= bed.z
    }

    /// The box volume `x · y · z`.
    #[must_use]
    pub fn volume(self) -> f64 {
        self.x * self.y * self.z
    }
}

impl Index<usize> for Dims {
    type Output = f64;
    /// Axis access: `0`→x, `1`→y, `2`→z. Panics out of range, like slice indexing.
    #[allow(
        clippy::panic,
        reason = "an out-of-range axis index is a bug — panics by contract, like [f64; 3]"
    )]
    fn index(&self, i: usize) -> &f64 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            _ => panic!("Dims index {i} out of range (0..3)"),
        }
    }
}

/// An RGBA color — four channels in `[0, 1]` (OpenSCAD's `color()` model; alpha 1 = opaque). The named
/// CSS table (`color("red")`) + hex (`"#rgb"` … `"#rrggbbaa"`) are the `from_*` constructors. NOT
/// clamped — OpenSCAD stores an out-of-range channel verbatim (warns only at export), so we preserve
/// it. BOSL2-critical: `recolor` / `rainbow` / debug-viz all ride on this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red, `0..=1`.
    pub r: f64,
    /// Green, `0..=1`.
    pub g: f64,
    /// Blue, `0..=1`.
    pub b: f64,
    /// Alpha, `0..=1` (1 = opaque).
    pub a: f64,
}

impl Rgba {
    /// Opaque white.
    pub const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    /// A color from its four channels.
    #[must_use]
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Rgba { r, g, b, a }
    }

    /// An opaque color (`a = 1`).
    #[must_use]
    pub const fn opaque(r: f64, g: f64, b: f64) -> Self {
        Rgba::new(r, g, b, 1.0)
    }

    /// To an `[r, g, b, a]` array.
    #[must_use]
    pub const fn to_array(self) -> [f64; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// From 0-255 sRGB bytes (opaque) — what the named + hex tables produce.
    #[must_use]
    pub fn from_u8(r: u8, g: u8, b: u8) -> Self {
        Rgba::opaque(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        )
    }

    /// A CSS color NAME (case-insensitive) → its color, or `None` if unknown. `"transparent"` is the
    /// one OpenSCAD-specific entry: `{0, 0, 0, 0}` (fully transparent), NOT just black.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Rgba> {
        if name.eq_ignore_ascii_case("transparent") {
            return Some(Rgba::new(0.0, 0.0, 0.0, 0.0));
        }
        crate::webcolors::lookup(name).map(|[r, g, b]| Rgba::from_u8(r, g, b))
    }

    /// A hex string `"#rgb"` / `"#rgba"` / `"#rrggbb"` / `"#rrggbbaa"` → its color, `None` if malformed.
    /// Short forms scale each nibble by 17 (`#f80` → `#ff8800`).
    #[must_use]
    #[allow(
        clippy::many_single_char_names,
        reason = "r/g/b/a ARE the color channels; s/h the input + hex body"
    )]
    pub fn from_hex(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#')?;
        let chan = |v: u8| f64::from(v) / 255.0;
        let (r, g, b, a) = match h.len() {
            3 => (nibble(h, 0)?, nibble(h, 1)?, nibble(h, 2)?, 255),
            4 => (nibble(h, 0)?, nibble(h, 1)?, nibble(h, 2)?, nibble(h, 3)?),
            6 => (byte(h, 0)?, byte(h, 2)?, byte(h, 4)?, 255),
            8 => (byte(h, 0)?, byte(h, 2)?, byte(h, 4)?, byte(h, 6)?),
            _ => return None,
        };
        Some(Rgba::new(chan(r), chan(g), chan(b), chan(a)))
    }
}

/// One hex nibble at byte `i`, scaled to a full byte (`0xF` → `0xFF`).
fn nibble(h: &str, i: usize) -> Option<u8> {
    Some(u8::from_str_radix(h.get(i..i + 1)?, 16).ok()? * 17)
}
/// Two hex digits at byte `i` → a byte.
fn byte(h: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(h.get(i..i + 2)?, 16).ok()
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact vector/matrix arithmetic on literal inputs"
)]
mod tests {
    use super::{Affine, Affine2, Dims, Rgba, Tri, Vec2, Vec3};

    #[test]
    fn vec2_ops() {
        let a = Vec2::new(1.0, 2.0);
        assert_eq!(a.to_array(), [1.0, 2.0]);
        assert_eq!(Vec2::from_array([3.0, 4.0]), Vec2::new(3.0, 4.0));
    }

    #[test]
    fn affine2_apply_and_layouts() {
        // A translate-by-(5,7) plus a 2× x-scale: [2,0,5, 0,1,7].
        let m = Affine2::row_major([2.0, 0.0, 5.0, 0.0, 1.0, 7.0]);
        assert_eq!(m.apply(Vec2::new(3.0, 4.0)), Vec2::new(11.0, 11.0)); // 2·3+5, 1·4+7
        assert_eq!(m.as_row_major(), [2.0, 0.0, 5.0, 0.0, 1.0, 7.0]);
        // Column-major reorders row-major into three (x, y) column pairs.
        assert_eq!(m.to_column_major(), [2.0, 0.0, 0.0, 1.0, 5.0, 7.0]);
        assert_eq!(Affine2::IDENTITY.apply(a_point()), a_point()); // identity is a no-op
    }

    fn a_point() -> Vec2 {
        Vec2::new(9.0, -3.0)
    }

    #[test]
    fn vec3_ops() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        assert_eq!(a + b, Vec3::new(5.0, 7.0, 9.0));
        assert_eq!(b - a, Vec3::new(3.0, 3.0, 3.0));
        assert_eq!(a * 2.0, Vec3::new(2.0, 4.0, 6.0));
        assert_eq!(-a, Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(a.dot(b), 32.0); // 4 + 10 + 18
        assert_eq!(
            Vec3::new(1.0, 0.0, 0.0).cross(Vec3::new(0.0, 1.0, 0.0)),
            Vec3::new(0.0, 0.0, 1.0)
        );
        assert_eq!(Vec3::new(3.0, 4.0, 0.0).length(), 5.0);
        assert_eq!(
            Vec3::from_array([1.0, 2.0, 3.0]).to_array(),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(Vec3::ZERO, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!([a[0], a[1], a[2]], [1.0, 2.0, 3.0]); // Index<usize>
        // normalize + angle_deg (ported from the old geom::V3 helpers).
        assert_eq!(
            Vec3::new(0.0, 0.0, 5.0).normalize(),
            Vec3::new(0.0, 0.0, 1.0)
        );
        assert_eq!(Vec3::ZERO.normalize(), Vec3::ZERO); // ~zero → unchanged, no NaN
        assert!((Vec3::new(1.0, 0.0, 0.0).angle_deg(Vec3::new(0.0, 2.0, 0.0)) - 90.0).abs() < 1e-9);
        assert!(
            Vec3::new(1.0, 0.0, 0.0)
                .angle_deg(Vec3::new(3.0, 0.0, 0.0))
                .abs()
                < 1e-9
        ); // parallel
    }

    #[test]
    fn dims_measurements() {
        let d = Dims::new(10.0, 20.0, 30.0);
        assert_eq!(d.to_array(), [10.0, 20.0, 30.0]);
        assert_eq!(Dims::from_array([1.0, 2.0, 3.0]), Dims::new(1.0, 2.0, 3.0));
        assert_eq!(d.volume(), 6000.0);
        assert_eq!([d[0], d[1], d[2]], [10.0, 20.0, 30.0]); // Index
        // extent between corners: |max − min| per axis, order-independent + nonneg.
        let (a, b) = (Vec3::new(5.0, 0.0, -2.0), Vec3::new(1.0, 3.0, 4.0));
        assert_eq!(Dims::from_extent(a, b), Dims::new(4.0, 3.0, 6.0));
        assert_eq!(Dims::from_extent(b, a), Dims::new(4.0, 3.0, 6.0));
        // fits_within: componentwise ≤.
        assert!(Dims::new(10.0, 10.0, 10.0).fits_within(Dims::new(10.0, 20.0, 30.0)));
        assert!(!Dims::new(10.0, 25.0, 10.0).fits_within(Dims::new(10.0, 20.0, 30.0)));
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn dims_index_out_of_range() {
        let _ = Dims::new(1.0, 2.0, 3.0)[3];
    }

    #[test]
    fn rgba_named_hex_and_alpha() {
        assert_eq!(Rgba::from_name("red"), Some(Rgba::opaque(1.0, 0.0, 0.0)));
        assert_eq!(Rgba::from_name("RED"), Some(Rgba::opaque(1.0, 0.0, 0.0))); // case-insensitive
        assert_eq!(
            Rgba::from_name("transparent"),
            Some(Rgba::new(0.0, 0.0, 0.0, 0.0))
        );
        assert_eq!(Rgba::from_name("notacolor"), None);
        assert_eq!(Rgba::from_u8(255, 0, 0), Rgba::opaque(1.0, 0.0, 0.0));
        assert_eq!(Rgba::WHITE.to_array(), [1.0, 1.0, 1.0, 1.0]);
        // hex: #rgb short form scales each nibble ×17; #rrggbb; #rgba/#rrggbbaa carry alpha.
        assert_eq!(Rgba::from_hex("#f80"), Rgba::from_hex("#ff8800"));
        assert_eq!(Rgba::from_hex("#ff0000"), Some(Rgba::opaque(1.0, 0.0, 0.0)));
        assert_eq!(Rgba::from_hex("#ffff"), Rgba::from_hex("#ffffffff"));
        assert_eq!(Rgba::from_hex("#00000080").unwrap().a, 128.0 / 255.0);
        assert!(Rgba::from_hex("#xyz").is_none() && Rgba::from_hex("nope").is_none());
        assert!(Rgba::from_hex("#12345").is_none()); // wrong length (not 3/4/6/8)
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn vec3_index_out_of_range() {
        let _ = Vec3::ZERO[3];
    }

    #[test]
    fn tri_indices() {
        assert_eq!(Tri::new(0, 1, 2).indices(), [0, 1, 2]);
        assert_eq!(Tri([3, 4, 5]).0, [3, 4, 5]);
    }

    #[test]
    fn affine_apply_and_layouts() {
        // A translate-by-(5,2,9): identity + translation column.
        let t = Affine::row_major([1.0, 0.0, 0.0, 5.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0, 9.0]);
        assert_eq!(t.apply(Vec3::new(1.0, 1.0, 1.0)), Vec3::new(6.0, 3.0, 10.0));
        assert_eq!(t.as_row_major()[3], 5.0);
        // column-major transpose: translation moves to the last 3 slots.
        assert_eq!(
            t.to_column_major(),
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 5.0, 2.0, 9.0]
        );
        assert_eq!(
            Affine::IDENTITY.apply(Vec3::new(7.0, 8.0, 9.0)),
            Vec3::new(7.0, 8.0, 9.0)
        );
        // from_column_major is the exact inverse of to_column_major (wrap column-major data losslessly).
        let cm = [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
        ];
        assert_eq!(Affine::from_column_major(cm).to_column_major(), cm);
    }
}
