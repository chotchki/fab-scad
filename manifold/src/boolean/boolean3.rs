//! `boolean3` — the intersection core, the robustness heart of the whole port.
//!
//! Ported VERBATIM from `boolean3.cpp`. Given two manifolds P and Q it produces the FOUR tables that
//! the assembly (M.1.3) turns into a watertight result:
//! - `xv12` / `xv21` — the edge×face intersections of P-edges-vs-Q-faces and Q-edges-vs-P-faces, each a
//!   sparse `(p1q2, x12, v12)` = (index pair, winding-type value, intersection point).
//! - `w03` / `w30` — the winding number of every P-vertex inside Q, and every Q-vertex inside P.
//!
//! The cascade is `Shadow01 → Kernel02 → Kernel11 → Kernel12`, each an exact-`f64` shadow test resolved
//! at coordinate ties by the symbolic perturbation ([`crate::boolean::predicates::shadows`]). NO exact
//! arithmetic, NO FMA. The `expandP`/`forward` template params of the C++ are CONST GENERICS here,
//! matching the C++ monomorphization: [`intersect12`] and [`winding03`] match on the runtime bools once
//! and dispatch into one of the four fully-monomorphized cascade instantiations.
//!
//! SERIAL: the C++ recorder is `tbb::combinable`; we accumulate into one `Intersections` and, exactly
//! like the C++, `stable_sort` the pairs by edge afterward so the emit order is normalized away.
//!
//! GATE-A note: `Shadow01` reads `vertNormal_`/`faceNormal_` to form each `dir`, but `shadows` only
//! CONSULTS `dir` at an exact `p == q`. In the OFFSET (general-position) tracer no cross-mesh
//! coordinate ties fire, so the normals are computed-but-inert — which is the whole point of proving
//! the core there first. They still must be POPULATED (the reads are unconditional), and thanks to
//! `mathf::acos` they're already bit-exact, so the coincident case (GATE-B) needs nothing new here.

use crate::boolean::OpType;
use crate::boolean::collider::Collider;
use crate::boolean::disjoint_sets::DisjointSets;
use crate::boolean::predicates::{interpolate, intersect, shadows, with_sign};
use crate::boolean::vocab::Intersections;
use crate::linalg::{Box3, Vec2, Vec3, Vec4};
use crate::mesh::{Halfedge, Mesh};
use crate::mesh_ids::{HalfedgeId, TriId, VertId};

// ─── The validated view (BU.4.2) ────────────────────────────────────────────────────────────────
//
// The cascade below is the boolean's profile hot spot (~18 `shadow01` calls per candidate pair,
// 124M candidate pairs on the big_twin case), and every load in it is a slice index the C++ reads
// UNCHECKED (`VecView::operator[]` asserts only in debug). `MeshView` gives ONLY this module the
// same codegen, with the check moved from per-lookup to per-construction (chotchki's design):
// `validate` runs one O(halfedges) pass proving every id STORED in the tables in-bounds, so every
// id REACHABLE from them — a query edge in `0..len`, a collider leaf built over the same mesh,
// `pair`/`next`/`tri` of an in-bounds edge — is in-bounds with no per-load branch. Lookups take
// only the typed ids minted from these tables ([`VertId`]/[`HalfedgeId`]/[`TriId`], never a raw
// index), `debug_assert!` keeps every bound live in debug/test/fuzz builds, and a mesh that
// VIOLATES the invariant (a mid-surgery caller) panics at construction — the loud version of the
// guarantee the per-load checks used to give.

struct MeshView<'a> {
    vert_pos: &'a [Vec3],
    halfedge: &'a [Halfedge],
    vert_normal: &'a [Vec3],
    face_normal: &'a [Vec3],
}

