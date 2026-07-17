//! The broad phase — Manifold's LBVH `Collider` (`collider.h`), ported SERIAL.
//!
//! A Karras Morton radix-tree BVH over one mesh's per-FACE boxes, queried by the other mesh's edges
//! (or verts) during a boolean. Verbatim `collider.h`: `CreateRadixTree` (Karras split via `PrefixLength`
//! with count-leading-zeros, ties broken by leaf index), `BuildInternalBoxes` (bottom-up box union), and
//! the `FindCollision` stack traversal. The radix-tree fill runs through the deterministic parallel seam
//! (`par::for_each_mut` — each internal node's children derive purely from the sorted codes); the rest of
//! the C++ parallel/atomic machinery is dropped — a serial counter reproduces `BuildInternalBoxes`
//! bit-for-bit because a box union is exact (componentwise min/max) and each internal box is computed
//! exactly once, when its second child arrives, regardless of order.
//!
//! ## Node numbering (`collider.h`)
//! Nodes are indexed so EVEN nodes are leaves and ODD nodes are internal; the root is node 1. Leaf `i`
//! lives at node `2i`, internal `i` at node `2i+1`. `nodeBBox` has `2·L−1` entries; `internalChildren`
//! has `L−1`. A `≤1`-leaf collider has no internal nodes and collides nothing (the C++ `NumLeaves()==0`
//! short-circuit) — real boolean inputs have ≥4 faces, so this only guards the degenerate case.
//!
//! ## Why this matches brute-force EXACTLY (the M.2.4.1 gate)
//! A correct BVH visits every leaf whose box overlaps the query (internal boxes bound their descendants),
//! so it emits the SAME candidate SET as an `O(n·leaves)` scan. Order differs, but both consumers wash it
//! out: `Kernel12` `stable_sort`s the recorded pairs by `(edge, face)` and `Winding03` sums into an
//! integer array. [`Collider::collisions_brute`] keeps the scan as the differential oracle.
//!
//! ## Leaf sort + remap
//! Karras requires the leaf keys Morton-SORTED. Manifold gets that for free — every mesh is Morton-sorted
//! (`SortGeometry`) before it enters a boolean, so leaf index == face index. We DON'T mutate the caller's
//! mesh, so [`Collider::from_mesh`] sorts the leaves internally and keeps a `leaf2face` remap, applied at
//! the record boundary. The morton normalization box is the union of the live face boxes (== the mesh
//! `bBox_`); its exact value only tunes tree balance, never the collision set.
//!
//! Two query modes, matching `Box::DoesOverlap`'s two overloads: a `Box3` query (edge box vs face box)
//! and a `Vec3` point query (XY-projected — the z-raycast winding). An empty (inverted-infinity) box
//! query — what a REVERSE half-edge produces — is skipped wholesale, verbatim to the C++ early-out.
//!
//! The collider is DELIBERATELY index-agnostic: [`Collider::collisions`] collides abstract leaf/query
//! indices (`i32`), and the CALLER assigns their meaning (a query is an edge here, a vert there) by
//! wrapping into the typed ids at the callback boundary.

use crate::linalg::{Box3, Vec3};
use crate::mesh::Mesh;
use crate::mesh_ids::{HalfedgeId, TriId};
use crate::sort::{K_NO_CODE, morton_code};

