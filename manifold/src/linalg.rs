//! The internal linear-algebra layer — a concrete-typed port of the subset of Manifold's `linalg.h`
//! the kernel uses (Sterling Orsten's single-header, specialized to `double`).
//!
//! Oracle-critical: `dot`/`cross`/`normalize`/matrix-mul OPERATION ORDER must match `linalg.h`
//! bit-for-bit or the C++ differential oracle diverges. So this is manifold-OWNED and validated
//! against the oracle, not borrowed from fab-lang's `geom.rs` (whose ops were written for the
//! evaluator, and whose `Affine([f64;12])` is even row-major where Manifold's `mat3x4` is column-
//! major). Once R0/R1 proves it clean, this lifts to a shared `fab-types` leaf crate (and fab-lang's
//! geom.rs migrates onto it) — see SPEC_manifold-rs.md. Componentwise arithmetic goes through a macro
//! (one definition = no per-component transcription drift); `dot`/`cross`/matmul stay EXPLICIT so the
//! summation order is auditable against `linalg.h`. No `mul_add` (FMA is banned — the #1 determinism
//! hazard); every accumulation is written left-to-right, which is Rust's default associativity.

use core::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

// ---------------------------------------------------------------------------
// Vector types. #[repr(C)] so the memory layout matches the {x,y,z,w} field order Manifold's
// MeshGL flat buffers assume (verts are stored x,y,z,x,y,z,...).
// ---------------------------------------------------------------------------

/// 2-vector (`la::vec<double,2>`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

/// 3-vector (`la::vec<double,3>`) — the kernel workhorse.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// 4-vector (`la::vec<double,4>`) — homogeneous points for the affine multiply.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(C)]
pub struct Vec4 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

// Generates the componentwise + scalar-broadcast arithmetic for one vec type. Each component is
// independent (no cross-component summation), so there's no op-ORDER subtlety here — the ordered
// operations (dot/cross/matmul) are written by hand below, NOT generated.
macro_rules! vec_arith {
    ($V:ident { $($f:ident),+ }, $n:literal) => {
        impl $V {
            /// Componentwise constructor.
            #[inline]
            pub const fn new($($f: f64),+) -> Self { Self { $($f),+ } }
            /// All components set to `s`.
            #[inline]
            pub const fn splat(s: f64) -> Self { Self { $($f: s),+ } }
            /// The zero vector.
            pub const ZERO: Self = Self { $($f: 0.0),+ };
            /// Componentwise minimum (`la::min`).
            #[inline]
            pub fn cmin(self, o: Self) -> Self { Self { $($f: self.$f.min(o.$f)),+ } }
            /// Componentwise maximum (`la::max`).
            #[inline]
            pub fn cmax(self, o: Self) -> Self { Self { $($f: self.$f.max(o.$f)),+ } }
            /// Componentwise absolute value (`la::abs`).
            #[inline]
            pub fn cabs(self) -> Self { Self { $($f: self.$f.abs()),+ } }
            /// True iff every component is finite (`la::all(la::isfinite(..))`).
            #[inline]
            pub fn is_finite(self) -> bool { $(self.$f.is_finite())&&+ }
            /// Square of the length, `dot(self, self)` (constexpr `length2`).
            #[inline]
            pub fn length2(self) -> f64 { self.dot(self) }
            /// Euclidean length, `sqrt(length2)`. `sqrt` is IEEE-exact hardware — deterministic.
            #[inline]
            pub fn length(self) -> f64 { self.length2().sqrt() }
            /// Unit vector in the same direction, `self / length(self)` (undefined for zero length).
            #[inline]
            pub fn normalize(self) -> Self { self / self.length() }
        }
        impl Add for $V {
            type Output = Self;
            #[inline]
            fn add(self, o: Self) -> Self { Self { $($f: self.$f + o.$f),+ } }
        }
        impl Sub for $V {
            type Output = Self;
            #[inline]
            fn sub(self, o: Self) -> Self { Self { $($f: self.$f - o.$f),+ } }
        }
        // vec * vec is COMPONENTWISE (`cmul`), matching linalg's `operator*`.
        impl Mul for $V {
            type Output = Self;
            #[inline]
            fn mul(self, o: Self) -> Self { Self { $($f: self.$f * o.$f),+ } }
        }
        impl Div for $V {
            type Output = Self;
            #[inline]
            fn div(self, o: Self) -> Self { Self { $($f: self.$f / o.$f),+ } }
        }
        impl Mul<f64> for $V {
            type Output = Self;
            #[inline]
            fn mul(self, s: f64) -> Self { Self { $($f: self.$f * s),+ } }
        }
        impl Mul<$V> for f64 {
            type Output = $V;
            #[inline]
            fn mul(self, v: $V) -> $V { $V { $($f: self * v.$f),+ } }
        }
        impl Div<f64> for $V {
            type Output = Self;
            #[inline]
            fn div(self, s: f64) -> Self { Self { $($f: self.$f / s),+ } }
        }
        impl Neg for $V {
            type Output = Self;
            #[inline]
            fn neg(self) -> Self { Self { $($f: -self.$f),+ } }
        }
        impl AddAssign for $V {
            #[inline]
            fn add_assign(&mut self, o: Self) { $(self.$f += o.$f;)+ }
        }
        impl SubAssign for $V {
            #[inline]
            fn sub_assign(&mut self, o: Self) { $(self.$f -= o.$f;)+ }
        }
        impl MulAssign for $V {
            #[inline]
            fn mul_assign(&mut self, o: Self) { $(self.$f *= o.$f;)+ }
        }
        impl From<[f64; $n]> for $V {
            #[inline]
            fn from(a: [f64; $n]) -> Self { let [$($f),+] = a; Self { $($f),+ } }
        }
        impl From<$V> for [f64; $n] {
            #[inline]
            fn from(v: $V) -> Self { [$(v.$f),+] }
        }
    };
}

