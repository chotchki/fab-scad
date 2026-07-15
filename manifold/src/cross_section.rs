//! `CrossSection` — the 2D polygon subsystem (Manifold's `CrossSection`, R5/M.5).
//!
//! Manifold's 2D IS Clipper2; per SPEC [OPEN #4] (chotchki-confirmed, M.5.0 spike) we adopt `i_overlay`
//! — pure-Rust, integer-coords ⇒ deterministic + wasm-clean — and validate by AREA-residual against
//! Clipper2-via-Manifold, NOT bit-identity. This is the ONE layer where the verbatim/byte-exact thesis
//! relaxes; the 3D core stays byte-exact.
//!
//! A `CrossSection` is a set of polygon contours under the POSITIVE fill rule (Manifold's `from_polygons`
//! default): a CCW contour adds +1 winding (fills), a CW contour −1 (a hole). i_overlay handles the
//! f64↔integer-grid round-trip internally, so the determinism seam lives inside the dep, not here.

use crate::linalg::Vec2;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin, OutlineStyle};

/// Corner handling for [`CrossSection::offset`] (Manifold `JoinType` / Clipper2 `JoinType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinType {
    /// Flat (squared-off) corners.
    Square,
    /// Rounded corners (arc, resolution = `circular_segments`).
    Round,
    /// Sharp mitered corners, bounded by `miter_limit`.
    Miter,
}

/// A 2D region as a set of polygon contours (Manifold `CrossSection`). Normalized under Positive fill:
/// CCW outers fill, CW contours subtract (holes) — the flat `Polygons` form, holes distinguished by
/// winding. Empty `contours` = no area.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossSection {
    /// The polygon contours (outers CCW, holes CW).
    pub contours: Vec<Vec<Vec2>>,
}

impl CrossSection {
    /// The empty cross-section (no area).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from raw polygon contours, normalizing under Positive fill (Manifold `from_polygons`): a
    /// `Subject`-rule self-overlay resolves self-intersections + canonicalizes the winding so booleans
    /// and area are well-defined. CCW is the outer-contour convention.
    pub fn from_polygons(polygons: &[Vec<Vec2>]) -> Self {
        if polygons.iter().all(|c| c.is_empty()) {
            return Self::new();
        }
        let subj = to_io(polygons);
        let shapes = subj.overlay(&empty_clip(), OverlayRule::Subject, FillRule::Positive);
        Self { contours: from_io(shapes) }
    }

    fn boolean(&self, other: &Self, rule: OverlayRule) -> Self {
        let a = to_io(&self.contours);
        let b = to_io(&other.contours);
        Self { contours: from_io(a.overlay(&b, rule, FillRule::Positive)) }
    }

    /// `self ∪ other` (Manifold `+` / `Boolean(Add)`).
    pub fn union(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Union)
    }

    /// `self − other` (Manifold `-` / `Boolean(Subtract)`).
    pub fn difference(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Difference)
    }

    /// `self ∩ other` (Manifold `^` / `Boolean(Intersect)`).
    pub fn intersection(&self, other: &Self) -> Self {
        self.boolean(other, OverlayRule::Intersect)
    }

    /// Grow (`delta > 0`) or shrink (`< 0`) the region by `delta`, with the given corner handling
    /// (Manifold `Offset`). `circular_segments` is the round-join arc resolution (segments per full
    /// circle); `miter_limit` bounds miter joins. Backed by i_overlay's `outline`.
    ///
    /// JOIN-TYPE FIDELITY: `Round` area-matches Clipper2 (both polygonize toward the true arc, so a fine
    /// `circular_segments` converges — this is the OpenSCAD `offset(r)` path, the gated case). `Miter` and
    /// `Square` are best-effort maps (i_overlay `Miter` is a turn-angle threshold, not a ratio; `Square`→
    /// `Bevel`) — the corner geometry differs from Clipper2, so their AREA diverges at corners and they
    /// are NOT area-gated. This is the 2D layer's documented approximation.
    pub fn offset(&self, delta: f64, join_type: JoinType, miter_limit: f64, circular_segments: i32) -> Self {
        if self.is_empty() {
            return Self::new();
        }
        let join = match join_type {
            JoinType::Round => {
                let n = (circular_segments.max(4)) as f64;
                LineJoin::Round(core::f64::consts::TAU / n)
            }
            // Miter length = offset / sin(turn/2); miter used while that ratio ≤ miter_limit ⇒ turn ≥
            // 2·asin(1/limit). i_overlay's Miter(angle) is that turn-angle threshold (below ⇒ bevel).
            JoinType::Miter => {
                LineJoin::Miter(2.0 * crate::mathf::asin((1.0 / miter_limit).clamp(-1.0, 1.0)))
            }
            JoinType::Square => LineJoin::Bevel,
        };
        let style = OutlineStyle::new(delta).line_join(join);
        let input = to_io(&self.contours);
        Self { contours: from_io(input.outline(&style)) }
    }

    /// Net signed area — outer contours positive, holes negative (Manifold `Area`).
    pub fn area(&self) -> f64 {
        self.contours.iter().map(|c| signed_area(c)).sum()
    }

    /// No contours?
    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }

    /// Number of contours (outers + holes).
    pub fn num_contour(&self) -> usize {
        self.contours.len()
    }

    /// Total vertex count across all contours.
    pub fn num_vert(&self) -> usize {
        self.contours.iter().map(|c| c.len()).sum()
    }

    /// Axis-aligned 2D bounds `(min, max)` (Manifold `Bounds`); `None` if empty.
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let mut pts = self.contours.iter().flatten().copied();
        let first = pts.next()?;
        let (mut min, mut max) = (first, first);
        for p in pts {
            min = Vec2::new(min.x.min(p.x), min.y.min(p.y));
            max = Vec2::new(max.x.max(p.x), max.y.max(p.y));
        }
        Some((min, max))
    }

    /// The contours as raw `[f64; 2]` polygons (Manifold `ToPolygons`) — the interchange the 2D↔3D
    /// bridges and the area-residual oracle consume.
    pub fn to_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        self.contours
            .iter()
            .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
            .collect()
    }
}

