//! `SimplifyTopology` — the manifold-preserving topology surgery that cleans the boolean's output
//! (`edge_op.cpp`). This is the R2 crux: the intersection assembly (R1) produces the CORRECT SOLID but
//! leaves internal degenerate structure at coincident/near-coincident seams (doubled walls, zero-length
//! edges, sliver triangles) — geometrically inert (volume + containment are already bit-identical to
//! C++), but topologically dirty (wrong genus, inflated area). SimplifyTopology collapses that structure
//! away, turning correct-but-unclean folds into exact-genus manifolds, WITHOUT moving the
//! non-intersecting input geometry (every mutation stays within tolerance, and only NEW verts collapse).
//!
//! ## Scope: the provenance-free stages ship now; colinear + swap follow provenance (M.2.2.1)
//!
//! Manifold's `SimplifyTopology` is `CleanupTopology` (`SplitPinchedVerts` + `DedupeEdges`) +
//! `CollapseShortEdges` + `CollapseColinearEdges` + `SwapDegenerates` + `CalculateVertNormals`. Three of
//! those are provenance-free and WIRED now: `SplitPinchedVerts`, `DedupeEdges`, `CollapseShortEdges` (the
//! short-edge collapse is GEOMETRIC — edge length + CCW inversion). They already meet the R2 genus
//! acceptance (identical cubes → genus 0) and are volume-exact on the folds.
//!
//! [`CollapseColinearEdges`] and [`swap_degenerates`] are PORTED but not yet wired — both need the
//! per-triangle `triRef`/coplanar-ID provenance (M.2.2.1, now in progress). CollapseColinearEdges is
//! gated on it directly (its whole flag is `SameFace(triRef)`), and SwapDegenerates is UNSAFE without it:
//! in the C++ order colinear-collapse runs FIRST and removes the collinear-vert slivers, so
//! SwapDegenerates only sees genuine degenerates — run without it, it mis-collapses REAL geometry
//! (measured: −1.16e-3 volume on a rotated fold; swap OFF is bit-identical to C++). So they wire together
//! once `triRef` exists.
//!
//! The one place `CollapseEdge` reads `triRef` (the `!shortEdge` colinear-restriction block) is
//! unreachable in the boolean scope, where `tolerance == epsilon` forces `shortEdge` true; it is
//! transliterated minus the `triRef` restriction (every face reads as the same face → restriction
//! skipped, inversion guard kept) and documented at its site.
//!
//! ## Faithfulness notes (deviations, all output-invariant for the gates)
//!
//! - **No exact arithmetic, no FMA** — same discipline as the rest of the kernel ([`crate::boolean::predicates`]).
//! - **Removal is mark-then-compact.** Manifold marks a removed triangle (`halfedge` → `NONE`, `vertPos`
//!   → NaN) and defers the actual compaction to a later `SortGeometry`/`Finish`. We skip `SortGeometry`
//!   (the gates are order-independent), so we compact in place at the end
//!   ([`Mesh::remove_dead_triangles`] + [`Mesh::remove_unreferenced_verts`]).
//! - **`vertNormal` is dropped on entry** (it's write-only until the final recompute) so every "keep
//!   `vertNormal` aligned" push in the C++ becomes a no-op; we rebuild it clean at the end.
//! - **`faceNormal` is CARRIED, not recomputed.** Manifold maintains `faceNormal_` through the surgery
//!   (swaps copy a neighbour's, dedupe copies the parent's, collapses leave shifted triangles' normals
//!   STALE) and ends `SimplifyTopology` with `CalculateVertNormals` over those carried normals. We do the
//!   same — carrying (even the stale ones) is what keeps a CHAINED boolean's perturbation bit-faithful,
//!   since `vertNormal` feeds the next op's coincident tie-break.
//! - **Properties are skipped.** Every property (`NumProp() > 0`) branch is guarded out — the boolean
//!   output is position-only (`num_prop == 3`).
//!
//! `SplitPinchedVerts` and `DedupeEdges` use the SERIAL path (the parallel branches only differ when
//! `> 1e4`/`1e5` edges and reduce to the same ordered result); `CollapseShortEdges`/`SwapDegenerates`
//! use the serial `FlagStore` = flag-all-then-process-in-ascending-order (the parallel path sorts to the
//! same order). Container/iteration order is load-bearing and deterministic throughout.

use crate::boolean::predicates::{ccw, get_axis_aligned_projection};
use crate::linalg::{Vec2, Vec3};
use crate::mesh::{Halfedge, Mesh};
use crate::mesh_ids::{HalfedgeId, VertId};