vec_arith!(Vec2 { x, y }, 2);
vec_arith!(Vec3 { x, y, z }, 3);
vec_arith!(Vec4 { x, y, z, w }, 4);

impl Vec2 {
    /// Dot product, `sum(self * o)` — left-to-right (`x·x + y·y`).
    #[inline]
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y
    }
}

impl Vec3 {
    /// Dot product, `sum(self * o)` — left-to-right (`(x·x + y·y) + z·z`).
    #[inline]
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    /// Cross product, exactly `linalg.h`'s ordering:
    /// `{y·z − z·y, z·x − x·z, x·y − y·x}`.
    #[inline]
    pub fn cross(self, o: Self) -> Self {
        Self {
            x: self.y * o.z - self.z * o.y,
            y: self.z * o.x - self.x * o.z,
            z: self.x * o.y - self.y * o.x,
        }
    }
}

impl Vec4 {
    /// Dot product, `sum(self * o)` — left-to-right.
    #[inline]
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }
    /// Homogeneous point/vector from a `Vec3` plus a `w` (Manifold's `vec4(v, w)`).
    #[inline]
    pub fn from_vec3(v: Vec3, w: f64) -> Self {
        Self {
            x: v.x,
            y: v.y,
            z: v.z,
            w,
        }
    }
    /// Drop `w` to a `Vec3` (the implicit `vec3(vec4)` truncation Manifold uses on affine results).
    #[inline]
    pub fn xyz(self) -> Vec3 {
        Vec3 {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }
}

// ---------------------------------------------------------------------------
// Affine transform. Manifold stores it as `mat3x4` = `mat<double,3,4>` — COLUMN-MAJOR, 4 columns of
// Vec3 (columns x,y,z are the linear part, w is the translation). `m * vec4 = x·b.x + y·b.y + z·b.z +
// w·b.w`, summed left-to-right (linalg.h mul(mat<M,4>, vec4)).
// ---------------------------------------------------------------------------

/// Affine transform `mat<double,3,4>`: 4 Vec3 columns, column-major (matches Manifold `mat3x4`).
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Mat3x4 {
    /// First basis column.
    pub x: Vec3,
    /// Second basis column.
    pub y: Vec3,
    /// Third basis column.
    pub z: Vec3,
    /// Translation column.
    pub w: Vec3,
}

impl Mat3x4 {
    /// The identity affine (basis = I, translation = 0).
    pub const IDENTITY: Self = Self {
        x: Vec3::new(1.0, 0.0, 0.0),
        y: Vec3::new(0.0, 1.0, 0.0),
        z: Vec3::new(0.0, 0.0, 1.0),
        w: Vec3::ZERO,
    };

    /// Transform a homogeneous `Vec4` → `Vec3`: `x·b.x + y·b.y + z·b.z + w·b.w` (left-to-right).
    #[inline]
    pub fn mul_vec4(self, b: Vec4) -> Vec3 {
        self.x * b.x + self.y * b.y + self.z * b.z + self.w * b.w
    }

    /// Transform a point (implicitly homogeneous, `w = 1`): `linear·p + translation`.
    #[inline]
    pub fn transform_point(self, p: Vec3) -> Vec3 {
        self.mul_vec4(Vec4::from_vec3(p, 1.0))
    }

