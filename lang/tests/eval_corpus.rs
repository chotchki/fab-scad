//! G.3.4 evaluator conformance corpus — OpenSCAD `Value.cc` semantics + the fragment formula.
//!
//! Arithmetic/undef rules are asserted bug-for-bug (dot product, `str+str`→undef, silent-truncate,
//! `fmod`, `pow`, cross-type equality/ordering). The explicit-stack machine is proven on a 100k-deep
//! chain (would overflow a recursive tree-walker). These seed the H.6 fuzz corpus.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration-test helpers: unwrap/expect/panic ARE the assertions; exact float asserts are deterministic values"
)]

use fab_lang::{Error, Scope, StmtKind, Value, eval_expr, eval_program, fragments, parse};

/// Evaluate a bare expression with a default scope.
fn ev(src: &str) -> Value {
    let prog = parse(&format!("v={src};")).expect("parses");
    let StmtKind::Assignment { value, .. } = prog.stmts.into_iter().next().unwrap().kind else {
        panic!("expected an assignment");
    };
    eval_expr(&value, &Scope::new()).expect("evaluates")
}

/// Evaluate expecting an error (a deferred construct).
fn ev_err(src: &str) -> Error {
    let prog = parse(&format!("v={src};")).expect("parses");
    let StmtKind::Assignment { value, .. } = prog.stmts.into_iter().next().unwrap().kind else {
        panic!("expected an assignment");
    };
    eval_expr(&value, &Scope::new()).unwrap_err()
}

fn num(n: f64) -> Value {
    Value::Num(n)
}
fn list(xs: &[f64]) -> Value {
    Value::num_list(xs.to_vec())
}

// ─────────────────────────────── numeric arithmetic ────────────────────────────────────────────

#[test]
fn number_arithmetic() {
    assert_eq!(ev("1+2"), num(3.0));
    assert_eq!(ev("5-3"), num(2.0));
    assert_eq!(ev("2*3"), num(6.0));
    assert_eq!(ev("6/2"), num(3.0));
    assert_eq!(ev("5%3"), num(2.0)); // fmod
    assert_eq!(ev("-5%3"), num(-2.0)); // fmod: sign of dividend
    assert_eq!(ev("2^10"), num(1024.0)); // pow, not xor
    assert_eq!(ev("1/0"), num(f64::INFINITY)); // IEEE
    assert!(matches!(ev("0/0"), Value::Num(n) if n.is_nan())); // IEEE
}

#[test]
fn vector_arithmetic_and_the_dot_product_trap() {
    assert_eq!(ev("[1,2]+[3,4]"), list(&[4.0, 6.0]));
    assert_eq!(ev("[5,5]-[1,2]"), list(&[4.0, 3.0]));
    assert_eq!(ev("[1,2]+[3,4,5]"), list(&[4.0, 6.0])); // silent-truncate to shorter
    assert_eq!(ev("2*[1,2]"), list(&[2.0, 4.0])); // scalar broadcast
    assert_eq!(ev("[1,2]*2"), list(&[2.0, 4.0]));
    assert_eq!(ev("[1,2]*[3,4]"), num(11.0)); // DOT PRODUCT (1*3+2*4), not element-wise
    assert_eq!(ev("[1,2,3,4,5]*[1,1,1,1,1]"), num(15.0)); // 5-element dot: a full 4-lane chunk + 1 tail
    assert_eq!(ev("[6,4]/2"), list(&[3.0, 2.0]));
    assert_eq!(ev("12/[2,3]"), list(&[6.0, 4.0]));
    assert_eq!(ev("[1,2]*[3,4,5]"), Value::Undef); // unequal length → undef, not dot
}

#[test]
fn undef_propagation() {
    assert_eq!(ev("1+undef"), Value::Undef);
    assert_eq!(ev("1-\"a\""), Value::Undef);
    assert_eq!(ev("\"a\"+\"b\""), Value::Undef); // str + str is NOT concat
    assert_eq!(ev("undef*1"), Value::Undef);
    assert_eq!(ev("1/undef"), Value::Undef);
    assert_eq!(ev("5%undef"), Value::Undef);
    assert_eq!(ev("2^undef"), Value::Undef);
    assert_eq!(ev("[]*[]"), Value::Undef); // empty vectors → undef
}

// ─────────────────────────────── comparison + logical ──────────────────────────────────────────

#[test]
fn equality_never_coerces() {
    assert_eq!(ev("1==1"), Value::Bool(true));
    assert_eq!(ev("1==2"), Value::Bool(false));
    assert_eq!(ev("1==true"), Value::Bool(false)); // cross-type → false, no coercion
    assert_eq!(ev("undef==undef"), Value::Bool(true));
    assert_eq!(ev("1!=2"), Value::Bool(true));
    assert_eq!(ev("[1,2]==[1,2]"), Value::Bool(true));
}

