//! 2D polygon offset — a verbatim f64 port of Clipper2's `ClipperOffset` polygon walk
//! (`clipper.offset.cpp`, the engine the reference Manifold's `CrossSection::Offset` drives via
//! `InflatePaths`). M.5.4.1.
//!
//! WHY a port and not i_overlay's `outline`: the K.6 gate demands offset AREA parity with
//! OpenSCAD/Clipper2 (the 78.2548 jtSquare canary), and join-corner geometry is engine-defined —
//! i_overlay's Miter is a turn-angle threshold (not Clipper2's ratio limit) and it has no Square
//! join at all. Porting the walk makes all four join types (Square/Round/Miter/Bevel) produce
//! Clipper2's exact corner geometry; the finishing Positive-fill union (which removes the negative
//! regions the concave-corner rule inserts) runs through i_overlay like every other boolean here.
//!
//! Deviations from the C++, all documented at their sites:
//! - f64 throughout, no int64 grid: Clipper2 scales coords by 10^precision (Manifold drives it at
//!   precision 8) and rounds every emitted point to that grid; our grid lives inside i_overlay's
//!   boolean instead. `GRID` below reproduces the two places the SCALED magnitude matters (the
//!   near-zero-delta short-circuit and the round-join steps cap).
//! - Polygon paths only: the open-path end caps (`OffsetOpenPath`/`OffsetOpenJoined`) and the
//!   single-point circle/square paths are not ported — a normalized `CrossSection` contour always
//!   has ≥ 3 vertices. The `j == k` arms of DoSquare/DoBevel/DoRound (end-cap-only) are omitted
//!   with them.
//! - No delta callback, no Z, no PolyTree output.
//! - The int grid also guarantees `GetUnitNormal` a hypot ≥ 1 between distinct points; in raw f64,
//!   edges shorter than ~2⁻⁵³⁷ (coordinates around 1e-150) underflow `dx²+dy²` to 0 and produce
//!   non-finite normals. Geometry at that scale is outside the supported coordinate range (debug
//!   builds panic in the finishing union; the C++ collapses such paths to one grid point instead).

use crate::cross_section::JoinType;
use crate::linalg::Vec2;
use crate::mathf;

/// `clipper.offset.cpp` `floating_point_tolerance`.
const FLOATING_POINT_TOLERANCE: f64 = 1e-12;
/// `clipper.offset.cpp` `arc_const` — default arc tolerance as a fraction of the radius (1/500).
const ARC_CONST: f64 = 0.002;
/// The int64 grid scale Manifold runs Clipper2 at (`cross_section.cpp` `precision_ = 8` →
/// coordinates ×10⁸). Only consulted where Clipper2's logic depends on the SCALED magnitude.
const GRID: f64 = 1e8;

/// π — offset math runs in radians (the walk), unlike the degree-based constructors.
const PI: f64 = core::f64::consts::PI;

/// `StripDuplicates(path, is_joined = true)`: drop consecutive duplicate vertices and, for closed
/// paths, a duplicated closing vertex. Exact `==` — the C++ compares int-grid coords, we compare
/// the raw f64s (the grid deviation above).
fn strip_duplicates(path: &[Vec2]) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(path.len());
    for &p in path {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    while out.len() > 1 && out.last() == Some(&out[0]) {
        out.pop();
    }
    out
}

/// `GetLowestClosedPathInfo`: find the path owning the extreme bottom point (per Clipper2's
/// comparison; x-min tie-break) and report whether that path is negatively wound — the lowest path
/// must be an outer, so its winding tells whether the whole set is reversed. Returns `None` when
/// every path is degenerate (zero area).
fn lowest_path_is_reversed(paths: &[Vec<Vec2>]) -> Option<bool> {
    let mut bot = Vec2::new(f64::INFINITY, f64::NEG_INFINITY);
    let mut found = false;
    let mut is_neg = false;
    for path in paths {
        let mut area = f64::MAX;
        for &pt in path {
            if pt.y < bot.y || (pt.y == bot.y && pt.x >= bot.x) {
                continue;
            }
            if area == f64::MAX {
                area = signed_area(path);
                if area == 0.0 {
                    break; // invalid closed path
                }
                is_neg = area < 0.0;
            }
            found = true;
            bot = pt;
        }
    }
    found.then_some(is_neg)
}