/// The three half-edges of a triangle, from `edge` (`edge_op.cpp` `TriOf`): `[edge, next, next·next]`.
#[inline]
fn tri_of(edge: HalfedgeId) -> [HalfedgeId; 3] {
    [edge, edge.next(), edge.next().next()]
}

/// Is edge `v0→v1` the strictly-longest of the triangle `v0,v1,v2`? (`edge_op.cpp` `Is01Longest`) —
/// squared lengths, no `sqrt`.
#[inline]
#[allow(dead_code)] // wired with CollapseColinearEdges once M.2.2.1 provenance lands
fn is01_longest(v0: Vec2, v1: Vec2, v2: Vec2) -> bool {
    let e = [v1 - v0, v2 - v1, v0 - v2];
    let l = [e[0].dot(e[0]), e[1].dot(e[1]), e[2].dot(e[2])];
    l[0] > l[1] && l[0] > l[2]
}

/// Push a fresh (unpaired) half-edge `(start, NONE, prop)` — Manifold's `Halfedges::push_back(start, -1,
/// prop)`. Used by `DedupeEdge` when it splits a 4-manifold edge by adding two triangles.
#[inline]
fn push_halfedge(mesh: &mut Mesh, start: VertId, prop: VertId) {
    mesh.halfedge.push(Halfedge {
        start_vert: start,
        paired_halfedge: HalfedgeId::NONE,
        prop_vert: prop,
    });
}

/// Mutually pair two half-edges (`edge_op.cpp` `PairUp`).
#[inline]
fn pair_up(mesh: &mut Mesh, edge0: HalfedgeId, edge1: HalfedgeId) {
    mesh.set_pair(edge0, edge1);
    mesh.set_pair(edge1, edge0);
}

/// Traverse CW around `startEdge`'s end-vert from `startEdge` to `endEdge`, repointing each visited
/// half-edge to `vert` (`edge_op.cpp` `UpdateVert`). The traversal reads only pair pointers + the
/// triangle `next` (both untouched by the `start`/`end` writes), so it transliterates directly.
fn update_vert(mesh: &mut Mesh, vert: VertId, start_edge: HalfedgeId, end_edge: HalfedgeId) {
    let mut current = start_edge;
    while current != end_edge {
        mesh.set_end(current, vert);
        current = current.next();
        mesh.set_start(current, vert);
        current = mesh.pair(current);
        debug_assert!(current != start_edge, "infinite loop in decimator!");
    }
}

/// When an edge collapse would create a non-manifold edge, instead duplicate the two verts and reattach
/// the two manifolds the other way across the edge (`edge_op.cpp` `FormLoop`) — decreasing the genus
/// rather than producing a non-manifold. Pushes two new verts; `vertNormal` is intentionally not grown
/// (it's rebuilt at the end).
fn form_loop(mesh: &mut Mesh, current: HalfedgeId, end: HalfedgeId) {
    let start_vert = VertId::from_usize(mesh.vert_pos.len());
    let p = mesh.pos(mesh.start(current));
    mesh.vert_pos.push(p);
    let end_vert = VertId::from_usize(mesh.vert_pos.len());
    let p = mesh.pos(mesh.end(current));
    mesh.vert_pos.push(p);

    let old_match = mesh.pair(current);
    let new_match = mesh.pair(end);

    update_vert(mesh, start_vert, old_match, new_match);
    update_vert(mesh, end_vert, end, current);

    pair_up(mesh, current, new_match);
    pair_up(mesh, end, old_match);

    remove_if_folded(mesh, end);
}

/// Remove a triangle by re-pairing its two non-collapsed neighbours across it, then marking all three
/// half-edges removed — keeping the `prop` (`edge_op.cpp` `CollapseTri`). No-op if already unpaired.
fn collapse_tri(mesh: &mut Mesh, tri_edge: [HalfedgeId; 3]) {
    if mesh.pair(tri_edge[1]).is_none() {
        return;
    }
    let pair1 = mesh.pair(tri_edge[1]);
    let pair2 = mesh.pair(tri_edge[2]);
    pair_up(mesh, pair1, pair2);
    for e in tri_edge {
        let prop = mesh.prop(e);
        mesh.set_halfedge(e, VertId::NONE, HalfedgeId::NONE, prop);
    }
}

