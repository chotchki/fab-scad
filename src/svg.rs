//! SVG (2D vector) import (Q.4) — the fab-scad side of `import("logo.svg")`. The text() playbook, applied
//! to vector art: usvg NORMALIZES the document (resolves viewBox/units/transforms, expands
//! shapes/`use`/`defs`/styles into a flat tree of Bézier paths), we walk the filled paths, flatten each
//! Bézier by a fixed segment count, and map every vertex through the OpenSCAD SVG affine → a `Vec<Contour>`
//! the evaluator wraps as an even-odd `Shape2D::Polygon`.
//!
//! ORACLE FIDELITY (verified against OpenSCAD 2026.06.12 + `src/io/import_svg.cc`, three release lines):
//! - SCALE: OpenSCAD's SVG default dpi is **72** (not the wikibook's 96 — that's only the explicit `px`
//!   unit's divisor). We set usvg's dpi to 72 too, so physical-unit widths (`width="100mm"`) resolve to px
//!   consistently, then convert px→mm by [`MM_PER_PX`] = 25.4/72. That one factor cancels usvg's mm→px for
//!   unit-bearing SVGs AND applies the 72-dpi rule for unitless/viewBox-only ones — exact for both regimes.
//! - Y-FLIP: SVG's Y points DOWN, OpenSCAD's UP. OpenSCAD flips about the document height; usvg's canvas is
//!   `[0, size.height()]` (px), so `y_mm = (size.height() - y_px) * MM_PER_PX` reproduces it. `center` is
//!   `false` (the default), so the viewBox bottom-left lands at the mm origin — matching the oracle.
//! - FILL: OpenSCAD IGNORES the SVG `fill-rule` and always resolves holes by even-odd nesting, so our
//!   `Shape2D::Polygon` (even-odd) is oracle-FAITHFUL, not a compromise — a glyph-like "O" fills as a ring.
//! - TESSELLATION: OpenSCAD's SVG default is 20 segments per Bézier when `$fn` is unset ([`BEZIER_SEGMENTS`]).
//!
//! v1 SIMPLIFICATIONS (documented gaps, revisit if a real model's residual demands it):
//! - FILLED-CLOSED paths only. OpenSCAD *strokes* open/unfilled paths (offset by `stroke-width/2`); we skip
//!   `fill:none` paths. Logos (the driving use case) are filled contours, so this bites CAD-outline SVGs.
//! - `$fn`/`$fa`/`$fs` at the import site are IGNORED (fixed 20/Bézier) — the reader runs OUTSIDE eval, with
//!   no call-site fragment vars. Matches OpenSCAD's default; a knob is a later lift into the eval seam.
//! - Explicit `px`-unit width/height scale at 72 dpi here vs OpenSCAD's hardcoded 96 for that ONE unit — a
//!   ~1.36× residual for the rare px-SIZED SVG (viewBox-only + mm, the common cases, are exact).
//! - All subpaths of a filled element pool into ONE even-odd `Polygon`; OpenSCAD does per-element even-odd
//!   then unions elements (nonzero). Identical for non-overlapping art; differs only for overlapping fills.
//! - `<text>` imports EMPTY (usvg's `text` feature is off) — which is exactly what OpenSCAD does with it.

use std::path::Path;

use fab_lang::{Contour, Error, Vec2};
use usvg::tiny_skia_path::{PathSegment, Point, Transform};

/// px → mm at OpenSCAD's SVG default dpi of 72: `INCH_TO_MM / dpi` = 25.4 / 72. See the module doc for why
/// this single factor is exact for both the physical-unit and unitless/viewBox-only scale regimes.
const MM_PER_PX: f64 = 25.4 / 72.0;

/// Segments per Bézier — OpenSCAD's SVG default when `$fn` is unset (`path.cc`, `max(pathSegmentCount, 20)`).
/// Fixed (not `$fn`-driven) in v1: the reader has no call-site fragment vars. Uniform-`t` sampling, so a
/// cubic and quadratic both split into this many chords.
const BEZIER_SEGMENTS: u32 = 20;

