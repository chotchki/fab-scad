//! G.3.5 geometry conformance corpus — sphere/cube/cylinder tessellation + arg resolution, driven
//! end to end through `evaluate()`. Vertex counts + positions are the conformance signal; the
//! strictest triangle-set tier is resolved at G.3.7.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration-test helpers: unwrap/expect/panic ARE the assertions; exact geometry asserts are deterministic"
)]

use fab_lang::{Error, Mesh, evaluate};

fn mesh(src: &str) -> Mesh {
    evaluate(src).expect("evaluates to a mesh")
}

fn err(src: &str) -> Error {
    evaluate(src).unwrap_err()
}

/// Every vertex lies on the sphere/ring of radius `r` (within float epsilon).
fn all_on_radius(mesh: &Mesh, r: f64) {
    for v in &mesh.verts {
        let d2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        assert!((d2 - r * r).abs() < 1e-9, "vertex {v:?} not on radius {r}");
    }
}

// ─────────────────────────────── sphere ────────────────────────────────────────────────────────

#[test]
fn sphere_vertex_count_and_positions() {
    // $fn=8 → num_rings = (8+1)/2 = 4 → 4*8 = 32 vertices; caps + quads → triangles.
    let m = mesh("sphere(1, $fn = 8);");
    assert_eq!(m.vert_count(), 32);
    assert!(m.tri_count() > 0);
    all_on_radius(&m, 1.0);
    // the first vertex sits at theta=0 of ring 0: y is EXACTLY 0 (sin_degrees(0) == 0).
    assert_eq!(m.verts[0][1], 0.0);
}

#[test]
fn sphere_radius_and_diameter_and_defaults() {
    all_on_radius(&mesh("sphere(d = 10, $fn = 8);"), 5.0); // diameter → r = 5
    all_on_radius(&mesh("sphere(r = 3, $fn = 8);"), 3.0); // named radius
    all_on_radius(&mesh("sphere(2, $fn = 8);"), 2.0); // positional
    all_on_radius(&mesh("sphere($fn = 8);"), 1.0); // default r = 1
}

#[test]
fn sphere_default_fn_uses_fa_fs() {
    // $fn = 0 → the $fa/$fs branch; r=5 → 16 fragments → 8 rings → 128 verts.
    assert_eq!(mesh("sphere(5);").vert_count(), 8 * 16);
}

#[test]
fn sphere_degenerate_and_guarded() {
    assert_eq!(mesh("sphere(0);").vert_count(), 0); // r <= 0 → empty
    assert_eq!(mesh("sphere(-1);").vert_count(), 0);
    assert_eq!(mesh("sphere(1, $fn = 100000);").vert_count(), 0); // unrepresentable in u32 → empty
}

// ─────────────────────────────── cube ──────────────────────────────────────────────────────────

#[test]
fn cube_corners() {
    let m = mesh("cube([2, 3, 4]);");
    assert_eq!(m.vert_count(), 8);
    assert_eq!(m.tri_count(), 12);
    assert_eq!(m.verts[0].to_array(), [0.0, 0.0, 0.0]);
    assert_eq!(m.verts[6].to_array(), [2.0, 3.0, 4.0]);
}

#[test]
fn cube_scalar_centered_and_defaults() {
    let m = mesh("cube(2, center = true);");
    assert_eq!(m.verts[0].to_array(), [-1.0, -1.0, -1.0]); // scalar → [2,2,2], centered
    assert_eq!(m.verts[6].to_array(), [1.0, 1.0, 1.0]);
    assert_eq!(mesh("cube();").verts[6].to_array(), [1.0, 1.0, 1.0]); // default [1,1,1]
    assert_eq!(mesh("cube([1, 2]);").verts[6].to_array(), [1.0, 1.0, 1.0]); // short vector → default
}

#[test]
fn cube_degenerate() {
    assert_eq!(mesh("cube(0);").vert_count(), 0);
    assert_eq!(mesh("cube([1, 0, 1]);").vert_count(), 0);
}

// ─────────────────────────────── cylinder ──────────────────────────────────────────────────────

#[test]
fn cylinder_rings_and_cones() {
    assert_eq!(mesh("cylinder(h = 10, r = 5, $fn = 8);").vert_count(), 16); // 2 rings × 8
    assert_eq!(
        mesh("cylinder(h = 10, r1 = 5, r2 = 0, $fn = 8);").vert_count(),
        9
    ); // cone: ring + apex
    assert_eq!(
        mesh("cylinder(h = 10, r1 = 0, r2 = 5, $fn = 8);").vert_count(),
        9
    ); // inverted cone
}