/// If the edge and its pair have folded onto a shared configuration (both triangles becoming the same
/// two faces), NaN out the now-redundant vert(s), re-pair the outer neighbours, and mark both triangles
/// removed (`edge_op.cpp` `RemoveIfFolded`). Pairs are read fresh between the two re-pairings, matching
/// the C++ argument-evaluation order.
fn remove_if_folded(mesh: &mut Mesh, edge: HalfedgeId) {
    let tri0edge = tri_of(edge);
    let tri1edge = tri_of(mesh.pair(edge));
    if mesh.pair(tri0edge[1]).is_none() {
        return;
    }
    if mesh.start(tri0edge[2]) == mesh.start(tri1edge[2]) {
        if mesh.pair(tri0edge[1]) == tri1edge[2] {
            if mesh.pair(tri0edge[2]) == tri1edge[1] {
                for e in tri0edge {
                    let v = mesh.start(e).u();
                    mesh.vert_pos[v] = Vec3::splat(f64::NAN);
                }
            } else {
                let v = mesh.start(tri0edge[1]).u();
                mesh.vert_pos[v] = Vec3::splat(f64::NAN);
            }
        } else if mesh.pair(tri0edge[2]) == tri1edge[1] {
            let v = mesh.start(tri1edge[1]).u();
            mesh.vert_pos[v] = Vec3::splat(f64::NAN);
        }
        let a = mesh.pair(tri0edge[1]);
        let b = mesh.pair(tri1edge[2]);
        pair_up(mesh, a, b);
        let c = mesh.pair(tri0edge[2]);
        let d = mesh.pair(tri1edge[1]);
        pair_up(mesh, c, d);
        for i in 0..3 {
            mesh.set_halfedge(tri0edge[i], VertId::NONE, HalfedgeId::NONE, VertId::NONE);
            mesh.set_halfedge(tri1edge[i], VertId::NONE, HalfedgeId::NONE, VertId::NONE);
        }
    }
}

/// Collapse `edge` by removing its `startVert` and replacing it with `endVert` — returns `false` if the
/// edge cannot be collapsed (`edge_op.cpp` `CollapseEdge`). May split the mesh topologically (via
/// [`form_loop`]) if the collapse would otherwise create a 4-manifold edge.
///
/// PROVENANCE: the `!short_edge` block's `triRef` colinear-restriction (C++ lines that `return false`
/// when the collapse crosses a face boundary or a sharp edge) is skipped — we have no `triRef`, so every
/// face reads as the same face and only the geometric inversion guard remains. In the boolean scope
/// (`tolerance == epsilon`) `short_edge` is always true when this is called, so the block is inert; it's
/// kept as a faithful-minus-`triRef` transliteration for the general case (full fidelity = M.2.2.1).
fn collapse_edge(
    mesh: &mut Mesh,
    edge: HalfedgeId,
    edges: &mut Vec<HalfedgeId>,
    tol: f64,
    first_new_vert: i32,
) -> bool {
    let tol = if tol < 0.0 { mesh.epsilon } else { tol };

    let pair = mesh.pair(edge);
    if pair.is_none() {
        return false;
    }

    let tri0edge = tri_of(edge);
    let tri1edge = tri_of(pair);
    let start_vert = mesh.start(tri0edge[0]);
    let end_vert = mesh.start(tri0edge[1]);

    let p_new = mesh.pos(end_vert);
    let p_old = mesh.pos(start_vert);
    let delta = p_new - p_old;
    // We don't re-check that startVert is still "new" — collapsing its own original neighbours further
    // can't stack errors arbitrarily far.
    let max_len = if end_vert.raw() < first_new_vert {
        tol * tol
    } else {
        mesh.epsilon * mesh.epsilon
    };
    let short_edge = delta.dot(delta) < max_len;

    // Orbit startVert. (C++ initializes `current` to tri1edge[2] here, but it's dead — always
    // reassigned to `start` before any read.)
    let mut start = mesh.pair(tri1edge[1]);
    let mut current;
    if !short_edge {
        current = start;
        let mut p_last = mesh.pos(mesh.start(tri1edge[2]));
        while current != tri1edge[0] {
            current = current.next();
            let p_next = mesh.pos(mesh.end(current));
            let tri = current.tri();
            let projection = get_axis_aligned_projection(mesh.face_normal[tri.u()]);
            // (triRef SameFace colinear-restriction skipped — see the fn doc.)
            // Don't collapse the edge if it would invert a triangle.
            if ccw(
                projection.apply(p_next),
                projection.apply(p_last),
                projection.apply(p_new),
                mesh.epsilon,
            ) < 0
            {
                return false;
            }
            p_last = p_next;
            current = mesh.pair(current);
        }
    }

    // Orbit endVert — collect the ring's edges for the loop-forming pass below.
    {
        let mut current = mesh.pair(tri0edge[1]);
        while current != tri1edge[2] {
            current = current.next();
            edges.push(current);
            current = mesh.pair(current);
        }
    }

    // Remove startVert and replace with endVert.
    mesh.vert_pos[start_vert.u()] = Vec3::splat(f64::NAN);
    collapse_tri(mesh, tri1edge);

    // Orbit startVert, forming a loop where the shifted ring re-meets a collected end-vert.
    current = start;
    while current != tri0edge[2] {
        current = current.next();
        // (NumProp() == 0 → the prop-shift block is skipped.)
        let vert = mesh.end(current);
        let next = mesh.pair(current);
        for i in 0..edges.len() {
            if vert == mesh.end(edges[i]) {
                form_loop(mesh, edges[i], current);
                start = next;
                edges.truncate(i);
                break;
            }
        }
        current = next;
    }

    update_vert(mesh, end_vert, start, tri0edge[2]);
    collapse_tri(mesh, tri0edge);
    remove_if_folded(mesh, start);
    true
}