impl<'a> MeshView<'a> {
    /// The O(halfedges) local validation — the whole soundness argument for the unchecked loads
    /// below. A `NONE` (-1) start/pair casts to a huge usize and fails the same comparison.
    fn validate(m: &'a Mesh) -> MeshView<'a> {
        let nv = m.vert_pos.len();
        let nh = m.halfedge.len();
        assert!(
            m.vert_normal.len() == nv && m.face_normal.len() == nh / 3,
            "MeshView: normals not sized to the mesh (vert {}/{nv}, face {}/{})",
            m.vert_normal.len(),
            m.face_normal.len(),
            nh / 3
        );
        for (i, h) in m.halfedge.iter().enumerate() {
            assert!(
                h.start_vert.u() < nv && h.paired_halfedge.u() < nh,
                "MeshView: halfedge {i} escapes its tables (start {:?}, pair {:?})",
                h.start_vert,
                h.paired_halfedge
            );
        }
        MeshView {
            vert_pos: &m.vert_pos,
            halfedge: &m.halfedge,
            vert_normal: &m.vert_normal,
            face_normal: &m.face_normal,
        }
    }

    #[inline(always)]
    fn num_vert(&self) -> usize {
        self.vert_pos.len()
    }

    #[inline(always)]
    fn num_halfedge(&self) -> usize {
        self.halfedge.len()
    }

    /// `Mesh::pos` without the per-load check.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn pos(&self, v: VertId) -> Vec3 {
        debug_assert!(v.u() < self.vert_pos.len());
        // SAFETY: `v` is a query vert (`0..num_vert`) or a `start_vert` proven in-bounds by `validate`.
        unsafe { *self.vert_pos.get_unchecked(v.u()) }
    }

    /// `Mesh::start` without the per-load check.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn start(&self, e: HalfedgeId) -> VertId {
        debug_assert!(e.u() < self.halfedge.len());
        // SAFETY: `e` is a query edge (`0..num_halfedge`), a leaf tri's edge (`3*tri + i` with the
        // leaf from this mesh's own collider), or a `pair`/`next` of one — all in-table, `validate`d.
        unsafe { self.halfedge.get_unchecked(e.u()) }.start_vert
    }

    /// `Mesh::end` (derived: start of the tri-local next edge — stays inside `e`'s own triangle).
    #[inline(always)]
    fn end(&self, e: HalfedgeId) -> VertId {
        self.start(e.next())
    }

    /// `Mesh::pair` without the per-load check.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn pair(&self, e: HalfedgeId) -> HalfedgeId {
        debug_assert!(e.u() < self.halfedge.len());
        // SAFETY: same id provenance as `start`.
        unsafe { self.halfedge.get_unchecked(e.u()) }.paired_halfedge
    }

    /// `vert_normal[v]` without the per-load check.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn vert_normal(&self, v: VertId) -> Vec3 {
        debug_assert!(v.u() < self.vert_normal.len());
        // SAFETY: `vert_normal.len() == num_vert` is `validate`d; `v`'s provenance as in `pos`.
        unsafe { *self.vert_normal.get_unchecked(v.u()) }
    }

    /// `face_normal[t]` without the per-load check.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn face_normal(&self, t: TriId) -> Vec3 {
        debug_assert!(t.u() < self.face_normal.len());
        // SAFETY: `face_normal.len() == num_tri` is `validate`d; `t` is a collider leaf over this
        // mesh or an in-bounds halfedge's own `tri()` (`e.u()/3 < num_tri`).
        unsafe { *self.face_normal.get_unchecked(t.u()) }
    }

    /// [`crate::boolean::collider::edge_query_box`], on the view (same values, unchecked loads).
    #[inline(always)]
    fn edge_box(&self, edge: HalfedgeId) -> Box3 {
        let start = self.start(edge);
        let end = self.end(edge);
        if start < end {
            Box3::from_points(self.pos(start), self.pos(end))
        } else {
            Box3::default()
        }
    }
}

/// One edge of a face, oriented forward, plus whether that matched the stored half-edge direction
/// (`boolean3.cpp` `FaceEdge`).
#[derive(Clone, Copy)]
struct FaceEdge {
    edge: HalfedgeId,
    start: VertId,
    end: VertId,
    is_forward: bool,
}

/// The three forward edges of triangle `tri` (`LoadFaceEdges`): each is stored forward if `start <
/// end`, else it borrows its pair's index and swapped endpoints.
fn load_face_edges(mesh: &MeshView<'_>, tri: TriId) -> [FaceEdge; 3] {
    core::array::from_fn(|i| {
        let halfedge = tri.halfedge(i);
        let start = mesh.start(halfedge);
        let end = mesh.end(halfedge); // == Start(Next3(halfedge))
        if start < end {
            FaceEdge {
                edge: halfedge,
                start,
                end,
                is_forward: true,
            }
        } else {
            FaceEdge {
                edge: mesh.pair(halfedge),
                start: end,
                end: start,
                is_forward: false,
            }
        }
    })
}

