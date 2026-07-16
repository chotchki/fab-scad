//! `Face2Tri` — retriangulating the assembled polygonal faces into the result mesh (`face_op.cpp`).
//!
//! After the assembly ([`crate::boolean::boolean_result`]) the result's half-edges are NOT yet
//! triangles: they're general polygon faces, one per original P/Q triangle, delimited by `face_edge`
//! (offsets into `face_halfedges`, `numFaceR + 1` of them). `Face2Tri` turns each into triangles and
//! stitches the final half-edge mesh.
//!
//! PATH STRUCTURE vs Manifold: `face_op.cpp` special-cases tris (`WriteLocalTriangles` with 1 triangle,
//! plus an edge-reorder to form a valid triangle) and quads (2 triangles chosen by a CCW/diagonal-length
//! test), and routes everything else through `TriangulateIdxHalfedges`/`HalfedgeTriangulation`. We keep
//! the quad fast path verbatim (diagonal choice + `write_local_triangles`) and route every other face
//! (tris included) through `AssembleHalfedges` → project → ear-clip ([`crate::boolean::polygon`]) →
//! [`write_general_triangulation`], a port of the C++ `HalfedgeTriangulation` pairing scheme
//! (`polygon_internal.h` `AddHalfedge`/`AddContours` + `face_op.cpp` `WriteGeneralTriangulation`):
//! contour half-edges first (reversed), then triangle edges in emission order, each CONSUMING its
//! reverse match from a per-direction stack (LIFO). Consumption is load-bearing — a degenerate
//! self-touching face can legitimately use the same label diagonal TWICE (fuzzer trophy M.2.4b), and a
//! non-consuming first-match pairs both instances to the same partner, breaking the pairing involution
//! and sending `split_pinched_verts`' `for_vert` orbit off the rails (the M.3.9 class, OOM flavor).
//! Naive label matching survives only where labels are provably unique — the ≤2-triangle quad path,
//! exactly where C++ uses it. Routing single tris through the general writer (C++ uses
//! `WriteLocalTriangles`) is output-identical: one triangle has no diagonals, so all three edges land in
//! `contour2tri` either way.
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
/// boundary edges into the face's `contour2tri` chunk (`face_op.cpp` `WriteLocalTriangles`). QUAD PATH
/// ONLY: the naive reverse-label match is sound only while every directed label edge is unique —
/// guaranteed for ≤2 triangles of a single loop, NOT for general faces (see
/// [`write_general_triangulation`]).
///
/// CHUNKED FORM (parallel pass 2): `hes_out` is the face's own `3 * numTri` slice of the output
/// half-edge array and `c2t` its `face_edge[face]..face_edge[face+1]` slice of `contour2tri` —
/// slot indices are chunk-LOCAL (`label - label_base` for `c2t`), while the pair VALUES stay global
/// [`HalfedgeId`]s built from `first_tri`. Same values into the same slots as the old whole-array
/// writes, so serial == parallel bytes by construction.
fn write_local_triangles(
    hes_out: &mut [crate::mesh::Halfedge],
    c2t: &mut [HalfedgeId],
    hes: &[Halfedge],
    first_tri: usize,
    label_base: i32,
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
            let slot = &mut hes_out[num_edge as usize];
            slot.start_vert = hes[start as usize].start_vert;
            slot.prop_vert = hes[start as usize].prop_vert;
            slot.paired_halfedge = HalfedgeId::NONE;
            num_edge += 1;
        }
    }

    // Interior diagonals occur twice (once per adjacent triangle, reversed) → pair them; a boundary edge
    // occurs once → stash it in contour2tri for the later cross-face stitch.
    for (local, e) in local_edges.iter().enumerate() {
        let mut pair = HalfedgeId::NONE;
        for cand in &local_edges {
            if cand.start == e.end && cand.end == e.start {
                pair = cand.out;
                break;
            }
        }
        if pair.is_some() {
            hes_out[local].paired_halfedge = pair;
        } else {
            c2t[(e.start - label_base) as usize] = e.out;
        }
    }
}

/// One half-edge of a face's local triangulation (C++ `polygon_internal.h` `HalfedgeTriangulation`
/// entry): `start`/`end` are corner LABELS (buffer indices into `face_halfedges`), `pair` a LOCAL index
/// into the same list (`-1` = unpaired).
struct FaceHalfedge {
    start: i32,
    end: i32,
    pair: i32,
}