/// Swap the shared long edge of two facing degenerate triangles to the opposite verts (`edge_op.cpp`'s
/// `SwapEdge` lambda). Copies the neighbour's face normal (the swapped triangle becomes a subset of it);
/// `triRef`/property updates are skipped (provenance/position-only). If the swap would recreate an
/// existing edge, [`form_loop`] splits instead.
#[allow(dead_code)] // wired with CollapseColinearEdges once M.2.2.1 provenance lands
fn swap_edge(mesh: &mut Mesh, tri0edge: [HalfedgeId; 3], tri1edge: [HalfedgeId; 3]) {
    // The 0-verts are swapped to the opposite 2-verts.
    let v0 = mesh.start(tri0edge[2]);
    let v1 = mesh.start(tri1edge[2]);
    mesh.set_start(tri0edge[0], v1);
    mesh.set_end(tri0edge[2], v1);
    mesh.set_start(tri1edge[0], v0);
    mesh.set_end(tri1edge[2], v0);
    let a = mesh.pair(tri1edge[2]);
    pair_up(mesh, tri0edge[0], a);
    let b = mesh.pair(tri0edge[2]);
    pair_up(mesh, tri1edge[0], b);
    pair_up(mesh, tri0edge[2], tri1edge[2]);
    // Both triangles are now subsets of the neighbouring triangle.
    let tri0 = tri0edge[0].tri();
    let tri1 = tri1edge[0].tri();
    mesh.face_normal[tri0.u()] = mesh.face_normal[tri1.u()];
    // (triRef copy + property interpolation skipped — provenance / NumProp() == 0.)

    // If the new edge already exists, duplicate the verts and split the mesh.
    let mut current = mesh.pair(tri1edge[0]);
    let end_vert = mesh.end(tri1edge[1]);
    while current != tri0edge[1] {
        current = current.next();
        if mesh.end(current) == end_vert {
            form_loop(mesh, tri0edge[2], current);
            remove_if_folded(mesh, tri0edge[2]);
            return;
        }
        current = mesh.pair(current);
    }
}

