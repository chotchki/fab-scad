//! Simple-polygon ear clipping — the GATE-A triangulation subset of `polygon.cpp`.
//!
//! Manifold's `EarClip` is the FULL robust triangulator: a `tree2d` BVH to bring ear-validity testing
//! down to O(n log n), keyhole cutting to fold HOLES into their outer contour, and a Delaunay-cost
//! priority queue for triangle quality. GATE-A needs none of that. An offset (general-position)
//! cube∪cube produces cut faces that are SIMPLE loops — the intersection curve crosses face boundaries,
//! it never forms an interior island, so no holes — each only a handful of verts, where an O(n²)
//! ear search is free. And the gate metric is the triangulation-INDEPENDENT residual, so this needn't
//! reproduce Manifold's ear ORDER, only emit a valid CCW triangulation of the same loop.
//!
//! So this is the correctness-proving core, deliberately NOT the verbatim `EarClip`. The BVH + keyhole
//! holes + Delaunay cost are a later determinism task (they change triangle CHOICE, never the covered
//! solid). Input: one loop of 2D points wound CCW (what [`crate::boolean::predicates::get_axis_aligned_projection`]
//! yields for an outward face). Output: triangles as index triples into that loop. Textbook Eberly ear
//! clip with a force-clip fallback, so even a slightly-degenerate loop still terminates with a manifold
//! (if lower-quality) triangulation — Manifold's same "always manifold, matches input edges" guarantee.

use crate::boolean::predicates::ccw;
use crate::linalg::Vec2;

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
}