/// Emit a general face's ear-clip triangulation as output half-edges, pairing via the C++
/// `HalfedgeTriangulation` scheme (`polygon_internal.h` `AddContours`/`AddHalfedge` +
/// `face_op.cpp` `WriteGeneralTriangulation`): the face's contour half-edges are added FIRST, each
/// REVERSED (the exterior side, opposite the filled interior), then the triangles' edges in emission
/// order; every added edge CONSUMES an unpaired reverse match from a per-direction stack (LIFO —
/// C++ `back()`/`pop_back`). A triangle edge that pairs another triangle edge is an interior diagonal
/// (intra-face pair); one that pairs a contour half-edge is a face boundary, recorded in `contour2tri`
/// (keyed by the contour edge's start label) for the cross-face stitch. Consuming matches is what keeps
/// the pairing an involution when a degenerate self-touching face uses the same label diagonal twice
/// (fuzzer trophy M.2.4b) — naive first-match pairs both instances to one partner and breaks it.
///
/// CHUNKED FORM (parallel pass 2): `hes_out`/`c2t` are the face's own disjoint slices (see
/// [`write_local_triangles`]) — chunk-local slots, global [`HalfedgeId`] values.
fn write_general_triangulation(
    hes_out: &mut [crate::mesh::Halfedge],
    c2t: &mut [HalfedgeId],
    hes: &[Halfedge],
    first_tri: usize,
    label_base: i32,
    loops: &[Vec<i32>],
    tris: &[[i32; 3]],
) {
    use std::collections::HashMap;
    let num_contour: usize = loops.iter().map(Vec::len).sum();
    let mut halfedges: Vec<FaceHalfedge> = Vec::with_capacity(num_contour + 3 * tris.len());
    // Directed label edge → stack of yet-unpaired local half-edge indices. Lookup-only (never
    // iterated), so a HashMap is order-safe here; determinism comes from insertion/pop order.
    let mut edge2halfedge: HashMap<(i32, i32), Vec<i32>> = HashMap::new();
    let mut add_halfedge = |halfedges: &mut Vec<FaceHalfedge>, start: i32, end: i32| {
        let idx = halfedges.len() as i32;
        let mut pair = -1;
        if let Some(stack) = edge2halfedge.get_mut(&(end, start)) {
            let rev = stack.pop().expect("empty stacks are removed");
            if stack.is_empty() {
                edge2halfedge.remove(&(end, start));
            }
            halfedges[rev as usize].pair = idx;
            pair = rev;
        } else {
            edge2halfedge.entry((start, end)).or_default().push(idx);
        }
        halfedges.push(FaceHalfedge { start, end, pair });
    };

    // AddContours: store the exterior contour half-edge, opposite the filled interior.
    for lp in loops {
        let n = lp.len();
        for i in 0..n {
            let start = lp[i];
            let end = lp[(i + 1) % n];
            add_halfedge(&mut halfedges, end, start);
        }
    }
    let contour_end = halfedges.len() as i32;

    for t in tris {
        add_halfedge(&mut halfedges, t[0], t[1]);
        add_halfedge(&mut halfedges, t[1], t[2]);
        add_halfedge(&mut halfedges, t[2], t[0]);
    }

    // Triangle half-edges → output mesh (C++ `WriteGeneralTriangulation`, triangle pass): an
    // intra-face pair maps to the partner's output slot; a contour pair stays NONE here (the contour
    // pass + cross-face stitch fill it).
    let first_out = TriId::from_usize(first_tri).first_halfedge();
    let num_tri_he = halfedges.len() as i32 - contour_end;
    for local in 0..num_tri_he {
        let he = &halfedges[(contour_end + local) as usize];
        let slot = &mut hes_out[local as usize];
        slot.start_vert = hes[he.start as usize].start_vert;
        slot.prop_vert = hes[he.start as usize].prop_vert;
        slot.paired_halfedge = if he.pair >= contour_end {
            first_out.offset(he.pair - contour_end)
        } else {
            HalfedgeId::NONE
        };
    }

    // Contour pass: each paired contour half-edge names the boundary triangle-edge for the cross-face
    // stitch. The contour half-edge was stored REVERSED, so its `end` is the contour edge's START
    // label — the `contour2tri` key. A contour half-edge paired to another contour half-edge would be a
    // doubled contour edge (C++ DEBUG_ASSERTs `topologyErr` there); skip it rather than write a
    // negative offset.
    for c in 0..contour_end {
        let he = &halfedges[c as usize];
        // `< contour_end` covers both C++ checks: unpaired (`< 0`, skipped) and contour-paired-to-
        // contour (the doubled-contour degenerate C++ DEBUG_ASSERTs on — skipping keeps us from
        // writing a negative offset).
        if he.pair < contour_end {
            continue;
        }
        c2t[(he.end - label_base) as usize] = first_out.offset(he.pair - contour_end);
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

    // Pass 1 (PARALLEL, M.4.3c): triangulate every face — a pure per-face function of the shared
    // assembly inputs, so the order-preserving `par::map_collect` gives par == seq bit-for-bit (the
    // ear-clip's BTreeSet cost queue is per-face LOCAL state). The prefix-sum layout stays serial.
    // Yields `(label loops, triangles)` per face: the general writer needs the assembled contour
    // loops for its pairing (the quad path doesn't — its loops slot stays empty).
    let vert_pos = &out.vert_pos;
    let faces: Vec<usize> = (0..num_face).collect();
    type FacePolys = (Vec<Vec<i32>>, Vec<[i32; 3]>);
    let face_polys: Vec<FacePolys> = crate::par::map_collect(&faces, |&face| {
        let first = face_edge[face] as usize;
        let last = face_edge[face + 1] as usize;
        let num_edge = last - first;
        // C++ `Face2Tri`'s numEdge==3 SINGLE-TRIANGLE fast path (face_op.cpp:219), M.7.3.2: the
        // three half-edges already ARE the triangle — order them (at most one swap) and emit,
        // skipping assembly, projection, and the general writer's per-face maps. ~95% of a big
        // union's faces are plain triangles; routing them through the general path was the
        // constant-factor half of the outlet runaway.
        if num_edge == 3 {
            let ge = [first as i32, first as i32 + 1, first as i32 + 2];
            let tri = ge.map(|e| face_halfedges[e as usize].start_vert);
            let ends = ge.map(|e| face_halfedges[e as usize].end_vert);
            let ordered = if ends[0] == tri[2] {
                [ge[0], ge[2], ge[1]]
            } else {
                ge
            };
            return (Vec::new(), vec![ordered]);
        }
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
            let p =
                |ge: i32| projection.apply(vert_pos[face_halfedges[ge as usize].start_vert.u()]);
            let tri_ccw = |t: [i32; 3]| ccw(p(t[0]), p(t[1]), p(t[2]), epsilon) >= 0;
            let cand = [
                [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]],
                [[quad[1], quad[2], quad[3]], [quad[0], quad[1], quad[3]]],
            ];
            let mut choice = 0usize;
            if !(tri_ccw(cand[0][0]) && tri_ccw(cand[0][1])) {
                choice = 1;
            } else if tri_ccw(cand[1][0]) && tri_ccw(cand[1][1]) {
                let pos = |ge: i32| vert_pos[face_halfedges[ge as usize].start_vert.u()];
                let diag0 = pos(quad[0]) - pos(quad[2]);
                let diag1 = pos(quad[1]) - pos(quad[3]);
                if diag0.dot(diag0) > diag1.dot(diag1) {
                    choice = 1;
                }
            }
            return (Vec::new(), cand[choice].to_vec());
        }
        if num_edge >= 3 {
            // Collect ALL loops of the face — an outer plus any interior hole loops — and hand them to the
            // multi-loop triangulator together, so a punched-through face gets its hole keyholed instead of
            // filled over. `idx` is the GLOBAL buffer index, so the returned triangles name face-halfedge
            // corners directly (what `write_general_triangulation` consumes).
            let projection = get_axis_aligned_projection(face_normal_in[face]);
            let label_loops = assemble_halfedges(face_halfedges, first, last, face_edge[face]);
            let loops: Vec<Vec<PolyVert>> = label_loops
                .iter()
                .map(|loop_edges| {
                    loop_edges
                        .iter()
                        .map(|&ge| PolyVert {
                            pos: projection
                                .apply(vert_pos[face_halfedges[ge as usize].start_vert.u()]),
                            idx: ge,
                        })
                        .collect()
                })
                .collect();
            let tris = triangulate(&loops, epsilon);
            return (label_loops, tris);
        }
        (Vec::new(), Vec::new())
    });
    let mut tri_offset: Vec<usize> = Vec::with_capacity(num_face + 1);
    let mut total_tris = 0usize;
    for (_, tris) in &face_polys {
        tri_offset.push(total_tris);
        total_tris += tris.len();
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

    // Pass 2 (PARALLEL, C++ `outputFace` Par ≥1e4 at face_op.cpp:342): write each face's triangles
    // (with intra-face pairing + boundary recording), normals + refs. The quad fast path keeps C++
    // `WriteLocalTriangles` (label matching is safe at ≤2 triangles of one loop — labels are unique);
    // every other face goes through the `HalfedgeTriangulation` port.
    //
    // DETERMINISM: every write a face makes lands in slots OWNED by that face — output half-edges
    // `3*tri_offset[face]..3*tri_offset[face+1]`, normals/refs `tri_offset[face]..tri_offset[face+1]`,
    // and `contour2tri` only at the face's own labels `face_edge[face]..face_edge[face+1]` (both
    // writers key `contour2tri` by the face's OWN corner labels). So we pre-split all four arrays into
    // per-face chunks and drive `par::for_each_mut` over the chunk structs: slot values are pure
    // per-face functions of the shared assembly inputs, writes are disjoint by construction — the
    // deterministic scatter, NOT C++'s tbb emit-order scheme (which is only safe because ITS writes
    // are disjoint too; ours makes the disjointness structural).
    struct FaceSlots<'a> {
        hes_out: &'a mut [crate::mesh::Halfedge],
        normals: &'a mut [crate::linalg::Vec3],
        refs: &'a mut [TriRef],
        c2t: &'a mut [HalfedgeId],
    }
    {
        let mut face_slots: Vec<FaceSlots> = Vec::with_capacity(num_face);
        let mut hes_rest: &mut [crate::mesh::Halfedge] = &mut out.halfedge;
        let mut norm_rest: &mut [crate::linalg::Vec3] = &mut tri_normal;
        let mut ref_rest: &mut [TriRef] = &mut tri_ref;
        let mut c2t_rest: &mut [HalfedgeId] = &mut contour2tri;
        for face in 0..num_face {
            let ntri = tri_offset[face + 1] - tri_offset[face];
            let nedge = (face_edge[face + 1] - face_edge[face]) as usize;
            let (hes_out, hr) = hes_rest.split_at_mut(3 * ntri);
            let (normals, nr) = norm_rest.split_at_mut(ntri);
            let (refs, rr) = ref_rest.split_at_mut(ntri);
            let (c2t, cr) = c2t_rest.split_at_mut(nedge);
            hes_rest = hr;
            norm_rest = nr;
            ref_rest = rr;
            c2t_rest = cr;
            face_slots.push(FaceSlots {
                hes_out,
                normals,
                refs,
                c2t,
            });
        }
        crate::par::for_each_mut(&mut face_slots, |face, slots| {
            let (loops, tris) = &face_polys[face];
            if tris.is_empty() {
                return;
            }
            let num_edge = (face_edge[face + 1] - face_edge[face]) as usize;
            // Tri AND quad take C++ `WriteLocalTriangles` (label matching is safe at ≤2 triangles of
            // one loop). Routing the tri path's loop-less output through the GENERAL writer left every
            // tri boundary edge out of `contour2tri` — NONE pairs → runaway `for_vert` orbits.
            if num_edge <= 4 {
                write_local_triangles(
                    slots.hes_out,
                    slots.c2t,
                    face_halfedges,
                    tri_offset[face],
                    face_edge[face],
                    tris,
                );
            } else {
                write_general_triangulation(
                    slots.hes_out,
                    slots.c2t,
                    face_halfedges,
                    tri_offset[face],
                    face_edge[face],
                    loops,
                    tris,
                );
            }
            let face_ref = halfedge_ref[face_edge[face] as usize];
            for t in 0..tris.len() {
                slots.normals[t] = face_normal_in[face];
                slots.refs[t] = face_ref;
            }
        });
    }

    // Cross-face stitch (PARALLEL, C++ Par ≥1e5 at face_op.cpp:355): pair each boundary output
    // half-edge with the triangulated half-edge of the face-half-edge's reverse (its
    // `paired_halfedge`, a buffer index), via `contour2tri`.
    //
    // DETERMINISM: the stitch is per-face independent in its WRITES — `contour2tri[edge]` was
    // written by edge's own face and points into that face's output-halfedge chunk (both writers
    // only record their OWN output half-edges), so chunking `out.halfedge` per face again gives
    // disjoint writes; the cross-face part (`contour2tri[pair]`, the other face's boundary edge) is
    // a pure READ of the now-frozen `contour2tri`. Within a face, edges run in the original
    // ascending order, so even a degenerate duplicate `tri_edge` resolves exactly as the old serial
    // loop did.
    let contour2tri = contour2tri; // frozen: reads only from here on
    {
        let mut stitch_chunks: Vec<&mut [crate::mesh::Halfedge]> = Vec::with_capacity(num_face);
        let mut rest: &mut [crate::mesh::Halfedge] = &mut out.halfedge;
        for face in 0..num_face {
            let ntri = tri_offset[face + 1] - tri_offset[face];
            let (chunk, r) = rest.split_at_mut(3 * ntri);
            stitch_chunks.push(chunk);
            rest = r;
        }
        crate::par::for_each_mut(&mut stitch_chunks, |face, chunk| {
            let he_base = 3 * tri_offset[face];
            for edge in face_edge[face] as usize..face_edge[face + 1] as usize {
                let tri_edge = contour2tri[edge];
                if tri_edge.is_none() {
                    continue;
                }
                let pair = face_halfedges[edge].paired_halfedge;
                if pair.is_none() {
                    continue;
                }
                let pair_tri = contour2tri[pair.u()];
                chunk[tri_edge.u() - he_base].paired_halfedge = pair_tri;
            }
        });
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