    /// Transform a direction (translation-free, `w = 0`): `linear·v`.
    #[inline]
    pub fn transform_dir(self, v: Vec3) -> Vec3 {
        self.mul_vec4(Vec4::from_vec3(v, 0.0))
    }

    /// The linear (3×3) part — the first three columns (`mat3(transform)` in Manifold).
    #[inline]
    pub fn linear(self) -> Mat3 {
        Mat3 {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }

    /// Are all 12 entries finite? (`la::all(la::isfinite(transform))`.)
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }

    /// Pure translation (`CsgNode::Translate`): identity basis, translation `t`.
    #[inline]
    pub fn translate(t: Vec3) -> Mat3x4 {
        Mat3x4 {
            w: t,
            ..Mat3x4::IDENTITY
        }
    }

    /// Per-axis scale (`CsgNode::Scale`): diagonal basis `v`, zero translation.
    #[inline]
    pub fn scale(v: Vec3) -> Mat3x4 {
        Mat3x4 {
            x: Vec3::new(v.x, 0.0, 0.0),
            y: Vec3::new(0.0, v.y, 0.0),
            z: Vec3::new(0.0, 0.0, v.z),
            w: Vec3::ZERO,
        }
    }

    /// Euler rotation about x, then y, then z (degrees) — `CsgNode::Rotate`, `rZ·rY·rX` with zero
    /// translation. The per-axis matrices are built column-major exactly as the C++ (`sind`/`cosd` via
    /// [`crate::mathf`], NOT `f64::sin`, so it's bit-identical native==wasm).
    pub fn rotate(x_degrees: f64, y_degrees: f64, z_degrees: f64) -> Mat3x4 {
        use crate::mathf::{cosd, sind};
        let (cx, sx) = (cosd(x_degrees), sind(x_degrees));
        let (cy, sy) = (cosd(y_degrees), sind(y_degrees));
        let (cz, sz) = (cosd(z_degrees), sind(z_degrees));
        // Column-major: each `Vec3::new(...)` is a COLUMN (verbatim from `csg_tree.cpp`).
        let r_x = Mat3 {
            x: Vec3::new(1.0, 0.0, 0.0),
            y: Vec3::new(0.0, cx, sx),
            z: Vec3::new(0.0, -sx, cx),
        };
        let r_y = Mat3 {
            x: Vec3::new(cy, 0.0, -sy),
            y: Vec3::new(0.0, 1.0, 0.0),
            z: Vec3::new(sy, 0.0, cy),
        };
        let r_z = Mat3 {
            x: Vec3::new(cz, sz, 0.0),
            y: Vec3::new(-sz, cz, 0.0),
            z: Vec3::new(0.0, 0.0, 1.0),
        };
        let linear = r_z.mul_mat3(r_y).mul_mat3(r_x);
        Mat3x4 {
            x: linear.x,
            y: linear.y,
            z: linear.z,
            w: Vec3::ZERO,
        }
    }
}

impl core::ops::Mul for Mat3x4 {
    type Output = Mat3x4;

    /// Compose two affines: `self * rhs` applies `rhs` FIRST, then `self` — Manifold's `m * Mat4(other)`
    /// used by `CsgLeafNode::Transform` to fold chained Translate/Scale/Rotate into ONE matrix. For a
    /// point `p`: `self(rhs(p)) = (self.linear·rhs.linear)·p + (self.linear·rhs.w + self.w)`. Folding
    /// FIRST, then applying `transform` ONCE, is what keeps the eager port bit-identical to the lazy C++
    /// (a per-op `transform` would rescale epsilon by each factor's spectral norm, not the product's).
    #[inline]
    fn mul(self, rhs: Mat3x4) -> Mat3x4 {
        Mat3x4 {
            x: self.transform_dir(rhs.x),
            y: self.transform_dir(rhs.y),
            z: self.transform_dir(rhs.z),
            w: self.transform_point(rhs.w),
        }
    }
}

/// Linear `mat<double,3,3>`: 3 Vec3 columns, column-major (Manifold `mat3`). Used by the transform
/// normal/winding/epsilon math — the affine [`Mat3x4`]'s linear part.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mat3 {
    /// First basis column.
    pub x: Vec3,
    /// Second basis column.
    pub y: Vec3,
    /// Third basis column.
    pub z: Vec3,
}

