//! J.3.2.1 — the 2D subsystem's eval-wire. Two things get pinned here:
//!
//! 1. 2D geometry builds a strongly-typed [`Shape2D`] tree (the sibling of `GeoNode`): `square`/`circle`/
//!    `polygon` → `Polygon` leaves, 2D transforms → `Shape2D::Transform` of the matrix's 2D restriction,
//!    2D booleans → `Shape2D::{Union,Difference,Intersection}`.
//! 2. The 2D/3D dimension-MIXING rules — which child fixes the dimension, which get dropped, and the
//!    exact warning text — EVERY clause verified against OpenSCAD 2026.06.12 (`OpenSCAD --version`).
//!
//! The mixing behavior is subtle and hard-won, so each case cites what the oracle actually did. It is
//! "Mixing 2D and 3D objects is not supported" once per operation, then "Ignoring {n}D child object for
//! {m}D operation" once per dropped child; the FIRST non-null child fixes the dimension (a present-but-
//! empty `cube(0)` counts — only a truly absent `{}`/never-run-`for` is dimension-neutral).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration test: unwrap/panic ARE the assertions; 2D primitive vertices + affine literals are EXACT"
)]

use fab_lang::{
    Error, Geo, GeoNode, Shape2D, Vec2, evaluate, evaluate_geometry, evaluate_geometry_full,
};

/// Evaluate to a 2D [`Shape2D`] result, panicking if it came out 3D — the 2D analogue of the 3D tests'
/// `d3` unwrap.
fn d2(src: &str) -> Shape2D {
    match evaluate_geometry(src).unwrap() {
        Geo::D2(shape) => shape,
        Geo::D3(node) => panic!("expected a 2D result for {src:?}, got 3D: {node:?}"),
    }
}

/// The warning CONTENTS a program emits, in order (the `WARNING: ` prefix stripped) — the oracle-verified
/// mixing text lands here.
fn warnings(src: &str) -> Vec<String> {
    let (_, messages) = evaluate_geometry_full(src).unwrap();
    messages
        .iter()
        .filter_map(|m| match m {
            fab_lang::Message::Warning(s) => Some(s.clone()),
            fab_lang::Message::Echo(_) => None,
        })
        .collect()
}

/// A point.
fn p(x: f64, y: f64) -> Vec2 {
    Vec2::new(x, y)
}

// ─────────────────────────────── 2D primitives → Shape2D leaves ───────────────────────────────

#[test]
fn primitives_build_polygon_leaves() {
    // square → its CCW contour.
    assert_eq!(
        d2("square(2);"),
        Shape2D::Polygon(vec![vec![
            p(0.0, 0.0),
            p(2.0, 0.0),
            p(2.0, 2.0),
            p(0.0, 2.0)
        ]])
    );
    // square([x, y], center) → the centered rectangle.
    assert_eq!(
        d2("square([4, 2], center = true);"),
        Shape2D::Polygon(vec![vec![
            p(-2.0, -1.0),
            p(2.0, -1.0),
            p(2.0, 1.0),
            p(-2.0, 1.0)
        ]])
    );
    // polygon(points) → a single contour of all points.
    assert_eq!(
        d2("polygon([[0, 0], [4, 0], [2, 3]]);"),
        Shape2D::Polygon(vec![vec![p(0.0, 0.0), p(4.0, 0.0), p(2.0, 3.0)]])
    );
}

#[test]
fn polygon_paths_select_contours_and_bad_input_drops() {
    // polygon(points, paths) → each path is a ring of indices → a contour (an outer boundary + a hole).
    assert_eq!(
        d2(
            "polygon([[0, 0], [4, 0], [4, 4], [0, 4], [1, 1], [2, 1], [2, 2]], [[0, 1, 2, 3], [4, 5, 6]]);"
        ),
        Shape2D::Polygon(vec![
            vec![p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(0.0, 4.0)],
            vec![p(1.0, 1.0), p(2.0, 1.0), p(2.0, 2.0)],
        ])
    );
    // a point that isn't a ≥2-vector is DROPPED (its later index-references then land out of range and
    // drop too, mirroring polyhedron) — here [1] is a 1-vector, so only 2 valid points remain → no contour.
    assert_eq!(
        d2("polygon([[0, 0], [1], [4, 4]]);"),
        Shape2D::Polygon(vec![])
    );
    // `points` that isn't a list at all → no vertices → no contour.
    assert_eq!(d2("polygon(5);"), Shape2D::Polygon(vec![]));
    // `paths` that isn't a list → no paths given, so it falls back to the single all-points contour.
    assert_eq!(
        d2("polygon([[0, 0], [4, 0], [2, 3]], paths = 5);"),
        Shape2D::Polygon(vec![vec![p(0.0, 0.0), p(4.0, 0.0), p(2.0, 3.0)]])
    );
    // a non-numeric-list path entry (a string) is DROPPED → an empty path set → no contour.
    assert_eq!(
        d2("polygon([[0, 0], [4, 0], [2, 3]], paths = [\"x\"]);"),
        Shape2D::Polygon(vec![])
    );
}

