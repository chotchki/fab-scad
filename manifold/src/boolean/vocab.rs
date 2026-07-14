//! The boolean's shared vocabulary — the value-style records `shared.h` / `boolean3.h` pass around
//! while assembling a result, distinct from the mesh spine's stored SoA half-edge.
//!
//! Naming note: [`Halfedge`] here is Manifold's `struct Halfedge` (the VALUE form, storing `end_vert`
//! explicitly), used when building output half-edges before they're committed to the spine. The spine's
//! [`crate::mesh::Halfedge`] is the stored element that DERIVES `end` from the next half-edge — same
//! concept, different representation. The C++ carries both under one name; we keep them in separate
//! modules so which one a signature means is unambiguous.
//!
//! Indices are typed ([`crate::mesh_ids`]). The exception is [`Intersections::p1q2`], which stays a raw
//! `[i32; 2]`: it packs an edge and a face in one array and the cascade selects between them with a
//! runtime `[index]`/`[1-index]` (the C++ forward/reverse symmetry) — a typed struct would break that,
//! so it's typed at the point of USE instead. [`TriRef`] also stays raw `i32`: its fields are a mesh
//! INSTANCE id and user/coplanar tags, a different namespace from the mesh index spaces.

use core::cmp::Ordering;

use crate::linalg::Vec3;
use crate::mesh::Mesh;
use crate::mesh_ids::{HalfedgeId, VertId};

/// A value-style half-edge (`shared.h` `struct Halfedge`) — `end_vert` stored, not derived. Emitted by
/// the boolean assembly before the pieces become a manifold spine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Halfedge {
    /// Start vertex.
    pub start_vert: VertId,
    /// End vertex (stored, unlike the spine's derived end).
    pub end_vert: VertId,
    /// The opposite half-edge, or [`HalfedgeId::NONE`].
    pub paired_halfedge: HalfedgeId,
    /// The property vertex.
    pub prop_vert: VertId,
}

impl Halfedge {
    /// Is this the forward (`start < end`) half of its undirected edge?
    #[inline]
    pub fn is_forward(self) -> bool {
        self.start_vert < self.end_vert
    }

    /// The `operator<` strict-weak-ordering Manifold sorts half-edges by: `(start_vert, end_vert)`
    /// lexicographic, IGNORING pair/prop. Exposed as a comparator rather than an `Ord` impl on purpose
    /// — two half-edges can share `(start, end)` yet differ in pair/prop, so an `Ord` that ignored
    /// those would contradict the `Eq` derive (which compares all four fields). Use with `sort_by`.
    #[inline]
    pub fn order(a: &Self, b: &Self) -> Ordering {
        (a.start_vert, a.end_vert).cmp(&(b.start_vert, b.end_vert))
    }
}

/// Provenance of an output triangle (`shared.h` `TriRef`) — which input mesh/face it came from, threaded
/// through the boolean so properties (UVs, colours) and coplanar-merge decisions can be reapplied. Its
/// fields are a different id namespace from the mesh index spaces (a mesh INSTANCE id, user/coplanar
/// tags), so they stay raw `i32`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TriRef {
    /// The mesh-instance ID this triangle belongs to.
    pub mesh_id: i32,
    /// The `OriginalID` of the source mesh (for reapplying properties).
    pub original_id: i32,
    /// A user-set face ID passed through unchanged; `-1` if unset. Divides faces the cleanup must not
    /// collapse across.
    pub face_id: i32,
    /// Coplanar-group ID — triangles sharing it are coplanar. A canonical tri index at first; after a
    /// boolean it may name a triangle no longer present.
    pub coplanar_id: i32,
}

impl TriRef {
    /// Do two refs describe the same original face? (`meshID && coplanarID && faceID` all equal —
    /// `originalID` is deliberately NOT compared, matching `TriRef::SameFace`.)
    #[inline]
    pub fn same_face(self, other: TriRef) -> bool {
        self.mesh_id == other.mesh_id
            && self.coplanar_id == other.coplanar_id
            && self.face_id == other.face_id
    }
}

/// A temporary FORWARD-only edge referencing the half-edge it came from (`shared.h` `TmpEdge`). `first
/// <= second` always (the endpoints are sorted at construction), so an undirected edge has one
/// canonical `TmpEdge`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TmpEdge {
    /// Lower-indexed endpoint.
    pub first: VertId,
    /// Higher-indexed endpoint.
    pub second: VertId,
    /// The half-edge this edge was created from ([`HalfedgeId::NONE`] marks the reverse half, dropped by
    /// [`create_tmp_edges`]).
    pub halfedge_idx: HalfedgeId,
}

impl TmpEdge {
    /// Build from a directed `(start, end)` half-edge, sorting the endpoints so `first <= second`.
    #[inline]
    pub fn new(start: VertId, end: VertId, idx: HalfedgeId) -> Self {
        Self {
            first: start.min(end),
            second: start.max(end),
            halfedge_idx: idx,
        }
    }

    /// `operator<`: `(first, second)` lexicographic. A comparator, not `Ord`, for the same reason as
    /// [`Halfedge::order`] — `halfedge_idx` is part of `Eq` but not the order.
    #[inline]
    pub fn order(a: &Self, b: &Self) -> Ordering {
        (a.first, a.second).cmp(&(b.first, b.second))
    }
}

