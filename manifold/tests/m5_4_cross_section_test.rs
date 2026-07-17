//! M.5.4.3 — the C++ `cross_section_test.cpp` suite ported 1:1 (all 15 TESTs, same order, same
//! constants). Default lane: every assertion is analytic or compares two of OUR meshes — the
//! C++-differential half of K.6 lives in `oracle.rs` (`m5_4_*`).
//!
//! Port deviations, each noted at its site:
//! - `EXPECT_FLOAT_EQ` (4 f32-ULPs) → a relative epsilon; exact-arithmetic cases use 1e-9.
//! - The C++ `Identical(MeshGL)` (test_main.cpp:304) compares vertex positions within 1e-4 and
//!   SORTS triangle triples before comparing (emission-order-insensitive). Our `identical`/
//!   `identical_within` are strictly TIGHTER — emission-order triangles + exact / 1e-9 vertices —
//!   so passing them implies passing the C++ gate; `identical_within` exists only because eager
//!   transforms shift vertices by ULPs where the C++ lazily composes. If a legal triangle-emission
//!   reorder ever fails the strict gate, loosen to the C++ sorted-compare semantics, not further.
//! - `Decompose` returns components in i_overlay's sweep order, not the C++ reversed-PolyTree
//!   order — components are identified by bounds, not index.
//! - Constructors/transforms return `Result` (the M.5.4.5 no-panic boundary); the C++ inputs here
//!   are all finite, so the ports unwrap.

use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::check;
use fab_manifold::cross_section::{CrossSection, FillRule, JoinType};
use fab_manifold::linalg::rotate2_degrees;
use fab_manifold::linalg::{Mat2x3, Rect, Vec2, Vec3};
use fab_manifold::mesh::Mesh;

fn v(x: f64, y: f64) -> Vec2 {
    Vec2::new(x, y)
}

/// Stands in for the C++ `Identical(MeshGL, MeshGL)` where both meshes come from the SAME
/// pipeline — strictly tighter than the C++ helper (see the header): exact equality, emission
/// order included.
fn identical(a: &Mesh, b: &Mesh) {
    let (ga, gb) = (a.to_mesh_gl(), b.to_mesh_gl());
    assert_eq!(ga.tri_verts, gb.tri_verts, "triangle indices differ");
    assert_eq!(
        ga.vert_properties, gb.vert_properties,
        "vertex properties differ"
    );
}

/// `Identical` relaxed to a vertex epsilon — for the eager-vs-composed transform deviation, where
/// per-step rounding makes byte equality the wrong ask.
fn identical_within(a: &Mesh, b: &Mesh, eps: f64) {
    let (ga, gb) = (a.to_mesh_gl(), b.to_mesh_gl());
    assert_eq!(ga.tri_verts, gb.tri_verts, "triangle indices differ");
    assert_eq!(ga.vert_properties.len(), gb.vert_properties.len());
    for (pa, pb) in ga.vert_properties.iter().zip(&gb.vert_properties) {
        assert!((pa - pb).abs() < eps, "vertex property {pa} vs {pb}");
    }
}

/// Triangulation-independent solid compare — for meshes whose 2D contours came out of DIFFERENT
/// boolean passes (decompose vs original), where contour start/vertex order legitimately differs.
fn same_solid(a: &Mesh, b: &Mesh) {
    assert!(
        (a.volume() - b.volume()).abs() < 1e-9,
        "volume {} vs {}",
        a.volume(),
        b.volume()
    );
    assert!(
        (a.surface_area() - b.surface_area()).abs() < 1e-9,
        "area {} vs {}",
        a.surface_area(),
        b.surface_area()
    );
    assert_eq!(check::genus(a), check::genus(b), "genus differs");
}

// TEST(CrossSection, Square)
#[test]
fn square() {
    let a = Mesh::cube(Vec3::new(5.0, 5.0, 5.0), false).unwrap();
    let b = CrossSection::square(v(5.0, 5.0), false)
        .unwrap()
        .extrude(5.0);
    assert!(boolean(&a, &b, OpType::Subtract).volume().abs() < 1e-9);
}