/// A broad-phase query — either a `Box3` (box-vs-box overlap) or a `Vec3` (point projected into the
/// leaf's XY extent). Unifies the two `Box::DoesOverlap` overloads so [`Collider::collisions`] is one
/// function over both.
pub trait ColliderQuery: Copy {
    /// Does this query overlap `leaf`? Box→Box is the symmetric AABB test; point→box is XY-projected.
    fn overlaps(self, leaf: Box3) -> bool;
    /// The empty-box early-out: a reverse half-edge yields an inverted-infinity `Box`, which overlaps
    /// nothing — the C++ skips it before traversal, so we skip it before descending the tree.
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

// --- node arithmetic (`collider.h` collider_internal) ---
const K_ROOT: i32 = 1;
/// Exponential-search seed + growth for `RangeEnd` (`collider.h` kInitialLength / kLengthMultiple).
const K_INITIAL_LENGTH: i32 = 128;
const K_LENGTH_MULTIPLE: i32 = 4;

#[inline]
const fn is_leaf(node: i32) -> bool {
    node % 2 == 0
}
#[inline]
const fn is_internal(node: i32) -> bool {
    node % 2 == 1
}
#[inline]
const fn node2internal(node: i32) -> i32 {
    (node - 1) / 2
}
#[inline]
const fn internal2node(internal: i32) -> i32 {
    internal * 2 + 1
}
#[inline]
const fn node2leaf(node: i32) -> i32 {
    node / 2
}
#[inline]
const fn leaf2node(leaf: i32) -> i32 {
    leaf * 2
}

/// Karras radix-tree builder (`collider.h` `CreateRadixTree`) — a READ-ONLY view over the
/// Morton-SORTED leaf codes. Each internal node's `children(internal)` derives purely from the
/// codes (the Karras binary searches — per-node independent, and the bulk of the build cost), so
/// the fill is an order-preserving parallel map (`par::for_each_mut`, matching the C++
/// Par-over-`NumInternal` dispatch at the same 1e4 threshold); the parent back-pointers are wired
/// afterward in a cheap serial pass, replacing the C++'s in-kernel scattered stores.
struct RadixTreeBuilder<'a> {
    leaf_morton: &'a [u32],
}

impl RadixTreeBuilder<'_> {
    /// Count of identical high-order bits of two codes (`__builtin_clz(a ^ b)`). `a != b` at every real
    /// call site (the tie-break below guarantees distinct keys), so the `clz(0)==32` fallback is never
    /// hit — matching the C++, which relies on the same uniqueness.
    #[inline]
    fn prefix_bits(a: u32, b: u32) -> i32 {
        (a ^ b).leading_zeros() as i32
    }

    /// Common-prefix length of leaves `i`,`j` (`PrefixLength(int,int)`). Out-of-range `j` → −1; equal
    /// Morton codes fall back to disambiguating by leaf INDEX (`32 + clz(i^j)`), so the keys are totally
    /// ordered even under Morton ties.
    #[inline]
    fn prefix_length(&self, i: i32, j: i32) -> i32 {
        if j < 0 || j >= self.leaf_morton.len() as i32 {
            return -1;
        }
        let (mi, mj) = (self.leaf_morton[i as usize], self.leaf_morton[j as usize]);
        if mi == mj {
            32 + Self::prefix_bits(i as u32, j as u32)
        } else {
            Self::prefix_bits(mi, mj)
        }
    }

    /// The far end of leaf `i`'s Karras range (`RangeEnd`): pick the direction of longer common prefix,
    /// grow a conservative bound exponentially, then binary-search the exact length.
    fn range_end(&self, i: i32) -> i32 {
        let mut dir = self.prefix_length(i, i + 1) - self.prefix_length(i, i - 1);
        dir = (dir > 0) as i32 - (dir < 0) as i32;
        let common_prefix = self.prefix_length(i, i - dir);
        let mut max_length = K_INITIAL_LENGTH;
        while self.prefix_length(i, i + dir * max_length) > common_prefix {
            max_length *= K_LENGTH_MULTIPLE;
        }
        let mut length = 0;
        let mut step = max_length / 2;
        while step > 0 {
            if self.prefix_length(i, i + dir * (length + step)) > common_prefix {
                length += step;
            }
            step /= 2;
        }
        i + dir * length
    }

    /// The split position within `[first, last]` where the next-highest bit differs (`FindSplit`),
    /// by binary search on the common-prefix length.
    fn find_split(&self, first: i32, last: i32) -> i32 {
        let common_prefix = self.prefix_length(first, last);
        let mut split = first;
        let mut step = last - first;
        loop {
            step = (step + 1) >> 1; // divide by 2, rounding up
            let new_split = split + step;
            if new_split < last {
                let split_prefix = self.prefix_length(first, new_split);
                if split_prefix > common_prefix {
                    split = new_split;
                }
            }
            if step <= 1 {
                break;
            }
        }
        split
    }

    /// One internal node's `(child1, child2)` (`CreateRadixTree::operator()` minus the parent
    /// wiring): a pure function of the sorted codes, independent per node — the unit the parallel
    /// fill maps over.
    fn children(&self, internal: i32) -> (i32, i32) {
        let mut first = internal;
        let mut last = self.range_end(first);
        if first > last {
            std::mem::swap(&mut first, &mut last);
        }
        let mut split = self.find_split(first, last);
        let child1 = if split == first {
            leaf2node(split)
        } else {
            internal2node(split)
        };
        split += 1;
        let child2 = if split == last {
            leaf2node(split)
        } else {
            internal2node(split)
        };
        (child1, child2)
    }
}

