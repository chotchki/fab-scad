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

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact vector/matrix arithmetic on literal inputs"
)]
mod tests {
    use super::{Affine, Tri, Vec3};

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