impl Mat3 {
    /// Matrix·vector (`M * v`, column-major): `x·v.x + y·v.y + z·v.z`.
    #[inline]
    pub fn mul_vec(self, v: Vec3) -> Vec3 {
        self.x * v.x + self.y * v.y + self.z * v.z
    }

    /// Matrix·matrix (`self * rhs`, column-major): each result column is `self·(rhs column)`.
    #[inline]
    pub fn mul_mat3(self, rhs: Mat3) -> Mat3 {
        Mat3 {
            x: self.mul_vec(rhs.x),
            y: self.mul_vec(rhs.y),
            z: self.mul_vec(rhs.z),
        }
    }

    /// Determinant `a·(b×c)` of the columns — sign gives handedness (negative ⇒ the transform mirrors).
    #[inline]
    pub fn determinant(self) -> f64 {
        self.x.dot(self.y.cross(self.z))
    }

    /// The normal transform `inverse(transpose(M))` (`shared.h` `NormalTransform`), by which face/vertex
    /// normals transform. For columns `a,b,c` this is the columns `(b×c, c×a, a×b) / det` — the exact
    /// inverse-transpose without a general matrix inverse (verified on rotations and scales).
    #[inline]
    pub fn normal_transform(self) -> Mat3 {
        let (a, b, c) = (self.x, self.y, self.z);
        let inv_det = 1.0 / a.dot(b.cross(c));
        Mat3 {
            x: b.cross(c) * inv_det,
            y: c.cross(a) * inv_det,
            z: a.cross(b) * inv_det,
        }
    }

    /// The spectral norm = the largest singular value (`svd.h` `SpectralNorm` = `SVD(A).S[0][0]`), used
    /// ONLY to scale a transformed mesh's epsilon. Manifold computes it via an iterative Jacobi SVD; we
    /// use deterministic power iteration on `MᵀM` (fixed 32 iters + IEEE `sqrt`, so native≡wasm) — the
    /// result may differ from C++'s in the last ULPs, but epsilon is a tolerance that never enters the
    /// output geometry, only downstream merge decisions, so it is not part of the geometry fingerprint.
    pub fn spectral_norm(self) -> f64 {
        let mtm = |v: Vec3| {
            let mv = self.mul_vec(v);
            Vec3::new(self.x.dot(mv), self.y.dot(mv), self.z.dot(mv))
        };
        let mut v = Vec3::new(1.0, 1.0, 1.0);
        for _ in 0..32 {
            let w = mtm(v);
            let n2 = w.dot(w);
            if n2 == 0.0 {
                return 0.0;
            }
            v = w * (1.0 / n2.sqrt());
        }
        mtm(v).dot(v).max(0.0).sqrt()
    }
}

// ---------------------------------------------------------------------------
// Axis-aligned bounding volumes (common.h Box / Rect) — the mesh spine + collider lean on these.
// ---------------------------------------------------------------------------

/// Axis-aligned 3D bounding box (common.h `Box`). Default is the INVERTED-infinity empty box, so the
/// first `union_point` sets both bounds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Box3 {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Default for Box3 {
    fn default() -> Self {
        Self {
            min: Vec3::splat(f64::INFINITY),
            max: Vec3::splat(f64::NEG_INFINITY),
        }
    }
}

impl Box3 {
    /// A box that contains the two given points.
    #[inline]
    pub fn from_points(a: Vec3, b: Vec3) -> Self {
        Self {
            min: a.cmin(b),
            max: a.cmax(b),
        }
    }
    /// Dimensions (`max - min`).
    #[inline]
    pub fn size(self) -> Vec3 {
        self.max - self.min
    }
    /// Center (`0.5 * (max + min)`).
    #[inline]
    pub fn center(self) -> Vec3 {
        0.5 * (self.max + self.min)
    }
    /// The absolute-largest coordinate of any contained point (`common.h` `Box::Scale`) — the length
    /// scale the epsilon/tolerance model is measured against. `max(|min|, |max|)` componentwise, then
    /// the largest of x/y/z, matching the C++ reduction order.
    #[inline]
    pub fn scale(self) -> f64 {
        let abs_max = self.min.cabs().cmax(self.max.cabs());
        abs_max.x.max(abs_max.y.max(abs_max.z))
    }
    /// Expand in place to include a point.
    #[inline]
    pub fn union_point(&mut self, p: Vec3) {
        self.min = self.min.cmin(p);
        self.max = self.max.cmax(p);
    }
    /// Union with another box.
    #[inline]
    pub fn union(self, o: Self) -> Self {
        Self {
            min: self.min.cmin(o.min),
            max: self.max.cmax(o.max),
        }
    }
    /// Do the two boxes overlap (inclusive)?
    #[inline]
    pub fn overlaps(self, o: Self) -> bool {
        self.min.x <= o.max.x
            && self.min.y <= o.max.y
            && self.min.z <= o.max.z
            && self.max.x >= o.min.x
            && self.max.y >= o.min.y
            && self.max.z >= o.min.z
    }
    /// Does a point fall within this box's XY extent, IGNORING z (`common.h`
    /// `DoesOverlap(vec3)` — "projected in z")? The z-raycast winding query in the boolean uses this:
    /// a vertex "hits" a face box iff it's under/over it in the XY plane.
    #[inline]
    pub fn does_overlap_point_xy(self, p: Vec3) -> bool {
        p.x <= self.max.x && p.x >= self.min.x && p.y <= self.max.y && p.y >= self.min.y
    }
    /// Finite bounds?
    #[inline]
    pub fn is_finite(self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }
    /// Transform by an axis-aligned affine (rotations multiples of 90°, else no longer bounds).
    #[inline]
    pub fn transform(self, m: Mat3x4) -> Self {
        let a = m.transform_point(self.min);
        let b = m.transform_point(self.max);
        Self {
            min: a.cmin(b),
            max: a.cmax(b),
        }
    }
}