/// The broad-phase acceleration structure (Manifold's `Collider`): a Karras Morton-BVH over one mesh's
/// per-face boxes. Built once per boolean input, queried by the other mesh's edges/verts.
#[derive(Clone, Debug, Default)]
pub struct Collider {
    /// One AABB per leaf, in ORIGINAL face order — the brute-force oracle ([`Collider::collisions_brute`])
    /// scans these, and it's the source the BVH is built from. A removed triangle keeps the default empty
    /// box, so it never collides.
    pub leaf_box: Vec<Box3>,
    /// BVH node boxes (`2·L−1`): leaf `i` at `nodeBBox[2i]`, internal `i` at `nodeBBox[2i+1]`. Empty for a
    /// `≤1`-leaf collider (which collides nothing).
    node_bbox: Vec<Box3>,
    /// `(child1, child2)` node ids per internal node (`L−1` entries). Empty ⇒ no traversal.
    internal_children: Vec<(i32, i32)>,
    /// Parent node id per node (`2·L−1`); root's is left at −1.
    node_parent: Vec<i32>,
    /// Sorted-leaf position → original face index. Applied at the record boundary so callers see face
    /// indices, not Morton-sorted positions.
    leaf2face: Vec<i32>,
}

impl Collider {
    /// Build a BVH over explicit leaf boxes, deriving a Morton code per box from its CENTER (used by tests
    /// and any caller without triangle centroids). Boxes are the source of truth; a non-finite box is a
    /// removed leaf (`kNoCode`, sorts last, never collides).
    pub fn new(leaf_box: Vec<Box3>) -> Self {
        let bbox = live_bbox(&leaf_box);
        let morton: Vec<u32> = leaf_box
            .iter()
            .map(|b| {
                if b.is_finite() {
                    morton_code(b.center(), bbox)
                } else {
                    K_NO_CODE
                }
            })
            .collect();
        Self::build(leaf_box, &morton)
    }

    /// Build over a mesh's per-face boxes — the way a boolean builds `inQ.collider_`
    /// (`GetFaceBoxMorton` + `SortFaces` + `Collider(faceBox, faceMorton)`, fused). Each face box unions
    /// its three verts; its Morton code is the centroid normalized by the live bounding box; a removed
    /// face (`pair(first_halfedge)` is NONE) gets the empty box + `kNoCode`.
    pub fn from_mesh(mesh: &Mesh) -> Self {
        let num_tri = mesh.num_tri();
        let mut leaf_box = vec![Box3::default(); num_tri];
        let mut centroid = vec![Vec3::ZERO; num_tri];
        for face in 0..num_tri {
            let t = TriId::from_usize(face);
            if mesh.pair(t.halfedge(0)).is_none() {
                continue; // removed face: empty box, kNoCode below
            }
            let mut c = Vec3::ZERO;
            for i in 0..3 {
                let p = mesh.pos(mesh.start(t.halfedge(i)));
                c += p;
                leaf_box[face].union_point(p);
            }
            centroid[face] = c / 3.0;
        }
        Self::from_sorted_leaves(leaf_box, &centroid)
    }