/// Read an SVG file → its filled contours in millimeters, in OpenSCAD's coordinate frame (Y-up, viewBox
/// bottom-left at the origin). Empty (no filled paths, or an empty document) is a valid result — a
/// present-but-empty 2D leaf, like `circle(0)`.
///
/// # Errors
/// [`Error::Load`] if the file can't be read or usvg can't parse it.
pub fn svg_contours(path: &Path) -> Result<Vec<Contour>, Error> {
    let bytes = std::fs::read(path).map_err(|e| Error::Load(format!("{}: {e}", path.display())))?;
    // dpi=72 to match OpenSCAD's SVG importer; the `text` feature is compiled out (no fontdb), so <text>
    // elements contribute nothing — exactly OpenSCAD's behavior.
    let options = usvg::Options {
        dpi: 72.0,
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&bytes, &options)
        .map_err(|e| Error::Load(format!("SVG parse: {e}")))?;
    let height = f64::from(tree.size().height());
    let mut contours = Vec::new();
    collect(tree.root(), height, &mut contours);
    Ok(contours)
}

/// Walk the usvg tree, flattening every visible FILLED path into `out`. Groups recurse (their transforms are
/// already baked into each path's `abs_transform`, so there's nothing to accumulate here). Image/Text nodes
/// are ignored — images aren't vector geometry, and text is empty by construction (feature off).
fn collect(group: &usvg::Group, height: f64, out: &mut Vec<Contour>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(child) => collect(child, height, out),
            usvg::Node::Path(path) if path.is_visible() && path.fill().is_some() => {
                flatten_path(path, height, out);
            }
            _ => {}
        }
    }
}

/// Flatten one path's subpaths into contours. Each `MoveTo`/`Close` starts a fresh contour; Béziers split
/// into [`BEZIER_SEGMENTS`] chords. Points are mapped to mm through the path's absolute transform + the
/// OpenSCAD affine BEFORE flattening — valid because both are affine, and an affine commutes with uniform-`t`
/// Bézier sampling (flatten-then-map ≡ map-then-flatten). A sub-3-point contour drops (a degenerate subpath).
fn flatten_path(path: &usvg::Path, height: f64, out: &mut Vec<Contour>) {
    let transform = path.abs_transform();
    let mut current: Contour = Vec::new();
    let mut last = (0.0, 0.0);
    for seg in path.data().segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                flush(&mut current, out);
                last = map(p, &transform, height);
                current.push(Vec2::new(last.0, last.1));
            }
            PathSegment::LineTo(p) => {
                last = map(p, &transform, height);
                current.push(Vec2::new(last.0, last.1));
            }
            PathSegment::QuadTo(c, p) => {
                let c = map(c, &transform, height);
                let end = map(p, &transform, height);
                flatten_quad(last, c, end, &mut current);
                last = end;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let c1 = map(c1, &transform, height);
                let c2 = map(c2, &transform, height);
                let end = map(p, &transform, height);
                flatten_cubic(last, c1, c2, end, &mut current);
                last = end;
            }
            PathSegment::Close => flush(&mut current, out),
        }
    }
    flush(&mut current, out);
}

/// Map a path point → mm in OpenSCAD's frame: apply the path's absolute transform (into usvg's Y-down canvas
/// space, `[0, height]` px), then scale by [`MM_PER_PX`] and flip Y about `height`.
fn map(mut p: Point, transform: &Transform, height: f64) -> (f64, f64) {
    transform.map_point(&mut p);
    (
        f64::from(p.x) * MM_PER_PX,
        (height - f64::from(p.y)) * MM_PER_PX,
    )
}

/// Push the interior + end points of a quadratic Bézier (the start is already the contour's last point).
fn flatten_quad(p0: (f64, f64), c: (f64, f64), p1: (f64, f64), out: &mut Contour) {
    for i in 1..=BEZIER_SEGMENTS {
        let t = f64::from(i) / f64::from(BEZIER_SEGMENTS);
        let mt = 1.0 - t;
        let x = mt * mt * p0.0 + 2.0 * mt * t * c.0 + t * t * p1.0;
        let y = mt * mt * p0.1 + 2.0 * mt * t * c.1 + t * t * p1.1;
        out.push(Vec2::new(x, y));
    }
}

/// Push the interior + end points of a cubic Bézier (the start is already the contour's last point).
fn flatten_cubic(
    p0: (f64, f64),
    c1: (f64, f64),
    c2: (f64, f64),
    p1: (f64, f64),
    out: &mut Contour,
) {
    for i in 1..=BEZIER_SEGMENTS {
        let t = f64::from(i) / f64::from(BEZIER_SEGMENTS);
        let mt = 1.0 - t;
        let (mt2, mt3, t2, t3) = (mt * mt, mt * mt * mt, t * t, t * t * t);
        let x = mt3 * p0.0 + 3.0 * mt2 * t * c1.0 + 3.0 * mt * t2 * c2.0 + t3 * p1.0;
        let y = mt3 * p0.1 + 3.0 * mt2 * t * c1.1 + 3.0 * mt * t2 * c2.1 + t3 * p1.1;
        out.push(Vec2::new(x, y));
    }
}