#[test]
fn cylinder_radius_forms() {
    // d1/d2 → r1=5, r2=2; both rings present.
    let m = mesh("cylinder(h = 10, d1 = 10, d2 = 4, $fn = 8);");
    assert_eq!(m.vert_count(), 16);
    all_on_radius_at_z(&m, 5.0, 0.0); // bottom ring radius 5 at z=0
    // centered → z spans −5..5
    let c = mesh("cylinder(h = 10, r = 5, center = true, $fn = 8);");
    assert!(c.verts.iter().any(|v| v[2] == -5.0) && c.verts.iter().any(|v| v[2] == 5.0));
}

fn all_on_radius_at_z(mesh: &Mesh, r: f64, z: f64) {
    let ring: Vec<_> = mesh.verts.iter().filter(|v| v[2] == z).collect();
    assert!(!ring.is_empty());
    for v in ring {
        assert!((v[0] * v[0] + v[1] * v[1] - r * r).abs() < 1e-9);
    }
}

#[test]
fn cylinder_degenerate() {
    assert_eq!(mesh("cylinder(h = 0, r = 5);").vert_count(), 0); // h <= 0
    assert_eq!(mesh("cylinder(h = 10, r1 = 0, r2 = 0);").vert_count(), 0); // both apex → empty
    assert_eq!(mesh("cylinder(h = 10, r = -1);").vert_count(), 0); // negative radius
    // cylinder's guard is 2*nf (linear), so it only trips near u32::MAX/2, not at $fn=100000.
    assert_eq!(
        mesh("cylinder(h = 10, r = 5, $fn = 3000000000);").vert_count(),
        0
    ); // 2*nf > u32::MAX
}

// ─────────────────────────────── program eval ──────────────────────────────────────────────────

#[test]
fn program_eval() {
    assert_eq!(mesh("").vert_count(), 0); // empty program
    assert_eq!(mesh(";").vert_count(), 0); // only empty statements
    assert_eq!(mesh("x = 5; sphere(x, $fn = 8);").vert_count(), 32); // assignment then use
    assert_eq!(mesh("{ sphere(1, $fn = 8); }").vert_count(), 32); // block
    // a block-INTERNAL assignment binds sequentially (blocks don't yet hoist — that rides Phase J with
    // module bodies; top-level hoisting is I.2.7). In-order, so it matches either way:
    assert_eq!(
        mesh("{ x = 5; sphere(x, $fn = 8); }"),
        mesh("sphere(5, $fn = 8);")
    );
}

#[test]
fn beyond_the_subset_is_loud() {
    assert!(matches!(
        err("sphere(1); cube(1);"),
        Error::Unimplemented(_)
    )); // implicit union
    assert!(matches!(err("foo();"), Error::Unimplemented(_))); // unknown module
    assert!(matches!(
        err("translate([1,0,0]) cube(1);"),
        Error::Unimplemented(_)
    )); // transform
    assert!(matches!(
        err("v = 1; sphere(bogus_fn(v));"),
        Error::Unimplemented(_)
    )); // an UNKNOWN function in an arg (builtin/known-function calls in args now work — I.4)
}

#[test]
fn whole_scope_variable_hoisting() {
    // Top-level assignments hoist: geometry sees a variable's FINAL value regardless of source
    // position, last-assignment-wins, evaluated in first-occurrence order (so forward/self refs are
    // undef). Every case matches a `ECHO:` probe against the real OpenSCAD 2026.06.12 oracle.
    // read-before-assign → the hoisted value:
    assert_eq!(
        mesh("sphere(x, $fn = 8); x = 5;"),
        mesh("sphere(5, $fn = 8);")
    );
    // reassignment, last wins:
    assert_eq!(
        mesh("x = 1; sphere(x, $fn = 8); x = 9;"),
        mesh("sphere(9, $fn = 8);")
    );
    // the self-referential gotcha: `n = n + 4` sees n as undef → sphere(undef):
    assert_eq!(
        mesh("n = 1; n = n + 4; sphere(n, $fn = 8);"),
        mesh("sphere(undef, $fn = 8);")
    );
    // forward reference → undef (a is evaluated before b is bound, in first-occurrence order):
    assert_eq!(
        mesh("sphere(a, $fn = 8); a = b; b = 5;"),
        mesh("sphere(undef, $fn = 8);")
    );
    // backward reference resolves normally:
    assert_eq!(
        mesh("b = 5; a = b; sphere(a, $fn = 8);"),
        mesh("sphere(5, $fn = 8);")
    );
}

#[test]
fn evaluation_is_deterministic() {
    let src = "sphere(3, $fn = 16);";
    assert_eq!(mesh(src).verts, mesh(src).verts);
}