/// Signed shoelace area (Clipper2 `Area`) — CCW ⇒ positive.
fn signed_area(c: &[Vec2]) -> f64 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = c[i];
        let q = c[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    0.5 * a
}

/// `GetUnitNormal(pt1, pt2)` = `(dy, -dx) / hypot` — the edge's outward normal for a CCW walk.
/// Hypot per the C++'s own `sqrt(x·x + y·y)` (NOT `f64::hypot`, whose rounding differs).
fn unit_normal(p1: Vec2, p2: Vec2) -> Vec2 {
    if p1 == p2 {
        return Vec2::ZERO;
    }
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let inv = 1.0 / (dx * dx + dy * dy).sqrt();
    Vec2::new(dy * inv, -(dx * inv))
}

/// `GetAvgUnitVector` (with its `AlmostZero(hypot, 0.001)` zero-guard).
fn avg_unit_vector(v1: Vec2, v2: Vec2) -> Vec2 {
    let s = v1 + v2;
    let h = (s.x * s.x + s.y * s.y).sqrt();
    if h.abs() < 0.001 {
        return Vec2::ZERO;
    }
    s * (1.0 / h)
}

/// The simple (non-HI_PRECISION, Clipper2's default) `GetLineIntersectPt`: intersection of the two
/// infinite lines, constrained to segment 1; `None` on parallel (caller keeps its fallback point).
fn line_intersect(ln1a: Vec2, ln1b: Vec2, ln2a: Vec2, ln2b: Vec2) -> Option<Vec2> {
    let dx1 = ln1b.x - ln1a.x;
    let dy1 = ln1b.y - ln1a.y;
    let dx2 = ln2b.x - ln2a.x;
    let dy2 = ln2b.y - ln2a.y;
    let det = dy1 * dx2 - dy2 * dx1;
    if det == 0.0 {
        return None;
    }
    let t = ((ln1a.x - ln2a.x) * dy2 - (ln1a.y - ln2a.y) * dx2) / det;
    Some(if t <= 0.0 {
        ln1a
    } else if t >= 1.0 {
        ln1b
    } else {
        Vec2::new(ln1a.x + t * dx1, ln1a.y + t * dy1)
    })
}

/// `ReflectPoint(pt, pivot)`.
fn reflect(pt: Vec2, pivot: Vec2) -> Vec2 {
    Vec2::new(pivot.x + (pivot.x - pt.x), pivot.y + (pivot.y - pt.y))
}

/// The per-run walk state — `ClipperOffset`'s members that the corner emitters read.
struct OffsetWalk {
    group_delta: f64,
    join_type: JoinType,
    /// Miter ratio-limit test constant: `miter_limit <= 1 ? 2 : 2/ml²`; miter applies while
    /// `cos_a > temp_lim - 1`.
    temp_lim: f64,
    steps_per_rad: f64,
    step_sin: f64,
    step_cos: f64,
    norms: Vec<Vec2>,
    path_out: Vec<Vec2>,
}

impl OffsetWalk {
    /// `BuildNormals`.
    fn build_normals(&mut self, path: &[Vec2]) {
        self.norms.clear();
        self.norms.reserve(path.len());
        let n = path.len();
        for i in 0..n {
            self.norms.push(unit_normal(path[i], path[(i + 1) % n]));
        }
    }

    /// `DoBevel` (closed-path arm).
    fn do_bevel(&mut self, path: &[Vec2], j: usize, k: usize) {
        self.path_out
            .push(path[j] + self.norms[k] * self.group_delta);
        self.path_out
            .push(path[j] + self.norms[j] * self.group_delta);
    }

    /// `DoSquare` (closed-path arm): flat cap perpendicular to the corner bisector, tangent to the
    /// delta circle — Clipper2's jtSquare, the OpenSCAD `offset(delta, chamfer=true)` geometry.
    fn do_square(&mut self, path: &[Vec2], j: usize, k: usize) {
        let vec = avg_unit_vector(
            Vec2::new(-self.norms[k].y, self.norms[k].x),
            Vec2::new(self.norms[j].y, -self.norms[j].x),
        );
        let abs_delta = self.group_delta.abs();
        // Offset the original vertex delta units along the bisector, then cap through that point.
        let ptq = path[j] + vec * abs_delta;
        let pt1 = ptq + Vec2::new(vec.y, -vec.x) * self.group_delta;
        let pt2 = ptq + Vec2::new(-vec.y, vec.x) * self.group_delta;
        // Two vertices along the incoming edge's offset line.
        let pt3 = path[k] + self.norms[k] * self.group_delta;
        let pt4 = path[j] + self.norms[k] * self.group_delta;
        let pt = line_intersect(pt1, pt2, pt3, pt4).unwrap_or(ptq);
        self.path_out.push(pt);
        // The second cap corner through reflection about the bisector point.
        self.path_out.push(reflect(pt, ptq));
    }