#[test]
fn ordering() {
    assert_eq!(ev("1<2"), Value::Bool(true));
    assert_eq!(ev("2<=2"), Value::Bool(true));
    assert_eq!(ev("3>2"), Value::Bool(true));
    assert_eq!(ev("2>=2"), Value::Bool(true));
    assert_eq!(ev("\"a\"<\"b\""), Value::Bool(true)); // string lexicographic
    assert_eq!(ev("[1,2]<[1,3]"), Value::Bool(true)); // list: 1==1, then 2<3
    assert_eq!(ev("[1,3]<[1,2]"), Value::Bool(false)); // list: 1==1, then 3>2
    assert_eq!(ev("[1,2]<[1,2,3]"), Value::Bool(true)); // shorter < longer
    assert_eq!(ev("[1,2]<[1,2]"), Value::Bool(false)); // equal
    assert_eq!(ev("1<\"a\""), Value::Undef); // cross-type ordering → undef
    assert_eq!(ev("(0/0)<1"), Value::Bool(false)); // NaN comparison → false
    assert_eq!(ev("[0/0,1]<[1,2]"), Value::Bool(false)); // NaN in a list → false
    assert_eq!(ev("[true]<[false]"), Value::Bool(false)); // list of NON-orderable (bool) elems → incomparable
}

#[test]
fn logical_and_bitwise() {
    assert_eq!(ev("true&&false"), Value::Bool(false));
    assert_eq!(ev("true||false"), Value::Bool(true));
    assert_eq!(ev("false||true"), Value::Bool(true)); // OR: LHS false → RHS is evaluated
    assert_eq!(ev("5|2"), num(7.0));
    assert_eq!(ev("6&2"), num(2.0));
    assert_eq!(ev("1<<3"), num(8.0));
    assert_eq!(ev("16>>2"), num(4.0));
    assert_eq!(ev("1<<64"), Value::Undef); // >= 64 shift → undef
    assert_eq!(ev("1<<-1"), Value::Undef); // negative shift → undef
    assert_eq!(ev("undef|1"), Value::Undef); // non-number → undef
    assert_eq!(ev("5<<undef"), Value::Undef);
}

#[test]
fn unary() {
    assert_eq!(ev("-5"), num(-5.0));
    assert_eq!(ev("-[1,2]"), list(&[-1.0, -2.0]));
    assert_eq!(ev("-\"a\""), Value::Undef);
    assert_eq!(ev("+5"), num(5.0)); // no-op
    assert_eq!(ev("!true"), Value::Bool(false));
    assert_eq!(ev("!0"), Value::Bool(true)); // 0 is falsy
    assert_eq!(ev("~5"), num(-6.0)); // bitwise not
    assert_eq!(ev("~\"a\""), Value::Undef);
}

// ─────────────────────────────── atoms + control ───────────────────────────────────────────────