/// One forward `TmpEdge` per undirected edge of `mesh` (`shared.h` `CreateTmpEdges`): build a temp edge
/// for every half-edge, tagging the reverse halves `NONE`, then drop them — leaving exactly `numEdge =
/// halfedge/2` forward edges. Panics (debug) if the mesh isn't oriented (the count wouldn't halve).
pub fn create_tmp_edges(mesh: &Mesh) -> Vec<TmpEdge> {
    let mut edges: Vec<TmpEdge> = mesh
        .halfedge_ids()
        .map(|idx| {
            let is_forward = mesh.start(idx) < mesh.end(idx);
            TmpEdge::new(
                mesh.start(idx),
                mesh.end(idx),
                if is_forward { idx } else { HalfedgeId::NONE },
            )
        })
        .collect();
    edges.retain(|e| e.halfedge_idx.is_some());
    debug_assert_eq!(
        edges.len(),
        mesh.halfedge.len() / 2,
        "CreateTmpEdges: mesh not oriented!"
    );
    edges
}

/// The intersection records of one direction of the boolean (`boolean3.h` `Intersections`). In forward
/// mode: intersections of edges of P with faces of Q; the three arrays are parallel (one entry per
/// intersection). Reverse mode swaps the roles (`p1q2 → p2q1`, etc.).
///
/// `p1q2` stays a raw `[i32; 2]` = `[edge, face]`: the cascade picks edge-or-face with a runtime
/// `[index]` that flips between the forward and reverse passes, so it's typed at the point of use.
#[derive(Clone, Debug, Default)]
pub struct Intersections {
    /// Each `[edgeP, faceQ]` (forward) sparse index pair — an edge id and a face id, raw (see above).
    pub p1q2: Vec<[i32; 2]>,
    /// The winding-number-type `X` value for each pair.
    pub x12: Vec<i32>,
    /// The intersection point for each pair.
    pub v12: Vec<Vec3>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Mesh;
    use crate::mesh_ids::TriId;

    #[test]
    fn halfedge_is_forward_and_order() {
        let fwd = Halfedge {
            start_vert: VertId::new(1),
            end_vert: VertId::new(4),
            paired_halfedge: HalfedgeId::new(7),
            prop_vert: VertId::new(1),
        };
        let rev = Halfedge {
            start_vert: VertId::new(4),
            end_vert: VertId::new(1),
            paired_halfedge: HalfedgeId::NONE,
            prop_vert: VertId::new(4),
        };
        assert!(fwd.is_forward());
        assert!(!rev.is_forward());
        assert_eq!(Halfedge::order(&fwd, &rev), Ordering::Less); // (1,4) < (4,1)
        // Order ignores pair/prop: same (start,end), different pair ⇒ Equal by order, != by Eq.
        let same_verts = Halfedge {
            paired_halfedge: HalfedgeId::new(99),
            ..fwd
        };
        assert_eq!(Halfedge::order(&fwd, &same_verts), Ordering::Equal);
        assert_ne!(fwd, same_verts);
    }

    #[test]
    fn tri_ref_same_face_ignores_original_id() {
        let a = TriRef {
            mesh_id: 2,
            original_id: 10,
            face_id: 5,
            coplanar_id: 3,
        };
        // Differs only in original_id ⇒ still the same face.
        let b = TriRef {
            original_id: 999,
            ..a
        };
        assert!(a.same_face(b));
        // A different coplanar_id ⇒ different face.
        let c = TriRef {
            coplanar_id: 4,
            ..a
        };
        assert!(!a.same_face(c));
    }

    #[test]
    fn tmp_edge_sorts_endpoints_and_orders() {
        let e = TmpEdge::new(VertId::new(5), VertId::new(2), HalfedgeId::new(11));
        assert_eq!(
            (e.first, e.second, e.halfedge_idx),
            (VertId::new(2), VertId::new(5), HalfedgeId::new(11))
        );
        let f = TmpEdge::new(VertId::new(2), VertId::new(5), HalfedgeId::new(11)); // same undirected edge
        assert_eq!(e, f);
        let g = TmpEdge::new(VertId::new(3), VertId::new(9), HalfedgeId::new(4));
        assert_eq!(TmpEdge::order(&e, &g), Ordering::Less); // (2,5) < (3,9)
    }

    #[test]
    fn create_tmp_edges_halves_a_manifold() {
        // A tetrahedron: 4 tris, 12 half-edges, 6 undirected edges.
        let mut mesh = Mesh {
            vert_pos: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
            ],
            ..Default::default()
        };
        let tris = [[0u32, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
        mesh.create_halfedges(&tris);
        assert!(mesh.is_manifold());
        let edges = create_tmp_edges(&mesh);
        assert_eq!(edges.len(), 6);
        // Every temp edge is forward-tagged (a real half-edge index) and canonical (first <= second).
        for e in &edges {
            assert!(e.halfedge_idx.is_some());
            assert!(e.first <= e.second);
        }
    }

    #[test]
    fn intersections_default_is_empty() {
        let i = Intersections::default();
        assert!(i.p1q2.is_empty() && i.x12.is_empty() && i.v12.is_empty());
        // TriId is part of the id vocabulary the assembly uses to key faces.
        assert_eq!(TriId::new(2).halfedge(0), HalfedgeId::new(6));
    }
}
