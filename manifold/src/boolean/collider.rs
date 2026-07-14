//! The broad phase — SERIAL brute-force stand-in for Manifold's LBVH `Collider`.
//!
//! Manifold's `collider.h` is a Karras radix-tree BVH: Morton-sort the leaf boxes, build the tree,
//! and traverse it per query. We DEFER all of that. A brute-force `O(n · leaves)` scan emits the exact
//! same candidate SET — every `(query, leaf)` whose boxes overlap — and for the offset tracer that's
//! all GATE-A needs:
//! - The `Kernel12` consumer `stable_sort`s the recorded pairs by `(edge, face)` afterward, so the
//!   ORDER we emit them in is normalized away (no `(edge, face)` pair repeats → the sort is total).
//! - The `Winding03` consumer accumulates into an INTEGER winding array, so its sum is
//!   order-independent too.
//!
//! So a natural-face-order brute force is bit-equivalent to the BVH here. The Morton BVH (and the
//! `SortGeometry` reindex it needs) is ported later, behind a flag, differential-tested to emit the
//! same set — see [[SPEC_manifold-rs_R1]] and M.1.1's note.
//!
//! Two query modes, matching `Box::DoesOverlap`'s two overloads: a `Box3` query (edge box vs face box)
//! and a `Vec3` point query (XY-projected — the z-raycast winding). An empty (inverted-infinity) box
//! query — what a REVERSE half-edge produces — is skipped wholesale, verbatim to the C++ early-out.
//!
//! The collider itself is DELIBERATELY index-agnostic: [`Collider::collisions`] collides abstract
//! leaf/query indices (`i32`), and the CALLER assigns their meaning (a query is an edge here, a vert
//! there) by wrapping into the typed ids at the callback boundary. Typing the broad phase would be
//! false precision — it doesn't know or care what the boxes represent.

use crate::linalg::{Box3, Vec3};
use crate::mesh::Mesh;
use crate::mesh_ids::{HalfedgeId, TriId};

/// A broad-phase query — either a `Box3` (box-vs-box overlap) or a `Vec3` (point projected into the
/// leaf's XY extent). Unifies the two `Box::DoesOverlap` overloads so [`Collider::collisions`] is one
/// function over both.
pub trait ColliderQuery: Copy {
    /// Does this query overlap `leaf`? Box→Box is the symmetric AABB test; point→box is XY-projected.
    fn overlaps(self, leaf: Box3) -> bool;
    /// The empty-box early-out: a reverse half-edge yields an inverted-infinity `Box`, which overlaps
    /// nothing — the C++ skips it before traversal, so we skip it before scanning.
    fn is_empty(self) -> bool;
}

impl ColliderQuery for Box3 {
    #[inline]
    fn overlaps(self, leaf: Box3) -> bool {
        // DoesOverlap(Box) is symmetric, so leaf-vs-query == query-vs-leaf.
        leaf.overlaps(self)
    }
    #[inline]
    fn is_empty(self) -> bool {
        // Matches the C++ `query.min.x == infinity` test exactly (the default `Box()` min).
        self.min.x == f64::INFINITY
    }
}

impl ColliderQuery for Vec3 {
    #[inline]
    fn overlaps(self, leaf: Box3) -> bool {
        leaf.does_overlap_point_xy(self)
    }
    #[inline]
    fn is_empty(self) -> bool {
        false
    }
}

/// The broad-phase acceleration structure (Manifold's `Collider`), here just the leaf boxes. Built over
/// one mesh's per-FACE boxes; queried by the other mesh's edges (or verts) during a boolean.
#[derive(Clone, Debug, Default)]
pub struct Collider {
    /// One AABB per leaf (per triangle of the source mesh). A removed triangle keeps the default empty
    /// box, so it never collides.
    pub leaf_box: Vec<Box3>,
}

impl Collider {
    /// Build from explicit leaf boxes.
    pub fn new(leaf_box: Vec<Box3>) -> Self {
        Self { leaf_box }
    }

