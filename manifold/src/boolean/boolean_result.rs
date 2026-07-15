//! `Boolean3::Result` — reassembling a watertight manifold from the four intersection tables
//! (`boolean_result.cpp`).
//!
//! Given [`Boolean3`]'s `xv12`/`xv21` (edge×face crossings) and `w03`/`w30` (per-vertex winding), this
//! builds the output mesh: winding → inclusion counts, vertex duplication + remapping, per-edge new
//! vertices, the polygon faces (`AppendPartialEdges`/`AppendNewEdges`/`AppendWholeEdges`), then
//! retriangulation ([`crate::boolean::face_op::face2tri`]). Ported from `Result()` and its helpers.
//!
//! TYPES vs the C++ `int` soup: remaps that map a source vertex to an OUTPUT vertex (`vP2R`/`v12R`/…)
//! are `Vec<VertId>`; the inclusion arrays (`i03`/`i12`/…) stay `Vec<i32>` — their values are winding
//! COUNTS, not ids. Faces key `edgesNew` as `(TriId, TriId)`, cut edges key `edgesP` as [`HalfedgeId`].
//! The per-result-face index (`facePQ2R`, `faceEdge`, `facePtrR`) is a local i32 space (offsets into the
//! `face_halfedges` buffer), left raw.
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
//! Container discipline: `edgesP`/`edgesQ`/`edgesNew` are `BTreeMap` NOT HashMap, because the iteration
//! order is load-bearing (it sequences the output half-edges written per face).

use std::collections::BTreeMap;

use crate::boolean::OpType;
use crate::boolean::boolean3::Boolean3;
use crate::boolean::face_op::face2tri;
use crate::boolean::predicates::{fmax, get_barycentric};
use crate::boolean::vocab::{Halfedge as VHalfedge, TriRef};
use crate::linalg::{Box3, Vec3};
use crate::mesh::{Mesh, mesh_id_counter};
use crate::mesh_ids::{HalfedgeId, TriId, VertId};

/// A temporary provenance ref written during assembly (`boolean_result.cpp`'s `{meshID, -1, faceID,
/// -1}`): `mesh_id` is `0` for a P-source half-edge, `1` for Q, and `face_id` is the SOURCE triangle
/// index in that input mesh. [`update_reference`] later swaps in the source triangle's REAL [`TriRef`].
#[inline]
fn temp_ref(mesh_id: i32, source_tri: TriId) -> TriRef {
    TriRef {
        mesh_id,
        original_id: -1,
        face_id: source_tri.raw(),
        coplanar_id: -1,
    }
}

