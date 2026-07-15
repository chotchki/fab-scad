//! Polygon triangulation — `polygon.cpp`'s `EarClip`, the Delaunay-cost ear clipper with keyhole holes.
//!
//! Entry points:
//! - [`triangulate`] — the verbatim multi-loop `EarClip`: classify each input loop as an outer (CCW,
//!   positive area) or a hole (CW, negative area), CUT A KEYHOLE from each hole to a containing outer
//!   (a zero-width bridge of two duplicated verts, `CutKeyhole`/`FindCloserBridge`/`JoinPolygons`) so the
//!   holed polygon becomes one simple loop, then ear-clip each simple loop lowest-cost-ear first. The cost
//!   is a sharpness + Delaunay metric ([`EarClipper::ear_cost`]); matching C++'s diagonals is what keeps
//!   the boolean's output tessellation byte-identical to C++ (so a chained fold stays bit-identical).
//! - [`earclip`] — a single-loop convenience wrapper over [`triangulate`] (idx = position index), kept for
//!   the structure-aware fuzz test and any simple-polygon caller.
//! - [`triangulate_simple`] — the older textbook Eberly ear clip, kept for its own fuzz test (the simplest
//!   thing that "emits a valid CCW triangulation of one loop").
//!
//! The `tree2d` BVH (`VertCollider`/`QueryTwoDTree`) is a pure O(n log n) perf prune of [`EarClipper::ear_cost`]'s
//! `max` — order-independent, far verts Delaunay-suppressed — so a brute force over the current loop's
//! verts is EQUIVALENT (proven in M.2.2.2). We keep the brute force, scoped to the loop like the collider.
//!
//! Input: loops of 2D points, outer loops wound CCW (what [`crate::boolean::predicates::get_axis_aligned_projection`]
//! yields for an outward face), holes CW. Output: triangles as index triples into the caller's `idx` space.

use crate::boolean::predicates::{ccw, determinant2x2};
use crate::linalg::Vec2;

use std::collections::BTreeSet;

/// One input polygon vertex: its 2D-projected position and the caller's index for it (`polygon.cpp`
/// `PolyVert`/`PolyVertIdx`). A clipped triangle reports these `idx` values; the keyhole bridge duplicates
/// a vert, so two verts can share an `idx` (a zero-width cut — the degenerate-triangle filter drops the
/// sliver).
#[derive(Clone, Copy)]
pub struct PolyVert {
    pub pos: Vec2,
    pub idx: i32,
}

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
/// instead of iterator-based. `left`/`right` index the [`EarClipper::verts`] arena. Unlike the C++
/// iterators, arena indices survive `verts` growth — so `JoinPolygons` can push duplicated verts freely.
#[derive(Clone, Copy)]
struct Vert {
    /// The caller's index — what a clipped triangle reports (duplicated bridge verts share one).
    mesh_idx: i32,
    pos: Vec2,
    /// Unit vector toward `right` (`SafeNormalize(right.pos - pos)`), kept in sync by [`EarClipper::link`].
    right_dir: Vec2,
    left: usize,
    right: usize,
    /// This vert's current queue entry, or `None` if not queued (reflex verts aren't queued).
    ear: Option<Ear>,
}

/// The `polygon.cpp` `EarClip` state over one set of polygons (outers + holes).
struct EarClipper {
    verts: Vec<Vert>,
    queue: BTreeSet<Ear>,
    seq: u64,
    epsilon: f64,
    tris: Vec<[i32; 3]>,
    /// Hole starts (rightmost-reflex vert) + their bounding boxes (min, max), from [`EarClipper::find_start`].
    holes: Vec<(usize, Vec2, Vec2)>,
    /// One start per simple polygon (outers + degenerate-area contours); every one gets ear-clipped.
    simples: Vec<usize>,
    /// One start per positive-area (outer) contour; keyhole bridges attach to these.
    outers: Vec<usize>,
    /// The current simple polygon's vert ring — the scope [`EarClipper::ear_cost`] tests against (the
    /// brute-force stand-in for `VertCollider`; other simples' verts must NOT block this loop's ears).
    active: Vec<usize>,
}