#[test]
fn square_arg_fallbacks() {
    // square() defaults to the unit square (OpenSCAD's `size = 1`).
    assert_eq!(
        d2("square();"),
        Shape2D::Polygon(vec![vec![
            p(0.0, 0.0),
            p(1.0, 0.0),
            p(1.0, 1.0),
            p(0.0, 1.0)
        ]])
    );
    // a malformed size vector (fewer than 2 elements) falls back to the unit square, mirroring `cube`'s
    // convention — a documented fallback, not an OpenSCAD-parity claim.
    assert_eq!(d2("square([5]);"), d2("square(1);"));
}

#[test]
fn circle_shares_fn_parity_with_the_ring_math() {
    // circle(1, $fn = 4) → a diamond at the axes, EXACT (the same exact-quadrant trig cylinder/sphere use).
    assert_eq!(
        d2("circle(1, $fn = 4);"),
        Shape2D::Polygon(vec![vec![
            p(1.0, 0.0),
            p(0.0, 1.0),
            p(-1.0, 0.0),
            p(0.0, -1.0)
        ]])
    );
    // d = 2r; the count comes from $fn.
    match d2("circle(d = 10, $fn = 32);") {
        Shape2D::Polygon(ref cs) => {
            assert_eq!(cs.len(), 1);
            assert_eq!(cs[0].len(), 32);
            assert_eq!(cs[0][0], p(5.0, 0.0)); // first point on +x, radius 5
        }
        other => panic!("expected a Polygon, got {other:?}"),
    }
}

#[test]
fn degenerate_2d_primitive_is_present_not_null() {
    // circle(0) tessellates to NO contours but is still a PRESENT 2D object (`Polygon([])`), NOT the
    // dimension-neutral `Empty` — mirrors `cube(0)` being a present empty 3D leaf (the oracle fixes
    // dimension off it, tested below).
    assert_eq!(d2("circle(0);"), Shape2D::Polygon(vec![]));
    assert_eq!(d2("square(0);"), Shape2D::Polygon(vec![]));
}

// ─────────────────────────────── 2D transforms (Affine2 = the 2D restriction) ───────────────────

#[test]
fn transforms_apply_the_2d_submatrix() {
    // translate([x, y, z]) drops z — the 2D affine's translation is [x, y]. Verified vs oracle: the SVG
    // bbox of `translate([3,4,99]) square(2)` is [3,5]×[4,6] (z ignored).
    match d2("translate([3, 4, 99]) square(2);") {
        Shape2D::Transform {
            ref matrix,
            ref child,
        } => {
            let m = matrix.as_row_major(); // [a, b, c, d, e, f] → x' = a·x+b·y+c, y' = d·x+e·y+f
            assert_eq!([m[2], m[5]], [3.0, 4.0]); // translation, z-component gone
            assert!(matches!(**child, Shape2D::Polygon(_)));
        }
        other => panic!("expected a 2D Transform, got {other:?}"),
    }
    // scale([2, 3]) → the diagonal 2×2. Oracle: scale([2,3]) square(1) → [0,2]×[0,3].
    assert!(matches!(
        d2("scale([2, 3]) square(1);"),
        Shape2D::Transform { ref matrix, .. } if {
            let m = matrix.as_row_major();
            [m[0], m[4]] == [2.0, 3.0]
        }
    ));
    // rotate(90) about +Z on a 2D shape: (x, y) → (-y, x). Oracle: rotate(90) square([4,2]) → [-2,0]×[0,4].
    match d2("rotate(90) square([4, 2]);") {
        Shape2D::Transform { ref matrix, .. } => {
            assert_eq!(matrix.apply(p(4.0, 0.0)), p(0.0, 4.0)); // +x maps to +y
            assert_eq!(matrix.apply(p(0.0, 2.0)), p(-2.0, 0.0)); // +y maps to -x
        }
        other => panic!("expected a 2D Transform, got {other:?}"),
    }
}

