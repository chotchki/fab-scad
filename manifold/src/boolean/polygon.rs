//! Polygon triangulation — `polygon.cpp`'s `EarClip`, the Delaunay-cost ear clipper.
//!
//! Two entry points:
//! - [`earclip`] — the verbatim `EarClip` DIAGONAL choice: clip the lowest-cost ear first, where the
//!   cost is a sharpness + Delaunay metric ([`EarClipper::ear_cost`]). Matching C++'s diagonals is what
//!   makes the boolean's OUTPUT tessellation byte-identical to C++ (so a chained fold, whose intermediate
//!   feeds the next op's near-coincident tie-breaks, stays bit-identical instead of diverging). Scoped to
//!   SIMPLE polygons — the boolean's faces are single loops, never holes (the intersection curve crosses
//!   face boundaries, it doesn't form interior islands). So the keyhole/hole machinery and the `tree2d`
//!   BVH (a pure O(n log n) perf optimization — `ear_cost` takes a `max`, order-independent, and far
//!   verts are Delaunay-suppressed, so brute-force is EQUIVALENT) are dropped.
//! - [`triangulate_simple`] — the older textbook Eberly ear clip, kept for its structure-aware fuzz test
//!   (it's the simplest thing that "emits a valid CCW triangulation of the same loop").
//!
//! Input: one loop of 2D points wound CCW (what [`crate::boolean::predicates::get_axis_aligned_projection`]
//! yields for an outward face). Output: triangles as index triples into that loop.

use crate::boolean::predicates::{ccw, determinant2x2};
use crate::linalg::Vec2;

use std::collections::BTreeSet;

/// `la::normalize` guarded against a zero input (`polygon.cpp` `EarClip::SafeNormalize`) — the 2D twin of
/// [`crate::boolean::predicates::safe_normalize`].
#[inline]
fn safe_normalize2(v: Vec2) -> Vec2 {
    let n = v.normalize();
    if n.x.is_finite() { n } else { Vec2::ZERO }
}