    /// Build the collider from PRECOMPUTED per-face boxes + centroids (BU.4.7) — the fused entry
    /// [`Mesh::sort_faces`] hands its already-computed `faceBox`/centroid to, so the second per-vertex sweep
    /// [`from_mesh`](Self::from_mesh) does is skipped (C++ reuses `SortFaces`' `faceBox`/`faceMorton` in
    /// `Collider(faceBox, faceMorton)`, `sort.cpp:213`). Byte-IDENTICAL to `from_mesh` over the same mesh:
    /// `leaf_box`/`centroid` ARE that mesh's per-face box/centroid, and `live_bbox` + `morton_code` are
    /// recomputed here exactly as `from_mesh` did (the collider's Morton is the LIVE-bbox one — distinct from
    /// the mesh-bbox sort key — so it must be derived here, not reused from the sort). `build` re-sorts by
    /// Morton, so a non-sorted `leaf_box` (the `from_mesh` callers) is fine too; the fused path just feeds it
    /// pre-sorted, which the adaptive stable sort takes as its fast path.
    pub(crate) fn from_sorted_leaves(leaf_box: Vec<Box3>, centroid: &[Vec3]) -> Self {
        let bbox = live_bbox(&leaf_box);
        let morton: Vec<u32> = (0..leaf_box.len())
            .map(|f| {
                if leaf_box[f].is_finite() {
                    morton_code(centroid[f], bbox)
                } else {
                    K_NO_CODE
                }
            })
            .collect();
        Self::build(leaf_box, &morton)
    }

    /// Sort the leaves by Morton code (stable, ties → index) and build the radix tree + node boxes over
    /// the sorted order (`Collider(leafBB, leafMorton)`). `leaf_box` is retained in ORIGINAL order; the
    /// sort lives entirely in `leaf2face`.
    fn build(leaf_box: Vec<Box3>, morton: &[u32]) -> Self {
        let num_leaves = leaf_box.len();
        // new (sorted) -> old (face) permutation, with the leaf index as an explicit total-order
        // tiebreak (M.4.2) so equal-Morton runs are ordered deterministically even under an unstable
        // parallel sort. Set-invariant here (the BVH collision SET is independent of leaf order), so this
        // is hygiene — but it makes the whole sort surface parallel-safe. No-op on the current output.
        let mut leaf2face: Vec<i32> = (0..num_leaves as i32).collect();
        leaf2face.sort_by(|&a, &b| morton[a as usize].cmp(&morton[b as usize]).then(a.cmp(&b)));

        let mut c = Self {
            leaf_box,
            node_bbox: Vec::new(),
            internal_children: Vec::new(),
            node_parent: Vec::new(),
            leaf2face,
        };
        // No internal nodes ⇒ nothing to build and nothing collides (C++ `NumLeaves()==0` case).
        if num_leaves <= 1 {
            return c;
        }
        let num_nodes = 2 * num_leaves - 1;
        let num_internal = num_leaves - 1;
        c.node_bbox = vec![Box3::default(); num_nodes];
        c.node_parent = vec![-1; num_nodes];
        c.internal_children = vec![(-1, -1); num_internal];

        // Copy the sorted leaf boxes into the even node slots.
        for (leaf, &old) in c.leaf2face.iter().enumerate() {
            c.node_bbox[leaf2node(leaf as i32) as usize] = c.leaf_box[old as usize];
        }
        // Organize the tree. The per-node child derivation (the Karras binary searches — the bulk
        // of the build) is a pure read over the sorted codes, so it maps in parallel: slot
        // `internal` depends only on its own index → deterministic by construction. Measured on
        // self_intersect (17k faces): whole-build 1.84 ms → 1.48 ms par. The neighboring passes
        // (morton sort/gather, face boxes, internal boxes) were each measured par and REGRESSED —
        // their per-item work is a few ns, below rayon's dispatch cost at these sizes — so they
        // stay serial.
        let sorted_morton: Vec<u32> = c.leaf2face.iter().map(|&o| morton[o as usize]).collect();
        {
            let rt = RadixTreeBuilder {
                leaf_morton: &sorted_morton,
            };
            crate::par::for_each_mut(&mut c.internal_children, |i, slot| {
                *slot = rt.children(i as i32);
            });
        }
        // Parent wiring: two stores per internal, each node has exactly one parent (disjoint
        // writes, fixed order) — trivially cheap, kept serial.
        for internal in 0..num_internal as i32 {
            let (child1, child2) = c.internal_children[internal as usize];
            let node = internal2node(internal);
            c.node_parent[child1 as usize] = node;
            c.node_parent[child2 as usize] = node;
        }
        c.build_internal_boxes(num_internal);
        c
    }

