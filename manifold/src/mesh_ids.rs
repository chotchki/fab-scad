//! Typed mesh indices — the misuse-resistance layer over the kernel's three interchangeable `i32`
//! index spaces (chotchki: "make it hard to use the APIs wrong").
//!
//! Manifold's C++ uses raw `int` for every index, so a vertex, a half-edge, and a triangle are all the
//! same type and NOTHING stops you passing one where another is expected — the exact bug class a
//! geometry kernel must not have. These newtypes make the three spaces distinct at compile time:
//! - [`VertId`] indexes `vert_pos` (and the per-vertex winding/inclusion/remap arrays).
//! - [`HalfedgeId`] indexes `halfedge`. Three consecutive half-edges form a triangle (`3·tri + i`).
//! - [`TriId`] indexes `face_normal` / the triangle number (`= halfedge / 3`).
//!
//! The index ARITHMETIC that mixes spaces in the C++ (`edge / 3` = halfedge→tri, `3·tri + i` =
//! tri→halfedge) becomes EXPLICIT NAMED conversions here ([`HalfedgeId::tri`], [`TriId::halfedge`]), so a
//! `/3` can't silently land in the wrong space. All three are `#[repr(transparent)]` over `i32` and
//! derive `Ord` — zero runtime cost, and bit-identical output (this is a pure type-level change).
//!
//! `-1` is the removed/unpaired sentinel (Manifold's `int` convention), exposed as `NONE` + `is_none`.
//! Scope is deliberately the API SURFACE, not every internal: single id per axis (no phantom P/Q/R
//! spaces — they'd bloat signatures for marginal safety), and verbatim reps that rely on the C++
//! forward/reverse `[index]` symmetry (e.g. `Intersections::p1q2`) stay raw `i32`.

/// Index of a vertex (into `vert_pos` and the per-vertex arrays). `-1` = removed.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VertId(i32);

/// Index of a half-edge (into `halfedge`). `-1` = unpaired/removed.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HalfedgeId(i32);

/// Index of a triangle/face (into `face_normal`; `= halfedge / 3`).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TriId(i32);

macro_rules! common_id_impls {
    ($T:ty) => {
        impl $T {
            /// The removed/unpaired sentinel (`-1`, Manifold's `int` convention).
            pub const NONE: Self = Self(-1);

            /// Wrap a raw `i32` index.
            #[inline]
            pub const fn new(i: i32) -> Self {
                Self(i)
            }

            /// Wrap a `usize` index (the common case indexing into a `Vec`'s `.len()`-bounded range).
            #[inline]
            pub fn from_usize(i: usize) -> Self {
                Self(i as i32)
            }

            /// The raw `i32` — for arithmetic, comparison, and sentinel checks that must match the C++.
            #[inline]
            pub const fn raw(self) -> i32 {
                self.0
            }

            /// As a `usize`, for indexing a slice/`Vec`.
            #[inline]
            pub const fn u(self) -> usize {
                self.0 as usize
            }

            /// The sentinel test (`< 0`) — removed vertex, unpaired half-edge (Manifold checks `< 0`).
            #[inline]
            pub const fn is_none(self) -> bool {
                self.0 < 0
            }

            /// Not the sentinel.
            #[inline]
            pub const fn is_some(self) -> bool {
                self.0 >= 0
            }
        }
    };
}

common_id_impls!(VertId);
common_id_impls!(HalfedgeId);
common_id_impls!(TriId);

impl VertId {
    /// The `n`-th duplicate of this vertex (`self + n`) — the boolean duplicates a vert `|inclusion|`
    /// times into consecutive output slots.
    #[inline]
    pub const fn offset(self, n: i32) -> VertId {
        VertId(self.0 + n)
    }

    /// Advance to the next consecutive vertex (`++vert` in the duplication loops).
    #[inline]
    pub fn advance(&mut self) {
        self.0 += 1;
    }
}

impl HalfedgeId {
    /// The triangle this half-edge belongs to (`self / 3`).
    #[inline]
    pub const fn tri(self) -> TriId {
        TriId(self.0 / 3)
    }

    /// Which corner of its triangle this is (`self % 3`, in `0..3`).
    #[inline]
    pub const fn corner(self) -> i32 {
        self.0 % 3
    }

    /// The next half-edge within the triangle (`3·tri + (i+1)%3`) — Manifold's `NextHalfedge`.
    #[inline]
    pub const fn next(self) -> HalfedgeId {
        HalfedgeId(self.0 + if self.0 % 3 == 2 { -2 } else { 1 })
    }

    /// The previous half-edge within the triangle — Manifold's `PrevHalfedge`.
    #[inline]
    pub const fn prev(self) -> HalfedgeId {
        HalfedgeId(self.0 + if self.0 % 3 == 0 { 2 } else { -1 })
    }

    /// This half-edge plus a positive output offset (used when laying out a face's output half-edges).
    #[inline]
    pub const fn offset(self, n: i32) -> HalfedgeId {
        HalfedgeId(self.0 + n)
    }
}

impl TriId {
    /// The `i`-th half-edge of this triangle (`3·self + i`, `i` in `0..3`).
    #[inline]
    pub fn halfedge(self, i: usize) -> HalfedgeId {
        HalfedgeId(3 * self.0 + i as i32)
    }

    /// This triangle's first half-edge (`3·self`).
    #[inline]
    pub const fn first_halfedge(self) -> HalfedgeId {
        HalfedgeId(3 * self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halfedge_tri_roundtrip() {
        // Half-edges 3,4,5 all belong to triangle 1; corner is the offset within.
        for i in 0..3 {
            let he = TriId::new(1).halfedge(i);
            assert_eq!(he, HalfedgeId::new(3 + i as i32));
            assert_eq!(he.tri(), TriId::new(1));
            assert_eq!(he.corner(), i as i32);
        }
        assert_eq!(TriId::new(1).first_halfedge(), HalfedgeId::new(3));
    }

    #[test]
    fn next_prev_cycle_within_triangle() {
        // Triangle 0: 0→1→2→0.
        assert_eq!(HalfedgeId::new(0).next(), HalfedgeId::new(1));
        assert_eq!(HalfedgeId::new(2).next(), HalfedgeId::new(0));
        assert_eq!(HalfedgeId::new(0).prev(), HalfedgeId::new(2));
        // Triangle 1 stays within its triple.
        assert_eq!(HalfedgeId::new(5).next(), HalfedgeId::new(3));
        assert_eq!(HalfedgeId::new(3).prev(), HalfedgeId::new(5));
    }

    #[test]
    fn sentinel_and_conversions() {
        assert!(VertId::NONE.is_none());
        assert!(HalfedgeId::NONE.is_none());
        assert!(VertId::new(0).is_some());
        assert_eq!(VertId::new(7).u(), 7);
        assert_eq!(VertId::new(7).raw(), 7);
        assert_eq!(VertId::from_usize(9), VertId::new(9));
        // Ordering is by the raw index (used as BTreeMap keys + start<end tests).
        assert!(VertId::new(2) < VertId::new(5));
        // Duplication offset + advance.
        assert_eq!(VertId::new(4).offset(3), VertId::new(7));
        let mut v = VertId::new(4);
        v.advance();
        assert_eq!(v, VertId::new(5));
    }
}