/// Axis-aligned 2D bounding rectangle (common.h `Rect`) — `Box3`'s 2D sibling, the `CrossSection`
/// bounds type (M.5.4). Default is the INVERTED-infinity empty rect, so the first `union_point` sets
/// both bounds; note that per the C++, the empty default is CONTAINED by every rect and overlaps
/// nothing (the ∞/−∞ comparisons resolve that way by construction).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    /// Minimum corner.
    pub min: Vec2,
    /// Maximum corner.
    pub max: Vec2,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            min: Vec2::splat(f64::INFINITY),
            max: Vec2::splat(f64::NEG_INFINITY),
        }
    }
}

impl Rect {
    /// A rectangle that contains the two given points.
    #[inline]
    pub fn from_points(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.cmin(b),
            max: a.cmax(b),
        }
    }
    /// Dimensions (`max - min`).
    #[inline]
    pub fn size(self) -> Vec2 {
        self.max - self.min
    }
    /// Area (`size.x · size.y`).
    #[inline]
    pub fn area(self) -> f64 {
        let sz = self.size();
        sz.x * sz.y
    }
    /// The absolute-largest coordinate of any contained point (`Rect::Scale`) — the 2D length scale.
    #[inline]
    pub fn scale(self) -> f64 {
        let abs_max = self.min.cabs().cmax(self.max.cabs());
        abs_max.x.max(abs_max.y)
    }
    /// Center (`0.5 * (max + min)`).
    #[inline]
    pub fn center(self) -> Vec2 {
        0.5 * (self.max + self.min)
    }
    /// Does this rectangle contain (border inclusive) the given point?
    #[inline]
    pub fn contains_point(self, p: Vec2) -> bool {
        p.x >= self.min.x && p.y >= self.min.y && self.max.x >= p.x && self.max.y >= p.y
    }
    /// Does this rectangle contain (equality inclusive) the given rectangle?
    #[inline]
    pub fn contains_rect(self, r: Rect) -> bool {
        r.min.x >= self.min.x
            && r.min.y >= self.min.y
            && self.max.x >= r.max.x
            && self.max.y >= r.max.y
    }
    /// Does this rectangle overlap the one given (equality inclusive)?
    #[inline]
    pub fn does_overlap(self, r: Rect) -> bool {
        self.min.x <= r.max.x
            && self.min.y <= r.max.y
            && self.max.x >= r.min.x
            && self.max.y >= r.min.y
    }
    /// Empty (containing no space)? `max.y <= min.y || max.x <= min.x`, so a degenerate line is empty.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.max.y <= self.min.y || self.max.x <= self.min.x
    }
    /// Finite bounds?
    #[inline]
    pub fn is_finite(self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }
    /// Expand in place to include a point.
    #[inline]
    pub fn union_point(&mut self, p: Vec2) {
        self.min = self.min.cmin(p);
        self.max = self.max.cmax(p);
    }
    /// Union with another rectangle.
    #[inline]
    pub fn union(self, o: Self) -> Self {
        Self {
            min: self.min.cmin(o.min),
            max: self.max.cmax(o.max),
        }
    }
}

// ---------------------------------------------------------------------------
// 2D affine transform. Manifold's `mat2x3` = `mat<double,2,3>` — COLUMN-MAJOR, 3 columns of Vec2
// (columns x,y are the linear part, w is the translation) — the CrossSection transform vocabulary.
// ---------------------------------------------------------------------------