/// An entry in the ear priority queue, ordered by `(cost, seq)` (`polygon.cpp`'s `std::multiset<VertItr,
/// MinCost>`). The `seq` reproduces the multiset's insertion-order tie-break among equal costs — a
/// re-queued ear (via [`EarClipper::process_ear`]) gets a fresh higher `seq`, landing at the upper bound
/// of its equal-cost range exactly like the C++ `insert`. `f64` cost is compared by `total_cmp` (finite
/// here), and `vert` is the final disambiguator.
#[derive(Clone, Copy, PartialEq)]
struct Ear {
    cost: f64,
    seq: u64,
    vert: usize,
}
impl Eq for Ear {}
impl Ord for Ear {
    fn cmp(&self, o: &Self) -> core::cmp::Ordering {
        self.cost
            .total_cmp(&o.cost)
            .then(self.seq.cmp(&o.seq))
            .then(self.vert.cmp(&o.vert))
    }
}
impl PartialOrd for Ear {
    fn partial_cmp(&self, o: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// A vertex in the circular linked list being clipped (`polygon.cpp` `EarClip::Vert`), index-based
/// instead of iterator-based. `left`/`right` index the [`EarClipper::verts`] arena.
#[derive(Clone, Copy)]
struct Vert {
    /// Index into the INPUT polygon — what a clipped triangle reports.
    mesh_idx: usize,
    pos: Vec2,
    /// Unit vector toward `right` (`SafeNormalize(right.pos - pos)`), kept in sync by [`EarClipper::link`].
    right_dir: Vec2,
    left: usize,
    right: usize,
    /// This vert's current queue entry, or `None` if not queued (reflex verts aren't queued).
    ear: Option<Ear>,
}

/// The `polygon.cpp` `EarClip` state, scoped to a single simple polygon.
struct EarClipper {
    verts: Vec<Vert>,
    queue: BTreeSet<Ear>,
    seq: u64,
    epsilon: f64,
    tris: Vec<[usize; 3]>,
}

impl EarClipper {
    /// Build the circular list from a CCW loop, then clip any born-degenerate ears.
    fn new(poly: &[Vec2], epsilon: f64) -> EarClipper {
        let n = poly.len();
        let mut verts: Vec<Vert> = (0..n)
            .map(|i| Vert {
                mesh_idx: i,
                pos: poly[i],
                right_dir: Vec2::ZERO,
                left: (i + n - 1) % n,
                right: (i + 1) % n,
                ear: None,
            })
            .collect();
        // Initialize each rightDir (Link would double-write; do it directly).
        for i in 0..n {
            let r = verts[i].right;
            verts[i].right_dir = safe_normalize2(verts[r].pos - verts[i].pos);
        }
        let mut ec = EarClipper {
            verts,
            queue: BTreeSet::new(),
            seq: 0,
            epsilon,
            tris: Vec::with_capacity(n.saturating_sub(2)),
        };
        for v in 0..n {
            ec.clip_if_degenerate(v);
        }
        ec
    }

    // --- circular-list primitives (`Link`/`Clipped`) ---

    #[inline]
    fn clipped(&self, v: usize) -> bool {
        self.verts[self.verts[v].right].left != v
    }

    fn link(&mut self, left: usize, right: usize) {
        self.verts[left].right = right;
        self.verts[right].left = left;
        let dir = safe_normalize2(self.verts[right].pos - self.verts[left].pos);
        self.verts[left].right_dir = dir;
    }

    // --- geometric predicates on a vert (`IsShort`/`IsConvex`) ---

    #[inline]
    fn is_short(&self, v: usize) -> bool {
        let edge = self.verts[self.verts[v].right].pos - self.verts[v].pos;
        edge.dot(edge) * 4.0 < self.epsilon * self.epsilon
    }

    #[inline]
    fn is_convex(&self, v: usize, epsilon: f64) -> bool {
        let (l, r) = (self.verts[v].left, self.verts[v].right);
        ccw(self.verts[l].pos, self.verts[v].pos, self.verts[r].pos, epsilon) >= 0
    }

    // --- ear cost (`SignedDist`/`Cost`/`DelaunayCost`/`EarCost`), brute-force collider ---

    /// `SignedDist` — signed distance of `test` from the `ear` vert along `unit`, walking to `test`'s
    /// neighbours if it's within epsilon of the line (so a touching-but-outside vert stays valid).
    fn signed_dist(&self, ear: usize, test: usize, unit: Vec2) -> f64 {
        let pos = self.verts[ear].pos;
        let d = determinant2x2(unit, self.verts[test].pos - pos);
        if d.abs() < self.epsilon {
            let d_r = determinant2x2(unit, self.verts[self.verts[test].right].pos - pos);
            if d_r.abs() > self.epsilon {
                return d_r;
            }
            let d_l = determinant2x2(unit, self.verts[self.verts[test].left].pos - pos);
            if d_l.abs() > self.epsilon {
                return d_l;
            }
        }
        d
    }

    /// `Cost` — the cost of `test` within the `ear`, the min over the ear's two closed sides + open side.
    fn cost(&self, ear: usize, test: usize, open_side: Vec2) -> f64 {
        let l = self.verts[ear].left;
        let right = self.verts[ear].right;
        let c = self
            .signed_dist(ear, test, self.verts[ear].right_dir)
            .min(self.signed_dist(ear, test, self.verts[l].right_dir));
        let open_cost = determinant2x2(open_side, self.verts[test].pos - self.verts[right].pos);
        c.min(open_cost)
    }

    /// `DelaunayCost` — a Delaunay-condition cost for verts outside the ear (always `< -epsilon`, so it
    /// never affects validity, only prioritization toward cleaner triangles).
    #[inline]
    fn delaunay_cost(diff: Vec2, scale: f64, epsilon: f64) -> f64 {
        -epsilon - scale * diff.dot(diff)
    }

    /// `EarCost` — the priority of clipping `ear`: sharpness (`dot(left.rightDir, rightDir) - 1 - eps`)
    /// worsened toward `0`/positive by any vert inside the ear. Brute-force over all un-clipped verts
    /// (the C++ BVH is a perf-only prune of this `max`; see the module doc).
    fn ear_cost(&self, ear: usize) -> f64 {
        let l = self.verts[ear].left;
        let r = self.verts[ear].right;
        let (lp, rp, ep) = (self.verts[l].pos, self.verts[r].pos, self.verts[ear].pos);
        let mut open_side = lp - rp;
        let center = 0.5 * (lp + rp);
        let scale = 4.0 / open_side.dot(open_side);
        // C++ EarCost uses plain `la::normalize` here (NOT SafeNormalize) — matters on near-degenerate
        // ears, exactly where the divergence lives.
        open_side = open_side.normalize();

        let mut total_cost = self.verts[l].right_dir.dot(self.verts[ear].right_dir) - 1.0 - self.epsilon;
        if ccw(ep, lp, rp, self.epsilon) == 0 {
            return total_cost; // clip folded ears first
        }

        let (mid, lid, rid) = (self.verts[ear].mesh_idx, self.verts[l].mesh_idx, self.verts[r].mesh_idx);
        for test in 0..self.verts.len() {
            let tid = self.verts[test].mesh_idx;
            if self.clipped(test) || tid == mid || tid == lid || tid == rid {
                continue;
            }
            let mut cost = self.cost(ear, test, open_side);
            if cost < -self.epsilon {
                cost = Self::delaunay_cost(self.verts[test].pos - center, scale, self.epsilon);
            }
            if cost > total_cost {
                total_cost = cost;
            }
        }
        total_cost
    }

    // --- clipping (`ClipEar`/`ClipIfDegenerate`) ---

    /// Remove `ear` from the list, emitting the triangle `(left, ear, right)` (`ClipEar`). The
    /// distinct-`mesh_idx` guard filters topological degenerates; a simple polygon never trips it.
    fn clip_ear(&mut self, ear: usize) {
        let (l, r) = (self.verts[ear].left, self.verts[ear].right);
        self.link(l, r);
        let (lid, mid, rid) = (self.verts[l].mesh_idx, self.verts[ear].mesh_idx, self.verts[r].mesh_idx);
        if lid != mid && mid != rid && rid != lid {
            self.tris.push([lid, mid, rid]);
        }
    }

    /// Clip an ear early if it's short or collinear-folded, cascading to its neighbours (`ClipIfDegenerate`).
    fn clip_if_degenerate(&mut self, ear: usize) {
        if self.clipped(ear) {
            return;
        }
        let (l, r) = (self.verts[ear].left, self.verts[ear].right);
        if l == r {
            return;
        }
        let (lp, ep, rp) = (self.verts[l].pos, self.verts[ear].pos, self.verts[r].pos);
        let folded = ccw(lp, ep, rp, self.epsilon) == 0 && (lp - ep).dot(rp - ep) > 0.0;
        if self.is_short(ear) || folded {
            self.clip_ear(ear);
            self.clip_if_degenerate(l);
            self.clip_if_degenerate(r);
        }
    }

    // --- queue management (`ProcessEar`) ---

    /// Recompute `v`'s cost and its place in the ear queue: `kBest` (`-∞`) for short ears, [`EarClipper::ear_cost`]
    /// for convex ears, and un-queued for reflex verts (`ProcessEar`).
    fn process_ear(&mut self, v: usize) {
        if let Some(e) = self.verts[v].ear.take() {
            self.queue.remove(&e);
        }
        let cost = if self.is_short(v) {
            Some(f64::NEG_INFINITY)
        } else if self.is_convex(v, 2.0 * self.epsilon) {
            Some(self.ear_cost(v))
        } else {
            None
        };
        if let Some(cost) = cost {
            let ear = Ear { cost, seq: self.seq, vert: v };
            self.seq += 1;
            self.verts[v].ear = Some(ear);
            self.queue.insert(ear);
        }
    }

    // --- the main loop (`TriangulatePoly`) ---

    /// Ear-clip the whole simple polygon starting the fan at `start` (`TriangulatePoly`): queue every
    /// un-clipped vert, then repeatedly clip the lowest-cost ear and re-cost its neighbours until two
    /// verts remain.
    fn triangulate_poly(&mut self, start: usize) {
        // Queue every un-clipped vert (the `Loop(start, QueueVert)` pass). numTri = count - 2.
        let ring = self.collect_ring(start);
        if ring.len() < 3 {
            return;
        }
        let mut num_tri = ring.len() as i64 - 2;
        for &v in &ring {
            self.process_ear(v);
        }
        // The backup vert (used if the queue empties on a geometrically-invalid polygon).
        let mut backup = *ring.last().unwrap();

        while num_tri > 0 {
            let v = match self.queue.iter().next().copied() {
                Some(e) => {
                    self.queue.remove(&e);
                    self.verts[e.vert].ear = None;
                    e.vert
                }
                None => backup,
            };
            self.clip_ear(v);
            num_tri -= 1;
            let (l, r) = (self.verts[v].left, self.verts[v].right);
            self.process_ear(l);
            self.process_ear(r);
            backup = r;
        }
    }

    /// Collect the un-clipped verts of the ring containing `start`, in fan order (`Loop`, materialized).
    /// Nothing mutates links during the initial queueing, so a plain walk is faithful.
    fn collect_ring(&self, start: usize) -> Vec<usize> {
        // Advance to an un-clipped seed (a clipped `start` points at its replacement via right.left).
        let mut first = start;
        if self.clipped(first) {
            first = self.verts[self.verts[first].right].left;
            if self.clipped(first) {
                return Vec::new();
            }
        }
        let mut ring = Vec::new();
        let mut v = first;
        loop {
            if self.verts[v].right == self.verts[v].left {
                return Vec::new();
            }
            ring.push(v);
            v = self.verts[v].right;
            if v == first {
                break;
            }
        }
        ring
    }
}

/// Triangulate a simple CCW polygon by the Delaunay-cost ear clip (`polygon.cpp` `EarClip`), returning
/// triangles as index triples into `poly`. Reproduces C++'s DIAGONAL choice (lowest-cost ear first), so
/// the boolean's output tessellation matches C++ on 112/120 rotated folds. Fewer than 3 verts → nothing.
///
/// The fan start is the rightmost strictly-reflex vert (falling back to vertex 0) — an approximation of
/// `FindStart` (whose exact form walks `InsideEdge`). Measured to be OUTPUT-IRRELEVANT: the start only
/// reorders equal-cost ears, and the costs here don't tie, so forcing any start gives identical results.
/// That's also why the residual ~8 non-matching folds are NOT a triangulation issue — they're
/// near-degenerate SIMPLIFY collapse-order divergence downstream.
pub fn earclip(poly: &[Vec2], epsilon: f64) -> Vec<[usize; 3]> {
    let n = poly.len();
    if n < 3 {
        return Vec::new();
    }
    let mut ec = EarClipper::new(poly, epsilon);

    // FindStart approximation: the rightmost strictly-reflex vert, else vertex 0. Only the equal-cost
    // tie-break depends on this (the queue picks by cost first), so the strict-reflex test suffices where
    // the exact `InsideEdge` reflex would differ only on collinear verts.
    let mut start = 0usize;
    let mut max_x = f64::NEG_INFINITY;
    for v in 0..n {
        if ec.clipped(v) {
            continue;
        }
        let (l, r) = (ec.verts[v].left, ec.verts[v].right);
        let reflex = ccw(ec.verts[l].pos, ec.verts[v].pos, ec.verts[r].pos, epsilon) < 0;
        if reflex && ec.verts[v].pos.x > max_x {
            max_x = ec.verts[v].pos.x;
            start = v;
        }
    }
    // If `start` got clipped as degenerate, fall back to any un-clipped vert.
    if ec.clipped(start) {
        start = (0..n).find(|&v| !ec.clipped(v)).unwrap_or(0);
    }

    ec.triangulate_poly(start);
    ec.tris
}

/// Is `p` inside (or on the boundary of) the CCW triangle `a, b, c` within `tol`? Uses the shared
/// [`ccw`] predicate on all three edges — `>= 0` on every edge means inside/on, so a point lying on an
/// edge counts as inside (conservative: it blocks the ear rather than clipping through it).
#[inline]
fn point_in_tri(p: Vec2, a: Vec2, b: Vec2, c: Vec2, tol: f64) -> bool {
    ccw(a, b, p, tol) >= 0 && ccw(b, c, p, tol) >= 0 && ccw(c, a, p, tol) >= 0
}

/// Triangulate a simple polygon wound CCW by ear clipping, returning triangles as index triples into
/// `poly`. Fewer than 3 verts triangulate to nothing; exactly 3 is the single triangle. The result
/// always covers the polygon with `n - 2` triangles and is a valid manifold triangulation.
pub fn triangulate_simple(poly: &[Vec2], epsilon: f64) -> Vec<[usize; 3]> {
    let n = poly.len();
    if n < 3 {
        return Vec::new();
    }
    let mut tris = Vec::with_capacity(n - 2);

    // Circular doubly-linked list over the still-uncut verts.
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1) % n).collect();
    let mut prev: Vec<usize> = (0..n).map(|i| (i + n - 1) % n).collect();
    let mut remaining = n;
    let mut cur = 0usize;