impl EarClipper {
    /// Build the circular lists for all loops (`Initialize`), clip born-degenerate ears, then classify
    /// each loop via [`EarClipper::find_start`]. Loops are appended to one arena; each is linked circularly.
    fn new(polys: &[Vec<PolyVert>], epsilon: f64) -> EarClipper {
        let mut ec = EarClipper {
            verts: Vec::new(),
            queue: BTreeSet::new(),
            seq: 0,
            epsilon,
            tris: Vec::new(),
            holes: Vec::new(),
            simples: Vec::new(),
            outers: Vec::new(),
            active: Vec::new(),
        };
        let mut starts: Vec<usize> = Vec::new();
        let mut bbox_min = Vec2::splat(f64::INFINITY);
        let mut bbox_max = Vec2::splat(f64::NEG_INFINITY);
        for poly in polys {
            if poly.is_empty() {
                continue;
            }
            let base = ec.verts.len();
            let n = poly.len();
            for (i, pv) in poly.iter().enumerate() {
                bbox_min = bbox_min.cmin(pv.pos);
                bbox_max = bbox_max.cmax(pv.pos);
                ec.verts.push(Vert {
                    mesh_idx: pv.idx,
                    pos: pv.pos,
                    right_dir: Vec2::ZERO,
                    left: base + (i + n - 1) % n,
                    right: base + (i + 1) % n,
                    ear: None,
                });
            }
            // Initialize each rightDir (Link would double-write; do it directly).
            for i in 0..n {
                let v = base + i;
                let r = ec.verts[v].right;
                ec.verts[v].right_dir = safe_normalize2(ec.verts[r].pos - ec.verts[v].pos);
            }
            starts.push(base);
        }
        // Auto-epsilon from the global bounding box (`Initialize`: `bBox_.Scale() * kPrecision`).
        if ec.epsilon < 0.0 && !ec.verts.is_empty() {
            let scale = bbox_min
                .x
                .abs()
                .max(bbox_max.x.abs())
                .max(bbox_min.y.abs())
                .max(bbox_max.y.abs());
            ec.epsilon = scale * K_PRECISION;
        }
        // Clip degenerates across ALL loops (C++ order), then classify.
        for v in 0..ec.verts.len() {
            ec.clip_if_degenerate(v);
        }
        for start in starts {
            ec.find_start(start);
        }
        ec
    }

    // --- circular-list primitives (`Link`/`Clipped`/`Loop`) ---

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

    /// The un-clipped verts of the contour containing `start`, in ring order (`Loop`, materialized).
    /// `None` if the contour has degenerated below a triangle (the C++ `polygon_.end()` return). Advances
    /// past a clipped `start` via `right.left`, exactly like `Loop`.
    fn ring(&self, start: usize) -> Option<Vec<usize>> {
        let mut first = start;
        if self.clipped(first) {
            first = self.verts[self.verts[first].right].left;
            if self.clipped(first) {
                return None;
            }
        }
        let mut out = Vec::new();
        let mut v = first;
        loop {
            if self.verts[v].right == self.verts[v].left {
                return None;
            }
            out.push(v);
            v = self.verts[v].right;
            if v == first {
                break;
            }
        }
        Some(out)
    }

    // --- geometric predicates on a vert (`IsShort`/`IsConvex`/`IsReflex`/`InsideEdge`/`InterpY2X`) ---

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

    /// Reflex test that walks to certainty (`IsReflex` = `!left.InsideEdge(left.right, eps, true)`) — subtly
    /// stricter than `!is_convex` (which calls a colinear vert convex).
    #[inline]
    fn is_reflex(&self, v: usize) -> bool {
        let l = self.verts[v].left;
        let lr = self.verts[l].right;
        !self.inside_edge(l, lr, true)
    }

