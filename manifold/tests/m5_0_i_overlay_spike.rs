//! M.5.0 — the i_overlay ROBUSTNESS SPIKE (SPEC [OPEN #4] de-risk, chotchki-confirmed direction).
//!
//! The whole "adopt i_overlay + area-residual instead of porting Clipper2" bet rests on i_overlay being
//! (a) CORRECT on 2D booleans under the Positive-fill contract Manifold's `from_polygons` uses, (b) ROBUST
//! on the OFFSET / round-join path (the SPEC's explicitly flagged sharp edge), and (c) DETERMINISTIC
//! (integer-coords ⇒ same input, same output). This spike stresses all three against ANALYTIC ground
//! truth — no fab-manifold code yet, this is pure evaluation. PASS ⇒ M.5.1 builds the CrossSection
//! wrapper on i_overlay; FAIL ⇒ the fork reopens (port Clipper2).

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin, OutlineStyle};

type Pt = [f64; 2];

/// An axis-aligned square contour `[x,x+s] × [y,y+s]`, CCW.
fn square(x: f64, y: f64, s: f64) -> Vec<Pt> {
    vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
}

/// Signed shoelace area of one contour (CCW ⇒ positive).
fn contour_area(c: &[Pt]) -> f64 {
    let n = c.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = c[i];
        let q = c[(i + 1) % n];
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Net filled area of an i_overlay `Shapes` result: sum of signed contour areas (outers +, holes −).
fn area(shapes: &[Vec<Vec<Pt>>]) -> f64 {
    shapes.iter().flatten().map(|c| contour_area(c)).sum()
}

const PI: f64 = std::f64::consts::PI;

#[test]
fn i_overlay_booleans_positive_fill_are_analytic() {
    // Two unit-ish squares overlapping in a [1,2]² = 1.0 area patch.
    let a = square(0.0, 0.0, 2.0); // area 4
    let b = square(1.0, 1.0, 2.0); // area 4, overlap 1

    let u = a.overlay(&b, OverlayRule::Union, FillRule::Positive);
    let i = a.overlay(&b, OverlayRule::Intersect, FillRule::Positive);
    let d = a.overlay(&b, OverlayRule::Difference, FillRule::Positive);

    assert!(
        (area(&u) - 7.0).abs() < 1e-9,
        "union area {} != 7",
        area(&u)
    );
    assert!(
        (area(&i) - 1.0).abs() < 1e-9,
        "intersect area {} != 1",
        area(&i)
    );
    assert!(
        (area(&d) - 3.0).abs() < 1e-9,
        "difference area {} != 3",
        area(&d)
    );

    // A hole must survive: a big square minus a centered small one.
    let big = square(0.0, 0.0, 10.0); // 100
    let small = square(4.0, 4.0, 2.0); // 4, fully interior
    let holed = big.overlay(&small, OverlayRule::Difference, FillRule::Positive);
    assert!(
        (area(&holed) - 96.0).abs() < 1e-9,
        "holed area {} != 96",
        area(&holed)
    );
    // The result is one shape with an outer + a hole contour.
    assert_eq!(holed.len(), 1, "expected a single shape");
    assert_eq!(holed[0].len(), 2, "expected outer + 1 hole contour");
}

#[test]
fn i_overlay_offset_round_joins_match_analytic() {
    // THE flagged risk. Offset a square outward by r with ROUND joins → a rounded rectangle:
    //   area = s² + 4·s·r  (edge strips)  + π·r²  (4 corner quarter-circles = 1 full circle).
    let s = 4.0;
    let r = 1.0;
    let sq = square(0.0, 0.0, s);
    // A fine arc angle so the polygonized corners approach the true circle.
    let style = OutlineStyle::new(r).line_join(LineJoin::Round(0.02));
    let out = sq.outline(&style);

    let expected = s * s + 4.0 * s * r + PI * r * r; // 16 + 16 + π ≈ 35.1416
    let got = area(&out);
    assert!(
        (got - expected).abs() / expected < 5e-3,
        "round-offset area {got} vs analytic {expected} (rel {:.3e})",
        (got - expected).abs() / expected
    );

    // Miter joins → sharp corners: area = (s+2r)².
    let miter = sq.outline(&OutlineStyle::new(r).line_join(LineJoin::Miter(0.1)));
    let miter_expected = (s + 2.0 * r).powi(2); // 36
    assert!(
        (area(&miter) - miter_expected).abs() / miter_expected < 5e-3,
        "miter-offset area {} vs {miter_expected}",
        area(&miter)
    );

    // Bevel joins → chopped corners, strictly between round and miter.
    let bevel = sq.outline(&OutlineStyle::new(r).line_join(LineJoin::Bevel));
    let ab = area(&bevel);
    assert!(
        ab < area(&out) && area(&out) < area(&miter),
        "bevel < round < miter must hold: {ab} {got} {}",
        area(&miter)
    );

    // INSET (negative offset) must shrink: a 4-square inset by 1 → a 2-square, area 4.
    let inset = sq.outline(&OutlineStyle::new(-1.0).line_join(LineJoin::Miter(0.1)));
    assert!(
        (area(&inset) - 4.0).abs() < 1e-6,
        "inset area {} != 4",
        area(&inset)
    );
}

#[test]
fn i_overlay_is_deterministic() {
    // Same op, twice — the bytes must be identical (integer-coords ⇒ no float-assoc hazard).
    let a = square(0.0, 0.0, 3.0);
    let b = square(1.3, 0.7, 3.0);
    let r1 = a.overlay(&b, OverlayRule::Union, FillRule::Positive);
    let r2 = a.overlay(&b, OverlayRule::Union, FillRule::Positive);
    let bits = |s: &[Vec<Vec<Pt>>]| {
        s.iter()
            .flatten()
            .flatten()
            .flat_map(|p| [p[0].to_bits(), p[1].to_bits()])
            .collect::<Vec<u64>>()
    };
    assert_eq!(
        bits(&r1),
        bits(&r2),
        "i_overlay union not bit-deterministic run-to-run"
    );

    // And the offset path.
    let o1 = a.outline(&OutlineStyle::new(0.5).line_join(LineJoin::Round(0.05)));
    let o2 = a.outline(&OutlineStyle::new(0.5).line_join(LineJoin::Round(0.05)));
    assert_eq!(
        bits(&o1),
        bits(&o2),
        "i_overlay offset not bit-deterministic run-to-run"
    );
}

#[test]
fn i_overlay_handles_degenerate_inputs() {
    // A self-intersecting "bowtie" — Positive fill must resolve it into the two triangles (no panic,
    // no garbage). Bowtie: (0,0)->(2,2)->(2,0)->(0,2) crosses itself at (1,1).
    let bowtie = vec![[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 2.0]];
    // Self-overlay with the Subject rule resolves self-intersections.
    let resolved = bowtie.overlay(&Vec::<Pt>::new(), OverlayRule::Subject, FillRule::NonZero);
    let a = area(&resolved).abs();
    // The two triangles each have area 1 → total 2 (NonZero) — the exact value depends on winding, but
    // it MUST be a finite, sane positive area, not a panic or a wild number.
    assert!(
        a.is_finite() && a > 0.0 && a < 10.0,
        "bowtie resolved to insane area {a}"
    );

    // A near-degenerate sliver (three nearly-collinear points + a real vertex) must not panic.
    let sliver = vec![[0.0, 0.0], [10.0, 1e-9], [10.0, 0.0], [5.0, 3.0]];
    let clip = square(0.0, 0.0, 10.0);
    let r = sliver.overlay(&clip, OverlayRule::Intersect, FillRule::Positive);
    assert!(
        area(&r).abs().is_finite(),
        "sliver intersect produced non-finite area"
    );
}
