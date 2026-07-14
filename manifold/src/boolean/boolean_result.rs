//! `Boolean3::Result` — reassembling a watertight manifold from the four intersection tables
//! (`boolean_result.cpp`).
//!
//! Given [`Boolean3`]'s `xv12`/`xv21` (edge×face crossings) and `w03`/`w30` (per-vertex winding), this
//! builds the output mesh: winding → inclusion counts, vertex duplication + remapping, per-edge new
//! vertices, the polygon faces (`AppendPartialEdges`/`AppendNewEdges`/`AppendWholeEdges`), then
//! retriangulation ([`crate::boolean::face_op::face2tri`]). Ported from `Result()` and its helpers.
//!
//! GATE-A MINIMAL PIPELINE — the provenance + cleanup tail is deferred (none of it changes the covered
//! solid, which is all the residual/is_manifold/genus/volume gates measure):
//! - SKIPPED: `MapTriRef`/`UpdateReference`/`CreateProperties` (color/UV provenance), `SimplifyTopology`
//!   (coplanar-edge cleanup — `IsManifold` passes BEFORE it), `SortGeometry` (Morton reindex — the gates
//!   are order-independent), `IncrementMeshIDs`.
//! - `ReorderHalfedges` skipped (within-face canonicalization for run-to-run bit determinism).
//! - `RemoveUnreferencedVerts` KEPT but as a compaction (see [`crate::mesh::Mesh::remove_unreferenced_verts`]),
//!   so `genus` — which counts `vert_pos.len()` — stays exact without `SortGeometry`.
//!
//! Container discipline: `edgesP`/`edgesQ` are keyed by forward-halfedge index, `edgesNew` by
//! `(faceP, faceQ)` — `BTreeMap` NOT HashMap, because the iteration order is load-bearing (it sequences
//! the output half-edges written per face).

use std::collections::BTreeMap;

use crate::boolean::OpType;
use crate::boolean::boolean3::Boolean3;
use crate::boolean::face_op::face2tri;
use crate::boolean::predicates::fmax;
use crate::boolean::vocab::Halfedge as VHalfedge;
use crate::linalg::{Box3, Vec3};
use crate::mesh::Mesh;

/// A cut-edge point awaiting pairing (`boolean_result.cpp` `EdgePos`). Sorted by `(edge_pos,
/// collision_id)` — `collision_id` deterministically breaks position ties (the fix for the C++'s prior
/// nondeterminism).
#[derive(Clone, Copy, Debug)]
struct EdgePos {
    edge_pos: f64,
    vert: i32,
    collision_id: i32,
    is_start: bool,
}

impl EdgePos {
    /// `operator<`: `edge_pos` ascending, `collision_id` breaking exact ties (`boolean_result.cpp`).
    fn order(a: &EdgePos, b: &EdgePos) -> core::cmp::Ordering {
        match a.edge_pos.partial_cmp(&b.edge_pos) {
            Some(core::cmp::Ordering::Equal) | None => a.collision_id.cmp(&b.collision_id),
            Some(o) => o,
        }
    }
}