/// Swap the long edge of a degenerate triangle, cascading via an explicit stack (`edge_op.cpp`
/// `RecursiveEdgeSwap` — despite the name, the recursion is the `edgeSwapStack` in [`swap_degenerates`]).
/// `visited`/`tag` break infinite cycles. Reads only geometry + face normals (provenance-free).
#[allow(dead_code, clippy::too_many_arguments)] // wired at M.2.2.1
fn recursive_edge_swap(
    mesh: &mut Mesh,
    edge: HalfedgeId,
    tag: &mut i32,
    visited: &mut [i32],
    edge_swap_stack: &mut Vec<HalfedgeId>,
    edges: &mut Vec<HalfedgeId>,
) {
    if edge.is_none() {
        return;
    }
    let pair = mesh.pair(edge);
    if pair.is_none() {
        return;
    }
    // Avoid infinite recursion.
    if visited[edge.u()] == *tag && visited[pair.u()] == *tag {
        return;
    }

    let tri0edge = tri_of(edge);
    let tri1edge = tri_of(pair);

    let projection = get_axis_aligned_projection(mesh.face_normal[edge.tri().u()]);
    let mut v = [Vec2::ZERO; 4];
    for i in 0..3 {
        v[i] = projection.apply(mesh.pos(mesh.start(tri0edge[i])));
    }
    // Only operate on the long edge of a degenerate triangle.
    if ccw(v[0], v[1], v[2], mesh.tolerance) > 0 || !is01_longest(v[0], v[1], v[2]) {
        return;
    }

    // Switch to the neighbour's projection.
    let projection = get_axis_aligned_projection(mesh.face_normal[pair.tri().u()]);
    for i in 0..3 {
        v[i] = projection.apply(mesh.pos(mesh.start(tri0edge[i])));
    }
    v[3] = projection.apply(mesh.pos(mesh.start(tri1edge[2])));

    // Only operate if the other triangles are not degenerate.
    if ccw(v[1], v[0], v[3], mesh.tolerance) <= 0 {
        if !is01_longest(v[1], v[0], v[3]) {
            return;
        }
        // Two facing, long-edge degenerates can swap.
        swap_edge(mesh, tri0edge, tri1edge);
        let e23 = v[3] - v[2];
        if e23.dot(e23) < mesh.tolerance * mesh.tolerance {
            *tag += 1;
            collapse_edge(mesh, tri0edge[2], edges, -1.0, 0);
            edges.clear();
        } else {
            visited[edge.u()] = *tag;
            visited[pair.u()] = *tag;
            for e in [tri1edge[1], tri1edge[0], tri0edge[1], tri0edge[0]] {
                edge_swap_stack.push(e);
            }
        }
        return;
    } else if ccw(v[0], v[3], v[2], mesh.tolerance) <= 0 || ccw(v[1], v[2], v[3], mesh.tolerance) <= 0 {
        return;
    }
    // Normal path.
    swap_edge(mesh, tri0edge, tri1edge);
    visited[edge.u()] = *tag;
    visited[pair.u()] = *tag;
    let a = mesh.pair(tri1edge[0]);
    let b = mesh.pair(tri0edge[1]);
    for e in [a, b] {
        edge_swap_stack.push(e);
    }
}

/// Repoint an entire vertex fan (seeded at `seed`) to `new_vert` (the `ForVert` lambda shared by
/// `DedupeEdge`). The fan is collected first (pair pointers, which drive the walk, are untouched by the
/// `start`/`end` writes), then repointed.
fn repoint_vert_ring(mesh: &mut Mesh, seed: HalfedgeId, new_vert: VertId) {
    let mut ring = Vec::new();
    mesh.for_vert(seed, |e| ring.push(e));
    for e in ring {
        mesh.set_start(e, new_vert);
        let pe = mesh.pair(e);
        mesh.set_end(pe, new_vert);
    }
}

/// Deduplicate a 4-manifold edge by duplicating its `endVert` (and its `startVert` if that becomes
/// pinched), making the coincident edges distinct (`edge_op.cpp` `DedupeEdge`). The "single topological
/// unit" case adds two triangles to separate the fans; the "separate unit" case just repoints a fan.
fn dedupe_edge(mesh: &mut Mesh, edge: HalfedgeId) {
    // Orbit endVert.
    let next_edge = edge.next();
    let start_vert = mesh.start(edge);
    let end_vert = mesh.start(next_edge);
    let end_prop = mesh.prop(next_edge);
    let mut current = mesh.pair(next_edge);
    while current != edge {
        let vert = mesh.start(current);
        if vert == start_vert {
            // Single topological unit — needs 2 faces added to be split.
            let new_vert = VertId::from_usize(mesh.vert_pos.len());
            let p = mesh.pos(end_vert);
            mesh.vert_pos.push(p);
            current = mesh.pair(current.next());
            let opposite = mesh.pair(next_edge);

            update_vert(mesh, new_vert, current, opposite);

            let mut new_halfedge = HalfedgeId::from_usize(mesh.halfedge.len());
            let mut old_face = current.tri();
            let mut outside_vert = mesh.start(current);
            push_halfedge(mesh, end_vert, end_prop);
            push_halfedge(mesh, new_vert, end_prop);
            let prop_c = mesh.prop(current);
            push_halfedge(mesh, outside_vert, prop_c);
            let pc = mesh.pair(current);
            pair_up(mesh, new_halfedge.offset(2), pc);
            pair_up(mesh, new_halfedge.offset(1), current);
            if !mesh.face_normal.is_empty() {
                let n = mesh.face_normal[old_face.u()];
                mesh.face_normal.push(n);
            }

            new_halfedge = new_halfedge.offset(3);
            old_face = opposite.tri();
            outside_vert = mesh.start(opposite);
            push_halfedge(mesh, new_vert, end_prop); // fix prop
            push_halfedge(mesh, end_vert, end_prop);
            let prop_o = mesh.prop(opposite);
            push_halfedge(mesh, outside_vert, prop_o);
            let po = mesh.pair(opposite);
            pair_up(mesh, new_halfedge.offset(2), po);
            pair_up(mesh, new_halfedge.offset(1), opposite);
            pair_up(mesh, new_halfedge, new_halfedge.offset(-3));
            if !mesh.face_normal.is_empty() {
                let n = mesh.face_normal[old_face.u()];
                mesh.face_normal.push(n);
            }

            break;
        }

        current = mesh.pair(current.next());
    }

    if current == edge {
        // Separate topological unit — needs no new faces to be split.
        let new_vert = VertId::from_usize(mesh.vert_pos.len());
        let p = mesh.pos(end_vert);
        mesh.vert_pos.push(p);
        repoint_vert_ring(mesh, current.next(), new_vert);
    }

    // Orbit startVert.
    let pair = mesh.pair(edge);
    current = mesh.pair(pair.next());
    while current != pair {
        let vert = mesh.start(current);
        if vert == end_vert {
            break; // Connected: not a pinched vert.
        }
        current = mesh.pair(current.next());
    }

    if current == pair {
        // Split the pinched vert the previous split created.
        let new_vert = VertId::from_usize(mesh.vert_pos.len());
        let p = mesh.pos(end_vert);
        mesh.vert_pos.push(p);
        repoint_vert_ring(mesh, current.next(), new_vert);
    }
}