#[test]
fn literals_idents_ternary_vectors() {
    assert_eq!(ev("true"), Value::Bool(true));
    assert_eq!(ev("undef"), Value::Undef);
    assert_eq!(ev(r#""hi""#), Value::string("hi"));
    assert_eq!(ev("$fn"), num(0.0)); // resolves the special default
    assert_eq!(ev("nope"), Value::Undef); // unbound → undef
    assert_eq!(ev("true?1:2"), num(1.0));
    assert_eq!(ev("false?1:2"), num(2.0));
    assert_eq!(ev("[1,2,3]"), list(&[1.0, 2.0, 3.0]));
    assert_eq!(ev("[]"), list(&[]));
}

#[test]
fn heterogeneous_lists_and_indexing() {
    // representation: all-number → NumList fast path; anything else → the general List slow path.
    assert!(matches!(ev("[1,2,3]"), Value::NumList(_)));
    assert!(matches!(ev("[1,true]"), Value::List(_)));
    assert!(matches!(ev("[[1,2],[3,4]]"), Value::List(_))); // nested → List (elements are lists)
    assert_eq!(
        ev("[1, true, \"a\"]"),
        Value::list(vec![num(1.0), Value::Bool(true), Value::string("a")])
    );
    // fast == slow: the two representations of the same vector compare EQUAL.
    assert_eq!(ev("[1,2] == [1,2]"), Value::Bool(true));
    assert_eq!(ev("[1,2]"), Value::list(vec![num(1.0), num(2.0)])); // NumList == List, via the custom eq
    assert_ne!(ev("[1,2]"), Value::list(vec![num(1.0)])); // cross-repr, unequal length → not equal
    assert_ne!(ev("[1,2]"), Value::list(vec![num(1.0), num(3.0)])); // cross-repr, same length, one differs
    assert_eq!(ev("[1,[2,3]] == [1,[2,3]]"), Value::Bool(true)); // nested equality
    // indexing (Value.cc operator[]): in-range, out-of-range → undef, fractional truncates, negative → undef.
    assert_eq!(ev("[10,20,30][1]"), num(20.0));
    assert_eq!(ev("[10,20][5]"), Value::Undef);
    assert_eq!(ev("[10,20][1.9]"), num(20.0)); // trunc toward zero
    assert_eq!(ev("[10,20][-1]"), Value::Undef);
    assert_eq!(ev("[1,[2,3]][1]"), list(&[2.0, 3.0])); // nested element
    assert_eq!(ev(r#""abc"[1]"#), Value::string("b")); // string index → the char
    assert_eq!(ev("5[0]"), Value::Undef); // indexing a scalar → undef
    assert_eq!(ev(r#"[1,2]["x"]"#), Value::Undef); // non-number index → undef
    // a non-empty List is truthy; Lists order lexicographically like NumLists (mixed-element too).
    assert_eq!(ev("[1,true] ? 10 : 20"), num(10.0));
    assert_eq!(ev(r#"[1,"a"] < [1,"b"]"#), Value::Bool(true));
}

#[test]
fn ranges_are_first_class_values() {
    // a range is a lazy VALUE (assignable, comparable), NOT materialized into a list.
    assert_eq!(
        ev("[0:5]"),
        Value::Range {
            start: 0.0,
            step: 1.0,
            end: 5.0
        }
    ); // 2-part → default step 1
    assert_eq!(
        ev("[0:2:10]"),
        Value::Range {
            start: 0.0,
            step: 2.0,
            end: 10.0
        }
    ); // 3-part
    assert_eq!(
        ev("[5:-1:0]"),
        Value::Range {
            start: 5.0,
            step: -1.0,
            end: 0.0
        }
    ); // descending
    assert_eq!(ev("[a:b]"), Value::Undef); // non-numeric bounds → undef
    assert_eq!(ev("[0:5] == [0:5]"), Value::Bool(true)); // fieldwise equality
    assert_eq!(ev("[0:5] == [0:1:5]"), Value::Bool(true)); // 2-part == explicit step 1
    assert_eq!(ev("[0:5] == [0:6]"), Value::Bool(false)); // different end
    assert_eq!(ev("[0:5] ? 1 : 2"), num(1.0)); // a range is truthy
    assert_eq!(ev("[0:5]").type_name(), "range");
}

#[test]
fn deferred_constructs_are_loud() {
    assert!(matches!(ev_err("f(1)"), Error::Unimplemented(m) if m.contains("I.4"))); // unknown/builtin fn
    assert!(matches!(ev_err("a.b"), Error::Unimplemented(m) if m.contains("I.1"))); // member access
    // (function literals + calling a function VALUE now evaluate — I.2.3.3; see function_values below.)
    // the remaining H.3 expression forms parse but defer: let → I.3, assert / echo → I.5.
    assert!(matches!(ev_err("let(a=1)a"), Error::Unimplemented(m) if m.contains("I.3")));
    assert!(matches!(ev_err("assert(true)1"), Error::Unimplemented(m) if m.contains("I.5")));
    assert!(matches!(ev_err("echo(1)2"), Error::Unimplemented(m) if m.contains("I.5")));
    // list comprehensions defer to I.3 (control flow).
    assert!(matches!(ev_err("[for(i=[0:3])i]"), Error::Unimplemented(m) if m.contains("I.3")));
    assert!(matches!(ev_err("[each [1]]"), Error::Unimplemented(m) if m.contains("I.3")));
}

#[test]
fn function_values() {
    // `function(params) body` evaluates to a first-class Function value (a closure).
    assert!(matches!(ev("function(x) x"), Value::Function { .. }));
    assert_eq!(ev("function() 1").type_name(), "function");
    assert_eq!(ev("(function() 1) ? 10 : 20"), num(10.0)); // a function value is truthy
    // immediately-invoked: `(expr)(args)` — the dynamic-callee path.
    assert_eq!(ev("(function(x) x + 1)(41)"), num(42.0));
    assert_eq!(ev("(function(x, y) x * y)(6, 7)"), num(42.0));
    assert_eq!(ev("(function(x, y = 10) x + y)(5)"), num(15.0)); // defaults work for closures too
    assert_eq!(ev("(function(x) x)()"), Value::Undef); // unfilled param → undef
    assert_eq!(ev("(function() $fn)($fn = 7)"), num(7.0)); // a $-arg injects into a closure call
    assert_eq!(ev("(function() $fn)()"), num(0.0)); // else the callee sees the reaching $fn (root 0)
}

#[test]
fn program_level_defers_are_loud() {
    // These constructs parse (H.2) but eval defers to a later phase (the tag says which): defs → the
    // I.2 loader/scope engine, if/else → I.3 control flow.
    // (user FUNCTION defs now evaluate — I.2.3.2; their calls are covered by the eval/mod.rs unit tests.)
    for (src, phase) in [
        ("module m() cube(1);", "I.2.4"),
        ("if (true) cube(1);", "I.3"),
        ("use <lib.scad>", "I.2"),
        ("include <lib.scad>", "I.2"),
        ("x = a.b;", "I.1"), // an erroring assignment RHS propagates out of eval_stmt
        ("{ y = a.b; }", "I.1"), // …and out of a block's inner statement
    ] {
        let prog = parse(src).expect("parses");
        let err = eval_program(&prog, &Scope::new()).unwrap_err();
        assert!(
            matches!(&err, Error::Unimplemented(m) if m.contains(phase)),
            "expected Unimplemented({phase}) for {src:?}, got {err:?}"
        );
    }
}

// ─────────────────────────────── the explicit stack ────────────────────────────────────────────

#[test]
fn explicit_stack_evaluates_deep_chains_without_overflow() {
    // A 100k-deep left-spine would blow a recursive tree-walker; the explicit stack grows the HEAP.
    let chain = format!("{}1", "1+".repeat(100_000));
    assert_eq!(ev(&chain), num(100_001.0));
}

// ─────────────────────────────── scope + $fn/$fa/$fs ────────────────────────────────────────────

#[test]
fn scope_defaults_lookup_bind() {
    let mut s = Scope::new();
    assert_eq!(s.fn_fa_fs(), (0.0, 12.0, 2.0)); // Builtins defaults
    assert_eq!(s.lookup("$fn"), num(0.0));
    assert_eq!(s.lookup("unbound"), Value::Undef);
    s.bind("x", num(7.0));
    assert_eq!(s.lookup("x"), num(7.0));
    s.bind("$fn", Value::string("oops")); // non-number $-var → 0.0 via toDouble
    assert_eq!(s.fn_fa_fs().0, 0.0);
    assert_eq!(Scope::default().fn_fa_fs(), (0.0, 12.0, 2.0));
}

// ─────────────────────────────── the fragment formula ──────────────────────────────────────────

#[test]
fn fragment_formula() {
    assert_eq!(fragments(5.0, 8.0, 12.0, 2.0), 8); // $fn > 0 → $fn (integer)
    assert_eq!(fragments(5.0, 6.5, 12.0, 2.0), 7); // $fn > 0 non-integer → ceil (nightly/master)
    assert_eq!(fragments(5.0, 2.0, 12.0, 2.0), 3); // $fn > 0 but < 3 → clamp to 3
    assert_eq!(fragments(1.0, 0.0, 12.0, 2.0), 5); // $fn=0: min(30, π)=3.14, floor 5 → 5
    assert_eq!(fragments(5.0, 0.0, 12.0, 2.0), 16); // min(30, 5π=15.7) → ceil 16
    assert_eq!(fragments(100.0, 0.0, 12.0, 2.0), 30); // $fa caps it: 360/12 = 30
    assert_eq!(fragments(5.0, -1.0, 12.0, 2.0), 16); // $fn < 0 → 0 → $fa/$fs branch
    assert_eq!(fragments(0.0, 0.0, 12.0, 2.0), 3); // r < GRID_FINE → 3
    assert_eq!(fragments(f64::NAN, 0.0, 12.0, 2.0), 3); // non-finite r → 3
    assert_eq!(fragments(5.0, f64::INFINITY, 12.0, 2.0), 3); // non-finite $fn → 3
    assert!(fragments(5.0, 0.0, 0.0, 0.0) > 30); // $fa/$fs floored at 0.01 → large count
}

// ─────────────────────────────── value model + determinism ─────────────────────────────────────

#[test]
fn value_truthiness_and_type_name() {
    assert!(!Value::Undef.is_truthy());
    assert!(Value::Bool(true).is_truthy());
    assert!(!Value::Num(0.0).is_truthy());
    assert!(Value::Num(f64::NAN).is_truthy()); // NaN != 0 → truthy
    assert!(!Value::string("").is_truthy());
    assert!(Value::string("x").is_truthy());
    assert!(!list(&[]).is_truthy());
    assert!(list(&[0.0]).is_truthy()); // non-empty (even [0]) is truthy
    for (v, name) in [
        (Value::Undef, "undef"),
        (Value::Bool(true), "bool"),
        (num(1.0), "number"),
        (Value::string("s"), "string"),
        (list(&[1.0]), "list"),
    ] {
        assert_eq!(v.type_name(), name);
        assert_eq!(v.clone(), v); // Clone + PartialEq
        assert!(!format!("{v:?}").is_empty()); // Debug
    }
}

#[test]
fn evaluation_is_deterministic() {
    let expr = "1 + 2*3 - [1,2]*[3,4] + (true ? 10 : 20)";
    assert_eq!(ev(expr), ev(expr));
}
