//! The in-process geometry kernel (Track C) — a typed Rust wrapper over Manifold (`manifold3d`),
//! the same CSG engine OpenSCAD's Manifold backend uses. This is the seam that lets fab do slicing +
//! connector CSG WITHOUT shelling out per piece: a re-slice is an in-process boolean on a cached mesh
//! (~ms), not a process spawn (~hundreds of ms). OpenSCAD stays the SCAD→mesh front-door (see
//! `docs/manifold-kernel-spike.md` for the go/no-go); this owns everything downstream of the base mesh.
//!
//! [`Solid`] is a newtype around a Manifold handle so the rest of fab talks in one strongly-typed
//! shape instead of raw bindings. Import (11.2), STL/3mf export (11.3), the slicer (11.4), and the
//! connectors (11.6) build on it.

use anyhow::{anyhow, Result};
use manifold3d::Manifold;

/// A closed, manifold 3D solid — the unit every kernel op consumes and produces.
#[derive(Clone)]
pub struct Solid(Manifold);

impl Solid {
    /// Wrap a raw Manifold (import/slicer internals build these). Used by 11.2 import / 11.4 slicer.
    #[allow(dead_code)]
    pub(crate) fn from_manifold(m: Manifold) -> Self {
        Solid(m)
    }

    /// Borrow the underlying handle (for ops the wrapper doesn't surface yet). Used by 11.3 export.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Manifold {
        &self.0
    }

    /// An axis-aligned box. `center` puts the centroid at the origin (else the min corner).
    pub fn cube(x: f64, y: f64, z: f64, center: bool) -> Self {
        Solid(Manifold::cube(x, y, z, center))
    }

    /// A UV sphere of `radius` with `segments` around the equator.
    pub fn sphere(radius: f64, segments: i32) -> Self {
        Solid(Manifold::sphere(radius, segments))
    }

    // --- booleans --------------------------------------------------------------------------------

    pub fn union(&self, other: &Solid) -> Solid {
        Solid(self.0.union(&other.0))
    }
    pub fn difference(&self, other: &Solid) -> Solid {
        Solid(self.0.difference(&other.0))
    }
    pub fn intersection(&self, other: &Solid) -> Solid {
        Solid(self.0.intersection(&other.0))
    }

    /// Union many solids at once (cheaper + more robust than folding `union`). Empty ⇒ empty solid.
    pub fn batch_union(solids: &[Solid]) -> Solid {
        let hs: Vec<Manifold> = solids.iter().map(|s| s.0.clone()).collect();
        Solid(Manifold::batch_union(&hs))
    }

    // --- transforms ------------------------------------------------------------------------------

    pub fn translate(&self, x: f64, y: f64, z: f64) -> Solid {
        Solid(self.0.translate(x, y, z))
    }
    /// Rotate by Euler angles in DEGREES (X then Y then Z).
    pub fn rotate(&self, x_deg: f64, y_deg: f64, z_deg: f64) -> Solid {
        Solid(self.0.rotate(x_deg, y_deg, z_deg))
    }
    /// Apply a 3×4 affine (column-major 12-float, as Manifold expects).
    pub fn transform(&self, m: &[f64; 12]) -> Solid {
        Solid(self.0.transform(m))
    }

    // --- half-space cuts (the slicer primitives, 11.4) -------------------------------------------

    /// Split by the plane `normal·p = offset` into `(positive, negative)` — the positive half is the
    /// `normal·p > offset` side. Both halves are independent solids; this is the slicer primitive
    /// (11.4), preferred over `trim_by_plane` because both sides come back clean.
    pub fn split_by_plane(&self, normal: [f64; 3], offset: f64) -> (Solid, Solid) {
        let (pos, neg) = self.0.split_by_plane(normal, offset);
        (Solid(pos), Solid(neg))
    }
    /// Keep only the `normal·p > offset` half (drops the rest). NOTE upstream #1516: trimmed halves
    /// may not re-union cleanly (coincident faces) — use `split_by_plane` when you need both sides.
    pub fn trim_by_plane(&self, normal: [f64; 3], offset: f64) -> Solid {
        Solid(self.0.trim_by_plane(normal, offset))
    }

    // --- queries ---------------------------------------------------------------------------------

    /// Err if the solid isn't a valid 2-manifold — the gate a slice/connector result must pass.
    pub fn check(&self) -> Result<()> {
        self.0.status().map_err(|e| anyhow!("non-manifold solid: {e:?}"))
    }
    pub fn is_manifold(&self) -> bool {
        self.0.status().is_ok()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn num_tri(&self) -> usize {
        self.0.num_tri()
    }
    pub fn num_vert(&self) -> usize {
        self.0.num_vert()
    }

    /// `(min, max)` corners, or None when empty.
    pub fn bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        self.0.bounding_box().map(|b| (b.min(), b.max()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_union_is_a_valid_solid() {
        let a = Solid::cube(40.0, 40.0, 40.0, true);
        let b = Solid::cube(30.0, 30.0, 30.0, true).translate(15.0, 0.0, 0.0);
        a.check().unwrap();
        let u = a.union(&b);
        u.check().unwrap();
        assert!(u.num_tri() > 0 && !u.is_empty());
        // Union spans from A's low face (-20) to B's high face (+15+15 = +30) on X.
        let (min, max) = u.bbox().unwrap();
        assert!((min[0] - -20.0).abs() < 1e-6, "min x {}", min[0]);
        assert!((max[0] - 30.0).abs() < 1e-6, "max x {}", max[0]);
    }

    #[test]
    fn split_halves_partition_the_solid() {
        let c = Solid::cube(20.0, 20.0, 20.0, true);
        let (pos, neg) = c.split_by_plane([1.0, 0.0, 0.0], 0.0); // (x>0, x<0)
        pos.check().unwrap();
        neg.check().unwrap();
        // Each half is 10mm thick on X; the positive half is [0, 10], the negative [-10, 0].
        assert!((pos.bbox().unwrap().1[0] - 10.0).abs() < 1e-6);
        assert!((pos.bbox().unwrap().0[0] - 0.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().0[0] - -10.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().1[0] - 0.0).abs() < 1e-6);
    }
}