    /// Build over a mesh's per-face boxes — the way a boolean builds `inQ.collider_` (`GetFaceBoxMorton`
    /// minus the Morton sort). Each face box is the union of its three vertices; a removed face
    /// (`pair(first_halfedge)` is NONE) keeps the empty default so it collides with nothing.
    pub fn from_mesh(mesh: &Mesh) -> Self {
        let mut leaf_box = vec![Box3::default(); mesh.num_tri()];
        for (face, bx) in leaf_box.iter_mut().enumerate() {
            let t = TriId::from_usize(face);
            if mesh.pair(t.halfedge(0)).is_none() {
                continue;
            }
            for i in 0..3 {
                bx.union_point(mesh.pos(mesh.start(t.halfedge(i))));
            }
        }
        Self { leaf_box }
    }

    /// Serial brute-force broad phase (`Collider::Collisions`). For each query `i` in `0..n`, take
    /// `query_fn(i)`; skip an empty box query; otherwise call `record(i, leaf)` for every leaf box it
    /// overlaps. When `self_collision`, the `i == leaf` self-pair is skipped (only meaningful when the
    /// queries ARE the leaves; the boolean always passes `false`). Leaves are scanned in natural order.
    /// Indices are raw `i32` — the caller assigns their meaning.
    pub fn collisions<Q: ColliderQuery>(
        &self,
        n: usize,
        self_collision: bool,
        query_fn: impl Fn(i32) -> Q,
        mut record: impl FnMut(i32, i32),
    ) {
        for i in 0..n as i32 {
            let q = query_fn(i);
            if q.is_empty() {
                continue;
            }
            for (leaf, &b) in self.leaf_box.iter().enumerate() {
                let leaf = leaf as i32;
                if (!self_collision || leaf != i) && q.overlaps(b) {
                    record(i, leaf);
                }
            }
        }
    }
}

