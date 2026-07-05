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

use fab_lang::{Error, GeoNode, evaluate, evaluate_geometry};

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
        evaluate_geometry("module shifted() translate([5, 0, 0]) cube(1); shifted();").unwrap(),
        GeoNode::Transform { .. }
    ));
    assert!(matches!(
        evaluate_geometry("module two() { cube(1); sphere(1, $fn = 8); } two();").unwrap(),
        GeoNode::Union(c) if c.len() == 2
    ));
    // instantiated twice → two objects → an implicit union at the top level.
    assert!(matches!(
        evaluate_geometry("module u() cube(1); u(); translate([3, 0, 0]) u();").unwrap(),
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
    assert!(!matches!(tree, GeoNode::Empty));
    // rec(0) → the `if` is false → no geometry.
    assert_eq!(
        evaluate_geometry("module rec(n) if (n > 0) cube(1); rec(0);").unwrap(),
        GeoNode::Empty
    );
}

/// A module with NO base case recurses forever — the depth guard bails LOUD instead of crashing the
/// process on a blown host stack (the Safari-cliff doctrine for the statement side).
#[test]
fn runaway_module_recursion_is_loud() {
    let err = evaluate_geometry("module inf() inf(); inf();").unwrap_err();
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
        Error::Unimplemented(m) if m.contains("unknown module")
    ));
}