// ─────────────────────────────── 2D booleans + implicit union ───────────────────────────────

#[test]
fn booleans_and_union_build_shape2d_nodes() {
    // two 2D objects at the top level → an implicit 2D union.
    assert!(
        matches!(d2("circle(5, $fn = 8); circle(3, $fn = 8);"), Shape2D::Union(ref c) if c.len() == 2)
    );
    // explicit booleans over 2D children → the matching Shape2D node.
    assert!(matches!(
        d2("difference() { circle(5, $fn = 8); circle(3, $fn = 8); }"),
        Shape2D::Difference(ref c) if c.len() == 2
    ));
    assert!(matches!(
        d2("intersection() { square(5); circle(3, $fn = 8); }"),
        Shape2D::Intersection(ref c) if c.len() == 2
    ));
    // a transform over a 2D boolean nests correctly (both stay 2D).
    assert!(matches!(
        d2("translate([1, 0]) union() { square(2); circle(1, $fn = 8); }"),
        Shape2D::Transform { ref child, .. } if matches!(**child, Shape2D::Union(ref c) if c.len() == 2)
    ));
}

// ─────────────────────────────── offset() (J.3.3) ───────────────────────────────

#[test]
fn offset_resolves_r_delta_and_chamfer() {
    use fab_lang::Join2D;
    // `r` (positional) → ROUNDED, $fn-faceted (segments = the full-circle count, like `circle`).
    assert!(matches!(
        d2("offset(2, $fn = 64) square(5);"),
        Shape2D::Offset { delta, join: Join2D::Round, segments, ref child }
            if delta == 2.0 && segments == 64 && matches!(**child, Shape2D::Polygon(_))
    ));
    // named `r` is the same rounded path.
    assert!(matches!(
        d2("offset(r = 3) square(5);"),
        Shape2D::Offset {
            join: Join2D::Round,
            ..
        }
    ));
    // `delta` (named, no `r`) → MITERED sharp corners.
    assert!(matches!(
        d2("offset(delta = 2) square(5);"),
        Shape2D::Offset { delta, join: Join2D::Miter, .. } if delta == 2.0
    ));
    // `delta` + `chamfer = true` → BEVELED.
    assert!(matches!(
        d2("offset(delta = 2, chamfer = true) square(5);"),
        Shape2D::Offset {
            join: Join2D::Bevel,
            ..
        }
    ));
    // `r` BEATS `delta` — OpenSCAD (verified: offset(r=2, delta=9) renders as r=2, rounded).
    assert!(matches!(
        d2("offset(r = 2, delta = 9) square(5);"),
        Shape2D::Offset { delta, join: Join2D::Round, .. } if delta == 2.0
    ));
}

#[test]
fn offset_is_a_fixed_2d_op() {
    // A 3D child is IGNORED with just "Ignoring 3D child object for 2D operation" (NO "Mixing" — offset's
    // dimension is fixed at 2D), yielding an empty 2D offset. Verified vs OpenSCAD 2026.06.12.
    assert!(matches!(
        d2("offset(2) cube(5);"),
        Shape2D::Offset { ref child, .. } if matches!(**child, Shape2D::Empty)
    ));
    assert_eq!(
        warnings("offset(2) cube(5);"),
        ["Ignoring 3D child object for 2D operation"]
    );
    // A NULL child (`{}`) → an empty 2D offset, SILENTLY (no "Ignoring" — nothing there to ignore).
    assert!(matches!(
        d2("offset(2) { }"),
        Shape2D::Offset { ref child, .. } if matches!(**child, Shape2D::Empty)
    ));
    assert!(warnings("offset(2) { }").is_empty());
}

#[test]
fn offset_with_no_r_or_delta_is_the_identity() {
    use fab_lang::Join2D;
    // `offset()` with neither `r` nor `delta` → a zero (identity) offset — no change to the outline.
    assert!(matches!(
        d2("offset() square(5);"),
        Shape2D::Offset { delta, join: Join2D::Miter, .. } if delta == 0.0
    ));
}

// ─────────────────────── 2D/3D MIXING — every clause vs OpenSCAD 2026.06.12 ───────────────────────