/// The `i`-th component of a `Vec3` (`0→x, 1→y, 2→z`) — `AppendNewEdges` orders points along the
/// bounding box's longest axis.
#[inline]
fn component(v: Vec3, i: usize) -> f64 {
    match i {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// `exclusive_scan` with the `AbsSum(a, b) = a + |b|` operator (`boolean_result.cpp`). Returns the scan
/// (`out[k] = init + Σ_{j<k}|input[j]|`) and the final accumulator (`= AbsSum(out.back(), input.back())`
/// — the running vertex count after this block).
fn exclusive_scan_abssum(input: &[i32], init: i32) -> (Vec<i32>, i32) {
    let mut out = Vec::with_capacity(input.len());
    let mut acc = init;
    for &v in input {
        out.push(acc);
        acc += v.abs();
    }
    (out, acc)
}

/// Fill the output vertex positions, duplicating each source vert `|inclusion|` times starting at its
/// remap index (`boolean_result.cpp` `DuplicateVerts`). `inclusion`/`remap`/`src` are all indexed by the
/// source vertex; a `0`-inclusion vert writes nothing.
fn duplicate_verts(vert_pos_r: &mut [Vec3], inclusion: &[i32], remap: &[i32], src: &[Vec3]) {
    for vert in 0..src.len() {
        let n = inclusion[vert].abs();
        for i in 0..n {
            vert_pos_r[(remap[vert] + i) as usize] = src[vert];
        }
    }
}

/// For each edge×face intersection, add its new vertex to the intersected edge's list (`edgesP`) and to
/// the two new edges between the Q-face and the edge's two P-faces (`edgesNew`) — direction and
/// multiplicity from `inclusion` (`boolean_result.cpp` `AddNewEdgeVerts`). `forward` false reverses the
/// roles; `offset` keeps `collision_id` unique across the two calls.
///
/// NB: the C++ has a trailing `direction = !direction` inside the per-edge loop that is DEAD (the three
/// targets captured `direction` by value at construction), so only the three initial `is_start` values
/// matter — reproduced directly here.
#[allow(clippy::too_many_arguments)]
fn add_new_edge_verts(
    edges_p: &mut BTreeMap<i32, Vec<EdgePos>>,
    edges_new: &mut BTreeMap<(i32, i32), Vec<EdgePos>>,
    p1q2: &[[i32; 2]],
    i12: &[i32],
    v12r: &[i32],
    halfedge_p: &Mesh,
    forward: bool,
    offset: usize,
) {
    for i in 0..p1q2.len() {
        let edge_p = p1q2[i][if forward { 0 } else { 1 }];
        let face_q = p1q2[i][if forward { 1 } else { 0 }];
        let vert = v12r[i];
        let inclusion = i12[i];

        let mut key_right = (halfedge_p.pair(edge_p) / 3, face_q);
        let mut key_left = (edge_p / 3, face_q);
        if !forward {
            core::mem::swap(&mut key_right.0, &mut key_right.1);
            core::mem::swap(&mut key_left.0, &mut key_left.1);
        }

        let direction = inclusion < 0;
        let is_start = [direction, direction ^ !forward, direction ^ forward];
        let n = inclusion.abs();
        let cid = (i + offset) as i32;

        for j in 0..n {
            edges_p.entry(edge_p).or_default().push(EdgePos {
                edge_pos: 0.0,
                vert: vert + j,
                collision_id: cid,
                is_start: is_start[0],
            });
        }
        for j in 0..n {
            edges_new.entry(key_right).or_default().push(EdgePos {
                edge_pos: 0.0,
                vert: vert + j,
                collision_id: cid,
                is_start: is_start[1],
            });
        }
        for j in 0..n {
            edges_new.entry(key_left).or_default().push(EdgePos {
                edge_pos: 0.0,
                vert: vert + j,
                collision_id: cid,
                is_start: is_start[2],
            });
        }
    }
}

/// Pair start-verts with end-verts to form output half-edges (`boolean_result.cpp` `PairUp`). Partition
/// starts from ends, sort each half by [`EdgePos::order`], then pair the i-th start with the i-th end.
/// The ordered pairing is what makes the result geometrically valid.
fn pair_up(edge_pos: &mut Vec<EdgePos>, mut f: impl FnMut(VHalfedge)) {
    debug_assert!(
        edge_pos.len().is_multiple_of(2),
        "non-manifold edge: odd number of points"
    );
    let n_edges = edge_pos.len() / 2;
    let (mut starts, mut ends): (Vec<EdgePos>, Vec<EdgePos>) =
        edge_pos.drain(..).partition(|e| e.is_start);
    debug_assert_eq!(starts.len(), n_edges, "non-manifold edge: start/end imbalance");
    starts.sort_by(EdgePos::order);
    ends.sort_by(EdgePos::order);
    for i in 0..n_edges {
        f(VHalfedge {
            start_vert: starts[i].vert,
            end_vert: ends[i].vert,
            paired_halfedge: -1,
            prop_vert: starts[i].vert,
        });
    }
}

/// The two-halfedge emit shared by `AppendPartialEdges`/`AppendNewEdges`: advance the write cursor for
/// the left and right result faces, write the forward edge to `face_left` and its reverse to
/// `face_right`, cross-linking their pairs.
fn emit_paired(
    face_halfedges: &mut [VHalfedge],
    face_ptr_r: &mut [i32],
    face_left: i32,
    face_right: i32,
    mut e: VHalfedge,
) {
    let forward_edge = face_ptr_r[face_left as usize];
    face_ptr_r[face_left as usize] += 1;
    let backward_edge = face_ptr_r[face_right as usize];
    face_ptr_r[face_right as usize] += 1;

    e.paired_halfedge = backward_edge;
    face_halfedges[forward_edge as usize] = e;

    core::mem::swap(&mut e.start_vert, &mut e.end_vert);
    e.paired_halfedge = forward_edge;
    face_halfedges[backward_edge as usize] = e;
}

/// Distribute the partially-retained edges to their faces (`boolean_result.cpp` `AppendPartialEdges`).
/// Each map entry is a cut edge: project its new verts and its retained endpoints onto the edge vector,
/// pair them up, and emit to the edge's two faces. Marks the whole-edge bitmap false for cut edges.
#[allow(clippy::too_many_arguments)]
fn append_partial_edges(
    out: &Mesh,
    face_halfedges: &mut [VHalfedge],
    whole_halfedge_p: &mut [bool],
    face_ptr_r: &mut [i32],
    edges_p: &BTreeMap<i32, Vec<EdgePos>>,
    in_p: &Mesh,
    i03: &[i32],
    v_p2r: &[i32],
    face_p2r: &[i32],
) {
    for (&edge_p, edge_pos_ref) in edges_p {
        let mut edge_pos_p = edge_pos_ref.clone();
        edge_pos_p.sort_by(EdgePos::order);

        let pair_p = in_p.pair(edge_p);
        whole_halfedge_p[edge_p as usize] = false;
        whole_halfedge_p[pair_p as usize] = false;

        let v_start = in_p.start(edge_p);
        let v_end = in_p.end(edge_p);
        let edge_vec = in_p.vert_pos[v_end as usize] - in_p.vert_pos[v_start as usize];

        for e in &mut edge_pos_p {
            e.edge_pos = out.vert_pos[e.vert as usize].dot(edge_vec);
        }

        let mut inclusion = i03[v_start as usize];
        let mut ep = EdgePos {
            edge_pos: out.vert_pos[v_p2r[v_start as usize] as usize].dot(edge_vec),
            vert: v_p2r[v_start as usize],
            collision_id: i32::MAX,
            is_start: inclusion > 0,
        };
        for _ in 0..inclusion.abs() {
            edge_pos_p.push(ep);
            ep.vert += 1;
        }

        inclusion = i03[v_end as usize];
        ep = EdgePos {
            edge_pos: out.vert_pos[v_p2r[v_end as usize] as usize].dot(edge_vec),
            vert: v_p2r[v_end as usize],
            collision_id: i32::MAX,
            is_start: inclusion < 0,
        };
        for _ in 0..inclusion.abs() {
            edge_pos_p.push(ep);
            ep.vert += 1;
        }

        let face_left = face_p2r[(edge_p / 3) as usize];
        let face_right = face_p2r[(pair_p / 3) as usize];

        pair_up(&mut edge_pos_p, |e| {
            emit_paired(face_halfedges, face_ptr_r, face_left, face_right, e)
        });
    }
}

/// Distribute the brand-new intersection edges to their `(faceP, faceQ)` face pair
/// (`boolean_result.cpp` `AppendNewEdges`). Orders each edge's verts along the longest bounding-box axis
/// before pairing.
fn append_new_edges(
    out: &Mesh,
    face_halfedges: &mut [VHalfedge],
    face_ptr_r: &mut [i32],
    edges_new: &BTreeMap<(i32, i32), Vec<EdgePos>>,
    face_pq2r: &[i32],
    num_face_p: usize,
) {
    for (&(face_p, face_q), edge_pos_ref) in edges_new {
        let mut edge_pos = edge_pos_ref.clone();
        edge_pos.sort_by(EdgePos::order);

        let mut bbox = Box3::default();
        for e in &edge_pos {
            bbox.union_point(out.vert_pos[e.vert as usize]);
        }
        let size = bbox.size();
        let dim = if size.x > size.y && size.x > size.z {
            0
        } else if size.y > size.z {
            1
        } else {
            2
        };
        for e in &mut edge_pos {
            e.edge_pos = component(out.vert_pos[e.vert as usize], dim);
        }

        let face_left = face_pq2r[face_p as usize];
        let face_right = face_pq2r[num_face_p + face_q as usize];

        pair_up(&mut edge_pos, |e| {
            emit_paired(face_halfedges, face_ptr_r, face_left, face_right, e)
        });
    }
}

/// Emit the whole (uncut) edges, duplicated per inclusion (`boolean_result.cpp` `DuplicateHalfedges` via
/// `AppendWholeEdges`). Each uncut edge is processed once from its forward half-edge, emitting both
/// output half-edges directly with their pairing.
#[allow(clippy::too_many_arguments)]
fn append_whole_edges(
    face_halfedges: &mut [VHalfedge],
    face_ptr_r: &mut [i32],
    in_p: &Mesh,
    whole_halfedge_p: &[bool],
    i03: &[i32],
    v_p2r: &[i32],
    face_p2r: &[i32],
) {
    for idx in 0..in_p.halfedge.len() as i32 {
        if !whole_halfedge_p[idx as usize] {
            continue;
        }
        let mut start_vert = in_p.start(idx);
        let mut end_vert = in_p.end(idx);
        if start_vert >= end_vert {
            continue;
        }
        let inclusion = i03[start_vert as usize];
        if inclusion == 0 {
            continue;
        }
        if inclusion < 0 {
            core::mem::swap(&mut start_vert, &mut end_vert);
        }
        start_vert = v_p2r[start_vert as usize];
        end_vert = v_p2r[end_vert as usize];
        let prop_vert = in_p.prop(idx);
        let pair = in_p.pair(idx);
        let pair_prop_vert = in_p.prop(pair);
        let new_face = face_p2r[(idx / 3) as usize];
        let face_right = face_p2r[(pair / 3) as usize];

        for _ in 0..inclusion.abs() {
            let forward_edge = face_ptr_r[new_face as usize];
            face_ptr_r[new_face as usize] += 1;
            let backward_edge = face_ptr_r[face_right as usize];
            face_ptr_r[face_right as usize] += 1;

            face_halfedges[forward_edge as usize] = VHalfedge {
                start_vert,
                end_vert,
                paired_halfedge: backward_edge,
                prop_vert,
            };
            face_halfedges[backward_edge as usize] = VHalfedge {
                start_vert: end_vert,
                end_vert: start_vert,
                paired_halfedge: forward_edge,
                prop_vert: pair_prop_vert,
            };
            start_vert += 1;
            end_vert += 1;
        }
    }
}

/// Size the output faces and gather their normals (`boolean_result.cpp` `SizeOutput`). Counts the
/// half-edges landing on each face (retained verts + new edge verts), drops empty faces, and returns
/// `(faceEdge, facePQ2R)`: per-result-face half-edge offsets, and the old→new face-index remap.
/// Populates `out.face_normal` with ONE normal per result face (Q negated under `invert_q`).
#[allow(clippy::too_many_arguments)]
fn size_output(
    out: &mut Mesh,
    in_p: &Mesh,
    in_q: &Mesh,
    i03: &[i32],
    i30: &[i32],
    i12: &[i32],
    i21: &[i32],
    p1q2: &[[i32; 2]],
    p2q1: &[[i32; 2]],
    invert_q: bool,
) -> (Vec<i32>, Vec<i32>) {
    let num_tri_p = in_p.num_tri();
    let num_tri_q = in_q.num_tri();
    let mut sides = vec![0i32; num_tri_p + num_tri_q];

    // CountVerts: each face collects |inclusion| of each of its 3 corner verts.
    for i in 0..num_tri_p {
        for j in 0..3 {
            sides[i] += i03[in_p.start(3 * i as i32 + j) as usize].abs();
        }
    }
    for i in 0..num_tri_q {
        for j in 0..3 {
            sides[num_tri_p + i] += i30[in_q.start(3 * i as i32 + j) as usize].abs();
        }
    }

    // CountNewVerts<false> over p1q2: edgeP = pq[0] (P edge), faceQ = pq[1] (Q face).
    for idx in 0..p1q2.len() {
        let edge_p = p1q2[idx][0];
        let face_q = p1q2[idx][1];
        let incl = i12[idx].abs();
        sides[num_tri_p + face_q as usize] += incl;
        sides[(edge_p / 3) as usize] += incl;
        sides[(in_p.pair(edge_p) / 3) as usize] += incl;
    }
    // CountNewVerts<true> over p2q1: edgeP = pq[1] (Q edge), faceQ = pq[0] (P face); counts swap roles.
    for idx in 0..p2q1.len() {
        let edge_q = p2q1[idx][1];
        let face_p = p2q1[idx][0];
        let incl = i21[idx].abs();
        sides[face_p as usize] += incl;
        sides[num_tri_p + (edge_q / 3) as usize] += incl;
        sides[num_tri_p + (in_q.pair(edge_q) / 3) as usize] += incl;
    }

    // facePQ2R[f] = number of kept faces strictly before f = f's new result index (when kept).
    let mut face_pq2r = vec![0i32; num_tri_p + num_tri_q];
    let mut running = 0i32;
    for f in 0..(num_tri_p + num_tri_q) {
        face_pq2r[f] = running;
        if sides[f] > 0 {
            running += 1;
        }
    }

    // One normal per kept face, P then Q in face order (Q negated under Subtract) — matches facePQ2R.
    let mut face_normal_r = Vec::with_capacity(running as usize);
    for (f, &s) in sides.iter().enumerate().take(num_tri_p) {
        if s > 0 {
            face_normal_r.push(in_p.face_normal[f]);
        }
    }
    for f in 0..num_tri_q {
        if sides[num_tri_p + f] > 0 {
            let n = in_q.face_normal[f];
            face_normal_r.push(if invert_q { -n } else { n });
        }
    }
    out.face_normal = face_normal_r;

    // faceEdge: prefix-sum of the nonzero side counts (per-result-face half-edge offsets).
    let nonzero: Vec<i32> = sides.iter().copied().filter(|&s| s > 0).collect();
    let mut face_edge = vec![0i32; nonzero.len() + 1];
    for k in 0..nonzero.len() {
        face_edge[k + 1] = face_edge[k] + nonzero[k];
    }

    (face_edge, face_pq2r)
}

/// Run a full boolean: the [`Boolean3`] intersection stage then the result assembly. This is the R1
/// tracer entry point (`Manifold::Impl::Boolean` + `Boolean3::Result` fused).
pub fn boolean(in_p: &Mesh, in_q: &Mesh, op: OpType) -> Mesh {
    let b3 = Boolean3::new(in_p, in_q, op);
    result(&b3, in_p, in_q, op)
}

/// Assemble the result mesh from the intersection tables (`boolean_result.cpp` `Boolean3::Result`, the
/// GATE-A minimal pipeline — see the module doc for what's deferred).
fn result(b3: &Boolean3, in_p: &Mesh, in_q: &Mesh, op: OpType) -> Mesh {
    debug_assert_eq!(
        b3.expand_p,
        op == OpType::Add,
        "Result op type not compatible with constructor op type"
    );

    // Empty-input early-outs (verbatim).
    if in_p.is_empty() {
        if !in_q.is_empty() && op == OpType::Add {
            return in_q.clone();
        }
        return Mesh::default();
    }
    if in_q.is_empty() {
        if op == OpType::Intersect {
            return Mesh::default();
        }
        return in_p.clone();
    }
    if !b3.valid {
        return Mesh::default(); // ResultTooLarge → empty
    }

    // Winding numbers → inclusion values.
    let c1 = if op == OpType::Intersect { 0 } else { 1 };
    let c2 = if op == OpType::Add { 1 } else { 0 };
    let c3 = if op == OpType::Intersect { 1 } else { -1 };
    let invert_q = op == OpType::Subtract;

    let i12: Vec<i32> = b3.xv12.x12.iter().map(|&v| c3 * v).collect();
    let i21: Vec<i32> = b3.xv21.x12.iter().map(|&v| c3 * v).collect();
    let i03: Vec<i32> = b3.w03.iter().map(|&v| c1 + c3 * v).collect();
    let i30: Vec<i32> = b3.w30.iter().map(|&v| c2 + c3 * v).collect();

    // Vertex remaps + total vertex count.
    let (v_p2r, num_vert_r) = exclusive_scan_abssum(&i03, 0);
    let n_pv = num_vert_r;
    let (v_q2r, num_vert_r) = exclusive_scan_abssum(&i30, num_vert_r);
    let n_qv = num_vert_r - n_pv;
    let (v12r, num_vert_r) = if b3.xv12.v12.is_empty() {
        (Vec::new(), num_vert_r)
    } else {
        exclusive_scan_abssum(&i12, num_vert_r)
    };
    let n12 = num_vert_r - n_pv - n_qv;
    let (v21r, num_vert_r) = if b3.xv21.v12.is_empty() {
        (Vec::new(), num_vert_r)
    } else {
        exclusive_scan_abssum(&i21, num_vert_r)
    };
    let _n21 = num_vert_r - n_pv - n_qv - n12;

    let mut out = Mesh::default();
    if num_vert_r == 0 {
        return out;
    }
    out.num_prop = 3;
    out.epsilon = fmax(in_p.epsilon, in_q.epsilon);
    out.tolerance = fmax(in_p.tolerance, in_q.tolerance);
    out.vert_pos = vec![Vec3::ZERO; num_vert_r as usize];

    // Duplicate/remap all vertices (retained P, retained Q, then the two new-vert sets).
    duplicate_verts(&mut out.vert_pos, &i03, &v_p2r, &in_p.vert_pos);
    duplicate_verts(&mut out.vert_pos, &i30, &v_q2r, &in_q.vert_pos);
    duplicate_verts(&mut out.vert_pos, &i12, &v12r, &b3.xv12.v12);
    duplicate_verts(&mut out.vert_pos, &i21, &v21r, &b3.xv21.v12);

    // Level 3: new-edge verts into the per-edge and per-face-pair maps.
    let mut edges_p: BTreeMap<i32, Vec<EdgePos>> = BTreeMap::new();
    let mut edges_q: BTreeMap<i32, Vec<EdgePos>> = BTreeMap::new();
    let mut edges_new: BTreeMap<(i32, i32), Vec<EdgePos>> = BTreeMap::new();
    add_new_edge_verts(&mut edges_p, &mut edges_new, &b3.xv12.p1q2, &i12, &v12r, in_p, true, 0);
    add_new_edge_verts(
        &mut edges_q,
        &mut edges_new,
        &b3.xv21.p1q2,
        &i21,
        &v21r,
        in_q,
        false,
        b3.xv12.p1q2.len(),
    );

    // Level 4: SizeOutput → per-face offsets + face remap + gathered face normals.
    let (face_edge, face_pq2r) = size_output(
        &mut out,
        in_p,
        in_q,
        &i03,
        &i30,
        &i12,
        &i21,
        &b3.xv12.p1q2,
        &b3.xv21.p1q2,
        invert_q,
    );

    let mut face_ptr_r = face_edge.clone();
    let mut whole_halfedge_p = vec![true; in_p.halfedge.len()];
    let mut whole_halfedge_q = vec![true; in_q.halfedge.len()];
    let total_he = *face_edge.last().unwrap() as usize;
    let mut face_halfedges = vec![
        VHalfedge {
            start_vert: -1,
            end_vert: -1,
            paired_halfedge: -1,
            prop_vert: -1,
        };
        total_he
    ];
    let num_tri_p = in_p.num_tri();

    // Partial (cut) edges.
    append_partial_edges(
        &out,
        &mut face_halfedges,
        &mut whole_halfedge_p,
        &mut face_ptr_r,
        &edges_p,
        in_p,
        &i03,
        &v_p2r,
        &face_pq2r[..],
    );
    append_partial_edges(
        &out,
        &mut face_halfedges,
        &mut whole_halfedge_q,
        &mut face_ptr_r,
        &edges_q,
        in_q,
        &i30,
        &v_q2r,
        &face_pq2r[num_tri_p..],
    );

    // New intersection edges.
    append_new_edges(&out, &mut face_halfedges, &mut face_ptr_r, &edges_new, &face_pq2r, num_tri_p);

    // Whole (uncut) edges.
    append_whole_edges(
        &mut face_halfedges,
        &mut face_ptr_r,
        in_p,
        &whole_halfedge_p,
        &i03,
        &v_p2r,
        &face_pq2r[..],
    );
    append_whole_edges(
        &mut face_halfedges,
        &mut face_ptr_r,
        in_q,
        &whole_halfedge_q,
        &i30,
        &v_q2r,
        &face_pq2r[num_tri_p..],
    );

    // Level 6: retriangulate the polygon faces into the final half-edge mesh.
    let epsilon = out.epsilon;
    face2tri(&mut out, &face_edge, &face_halfedges, epsilon);

    // Cleanup tail (GATE-A subset): compact dangling verts (keeps genus exact), recompute the bbox.
    out.remove_unreferenced_verts();
    out.calculate_bbox();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshGl;

    /// A unit cube at an offset, fully prepared for a boolean (halfedges, bbox, epsilon, both normals).
    fn cube(ox: f64, oy: f64, oz: f64) -> Mesh {
        #[rustfmt::skip]
        let base = [
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        let mut verts = Vec::new();
        for c in base.chunks_exact(3) {
            verts.push(c[0] + ox);
            verts.push(c[1] + oy);
            verts.push(c[2] + oz);
        }
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7,
            0,1,5, 0,5,4, 2,3,7, 2,7,6,
            0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mut mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        });
        mesh.set_epsilon(-1.0, false);
        mesh.calculate_face_normals();
        mesh.calculate_vert_normals();
        mesh
    }

    /// GATE-A in pure Rust (no oracle): an OFFSET (general-position) cube∪cube must produce a watertight,
    /// genus-0 solid of the analytic union volume. The offset (0.3,0.4,0.5) shares no coordinate between
    /// the meshes, so no cross-mesh `p == q` tie ever fires — the perturbation normals stay inert and the
    /// pure-f64 core is what's under test.
    #[test]
    fn offset_cube_union_is_watertight_genus0_analytic_volume() {
        let p = cube(0.0, 0.0, 0.0);
        let q = cube(0.3, 0.4, 0.5);
        let u = boolean(&p, &q, OpType::Add);

        assert!(!u.is_empty(), "union produced an empty mesh");
        assert!(u.is_manifold(), "union is not a watertight manifold");
        assert_eq!(crate::check::genus(&u), 0, "union should be genus 0");

        // Union volume = 2·1 − overlap; overlap = [0.3,1]×[0.4,1]×[0.5,1] = 0.7·0.6·0.5 = 0.21.
        let expected = 2.0 - 0.7 * 0.6 * 0.5;
        let v = u.volume();
        assert!((v - expected).abs() < 1e-9, "volume {v} != {expected}");
    }

    /// Disjoint cubes union to the two separate cubes: volume 2, genus 0, still manifold. Exercises the
    /// no-overlap early-out path in `Boolean3` feeding a clean assembly (all verts retained, no cuts).
    #[test]
    fn disjoint_cube_union_sums_volumes() {
        let p = cube(0.0, 0.0, 0.0);
        let q = cube(5.0, 5.0, 5.0);
        let u = boolean(&p, &q, OpType::Add);
        assert!(u.is_manifold());
        assert!((u.volume() - 2.0).abs() < 1e-12, "two unit cubes ⇒ volume 2");
        // Two disjoint closed surfaces: χ = 4, genus formula 1 − 4/2 = −1 (the single-component formula's
        // known behaviour on two components — a documented backstop limitation, not a bug here).
        assert_eq!(crate::check::euler_characteristic(&u), 4);
    }
}