// TEST(CrossSection, MirrorUnion)
#[test]
fn mirror_union() {
    let a = CrossSection::square(v(5.0, 5.0), true).unwrap();
    let b = a.translate(v(2.5, 2.5)).unwrap();
    let cross = a.union(&b).union(&b.mirror(v(1.0, 1.0)).unwrap());
    // The C++ extrudes `result` (export-only); volume == 5·area pins the same construction.
    let result = cross.extrude(5.0);
    assert!((result.volume() - 5.0 * cross.area()).abs() < 1e-6);

    assert!((2.5 * a.area() - cross.area()).abs() < 1e-9);
    assert!(a.mirror(Vec2::ZERO).unwrap().is_empty());
}

// TEST(CrossSection, MirrorCheckAxis)
#[test]
fn mirror_check_axis() {
    let tri = CrossSection::from_polygons(&[vec![v(0.0, 0.0), v(5.0, 5.0), v(0.0, 10.0)]]).unwrap();

    let a = tri.mirror(v(1.0, 1.0)).unwrap().bounds();
    let a_expected =
        CrossSection::from_polygons(&[vec![v(0.0, 0.0), v(-10.0, 0.0), v(-5.0, -5.0)]])
            .unwrap()
            .bounds();
    assert!((a.min.x - a_expected.min.x).abs() < 0.001);
    assert!((a.min.y - a_expected.min.y).abs() < 0.001);
    assert!((a.max.x - a_expected.max.x).abs() < 0.001);
    assert!((a.max.y - a_expected.max.y).abs() < 0.001);

    let b = tri.mirror(v(-1.0, 1.0)).unwrap().bounds();
    let b_expected = CrossSection::from_polygons(&[vec![v(0.0, 0.0), v(10.0, 0.0), v(5.0, 5.0)]])
        .unwrap()
        .bounds();
    assert!((b.min.x - b_expected.min.x).abs() < 0.001);
    assert!((b.min.y - b_expected.min.y).abs() < 0.001);
    assert!((b.max.x - b_expected.max.x).abs() < 0.001);
    assert!((b.max.y - b_expected.max.y).abs() < 0.001);
}

// TEST(CrossSection, RoundOffset)
#[test]
fn round_offset() {
    let a = CrossSection::square(v(20.0, 20.0), true).unwrap();
    let segments = 20;
    let rounded = a.offset(5.0, JoinType::Round, 2.0, segments).unwrap();
    let result = rounded.extrude(5.0);

    assert_eq!(check::genus(&result), 0);
    assert!(
        (result.volume() - 4386.0).abs() < 1.0,
        "volume {}",
        result.volume()
    );
    assert_eq!(rounded.num_vert(), (segments + 4) as usize);
}

// TEST(CrossSection, BevelOffset)
#[test]
fn bevel_offset() {
    let a = CrossSection::square(v(20.0, 20.0), true).unwrap();
    let segments = 20;
    let beveled = a.offset(5.0, JoinType::Bevel, 2.0, segments).unwrap();
    let result = beveled.extrude(5.0);

    assert_eq!(check::genus(&result), 0);
    let expected = 5.0 * ((20.0 + 2.0 * 5.0) * (20.0 + 2.0 * 5.0) - 2.0 * 5.0 * 5.0);
    assert!(
        (result.volume() - expected).abs() < 1.0,
        "volume {}",
        result.volume()
    );
    assert_eq!(beveled.num_vert(), 4 + 4);
}

// TEST(CrossSection, Empty)
#[test]
fn empty() {
    let e = CrossSection::from_polygons(&[Vec::new(), Vec::new()]).unwrap();
    assert!(e.is_empty());
}

// TEST(CrossSection, Rect)
#[test]
fn rect() {
    let (w, h) = (10.0, 5.0);
    let rect = Rect::from_points(v(0.0, 0.0), v(w, h));
    let cross = CrossSection::from_rect(rect).unwrap();
    let area = rect.area();

    assert!((area - w * h).abs() < 1e-12);
    assert!((area - cross.area()).abs() < 1e-12);
    assert!(rect.contains_point(v(5.0, 5.0)));
    assert!(rect.contains_rect(cross.bounds()));
    assert!(rect.contains_rect(Rect::default()));
    assert!(rect.does_overlap(Rect::from_points(v(5.0, 5.0), v(15.0, 15.0))));
    assert!(Rect::default().is_empty());
}