/// Duplicate just enough verts to convert an even-manifold to a proper 2-manifold, splitting
/// non-manifold verts where multiple fan-cycles share one vertex (`edge_op.cpp` `SplitPinchedVerts`,
/// serial branch). Each vertex fan is processed once; a second fan on an already-seen vertex gets a
/// fresh duplicate vert.
fn split_pinched_verts(mesh: &mut Mesh) {
    let nb_edges = mesh.halfedge.len();
    let mut vert_processed = vec![false; mesh.num_vert()];
    let mut halfedge_processed = vec![false; nb_edges];
    for i in 0..nb_edges {
        if halfedge_processed[i] {
            continue;
        }
        let hi = HalfedgeId::from_usize(i);
        let vert = mesh.start(hi);
        if vert.is_none() {
            continue;
        }
        let mut ring = Vec::new();
        mesh.for_vert(hi, |e| ring.push(e));
        if vert_processed[vert.u()] {
            let p = mesh.pos(vert);
            mesh.vert_pos.push(p);
            let new_vert = VertId::from_usize(mesh.num_vert() - 1);
            for e in ring {
                halfedge_processed[e.u()] = true;
                mesh.set_start(e, new_vert);
                let pe = mesh.pair(e);
                mesh.set_end(pe, new_vert);
            }
        } else {
            vert_processed[vert.u()] = true;
            for e in ring {
                halfedge_processed[e.u()] = true;
            }
        }
    }
}

/// Find the duplicate half-edges to split — for each vertex fan, all out-edges sharing an end-vert keep
/// the minimal half-edge index; the rest are flagged (the serial `localLoop` of `edge_op.cpp`
/// `DedupeEdges`). Deterministic: emission is in fan order, seeds in ascending index. The end-vert→min
/// map is lookup-only (never iterated) so a `HashMap` is order-safe here.
fn find_duplicate_edges(mesh: &Mesh, nb_edges: usize) -> Vec<HalfedgeId> {
    use std::collections::HashMap;
    let mut local = vec![false; nb_edges];
    let mut results = Vec::new();
    for i in 0..nb_edges {
        if local[i] {
            continue;
        }
        let hi = HalfedgeId::from_usize(i);
        if mesh.start(hi).is_none() || mesh.end(hi).is_none() {
            continue;
        }
        let mut ring = Vec::new();
        mesh.for_vert(hi, |e| ring.push(e));

        // First pass: minimal half-edge index per end-vert.
        let mut min_by_end: HashMap<VertId, HalfedgeId> = HashMap::new();
        for &current in &ring {
            local[current.u()] = true;
            let sv = mesh.start(current);
            let ev = mesh.end(current);
            if sv.is_none() || ev.is_none() {
                continue;
            }
            min_by_end
                .entry(ev)
                .and_modify(|c| {
                    if current < *c {
                        *c = current;
                    }
                })
                .or_insert(current);
        }
        // Second pass: flag every non-minimal duplicate.
        for &current in &ring {
            let sv = mesh.start(current);
            let ev = mesh.end(current);
            if sv.is_none() || ev.is_none() {
                continue;
            }
            if min_by_end[&ev] != current {
                results.push(current);
            }
        }
    }
    results
}