#[test]
fn mixing_3d_first_keeps_3d_and_drops_the_2d_child() {
    // Oracle `cube(2); circle(5);`: "Mixing 2D and 3D objects is not supported" + "Ignoring 2D child
    // object for 3D operation"; the result is the 3D cube (the 2D circle dropped).
    assert!(matches!(
        evaluate_geometry("cube(2); circle(5);").unwrap(),
        Geo::D3(GeoNode::Leaf(_))
    ));
    assert_eq!(
        warnings("cube(2); circle(5);"),
        [
            "Mixing 2D and 3D objects is not supported",
            "Ignoring 2D child object for 3D operation",
        ]
    );
}

#[test]
fn mixing_2d_first_keeps_2d_and_drops_the_3d_child() {
    // Oracle `circle(5); cube(2);`: same "Mixing", then "Ignoring 3D child object for 2D operation"; the
    // result is the 2D circle (the 3D cube dropped).
    assert!(matches!(
        evaluate_geometry("circle(5, $fn = 8); cube(2);").unwrap(),
        Geo::D2(Shape2D::Polygon(_))
    ));
    assert_eq!(
        warnings("circle(5, $fn = 8); cube(2);"),
        [
            "Mixing 2D and 3D objects is not supported",
            "Ignoring 3D child object for 2D operation",
        ]
    );
}

#[test]
fn mixing_warns_once_but_ignores_each_mismatched_child() {
    // Oracle `union() { circle(5); cube(2); sphere(3); square(1); }`: ONE "Mixing", then TWO "Ignoring
    // 3D child" (cube + sphere), and the trailing 2D square is KEPT — dim is set by the first child
    // (circle, 2D), each 3D child is dropped individually, matching children survive.
    let src = "union() { circle(5, $fn = 8); cube(2); sphere(3); square(1); }";
    assert_eq!(
        warnings(src),
        [
            "Mixing 2D and 3D objects is not supported",
            "Ignoring 3D child object for 2D operation",
            "Ignoring 3D child object for 2D operation",
        ]
    );
    // circle + square both survive → a 2D union of two.
    assert!(matches!(d2(src), Shape2D::Union(ref c) if c.len() == 2));
}

#[test]
fn a_matching_child_after_a_mismatch_survives() {
    // Oracle `union() { circle(5); cube(2); translate([100,0,0]) square(3); }`: SVG bbox extends to
    // x = 104 — the trailing 2D square is kept despite the 3D cube between them (per-child filtering,
    // not break-on-first).
    let src = "union() { circle(5, $fn = 8); cube(2); translate([100, 0, 0]) square(3); }";
    assert!(matches!(d2(src), Shape2D::Union(ref c) if c.len() == 2));
    assert_eq!(warnings(src).len(), 2); // one Mixing + one Ignoring (the single 3D child)
}

#[test]
fn a_present_but_empty_primitive_still_fixes_the_dimension() {
    // Oracle `union() { cube(0); circle(5); }`: cube(0) is empty but PRESENT — it fixes dim = 3, so the
    // circle is "Ignoring 2D child object for 3D operation" and the result is 3D (empty). This is why
    // `cube(0)` lowers to a present `Leaf(empty)`, not the dimension-neutral `Empty`.
    assert!(matches!(
        evaluate_geometry("cube(0); circle(5);").unwrap(),
        Geo::D3(_)
    ));
    assert_eq!(
        warnings("cube(0); circle(5);"),
        [
            "Mixing 2D and 3D objects is not supported",
            "Ignoring 2D child object for 3D operation",
        ]
    );
}

#[test]
fn an_empty_block_is_dimension_neutral_and_drops_out() {
    // Oracle `difference() { {} cube(4, center=true); }`: the empty `{}` block is dropped (not an empty
    // first operand), so the cube survives (6 facets). `{}` → `Empty` is dimension-neutral, distinct from
    // a present-empty `cube(0)`. No mixing warning fires (nothing to mix).
    match evaluate_geometry("difference() { { } cube(4, center = true); }").unwrap() {
        Geo::D3(GeoNode::Difference(ref c)) => assert_eq!(c.len(), 1), // just the cube; the `{}` dropped
        other => panic!("expected a 3D Difference of one, got {other:?}"),
    }
    assert!(warnings("difference() { { } cube(4, center = true); }").is_empty());
}