/// `Shadow01` — does vertex `a0` (of `in_a`) shadow edge `b1` (of `in_b`, endpoints `b1s`/`b1e`), and
/// where in `(y, z)` (`boolean3.cpp` `Shadow01`)? Returns `(s01, yz01)`; `yz01.x` NaN means no overlap.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn shadow01<const EXPAND_P: bool, const FORWARD: bool>(
    a0: VertId,
    b1: HalfedgeId,
    b1s: VertId,
    b1e: VertId,
    in_a: &MeshView<'_>,
    in_b: &MeshView<'_>,
) -> (i32, Vec2) {
    let a0x = in_a.pos(a0).x;
    let b1sx = in_b.pos(b1s).x;
    let b1ex = in_b.pos(b1e).x;
    let a0xp = in_a.vert_normal(a0).x;
    let b1sxp = in_b.vert_normal(b1s).x;
    let b1exp = in_b.vert_normal(b1e).x;
    let mut s01 = if FORWARD {
        shadows(a0x, b1ex, with_sign(EXPAND_P, a0xp) - b1exp) as i32
            - shadows(a0x, b1sx, with_sign(EXPAND_P, a0xp) - b1sxp) as i32
    } else {
        shadows(b1sx, a0x, with_sign(EXPAND_P, b1sxp) - a0xp) as i32
            - shadows(b1ex, a0x, with_sign(EXPAND_P, b1exp) - a0xp) as i32
    };
    let mut yz01 = Vec2::new(f64::NAN, f64::NAN);

    if s01 != 0 {
        yz01 = interpolate(in_b.pos(b1s), in_b.pos(b1e), in_a.pos(a0).x);
        let b1pair = in_b.pair(b1);
        let dir = in_b.face_normal(b1.tri()).y + in_b.face_normal(b1pair.tri()).y;
        if FORWARD {
            if !shadows(in_a.pos(a0).y, yz01.x, -dir) {
                s01 = 0;
            }
        } else if !shadows(yz01.x, in_a.pos(a0).y, with_sign(EXPAND_P, dir)) {
            s01 = 0;
        }
    }
    (s01, yz01)
}

/// `Kernel11` — the edge×edge shadow in the P/Q frame (`boolean3.cpp` `Kernel11`). Returns `(s11,
/// xyzz11)`; `xyzz11.x` NaN means no intersection. Always uses the ORIGINAL `in_p`/`in_q` (its callers
/// map their `a`/`b` edges into the right P/Q slot before calling).
#[allow(clippy::too_many_arguments)]
#[inline]
fn kernel11<const EXPAND_P: bool>(
    p1: HalfedgeId,
    p1s: VertId,
    p1e: VertId,
    q1: HalfedgeId,
    q1s: VertId,
    q1e: VertId,
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
) -> (i32, Vec4) {
    let mut s11 = 0;
    let mut k = 0usize;
    let mut p_rl = [Vec3::ZERO; 2];
    let mut q_rl = [Vec3::ZERO; 2];
    let mut shadows_flag = false;

    for (i, &p0i) in [p1s, p1e].iter().enumerate() {
        let (s01, yz01) = shadow01::<EXPAND_P, true>(p0i, q1, q1s, q1e, in_p, in_q);
        if yz01.x.is_finite() {
            s11 += s01 * if i == 0 { -1 } else { 1 };
            if k < 2 && (k == 0 || (s01 != 0) != shadows_flag) {
                shadows_flag = s01 != 0;
                p_rl[k] = in_p.pos(p0i);
                q_rl[k] = Vec3::new(p_rl[k].x, yz01.x, yz01.y);
                k += 1;
            }
        }
    }

    for (i, &q0i) in [q1s, q1e].iter().enumerate() {
        let (s10, yz10) = shadow01::<EXPAND_P, false>(q0i, p1, p1s, p1e, in_q, in_p);
        if yz10.x.is_finite() {
            s11 += s10 * if i == 0 { -1 } else { 1 };
            if k < 2 && (k == 0 || (s10 != 0) != shadows_flag) {
                shadows_flag = s10 != 0;
                q_rl[k] = in_q.pos(q0i);
                p_rl[k] = Vec3::new(q_rl[k].x, yz10.x, yz10.y);
                k += 1;
            }
        }
    }

    if s11 == 0 {
        return (0, Vec4::splat(f64::NAN));
    }
    debug_assert!(k == 2, "Boolean manifold error: s11");
    // xyzz11 keeps its value even when the shadow check zeroes s11 — Kernel12 still reads the point
    // (a finite xyzz[0]) to record the shadow boundary, gating only the winding sum on s11.
    let xyzz11 = intersect(p_rl[0], p_rl[1], q_rl[0], q_rl[1]);
    let p1pair = in_p.pair(p1);
    let dir_p = in_p.face_normal(p1.tri()).z + in_p.face_normal(p1pair.tri()).z;
    let q1pair = in_q.pair(q1);
    let dir_q = in_q.face_normal(q1.tri()).z + in_q.face_normal(q1pair.tri()).z;
    if !shadows(xyzz11.z, xyzz11.w, with_sign(EXPAND_P, dir_p) - dir_q) {
        s11 = 0;
    }
    (s11, xyzz11)
}