/// A cut-edge point awaiting pairing (`boolean_result.cpp` `EdgePos`). Sorted by `(edge_pos,
/// collision_id)` — `collision_id` deterministically breaks position ties (the fix for the C++'s prior
/// nondeterminism).
#[derive(Clone, Copy, Debug)]
struct EdgePos {
    edge_pos: f64,
    vert: VertId,
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

/// `exclusive_scan` with the `AbsSum(a, b) = a + |b|` operator (`boolean_result.cpp`). Returns the
/// per-source-vertex remap (`out[k] = VertId(init + Σ_{j<k}|input[j]|)`) and the final accumulator
/// (`= AbsSum(out.back(), input.back())` — the running output-vertex count after this block).
fn exclusive_scan_abssum(input: &[i32], init: i32) -> (Vec<VertId>, i32) {
    let mut out = Vec::with_capacity(input.len());
    let mut acc = init;
    for &v in input {
        out.push(VertId::new(acc));
        acc += v.abs();
    }
    (out, acc)
}

/// Fill the output vertex positions, duplicating each source vert `|inclusion|` times starting at its
/// remap index (`boolean_result.cpp` `DuplicateVerts`). `inclusion`/`remap`/`src` are all indexed by the
/// source vertex; a `0`-inclusion vert writes nothing.
fn duplicate_verts(vert_pos_r: &mut [Vec3], inclusion: &[i32], remap: &[VertId], src: &[Vec3]) {
    for vert in 0..src.len() {
        let n = inclusion[vert].abs();
        for i in 0..n {
            vert_pos_r[remap[vert].offset(i).u()] = src[vert];
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
    edges_p: &mut BTreeMap<HalfedgeId, Vec<EdgePos>>,
    edges_new: &mut BTreeMap<(TriId, TriId), Vec<EdgePos>>,
    p1q2: &[[i32; 2]],
    i12: &[i32],
    v12r: &[VertId],
    halfedge_p: &Mesh,
    forward: bool,
    offset: usize,
) {
    for i in 0..p1q2.len() {
        let edge_p = HalfedgeId::new(p1q2[i][if forward { 0 } else { 1 }]);
        let face_q = TriId::new(p1q2[i][if forward { 1 } else { 0 }]);
        let vert = v12r[i];
        let inclusion = i12[i];

        let mut key_right = (halfedge_p.pair(edge_p).tri(), face_q);
        let mut key_left = (edge_p.tri(), face_q);
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
                vert: vert.offset(j),
                collision_id: cid,
                is_start: is_start[0],
            });
        }
        for j in 0..n {
            edges_new.entry(key_right).or_default().push(EdgePos {
                edge_pos: 0.0,
                vert: vert.offset(j),
                collision_id: cid,
                is_start: is_start[1],
            });
        }
        for j in 0..n {
            edges_new.entry(key_left).or_default().push(EdgePos {
                edge_pos: 0.0,
                vert: vert.offset(j),
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
            paired_halfedge: HalfedgeId::NONE,
            prop_vert: starts[i].vert,
        });
    }
}

/// The two-halfedge emit shared by `AppendPartialEdges`/`AppendNewEdges`: advance the write cursor for
/// the left and right result faces, write the forward edge to `face_left` and its reverse to
/// `face_right`, cross-linking their pairs. `face_left`/`face_right` are result-face indices; the write
/// cursors hold `face_halfedges` BUFFER indices.
#[allow(clippy::too_many_arguments)]
fn emit_paired(
    face_halfedges: &mut [VHalfedge],
    halfedge_ref: &mut [TriRef],
    face_ptr_r: &mut [i32],
    face_left: i32,
    face_right: i32,
    forward_ref: TriRef,
    backward_ref: TriRef,
    mut e: VHalfedge,
) {
    let forward_edge = face_ptr_r[face_left as usize];
    face_ptr_r[face_left as usize] += 1;
    let backward_edge = face_ptr_r[face_right as usize];
    face_ptr_r[face_right as usize] += 1;

    e.paired_halfedge = HalfedgeId::new(backward_edge);
    face_halfedges[forward_edge as usize] = e;
    halfedge_ref[forward_edge as usize] = forward_ref;

    core::mem::swap(&mut e.start_vert, &mut e.end_vert);
    e.paired_halfedge = HalfedgeId::new(forward_edge);
    face_halfedges[backward_edge as usize] = e;
    halfedge_ref[backward_edge as usize] = backward_ref;
}

/// Distribute the partially-retained edges to their faces (`boolean_result.cpp` `AppendPartialEdges`).
/// Each map entry is a cut edge: project its new verts and its retained endpoints onto the edge vector,
/// pair them up, and emit to the edge's two faces. Marks the whole-edge bitmap false for cut edges.
#[allow(clippy::too_many_arguments)]
fn append_partial_edges(
    out: &Mesh,
    face_halfedges: &mut [VHalfedge],
    halfedge_ref: &mut [TriRef],
    whole_halfedge_p: &mut [bool],
    face_ptr_r: &mut [i32],
    edges_p: &BTreeMap<HalfedgeId, Vec<EdgePos>>,
    in_p: &Mesh,
    i03: &[i32],
    v_p2r: &[VertId],
    face_p2r: &[i32],
    forward: bool,
) {
    let mesh_id = if forward { 0 } else { 1 };
    for (&edge_p, edge_pos_ref) in edges_p {
        let mut edge_pos_p = edge_pos_ref.clone();
        edge_pos_p.sort_by(EdgePos::order);

        let pair_p = in_p.pair(edge_p);
        whole_halfedge_p[edge_p.u()] = false;
        whole_halfedge_p[pair_p.u()] = false;

        let v_start = in_p.start(edge_p);
        let v_end = in_p.end(edge_p);
        let edge_vec = in_p.pos(v_end) - in_p.pos(v_start);

        for e in &mut edge_pos_p {
            e.edge_pos = out.pos(e.vert).dot(edge_vec);
        }

        let mut inclusion = i03[v_start.u()];
        let mut ep = EdgePos {
            edge_pos: out.pos(v_p2r[v_start.u()]).dot(edge_vec),
            vert: v_p2r[v_start.u()],
            collision_id: i32::MAX,
            is_start: inclusion > 0,
        };
        for _ in 0..inclusion.abs() {
            edge_pos_p.push(ep);
            ep.vert.advance();
        }

        inclusion = i03[v_end.u()];
        ep = EdgePos {
            edge_pos: out.pos(v_p2r[v_end.u()]).dot(edge_vec),
            vert: v_p2r[v_end.u()],
            collision_id: i32::MAX,
            is_start: inclusion < 0,
        };
        for _ in 0..inclusion.abs() {
            edge_pos_p.push(ep);
            ep.vert.advance();
        }

        let face_left = face_p2r[edge_p.tri().u()];
        let face_right = face_p2r[pair_p.tri().u()];

        // Both output half-edges of this cut edge belong to the same input triangle pair (faceLeftP =
        // edgeP/3, faceRightP = pairP/3), same mesh side.
        let forward_ref = temp_ref(mesh_id, edge_p.tri());
        let backward_ref = temp_ref(mesh_id, pair_p.tri());

        pair_up(&mut edge_pos_p, |e| {
            emit_paired(
                face_halfedges,
                halfedge_ref,
                face_ptr_r,
                face_left,
                face_right,
                forward_ref,
                backward_ref,
                e,
            )
        });
    }
}

/// Distribute the brand-new intersection edges to their `(faceP, faceQ)` face pair
/// (`boolean_result.cpp` `AppendNewEdges`). Orders each edge's verts along the longest bounding-box axis
/// before pairing.
#[allow(clippy::too_many_arguments)]
fn append_new_edges(
    out: &Mesh,
    face_halfedges: &mut [VHalfedge],
    halfedge_ref: &mut [TriRef],
    face_ptr_r: &mut [i32],
    edges_new: &BTreeMap<(TriId, TriId), Vec<EdgePos>>,
    face_pq2r: &[i32],
    num_face_p: usize,
) {
    for (&(face_p, face_q), edge_pos_ref) in edges_new {
        let mut edge_pos = edge_pos_ref.clone();
        edge_pos.sort_by(EdgePos::order);

        let mut bbox = Box3::default();
        for e in &edge_pos {
            bbox.union_point(out.pos(e.vert));
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
            e.edge_pos = component(out.pos(e.vert), dim);
        }

        let face_left = face_pq2r[face_p.u()];
        let face_right = face_pq2r[num_face_p + face_q.u()];

        // A brand-new intersection edge: forward side is the P face, backward the Q face.
        let forward_ref = temp_ref(0, face_p);
        let backward_ref = temp_ref(1, face_q);

        pair_up(&mut edge_pos, |e| {
            emit_paired(
                face_halfedges,
                halfedge_ref,
                face_ptr_r,
                face_left,
                face_right,
                forward_ref,
                backward_ref,
                e,
            )
        });
    }
}

/// Emit the whole (uncut) edges, duplicated per inclusion (`boolean_result.cpp` `DuplicateHalfedges` via
/// `AppendWholeEdges`). Each uncut edge is processed once from its forward half-edge, emitting both
/// output half-edges directly with their pairing.
#[allow(clippy::too_many_arguments)]
fn append_whole_edges(
    face_halfedges: &mut [VHalfedge],
    halfedge_ref: &mut [TriRef],
    face_ptr_r: &mut [i32],
    in_p: &Mesh,
    whole_halfedge_p: &[bool],
    i03: &[i32],
    v_p2r: &[VertId],
    face_p2r: &[i32],
    forward: bool,
) {
    let mesh_id = if forward { 0 } else { 1 };
    for idx in in_p.halfedge_ids() {
        if !whole_halfedge_p[idx.u()] {
            continue;
        }
        let mut start_vert = in_p.start(idx);
        let mut end_vert = in_p.end(idx);
        if start_vert >= end_vert {
            continue;
        }
        let inclusion = i03[start_vert.u()];
        if inclusion == 0 {
            continue;
        }
        if inclusion < 0 {
            core::mem::swap(&mut start_vert, &mut end_vert);
        }
        start_vert = v_p2r[start_vert.u()];
        end_vert = v_p2r[end_vert.u()];
        let prop_vert = in_p.prop(idx);
        let pair = in_p.pair(idx);
        let pair_prop_vert = in_p.prop(pair);
        let new_face = face_p2r[idx.tri().u()];
        let face_right = face_p2r[pair.tri().u()];

        // This uncut edge belongs to input triangle idx/3 on one side, pair/3 on the other.
        let forward_ref = temp_ref(mesh_id, idx.tri());
        let backward_ref = temp_ref(mesh_id, pair.tri());

        for _ in 0..inclusion.abs() {
            let forward_edge = face_ptr_r[new_face as usize];
            face_ptr_r[new_face as usize] += 1;
            let backward_edge = face_ptr_r[face_right as usize];
            face_ptr_r[face_right as usize] += 1;

            face_halfedges[forward_edge as usize] = VHalfedge {
                start_vert,
                end_vert,
                paired_halfedge: HalfedgeId::new(backward_edge),
                prop_vert,
            };
            halfedge_ref[forward_edge as usize] = forward_ref;
            face_halfedges[backward_edge as usize] = VHalfedge {
                start_vert: end_vert,
                end_vert: start_vert,
                paired_halfedge: HalfedgeId::new(forward_edge),
                prop_vert: pair_prop_vert,
            };
            halfedge_ref[backward_edge as usize] = backward_ref;
            start_vert.advance();
            end_vert.advance();
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
    for (i, side) in sides.iter_mut().enumerate().take(num_tri_p) {
        let t = TriId::from_usize(i);
        for j in 0..3 {
            *side += i03[in_p.start(t.halfedge(j)).u()].abs();
        }
    }
    for i in 0..num_tri_q {
        let t = TriId::from_usize(i);
        for j in 0..3 {
            sides[num_tri_p + i] += i30[in_q.start(t.halfedge(j)).u()].abs();
        }
    }

    // CountNewVerts<false> over p1q2: edgeP = pq[0] (P edge), faceQ = pq[1] (Q face).
    for idx in 0..p1q2.len() {
        let edge_p = HalfedgeId::new(p1q2[idx][0]);
        let face_q = p1q2[idx][1] as usize;
        let incl = i12[idx].abs();
        sides[num_tri_p + face_q] += incl;
        sides[edge_p.tri().u()] += incl;
        sides[in_p.pair(edge_p).tri().u()] += incl;
    }
    // CountNewVerts<true> over p2q1: edgeP = pq[1] (Q edge), faceQ = pq[0] (P face); counts swap roles.
    for idx in 0..p2q1.len() {
        let edge_q = HalfedgeId::new(p2q1[idx][1]);
        let face_p = p2q1[idx][0] as usize;
        let incl = i21[idx].abs();
        sides[face_p] += incl;
        sides[num_tri_p + edge_q.tri().u()] += incl;
        sides[num_tri_p + in_q.pair(edge_q).tri().u()] += incl;
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

/// `Next3`/`Prev3` (`utils.h`): the next / previous corner within a triangle (`(i+1)%3` / `(i+2)%3`).
#[inline]
fn next3(i: usize) -> usize {
    (i + 1) % 3
}
#[inline]
fn prev3(i: usize) -> usize {
    (i + 2) % 3
}

/// Component `i` (0=x,1=y,2=z) of a [`Vec3`] — C++ `vec3::operator[]`, which our `Vec3` lacks.
#[inline]
fn comp(v: Vec3, i: usize) -> f64 {
    match i {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// `CreateProperties` (`boolean_result.cpp:571-687`): barycentric-interpolate each input mesh's extra
/// properties (UV / colour / …) onto the boolean's output vertices, MANUFACTURING the decoupled
/// prop-verts (a fresh [`Mesh::properties`] row per distinct `(source-face, on-vert | on-edge | interior)`
/// key) so a coloured subtree keeps its properties across the seam. Reads `out.tri_ref` in its TEMP
/// `{0|1, srcTri}` form — so it MUST run after `face2tri`/`reorder_halfedges` but BEFORE
/// [`update_reference`] swaps in the real source refs (mirrors C++ Result 947→950). No-op when both
/// inputs are position-only (`num_prop == 0`).
///
/// DEVIATION (documented, LOUD): the `negateNormals` branch — which flips Q's world-frame vertex
/// NORMALS (properties slots 0..2) under `Subtract` — is hardwired OFF. It gates on
/// `Impl::TriHasNormals`, the per-mesh-instance `hasNormals` provenance our `Mesh` doesn't track. So
/// this is EXACT for colour / UV / any non-normal property (the fab-scad use case + the M.3.4b.6 gate),
/// and diverges ONLY for a mesh carrying world-frame vertex normals AS properties through
/// difference/intersection — its own box if that ever lands.
fn create_properties(out: &mut Mesh, in_p: &Mesh, in_q: &Mesh, _invert_q: bool) {
    let num_prop_p = in_p.num_prop;
    let num_prop_q = in_q.num_prop;
    let num_prop = num_prop_p.max(num_prop_q);
    out.num_prop = num_prop;
    if num_prop == 0 {
        return;
    }

    let num_tri = out.num_tri();
    // Barycentric coords per output half-edge (Manifold `bary`), against each corner's SOURCE triangle.
    let mut bary = vec![Vec3::ZERO; out.halfedge.len()];
    for tri in 0..num_tri {
        let ref_pq = out.tri_ref[tri];
        if out.start(HalfedgeId::from_usize(3 * tri)).is_none() {
            continue;
        }
        let tri_pq = ref_pq.face_id as usize;
        let pq = ref_pq.mesh_id == 0;
        let src = if pq { in_p } else { in_q };
        let mut tri_pos = [Vec3::ZERO; 3];
        for (j, tp) in tri_pos.iter_mut().enumerate() {
            *tp = src.pos(src.start(HalfedgeId::from_usize(3 * tri_pq + j)));
        }
        for i in 0..3 {
            let vert = out.start(HalfedgeId::from_usize(3 * tri + i));
            bary[3 * tri + i] = get_barycentric(out.pos(vert), tri_pos, out.epsilon);
        }
    }

    let id_miss_prop = out.num_vert() as i32;
    // Per output-vert bins of `((key.x, key.z, key.w), idx)`; the `+ 1` bin is `propIdx[idMissProp]`.
    let mut prop_idx: Vec<Vec<([i32; 3], i32)>> = vec![Vec::new(); out.num_vert() + 1];
    // `[0]` indexed by inQ prop-verts, `[1]` by inP prop-verts (mirrors C++'s swapped sizing).
    let mut prop_miss_idx: [Vec<i32>; 2] =
        [vec![-1; in_q.num_prop_vert()], vec![-1; in_p.num_prop_vert()]];

    let mut properties: Vec<f64> = Vec::with_capacity(out.num_vert() * num_prop);
    let mut idx: i32 = 0;

    for tri in 0..num_tri {
        // Skip collapsed triangles.
        if out.start(HalfedgeId::from_usize(3 * tri)).is_none() {
            continue;
        }
        let r = out.tri_ref[tri];
        let pq = r.mesh_id == 0;
        let old_num_prop = if pq { num_prop_p } else { num_prop_q };
        let src = if pq { in_p } else { in_q };
        let face_id = r.face_id as usize;

        for i in 0..3 {
            let he = HalfedgeId::from_usize(3 * tri + i);
            let vert = out.start(he);
            let uvw = bary[3 * tri + i];

            // ivec4 key(PQ, idMissProp, -1, -1).
            let mut key = [pq as i32, id_miss_prop, -1i32, -1i32];
            if old_num_prop > 0 {
                let mut edge: i32 = -2;
                for j in 0..3 {
                    if comp(uvw, j) == 1.0 {
                        // On a retained vert, the propVert must also match.
                        key[2] = src.prop(HalfedgeId::from_usize(3 * face_id + j)).raw();
                        edge = -1;
                        break;
                    }
                    if comp(uvw, j) == 0.0 {
                        edge = j as i32;
                    }
                }
                if edge >= 0 {
                    // On an edge, both propVerts must match.
                    let e = edge as usize;
                    let p0 = src.prop(HalfedgeId::from_usize(3 * face_id + next3(e))).raw();
                    let p1 = src.prop(HalfedgeId::from_usize(3 * face_id + prev3(e))).raw();
                    key[1] = vert.raw();
                    key[2] = p0.min(p1);
                    key[3] = p0.max(p1);
                } else if edge == -2 {
                    key[1] = vert.raw();
                }
            }

            if key[1] == id_miss_prop && key[2] >= 0 {
                // Only key.x/key.z matter.
                let entry = &mut prop_miss_idx[key[0] as usize][key[2] as usize];
                if *entry >= 0 {
                    out.set_prop(he, VertId::new(*entry));
                    continue;
                }
                *entry = idx;
            } else {
                let bin = &mut prop_idx[key[1] as usize];
                let mut b_found = false;
                for b in bin.iter() {
                    if b.0 == [key[0], key[2], key[3]] {
                        b_found = true;
                        out.set_prop(he, VertId::new(b.1));
                        break;
                    }
                }
                if b_found {
                    continue;
                }
                bin.push(([key[0], key[2], key[3]], idx));
            }

            out.set_prop(he, VertId::new(idx));
            idx += 1;
            for p in 0..num_prop {
                if p < old_num_prop {
                    let mut old_props = [0.0f64; 3];
                    for (j, op) in old_props.iter_mut().enumerate() {
                        let pv = src.prop(HalfedgeId::from_usize(3 * face_id + j)).u();
                        *op = src.properties[old_num_prop * pv + p];
                    }
                    // la::dot(uvw, oldProps). negateNormals branch omitted (see fn doc).
                    let val = uvw.dot(Vec3::new(old_props[0], old_props[1], old_props[2]));
                    properties.push(val);
                } else {
                    properties.push(0.0);
                }
            }
        }
    }

    out.properties = properties;
}

/// Map each output triangle's TEMPORARY provenance ref (`{0|1, srcTri}`) to the source triangle's real
/// [`TriRef`] (`boolean_result.cpp` `UpdateReference` + `MapTriRef`). `mesh_id == 0` reads P's `tri_ref`,
/// else Q's — with Q's `mesh_id` shifted up by `offsetQ = meshIDCounter` (every ID reserved so far), so
/// P- and Q-origin instance IDs never collide and [`TriRef::same_face`] correctly separates them. Only
/// `mesh_id` is offset (matching C++); `same_face` ignores `original_id`, so the rest carries verbatim.
fn update_reference(out: &mut Mesh, in_p: &Mesh, in_q: &Mesh) {
    let offset_q = mesh_id_counter();
    for r in &mut out.tri_ref {
        let src = r.face_id as usize;
        if r.mesh_id == 0 {
            *r = in_p.tri_ref[src];
        } else {
            let mut q = in_q.tri_ref[src];
            q.mesh_id += offset_q;
            *r = q;
        }
    }
}

/// Run a full boolean: the [`Boolean3`] intersection stage then the result assembly. This is the R1
/// tracer entry point (`Manifold::Impl::Boolean` + `Boolean3::Result` fused).
pub fn boolean(in_p: &Mesh, in_q: &Mesh, op: OpType) -> Mesh {
    let t = std::time::Instant::now();
    let b3 = Boolean3::new(in_p, in_q, op);
    tracing::debug!(target: "manifold::boolean", ms = t.elapsed().as_millis() as u64, "Boolean3::new (narrow phase)");
    result(&b3, in_p, in_q, op)
}

/// Split `in_p` by the closed cutter `in_q` (`Manifold::Split`): ONE `Boolean3(Subtract)` shared across
/// both extractions — `Result(Intersect)` is the piece INSIDE the cutter, `Result(Subtract)` the piece
/// OUTSIDE. Sharing the narrow phase (both ops need `expand_p == false`, since neither is `Add`) is the
/// C++ trick that halves the cut cost vs two independent booleans. The two results are exactly what a
/// separate `boolean(p, q, Intersect)` + `boolean(p, q, Subtract)` would produce.
pub fn split(in_p: &Mesh, in_q: &Mesh) -> (Mesh, Mesh) {
    let b3 = Boolean3::new(in_p, in_q, OpType::Subtract);
    let inside = result(&b3, in_p, in_q, OpType::Intersect);
    let outside = result(&b3, in_p, in_q, OpType::Subtract);
    (inside, outside)
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
    // Position-only output for now (`num_prop == 0` = C++ `numProp_`, no extras). `CreateProperties`
    // (M.3.4b.4) overwrites this with `max(numPropP, numPropQ)` and fills the decoupled `properties`.
    out.num_prop = 0;
    out.epsilon = fmax(in_p.epsilon, in_q.epsilon);
    out.tolerance = fmax(in_p.tolerance, in_q.tolerance);
    out.vert_pos = vec![Vec3::ZERO; num_vert_r as usize];

    // Duplicate/remap all vertices (retained P, retained Q, then the two new-vert sets).
    duplicate_verts(&mut out.vert_pos, &i03, &v_p2r, &in_p.vert_pos);
    duplicate_verts(&mut out.vert_pos, &i30, &v_q2r, &in_q.vert_pos);
    duplicate_verts(&mut out.vert_pos, &i12, &v12r, &b3.xv12.v12);
    duplicate_verts(&mut out.vert_pos, &i21, &v21r, &b3.xv21.v12);

    // Level 3: new-edge verts into the per-edge and per-face-pair maps.
    let mut edges_p: BTreeMap<HalfedgeId, Vec<EdgePos>> = BTreeMap::new();
    let mut edges_q: BTreeMap<HalfedgeId, Vec<EdgePos>> = BTreeMap::new();
    let mut edges_new: BTreeMap<(TriId, TriId), Vec<EdgePos>> = BTreeMap::new();
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
            start_vert: VertId::NONE,
            end_vert: VertId::NONE,
            paired_halfedge: HalfedgeId::NONE,
            prop_vert: VertId::NONE,
        };
        total_he
    ];
    // The temporary provenance ref per output half-edge (`{0|1, srcTri}`) — becomes `tri_ref` after
    // Face2Tri, then `update_reference` swaps in the real source refs.
    let mut halfedge_ref = vec![temp_ref(0, TriId::new(0)); total_he];
    let num_tri_p = in_p.num_tri();

    // Partial (cut) edges.
    append_partial_edges(
        &out,
        &mut face_halfedges,
        &mut halfedge_ref,
        &mut whole_halfedge_p,
        &mut face_ptr_r,
        &edges_p,
        in_p,
        &i03,
        &v_p2r,
        &face_pq2r[..],
        true,
    );
    append_partial_edges(
        &out,
        &mut face_halfedges,
        &mut halfedge_ref,
        &mut whole_halfedge_q,
        &mut face_ptr_r,
        &edges_q,
        in_q,
        &i30,
        &v_q2r,
        &face_pq2r[num_tri_p..],
        false,
    );

    // New intersection edges.
    append_new_edges(
        &out,
        &mut face_halfedges,
        &mut halfedge_ref,
        &mut face_ptr_r,
        &edges_new,
        &face_pq2r,
        num_tri_p,
    );

    // Whole (uncut) edges.
    append_whole_edges(
        &mut face_halfedges,
        &mut halfedge_ref,
        &mut face_ptr_r,
        in_p,
        &whole_halfedge_p,
        &i03,
        &v_p2r,
        &face_pq2r[..],
        true,
    );
    append_whole_edges(
        &mut face_halfedges,
        &mut halfedge_ref,
        &mut face_ptr_r,
        in_q,
        &whole_halfedge_q,
        &i30,
        &v_q2r,
        &face_pq2r[num_tri_p..],
        false,
    );

    // Level 6: retriangulate the polygon faces into the final half-edge mesh. `face2tri` leaves
    // `out.face_normal` per-TRIANGLE (each triangle carries its polygon face's normal) and `out.tri_ref`
    // the per-triangle TEMPORARY provenance ref — both read + carried by `simplify_topology`.
    let epsilon = out.epsilon;
    let t = std::time::Instant::now();
    face2tri(&mut out, &face_edge, &face_halfedges, &halfedge_ref, epsilon);
    tracing::debug!(target: "manifold::boolean", ms = t.elapsed().as_millis() as u64, tris = out.num_tri(), "face2tri (earclip)");

    // Canonicalize within-face half-edge order (Manifold runs this BEFORE SimplifyTopology) so the
    // collapse cascade visits edges in the same sequence C++ does — without it the surgery gets stuck at
    // a higher-genus fixed point on near-degenerate folds.
    out.reorder_halfedges();

    // Barycentric-interpolate the inputs' extra properties onto the output verts (Manifold Result 947),
    // manufacturing the decoupled prop-verts. MUST read `out.tri_ref` in its TEMP `{0|1, srcTri}` form,
    // so it runs BEFORE `update_reference` below. No-op when both inputs are position-only.
    create_properties(&mut out, in_p, in_q, invert_q);

    // Map each output triangle's temporary `{0|1, srcTri}` ref to the real source `TriRef`
    // (`triRefP/Q[srcTri]`, Q offset above P), so `simplify_topology`'s `CollapseColinearEdges` can read
    // the coplanar-face IDs.
    update_reference(&mut out, in_p, in_q);

    // R2 cleanup (`SimplifyTopology`): collapse the coincident/degenerate structure the intersection
    // assembly leaves at seams — turning the correct-but-unclean fold into an exact-genus manifold
    // WITHOUT moving the non-intersecting input geometry. New intersection verts begin at `n_pv + n_qv`
    // (retained P + retained Q); only those may collapse. Also compacts the marked-removed geometry (it
    // subsumes the old GATE-A `remove_unreferenced_verts`) and rebuilds the vertex normals.
    let t = std::time::Instant::now();
    crate::boolean::edge_op::simplify_topology(&mut out, n_pv + n_qv);
    tracing::debug!(target: "manifold::boolean", ms = t.elapsed().as_millis() as u64, "simplify_topology");

    let t = std::time::Instant::now();
    out.calculate_bbox();
    // Canonicalize vertex + triangle order by Morton code (Manifold's `SortGeometry`) — makes a CHAINED
    // op's intermediate byte-identical to C++'s, so a fold stays bit-identical instead of amplifying
    // order-dependent tie-breaks into a divergent result.
    out.sort_geometry();
    tracing::debug!(target: "manifold::boolean", ms = t.elapsed().as_millis() as u64, "bbox + sort_geometry");
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
            tri_verts: tris, ..Default::default()
        });
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.set_normals_and_coplanar();
        mesh
    }

    /// M.3.4b — CreateProperties end-to-end, self-checking (no C++). Colour each vertex of `A` by its
    /// own POSITION (`rgb = xyz`, `a = 1`); then `A − B` (B is the uncoloured cutter). Because colour was
    /// wired equal to position and the boolean interpolates BOTH barycentrically, every output corner
    /// must be EITHER all-zero (a corner from B, which had no properties) OR have `rgb ≈ its own position`
    /// with `a ≈ 1` (a corner from A, colour carried across the seam). Any corruption in
    /// create_properties / the prop maintenance through simplify+sort (mis-referenced prop-vert, stale
    /// `properties`, dropped row) breaks this invariant or the manifoldness.
    #[test]
    fn colored_cube_minus_cube_carries_position_as_color() {
        let a = cube(0.0, 0.0, 0.0)
            .set_properties(4, |new, pos, _old| new.copy_from_slice(&[pos.x, pos.y, pos.z, 1.0]));
        let b = cube(0.5, 0.5, 0.5);
        let out = boolean(&a, &b, OpType::Subtract);

        assert!(out.is_manifold(), "coloured difference must stay manifold");
        assert_eq!(out.num_prop, 4, "num_prop = max(4, 0)");
        assert!(!out.properties.is_empty(), "output must carry interpolated properties");
        assert_eq!(
            out.properties.len(),
            out.num_prop_vert() * 4,
            "properties length must be a whole number of prop-vert rows"
        );

        // Every corner: read (position, its 4 props) and check the invariant.
        let mut saw_colored = false;
        let mut saw_zero = false;
        for tri in 0..out.num_tri() {
            let t = TriId::from_usize(tri);
            for i in 0..3 {
                let he = t.halfedge(i);
                let pv = out.prop(he);
                assert!(pv.is_some() && pv.u() < out.num_prop_vert(), "prop-vert in range");
                let pos = out.pos(out.start(he));
                let row = &out.properties[pv.u() * 4..pv.u() * 4 + 4];
                let is_zero = row.iter().all(|&x| x == 0.0);
                if is_zero {
                    saw_zero = true;
                } else {
                    saw_colored = true;
                    assert!(
                        (row[0] - pos.x).abs() < 1e-6
                            && (row[1] - pos.y).abs() < 1e-6
                            && (row[2] - pos.z).abs() < 1e-6
                            && (row[3] - 1.0).abs() < 1e-6,
                        "A-corner colour must track its own position: row {row:?} vs pos {pos:?}"
                    );
                }
            }
        }
        assert!(saw_colored, "some corners keep A's colour (rgb = position)");
        assert!(saw_zero, "the cut faces from B carry zero properties");
    }

    /// M.3.4b.7 — a property-carrying boolean output SURVIVES a MeshGL serialization round-trip. The
    /// seam-split prop-verts become coincident interchange rows tagged by merge-vectors; re-import folds
    /// them back into a manifold with the properties intact. Then CHAINED: the re-imported coloured mesh
    /// feeds a FURTHER boolean and still carries colour. (Native chaining never round-trips, so this gates
    /// the SERIALIZATION path — save/load, cross-subsystem hand-off.)
    #[test]
    fn colored_output_survives_mesh_gl_round_trip() {
        let a = cube(0.0, 0.0, 0.0)
            .set_properties(4, |new, pos, _| new.copy_from_slice(&[pos.x, pos.y, pos.z, 1.0]));
        let b = cube(0.5, 0.5, 0.5);
        let out = boolean(&a, &b, OpType::Subtract);
        assert!(out.num_prop_vert() > out.num_vert(), "the seam must split some prop-verts");

        // Serialize (with merge-vectors) and re-import.
        let gl = out.to_mesh_gl();
        assert!(!gl.merge_from_vert.is_empty(), "a seam-split output must carry merge-vectors");
        let re = Mesh::from_mesh_gl(&gl);
        assert!(re.is_manifold(), "merge-vectors must re-share the seam into a manifold");
        assert!((re.volume() - out.volume()).abs() < 1e-9, "round-trip preserves volume");
        assert_eq!(re.num_prop, 4);
        assert_eq!(re.num_vert(), out.num_vert(), "geometric vert count preserved");

        // Colour still tracks position (or is zero from B) on every corner of the re-imported mesh.
        for tri in 0..re.num_tri() {
            let t = TriId::from_usize(tri);
            for i in 0..3 {
                let he = t.halfedge(i);
                let pos = re.pos(re.start(he));
                let row = &re.properties[re.prop(he).u() * 4..re.prop(he).u() * 4 + 4];
                let is_zero = row.iter().all(|&x| x == 0.0);
                assert!(
                    is_zero
                        || ((row[0] - pos.x).abs() < 1e-6
                            && (row[1] - pos.y).abs() < 1e-6
                            && (row[2] - pos.z).abs() < 1e-6
                            && (row[3] - 1.0).abs() < 1e-6),
                    "round-trip corrupted a colour: row {row:?} vs pos {pos:?}"
                );
            }
        }

        // Chained: re-prep the re-imported mesh and run a FURTHER boolean — colour carries on.
        let mut re = re;
        re.set_epsilon(-1.0, false);
        re.initialize_original();
        re.set_normals_and_coplanar();
        let c = cube(0.25, 0.25, 0.25);
        let chained = boolean(&re, &c, OpType::Subtract);
        assert!(chained.is_manifold(), "chained boolean on a re-imported coloured mesh must be manifold");
        assert_eq!(chained.num_prop, 4);
        assert!(!chained.properties.is_empty(), "chained output still carries colour");
    }

/// M.4 pull-forward — the deterministic PARALLEL narrow phase: a multi-cube fold must be BYTE-identical
    /// across two independent runs. With `--features par` this proves rayon SCHEDULING can't perturb the
    /// output: `intersect12`/`winding03` map over queries via `par::map_collect` (index-preserving) + the
    /// existing `stable_sort`, so thread interleaving is invisible. Serial (default) it's a trivial pass;
    /// the value is that the SAME test guards the parallel build.
    #[test]
    fn narrow_phase_is_run_to_run_deterministic() {
        use crate::boolean::OpType;
        fn fold() -> MeshGl {
            let offsets = [
                (0.0, 0.0, 0.0), (0.5, 0.3, 0.4), (0.2, 0.7, 0.1), (0.6, 0.1, 0.5), (0.3, 0.5, 0.8),
            ];
            let mut acc = cube(offsets[0].0, offsets[0].1, offsets[0].2);
            for &(ox, oy, oz) in &offsets[1..] {
                let c = cube(ox, oy, oz);
                acc = boolean(&acc, &c, OpType::Add);
                acc.set_epsilon(-1.0, false);
                acc.initialize_original();
                acc.set_normals_and_coplanar();
            }
            acc.to_mesh_gl()
        }
        let (a, b) = (fold(), fold());
        assert!(!a.tri_verts.is_empty(), "fold produced geometry");
        assert_eq!(a.tri_verts, b.tri_verts, "triangulation differs run-to-run");
        let bits = |m: &MeshGl| m.vert_properties.iter().map(|f| f.to_bits()).collect::<Vec<u64>>();
        assert_eq!(bits(&a), bits(&b), "vertex positions differ run-to-run (bitwise)");
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

    /// Build a prepared slab from a flat vert list + 0-based tri index list.
    fn prepared(verts: Vec<f64>, tris: Vec<u32>) -> Mesh {
        let mut mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris, ..Default::default()
        });
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.set_normals_and_coplanar();
        mesh
    }

    /// Regression (M.3.9): the coplanar-union INFINITE LOOP. Two axis-aligned slabs share the x=6.5 and
    /// z=6.5 planes with coincident/coplanar faces; before the `ring` re-anchor fix, `face2tri` mis-
    /// triangulated a self-touching degenerate seam face, dropped a triangle, and left an output half-edge
    /// UNPAIRED — which sent `for_vert` (in `split_pinched_verts`) walking off the NONE pair forever.
    /// Now it must terminate and produce a watertight manifold. Slab data captured verbatim from the
    /// minimized minkowski-sum repro (all coords integer±0.5).
    #[test]
    fn coplanar_slab_union_terminates_and_is_manifold() {
        use crate::boolean::OpType;
        #[rustfmt::skip]
        let a = prepared(
            vec![
                -0.5,-0.5,5.5, -0.5,0.5,5.5, -0.5,-0.5,6.5, -0.5,0.5,6.5,
                2.5,3.5,5.5, 2.5,3.5,6.5, 6.5,-0.5,5.5, 6.5,0.5,5.5,
                6.5,-0.5,6.5, 6.5,0.5,6.5, 3.5,3.5,5.5, 3.5,3.5,6.5,
            ],
            vec![
                0,3,1, 4,0,1, 6,2,0, 4,6,0, 0,2,3, 2,5,3, 2,9,5, 3,4,1,
                3,5,4, 4,5,11, 9,6,7, 6,4,7, 2,6,8, 9,2,8, 6,9,8, 7,4,10,
                4,11,10, 11,7,10, 11,5,9, 9,7,11,
            ],
        );
        #[rustfmt::skip]
        let b = prepared(
            vec![
                5.5,-0.5,-0.5, 5.5,0.5,-0.5, 5.5,-0.5,6.5, 5.5,0.5,6.5,
                5.5,3.5,2.5, 5.5,3.5,3.5, 6.5,-0.5,-0.5, 6.5,0.5,-0.5,
                6.5,-0.5,6.5, 6.5,0.5,6.5, 6.5,3.5,2.5, 6.5,3.5,3.5,
            ],
            vec![
                0,4,1, 2,4,0, 7,0,1, 6,2,0, 4,2,3, 2,9,3, 4,7,1, 4,3,5,
                3,11,5, 11,4,5, 6,0,7, 10,6,7, 6,10,9, 2,6,8, 9,2,8, 6,9,8,
                4,10,7, 11,10,4, 3,9,11, 9,10,11,
            ],
        );
        let u = boolean(&a, &b, OpType::Add);
        assert!(!u.is_empty(), "union produced an empty mesh");
        assert!(u.is_manifold(), "coplanar-slab union is not a watertight manifold");
        assert!(u.volume().is_finite() && u.volume() > 0.0, "union volume invalid");
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