// ─────────────────────────────── LOUD deferrals + no-backend flattening ───────────────────────────────

#[test]
fn hull_over_2d_builds_the_hull_node() {
    // X.4: `hull()` over 2D children lowers to `Shape2D::Hull` (was a LOUD Unimplemented) — the geometry
    // (Manifold `CrossSection::hull_of`) is checked by area in `backend.rs`. Here we assert the TREE: two
    // 2D children pooled under one Hull node, no mixing warning. (3D hull is `geometry_tree.rs`.)
    let src = "hull() { circle(5, $fn = 8); translate([10, 0]) circle(3, $fn = 8); }";
    match d2(src) {
        Shape2D::Hull(ref c) => assert_eq!(c.len(), 2),
        other => panic!("hull() over 2D should build Shape2D::Hull of two, got {other:?}"),
    }
    assert!(warnings(src).is_empty());
}

#[test]
fn minkowski_over_2d_is_still_loud() {
    // 2D minkowski stays deferred (Clipper2 `MinkowskiSum` — a separate J.3 follow-up, out of X.4 scope) —
    // LOUD, never silently wrong.
    assert!(matches!(
        evaluate_geometry("minkowski() { square(4); circle(1, $fn = 8); }").unwrap_err().root(),
        Error::Unimplemented(m) if m.contains("2D") && m.contains("minkowski")
    ));
}

#[test]
fn color_over_2d_tags_the_color() {
    // A VALID color on a 2D child is TAGGED onto the tree (`Shape2D::Color`, so the GUI can read it) but
    // changes no geometry — the wrapped child is exactly the uncolored shape (L.3.8). It USED to error LOUD;
    // recording the color beats dropping it (re-plumbing at the GUI) or silently diverging from OpenSCAD.
    match d2("color(\"red\") circle(3, $fn = 8);") {
        Shape2D::Color { ref child, .. } => assert_eq!(**child, d2("circle(3, $fn = 8);")),
        other => panic!("color() on 2D should tag Shape2D::Color, got {other:?}"),
    }
    // ...but an INVALID color (unknown name) inherits regardless of dimension → the 2D child passes through
    // UNWRAPPED (no color to record, matching OpenSCAD's -1 sentinel).
    assert_eq!(
        d2("color(\"notacolor\") circle(3, $fn = 8);"),
        d2("circle(3, $fn = 8);")
    );
}

#[test]
fn projection_builds_the_3d_to_2d_bridge() {
    // J.3.6, the last 2D↔3D bridge. A shadow (default cut = false) of a 3D solid → a 2D result wrapping
    // the 3D subtree.
    assert!(matches!(
        evaluate_geometry("projection() cube(2);").unwrap(),
        Geo::D2(Shape2D::Projection { cut: false, ref child }) if matches!(**child, GeoNode::Leaf(_))
    ));
    // `cut = true` rides the flag; a multi-object group collapses into one union node.
    assert!(matches!(
        evaluate_geometry("projection(cut = true) { cube(2); sphere(3, $fn = 8); }").unwrap(),
        Geo::D2(Shape2D::Projection { cut: true, ref child }) if matches!(**child, GeoNode::Union(_))
    ));
    // A 2D child is coerced OUT (force_3d) → an empty subtree, not a projection of a plane (OpenSCAD
    // warns `Ignoring 2D child object for 3D operation` + renders nothing).
    assert!(matches!(
        evaluate_geometry("projection() square(5);").unwrap(),
        Geo::D2(Shape2D::Projection { ref child, .. }) if matches!(**child, GeoNode::Empty)
    ));
}