/// `Kernel02` — does vertex `a0` shadow face `b2` (with edges `edge_b`), and at what `z`
/// (`boolean3.cpp` `Kernel02`)? Returns `(s02, z02)`; `z02` NaN means no intersection.
#[allow(clippy::too_many_arguments)]
#[inline]
fn kernel02<const EXPAND_P: bool, const FORWARD: bool>(
    a0: VertId,
    b2: TriId,
    edge_b: &[FaceEdge; 3],
    in_a: &MeshView<'_>,
    in_b: &MeshView<'_>,
) -> (i32, f64) {
    let mut s02 = 0;
    let mut k = 0usize;
    let mut yzz_rl = [Vec3::ZERO; 2];
    let mut shadows_flag = false;

    for e in edge_b.iter() {
        let (s01, yz01) = shadow01::<EXPAND_P, FORWARD>(a0, e.edge, e.start, e.end, in_a, in_b);
        if yz01.x.is_finite() {
            s02 += s01 * if FORWARD == e.is_forward { -1 } else { 1 };
            if k < 2 && (k == 0 || (s01 != 0) != shadows_flag) {
                shadows_flag = s01 != 0;
                yzz_rl[k] = Vec3::new(yz01.x, yz01.y, yz01.y);
                k += 1;
            }
        }
    }

    if s02 == 0 {
        return (0, f64::NAN);
    }
    debug_assert!(k == 2, "Boolean manifold error: s02");
    let vert_pos_a = in_a.pos(a0);
    let z02 = interpolate(yzz_rl[0], yzz_rl[1], vert_pos_a.y).y;
    let keep = if FORWARD {
        shadows(vert_pos_a.z, z02, -in_b.face_normal(b2).z)
    } else {
        shadows(
            z02,
            vert_pos_a.z,
            with_sign(EXPAND_P, in_b.face_normal(b2).z),
        )
    };
    (if keep { s02 } else { 0 }, z02)
}

/// `Kernel12` — does edge `a1` (of `in_a`) pass through face `b2` (of `in_b`), and where
/// (`boolean3.cpp` `Kernel12`)? Returns `(x12, v12)`; `v12.x` NaN means no intersection. Combines the
/// two-endpoint `Kernel02` contributions with the three-edge `Kernel11` contributions.
#[inline]
fn kernel12<const EXPAND_P: bool, const FORWARD: bool>(
    a1: HalfedgeId,
    b2: TriId,
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
) -> (i32, Vec3) {
    let (in_a, in_b) = if FORWARD { (in_p, in_q) } else { (in_q, in_p) };
    let mut x12 = 0;
    let mut k = 0usize;
    let mut xzy_lr0 = [Vec3::ZERO; 2];
    let mut xzy_lr1 = [Vec3::ZERO; 2];
    let mut shadows_flag = false;

    let edge_a_start = in_a.start(a1);
    let edge_a_end = in_a.end(a1);
    let edge_b = load_face_edges(in_b, b2);

    for &vert_a in &[edge_a_start, edge_a_end] {
        let (s, z) = kernel02::<EXPAND_P, FORWARD>(vert_a, b2, &edge_b, in_a, in_b);
        if z.is_finite() {
            x12 += s * if (vert_a == edge_a_start) == FORWARD {
                1
            } else {
                -1
            };
            if k < 2 && (k == 0 || (s != 0) != shadows_flag) {
                shadows_flag = s != 0;
                let mut v = in_a.pos(vert_a);
                core::mem::swap(&mut v.y, &mut v.z);
                xzy_lr0[k] = v;
                xzy_lr1[k] = v;
                xzy_lr1[k].y = z;
                k += 1;
            }
        }
    }

    for e in edge_b.iter() {
        let (s, xyzz) = if FORWARD {
            kernel11::<EXPAND_P>(
                a1,
                edge_a_start,
                edge_a_end,
                e.edge,
                e.start,
                e.end,
                in_p,
                in_q,
            )
        } else {
            kernel11::<EXPAND_P>(
                e.edge,
                e.start,
                e.end,
                a1,
                edge_a_start,
                edge_a_end,
                in_p,
                in_q,
            )
        };
        if xyzz.x.is_finite() {
            x12 -= s * if e.is_forward { 1 } else { -1 };
            if k < 2 && (k == 0 || (s != 0) != shadows_flag) {
                shadows_flag = s != 0;
                let mut lo = Vec3::new(xyzz.x, xyzz.z, xyzz.y);
                let mut hi = lo;
                hi.y = xyzz.w;
                if !FORWARD {
                    core::mem::swap(&mut lo.y, &mut hi.y);
                }
                xzy_lr0[k] = lo;
                xzy_lr1[k] = hi;
                k += 1;
            }
        }
    }

    if x12 == 0 {
        return (0, Vec3::splat(f64::NAN));
    }
    debug_assert!(k == 2, "Boolean manifold error: v12");
    let xzyy = intersect(xzy_lr0[0], xzy_lr0[1], xzy_lr1[0], xzy_lr1[1]);
    (x12, Vec3::new(xzyy.x, xzyy.z, xzyy.y))
}