    /// Fill internal node boxes bottom-up (`BuildInternalBoxes` / `UpdateBoxes`). Each leaf walks toward
    /// the root; a per-internal counter lets only the SECOND arriving child compute the union, so both
    /// child boxes are ready. Serial + a plain counter is bit-identical to the parallel atomic version —
    /// box union is exact min/max, computed exactly once per node.
    ///
    /// KEPT SERIAL after measuring a deterministic level-sweep alternative (integer height climb,
    /// bucket internals by height, per-level `par::map_collect` union): every level is below
    /// `par`'s 10k threshold at corpus scale (level 1 holds at most `num_leaves/2` nodes, so
    /// nothing parallelizes under ~20k faces) and the extra climb + bucketing cost ~0.5 ms on
    /// self_intersect — pure overhead, no payoff until ~40k+ faces. The C++ atomic second-arrival
    /// scheme is the forbidden who-computes-it shape, so serial stands.
    fn build_internal_boxes(&mut self, num_internal: usize) {
        let mut counter = vec![0i32; num_internal];
        let num_leaves = num_internal + 1;
        for leaf in 0..num_leaves as i32 {
            let mut node = leaf2node(leaf);
            loop {
                node = self.node_parent[node as usize];
                let internal = node2internal(node);
                let first = counter[internal as usize] == 0;
                counter[internal as usize] += 1;
                if first {
                    break; // wait for the other child
                }
                let (c1, c2) = self.internal_children[internal as usize];
                self.node_bbox[node as usize] =
                    self.node_bbox[c1 as usize].union(self.node_bbox[c2 as usize]);
                if node == K_ROOT {
                    break;
                }
            }
        }
    }

    /// BVH broad phase (`Collider::Collisions` + `FindCollision`). For each query `i` in `0..n`, take
    /// `query_fn(i)`, skip an empty-box query, then depth-first-descend the tree calling `record(i, face)`
    /// for every leaf box it overlaps (face = the original index via `leaf2face`). When `self_collision`,
    /// the `face == i` self-pair is skipped (the boolean always passes `false`). Indices are raw `i32` —
    /// the caller assigns their meaning.
    pub fn collisions<Q: ColliderQuery>(
        &self,
        n: usize,
        self_collision: bool,
        query_fn: impl Fn(i32) -> Q,
        mut record: impl FnMut(i32, i32),
    ) {
        for i in 0..n as i32 {
            let q = query_fn(i);
            self.query_leaves(i, q, self_collision, |face| record(i, face));
        }
    }

    /// Traverse the BVH for a SINGLE query, calling `record(face)` for each overlapping leaf
    /// (`FindCollision::operator()` for one `queryIdx`). This is the per-query UNIT the deterministic
    /// parallel narrow phase maps over (`par::map_collect` builds one query's hit list from it, then the
    /// caller flattens + `stable_sort`s — so parallel output is bit-identical to the serial loop that
    /// [`Collider::collisions`] is). `query_idx` is only used for the `self_collision` skip.
    #[allow(unsafe_code)] // hot loop: unchecked loads, C++ VecView release parity (see below)
    #[inline]
    pub fn query_leaves<Q: ColliderQuery>(
        &self,
        query_idx: i32,
        q: Q,
        self_collision: bool,
        mut record: impl FnMut(i32),
    ) {
        if self.internal_children.is_empty() || q.is_empty() {
            return;
        }
        // Max depth is 30 (Morton) + 32 (index tie-break) < 64.
        let mut stack = [0i32; 64];
        let mut top: i32 = -1;
        let mut node = K_ROOT;
        loop {
            let internal = node2internal(node);
            debug_assert!((internal as usize) < self.internal_children.len());
            // SAFETY: `node` is the root or a child `record_collision` returned true for (odd ⇒
            // internal); `build` sizes `internal_children` to cover every internal node id.
            let (child1, child2) =
                unsafe { *self.internal_children.get_unchecked(internal as usize) };
            let traverse1 =
                self.record_collision(q, child1, query_idx, self_collision, &mut record);
            let traverse2 =
                self.record_collision(q, child2, query_idx, self_collision, &mut record);
            if !traverse1 && !traverse2 {
                if top < 0 {
                    break;
                }
                debug_assert!((top as usize) < stack.len());
                // SAFETY: `top < 64` — pushes are bounded by the tree depth (< 64, above).
                node = unsafe { *stack.get_unchecked(top as usize) };
                top -= 1;
            } else {
                node = if traverse1 { child1 } else { child2 };
                if traverse1 && traverse2 {
                    top += 1;
                    debug_assert!((top as usize) < stack.len());
                    // SAFETY: `top < 64` — one push per descent level, depth < 64 (above).
                    unsafe { *stack.get_unchecked_mut(top as usize) = child2 };
                }
            }
        }
    }