/// Remove duplicate edges (more than one triangle-pair sharing an edge) by splitting them, until none
/// remain (`edge_op.cpp` `DedupeEdges`). Each split may create new duplicates, so it loops to a
/// fixed point.
fn dedupe_edges(mesh: &mut Mesh) {
    loop {
        let nb_edges = mesh.halfedge.len();
        let duplicates = find_duplicate_edges(mesh, nb_edges);
        if duplicates.is_empty() {
            break;
        }
        for i in duplicates {
            dedupe_edge(mesh, i);
        }
    }
}

/// The short-edge flag (`edge_op.cpp` `CollapseShortEdges`'s `shortEdge` lambda): a paired edge touching
/// at least one new vert, whose squared length is under the (new-to-new = `epsilon²`, new-to-old =
/// `tol²`) bound.
fn short_edge_pred(mesh: &Mesh, edge: HalfedgeId, first_new_vert: i32, tol: f64) -> bool {
    let pair = mesh.pair(edge);
    if pair.is_none() {
        return false;
    }
    let start = mesh.start(edge);
    let end = mesh.end(edge);
    if start.raw() < first_new_vert && end.raw() < first_new_vert {
        return false;
    }
    let delta = mesh.pos(end) - mesh.pos(start);
    let len_sq = delta.dot(delta);
    // Only collapse a new↔old edge up to tol; a new↔new edge only up to epsilon (old verts may move by
    // at most epsilon, so tol-scale errors can't stack).
    let max_len = if end.raw() < first_new_vert {
        tol * tol
    } else {
        mesh.epsilon * mesh.epsilon
    };
    len_sq < max_len
}

/// Collapse edges shorter than tolerance, removing degenerate triangles (`edge_op.cpp`
/// `CollapseShortEdges`). Flag-all-then-collapse (the serial `FlagStore`): the flagging reads the clean
/// post-dedupe mesh, then each flagged edge is collapsed in ascending order (already-collapsed edges are
/// no-ops via the `pair < 0` guard). In a boolean (`first_new_vert > 0`) the bound is `tolerance`.
fn collapse_short_edges(mesh: &mut Mesh, first_new_vert: i32) {
    let nb_edges = mesh.halfedge.len();
    let tol = if first_new_vert == 0 {
        mesh.epsilon
    } else {
        mesh.tolerance
    };

    let mut flagged = Vec::new();
    for i in 0..nb_edges {
        let hi = HalfedgeId::from_usize(i);
        if short_edge_pred(mesh, hi, first_new_vert, tol) {
            flagged.push(hi);
        }
    }

    let mut scratch = Vec::new();
    for hi in flagged {
        collapse_edge(mesh, hi, &mut scratch, tol, first_new_vert);
        scratch.clear();
    }
}

/// The swappable-edge flag (`edge_op.cpp` `SwapDegenerates`'s `swappableEdge` lambda): a paired edge, at
/// least one endpoint new, that is the long edge of a degenerate (CW/collinear) triangle whose neighbour
/// is also degenerate or shares the long edge.
#[allow(dead_code)] // wired with CollapseColinearEdges once M.2.2.1 provenance lands
fn swappable_edge_pred(mesh: &Mesh, edge: HalfedgeId, first_new_vert: i32) -> bool {
    let pair = mesh.pair(edge);
    if pair.is_none() {
        return false;
    }
    let tri_edge = tri_of(edge);
    let pair_tri_edge = tri_of(pair);
    let fnv = first_new_vert;
    if mesh.start(tri_edge[0]).raw() < fnv
        && mesh.start(tri_edge[1]).raw() < fnv
        && mesh.start(tri_edge[2]).raw() < fnv
        && mesh.start(pair_tri_edge[2]).raw() < fnv
    {
        return false;
    }

    let projection = get_axis_aligned_projection(mesh.face_normal[edge.tri().u()]);
    let mut v = [Vec2::ZERO; 3];
    for i in 0..3 {
        v[i] = projection.apply(mesh.pos(mesh.start(tri_edge[i])));
    }
    if ccw(v[0], v[1], v[2], mesh.tolerance) > 0 || !is01_longest(v[0], v[1], v[2]) {
        return false;
    }

    // Switch to the neighbour's projection.
    let projection = get_axis_aligned_projection(mesh.face_normal[pair.tri().u()]);
    for i in 0..3 {
        v[i] = projection.apply(mesh.pos(mesh.start(pair_tri_edge[i])));
    }
    ccw(v[0], v[1], v[2], mesh.tolerance) > 0 || is01_longest(v[0], v[1], v[2])
}