/// 2D affine transform `mat<double,2,3>`: 3 Vec2 columns, column-major (matches Manifold `mat2x3`).
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Mat2x3 {
    /// First basis column.
    pub x: Vec2,
    /// Second basis column.
    pub y: Vec2,
    /// Translation column.
    pub w: Vec2,
}

impl Mat2x3 {
    /// The identity affine (basis = I, translation = 0).
    pub const IDENTITY: Self = Self {
        x: Vec2::new(1.0, 0.0),
        y: Vec2::new(0.0, 1.0),
        w: Vec2::ZERO,
    };

    /// Transform a point (implicitly homogeneous, `w = 1`): `x·p.x + y·p.y + w`, left-to-right.
    #[inline]
    pub fn transform_point(self, p: Vec2) -> Vec2 {
        self.x * p.x + self.y * p.y + self.w
    }

    /// Determinant of the linear (2×2) part — negative means the transform mirrors (flips winding).
    #[inline]
    pub fn det_linear(self) -> f64 {
        self.x.x * self.y.y - self.y.x * self.x.y
    }

    /// Affine composition `self ∘ rhs` (apply `rhs` first): the C++ `m * Mat3(transform_)` chain.
    #[inline]
    pub fn compose(self, rhs: Mat2x3) -> Mat2x3 {
        Mat2x3 {
            x: self.x * rhs.x.x + self.y * rhs.x.y,
            y: self.x * rhs.y.x + self.y * rhs.y.y,
            w: self.x * rhs.w.x + self.y * rhs.w.y + self.w,
        }
    }

    /// Pure translation (`CrossSection::Translate`'s matrix).
    #[inline]
    pub fn translate(v: Vec2) -> Mat2x3 {
        Mat2x3 {
            w: v,
            ..Mat2x3::IDENTITY
        }
    }

    /// Z-axis rotation by `degrees` (`CrossSection::Rotate`'s matrix) — `sind`/`cosd`, so quadrant
    /// angles are exact.
    #[inline]
    pub fn rotate_degrees(degrees: f64) -> Mat2x3 {
        let s = crate::mathf::sind(degrees);
        let c = crate::mathf::cosd(degrees);
        Mat2x3 {
            x: Vec2::new(c, s),
            y: Vec2::new(-s, c),
            w: Vec2::ZERO,
        }
    }

    /// Per-axis scale (`CrossSection::Scale`'s matrix).
    #[inline]
    pub fn scale(v: Vec2) -> Mat2x3 {
        Mat2x3 {
            x: Vec2::new(v.x, 0.0),
            y: Vec2::new(0.0, v.y),
            w: Vec2::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_and_dot_match_definitions() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        // cross: {2·6−3·5, 3·4−1·6, 1·5−2·4} = {-3, 6, -3}
        assert_eq!(a.cross(b), Vec3::new(-3.0, 6.0, -3.0));
        // anti-commutative
        assert_eq!(b.cross(a), -a.cross(b));
        // dot: 4+10+18 = 32
        assert_eq!(a.dot(b), 32.0);
        // cross ⟂ both operands
        assert_eq!(a.cross(b).dot(a), 0.0);
        assert_eq!(a.cross(b).dot(b), 0.0);
    }

    #[test]
    fn length_and_normalize() {
        let v = Vec3::new(3.0, 4.0, 0.0);
        assert_eq!(v.length2(), 25.0);
        assert_eq!(v.length(), 5.0);
        let n = v.normalize();
        assert_eq!(n, Vec3::new(0.6, 0.8, 0.0));
        assert_eq!(n.length(), 1.0);
    }

    #[test]
    fn signed_tet_volume_via_dot_cross() {
        // The M.0.6 volume gate's per-triangle term: dot(v0, cross(v1, v2)) / 6 for the unit
        // corner tetrahedron = 1/6.
        let (v0, v1, v2) = (
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        );
        assert_eq!(v0.dot(v1.cross(v2)) / 6.0, 1.0 / 6.0);
    }

    #[test]
    fn mat3_determinant_normal_transform_spectral_norm() {
        // A non-uniform scale diag(2,3,4).
        let s = Mat3 {
            x: Vec3::new(2.0, 0.0, 0.0),
            y: Vec3::new(0.0, 3.0, 0.0),
            z: Vec3::new(0.0, 0.0, 4.0),
        };
        assert_eq!(s.determinant(), 24.0);
        // Normal transform = inverse-transpose = diag(1/2, 1/3, 1/4).
        let nt = s.normal_transform();
        assert!((nt.mul_vec(Vec3::new(1.0, 0.0, 0.0)) - Vec3::new(0.5, 0.0, 0.0)).length() < 1e-12);
        assert!(
            (nt.mul_vec(Vec3::new(0.0, 0.0, 1.0)) - Vec3::new(0.0, 0.0, 0.25)).length() < 1e-12
        );
        // Spectral norm (largest singular value) of a diagonal = the largest |entry| = 4.
        assert!(
            (s.spectral_norm() - 4.0).abs() < 1e-9,
            "spectral_norm {} != 4",
            s.spectral_norm()
        );

        // A pure rotation (orthogonal) has spectral norm 1 and is its own inverse-transpose.
        let rot = Mat3 {
            x: Vec3::new(0.0, 1.0, 0.0),
            y: Vec3::new(-1.0, 0.0, 0.0),
            z: Vec3::new(0.0, 0.0, 1.0),
        };
        assert_eq!(rot.determinant(), 1.0);
        assert!((rot.spectral_norm() - 1.0).abs() < 1e-9);
        // NormalTransform(rot) == rot for an orthogonal matrix.
        for v in [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.3, -0.7, 0.5)] {
            assert!((rot.normal_transform().mul_vec(v) - rot.mul_vec(v)).length() < 1e-12);
        }
    }