    /// Is vert `this` on the inside of the edge `tail → tail.right`, walking the contour until an
    /// answer is clear beyond epsilon (`Vert::InsideEdge`). `to_left` walks `this`'s side leftward. Verbatim.
    fn inside_edge(&self, this: usize, tail: usize, to_left: bool) -> bool {
        let p2 = self.epsilon * self.epsilon;
        let mut next_l = self.verts[self.verts[this].left].right;
        let mut next_r = self.verts[tail].right;
        let mut center = tail;
        let mut last = center;
        let stop = if to_left { self.verts[this].right } else { self.verts[this].left };

        while next_l != next_r && tail != next_r && next_l != stop {
            let edge_l = self.verts[next_l].pos - self.verts[center].pos;
            let l2 = edge_l.dot(edge_l);
            if l2 <= p2 {
                next_l = if to_left { self.verts[next_l].left } else { self.verts[next_l].right };
                continue;
            }
            let edge_r = self.verts[next_r].pos - self.verts[center].pos;
            let r2 = edge_r.dot(edge_r);
            if r2 <= p2 {
                next_r = self.verts[next_r].right;
                continue;
            }
            let vec_lr = self.verts[next_r].pos - self.verts[next_l].pos;
            let lr2 = vec_lr.dot(vec_lr);
            if lr2 <= p2 {
                last = center;
                center = next_l;
                next_l = if to_left { self.verts[next_l].left } else { self.verts[next_l].right };
                if next_l == next_r {
                    break;
                }
                next_r = self.verts[next_r].right;
                continue;
            }
            let mut convexity = ccw(
                self.verts[next_l].pos,
                self.verts[center].pos,
                self.verts[next_r].pos,
                self.epsilon,
            );
            if center != last {
                convexity += ccw(self.verts[last].pos, self.verts[center].pos, self.verts[next_l].pos, self.epsilon)
                    + ccw(self.verts[next_r].pos, self.verts[center].pos, self.verts[last].pos, self.epsilon);
            }
            if convexity != 0 {
                return convexity > 0;
            }
            if l2 < r2 {
                center = next_l;
                next_l = if to_left { self.verts[next_l].left } else { self.verts[next_l].right };
            } else {
                center = next_r;
                next_r = self.verts[next_r].right;
            }
            last = center;
        }
        true // wholly degenerate contour — treat as convex
    }

    /// The x where edge `edge → edge.right` crosses `start.y` from below to above, right of `start`, or
    /// `NaN` otherwise, within epsilon (`Vert::InterpY2X`). `on_top` restricts which end may terminate in
    /// the epsilon band.
    fn interp_y2x(&self, edge: usize, start: Vec2, on_top: i32) -> f64 {
        let pos = self.verts[edge].pos;
        let rpos = self.verts[self.verts[edge].right].pos;
        let eps = self.epsilon;
        if (pos.y - start.y).abs() <= eps {
            if rpos.y <= start.y + eps || on_top == 1 {
                f64::NAN
            } else {
                pos.x
            }
        } else if pos.y < start.y - eps {
            if rpos.y > start.y + eps {
                pos.x + (start.y - pos.y) * (rpos.x - pos.x) / (rpos.y - pos.y)
            } else if rpos.y < start.y - eps || on_top == -1 {
                f64::NAN
            } else {
                rpos.x
            }
        } else {
            f64::NAN
        }
    }