/// Perform edge swaps on the long edges of degenerate triangles (`edge_op.cpp` `SwapDegenerates`).
/// Flag-all-then-process; each flagged edge seeds a fresh `tag` and drains its cascade stack before the
/// next. `visited` is sized to the (post-collapse-stage) half-edge count, which no swap grows.
#[allow(dead_code)] // wired with CollapseColinearEdges once M.2.2.1 provenance lands
fn swap_degenerates(mesh: &mut Mesh, first_new_vert: i32) {
    let nb_edges = mesh.halfedge.len();

    let mut flagged = Vec::new();
    for i in 0..nb_edges {
        let hi = HalfedgeId::from_usize(i);
        if swappable_edge_pred(mesh, hi, first_new_vert) {
            flagged.push(hi);
        }
    }

    let mut edge_swap_stack: Vec<HalfedgeId> = Vec::new();
    let mut visited = vec![-1i32; mesh.halfedge.len()];
    let mut tag = 0i32;
    let mut scratch = Vec::new();
    for hi in flagged {
        tag += 1;
        recursive_edge_swap(mesh, hi, &mut tag, &mut visited, &mut edge_swap_stack, &mut scratch);
        while let Some(last) = edge_swap_stack.pop() {
            recursive_edge_swap(mesh, last, &mut tag, &mut visited, &mut edge_swap_stack, &mut scratch);
        }
    }
}

/// Simplify the boolean's output topology: the four provenance-free stages of Manifold's
/// `SimplifyTopology`, then compact + rebuild normals (`edge_op.cpp` `SimplifyTopology`, minus
/// `CollapseColinearEdges` = M.2.2.1). `first_new_vert` is the first intersection-vert index (`n_pv +
/// n_qv` from the boolean): verts below it are the untouched input geometry and are never collapsed.
/// See the module doc for the faithfulness deviations (all output-invariant for the volume/genus/manifold
/// gates).
pub fn simplify_topology(mesh: &mut Mesh, first_new_vert: i32) {
    if mesh.halfedge.is_empty() {
        return;
    }
    // vertNormal is write-only until the final recompute; drop it so every "keep vertNormal aligned"
    // push in the surgery is a no-op, and rebuild it clean at the end.
    mesh.vert_normal.clear();

    // CleanupTopology: split pinched verts, then dedupe 4-manifold edges.
    split_pinched_verts(mesh);
    dedupe_edges(mesh);

    // Collapse edges shorter than tolerance.
    collapse_short_edges(mesh, first_new_vert);

    // CollapseColinearEdges and SwapDegenerates are BOTH DEFERRED to M.2.2.1. CollapseColinearEdges is
    // provenance-bound directly (its colinear test is `SameFace(triRef)`), and — proven empirically —
    // SwapDegenerates is UNSAFE without it: in the C++ order colinear-collapse runs first, so
    // SwapDegenerates only ever sees the genuine degenerates it's meant to fix; run WITHOUT it, it
    // encounters the collinear-vert slivers C++ has already removed and mis-collapses REAL geometry
    // (measured: −1.16e-3 volume on a rotated-cube fold, deleting 2 non-degenerate triangles — with swap
    // OFF the same fold is bit-identical to C++). The safe subset above already meets the R2 genus
    // acceptance (identical cubes → genus 0) and is volume-exact on the folds; genus-0 for the general
    // rotated fold needs the deferred pair. See [`swap_degenerates`] — the port is done, just unwired.

    // Compact: drop the marked-removed triangles + NaN/unreferenced verts, reindexing connectivity
    // (C++ defers this to SortGeometry/Finish; we skip SortGeometry so we compact here). faceNormal is
    // carried through the compaction (see the module doc), so vertNormal below sees the same normals C++
    // would.
    mesh.remove_dead_triangles();
    mesh.remove_unreferenced_verts();

    // Merging verts changed the geometry → recompute vertNormal on the clean mesh (the C++ tail).
    mesh.calculate_vert_normals();
}