/// Move the accumulated contour into `out` if it's a real ring (≥3 points); a shorter one is a degenerate
/// subpath (a lone move, a zero-length line) and drops — matching `polygon()`'s own <3-point rule.
fn flush(current: &mut Contour, out: &mut Vec<Contour>) {
    if current.len() >= 3 {
        out.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: unwrap/expect ARE the assertions"
)]
mod tests {
    use super::svg_contours;

    /// Write `svg` to a process-unique temp file and import it.
    fn contours_of(name: &str, svg: &str) -> Vec<fab_lang::Contour> {
        let path = std::env::temp_dir().join(format!("fab_svg_{}_{name}.svg", std::process::id()));
        std::fs::write(&path, svg).unwrap();
        svg_contours(&path).expect("svg imports")
    }

    fn bounds(contours: &[fab_lang::Contour]) -> (f64, f64, f64, f64) {
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for c in contours {
            for p in c {
                lo_x = lo_x.min(p.x);
                hi_x = hi_x.max(p.x);
                lo_y = lo_y.min(p.y);
                hi_y = hi_y.max(p.y);
            }
        }
        (lo_x, hi_x, lo_y, hi_y)
    }

    #[test]
    fn unitless_rect_scales_at_72_dpi_and_flips_y() {
        // A rect x=10 y=20 w=30 h=40 in a 100×100 viewBox, no physical units → 72-dpi scaling. This is the
        // exact fixture measured against the OpenSCAD oracle: x[10,40]·(25.4/72), Y flipped about 100.
        let c = contours_of(
            "rect_px",
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect x="10" y="20" width="30" height="40" fill="black"/></svg>"#,
        );
        let (lo_x, hi_x, lo_y, hi_y) = bounds(&c);
        let s = 25.4 / 72.0;
        assert!(
            (lo_x - 10.0 * s).abs() < 1e-6,
            "x min {lo_x} vs {}",
            10.0 * s
        );
        assert!(
            (hi_x - 40.0 * s).abs() < 1e-6,
            "x max {hi_x} vs {}",
            40.0 * s
        );
        // SVG y∈[20,60] flips about 100 → [40,80], then ·s.
        assert!(
            (lo_y - 40.0 * s).abs() < 1e-6,
            "y min {lo_y} vs {}",
            40.0 * s
        );
        assert!(
            (hi_y - 80.0 * s).abs() < 1e-6,
            "y max {hi_y} vs {}",
            80.0 * s
        );
    }

    #[test]
    fn mm_units_are_one_to_one() {
        // width/height in mm → 1 user unit = 1 mm (dpi drops out). Same rect → x[10,40] mm, y[40,80] mm.
        let c = contours_of(
            "rect_mm",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100"><rect x="10" y="20" width="30" height="40" fill="black"/></svg>"#,
        );
        let (lo_x, hi_x, lo_y, hi_y) = bounds(&c);
        assert!((lo_x - 10.0).abs() < 1e-4, "x min {lo_x}");
        assert!((hi_x - 40.0).abs() < 1e-4, "x max {hi_x}");
        assert!((lo_y - 40.0).abs() < 1e-4, "y min {lo_y}");
        assert!((hi_y - 80.0).abs() < 1e-4, "y max {hi_y}");
    }

    #[test]
    fn empty_svg_is_empty_not_an_error() {
        assert!(
            contours_of(
                "empty",
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"/>"#
            )
            .is_empty(),
            "no paths → no contours (a present-but-empty 2D leaf)"
        );
    }

    #[test]
    fn a_circle_becomes_a_filled_ring() {
        // usvg expands <circle> into cubic Béziers; we flatten them. r=10 in mm units → a ring near ±10 mm
        // about its center, with many points (4 cubics × 20 segs).
        let c = contours_of(
            "circle",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="40mm" viewBox="0 0 40 40"><circle cx="20" cy="20" r="10" fill="black"/></svg>"#,
        );
        assert_eq!(c.len(), 1, "one subpath → one contour");
        assert!(
            c[0].len() >= 40,
            "a flattened circle has many points, got {}",
            c[0].len()
        );
        let (lo_x, hi_x, lo_y, hi_y) = bounds(&c);
        assert!(
            (hi_x - lo_x - 20.0).abs() < 0.2,
            "≈20 mm wide, got {}",
            hi_x - lo_x
        );
        assert!(
            (hi_y - lo_y - 20.0).abs() < 0.2,
            "≈20 mm tall, got {}",
            hi_y - lo_y
        );
    }
}