    #[test]
    fn affine_transform_point() {
        // scale by (2,3,4) then translate by (10,20,30).
        let m = Mat3x4 {
            x: Vec3::new(2.0, 0.0, 0.0),
            y: Vec3::new(0.0, 3.0, 0.0),
            z: Vec3::new(0.0, 0.0, 4.0),
            w: Vec3::new(10.0, 20.0, 30.0),
        };
        assert_eq!(
            m.transform_point(Vec3::new(1.0, 1.0, 1.0)),
            Vec3::new(12.0, 23.0, 34.0)
        );
        // direction ignores translation.
        assert_eq!(
            m.transform_dir(Vec3::new(1.0, 1.0, 1.0)),
            Vec3::new(2.0, 3.0, 4.0)
        );
        // identity is a no-op.
        assert_eq!(
            Mat3x4::IDENTITY.transform_point(Vec3::new(7.0, 8.0, 9.0)),
            Vec3::new(7.0, 8.0, 9.0)
        );
    }

    /// M.3.5 — the transform BUILDERS (translate/scale/rotate) + matrix composition, the scaffolding
    /// `Halfspace` folds into one matrix.
    #[test]
    fn transform_builders_and_composition() {
        let v3 = Vec3::new;
        let p = v3(1.0, 0.0, 0.0);

        // Builders in isolation.
        assert_eq!(
            Mat3x4::translate(v3(10., 20., 30.)).transform_point(p),
            v3(11., 20., 30.)
        );
        assert_eq!(
            Mat3x4::scale(v3(2., 3., 4.)).transform_point(p),
            v3(2., 0., 0.)
        );

        // 90° about z maps +x → +y (column-major rZ). Uses mathf sind/cosd, so exact at 90°.
        let rz90 = Mat3x4::rotate(0., 0., 90.);
        let r = rz90.transform_point(p);
        assert!((r - v3(0., 1., 0.)).length() < 1e-15, "rot90z(+x) = {r:?}");

        // Composition `a * b` applies b FIRST: scale then translate == the hand-built affine.
        let composed = Mat3x4::translate(v3(10., 20., 30.)) * Mat3x4::scale(v3(2., 3., 4.));
        assert_eq!(composed.transform_point(v3(1., 1., 1.)), v3(12., 23., 34.));
        // Identity is the composition unit both ways.
        assert_eq!(
            (Mat3x4::IDENTITY * composed).transform_point(p),
            composed.transform_point(p)
        );
        assert_eq!(
            (composed * Mat3x4::IDENTITY).transform_point(p),
            composed.transform_point(p)
        );

        // Mat3 matrix product agrees with applying the two rotations in sequence.
        let a = Mat3x4::rotate(0., 0., 90.).linear();
        let b = Mat3x4::rotate(90., 0., 0.).linear();
        assert!((a.mul_mat3(b).mul_vec(p) - a.mul_vec(b.mul_vec(p))).length() < 1e-15);
    }

