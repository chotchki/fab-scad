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

use fab_lang::{Error, Mesh, Message, evaluate, evaluate_full};

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
fn deferred_builtins_name_their_feature() {
    // J.4: text/import/minkowski/surface are LOUD-deferred stubs — the error NAMES the feature (+ its
    // task), never a silent nothing and never a misleading "unknown module — a typo?".
    for (src, feature) in [
        ("text(\"hi\");", "text()"),
        ("import(\"a.stl\");", "import()"),
        (
            "minkowski() { cube(1); sphere(1, $fn = 8); }",
            "minkowski()",
        ),
        ("surface(\"h.dat\");", "surface()"),
    ] {
        assert!(
            matches!(&err(src), Error::Unimplemented(m) if m.contains(feature)),
            "{src}: expected a LOUD defer naming {feature}, got {:?}",
            err(src)
        );
    }
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

// ─────────────────────────────── polyhedron ──────────────────────────────────────────────────────

#[test]
fn polyhedron_vertices_and_fan_triangulation() {
    // a tetrahedron: 4 points verbatim, 4 triangular faces → 4 tris
    let tet = mesh(
        "polyhedron(points=[[0,0,0],[1,0,0],[0,1,0],[0,0,1]], \
         faces=[[0,2,1],[0,1,3],[1,2,3],[2,0,3]]);",
    );
    assert_eq!(tet.vert_count(), 4);
    assert_eq!(tet.tri_count(), 4);
    assert_eq!(
        [tet.verts[1][0], tet.verts[1][1], tet.verts[1][2]],
        [1.0, 0.0, 0.0]
    ); // verbatim

    // a square pyramid: the QUAD base fan-triangulates to 2, plus 4 triangular sides = 6
    let pyr = mesh(
        "polyhedron(points=[[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0.5,0.5,1]], \
         faces=[[0,1,2,3],[0,4,1],[1,4,2],[2,4,3],[3,4,0]]);",
    );
    assert_eq!(pyr.vert_count(), 5);
    assert_eq!(pyr.tri_count(), 6);
    // the base quad [0,1,2,3] fans from vertex 0, each triangle REVERSED (J.2.6): OpenSCAD winds faces
    // clockwise-from-outside, Manifold wants CCW, so (0,1,2)→(0,2,1) and (0,2,3)→(0,3,2). Without the
    // flip the solid is inside-out (a 2.0 boolean residual vs the oracle — the whole volume wrong).
    assert_eq!(pyr.tris[0].0, [0, 2, 1]);
    assert_eq!(pyr.tris[1].0, [0, 3, 2]);
}

#[test]
fn polyhedron_drops_bad_faces_without_panicking() {
    // an out-of-range index (5, past the 3-vertex table) drops that triangle; a <3-vertex face drops too
    let m = mesh("polyhedron(points=[[0,0,0],[1,0,0],[0,1,0]], faces=[[0,1,2],[0,1,5],[0,1]]);");
    assert_eq!(m.vert_count(), 3); // points kept verbatim
    assert_eq!(m.tri_count(), 1); // only [0,1,2] survives; the OOB and the 2-vertex face drop
    // a negative index is out of range too (OpenSCAD's size_t cast overflows) → dropped
    assert_eq!(
        mesh("polyhedron(points=[[0,0,0],[1,0,0],[0,1,0]], faces=[[0,1,-1]]);").tri_count(),
        0
    );
    // no points / no faces → an empty mesh, not an error
    assert_eq!(mesh("polyhedron(points=[], faces=[]);").tri_count(), 0);
    // a non-3-vector point (here a bare number) and a non-list face (a string) are each DROPPED — the
    // malformed-entry arms — leaving the two good points + the one good face, whose refs then dangle:
    let bad = mesh("polyhedron(points=[[0,0,0],[1,0,0],7], faces=[[0,1,2],\"x\"]);");
    assert_eq!(bad.vert_count(), 2); // the number `7` isn't a point → dropped
    assert_eq!(bad.tri_count(), 0); // face [0,1,2] refs the dropped point 2 → OOB → drops; "x" drops
}

#[test]
fn polyhedron_out_of_range_index_warns_and_renders() {
    // J.2.6.2: OpenSCAD WARNS on an out-of-range point index (bug-for-bug text) + drops that FACE (not
    // just a triangle) + renders the rest — never an error. Here faces[4][2] = 9 past the 5-point table.
    let ev = evaluate_full(
        "polyhedron(points=[[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0.5,0.5,1]], \
         faces=[[0,1,2,3],[0,4,1],[1,4,2],[2,4,3],[3,4,9]]);",
    )
    .expect("renders (warn, not error)");
    assert_eq!(ev.mesh.tri_count(), 5); // the base (2) + 3 valid sides; the 9-index face dropped
    assert!(
        ev.messages.iter().any(|m| matches!(
            m,
            Message::Warning(w) if w == "Point index 9 is out of bounds (from faces[4][2])"
        )),
        "expected OpenSCAD's exact out-of-bounds warning, got {:?}",
        ev.messages
    );
    // a whole QUAD face with one bad index drops ENTIRELY (OpenSCAD's per-face rule, not per-triangle):
    // [0,1,2,9] would fan to (0,2,1) + (0,3=9,2) — the second bad — but OpenSCAD drops BOTH → 0 tris here.
    let quad = evaluate_full("polyhedron(points=[[0,0,0],[1,0,0],[1,1,0]], faces=[[0,1,2,9]]);")
        .expect("renders");
    assert_eq!(quad.mesh.tri_count(), 0); // the whole face dropped, not just the bad triangle
}