/// The `Box3` a forward half-edge queries with (`Box(vertPos[start], vertPos[end])`), or the empty
/// default for a reverse half-edge — the exact `f(i)` lambda `Intersect12_` builds. A reverse edge's
/// empty box is skipped by [`Collider::collisions`], so each undirected edge is queried once.
#[inline]
pub fn edge_query_box(mesh: &Mesh, edge: HalfedgeId) -> Box3 {
    let start = mesh.start(edge);
    let end = mesh.end(edge);
    if start < end {
        Box3::from_points(mesh.pos(start), mesh.pos(end))
    } else {
        Box3::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_ids::VertId;

    /// A unit cube at a given origin, as a fresh manifold `Mesh`.
    fn cube_at(ox: f64, oy: f64, oz: f64) -> Mesh {
        #[rustfmt::skip]
        let verts = [
            (0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (1.0, 1.0, 0.0), (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0), (1.0, 0.0, 1.0), (1.0, 1.0, 1.0), (0.0, 1.0, 1.0),
        ];
        let mut mesh = Mesh {
            vert_pos: verts
                .iter()
                .map(|&(x, y, z)| Vec3::new(x + ox, y + oy, z + oz))
                .collect(),
            ..Default::default()
        };
        #[rustfmt::skip]
        let tris = [
            [0u32, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
            [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
            [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5],
        ];
        mesh.create_halfedges(&tris);
        mesh
    }

    #[test]
    fn face_boxes_union_the_triangle_verts() {
        let mesh = cube_at(0.0, 0.0, 0.0);
        let c = Collider::from_mesh(&mesh);
        assert_eq!(c.leaf_box.len(), 12);
        // Every face box of the unit cube is finite and inside [0,1]³.
        for b in &c.leaf_box {
            assert!(b.is_finite());
            assert!(b.min.x >= 0.0 && b.max.x <= 1.0);
            assert!(b.min.z >= 0.0 && b.max.z <= 1.0);
        }
        // The -Z faces (tris 0,1) are flat at z=0.
        assert_eq!(c.leaf_box[0].min.z, 0.0);
        assert_eq!(c.leaf_box[0].max.z, 0.0);
    }

    #[test]
    fn removed_face_gets_empty_box() {
        // Hand-break a face: mark its first half-edge's pair as NONE so from_mesh skips it.
        let mut mesh = cube_at(0.0, 0.0, 0.0);
        mesh.halfedge[0].paired_halfedge = HalfedgeId::NONE;
        let c = Collider::from_mesh(&mesh);
        assert!(!c.leaf_box[0].is_finite()); // empty (inverted-infinity)
        // An empty leaf box overlaps no query.
        let q = Box3::from_points(Vec3::ZERO, Vec3::splat(1.0));
        assert!(!q.overlaps(c.leaf_box[0]));
    }

    #[test]
    fn edge_query_skips_reverse_and_empties() {
        let mesh = cube_at(0.0, 0.0, 0.0);
        // A forward half-edge (start < end) yields a finite box; a reverse one yields empty.
        let fwd = mesh
            .halfedge_ids()
            .find(|&e| mesh.start(e) < mesh.end(e))
            .unwrap();
        let rev = mesh
            .halfedge_ids()
            .find(|&e| mesh.start(e) > mesh.end(e))
            .unwrap();
        assert!(edge_query_box(&mesh, fwd).is_finite());
        assert!(ColliderQuery::is_empty(edge_query_box(&mesh, rev)));
    }

    #[test]
    fn overlapping_cubes_collide_disjoint_dont() {
        let p = cube_at(0.0, 0.0, 0.0);
        // q offset by (0.5, 0.5, 0.5) overlaps p; the collider is built over q's faces, queried by p's
        // forward edges.
        let q = cube_at(0.5, 0.5, 0.5);
        let collider = Collider::from_mesh(&q);
        let mut pairs = Vec::new();
        collider.collisions(
            p.halfedge.len(),
            false,
            |e| edge_query_box(&p, HalfedgeId::new(e)),
            |edge, face| pairs.push((edge, face)),
        );
        assert!(
            !pairs.is_empty(),
            "overlapping cubes must produce candidate pairs"
        );
        // Every recorded query index is a FORWARD edge of p (reverse edges are empty → skipped).
        for &(edge, face) in &pairs {
            let e = HalfedgeId::new(edge);
            assert!(p.start(e) < p.end(e), "edge {edge} should be forward");
            assert!((0..12).contains(&face));
        }

        // A far-away cube shares no candidate pairs.
        let far = cube_at(100.0, 100.0, 100.0);
        let far_collider = Collider::from_mesh(&far);
        let mut far_pairs = 0;
        far_collider.collisions(
            p.halfedge.len(),
            false,
            |e| edge_query_box(&p, HalfedgeId::new(e)),
            |_, _| far_pairs += 1,
        );
        assert_eq!(far_pairs, 0);
    }

    #[test]
    fn point_query_is_xy_projected() {
        // The collider over q's faces, queried by a single point. The XY-projected test ignores z: a
        // point under the cube's XY footprint hits its face boxes regardless of height.
        let q = cube_at(0.0, 0.0, 0.0);
        let collider = Collider::from_mesh(&q);
        let mut hits_inside = 0;
        collider.collisions(
            1,
            false,
            |_| Vec3::new(0.5, 0.5, 999.0),
            |_, _| hits_inside += 1,
        );
        assert!(
            hits_inside > 0,
            "a point over the XY footprint must hit some face boxes"
        );

        let mut hits_outside = 0;
        collider.collisions(
            1,
            false,
            |_| Vec3::new(5.0, 5.0, 0.5),
            |_, _| hits_outside += 1,
        );
        assert_eq!(
            hits_outside, 0,
            "a point outside the XY footprint hits nothing"
        );
        // (VertId is used by callers to label point-query indices; keep the import exercised.)
        let _ = VertId::new(0);
    }

    #[test]
    fn self_collision_skips_identity_pair() {
        // A trivial collider of two identical boxes; self_collision must drop (i, i).
        let b = Box3::from_points(Vec3::ZERO, Vec3::splat(1.0));
        let collider = Collider::new(vec![b, b]);
        let mut with_self = Vec::new();
        collider.collisions(2, false, |_| b, |i, j| with_self.push((i, j)));
        assert_eq!(with_self, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        let mut no_self = Vec::new();
        collider.collisions(2, true, |_| b, |i, j| no_self.push((i, j)));
        assert_eq!(no_self, vec![(0, 1), (1, 0)]);
    }
}
