//! I.2.4 — user MODULE calls: definition + instantiation → geometry. A module DEF is a no-op at eval;
//! a CALL resolves in the module store, binds args (positional / named / default / `$`-args) into a
//! child of the GLOBAL scope (OpenSCAD hygiene), and evaluates the body there. Recursion is host-stack
//! bound, so a depth guard keeps a runaway module LOUD, never a silent crash. (`children()` is I.2.5.)

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration test: unwrap/panic ARE the assertions; the extents are EXACT (literal cube args, no trig)"
)]

use fab_lang::{Error, Geo, GeoNode, evaluate, evaluate_geometry};

/// The max-x extent of a single-primitive program's mesh — a `cube(s)` (uncentered) spans `x ∈ [0, s]`,
/// so this reads back `s`, letting a test prove which arg value actually reached the body.
fn extent(src: &str) -> f64 {
    evaluate(src)
        .unwrap()
        .verts
        .iter()
        .map(|v| v.x)
        .fold(f64::MIN, f64::max)
}

/// Unwrap a 3D geometry result to its [`GeoNode`] — every module here builds 3D geometry, so the
/// `Geo::D3` dimension tag is just noise to strip before matching on the tree shape.
fn d3(g: Geo) -> GeoNode {
    match g {
        Geo::D3(node) => node,
        Geo::D2(shape) => panic!("expected a 3D result, got 2D: {shape:?}"),
    }
}

/// A module wrapping a single primitive flattens to that primitive's mesh (no backend needed).
#[test]
fn module_call_binds_args() {
    // no args: the body's geometry, verbatim.
    assert!(
        evaluate("module unit() cube(1); unit();")
            .unwrap()
            .tri_count()
            > 0
    );

    // positional, named, and default args all reach the body's `size`.
    let positional = extent("module box(s) cube(s); box(4);");
    assert_eq!(positional, 4.0);
    assert_eq!(positional, extent("module box(s) cube(s); box(s = 4);")); // named == positional
    assert_eq!(positional, extent("module box(s = 4) cube(s); box();")); // default == positional
    // a different arg → a different box (the arg genuinely drives the body).
    assert_eq!(extent("module box(s) cube(s); box(2);"), 2.0);
    // an unfilled, defaultless param binds to undef — identical to passing `undef` explicitly.
    assert_eq!(
        evaluate("module m(x) cube(x); m();").unwrap(),
        evaluate("module m(x) cube(x); m(undef);").unwrap()
    );
}

/// A module body can be a transform, a boolean, or a block of several children — the full statement
/// vocabulary, producing real internal tree nodes.
#[test]
fn module_body_is_full_geometry() {
    assert!(matches!(
        d3(evaluate_geometry("module shifted() translate([5, 0, 0]) cube(1); shifted();").unwrap()),
        GeoNode::Transform { .. }
    ));
    assert!(matches!(
        d3(evaluate_geometry("module two() { cube(1); sphere(1, $fn = 8); } two();").unwrap()),
        GeoNode::Union(c) if c.len() == 2
    ));
    // instantiated twice → two objects → an implicit union at the top level.
    assert!(matches!(
        d3(evaluate_geometry("module u() cube(1); u(); translate([3, 0, 0]) u();").unwrap()),
        GeoNode::Union(c) if c.len() == 2
    ));
}

/// A module body sees GLOBALS + its params (OpenSCAD lexical hygiene), so a top-level variable reaches
/// in. (The negative — a module does NOT see a caller's locals — rides the global-child base by
/// construction: the body evaluates in `global.child()`, never the caller's frame.)
#[test]
fn module_sees_globals() {
    assert_eq!(extent("s = 6; module box() cube(s); box();"), 6.0);
}

/// A `$`-arg on the call injects a dynamic override the body sees ($fn drives the sphere's facets).
#[test]
fn module_dollar_arg_reaches_the_body() {
    let coarse = evaluate("module ball() sphere(10); ball($fn = 8);")
        .unwrap()
        .vert_count();
    let fine = evaluate("module ball() sphere(10); ball($fn = 32);")
        .unwrap()
        .vert_count();
    assert!(
        fine > coarse,
        "$fn override reaches the module body: {coarse} vs {fine}"
    );
}