    /// Test `query` against `node`'s box (`FindCollision::RecordCollision`). Records a hit on a leaf (by
    /// original face index); the return says whether to descend (overlaps AND internal).
    #[allow(unsafe_code)] // hot loop: unchecked loads, C++ VecView release parity
    #[inline]
    fn record_collision<Q: ColliderQuery>(
        &self,
        query: Q,
        node: i32,
        query_idx: i32,
        self_collision: bool,
        record: &mut impl FnMut(i32),
    ) -> bool {
        debug_assert!((node as usize) < self.node_bbox.len());
        // SAFETY: `node` is a child id out of `internal_children`; `create` only emits node ids
        // `< 2·L−1 == node_bbox.len()`.
        let overlaps = query.overlaps(unsafe { *self.node_bbox.get_unchecked(node as usize) });
        if overlaps && is_leaf(node) {
            debug_assert!((node2leaf(node) as usize) < self.leaf2face.len());
            // SAFETY: an even node id `< 2·L−1` maps to a leaf `< L == leaf2face.len()`.
            let face = unsafe { *self.leaf2face.get_unchecked(node2leaf(node) as usize) };
            if !self_collision || face != query_idx {
                record(face);
            }
        }
        overlaps && is_internal(node)
    }

    /// Serial brute-force broad phase — the DIFFERENTIAL ORACLE the BVH is gated against. Same contract as
    /// [`Collider::collisions`] but scans every leaf in natural face order; the emitted SET must match.
    pub fn collisions_brute<Q: ColliderQuery>(
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

/// Union of the finite (live) boxes — the Morton normalization box, equal to the mesh `bBox_`. Empty
/// (all-removed / empty mesh) yields the default inverted box; its exact value only tunes tree balance.
fn live_bbox(leaf_box: &[Box3]) -> Box3 {
    let mut bbox = Box3::default();
    for b in leaf_box {
        if b.is_finite() {
            bbox = bbox.union(*b);
        }
    }
    bbox
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

    /// Collect the collision SET a collider emits for a mesh's forward edges as a sorted `(edge, face)`
    /// vector — the order-normalized form both the BVH and brute-force must agree on.
    fn edge_pairs(c: &Collider, q: &Mesh, brute: bool) -> Vec<(i32, i32)> {
        let mut pairs = Vec::new();
        let mut collect = |edge: i32, face: i32| pairs.push((edge, face));
        let qf = |e: i32| edge_query_box(q, HalfedgeId::new(e));
        if brute {
            c.collisions_brute(q.halfedge.len(), false, qf, &mut collect);
        } else {
            c.collisions(q.halfedge.len(), false, qf, &mut collect);
        }
        pairs.sort_unstable();
        pairs
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
        // The root BVH box bounds the whole cube.
        let root = c.node_bbox[internal2node(0) as usize];
        assert_eq!(root.min, Vec3::ZERO);
        assert_eq!(root.max, Vec3::splat(1.0));
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
    fn bvh_matches_brute_force_overlapping_cubes() {
        let p = cube_at(0.0, 0.0, 0.0);
        // q offset by (0.5, 0.5, 0.5) overlaps p; the collider is built over q's faces, queried by p's
        // forward edges.
        let q = cube_at(0.5, 0.5, 0.5);
        let collider = Collider::from_mesh(&q);
        let bvh = edge_pairs(&collider, &p, false);
        let brute = edge_pairs(&collider, &p, true);
        assert!(
            !bvh.is_empty(),
            "overlapping cubes must produce candidate pairs"
        );
        assert_eq!(bvh, brute, "BVH set must equal brute-force set");
        // Every recorded query index is a FORWARD edge of p; every face is a real q face.
        for &(edge, face) in &bvh {
            let e = HalfedgeId::new(edge);
            assert!(p.start(e) < p.end(e), "edge {edge} should be forward");
            assert!((0..12).contains(&face));
        }

        // A far-away cube shares no candidate pairs (both modes).
        let far = cube_at(100.0, 100.0, 100.0);
        let far_collider = Collider::from_mesh(&far);
        assert!(edge_pairs(&far_collider, &p, false).is_empty());
        assert!(edge_pairs(&far_collider, &p, true).is_empty());
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
        let mut brute_inside = 0;
        collider.collisions_brute(
            1,
            false,
            |_| Vec3::new(0.5, 0.5, 999.0),
            |_, _| brute_inside += 1,
        );
        assert!(
            hits_inside > 0,
            "a point over the XY footprint must hit some face boxes"
        );
        assert_eq!(
            hits_inside, brute_inside,
            "BVH point-query count must match brute-force"
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
        with_self.sort_unstable();
        assert_eq!(with_self, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        let mut no_self = Vec::new();
        collider.collisions(2, true, |_| b, |i, j| no_self.push((i, j)));
        no_self.sort_unstable();
        assert_eq!(no_self, vec![(0, 1), (1, 0)]);
    }

    #[test]
    fn bvh_matches_brute_on_many_overlaps() {
        // A denser scene: 27 unit cubes on a 3×3×3 grid at half-unit spacing so boxes richly overlap,
        // unioned into one collider mesh queried by a shifted probe cube. Exercises deep tree traversal.
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for gx in 0..3 {
            for gy in 0..3 {
                for gz in 0..3 {
                    let base = (verts.len() / 3) as u32;
                    let (ox, oy, oz) = (gx as f64 * 0.5, gy as f64 * 0.5, gz as f64 * 0.5);
                    #[rustfmt::skip]
                    let cube = [
                        (0.0,0.0,0.0),(1.0,0.0,0.0),(1.0,1.0,0.0),(0.0,1.0,0.0),
                        (0.0,0.0,1.0),(1.0,0.0,1.0),(1.0,1.0,1.0),(0.0,1.0,1.0),
                    ];
                    for (x, y, z) in cube {
                        verts.extend_from_slice(&[x + ox, y + oy, z + oz]);
                    }
                    #[rustfmt::skip]
                    let t = [
                        [0u32,2,1],[0,3,2],[4,5,6],[4,6,7],[0,1,5],[0,5,4],
                        [2,3,7],[2,7,6],[0,4,7],[0,7,3],[1,2,6],[1,6,5],
                    ];
                    for tri in t {
                        tris.push([tri[0] + base, tri[1] + base, tri[2] + base]);
                    }
                }
            }
        }
        let mut scene = Mesh {
            vert_pos: verts
                .chunks_exact(3)
                .map(|c| Vec3::new(c[0], c[1], c[2]))
                .collect(),
            ..Default::default()
        };
        scene.create_halfedges(&tris);
        let collider = Collider::from_mesh(&scene);
        assert_eq!(collider.leaf_box.len(), 27 * 12);

        let probe = cube_at(0.3, 0.7, 1.1);
        let bvh = edge_pairs(&collider, &probe, false);
        let brute = edge_pairs(&collider, &probe, true);
        assert!(!bvh.is_empty());
        assert_eq!(
            bvh, brute,
            "BVH and brute-force must emit the identical pair set"
        );
    }
}