#[test]
fn rotate_extrude_builds_the_2d_to_3d_bridge() {
    use fab_lang::ExtrudeKind;
    // A full revolution: default angle 360, segment count straight from `$fn`.
    assert!(matches!(
        evaluate_geometry("rotate_extrude($fn = 64) translate([10, 0]) square([2, 3]);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Rotate { angle, segments }, .. })
            if angle == 360.0 && segments == 64
    ));
    // A partial angle rides `angle=`; the profile's max radius (10 + circle r 2 = 12) sets the `$fa`/`$fs`
    // segment count when `$fn` is unset — proving `Shape2D::max_x` walked the translate.
    assert!(matches!(
        evaluate_geometry("rotate_extrude(angle = 90, $fa = 12, $fs = 2) translate([10, 0]) circle(2, $fn = 16);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Rotate { angle, segments }, .. })
            if angle == 90.0 && segments == fab_lang::fragments(12.0, 0.0, 12.0, 2.0)
    ));
    // A 3D child is coerced out (force_2d) → an empty profile, not a revolve of a solid.
    assert!(matches!(
        evaluate_geometry("rotate_extrude() cube(2);").unwrap(),
        Geo::D3(GeoNode::Extrude { ref child, .. }) if matches!(**child, Shape2D::Empty)
    ));
    // A UNION profile: max_x walks BOTH children (two concentric rings) → the farther one (22) drives
    // the fragment count — proving the boolean arm of the walk.
    assert!(matches!(
        evaluate_geometry("rotate_extrude($fa = 12, $fs = 2) { translate([10, 0]) square(2); translate([20, 0]) square(2); }").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Rotate { segments, .. }, .. })
            if segments == fab_lang::fragments(22.0, 0.0, 12.0, 2.0)
    ));
    // An OFFSET grows the child's extent by its positive delta → a larger radius (12 + 1), more segments.
    assert!(matches!(
        evaluate_geometry("rotate_extrude($fa = 12, $fs = 2) offset(1) translate([10, 0]) square(2);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Rotate { segments, .. }, .. })
            if segments == fab_lang::fragments(13.0, 0.0, 12.0, 2.0)
    ));
}

#[test]
fn linear_extrude_builds_the_2d_to_3d_bridge() {
    use fab_lang::ExtrudeKind;
    // linear_extrude wraps a 2D child in the GeoNode::Extrude bridge (a 3D result).
    match evaluate_geometry("linear_extrude(5) square(4);").unwrap() {
        Geo::D3(GeoNode::Extrude {
            ref kind,
            ref child,
        }) => {
            assert!(matches!(**child, Shape2D::Polygon(_)));
            assert!(matches!(
                *kind,
                ExtrudeKind::Linear { height, twist, center, .. } if height == 5.0 && twist == 0.0 && !center
            ));
        }
        other => panic!("expected a 3D Extrude, got {other:?}"),
    }
    // height / center / scale ride their args.
    assert!(matches!(
        evaluate_geometry("linear_extrude(height = 10, center = true, scale = [2, 0.5]) square(4);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Linear { height, center, scale, .. }, .. })
            if height == 10.0 && center && scale == [2.0, 0.5]
    ));
    // a 3D child is IGNORED (force_2d, no "Mixing") → an extrude of the empty region.
    assert!(matches!(
        evaluate_geometry("linear_extrude(5) cube(2);").unwrap(),
        Geo::D3(GeoNode::Extrude { ref child, .. }) if matches!(**child, Shape2D::Empty)
    ));
    // TWIST builds (J.3.4.1: negate + resample); the `$fn`-resolved facet count rides the kind.
    assert!(matches!(
        evaluate_geometry("linear_extrude(10, twist = 90, $fn = 32) square(4);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Linear { twist, facets, .. }, .. })
            if twist == 90.0 && facets == 32
    ));
    // explicit `slices` (a scalar `scale` too); a saturating slices count clamps, not overflows.
    assert!(matches!(
        evaluate_geometry("linear_extrude(5, slices = 4, scale = 2) square(4);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Linear { slices, scale, .. }, .. })
            if slices == 4 && scale == [2.0, 2.0]
    ));
    assert!(matches!(
        evaluate_geometry("linear_extrude(5, slices = 5e9) square(4);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Linear { slices, .. }, .. }) if slices == u32::MAX
    ));
    // a non-numeric height arg falls back to the default (100) — `as_num` returns None.
    assert!(matches!(
        evaluate_geometry("linear_extrude([1, 2]) square(4);").unwrap(),
        Geo::D3(GeoNode::Extrude { kind: ExtrudeKind::Linear { height, .. }, .. }) if height == 100.0
    ));
}

#[test]
fn a_2d_result_has_no_mesh_without_a_backend() {
    // evaluate() flattens via the no-backend `mesh_of`; a 2D result can't become a 3D mesh → LOUD.
    assert!(
        matches!(evaluate("circle(5);").unwrap_err().root(), Error::Unimplemented(m) if m.contains("2D"))
    );
    // ...same for a 2D-winning mixed program (the 3D child was dropped, leaving 2D).
    assert!(evaluate("circle(5); cube(2);").is_err());
}
