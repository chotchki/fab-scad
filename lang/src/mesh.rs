//! A backend-agnostic triangle mesh — the differential harness's comparison unit.
//!
//! Deliberately decoupled from `kernel::Solid`: the harness (G.3.7) compares scad-rs output
//! against the OpenSCAD oracle as raw geometry, and a plain vertex/index pair is the honest
//! common denominator. Mirrors the shape of `kernel::Solid::to_indexed` so lowering (G.3.5) is a
//! straight hand-off.

use crate::geom::{Tri, Vec3};

/// An indexed triangle mesh: a vertex table plus triangles indexing into it.
///
/// Winding and vertex order are whatever the producing stage emits — canonicalization for
/// comparison is the harness's job (G.3.7), not this type's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    /// Vertex positions.
    pub verts: Vec<Vec3>,
    /// Triangles indexing into [`Mesh::verts`].
    pub tris: Vec<Tri>,
}

impl Mesh {
    /// An empty mesh — no vertices, no triangles.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of triangles.
    #[must_use]
    pub fn tri_count(&self) -> usize {
        self.tris.len()
    }

    /// The number of vertices.
    #[must_use]
    pub fn vert_count(&self) -> usize {
        self.verts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{Mesh, Tri, Vec3};

    #[test]
    fn empty_mesh() {
        let m = Mesh::new();
        assert_eq!(m.vert_count(), 0);
        assert_eq!(m.tri_count(), 0);
        assert_eq!(m, Mesh::default());
    }

    #[test]
    fn populated_mesh_counts_and_derives() {
        let m = Mesh {
            verts: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            tris: vec![Tri::new(0, 1, 2)],
        };
        assert_eq!(m.vert_count(), 3);
        assert_eq!(m.tri_count(), 1);
        assert_eq!(m.clone(), m); // Clone + PartialEq
        assert!(format!("{m:?}").contains("Mesh")); // Debug
    }
}