    /// `DoMiter`: the sharp corner point `path[j] + (norms[k] + norms[j]) · delta/(cos_a + 1)`.
    fn do_miter(&mut self, path: &[Vec2], j: usize, k: usize, cos_a: f64) {
        let q = self.group_delta / (cos_a + 1.0);
        self.path_out
            .push(path[j] + (self.norms[k] + self.norms[j]) * q);
    }

    /// `DoRound`: arc from the incoming edge's offset to the outgoing edge's, stepped by the
    /// precomputed per-group rotation.
    fn do_round(&mut self, path: &[Vec2], j: usize, k: usize, angle: f64) {
        let pt = path[j];
        let mut offset_vec = self.norms[k] * self.group_delta;
        self.path_out.push(pt + offset_vec);
        let steps = (self.steps_per_rad * angle.abs()).ceil() as i64;
        for _ in 1..steps {
            offset_vec = Vec2::new(
                offset_vec.x * self.step_cos - self.step_sin * offset_vec.y,
                offset_vec.x * self.step_sin + offset_vec.y * self.step_cos,
            );
            self.path_out.push(pt + offset_vec);
        }
        self.path_out.push(pt + self.norms[j] * self.group_delta);
    }

    /// `OffsetPoint`: the corner dispatcher — concave corners insert 3 points forming negative
    /// regions (removed by the finishing union), near-straight corners always miter, else the
    /// group's join type decides.
    fn offset_point(&mut self, path: &[Vec2], j: usize, k: usize) {
        if path[j] == path[k] {
            return;
        }
        // TRAP: Clipper2's 2-arg CrossProduct is `v1.y·v2.x − v2.y·v1.x` — the NEGATED standard
        // cross product (clipper.core.h:818). Port the formula, not the name: with the standard
        // orientation every convexity test inverts and grown corners come out concave (area 65,
        // not the 78.25 canary).
        let sin_a = (self.norms[j].y * self.norms[k].x - self.norms[k].y * self.norms[j].x)
            .clamp(-1.0, 1.0);
        let cos_a = self.norms[j].x * self.norms[k].x + self.norms[j].y * self.norms[k].y;

        if self.group_delta.abs() <= FLOATING_POINT_TOLERANCE {
            self.path_out.push(path[j]);
            return;
        }

        if cos_a > -0.999 && (sin_a * self.group_delta < 0.0) {
            // Concave: 3 points producing a negative region, removed by the finishing union.
            self.path_out
                .push(path[j] + self.norms[k] * self.group_delta);
            self.path_out.push(path[j]);
            self.path_out
                .push(path[j] + self.norms[j] * self.group_delta);
        } else if cos_a > 0.999 && self.join_type != JoinType::Round {
            // Almost straight (< 2.5°) — miter unconditionally.
            self.do_miter(path, j, k, cos_a);
        } else if self.join_type == JoinType::Miter {
            if cos_a > self.temp_lim - 1.0 {
                self.do_miter(path, j, k, cos_a);
            } else {
                self.do_square(path, j, k);
            }
        } else if self.join_type == JoinType::Round {
            self.do_round(path, j, k, mathf::atan2(sin_a, cos_a));
        } else if self.join_type == JoinType::Bevel {
            self.do_bevel(path, j, k);
        } else {
            self.do_square(path, j, k);
        }
    }

    /// `OffsetPolygon`: walk every vertex with its predecessor.
    fn offset_polygon(&mut self, path: &[Vec2]) {
        self.path_out.clear();
        let n = path.len();
        let mut k = n - 1;
        for j in 0..n {
            self.offset_point(path, j, k);
            k = j;
        }
    }
}