/// One edge×face intersection hit produced by [`intersect12`]'s per-query map. Named fields (not a bare
/// tuple) so the flatten below can't transpose the shadow count with the vertex. Flattened +
/// `stable_sort`ed into the sparse [`Intersections`] tables.
struct Hit12 {
    /// The `[p-tri, q-tri]` face-pair packing, matching [`Intersections::p1q2`].
    pair: [i32; 2],
    /// The shadow (winding) count — the `x12` column.
    shadow: i32,
    /// The intersection vertex — the `v12` column.
    vert: Vec3,
}

/// One query's (one edge's) contribution to [`intersect12`]: its hits plus the raw box-overlap count
/// (the `cand_pairs` diagnostic). Returned per query by the deterministic `par::map_collect`.
struct QueryHits {
    hits: Vec<Hit12>,
    overlaps: u64,
}

/// `Intersect12` — every edge×face crossing in one direction (`boolean3.cpp` `Intersect12_`). `forward`
/// picks P-edges×Q-faces (`true`) or Q-edges×P-faces (`false`); `b_collider` is the OTHER mesh's
/// face-box collider. Emits the sparse `(p1q2, x12, v12)`, `stable_sort`ed by the edge column so the
/// order is deterministic and collider-order-independent. The collider's raw `i32` indices are wrapped
/// into ids at the callback: a query is an edge ([`HalfedgeId`]), a leaf is a face ([`TriId`]).
fn intersect12(
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
    b_collider: &Collider,
    expand_p: bool,
    forward: bool,
) -> Intersections {
    // The one runtime→compile-time dispatch: everything below (the BVH traversal closure and the whole
    // Shadow01→Kernel02/11→Kernel12 cascade) is monomorphized per (EXPAND_P, FORWARD), as in the C++.
    match (expand_p, forward) {
        (true, true) => intersect12_impl::<true, true>(in_p, in_q, b_collider),
        (true, false) => intersect12_impl::<true, false>(in_p, in_q, b_collider),
        (false, true) => intersect12_impl::<false, true>(in_p, in_q, b_collider),
        (false, false) => intersect12_impl::<false, false>(in_p, in_q, b_collider),
    }
}

