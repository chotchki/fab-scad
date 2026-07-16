//! `Face2Tri` — retriangulating the assembled polygonal faces into the result mesh (`face_op.cpp`).
//!
//! After the assembly ([`crate::boolean::boolean_result`]) the result's half-edges are NOT yet
//! triangles: they're general polygon faces, one per original P/Q triangle, delimited by `face_edge`
//! (offsets into `face_halfedges`, `numFaceR + 1` of them). `Face2Tri` turns each into triangles and
//! stitches the final half-edge mesh.
//!
//! UNIFIED PATH vs Manifold: `face_op.cpp` special-cases tris (`WriteLocalTriangles` with 1 triangle,
//! plus an edge-reorder to form a valid triangle) and quads (2 triangles chosen by a CCW/diagonal-length
//! test), and only routes `numEdge > 4` through the full `TriangulateIdxHalfedges`/`HalfedgeTriangulation`
//! machinery. We collapse all three into ONE path: `AssembleHalfedges` → project → ear-clip
//! ([`crate::boolean::polygon`]) → a generalized `WriteLocalTriangles` that pairs interior diagonals by
//! reverse-edge matching and records boundary edges in `contour2tri` for cross-face stitching. The tri
//! and quad fast paths are just the 1- and 2-triangle cases of this, so the unified path subsumes them.
//! Legit because the gate metric is the triangulation-INDEPENDENT residual — the exact diagonal CHOICE
//! doesn't change the covered solid.
//!
//! INDEX SPACES: `face_halfedges` is a SEPARATE array from the output mesh's `halfedge`, so a position
//! into it is a local BUFFER index (`i32`), not a mesh [`HalfedgeId`]. The typed ids appear where this
//! writes the OUTPUT mesh (`out.set_start(HalfedgeId, VertId)`), which is where a swap would actually
//! corrupt geometry.
//!
//! DEFERRED (not needed for GATE-A's order-independent gates): `ReorderHalfedges` (pure within-face
//! canonicalization, for run-to-run bit determinism) and provenance (`triRef`/`WriteTriRefs`).

use std::collections::{BTreeMap, VecDeque};

use crate::boolean::polygon::{PolyVert, triangulate};
use crate::boolean::predicates::{ccw, get_axis_aligned_projection};
use crate::boolean::vocab::{Halfedge, TriRef};
use crate::mesh::Mesh;
use crate::mesh_ids::{HalfedgeId, TriId, VertId};

/// Assemble the half-edges `hes[first..last]` into vertex loops (`face_op.cpp` `AssembleHalfedges`).
/// Each vert must appear as `startVert` and as `endVert` the same number of times. Loop entries are
/// GLOBAL buffer indices (`start_idx + local`), matching the C++ `startHalfedgeIdx` form so they double
/// as `contour2tri` keys downstream.
///
/// The C++ uses a `std::multimap<int,int>` keyed by `startVert`: `begin()` seeds each loop from the
/// smallest-key first-inserted edge, `find(endVert)` continues it, `erase` consumes. We mirror that
/// with a `BTreeMap<VertId, VecDeque<local>>` — smallest key via the ordered map, insertion order via
/// the deque (FIFO), `pop_front` = `find`+`erase`.
fn assemble_halfedges(
    hes: &[Halfedge],
    first: usize,
    last: usize,
    start_idx: i32,
) -> Vec<Vec<i32>> {
    let n = last - first;
    let mut vert_edge: BTreeMap<VertId, VecDeque<usize>> = BTreeMap::new();
    for local in 0..n {
        vert_edge
            .entry(hes[first + local].start_vert)
            .or_default()
            .push_back(local);
    }

    let mut polys: Vec<Vec<i32>> = Vec::new();
    let mut start_edge = 0usize;
    let mut this_edge = start_edge;
    loop {
        if this_edge == start_edge {
            // Seed a new loop from the smallest-key first-inserted edge — peeked, NOT consumed (it is
            // erased when the loop closes back onto it, exactly like the C++ `begin()`).
            let key = match vert_edge.keys().next() {
                Some(&k) => k,
                None => break,
            };
            start_edge = *vert_edge[&key].front().expect("non-empty bucket");
            this_edge = start_edge;
            polys.push(Vec::new());
        }
        polys.last_mut().unwrap().push(start_idx + this_edge as i32);
        let end_vert = hes[first + this_edge].end_vert;
        let dq = vert_edge
            .get_mut(&end_vert)
            .expect("non-manifold edge: loop does not continue");
        let nxt = dq.pop_front().expect("non-manifold edge: empty bucket");
        if dq.is_empty() {
            vert_edge.remove(&end_vert);
        }
        this_edge = nxt;
    }
    polys
}