/// An empty clip contour set — the second operand for a `Subject`-rule normalization.
fn empty_clip() -> Vec<Vec<[f64; 2]>> {
    Vec::new()
}

fn to_io(contours: &[Vec<Vec2>]) -> Vec<Vec<[f64; 2]>> {
    contours
        .iter()
        .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

/// Flatten i_overlay's grouped `Shapes` (shape → contours) into the flat `Polygons` form.
fn from_io(shapes: Vec<Vec<Vec<[f64; 2]>>>) -> Vec<Vec<Vec2>> {
    shapes
        .into_iter()
        .flatten()
        .map(|c| c.into_iter().map(|p| Vec2::new(p[0], p[1])).collect())
        .collect()
}

/// Signed shoelace area of one contour (CCW ⇒ positive).
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
    fn booleans_and_area_are_analytic() {
        let a = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)]); // area 4
        let b = CrossSection::from_polygons(&[square(1.0, 1.0, 2.0)]); // area 4, overlap 1
        assert!((a.area() - 4.0).abs() < 1e-9, "square area {}", a.area());

        assert!((a.union(&b).area() - 7.0).abs() < 1e-9);
        assert!((a.intersection(&b).area() - 1.0).abs() < 1e-9);
        assert!((a.difference(&b).area() - 3.0).abs() < 1e-9);

        // A hole: big square minus a fully-interior small one → outer + hole contour, area 96.
        let big = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)]);
        let small = CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]);
        let holed = big.difference(&small);
        assert!((holed.area() - 96.0).abs() < 1e-9, "holed area {}", holed.area());
        assert_eq!(holed.num_contour(), 2, "outer + 1 hole");
    }

    #[test]
    fn empty_and_bounds() {
        let e = CrossSection::new();
        assert!(e.is_empty() && e.area() == 0.0 && e.bounds().is_none());
        // Disjoint squares that don't touch → empty intersection.
        let a = CrossSection::from_polygons(&[square(0.0, 0.0, 1.0)]);
        let b = CrossSection::from_polygons(&[square(5.0, 5.0, 1.0)]);
        assert!(a.intersection(&b).is_empty());

        let (min, max) = a.bounds().unwrap();
        assert_eq!((min, max), (Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)));
    }

    #[test]
    fn offset_round_matches_analytic() {
        let (s, r) = (4.0, 1.0);
        let a = CrossSection::from_polygons(&[square(0.0, 0.0, s)]);
        let grown = a.offset(r, JoinType::Round, 2.0, 256);
        // Rounded rectangle: s² + 4·s·r (edge strips) + π·r² (corner quarter-circles).
        let expected = s * s + 4.0 * s * r + core::f64::consts::PI * r * r;
        assert!(
            (grown.area() - expected).abs() / expected < 2e-3,
            "round-offset area {} vs analytic {expected}",
            grown.area()
        );
        // A negative offset shrinks: a 4-square inset by 1 → a 2-square, area 4.
        let inset = a.offset(-1.0, JoinType::Miter, 2.0, 16);
        assert!((inset.area() - 4.0).abs() < 1e-6, "inset area {}", inset.area());
    }

    #[test]
    fn is_deterministic() {
        let a = CrossSection::from_polygons(&[square(0.0, 0.0, 3.0)]);
        let b = CrossSection::from_polygons(&[square(1.3, 0.7, 3.0)]);
        assert_eq!(a.union(&b), a.union(&b), "CrossSection union not deterministic");
    }
}