fn intersect12_impl<const EXPAND_P: bool, const FORWARD: bool>(
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
    b_collider: &Collider,
) -> Intersections {
    let a = if FORWARD { in_p } else { in_q };
    let t = std::time::Instant::now();

    // Map each edge (query) to its intersection hits, INDEPENDENTLY — the per-query BVH traversal is a
    // pure read + `kernel12` is a pure fn, so this is a deterministic `par::map_collect` (parallel with
    // `par` on, serial otherwise; output order is index-preserved either way). Bit-identity holds: the
    // flatten below reproduces the serial traversal order, and the `stable_sort` finalizes it regardless.
    let queries: Vec<i32> = (0..a.num_halfedge() as i32).collect();
    let per_query: Vec<QueryHits> = crate::par::map_collect(&queries, |&i| {
        let q = a.edge_box(HalfedgeId::new(i));
        let mut hits = Vec::new();
        let mut overlaps = 0u64;
        b_collider.query_leaves(i, q, false, |leaf_idx| {
            overlaps += 1;
            let (x12, v12) =
                kernel12::<EXPAND_P, FORWARD>(HalfedgeId::new(i), TriId::new(leaf_idx), in_p, in_q);
            if v12.x.is_finite() {
                let pair = if FORWARD {
                    [i, leaf_idx]
                } else {
                    [leaf_idx, i]
                };
                hits.push(Hit12 {
                    pair,
                    shadow: x12,
                    vert: v12,
                });
            }
        });
        QueryHits { hits, overlaps }
    });

    let mut result = Intersections::default();
    let mut n_pairs = 0u64;
    for qh in &per_query {
        n_pairs += qh.overlaps;
        for h in &qh.hits {
            result.p1q2.push(h.pair);
            result.x12.push(h.shadow);
            result.v12.push(h.vert);
        }
    }
    tracing::debug!(target: "manifold::boolean", forward = FORWARD, ms = t.elapsed().as_millis() as u64, cand_pairs = n_pairs, hits = result.p1q2.len(), "intersect12");

    // Sort by the edge column (`index`), then the other, exactly as the C++ stable_sort comparator.
    // Each (edge, face) pair is unique, so the key is total and the permutation is deterministic.
    let index = if FORWARD { 0 } else { 1 };
    let mut order: Vec<usize> = (0..result.p1q2.len()).collect();
    order.sort_by(|&a, &b| {
        let ka = (result.p1q2[a][index], result.p1q2[a][1 - index]);
        let kb = (result.p1q2[b][index], result.p1q2[b][1 - index]);
        ka.cmp(&kb)
    });
    Intersections {
        p1q2: order.iter().map(|&i| result.p1q2[i]).collect(),
        x12: order.iter().map(|&i| result.x12[i]).collect(),
        v12: order.iter().map(|&i| result.v12[i]).collect(),
    }
}

/// Is `edge` present in the (edge-column-sorted) `p1q2` — i.e. is it "broken" by an intersection?
/// Binary search on the primary key, matching the C++ `lower_bound`. `p1q2` is the raw `[i32; 2]`
/// packing, so `edge` is passed as its raw `i32`.
fn edge_is_broken(p1q2: &[[i32; 2]], index: usize, edge: i32) -> bool {
    p1q2.binary_search_by(|pair| pair[index].cmp(&edge)).is_ok()
}

/// One representative vertex's winding contribution from [`winding03`]'s per-rep map — named so the
/// scatter can't confuse the winding with the box-overlap diagnostic count.
struct RepWinding {
    /// The summed Kernel02 shadow contributions at this representative (integer ⇒ order-independent).
    winding: i32,
    /// Raw box-overlap count for the `cand_pairs` diagnostic.
    overlaps: u64,
}

/// `Winding03` — the winding number of every vertex of one mesh inside the other (`boolean3.cpp`
/// `Winding03_`). Verts on the same connected component (bounded by the intersection curve `p1q2`)
/// share a winding number, so we union-find the intact edges, sample the winding once per component via
/// a `Kernel02` point-in-mesh query, and flood-fill the rest.
fn winding03(
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
    p1q2: &[[i32; 2]],
    b_collider: &Collider,
    expand_p: bool,
    forward: bool,
) -> Vec<i32> {
    // Same single dispatch as `intersect12` — the point-in-mesh sampling rides the same Kernel02.
    match (expand_p, forward) {
        (true, true) => winding03_impl::<true, true>(in_p, in_q, p1q2, b_collider),
        (true, false) => winding03_impl::<true, false>(in_p, in_q, p1q2, b_collider),
        (false, true) => winding03_impl::<false, true>(in_p, in_q, p1q2, b_collider),
        (false, false) => winding03_impl::<false, false>(in_p, in_q, p1q2, b_collider),
    }
}