/// A self-recursive module that TERMINATES (bounded by its own condition) evaluates fine — the geometry
/// tree unwinds without host-stack overflow because each level is a bounded body.
#[test]
fn recursive_module_terminates() {
    // rec(3) → three nested translated cubes → an implicit union somewhere down the chain; rec(0) stops.
    let tree = evaluate_geometry(
        "module rec(n) if (n > 0) { cube(1); translate([2, 0, 0]) rec(n - 1); } rec(3);",
    )
    .unwrap();
    // 3 cubes get produced (n = 3, 2, 1); the exact nesting is union/transform, just assert it built.
    assert!(!matches!(d3(tree), GeoNode::Empty));
    // rec(0) → the `if` is false → no geometry.
    assert_eq!(
        d3(evaluate_geometry("module rec(n) if (n > 0) cube(1); rec(0);").unwrap()),
        GeoNode::Empty
    );
}

/// A module with NO base case recurses forever — the depth guard bails LOUD instead of crashing the
/// process on a blown host stack (the Safari-cliff doctrine for the statement side).
///
/// Reaching the guard means `MAX_MODULE_DEPTH` (256) host-recursive statement frames on the stack. Release
/// frames are small enough that 256 fit on any real stack (the guard fires first), but cargo-llvm-cov's
/// INSTRUMENTED frames are fat enough to blow a default 2MB test thread before depth 256 — so run the
/// eval on a thread with headroom. This is a coverage-build accommodation, not a production limit.
#[test]
fn runaway_module_recursion_is_loud() {
    let err = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| evaluate_geometry("module inf() inf(); inf();").unwrap_err())
        .unwrap()
        .join()
        .unwrap();
    assert!(
        matches!(&err, Error::Unimplemented(m) if m.contains("recursion too deep")),
        "expected a LOUD depth-guard error, got {err:?}"
    );
}

/// An UNKNOWN module (typo, or a builtin still deferred) is LOUD — never silently nothing.
#[test]
fn unknown_module_is_loud() {
    assert!(matches!(
        evaluate_geometry("not_a_module();").unwrap_err(),
        Error::Unknown(m) if m.contains("module `not_a_module`")
    ));
}

/// `let(...) children` as a STATEMENT (I.9.6): binds vars for its children SEQUENTIALLY (a later binding
/// sees the earlier ones), including the `$`-context BOSL2's `attachable` sets on the geometry it wraps.
/// No geometry of its own — the statement sibling of the `let` EXPRESSION, a pure scope wrapper.
#[test]
fn statement_let_binds_children() {
    // the bound var reaches the child primitive
    assert_eq!(extent("let(a = 3) cube(a);"), 3.0);
    // SEQUENTIAL binding: `b` sees `a` (bindings resolve left-to-right in the growing scope)
    assert_eq!(extent("let(a = 2, b = a + 1) cube(b);"), 3.0);
    // a `$`-binding reaches the geometry exactly like passing it as an arg — attachable's whole trick
    assert_eq!(
        evaluate("let($fn = 8) sphere(10);").unwrap(),
        evaluate("sphere(10, $fn = 8);").unwrap()
    );
    // it wraps MULTIPLE children (a block), all under the bindings → a union
    assert!(matches!(
        d3(evaluate_geometry("let(a = 1) { cube(a); translate([2, 0, 0]) cube(a); }").unwrap()),
        GeoNode::Union(_)
    ));
}

/// An error inside a `for` body doesn't get swallowed by the iteration — it propagates LOUD out of the
/// loop (the `for_product` `?`), same as anywhere else. A single deferred/unknown child kills the render.
#[test]
fn for_body_error_propagates() {
    assert!(matches!(
        evaluate_geometry("for (i = [0, 1]) not_a_module();").unwrap_err(),
        Error::Unknown(m) if m.contains("module `not_a_module`")
    ));
}

/// A statement-level `$special = value;` assignment parses AND scopes (BOSL2 leans on it heavily —
/// `$fn=8;`, `$tags=…;`, `$color=…;`). `$fn = 8` reaches the geometry exactly like passing it as an arg.
#[test]
fn special_variable_assignment_scopes() {
    assert_eq!(
        evaluate("$fn = 8; sphere(10);").unwrap().vert_count(),
        evaluate("sphere(10, $fn = 8);").unwrap().vert_count()
    );
    // inside a module body it rides the call scope + reaches the module's own geometry.
    assert_eq!(
        evaluate("module ball() { $fn = 8; sphere(10); } ball();")
            .unwrap()
            .vert_count(),
        evaluate("sphere(10, $fn = 8);").unwrap().vert_count()
    );
}