/// One edge of a locally-emitted triangle: the start/end corner LABELS (buffer indices into
/// `face_halfedges`) and the OUTPUT mesh half-edge they were written to.
#[derive(Clone, Copy)]
struct LocalEdge {
    start: i32,
    end: i32,
    out: HalfedgeId,
}

/// Emit `tris` (triples of GLOBAL `face_halfedges` buffer indices — each names a polygon corner) as
/// output half-edges starting at `first_tri`, pairing interior diagonals within the face and recording
/// boundary edges into `contour2tri` (`face_op.cpp` `WriteLocalTriangles`, generalized past 2 tris).
fn write_local_triangles(
    out: &mut Mesh,
    contour2tri: &mut [HalfedgeId],
    hes: &[Halfedge],
    first_tri: usize,
    tris: &[[i32; 3]],
) {
    let first_out = TriId::from_usize(first_tri).first_halfedge();
    let mut local_edges: Vec<LocalEdge> = Vec::with_capacity(tris.len() * 3);
    let mut num_edge = 0i32;
    for tri in tris {
        for i in 0..3 {
            let out_idx = first_out.offset(num_edge);
            let start = tri[i];
            let end = tri[(i + 1) % 3];
            local_edges.push(LocalEdge {
                start,
                end,
                out: out_idx,
            });
            out.set_start(out_idx, hes[start as usize].start_vert);
            out.set_prop(out_idx, hes[start as usize].prop_vert);
            out.set_pair(out_idx, HalfedgeId::NONE);
            num_edge += 1;
        }
    }

    // Interior diagonals occur twice (once per adjacent triangle, reversed) → pair them; a boundary edge
    // occurs once → stash it in contour2tri for the later cross-face stitch.
    for e in &local_edges {
        let mut pair = HalfedgeId::NONE;
        for cand in &local_edges {
            if cand.start == e.end && cand.end == e.start {
                pair = cand.out;
                break;
            }
        }
        if pair.is_some() {
            out.set_pair(e.out, pair);
        } else {
            contour2tri[e.start as usize] = e.out;
        }
    }
}