fn winding03_impl<const EXPAND_P: bool, const FORWARD: bool>(
    in_p: &MeshView<'_>,
    in_q: &MeshView<'_>,
    p1q2: &[[i32; 2]],
    b_collider: &Collider,
) -> Vec<i32> {
    let (a, b) = if FORWARD { (in_p, in_q) } else { (in_q, in_p) };
    let index = if FORWARD { 0 } else { 1 };

    // Union the endpoints of every intact (non-intersected) forward edge of `a`.
    let mut u_a = DisjointSets::new(a.num_vert());
    for edge in (0..a.num_halfedge() as i32).map(HalfedgeId::new) {
        let start = a.start(edge);
        let end = a.end(edge);
        if start >= end {
            continue;
        }
        if !edge_is_broken(p1q2, index, edge.raw()) {
            u_a.unite(start.u(), end.u());
        }
    }

    // Collect one representative per component, in deterministic ascending-first-seen order (the C++
    // uses an unordered_set, but the per-root winding is an INTEGER sum so order can't matter).
    let mut seen = vec![false; a.num_vert()];
    let mut verts = Vec::new();
    for v in 0..a.num_vert() {
        let root = u_a.find(v);
        if !seen[root] {
            seen[root] = true;
            verts.push(root);
        }
    }

    // Sample the winding at each representative: an XY-projected point-in-face query, summing the
    // Kernel02 shadow contributions. Each representative is a DISTINCT vert with its own winding sum, and
    // the sum is INTEGER (order-independent) — so this maps deterministically (`par::map_collect`, parallel
    // with `par` on). The per-rep contribution is scattered back afterward, no cross-thread write.
    let mut w03 = vec![0i32; a.num_vert()];
    let t = std::time::Instant::now();
    let sign = if FORWARD { 1 } else { -1 };
    let reps: Vec<usize> = (0..verts.len()).collect();
    let contrib: Vec<RepWinding> = crate::par::map_collect(&reps, |&i| {
        let vert = VertId::from_usize(verts[i]);
        let q = a.pos(vert);
        let mut winding = 0i32;
        let mut overlaps = 0u64;
        b_collider.query_leaves(i as i32, q, false, |face| {
            overlaps += 1;
            let tri = TriId::new(face);
            let edge_b = load_face_edges(b, tri);
            let (s02, z02) = kernel02::<EXPAND_P, FORWARD>(vert, tri, &edge_b, a, b);
            if z02.is_finite() {
                winding += s02 * sign;
            }
        });
        RepWinding { winding, overlaps }
    });
    let mut n_pairs = 0u64;
    for (i, rw) in contrib.iter().enumerate() {
        n_pairs += rw.overlaps;
        w03[verts[i]] = rw.winding; // each rep is a distinct root vert → assignment, not accumulation
    }
    tracing::debug!(target: "manifold::boolean", forward = FORWARD, ms = t.elapsed().as_millis() as u64, cand_pairs = n_pairs, reps = verts.len(), "winding03");

    // Flood the representative's winding to the rest of its component.
    for i in 0..a.num_vert() {
        let root = u_a.find(i);
        if root != i {
            w03[i] = w03[root];
        }
    }
    w03
}

/// The intersection stage of a boolean (`boolean3.cpp` `Boolean3`). Holds the four tables that
/// `boolean_result` (M.1.3) assembles into a watertight manifold. Requires `face_normal` + `vert_normal`
/// + `b_box` to be computed on BOTH inputs (the kernels read them unconditionally).
#[derive(Clone, Debug)]
pub struct Boolean3 {
    /// P-edge × Q-face intersections.
    pub xv12: Intersections,
    /// Q-edge × P-face intersections.
    pub xv21: Intersections,
    /// Winding number of each P-vertex inside Q.
    pub w03: Vec<i32>,
    /// Winding number of each Q-vertex inside P.
    pub w30: Vec<i32>,
    /// `op == Add` — the symbolic-perturbation direction (union expands both inputs).
    pub expand_p: bool,
    /// `false` if the intersection overflowed `i32` (an over-large model); the result is unusable.
    pub valid: bool,
}