    // --- ear cost (`SignedDist`/`Cost`/`DelaunayCost`/`EarCost`) ---

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
    /// worsened toward `0`/positive by any vert inside the ear. Brute-force over the CURRENT loop's verts
    /// ([`EarClipper::active`], the collider-scope stand-in — see the module doc).
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
        for &test in &self.active {
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
    /// distinct-`mesh_idx` guard filters topological degenerates from hole vert duplication.
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

    // --- loop classification (`FindStart`) ---

    /// Classify the contour at `start`: compute its signed area (Kahan) + bounding box + rightmost-reflex
    /// start, then file it as a hole (negative area) or a simple/outer (positive) (`FindStart`).
    fn find_start(&mut self, first: usize) {
        let ring = match self.ring(first) {
            Some(r) => r,
            None => return, // fully clipped away
        };
        let origin = self.verts[first].pos;
        let mut start = first;
        let mut max_x = f64::NEG_INFINITY;
        let mut bmin = Vec2::splat(f64::INFINITY);
        let mut bmax = Vec2::splat(f64::NEG_INFINITY);
        // Kahan summation of the shoelace area.
        let mut area = 0.0;
        let mut comp = 0.0;
        for &v in &ring {
            let p = self.verts[v].pos;
            bmin = bmin.cmin(p);
            bmax = bmax.cmax(p);
            let r = self.verts[v].right;
            let area1 = determinant2x2(p - origin, self.verts[r].pos - origin);
            let t1 = area + area1;
            comp += (area - t1) + area1;
            area = t1;
            if p.x > max_x && self.is_reflex(v) {
                max_x = p.x;
                start = v;
            }
        }
        area += comp;
        let size = bmax - bmin;
        let min_area = self.epsilon * size.x.max(size.y);
        if max_x.is_finite() && area < -min_area {
            self.holes.push((start, bmin, bmax));
        } else {
            self.simples.push(start);
            if area > min_area {
                self.outers.push(start);
            }
        }
    }

    // --- keyhole cutting (`CutKeyhole`/`FindCloserBridge`/`JoinPolygons`) ---

    /// Attach a hole to an outer contour by a zero-width bridge (`CutKeyhole`): ray-cast right from the
    /// hole's rightmost vert to the nearest outer edge, refine the bridge vert, then splice.
    fn cut_keyhole(&mut self, start: usize, bmin: Vec2, bmax: Vec2) {
        let sp = self.verts[start].pos;
        let eps = self.epsilon;
        let on_top = if sp.y >= bmax.y - eps {
            1
        } else if sp.y <= bmin.y + eps {
            -1
        } else {
            0
        };
        let mut connector: Option<usize> = None;
        for oi in 0..self.outers.len() {
            let first = self.outers[oi];
            let Some(ring) = self.ring(first) else { continue };
            for &edge in &ring {
                let x = self.interp_y2x(edge, sp, on_top);
                if !x.is_finite() || !self.inside_edge(start, edge, true) {
                    continue;
                }
                let take = match connector {
                    None => true,
                    Some(c) => {
                        let cr = self.verts[c].right;
                        if ccw(Vec2::new(x, sp.y), self.verts[c].pos, self.verts[cr].pos, eps) == 1 {
                            true
                        } else if self.verts[c].pos.y < self.verts[edge].pos.y {
                            self.inside_edge(edge, c, false)
                        } else {
                            !self.inside_edge(c, edge, false)
                        }
                    }
                };
                if take {
                    connector = Some(edge);
                }
            }
        }

        let Some(connector) = connector else {
            // Hole found no outer — fall back to triangulating it alone (C++ pushes to simples_).
            self.simples.push(start);
            return;
        };
        let connector = self.find_closer_bridge(start, connector);
        self.join_polygons(start, connector);
    }

    /// Refine the initial bridge edge into the exact bridge VERT (`FindCloserBridge`): pick the closer
    /// endpoint, then walk the outer for any reflex vert inside the triangle of start + that endpoint.
    fn find_closer_bridge(&mut self, start: usize, edge: usize) -> usize {
        let sp = self.verts[start].pos;
        let eps = self.epsilon;
        let er = self.verts[edge].right;
        // C++ ternary chain: `edge.x<start ? er : er.x<start ? edge : er.y-start.y>start.y-edge.y ? edge : er`.
        // The two `edge` arms are merged (`||`, same short-circuit order) to satisfy clippy.
        let mut connector = if self.verts[edge].pos.x < sp.x {
            er
        } else if self.verts[er].pos.x < sp.x
            || self.verts[er].pos.y - sp.y > sp.y - self.verts[edge].pos.y
        {
            edge
        } else {
            er
        };
        if (self.verts[connector].pos.y - sp.y).abs() <= eps {
            return connector;
        }
        let above = if self.verts[connector].pos.y > sp.y { 1.0 } else { -1.0 };

        for oi in 0..self.outers.len() {
            let first = self.outers[oi];
            let Some(ring) = self.ring(first) else { continue };
            for &vert in &ring {
                let vp = self.verts[vert].pos;
                let cp = self.verts[connector].pos;
                let inside = above * ccw(sp, vp, cp, eps) as f64;
                if vp.x > sp.x - eps
                    && vp.y * above > sp.y * above - eps
                    && (inside > 0.0
                        || (inside == 0.0 && vp.x < cp.x && vp.y * above < cp.y * above))
                    && self.inside_edge(vert, edge, true)
                    && self.is_reflex(vert)
                {
                    connector = vert;
                }
            }
        }
        connector
    }

    /// Splice a hole into an outer via a keyhole bridge (`JoinPolygons`): duplicate both bridge verts and
    /// re-link, turning two contours into one, then clip any degenerate ears the bridge created.
    fn join_polygons(&mut self, start: usize, connector: usize) {
        let new_start = self.verts.len();
        self.verts.push(self.verts[start]);
        let new_connector = self.verts.len();
        self.verts.push(self.verts[connector]);

        let start_r = self.verts[start].right;
        self.verts[start_r].left = new_start;
        let conn_l = self.verts[connector].left;
        self.verts[conn_l].right = new_connector;
        self.link(start, connector);
        self.link(new_connector, new_start);

        self.clip_if_degenerate(start);
        self.clip_if_degenerate(new_start);
        self.clip_if_degenerate(connector);
        self.clip_if_degenerate(new_connector);
    }

    // --- queue management + main loop (`ProcessEar`/`TriangulatePoly`/`Triangulate`) ---

    /// Recompute `v`'s cost and its place in the ear queue: `kBest` (`-∞`) for short ears,
    /// [`EarClipper::ear_cost`] for convex ears, and un-queued for reflex verts (`ProcessEar`).
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

    /// Ear-clip one simple polygon starting the fan at `start` (`TriangulatePoly`): scope `ear_cost` to
    /// this loop's verts, queue every un-clipped vert, then repeatedly clip the lowest-cost ear and re-cost
    /// its neighbours until two verts remain.
    fn triangulate_poly(&mut self, start: usize) {
        let ring = match self.ring(start) {
            Some(r) if r.len() >= 3 => r,
            _ => return,
        };
        self.active = ring.clone();
        self.queue.clear();
        let mut num_tri = ring.len() as i64 - 2;
        for &v in &ring {
            self.process_ear(v);
        }
        // Backup vert (used if the queue empties on a geometrically-invalid polygon).
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

    /// The `Triangulate` driver: cut every hole (rightmost-x first), then ear-clip every simple polygon.
    fn run(&mut self) -> Vec<[i32; 3]> {
        // Holes cut in descending-x order (C++ `std::multiset<VertItr, MaxX>`); stable for x-ties.
        let mut holes = std::mem::take(&mut self.holes);
        holes.sort_by(|a, b| self.verts[b.0].pos.x.total_cmp(&self.verts[a.0].pos.x));
        for (start, bmin, bmax) in holes {
            self.cut_keyhole(start, bmin, bmax);
        }
        let simples = std::mem::take(&mut self.simples);
        for start in simples {
            self.triangulate_poly(start);
        }
        std::mem::take(&mut self.tris)
    }
}

/// `kPrecision` (`common.h`) — the auto-epsilon multiple of the bounding-box scale.
const K_PRECISION: f64 = 1e-12;

/// Triangulate a set of polygons — outer loops wound CCW, holes CW — by the Delaunay-cost ear clip with
/// keyhole holes (`polygon.cpp` `TriangulateIdx`/`EarClip::Triangulate`). Returns triangles as triples of
/// the input `PolyVert::idx` values. Empty input → nothing.
pub fn triangulate(polys: &[Vec<PolyVert>], epsilon: f64) -> Vec<[i32; 3]> {
    if polys.iter().all(|p| p.len() < 3) && polys.iter().map(|p| p.len()).sum::<usize>() < 3 {
        return Vec::new();
    }
    let mut ec = EarClipper::new(polys, epsilon);
    ec.run()
}

/// Triangulate a single simple CCW polygon by the Delaunay-cost ear clip (`polygon.cpp` `EarClip`),
/// returning triangles as index triples into `poly`. A thin wrapper over [`triangulate`] (idx = position
/// index). Fewer than 3 verts → nothing.
pub fn earclip(poly: &[Vec2], epsilon: f64) -> Vec<[usize; 3]> {
    if poly.len() < 3 {
        return Vec::new();
    }
    let loop_poly: Vec<PolyVert> = poly
        .iter()
        .enumerate()
        .map(|(i, &pos)| PolyVert { pos, idx: i as i32 })
        .collect();
    triangulate(&[loop_poly], epsilon)
        .into_iter()
        .map(|[a, b, c]| [a as usize, b as usize, c as usize])
        .collect()
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
    fn tri_area_sum(pts: &[Vec2], tris: &[[usize; 3]]) -> f64 {
        tris.iter()
            .map(|t| 0.5 * area2(pts[t[0]], pts[t[1]], pts[t[2]]))
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

    /// A square with a concentric square hole must triangulate the ANNULUS (keyhole path): the tris tile
    /// area = outer² − inner², all CCW, and the topology is a valid ring (2·(4+4) − ... via the bridge).
    #[test]
    fn square_with_square_hole_tiles_the_annulus() {
        let s = 4.0; // outer half-extent
        let h = 1.0; // hole half-extent
        // Outer CCW, hole CW (reversed). idx: outer 0..4, hole 4..8.
        let outer = vec![
            PolyVert { pos: Vec2::new(-s, -s), idx: 0 },
            PolyVert { pos: Vec2::new(s, -s), idx: 1 },
            PolyVert { pos: Vec2::new(s, s), idx: 2 },
            PolyVert { pos: Vec2::new(-s, s), idx: 3 },
        ];
        let hole = vec![
            PolyVert { pos: Vec2::new(-h, -h), idx: 4 },
            PolyVert { pos: Vec2::new(-h, h), idx: 5 },
            PolyVert { pos: Vec2::new(h, h), idx: 6 },
            PolyVert { pos: Vec2::new(h, -h), idx: 7 },
        ];
        let pts: Vec<Vec2> = outer.iter().chain(hole.iter()).map(|v| v.pos).collect();
        let tris_i = triangulate(&[outer, hole], 1e-9);
        let tris: Vec<[usize; 3]> = tris_i.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();

        let annulus = (2.0 * s) * (2.0 * s) - (2.0 * h) * (2.0 * h); // 64 - 4 = 60
        let sum = tri_area_sum(&pts, &tris);
        assert!(
            (sum - annulus).abs() < 1e-9,
            "annulus area {sum} != {annulus} ({} tris)",
            tris.len()
        );
        // Keyhole bridges 2 verts → a holed poly of V verts + 1 hole triangulates to V triangles.
        assert_eq!(tris.len(), 8, "expected V=8 triangles for a 4+4 holed square");
        // No grossly-inverted triangle.
        for t in &tris {
            assert!(area2(pts[t[0]], pts[t[1]], pts[t[2]]) > -1e-9, "inverted triangle {t:?}");
        }
    }

    /// An off-center hole (worst case for the rightmost-ray keyhole: the hole's rightmost vert doesn't
    /// align with an outer vert) still tiles the annulus.
    #[test]
    fn offset_hole_tiles_the_annulus() {
        let outer = vec![
            PolyVert { pos: Vec2::new(0.0, 0.0), idx: 0 },
            PolyVert { pos: Vec2::new(10.0, 0.0), idx: 1 },
            PolyVert { pos: Vec2::new(10.0, 6.0), idx: 2 },
            PolyVert { pos: Vec2::new(0.0, 6.0), idx: 3 },
        ];
        // CW hole near the right side.
        let hole = vec![
            PolyVert { pos: Vec2::new(6.0, 2.0), idx: 4 },
            PolyVert { pos: Vec2::new(6.0, 4.0), idx: 5 },
            PolyVert { pos: Vec2::new(8.5, 4.0), idx: 6 },
            PolyVert { pos: Vec2::new(8.5, 2.0), idx: 7 },
        ];
        let pts: Vec<Vec2> = outer.iter().chain(hole.iter()).map(|v| v.pos).collect();
        let tris_i = triangulate(&[outer, hole], 1e-9);
        let tris: Vec<[usize; 3]> = tris_i.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
        let annulus = 10.0 * 6.0 - 2.5 * 2.0; // 60 - 5 = 55
        let sum = tri_area_sum(&pts, &tris);
        assert!((sum - annulus).abs() < 1e-9, "annulus area {sum} != {annulus}");
        assert_eq!(tris.len(), 8);
        for t in &tris {
            assert!(area2(pts[t[0]], pts[t[1]], pts[t[2]]) > -1e-9, "inverted triangle {t:?}");
        }
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

    proptest::proptest! {
        // Keyhole fuzz: a big CCW outer square with ONE randomly-placed, randomly-sized CW square hole
        // strictly inside must always tile the annulus (area = outer − hole), no grossly-inverted tri.
        // Exercises CutKeyhole/FindCloserBridge/JoinPolygons on off-axis holes.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2048))]
        #[test]
        fn keyhole_tiles_square_with_one_hole(
            cx in 3.0f64..7.0, cy in 3.0f64..7.0, hw in 0.4f64..1.6, hh in 0.4f64..1.6,
        ) {
            // Outer square [0,10]², CCW.
            let outer = vec![
                PolyVert { pos: Vec2::new(0.0, 0.0), idx: 0 },
                PolyVert { pos: Vec2::new(10.0, 0.0), idx: 1 },
                PolyVert { pos: Vec2::new(10.0, 10.0), idx: 2 },
                PolyVert { pos: Vec2::new(0.0, 10.0), idx: 3 },
            ];
            // Hole centered (cx,cy), half-extents (hw,hh), wound CW.
            let hole = vec![
                PolyVert { pos: Vec2::new(cx - hw, cy - hh), idx: 4 },
                PolyVert { pos: Vec2::new(cx - hw, cy + hh), idx: 5 },
                PolyVert { pos: Vec2::new(cx + hw, cy + hh), idx: 6 },
                PolyVert { pos: Vec2::new(cx + hw, cy - hh), idx: 7 },
            ];
            let pts: Vec<Vec2> = outer.iter().chain(hole.iter()).map(|v| v.pos).collect();
            let tris_i = triangulate(&[outer, hole], 1e-9);
            let tris: Vec<[usize; 3]> = tris_i.iter().map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
            let annulus = 100.0 - (2.0 * hw) * (2.0 * hh);
            let sum = tri_area_sum(&pts, &tris);
            proptest::prop_assert!(
                (sum - annulus).abs() < 1e-7,
                "annulus area {} != {} (hole {:?})", sum, annulus, (cx, cy, hw, hh)
            );
            for t in &tris {
                proptest::prop_assert!(
                    area2(pts[t[0]], pts[t[1]], pts[t[2]]) > -1e-7,
                    "inverted triangle {:?}", t
                );
            }
        }
    }
}