/// Offset a set of closed polygon contours by `delta` (Clipper2 `InflatePaths` at Manifold's
/// parameters, polygon end type). Returns the RAW offset paths plus whether the input was reversed
/// (negatively wound outer) — the caller must finish with a self-union under the Positive fill rule
/// (Negative when reversed) to remove the concave-corner negative regions.
///
/// `steps_per_360` is the Round join's segments-per-circle, taken DIRECTLY (≤ 0 → Clipper2's
/// arc-tolerance default, `|δ|/500` sagitta). DEVIATION with rationale: Manifold encodes the count
/// as `arcTol = |δ|·(1 − cos(π/n))` purely to smuggle n through Clipper2's tolerance API, and
/// Clipper2 decodes with `π/acos(1 − arcTol/|δ|)` — an `acos∘cos` round-trip that must land on n
/// EXACTLY or `ceil` mints an extra arc step (measured: our libm lands at n + 3.6e-14 → 6 steps per
/// 90° corner where the C++ gets 5). Both sides live in this crate, so we pass n and skip the
/// platform-libm razor edge — the count is what was always meant.
pub(crate) fn offset_polygons(
    contours: &[Vec<Vec2>],
    delta: f64,
    join_type: JoinType,
    miter_limit: f64,
    steps_per_360: f64,
) -> (Vec<Vec<Vec2>>, bool) {
    // Group ctor: strip duplicates; a contour degenerating below 3 verts can't be offset as a
    // polygon (the C++ 1/2-point special cases are end-cap territory — see the module doc).
    let paths: Vec<Vec<Vec2>> = contours
        .iter()
        .map(|c| strip_duplicates(c))
        .filter(|c| c.len() >= 3)
        .collect();
    if paths.is_empty() {
        return (paths, false);
    }

    // DoGroupOffset's orientation read. A group where EVERY path has exactly zero signed area
    // (e.g. a collinear contour) has no orientation to read — Clipper2 then forces the delta
    // POSITIVE (`if (!group.lowest_path_idx.has_value()) delta_ = std::abs(delta_)`,
    // clipper.offset.cpp:461), so a degenerate contour still grows into its sausage under a
    // negative delta instead of emitting a reverse-wound loop the finishing union would delete.
    // (M.5.4 verification finding — confirmed differentially vs the compiled C++.)
    let (delta, is_reversed) = match lowest_path_is_reversed(&paths) {
        Some(rev) => (delta, rev),
        None => (delta.abs(), false),
    };

    // ExecuteInternal: an offset under half an int-grid unit is insignificant — copy through.
    if delta.abs() * GRID < 0.5 {
        return (paths, is_reversed);
    }

    let mut walk = OffsetWalk {
        group_delta: if is_reversed { -delta } else { delta },
        join_type,
        temp_lim: if miter_limit <= 1.0 {
            2.0
        } else {
            2.0 / (miter_limit * miter_limit)
        },
        steps_per_rad: 0.0,
        step_sin: 0.0,
        step_cos: 0.0,
        norms: Vec::new(),
        path_out: Vec::new(),
    };

    if join_type == JoinType::Round {
        // DoGroupOffset's steps-per-circle setup. The min() caps steps at the radius in INT-GRID
        // units × π — reproduced in scaled space (never binds at sane counts; kept for fidelity).
        let abs_delta = walk.group_delta.abs();
        let steps = if steps_per_360 > 0.0 {
            steps_per_360
        } else {
            // Clipper2's undefined-tolerance default: sagitta |δ|·ARC_CONST ⇒ the ratio is the
            // constant ARC_CONST, so the count is delta-independent (~49.7).
            PI / mathf::acos(1.0 - ARC_CONST)
        };
        let steps = steps.min(abs_delta * GRID * PI);
        walk.step_sin = mathf::sin(2.0 * PI / steps);
        walk.step_cos = mathf::cos(2.0 * PI / steps);
        if walk.group_delta < 0.0 {
            walk.step_sin = -walk.step_sin;
        }
        walk.steps_per_rad = steps / (2.0 * PI);
    }

    let mut solution: Vec<Vec<Vec2>> = Vec::with_capacity(paths.len());
    for path in &paths {
        walk.build_normals(path);
        walk.offset_polygon(path);
        solution.push(core::mem::take(&mut walk.path_out));
    }
    (solution, is_reversed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<Vec2> {
        vec![
            Vec2::new(x, y),
            Vec2::new(x + s, y),
            Vec2::new(x + s, y + s),
            Vec2::new(x, y + s),
        ]
    }

    #[test]
    fn strip_duplicates_drops_consecutive_and_closing() {
        let p = Vec2::new(0.0, 0.0);
        let q = Vec2::new(1.0, 0.0);
        let r = Vec2::new(1.0, 1.0);
        assert_eq!(strip_duplicates(&[p, p, q, q, r, p]), vec![p, q, r]);
        assert_eq!(strip_duplicates(&[p, q, r]), vec![p, q, r]);
    }

    #[test]
    fn lowest_path_orientation_detects_reversal() {
        let ccw = square(0.0, 0.0, 2.0);
        let mut cw = ccw.clone();
        cw.reverse();
        assert_eq!(
            lowest_path_is_reversed(core::slice::from_ref(&ccw)),
            Some(false)
        );
        assert_eq!(lowest_path_is_reversed(&[cw]), Some(true));
        // A CW hole inside the CCW outer doesn't flip the verdict — the outer owns the extreme.
        let mut hole = square(0.5, 0.5, 1.0);
        hole.reverse();
        assert_eq!(lowest_path_is_reversed(&[ccw, hole]), Some(false));
    }

    #[test]
    fn square_join_area_is_analytic() {
        // A convex square grown with Square joins: area = s² + 4·s·d + 4·2(√2−1)·d² (the flat cap
        // tangent to the delta circle) — the geometry behind the 78.2548 OpenSCAD canary.
        let (s, d) = (5.0, 2.0);
        let (paths, rev) = offset_polygons(&[square(0.0, 0.0, s)], d, JoinType::Square, 2.0, 0.0);
        assert!(!rev);
        // Raw output for a convex polygon is a single simple path — area readable directly.
        assert_eq!(paths.len(), 1);
        let expected = s * s + 4.0 * s * d + 4.0 * 2.0 * (core::f64::consts::SQRT_2 - 1.0) * d * d;
        assert!(
            (signed_area(&paths[0]) - expected).abs() < 1e-9,
            "square-join area {} vs analytic {expected}",
            signed_area(&paths[0])
        );
        assert!(
            (expected - 78.25483399593904).abs() < 1e-3,
            "canary algebra sanity"
        );
    }

    #[test]
    fn miter_and_bevel_join_areas_are_analytic() {
        let (s, d) = (5.0, 2.0);
        // Miter (limit 2 ≥ √2 required at 90°): full sharp corners — area (s+2d)².
        let (paths, _) = offset_polygons(&[square(0.0, 0.0, s)], d, JoinType::Miter, 2.0, 0.0);
        assert!((signed_area(&paths[0]) - (s + 2.0 * d) * (s + 2.0 * d)).abs() < 1e-9);
        // Bevel: corner triangles cut — area s² + 4sd + 4·(d²/2)·... bevel connects the two edge
        // offsets directly: each 90° corner contributes d²·... triangle with legs d,d → 2d² total.
        let (paths, _) = offset_polygons(&[square(0.0, 0.0, s)], d, JoinType::Bevel, 2.0, 0.0);
        let expected = s * s + 4.0 * s * d + 2.0 * d * d;
        assert!(
            (signed_area(&paths[0]) - expected).abs() < 1e-9,
            "bevel area {} vs {expected}",
            signed_area(&paths[0])
        );
    }

    #[test]
    fn round_join_step_count_is_exact() {
        // n segments per 360°: a 90° corner emits first + (ceil(n/4) − 1) intermediates + last =
        // n/4 + 1 points; a square's 4 corners → n + 4 total (the RoundOffset NumVert law the C++
        // test pins). Direct-n dodges the acos∘cos razor edge — see offset_polygons' note.
        let (paths, _) =
            offset_polygons(&[square(0.0, 0.0, 20.0)], 5.0, JoinType::Round, 2.0, 20.0);
        assert_eq!(paths[0].len(), 24, "20-segment round join on a square");
    }

    #[test]
    fn near_zero_delta_copies_input() {
        let sq = square(0.0, 0.0, 1.0);
        let (paths, _) =
            offset_polygons(core::slice::from_ref(&sq), 1e-12, JoinType::Miter, 2.0, 0.0);
        assert_eq!(paths, vec![sq]);
    }
}