impl Boolean3 {
    /// Run the intersection cascade for `in_p op in_q`. On no-overlap (empty input or disjoint bounding
    /// boxes) every vertex winds to 0 and no intersections are recorded — the early-out `Boolean3`.
    pub fn new(in_p: &Mesh, in_q: &Mesh, op: OpType) -> Self {
        let expand_p = op == OpType::Add;

        if in_p.is_empty() || in_q.is_empty() || !in_p.b_box.overlaps(in_q.b_box) {
            return Self {
                xv12: Intersections::default(),
                xv21: Intersections::default(),
                w03: vec![0; in_p.num_vert()],
                w30: vec![0; in_q.num_vert()],
                expand_p,
                valid: true,
            };
        }

        // The BU.4.2 validation gate — FIRST touch of the tables: one O(halfedges) pass per input
        // proves them closed, and everything downstream (the collider build + the whole cascade)
        // reads them unchecked through these views.
        let vp = MeshView::validate(in_p);
        let vq = MeshView::validate(in_q);

        // Each mesh's face-box collider is queried by the OTHER mesh's edges/verts.
        let collider_p = Collider::from_mesh(in_p);
        let collider_q = Collider::from_mesh(in_q);

        let xv12 = intersect12(&vp, &vq, &collider_q, expand_p, true);
        let xv21 = intersect12(&vp, &vq, &collider_p, expand_p, false);

        // `i32` overflow guard (the C++ INT_MAX_SZ check): an intersection set this large is unusable.
        if xv12.x12.len() > i32::MAX as usize || xv21.x12.len() > i32::MAX as usize {
            return Self {
                xv12,
                xv21,
                w03: Vec::new(),
                w30: Vec::new(),
                expand_p,
                valid: false,
            };
        }

        let w03 = winding03(&vp, &vq, &xv12.p1q2, &collider_q, expand_p, true);
        let w30 = winding03(&vp, &vq, &xv21.p1q2, &collider_p, expand_p, false);

        Self {
            xv12,
            xv21,
            w03,
            w30,
            expand_p,
            valid: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshGl;

    /// A unit cube at an offset, fully prepared for a boolean (halfedges, bbox, both normal fields).
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
        // Ingest runs the full ctor tail now (M.2.4a) — epsilon + normals included.
        Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn disjoint_cubes_early_out_all_zero() {
        let p = cube(0.0, 0.0, 0.0);
        let q = cube(10.0, 10.0, 10.0); // bounding boxes don't overlap
        let b = Boolean3::new(&p, &q, OpType::Add);
        assert!(b.valid);
        assert!(b.xv12.p1q2.is_empty() && b.xv21.p1q2.is_empty());
        assert_eq!(b.w03, vec![0; 8]);
        assert_eq!(b.w30, vec![0; 8]);
    }

    #[test]
    fn offset_cubes_produce_intersections_and_windings() {
        // The GATE-A configuration in miniature: a general-position offset overlap. The two cubes
        // interpenetrate on the (0.3,0.4,0.5) corner, so edges cross faces and some verts wind inside.
        let p = cube(0.0, 0.0, 0.0);
        let q = cube(0.3, 0.4, 0.5);
        let b = Boolean3::new(&p, &q, OpType::Add);
        assert!(b.valid);

        // Real edge×face crossings in both directions.
        assert!(!b.xv12.p1q2.is_empty(), "P-edges must cross Q-faces");
        assert!(!b.xv21.p1q2.is_empty(), "Q-edges must cross P-faces");
        // Every recorded intersection point is finite, and the tables stay parallel.
        assert_eq!(b.xv12.p1q2.len(), b.xv12.x12.len());
        assert_eq!(b.xv12.x12.len(), b.xv12.v12.len());
        assert!(b.xv12.v12.iter().all(|v| v.is_finite()));

        // p1q2 is sorted ascending by its edge column (the invariant winding03 binary-searches on).
        assert!(b.xv12.p1q2.windows(2).all(|w| w[0][0] <= w[1][0]));
        assert!(b.xv21.p1q2.windows(2).all(|w| w[1][1] >= w[0][1]));

        // P's far corner (0,0,0) is outside Q ⇒ winding 0; the shared corner region winds inside.
        assert_eq!(b.w03.len(), 8);
        assert_eq!(b.w30.len(), 8);
        assert_eq!(b.w03[0], 0, "P vertex at origin is outside Q");
        // Q's vertex 0 sits at (0.3,0.4,0.5), strictly inside P's [0,1]³ ⇒ winds inside (nonzero).
        assert_ne!(b.w30[0], 0, "Q vertex inside P must have nonzero winding");
    }

    #[test]
    fn winding_is_translation_invariant() {
        // The same relative overlap, both cubes shifted far from the origin: the winding pattern is
        // identical (the boolean depends only on relative position). Guards against absolute-coordinate
        // assumptions leaking into the kernels.
        let near = Boolean3::new(&cube(0.0, 0.0, 0.0), &cube(0.3, 0.4, 0.5), OpType::Add);
        let far = Boolean3::new(
            &cube(50.0, 50.0, 50.0),
            &cube(50.3, 50.4, 50.5),
            OpType::Add,
        );
        assert_eq!(near.w03, far.w03);
        assert_eq!(near.w30, far.w30);
        assert_eq!(near.xv12.p1q2, far.xv12.p1q2);
    }

    /// The BU.4.2 gate: a mesh whose tables escaped their bounds (here a corrupt pair pointer — the
    /// mid-surgery-caller scenario) PANICS at `MeshView::validate`, in release too. The loud
    /// replacement for the per-load bounds checks the cascade no longer pays.
    #[test]
    #[should_panic(expected = "MeshView: halfedge")]
    fn corrupt_tables_panic_at_the_validation_gate() {
        let p = cube(0.0, 0.0, 0.0);
        let mut q = cube(0.3, 0.4, 0.5);
        q.halfedge[7].paired_halfedge = HalfedgeId::NONE;
        let _ = Boolean3::new(&p, &q, OpType::Add);
    }
}