    #[test]
    fn box_union_and_transform() {
        let mut b = Box3::default();
        assert!(!b.is_finite()); // empty box is inverted-infinity
        b.union_point(Vec3::new(1.0, 2.0, 3.0));
        b.union_point(Vec3::new(-1.0, 5.0, 0.0));
        assert_eq!(b.min, Vec3::new(-1.0, 2.0, 0.0));
        assert_eq!(b.max, Vec3::new(1.0, 5.0, 3.0));
        assert_eq!(b.size(), Vec3::new(2.0, 3.0, 3.0));
        // translate: bounds shift, size preserved.
        let t = Mat3x4 {
            w: Vec3::new(10.0, 0.0, 0.0),
            ..Mat3x4::IDENTITY
        };
        let bt = b.transform(t);
        assert_eq!(bt.min, Vec3::new(9.0, 2.0, 0.0));
        assert_eq!(bt.size(), b.size());
    }

    #[test]
    fn round_trips_through_arrays() {
        let v = Vec3::new(1.5, -2.5, 3.5);
        let a: [f64; 3] = v.into();
        assert_eq!(a, [1.5, -2.5, 3.5]);
        assert_eq!(Vec3::from(a), v);
    }

    #[test]
    fn covers_all_vec_and_box_ops() {
        // Vec2 / Vec4 dot (Vec3 dot is covered above).
        assert_eq!(Vec2::new(1.0, 2.0).dot(Vec2::new(3.0, 4.0)), 11.0);
        assert_eq!(Vec4::new(1.0, 2.0, 3.0, 4.0).dot(Vec4::splat(1.0)), 10.0);

        // Componentwise vec*vec, vec/vec, scalar*vec, vec/scalar, neg, cabs, is_finite.
        let a = Vec3::new(2.0, -3.0, 4.0);
        let b = Vec3::new(5.0, 2.0, -1.0);
        assert_eq!(a * b, Vec3::new(10.0, -6.0, -4.0));
        assert_eq!(
            Vec3::new(6.0, 8.0, 10.0) / Vec3::new(2.0, 4.0, 5.0),
            Vec3::new(3.0, 2.0, 2.0)
        );
        assert_eq!(3.0 * a, Vec3::new(6.0, -9.0, 12.0)); // f64 * Vec
        assert_eq!(a / 2.0, Vec3::new(1.0, -1.5, 2.0));
        assert_eq!(-a, Vec3::new(-2.0, 3.0, -4.0));
        assert_eq!(a.cabs(), Vec3::new(2.0, 3.0, 4.0));
        assert!(a.is_finite());
        assert!(!Vec3::new(f64::NAN, 0.0, 0.0).is_finite());
        assert!(!Vec3::new(0.0, f64::INFINITY, 0.0).is_finite());

        // Assign ops.
        let mut c = a;
        c += b;
        assert_eq!(c, Vec3::new(7.0, -1.0, 3.0));
        c -= b;
        assert_eq!(c, a);
        c *= Vec3::splat(2.0);
        assert_eq!(c, Vec3::new(4.0, -6.0, 8.0));

        // Vec4 ⟷ Vec3 bridges + array round-trip for the other widths.
        let v4 = Vec4::from_vec3(Vec3::new(1.0, 2.0, 3.0), 4.0);
        assert_eq!(v4, Vec4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(v4.xyz(), Vec3::new(1.0, 2.0, 3.0));
        let p = Vec2::new(1.5, -2.5);
        assert_eq!(p + Vec2::new(0.5, 0.5), Vec2::new(2.0, -2.0));
        let arr2: [f64; 2] = p.into();
        assert_eq!(arr2, [1.5, -2.5]);
        assert_eq!(Vec2::from(arr2), p);
        let arr4: [f64; 4] = v4.into();
        assert_eq!(Vec4::from(arr4), v4);

        // Box3: from_points, center, union, overlaps (both verdicts), is_finite.
        let bx = Box3::from_points(Vec3::new(3.0, 0.0, 0.0), Vec3::new(-1.0, 5.0, 2.0));
        assert_eq!(bx.min, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(bx.max, Vec3::new(3.0, 5.0, 2.0));
        assert_eq!(bx.center(), Vec3::new(1.0, 2.5, 1.0));
        assert!(bx.is_finite());
        assert!(!Box3::default().is_finite()); // inverted-infinity empty box
        let near = Box3::from_points(Vec3::ZERO, Vec3::splat(1.0));
        let far = Box3::from_points(Vec3::splat(100.0), Vec3::splat(101.0));
        assert!(bx.overlaps(near));
        assert!(!bx.overlaps(far));
        let u = bx.union(far);
        assert_eq!(u.min, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(u.max, Vec3::splat(101.0));
    }
}