    // `scanned` counts how many verts we've tested since the last successful clip; once it exceeds the
    // current ring size, no strict ear exists (a geometrically-invalid loop) and we force-clip to
    // guarantee progress — this can only happen on input GATE-A never produces, but keeps the output
    // manifold if it ever does.
    let mut scanned = 0usize;
    while remaining > 3 {
        let u = prev[cur];
        let w = next[cur];
        let is_ear = ccw(poly[u], poly[cur], poly[w], epsilon) > 0 && {
            // No other remaining vert may lie inside the candidate ear.
            let mut t = next[w];
            let mut empty = true;
            while t != u {
                if point_in_tri(poly[t], poly[u], poly[cur], poly[w], epsilon) {
                    empty = false;
                    break;
                }
                t = next[t];
            }
            empty
        };

        if is_ear || scanned > remaining {
            tris.push([u, cur, w]);
            next[u] = w;
            prev[w] = u;
            remaining -= 1;
            cur = u; // resume from the neighbour — its convexity may have just changed
            scanned = 0;
        } else {
            cur = w;
            scanned += 1;
        }
    }

    // The final three verts form the last triangle.
    let a = cur;
    let b = next[a];
    let c = next[b];
    tris.push([a, b, c]);
    tris
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed area of a CCW triangle in the projected plane (positive ⇒ CCW).
    fn area2(a: Vec2, b: Vec2, c: Vec2) -> f64 {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    }