// TEST(CrossSection, Transform)
#[test]
fn transform() {
    let sq = CrossSection::square(v(10.0, 10.0), false).unwrap();
    let a = sq
        .rotate(45.0)
        .unwrap()
        .scale(v(2.0, 3.0))
        .unwrap()
        .translate(v(4.0, 5.0))
        .unwrap();

    // The C++ builds trans·scale·rot as mat3 products; compose() is that chain.
    let m = Mat2x3::translate(v(4.0, 5.0))
        .compose(Mat2x3::scale(v(2.0, 3.0)).compose(rotate2_degrees(45.0)));
    let b = sq.transform(m).unwrap();
    let b_copy = b.clone();

    let ex_b = b.extrude(1.0);
    // Eager per-step vs one composed apply: same math, different rounding points (the documented
    // lazy-transform deviation) — vertices agree within ULP noise, not bytes.
    identical_within(&a.extrude(1.0), &ex_b, 1e-9);
    // A copy transforms identically — exact.
    identical(&ex_b, &b_copy.extrude(1.0));
}

// TEST(CrossSection, Warp)
#[test]
fn warp() {
    let sq = CrossSection::square(v(10.0, 10.0), false).unwrap();
    let a = sq
        .scale(v(2.0, 3.0))
        .unwrap()
        .translate(v(4.0, 5.0))
        .unwrap();
    let b = sq
        .warp(|p| {
            p.x = p.x * 2.0 + 4.0;
            p.y = p.y * 3.0 + 5.0;
        })
        .unwrap();

    assert_eq!(sq.num_vert(), 4);
    assert_eq!(sq.num_contour(), 1);
    // The C++ leaves a/b unused; asserting they agree keeps the constructions honest.
    assert!((a.area() - b.area()).abs() < 1e-9);
}

// TEST(CrossSection, Decompose)
#[test]
fn decompose() {
    let a = CrossSection::square(v(2.0, 2.0), true)
        .unwrap()
        .difference(&CrossSection::square(v(1.0, 1.0), true).unwrap());
    let b = a.translate(v(4.0, 4.0)).unwrap();
    let ab = a.union(&b);
    let decomp = ab.decompose();
    let recomp = CrossSection::compose(&decomp);

    assert_eq!(decomp.len(), 2);
    assert_eq!(decomp[0].num_contour(), 2);
    assert_eq!(decomp[1].num_contour(), 2);

    // ORDER deviation: identify components by bounds (a straddles the origin), not index.
    let (da, db) = if decomp[0].bounds().contains_point(Vec2::ZERO) {
        (&decomp[0], &decomp[1])
    } else {
        (&decomp[1], &decomp[0])
    };
    // The decomposed contours come out of a different boolean pass than a/b's — vertex order
    // legitimately differs, so this is a solid compare, not the C++ byte-Identical.
    same_solid(&a.extrude(1.0), &da.extrude(1.0));
    same_solid(&b.extrude(1.0), &db.extrude(1.0));
    same_solid(&ab.extrude(1.0), &recomp.extrude(1.0));
}

// TEST(CrossSection, FillRule)
#[test]
fn fill_rule() {
    let polygon = vec![
        v(-7.0, 13.0),
        v(-7.0, 12.0),
        v(-5.0, 9.0),
        v(-5.0, 8.1),
        v(-4.8, 8.0),
    ];
    let polygon = core::slice::from_ref(&polygon);

    let positive = CrossSection::from_polygons(polygon).unwrap();
    assert!((positive.area() - 0.683).abs() < 0.001);

    let negative = CrossSection::from_polygons_with(polygon, FillRule::Negative).unwrap();
    assert!((negative.area() - 0.193).abs() < 0.001);

    let even_odd = CrossSection::from_polygons_with(polygon, FillRule::EvenOdd).unwrap();
    assert!((even_odd.area() - 0.875).abs() < 0.001);

    let non_zero = CrossSection::from_polygons_with(polygon, FillRule::NonZero).unwrap();
    assert!((non_zero.area() - 0.875).abs() < 0.001);
}

