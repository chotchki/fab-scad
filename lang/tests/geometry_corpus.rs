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
    // Geometry needing the BACKEND (implicit union, transforms, booleans) is still LOUD in the pure-eval
    // subset — Error::Unimplemented points you at evaluate_geometry (J.2). (Unknown SYMBOLS are no longer
    // loud — warn-and-undef, L.5.7 — see the module/loader corpora.)
    assert!(matches!(
        err("sphere(1); cube(1);").root(),
        Error::Unimplemented(_)
    )); // implicit union
    assert!(matches!(
        err("translate([1,0,0]) cube(1);").root(),
        Error::Unimplemented(_)
    )); // transform
}

#[test]
fn deferred_builtins_club_is_empty() {
    // The LOUD-defer club's history: import/surface → File NEEDs; text() → implemented; 2D hull() →
    // implemented (X.4); 2D minkowski → implemented (AC.2, the kernel's tiered sum). This tombstone
    // pins that the LAST member left: a 2D minkowski no longer defers naming itself — it builds the
    // Shape2D node (whose lowering needs the real backend, like every composite; that generic
    // needs-a-backend message is a different, correct error on this backend-free path).
    let e = err("minkowski() square(5);");
    assert!(
        !format!("{e}").contains("not yet wired"),
        "2D minkowski must not LOUD-defer anymore, got {e:?}"
    );
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

// ─────────────────── instantiation modifiers (`* ! % #`) — eval must honor OUTPUT-geometry ones ──────────
// The parser records `! # % *` (ast.rs `Modifiers`); `eval_stmt` honors them. Surfaced by the L.3 models
// sweep, where `*`-parked variants (`*alternate();`) rendered as REAL geometry — the top divergence cause
// vs the oracle. These cover the cases whose result stays a single no-backend primitive; the transform-
// keeping `!` + full oracle agreement live in the fab-scad `differential` suite.

/// [min, max] over each axis across all vertices.
fn bbox(m: &Mesh) -> ([f64; 3], [f64; 3]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for v in &m.verts {
        for i in 0..3 {
            lo[i] = lo[i].min(v[i]);
            hi[i] = hi[i].max(v[i]);
        }
    }
    (lo, hi)
}

#[test]
fn star_disable_drops_the_subtree() {
    // `*` renders nothing: the sphere (r=20) never reaches the mesh, so the bbox stays the bare cube's.
    let m = mesh("cube(10); *sphere(20, $fn = 8);");
    assert_eq!(m.vert_count(), mesh("cube(10);").vert_count());
    assert_eq!(bbox(&m), ([0.0; 3], [10.0; 3]));
}

#[test]
fn percent_background_excluded_from_output() {
    // `%` is a preview-only ghost — F6 render / STL export omits it, so the mesh matches the bare cube.
    assert_eq!(
        bbox(&mesh("cube(10); %sphere(20, $fn = 8);")),
        ([0.0; 3], [10.0; 3])
    );
}

#[test]
fn hash_highlight_is_a_render_no_op() {
    // `#` highlights in preview but changes nothing in the exported geometry.
    assert_eq!(
        mesh("#cube(10);").vert_count(),
        mesh("cube(10);").vert_count()
    );
}

// Modifiers on `if` (AA.1): `if` is grammatically an instantiation, so each modifier behaves exactly
// as on a module call. All four ORACLE-VERIFIED 2026-07-22 against OpenSCAD (same programs, bbox +
// vert equality) — including `!if` dropping the ancestor transform, mirroring bang_root below.

#[test]
fn star_disable_on_if_drops_the_subtree() {
    let m = mesh("cube(10); *if (true) sphere(20, $fn = 8);");
    assert_eq!(m.vert_count(), mesh("cube(10);").vert_count());
    assert_eq!(bbox(&m), ([0.0; 3], [10.0; 3]));
}

#[test]
fn percent_background_on_if_excluded_from_output() {
    assert_eq!(
        bbox(&mesh("cube(10); %if (true) sphere(20, $fn = 8);")),
        ([0.0; 3], [10.0; 3])
    );
}

#[test]
fn hash_highlight_on_if_is_a_render_no_op() {
    assert_eq!(
        mesh("#if (true) cube(10);").vert_count(),
        mesh("cube(10);").vert_count()
    );
}

#[test]
fn bang_root_on_if_renders_only_its_subtree() {
    // The ancestor `translate` AND the sibling sphere are discarded — the cube lands at the origin.
    let m = mesh("translate([50, 0, 0]) !if (true) cube(10); sphere(20, $fn = 8);");
    assert_eq!(m.vert_count(), 8);
    assert_eq!(bbox(&m), ([0.0; 3], [10.0; 3]));
}

#[test]
fn disabled_if_condition_never_evaluates() {
    // `*` drops side effects too: a condition that would ERROR (unknown function) is never reached —
    // mirrors a disabled call's unevaluated args.
    let m = mesh("cube(10); *if (no_such_function()) sphere(20, $fn = 8);");
    assert_eq!(bbox(&m), ([0.0; 3], [10.0; 3]));
}

#[test]
fn bang_root_renders_only_its_subtree() {
    // `!` renders ONLY its subtree — the ancestor `translate` AND the sibling `sphere` are discarded, so the
    // cube lands at the ORIGIN (dropping the ancestor transform reduces it to a bare Leaf). Oracle-verified.
    let m = mesh("translate([50, 0, 0]) !cube(10); sphere(20, $fn = 8);");
    assert_eq!(
        m.vert_count(),
        8,
        "only the cube's 8 verts — the sibling sphere is gone"
    );
    assert_eq!(
        bbox(&m),
        ([0.0; 3], [10.0; 3]),
        "cube at origin — the ancestor translate was dropped"
    );
}

#[test]
fn assert_and_echo_pass_through_to_child_geometry() {
    // `assert(cond) <geometry>` and `echo(…) <geometry>` are PASSTHROUGH modules — the child renders after the
    // check/emit. BOSL2's `left()`/`right()`/`fwd()`/`back()` guard their `translate(…) children()` body with a
    // semicolon-LESS `assert(is_finite(x), …)`, so the geometry is the assert's CHILD; dropping it rendered
    // EMPTY (the single biggest missing-geometry cause in the L.3 models sweep — those transforms are ubiquitous).
    assert_eq!(mesh("assert(true) cube(10);").vert_count(), 8); // passing guard → child renders
    assert_eq!(mesh("echo(\"hi\") cube(10);").vert_count(), 8);
    // a FAILING guard is LOUD in the console but NON-fatal (L.5.8): it halts before the child — so the
    // mesh is EMPTY here — and warns, rather than erroring the whole render.
    assert_eq!(mesh("assert(1 > 2) cube(10);").vert_count(), 0); // child unreached → empty, not an Err
    // (a guard over a TRANSFORMED child needs the backend → oracle-tested in fab-scad `differential`)
    // the statement form (semicolon, no child) is unchanged — a pure check/emit, no geometry:
    assert_eq!(mesh("assert(true); cube(10);").vert_count(), 8); // the cube is a SIBLING here, still one object
}

/// AA.4.3 end-to-end: a value far past the old 64-deep parser cliff parses, EVALS, and `str()`s —
/// the whole deep-value pipeline (spine parse → explicit-stack eval → iterative formatter → cache
/// keys → iterative Drop) on one program. 2000 deep ≈ 30× the old cliff.
#[test]
fn deep_value_parses_evals_and_formats() {
    let n = 2000;
    let src = format!(
        "v = {}1{}; echo(len = len(str(v)));cube(1);",
        "[".repeat(n),
        "]".repeat(n)
    );
    let m = mesh(&src);
    assert_eq!(m.vert_count(), 8, "the cube still renders alongside");
}

/// AB.3 end-to-end: comprehension NESTING and vector/generator ALTERNATION both cost heap now —
/// 5000-deep `each`-of-vector chains (the exact alternation that overflowed post-AA.4) and
/// bindingless-`for` nesting evaluate clean. ~75× the old 64-deep cliff.
#[test]
fn deep_comprehensions_eval_on_the_machine() {
    let n = 5000;
    let each = format!(
        "v=[{}1{}]; echo(len(v)); cube(1);",
        "each [".repeat(n),
        "]".repeat(n)
    );
    assert_eq!(mesh(&each).vert_count(), 8);
    let for_nest = format!(
        "v=[{}1{}]; echo(len(v)); cube(1);",
        "for(i=[0:0]) [".repeat(n),
        "]".repeat(n)
    );
    assert_eq!(mesh(&for_nest).vert_count(), 8);
}

/// AB.2 end-to-end: assert/echo-chained RECURSION (the tail-recursion-tests crasher shape) evals on
/// the machine — 10k levels of let+assert+echo+ternary recursion return clean.
#[test]
fn assert_echo_chained_recursion_evals() {
    let src = "function f(n) = let(x = n) assert(x >= 0) n == 0 ? 42 : f(n - 1);\n\
               echo(f = f(10000)); cube(1);";
    assert_eq!(mesh(src).vert_count(), 8);
}

/// AD.2: zero-progress recursion errors LOUD ("Recursion detected", upstream's verdict) instead of
/// grinding for hours. The task machine tail-collapses `crash() = crash()` (the outer body-eval IS the
/// inner dispatch — no task-stack growth), so the guard counts calls IN FLIGHT, not stack depth.
/// Census: recursion-test-function.scad / recursion-test-function2.scad.
#[test]
fn zero_progress_recursion_is_detected() {
    let e = err("function crash() = crash();\necho(crash());\ncube(1);");
    let msg = e.to_string();
    assert!(
        msg.contains("Recursion detected calling function 'crash'"),
        "wanted upstream's verdict, got: {msg}"
    );
}

/// AD.3: a `for` range whose element count overflows uint32 warns "too many elements" and iterates
/// ZERO times — upstream's verdict, pinned by for-tests.scad's own annotations (`[0:1:4294967294]`,
/// count exactly `u32::MAX`, already warns). Was: a silent 10M-capped grind (the census's 26s/53s
/// timeouts). All three iteration seams: statement-for, comprehension-for, `each`.
#[test]
fn too_many_range_elements_warns_and_skips() {
    let ev = evaluate_full(
        "for (i = [0:1:4294967296]) { echo(i); }\n\
         a = [for (i = [0:1:8589934592]) i];\n\
         b = [each [0:1:4294967296]];\n\
         echo(lens = [len(a), len(b)]);\n\
         cube(1);",
    )
    .expect("evaluates");
    assert_eq!(
        ev.echos(),
        ["lens = [0, 0]"],
        "every seam iterates zero times"
    );
    let warnings = ev.warnings();
    assert_eq!(warnings.len(), 3, "one warning per seam: {warnings:?}");
    assert!(warnings.iter().all(|w| w.contains("too many elements")));
    // for-tests' "Correct" case stays correct: a big-but-legal range still iterates.
    let ok = evaluate_full("echo(len([for (i = [0:1:5000]) i])); cube(1);").expect("evaluates");
    assert_eq!(ok.echos(), ["5001"]);
    assert!(ok.warnings().is_empty());
}

/// AD.4: the C-style-for iteration limit, oracle-probed at BOTH edges — exactly 1,000,000 iterations
/// complete clean, 1,000,001 (and the census's infinite `for(b=0; b!=1; b=0)`) is upstream's hard
/// "For loop counter exceeded limit". Was: a silent `RANGE_MAX` break returning a 10M-element partial.
#[test]
fn c_style_for_counter_limit_matches_the_oracle() {
    let ok =
        evaluate_full("x = [for (i = 0; i < 1000000; i = i + 1) 0]; echo(n = len(x)); cube(1);")
            .expect("exactly the limit is clean");
    assert_eq!(ok.echos(), ["n = 1e+6"]);
    let e = err("x = [for (b = 0; b != 1; b = 0) b]; cube(1);");
    assert!(
        e.to_string().contains("For loop counter exceeded limit"),
        "wanted upstream's verdict, got: {e}"
    );
}

/// AD.2, the shape that rules out arg-cycle detection: `sin(x) = sin()` re-enters with x=undef every
/// call (issue3118-recur-limit.scad — a user function shadowing the builtin, called argless). The call
/// is arity-defaulted (not JIT-eligible) so the error takes the nameless form; still "Recursion detected".
#[test]
fn argless_self_call_through_a_builtin_shadow_is_detected() {
    let e = err("function sin(x) = sin();\necho(sin(30));\ncube(1);");
    assert!(
        e.to_string().contains("Recursion detected"),
        "wanted the recursion verdict, got: {e}"
    );
}