// ───────────────────────────── children() / $children (I.2.5) ─────────────────────────────

/// A wrapper module renders its call-site children via `children()` — the BOSL2 currency (a module
/// that transforms / recolors / arrays whatever it's given).
#[test]
fn children_renders_call_site_children() {
    // `children()` wrapped in a transform → the child, transformed.
    assert!(matches!(
        d3(evaluate_geometry("module m() translate([5, 0, 0]) children(); m() cube(1);").unwrap()),
        GeoNode::Transform { .. }
    ));
    // several children → their union.
    assert!(matches!(
        d3(evaluate_geometry("module m() children(); m() { cube(1); translate([5, 0, 0]) cube(1); }")
            .unwrap()),
        GeoNode::Union(c) if c.len() == 2
    ));
    // `children()` OUTSIDE any module call → nothing.
    assert_eq!(
        d3(evaluate_geometry("children();").unwrap()),
        GeoNode::Empty
    );
}

/// `children(i)` picks the i-th call-site child; `$children` is their count.
#[test]
fn children_index_and_count() {
    let verts = |src: &str| evaluate(src).unwrap().vert_count();
    let (cube, sphere) = (verts("cube(1);"), verts("sphere(2, $fn = 8);"));
    assert_ne!(cube, sphere); // distinguishable
    // child 0 is the cube, child 1 the sphere.
    assert_eq!(
        verts("module pick() children(0); pick() { cube(1); sphere(2, $fn = 8); }"),
        cube
    );
    assert_eq!(
        verts("module pick() children(1); pick() { cube(1); sphere(2, $fn = 8); }"),
        sphere
    );
    // an out-of-range index → nothing.
    assert_eq!(
        d3(evaluate_geometry("module pick() children(9); pick() cube(1);").unwrap()),
        GeoNode::Empty
    );
    // children([indices]) → those children (out-of-range drop).
    assert!(matches!(
        d3(evaluate_geometry(
            "module pick() children([0, 2]); pick() { cube(1); sphere(1, $fn = 8); cube(2); }"
        )
        .unwrap()),
        GeoNode::Union(c) if c.len() == 2
    ));
    // a non-index arg (a string) → nothing.
    assert_eq!(
        d3(evaluate_geometry("module pick() children(\"x\"); pick() cube(1);").unwrap()),
        GeoNode::Empty
    );
    // $children is the count — a module can gate on it.
    assert!(matches!(
        d3(evaluate_geometry(
            "module g() if ($children == 2) cube(1); g() { sphere(1, $fn = 8); sphere(1, $fn = 8); }"
        )
        .unwrap()),
        GeoNode::Leaf(_)
    ));
    assert_eq!(
        d3(
            evaluate_geometry("module g() if ($children == 2) cube(1); g() sphere(1, $fn = 8);")
                .unwrap()
        ),
        GeoNode::Empty
    );
    // A lone `;` (an EMPTY statement) is NOT a child — it neither counts toward `$children` nor is
    // reachable via `children(i)` (oracle-verified: `union(){}; union(){};` → $children == 2, not 4).
    // This is what BOSL2's `attachable(){ shape; union(){}; }` relies on: the terminating `;` after the
    // empty-union attachments placeholder must NOT read as a third child (else attachable's `$children==2`
    // assert fails — the whole screw() family). The count and `children(i)` both skip empties.
    assert!(matches!(
        d3(evaluate_geometry(
            "module g() if ($children == 2) cube(1); g() { sphere(1, $fn = 8); union(){}; }"
        )
        .unwrap()),
        GeoNode::Leaf(_) // the empty `union(){}` + its terminating `;` is ONE child, so $children == 2
    ));
    // `children(1)` skips the lone `;` at index 1 → picks the sphere, not nothing.
    assert_eq!(
        verts("module pick() children(1); pick() { cube(1); ; sphere(2, $fn = 8); }"),
        sphere
    );
}

/// `children()` LATE-binds: a `children()` inside the rendered children refers to the ENCLOSING call,
/// not the current one — so a wrapper-of-a-wrapper passes the outer children all the way through.
#[test]
fn children_late_binds_through_nesting() {
    // a() calls b() with `children()` as b's child; b() renders it → a()'s child (the cube).
    assert_eq!(
        evaluate("module a() b() children(); module b() children(); a() cube(1);").unwrap(),
        evaluate("cube(1);").unwrap()
    );
}