// TEST(CrossSection, Hull)
#[test]
fn hull() {
    let circ = CrossSection::circle(10.0, 360).unwrap();
    let circs = vec![
        circ.clone(),
        circ.translate(v(0.0, 30.0)).unwrap(),
        circ.translate(v(30.0, 0.0)).unwrap(),
    ];
    let _circ_tri = CrossSection::hull_of(&circs);
    let centres = [v(0.0, 0.0), v(0.0, 30.0), v(30.0, 0.0), v(15.0, 5.0)];
    let tri = CrossSection::hull_of_points(&centres).unwrap();

    let circ_area = circ.area();
    // Hull of an annulus == the outer circle (same vertices back).
    let annulus_hull_area = circ
        .difference(&circ.scale(v(0.8, 0.8)).unwrap())
        .hull()
        .area();
    assert!(
        ((circ_area - annulus_hull_area) / circ_area).abs() < 1e-9,
        "annulus hull area {annulus_hull_area} vs circle {circ_area}"
    );
    // 3 disjoint circles minus the hull-triangle of their centres = 2.5 circles (the C++ FLOAT_EQ
    // constant relation). Boolean-grid areas: relative 1e-7 (f32-ULP class).
    let cut = CrossSection::batch_boolean(&circs, OpType::Add)
        .difference(&tri)
        .area();
    assert!(
        ((cut - circ_area * 2.5) / (circ_area * 2.5)).abs() < 1e-7,
        "swept area {cut} vs {}",
        circ_area * 2.5
    );
}

// TEST(CrossSection, HullError)
#[test]
fn hull_error() {
    let rounded_rectangle = |x: f64, y: f64, radius: f64, segments: i32| {
        let circ = CrossSection::circle(radius, segments).unwrap();
        CrossSection::hull_of(&[
            circ.translate(v(radius, radius)).unwrap(),
            circ.translate(v(x - radius, radius)).unwrap(),
            circ.translate(v(x - radius, y - radius)).unwrap(),
            circ.translate(v(radius, y - radius)).unwrap(),
        ])
    };
    let rr = rounded_rectangle(51.0, 36.0, 9.0, 36);

    // EXPECT_FLOAT_EQ is an f32 4-ULP compare (~5e-4 abs here); we land 5.5e-6 off the C++
    // constant — inside the C++ test's own tolerance, gated at the FLOAT_EQ class.
    assert!(
        ((rr.area() - 1765.1790375559026) / 1765.1790375559026).abs() < 5e-7,
        "area {}",
        rr.area()
    );
    assert_eq!(rr.num_vert(), 40);
}

// TEST(CrossSection, BatchBoolean)
#[test]
fn batch_boolean() {
    let square = CrossSection::square(v(100.0, 100.0), false).unwrap();
    let circle1 = CrossSection::circle(30.0, 30)
        .unwrap()
        .translate(v(-10.0, 30.0))
        .unwrap();
    let circle2 = CrossSection::circle(20.0, 30)
        .unwrap()
        .translate(v(110.0, 20.0))
        .unwrap();
    let circle3 = CrossSection::circle(40.0, 30)
        .unwrap()
        .translate(v(50.0, 110.0))
        .unwrap();
    let all = [square, circle1, circle2, circle3];

    let intersect = CrossSection::batch_boolean(&all, OpType::Intersect);
    assert!(intersect.area().abs() < 1e-12);
    assert_eq!(intersect.num_vert(), 0);

    // The C++ constants are Clipper2-grid areas; ours differ by grid quantization only — gate at
    // the FLOAT_EQ class (relative 5e-7).
    let add = CrossSection::batch_boolean(&all, OpType::Add);
    assert!(
        ((add.area() - 16278.637002) / 16278.637002).abs() < 5e-7,
        "add area {}",
        add.area()
    );

    let subtract = CrossSection::batch_boolean(&all, OpType::Subtract);
    assert!(
        ((subtract.area() - 7234.478452) / 7234.478452).abs() < 5e-7,
        "subtract area {}",
        subtract.area()
    );

    // Vertex counts are engine-arrangement facts — assert ours match the C++ exactly; a mismatch
    // here is a real finding, not noise.
    assert_eq!(add.num_vert(), 66);
    assert_eq!(subtract.num_vert(), 42);
}

// TEST(CrossSection, NegativeOffset)
#[test]
fn negative_offset() {
    let plus_sign = CrossSection::square(v(30.0, 50.0), true)
        .unwrap()
        .union(&CrossSection::square(v(50.0, 30.0), true).unwrap());
    let dilated = plus_sign.offset(-10.0, JoinType::Round, 2.0, 1024).unwrap();
    let expected = 30.0 * 30.0 - 10.0 * 10.0 * core::f64::consts::PI;
    assert!(
        (dilated.area() - expected).abs() < 0.01,
        "area {} vs {expected}",
        dilated.area()
    );
}