    /// Sum of triangle areas — must equal the polygon's shoelace area if the triangulation tiles it
    /// exactly with no overlap and no gap.
    fn tri_area_sum(poly: &[Vec2], tris: &[[usize; 3]]) -> f64 {
        tris.iter()
            .map(|t| 0.5 * area2(poly[t[0]], poly[t[1]], poly[t[2]]))
            .sum()
    }

    fn shoelace(poly: &[Vec2]) -> f64 {
        let n = poly.len();
        let mut s = 0.0;
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            s += a.x * b.y - b.x * a.y;
        }
        0.5 * s
    }

    #[test]
    fn triangle_is_itself() {
        let poly = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = triangulate_simple(&poly, 1e-9);
        assert_eq!(tris.len(), 1);
        // Covers the whole triangle, CCW.
        assert!((tri_area_sum(&poly, &tris) - shoelace(&poly)).abs() < 1e-12);
    }

    #[test]
    fn convex_quad_splits_into_two() {
        let poly = [
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = triangulate_simple(&poly, 1e-9);
        assert_eq!(tris.len(), 2);
        assert!((tri_area_sum(&poly, &tris) - shoelace(&poly)).abs() < 1e-12);
        // Every emitted triangle is CCW (positive area).
        for t in &tris {
            assert!(area2(poly[t[0]], poly[t[1]], poly[t[2]]) > 0.0);
        }
    }

    #[test]
    fn nonconvex_l_shape() {
        // An L-shaped hexagon (one reflex vertex) — a naive fan would emit an inverted triangle; the ear
        // clip must not. Wound CCW.
        let poly = [
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(0.0, 2.0),
        ];
        let tris = triangulate_simple(&poly, 1e-9);
        assert_eq!(tris.len(), 4); // n - 2
        // Tiles the L exactly (area 3), and every triangle is CCW (no inverted ear).
        assert!((tri_area_sum(&poly, &tris) - shoelace(&poly)).abs() < 1e-12);
        assert!((shoelace(&poly) - 3.0).abs() < 1e-12);
        for t in &tris {
            assert!(
                area2(poly[t[0]], poly[t[1]], poly[t[2]]) > 0.0,
                "triangle {t:?} is not CCW"
            );
        }
    }

    #[test]
    fn many_vert_convex_polygon() {
        // A regular 12-gon (points on the unit circle, wound CCW): n - 2 triangles, exact area.
        let n = 12;
        let poly: Vec<Vec2> = (0..n)
            .map(|i| {
                let ang = i as f64 / n as f64 * 2.0 * crate::mathf::PI;
                Vec2::new(crate::mathf::cos(ang), crate::mathf::sin(ang))
            })
            .collect();
        let tris = triangulate_simple(&poly, 1e-12);
        assert_eq!(tris.len(), n - 2);
        assert!((tri_area_sum(&poly, &tris) - shoelace(&poly)).abs() < 1e-9);
    }

    proptest::proptest! {
        // polygon_fuzz (M.1.5) — the ear-clip is the one NON-verbatim component, so it gets the
        // structure-aware fuzzer. Star-shaped polygons (points sorted by angle → always simple; random
        // radii → reflex verts + near-degenerate slivers) must always triangulate into n-2 triangles
        // that TILE the polygon exactly with no grossly-inverted triangle.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4096))]
        #[test]
        fn ear_clip_tiles_any_star_polygon(
            // (angular WEIGHT in [1, 1.8], radius). Cumulative weights → angles: the bounded ratio
            // makes every angular gap < π by CONSTRUCTION (max gap = maxW/ΣW ≤ 1.8/(n+0.8) < 0.5 for
            // n ≥ 3), so the origin is strictly interior ⇒ a genuinely SIMPLE star polygon, with ZERO
            // rejection. Random radii create reflex verts + near-degenerate slivers. (A filter-and-reject
            // approach blew proptest's global-reject cap; a non-simple input is outside the ear-clip's
            // contract, so we never generate one.)
            spec in proptest::collection::vec((1.0f64..1.8, 0.15f64..2.0), 3..=16)
        ) {
            let total: f64 = spec.iter().map(|&(w, _)| w).sum();
            let mut acc = 0.0;
            let poly: Vec<Vec2> = spec
                .iter()
                .map(|&(w, r)| {
                    let a = acc / total * 2.0 * crate::mathf::PI;
                    acc += w;
                    Vec2::new(r * crate::mathf::cos(a), r * crate::mathf::sin(a))
                })
                .collect();
            let n = poly.len();
            let area = shoelace(&poly); // > 0 (angle-sorted ⇒ CCW)
            proptest::prop_assume!(area > 1e-6);

            let tris = triangulate_simple(&poly, 1e-12);
            proptest::prop_assert_eq!(tris.len(), n - 2, "wrong triangle count for {}-gon", n);

            // Tiles the polygon exactly (signed tri areas sum to the shoelace area).
            let tol = 1e-9 * area;
            proptest::prop_assert!(
                (tri_area_sum(&poly, &tris) - area).abs() < tol,
                "triangulation does not tile the polygon (area {} vs {})",
                tri_area_sum(&poly, &tris), area
            );
            // No grossly-inverted triangle (a tiny negative sliver is tolerated).
            for t in &tris {
                proptest::prop_assert!(
                    area2(poly[t[0]], poly[t[1]], poly[t[2]]) > -tol,
                    "inverted triangle {:?}", t
                );
            }
        }
    }

    proptest::proptest! {
        // The Delaunay-cost `earclip` (the production path) must ALSO always produce a valid tiling — it
        // has more machinery (cost queue, degenerate pre-clip) that could drop or invert a triangle.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4096))]
        #[test]
        fn earclip_tiles_any_star_polygon(
            spec in proptest::collection::vec((1.0f64..1.8, 0.15f64..2.0), 3..=16)
        ) {
            let total: f64 = spec.iter().map(|&(w, _)| w).sum();
            let mut acc = 0.0;
            let poly: Vec<Vec2> = spec
                .iter()
                .map(|&(w, r)| {
                    let a = acc / total * 2.0 * crate::mathf::PI;
                    acc += w;
                    Vec2::new(r * crate::mathf::cos(a), r * crate::mathf::sin(a))
                })
                .collect();
            let n = poly.len();
            let area = shoelace(&poly);
            proptest::prop_assume!(area > 1e-6);

            let tris = earclip(&poly, 1e-12);
            proptest::prop_assert_eq!(tris.len(), n - 2, "wrong triangle count for {}-gon", n);
            let tol = 1e-9 * area;
            proptest::prop_assert!(
                (tri_area_sum(&poly, &tris) - area).abs() < tol,
                "earclip does not tile the polygon (area {} vs {})",
                tri_area_sum(&poly, &tris), area
            );
            for t in &tris {
                proptest::prop_assert!(
                    area2(poly[t[0]], poly[t[1]], poly[t[2]]) > -tol,
                    "earclip inverted triangle {:?}", t
                );
            }
        }
    }
}