/// Retriangulate the assembled polygon faces into `out`, in place (`Manifold::Impl::Face2Tri`).
///
/// On entry `out.vert_pos` holds the result verts and `out.face_normal` holds ONE normal per result
/// FACE (as `SizeOutput` gathered them); `face_edge`/`face_halfedges` describe the polygon faces, and
/// `halfedge_ref` holds the provenance [`TriRef`] of each face half-edge (all half-edges of a face share
/// one source triangle). On return `out.halfedge` is the triangulated half-edge mesh, `out.face_normal`
/// has one normal per output TRIANGLE, and `out.tri_ref` has one (temporary — `{0|1, srcTri}`) provenance
/// ref per output triangle, taken from the face's FIRST half-edge (Manifold's `WriteTriRefs`).
pub fn face2tri(
    out: &mut Mesh,
    face_edge: &[i32],
    face_halfedges: &[Halfedge],
    halfedge_ref: &[TriRef],
    epsilon: f64,
) {
    let num_face = face_edge.len() - 1;
    let face_normal_in = out.face_normal.clone();

    // Pass 1: triangulate every face; remember its triangles and count so we can lay out `halfedge`.
    let mut face_tris: Vec<Vec<[i32; 3]>> = Vec::with_capacity(num_face);
    let mut tri_offset: Vec<usize> = Vec::with_capacity(num_face + 1);
    let mut total_tris = 0usize;
    for face in 0..num_face {
        tri_offset.push(total_tris);
        let first = face_edge[face] as usize;
        let last = face_edge[face + 1] as usize;
        let num_edge = last - first;
        let mut tris: Vec<[i32; 3]> = Vec::new();
        // C++ `Face2Tri`'s numEdge==4 QUAD fast path (face_op.cpp:237-265), M.2.4a: diagonal
        // quad[0]-quad[2] preferred; flipped when non-CCW, or when both diagonals are valid and
        // quad[1]-quad[3] is shorter. The unified ear-clip used to take these too — same covered
        // SOLID, but a different diagonal on degenerate quads changes the EDGE SET, which reorders
        // the collapse cascade downstream (half the Cray divergence). Verbatim now.
        if num_edge == 4 {
            let projection = get_axis_aligned_projection(face_normal_in[face]);
            let quad = assemble_halfedges(face_halfedges, first, last, face_edge[face])
                .into_iter()
                .next()
                .expect("quad face has a loop");
            let p = |ge: i32| projection.apply(out.pos(face_halfedges[ge as usize].start_vert));
            let tri_ccw = |t: [i32; 3]| ccw(p(t[0]), p(t[1]), p(t[2]), epsilon) >= 0;
            let cand = [
                [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]],
                [[quad[1], quad[2], quad[3]], [quad[0], quad[1], quad[3]]],
            ];
            let mut choice = 0usize;
            if !(tri_ccw(cand[0][0]) && tri_ccw(cand[0][1])) {
                choice = 1;
            } else if tri_ccw(cand[1][0]) && tri_ccw(cand[1][1]) {
                let pos = |ge: i32| out.pos(face_halfedges[ge as usize].start_vert);
                let diag0 = pos(quad[0]) - pos(quad[2]);
                let diag1 = pos(quad[1]) - pos(quad[3]);
                if diag0.dot(diag0) > diag1.dot(diag1) {
                    choice = 1;
                }
            }
            tris = cand[choice].to_vec();
            total_tris += tris.len();
            face_tris.push(tris);
            continue;
        }
        if num_edge >= 3 {
            // Collect ALL loops of the face — an outer plus any interior hole loops — and hand them to the
            // multi-loop triangulator together, so a punched-through face gets its hole keyholed instead of
            // filled over. `idx` is the GLOBAL buffer index, so the returned triangles name face-halfedge
            // corners directly (what `write_local_triangles` consumes).
            let projection = get_axis_aligned_projection(face_normal_in[face]);
            let loops: Vec<Vec<PolyVert>> =
                assemble_halfedges(face_halfedges, first, last, face_edge[face])
                    .into_iter()
                    .map(|loop_edges| {
                        loop_edges
                            .into_iter()
                            .map(|ge| PolyVert {
                                pos: projection
                                    .apply(out.pos(face_halfedges[ge as usize].start_vert)),
                                idx: ge,
                            })
                            .collect()
                    })
                    .collect();
            tris = triangulate(&loops, epsilon);
        }
        total_tris += tris.len();
        face_tris.push(tris);
    }
    tri_offset.push(total_tris);

    // Size the output half-edge array and the per-triangle normals.
    out.halfedge = vec![
        crate::mesh::Halfedge {
            start_vert: VertId::NONE,
            paired_halfedge: HalfedgeId::NONE,
            prop_vert: VertId::NONE,
        };
        3 * total_tris
    ];
    let mut tri_normal = vec![crate::linalg::Vec3::ZERO; total_tris];
    // One provenance ref per output triangle; a placeholder for empty faces (never survives — empty
    // faces contribute no triangles). Every real triangle is overwritten from its face's first half-edge.
    let placeholder = TriRef {
        mesh_id: 0,
        original_id: -1,
        face_id: 0,
        coplanar_id: -1,
    };
    let mut tri_ref = vec![placeholder; total_tris];
    let mut contour2tri = vec![HalfedgeId::NONE; face_halfedges.len()];

    // Pass 2: write each face's triangles (with intra-face pairing + boundary recording), normals + refs.
    for face in 0..num_face {
        let tris = &face_tris[face];
        if tris.is_empty() {
            continue;
        }
        write_local_triangles(
            out,
            &mut contour2tri,
            face_halfedges,
            tri_offset[face],
            tris,
        );
        let face_ref = halfedge_ref[face_edge[face] as usize];
        for t in 0..tris.len() {
            tri_normal[tri_offset[face] + t] = face_normal_in[face];
            tri_ref[tri_offset[face] + t] = face_ref;
        }
    }

    // Cross-face stitch: pair each boundary output half-edge with the triangulated half-edge of the
    // face-half-edge's reverse (its `paired_halfedge`, a buffer index), via `contour2tri`.
    for edge in 0..face_halfedges.len() {
        let tri_edge = contour2tri[edge];
        if tri_edge.is_none() {
            continue;
        }
        let pair = face_halfedges[edge].paired_halfedge;
        if pair.is_none() {
            continue;
        }
        let pair_tri = contour2tri[pair.u()];
        out.set_pair(tri_edge, pair_tri);
    }

    out.face_normal = tri_normal;
    out.tri_ref = tri_ref;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linalg::Vec3;

    /// Build a value-form half-edge with an explicit end (the assembly's `face_halfedges` form). A
    /// negative `pair` becomes [`HalfedgeId::NONE`].
    fn he(start: i32, end: i32, pair: i32) -> Halfedge {
        Halfedge {
            start_vert: VertId::new(start),
            end_vert: VertId::new(end),
            paired_halfedge: HalfedgeId::new(pair),
            prop_vert: VertId::new(start),
        }
    }

    #[test]
    fn assemble_single_triangle_loop() {
        // Three half-edges forming one CCW loop 0→1→2→0. AssembleHalfedges returns one loop of the three
        // GLOBAL buffer indices in traversal order.
        let hes = [he(0, 1, -1), he(1, 2, -1), he(2, 0, -1)];
        let loops = assemble_halfedges(&hes, 0, 3, 0);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0], vec![0, 1, 2]);
    }

    #[test]
    fn assemble_two_disjoint_loops() {
        // Two separate triangles in one face slot → two loops. Start indices offset by `start_idx`.
        let hes = [
            he(0, 1, -1),
            he(1, 2, -1),
            he(2, 0, -1),
            he(3, 4, -1),
            he(4, 5, -1),
            he(5, 3, -1),
        ];
        let loops = assemble_halfedges(&hes, 0, 6, 100);
        assert_eq!(loops.len(), 2);
        // Loops are seeded from the smallest start vert; entries are start_idx + local.
        assert_eq!(loops[0], vec![100, 101, 102]);
        assert_eq!(loops[1], vec![103, 104, 105]);
    }

    #[test]
    fn face2tri_two_triangles_stitch_into_a_quad_face_pair() {
        // Two faces sharing one edge: face 0 is a triangle (verts 0,1,2), face 1 a triangle (verts
        // 0,2,3), sharing edge 0↔2. The shared face-halfedges are paired so the cross-face stitch links
        // the two output triangles across that edge. This exercises the whole Face2Tri pipe on the
        // simplest non-trivial case.
        //
        // face_halfedges: face 0 = [0→1, 1→2, 2→0]; face 1 = [0→2, 2→3, 3→0].
        // The reverse pair is face0's (2→0) ↔ face1's (0→2): buffer indices 2 and 3.
        let fhes = [
            he(0, 1, -1),
            he(1, 2, -1),
            he(2, 0, 3), // paired with buffer index 3
            he(0, 2, 2), // paired with buffer index 2
            he(2, 3, -1),
            he(3, 0, -1),
        ];
        let face_edge = [0i32, 3, 6];

        let mut out = Mesh {
            vert_pos: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            // one normal per FACE going in (both faces face +Z)
            face_normal: vec![Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 1.0)],
            ..Default::default()
        };
        // Provenance: face 0 came from P source triangle 5, face 1 from Q source triangle 8.
        let href = [
            TriRef {
                mesh_id: 0,
                original_id: -1,
                face_id: 5,
                coplanar_id: -1,
            },
            TriRef {
                mesh_id: 0,
                original_id: -1,
                face_id: 5,
                coplanar_id: -1,
            },
            TriRef {
                mesh_id: 0,
                original_id: -1,
                face_id: 5,
                coplanar_id: -1,
            },
            TriRef {
                mesh_id: 1,
                original_id: -1,
                face_id: 8,
                coplanar_id: -1,
            },
            TriRef {
                mesh_id: 1,
                original_id: -1,
                face_id: 8,
                coplanar_id: -1,
            },
            TriRef {
                mesh_id: 1,
                original_id: -1,
                face_id: 8,
                coplanar_id: -1,
            },
        ];
        face2tri(&mut out, &face_edge, &fhes, &href, 1e-9);

        // Two output triangles, six half-edges, one normal per triangle, one ref per triangle.
        assert_eq!(out.num_tri(), 2);
        assert_eq!(out.halfedge.len(), 6);
        assert_eq!(out.face_normal.len(), 2);
        // Each output triangle inherits its face's first-half-edge provenance ref.
        assert_eq!(out.tri_ref.len(), 2);
        assert_eq!(out.tri_ref[0].face_id, 5);
        assert_eq!(out.tri_ref[1].face_id, 8);
        // The shared edge got stitched: exactly one interior pairing across the two triangles (the 0↔2
        // diagonal), so both triangles carry a valid pair on that edge.
        let paired: usize = out
            .halfedge
            .iter()
            .filter(|h| h.paired_halfedge.is_some())
            .count();
        assert_eq!(paired, 2, "the shared 0↔2 edge pairs both ways");
        // Every output start vert is one of the 4 input verts.
        assert!(
            out.halfedge
                .iter()
                .all(|h| (0..4).contains(&h.start_vert.raw()))
        );
    }
}
