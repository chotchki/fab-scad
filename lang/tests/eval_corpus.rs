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
/// Assert a value is a number within 1e-9 of `expected` (for transcendental builtins).
fn approx(v: Value, expected: f64) {
    match v {
        Value::Num(n) => assert!((n - expected).abs() < 1e-9, "{n} != {expected}"),
        other => panic!("expected a number, got {other:?}"),
    }
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
    // `+`/`-` are element-wise regardless of NESTING — a MATRIX recurses per row down to the NumList
    // kernel (I.9.3; BOSL2's `sum()` over a list of vectors/matrices rides this). Result is List-of-rows.
    let mat = |rows: &[&[f64]]| {
        Value::list(
            rows.iter()
                .map(|r| Value::num_list(r.to_vec()))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        ev("[[1,2],[3,4]]+[[5,6],[7,8]]"),
        mat(&[&[6.0, 8.0], &[10.0, 12.0]])
    );
    assert_eq!(
        ev("[[1,2],[3,4]]-[[1,1],[1,1]]"),
        mat(&[&[0.0, 1.0], &[2.0, 3.0]])
    );
    assert_eq!(ev("[[1,2],[3,4]]+[[5,6]]"), mat(&[&[6.0, 8.0]])); // truncates to the shorter matrix
    assert_eq!(ev("1+[[1,2],[3,4]]"), Value::Undef); // scalar + matrix is not a vector op → undef
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
    // Bools ARE orderable (false < true, coerced 0/1) — BOSL2's `compare_vals(true,false)>0` needs it.
    assert_eq!(ev("false<true"), Value::Bool(true));
    assert_eq!(ev("true>false"), Value::Bool(true));
    assert_eq!(ev("true<true"), Value::Bool(false));
    assert_eq!(ev("false>=false"), Value::Bool(true));
    assert_eq!(ev("true<1"), Value::Undef); // but bool-vs-num is still CROSS-type → undef
    assert_eq!(ev("[true]<[false]"), Value::Bool(false)); // element-wise: true>false, so [true]>[false]
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
fn logical_ops_short_circuit() {
    // `&&`/`||` SHORT-CIRCUIT (OpenSCAD semantics) — the RHS runs ONLY when the LHS doesn't decide it.
    // BOSL2 leans on this HARD: it guards assertions + recursion base-cases behind `a || b` / `a && b`,
    // so eager evaluation makes guarded asserts fire (`is_undef(x) || assert(is_num(x))`) and guarded
    // recursion never terminate (`n<=0 || f(n-1)`). A skipped RHS's `assert(false)` proves it's untouched.
    // (the guarded assert is PARENTHESIZED — `assert`/`let`/`echo` aren't bare binary operands in
    // OpenSCAD's grammar, so BOSL2 always wraps them: `is_undef(x) || (assert(is_num(x)) …)`.)
    assert_eq!(ev("true || (assert(false) false)"), Value::Bool(true)); // `||` truthy LHS → RHS skipped
    assert_eq!(ev("1 || (assert(false) 0)"), Value::Bool(true)); // truthy NON-bool LHS short-circuits too
    assert_eq!(ev("false && (assert(false) true)"), Value::Bool(false)); // `&&` falsy LHS → RHS skipped
    assert_eq!(ev("0 && (assert(false) 1)"), Value::Bool(false));
    // when NOT short-circuited the RHS runs, and the result is its truthiness as a Bool (OpenSCAD).
    assert_eq!(ev("false || 5"), Value::Bool(true));
    assert_eq!(ev("true && 5"), Value::Bool(true));
    assert_eq!(ev("true && false"), Value::Bool(false));
}

#[test]
fn guarded_recursion_terminates_via_short_circuit() {
    // `g(n) = n<=0 || g(n-1)` — WITHOUT short-circuit the guarded recursive call runs unconditionally and
    // never stops (the BOSL2 eval hang). WITH it, `g(0)` short-circuits at `0<=0` → true, and it unwinds.
    let prog = parse("v = g(500); function g(n) = n<=0 || g(n-1);").expect("parses");
    let mesh = eval_program(&prog, &Scope::new()).expect("terminates");
    assert_eq!(mesh.tri_count(), 0); // an assignment → no geometry; the point is it RETURNS
}

#[test]
fn unary() {
    assert_eq!(ev("-5"), num(-5.0));
    assert_eq!(ev("-[1,2]"), list(&[-1.0, -2.0]));
    assert_eq!(ev("-\"a\""), Value::Undef);
    // Unary minus recurses into NESTED lists (L.2.8d) — `-matrix` negates element-wise (OpenSCAD), not
    // undef. Regression for BOSL2's rot_inverse: `-rot(90)` fed a downstream `hstack`, and undef there
    // collapsed the inverse to a malformed 3-column matrix, tripping rot_resample's `is_matrix` assert.
    assert_eq!(ev("-[[1,2],[3,4]]"), ev("[[-1,-2],[-3,-4]]"));
    assert_eq!(ev("-[1,\"a\"]"), ev("[-1,undef]")); // non-numeric leaf → undef, matching -\"a\"
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
    assert_eq!(ev("PI"), num(std::f64::consts::PI)); // OpenSCAD's one builtin math constant
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
    // A range is self-equal STRUCTURALLY: a NaN step doesn't make it differ from itself (unlike a list,
    // where `[NaN] != [NaN]` IEEE). BOSL2's `typeof([0:NAN:INF])=="invalid"` needs `is_nan(r)=(r!=r)` false.
    assert_eq!(ev("[0:0/0:1/0] == [0:0/0:1/0]"), Value::Bool(true)); // NaN step, Inf end → still self-equal
    assert_eq!(ev("[0:0/0:1/0] != [0:0/0:1/0]"), Value::Bool(false));
    assert_eq!(ev("[0:5] ? 1 : 2"), num(1.0)); // a range is truthy
    assert_eq!(ev("[0:5]").type_name(), "range");
    // INDEXING a range → its three fields: `r[0]=start`, `r[1]=step`, `r[2]=end`, else undef (OpenSCAD
    // `RangeType`, verified vs oracle). BOSL2's `is_range`/`typeof` rely on `is_finite(x[0..2])`.
    assert_eq!(ev("[10:2:20][0]"), num(10.0));
    assert_eq!(ev("[10:2:20][1]"), num(2.0));
    assert_eq!(ev("[10:2:20][2]"), num(20.0));
    assert_eq!(ev("[10:2:20][3]"), Value::Undef);
    assert_eq!(ev("[1:3][1]"), num(1.0)); // implicit step 1
}

#[test]
fn an_unknown_function_warns_and_undefs() {
    // L.5.7: a missing/typo'd function is NOT loud — OpenSCAD warns "Ignoring unknown function 'f'"
    // and the call evaluates to undef, so a corpus that names a newer-BOSL2 function still renders the
    // REST instead of hard-failing. (function literals + calling a function VALUE evaluate — I.2.3.3;
    // let → I.3.1; comprehensions → I.3.2; assert / echo → I.5; member access → I.9.1 below.)
    assert_eq!(ev("f(1)"), Value::Undef);
    let full = fab_lang::evaluate_full("v = f(1);").expect("warn-and-continue");
    assert_eq!(full.warnings(), ["Ignoring unknown function 'f'"]);
}

#[test]
fn matrix_and_vector_multiplication() {
    // OpenSCAD's `*` on lists is full linear algebra — the machinery BOSL2's affine transforms + is_matrix
    // live on (I.9.2). A matrix is a List-of-NumList; `mat` builds one from rows.
    let mat = |rows: &[&[f64]]| {
        Value::list(
            rows.iter()
                .map(|r| Value::num_list(r.to_vec()))
                .collect::<Vec<_>>(),
        )
    };
    // scalar × a NESTED list broadcasts recursively — the `0*matrix` that `is_consistent`/`is_matrix` need.
    assert_eq!(ev("0*[[1,0],[0,1]]"), mat(&[&[0.0, 0.0], &[0.0, 0.0]]));
    assert_eq!(ev("[[2,4],[6,8]] / 2"), mat(&[&[1.0, 2.0], &[3.0, 4.0]])); // nested ÷ scalar
    assert_eq!(ev("2/[[1,2],[4,8]]"), mat(&[&[2.0, 1.0], &[0.5, 0.25]])); // scalar ÷ nested list
    // matrix × matrix, matrix × vector, vector × matrix
    assert_eq!(
        ev("[[1,0],[0,1]] * [[2,0],[0,3]]"),
        mat(&[&[2.0, 0.0], &[0.0, 3.0]])
    );
    assert_eq!(ev("[[1,2],[3,4]] * [1,1]"), list(&[3.0, 7.0])); // matrix × vector
    assert_eq!(ev("[1,1] * [[1,2],[3,4]]"), list(&[4.0, 6.0])); // vector × matrix
    assert_eq!(ev("[[1,0,0,0],[0,1,0,5]] * [1,2,3,1]"), list(&[1.0, 7.0])); // affine row·vec
    // dimension / rectangularity guards → undef (OpenSCAD warns + returns undef)
    assert_eq!(ev("[[1,2]] * [1,2,3]"), Value::Undef); // matrix cols 2 ≠ vector length 3
    assert_eq!(ev("[[1,2],[3]] * [1,1]"), Value::Undef); // non-rectangular matrix
    assert_eq!(ev("[[1,2],[3,4]] * [[1,2,3]]"), Value::Undef); // left cols 2 ≠ right rows 1
    assert_eq!(ev("[1,1,1] * [[1,2],[3,4]]"), Value::Undef); // vector length 3 ≠ matrix row count 2
    // a non-numeric row anywhere → undef, at each product's rectangularity guard:
    assert_eq!(ev(r#"[[1,2],"x"] * [1,1]"#), Value::Undef); // matrix × vector, bad row
    assert_eq!(ev(r#"[1,1] * [[1,2],"x"]"#), Value::Undef); // vector × matrix, bad row
    assert_eq!(ev("[1,1] * [[1,2],[3,4,5]]"), Value::Undef); // vector × matrix, ragged rows
    assert_eq!(ev(r#"[[1,2],"x"] * [[1,2],[3,4]]"#), Value::Undef); // matrix × matrix, bad left row
}

#[test]
fn member_access_reads_vector_components() {
    // `.x`/`.y`/`.z` → index 0/1/2 (OpenSCAD's named components; BOSL2 uses them everywhere) — I.9.1.
    assert_eq!(ev("[10,20,30].x"), num(10.0));
    assert_eq!(ev("[10,20,30].y"), num(20.0));
    assert_eq!(ev("[10,20,30].z"), num(30.0));
    assert_eq!(ev("[1,2].z"), Value::Undef); // out of range → undef
    assert_eq!(ev("a.x"), Value::Undef); // an unbound base → undef (not an error)
    assert_eq!(ev("[1,2,3].w"), Value::Undef); // any other member name → undef
    assert_eq!(ev("[[1,2],[3,4]].y.x"), num(3.0)); // chains: second element, then its x
}

#[test]
fn echo_and_assert_evaluate() {
    // I.5: assert passes through its trailing value (a falsy condition is LOUD); echo emits an ECHO
    // line then passes through. Expression forms via the ev helper:
    assert_eq!(ev("assert(true) 1"), num(1.0));
    assert_eq!(ev("echo(1) 2"), num(2.0));
    assert_eq!(ev("echo(1)"), Value::Undef); // no trailing body → undef
    assert_eq!(ev("assert(true)"), Value::Undef);
    // A falsy assert raises Error::Assert (L.5.8: a DISTINCT variant from Error::Eval, so the top-level
    // geometry driver can catch it → warn + keep the pre-assert partial rather than hard-fail).
    assert!(matches!(ev_err("assert(false)"), Error::Assert(_)));
    // assert arg forms: named condition/message, an unknown named (dropped), a non-string message.
    assert!(matches!(
        ev_err("assert(condition = false, message = \"m\")"),
        Error::Assert(m) if m.contains('m')
    ));
    assert!(matches!(ev_err("assert(false, foo = 1)"), Error::Assert(_))); // unknown named dropped
    assert!(matches!(
        ev_err("assert(false, 42)"),
        Error::Assert(m) if m.contains("42") // non-string message
    ));
    // Echo OUTPUT via the program path — evaluate_full captures the ordered message log; numbers are
    // formatted bug-for-bug (0.333333), strings quoted, named args as `a = 5`.
    let full =
        fab_lang::evaluate_full("echo(9); echo(1 / 3); echo(\"hi\", a = 5);").expect("evaluates");
    assert_eq!(full.echos(), ["9", "0.333333", "\"hi\", a = 5"]);
    assert!(full.warnings().is_empty());
    // An UNKNOWN variable warns bug-for-bug with OpenSCAD ("Ignoring unknown variable 'x'"); an explicit
    // `x = undef` is BOUND, so it stays silent; a `$`-special stays silent too (dynamically scoped).
    let w = fab_lang::evaluate_full("echo(nope); u = undef; echo(u); echo($nope); cube(1);")
        .expect("evaluates");
    assert_eq!(w.warnings(), ["Ignoring unknown variable 'nope'"]);
    assert_eq!(w.console()[0], "WARNING: Ignoring unknown variable 'nope'"); // full rendered line
    // A top-level assert/echo is NOT geometry. A falsy top-level assert is LOUD in the CONSOLE but
    // NON-fatal (L.5.8): OpenSCAD warns + exports the geometry built BEFORE the assert, so `evaluate`
    // returns Ok with the pre-assert partial (here the leading cube; the post-assert one never runs).
    assert!(fab_lang::evaluate("assert(true); sphere(1, $fn = 8);").is_ok());
    assert_eq!(
        fab_lang::evaluate("cube(2); assert(1 == 2, \"nope\"); cube(1);")
            .expect("a failed top-level assert exports the pre-assert geometry, not an Err")
            .tri_count(),
        12 // just the leading cube; the assert halts before the second
    );
    let loud =
        fab_lang::evaluate_full("cube(2); assert(1 == 2, \"nope\"); cube(1);").expect("renders");
    assert_eq!(
        loud.warnings(),
        ["assertion failed: nope [assert((1 == 2))]"]
    );
}

#[test]
fn comprehension_frame_reuse_preserves_closure_capture() {
    // N.2: `lc_for` REUSES one child frame across iterations (bind is `Rc::make_mut`) rather than allocating a
    // fresh `Rc<Frame>` each iteration — a ~15% real-model win. The invariant that keeps it SOUND: a body that
    // CAPTURES the loop-var frame (a closure, a nested comprehension) must still see ITS OWN iteration's value,
    // because `make_mut` clones the frame the instant it's shared. If a future change reused the frame
    // UNCONDITIONALLY, these would collapse to the last value ([3,3,3,3]) — this is the guard.
    // Each closure captures its own `i`:
    assert!(
        fab_lang::evaluate(
            "funcs = [for(i = [0:3]) function() i]; \
             assert([for(f = funcs) f()] == [0, 1, 2, 3]); cube(1);"
        )
        .is_ok(),
        "closures in a comprehension must each keep their own loop-var value"
    );
    // A captured closure called AFTER the loop, with its OWN arg — the frame was cloned at capture, not reused:
    assert!(
        fab_lang::evaluate(
            "fs = [for(i = [1:3]) function(x) x + i]; \
             assert(fs[0](10) == 11 && fs[1](10) == 12 && fs[2](10) == 13); cube(1);"
        )
        .is_ok(),
        "a stored closure must hold its capture-time loop-var value"
    );
    // Nested comprehension — the inner loop's frame reuse must not corrupt the outer loop var:
    assert!(
        fab_lang::evaluate(
            "assert([for(i = [0:2]) [for(j = [0:2]) i * 10 + j]] == [[0,1,2],[10,11,12],[20,21,22]]); cube(1);"
        )
        .is_ok(),
        "nested comprehension: inner frame reuse must not corrupt the outer loop var"
    );
}

#[test]
fn a_top_level_constant_can_call_a_function_that_reads_another_global() {
    // Island-global bootstrapping (L.2.8a). A top-level constant whose RHS CALLS a function must let
    // that function resolve the OTHER top-level constants — DURING the hoist that builds them. The
    // function body's lexical base is its home island's global (use-scope hygiene); for the root that
    // global is what this very hoist produces, so it has to be published incrementally as it grows.
    // Without it `E` is invisible to `f` mid-hoist → `x` reads undef (+ an "unknown variable" warning).
    // This is the BOSL2 modular_hose cluster in miniature: `_modhose = [turtle([arc...])]` where BOSL2's
    // `arc` reads the library constant `_EPSILON` — undef made turtle's arc assert, blocking the load.
    let full = fab_lang::evaluate_full("E = 0.001;\nfunction f() = E;\nx = f();\necho(x);\n")
        .expect("evaluates");
    assert_eq!(full.echos(), ["0.001"]);
    assert!(
        full.warnings().is_empty(),
        "no unknown-variable warning: {:?}",
        full.warnings()
    );
}

#[test]
fn list_comprehensions() {
    assert_eq!(ev("[for(i = [0:3]) i]"), list(&[0.0, 1.0, 2.0, 3.0])); // for over a range
    assert_eq!(ev("[for(i = [10, 20, 30]) i]"), list(&[10.0, 20.0, 30.0])); // for over a list
    assert_eq!(ev("[for(i = [1:3]) i * i]"), list(&[1.0, 4.0, 9.0])); // body is an expression
    assert_eq!(ev("[1, for(i = [2:3]) i, 4]"), list(&[1.0, 2.0, 3.0, 4.0])); // spliced among plain elems
    assert_eq!(ev("[each [1, 2, 3]]"), list(&[1.0, 2.0, 3.0])); // each splices
    assert_eq!(ev("[each 5]"), list(&[5.0])); // each on a scalar → one element
    assert_eq!(ev("[for(i = [1:4]) if (i % 2 == 0) i]"), list(&[2.0, 4.0])); // if filters
    assert_eq!(ev("[if (true) 1 else 2]"), list(&[1.0])); // top-level if/else element
    assert_eq!(ev("[if (false) 1]"), list(&[])); // if with no else, false → nothing
    assert_eq!(
        ev("[for(i = [1:2]) for(j = [1:2]) i * 10 + j]"),
        list(&[11.0, 12.0, 21.0, 22.0])
    ); // nested
    assert_eq!(
        ev("[for(i = [1:2], j = [1:2]) i * 10 + j]"),
        list(&[11.0, 12.0, 21.0, 22.0])
    ); // multi-binding
    assert_eq!(
        ev("[for(i = [1:2]) [i, i]]"),
        Value::list(vec![list(&[1.0, 1.0]), list(&[2.0, 2.0])])
    ); // list body → nested
    assert_eq!(ev("[let(a = 5) for(i = [1:2]) i + a]"), list(&[6.0, 7.0])); // comprehension let
    assert_eq!(
        ev("[for(i = 0; i < 3; i = i + 1) i]"),
        list(&[0.0, 1.0, 2.0])
    ); // C-style for
    assert_eq!(ev("[let(a = 5) a]"), list(&[5.0])); // a let with a plain (scalar) body → one element
    assert_eq!(
        ev("[for(i = [[1], [2]]) i]"),
        Value::list(vec![list(&[1.0]), list(&[2.0])])
    ); // iterate a heterogeneous List
    assert_eq!(
        ev(r#"[each "ab"]"#),
        Value::list(vec![Value::string("a"), Value::string("b")])
    ); // each a string → its chars
    assert_eq!(ev("[if (false) 1 else 2]"), list(&[2.0])); // if/else, the ELSE taken
    assert_eq!(
        ev("[for(i = [1:2]) let(a = i) a * 10]"),
        list(&[10.0, 20.0])
    ); // a let NESTED in a for body
    assert_eq!(
        ev("[for(i = 0; i < 2; i = i + 1, k = i) i]"),
        list(&[0.0, 1.0])
    ); // C-style update adds a new var
    // C-style for binds init AND update SEQUENTIALLY within the clause (L.2.8e) — a later assignment
    // sees the NEW value of an earlier one, `let`-style (OpenSCAD-verified). Regression for BOSL2's
    // `_dp_distance_row` DP (skin method="distance"), which does `costs=…, newrow=…min(costs)…`.
    assert_eq!(
        ev("[for(a = 1, b = a + 1; a <= 1; a = a + 1) b]"),
        list(&[2.0])
    ); // init: b sees a=1
    assert_eq!(
        ev(
            "[for(i = 0, x = 0, y = 0; i <= 2; x = i * 10, y = x + 1, i = i + 1) if (i == 2) each [x, y]]"
        ),
        list(&[10.0, 11.0]) // update: y sees the NEW x (=10), not the old (would give 1)
    );
    // `each` SPLICES into a guard/loop operand (L.2.8f): `each if(cond) list` splices the list, not
    // `[[list]]` (OpenSCAD-verified). Regression for BOSL2's `nurbs_curve`, whose sample vector is
    // `[for(i) each if(!approx(...)) lerpn(...)]` — a nested result derailed the whole knot indexing.
    assert_eq!(ev("[each if(true) [1, 2, 3]]"), list(&[1.0, 2.0, 3.0])); // spliced, not [[1,2,3]]
    assert_eq!(ev("[each if(false) [1, 2, 3], 9]"), list(&[9.0])); // false guard → nothing
    assert_eq!(
        ev("[for(i = [0, 1]) each if(true) [i, i + 10]]"),
        list(&[0.0, 10.0, 1.0, 11.0])
    );
    // A `let` in a vector is TRANSPARENT (L.2.8h): it splices IFF its body does. `[let(x) [a,b]]`
    // contributes the vector as ONE element (a `let` is not an `each`), while `[let(x) each L]` splices —
    // OpenSCAD-verified. Regression for BOSL2's trapezoid, whose corners are `(let(i) [base[i]])`: the
    // single-point list must survive as one path point, not get flattened away.
    assert_eq!(
        ev("[(let(x = 5) [x, x + 1])]"),
        Value::list(vec![list(&[5.0, 6.0])]) // [[5,6]], not [5,6]
    );
    assert_eq!(
        ev("[let(x = 5) [x, x + 1]]"),
        Value::list(vec![list(&[5.0, 6.0])])
    ); // bare too
    assert_eq!(ev("[let(x = 5) each [x, x + 1]]"), list(&[5.0, 6.0])); // `each` body → splices
}

#[test]
fn math_builtins() {
    // exact (algebraic) results
    assert_eq!(ev("abs(-5)"), num(5.0));
    assert_eq!(ev("sign(-3)"), num(-1.0));
    assert_eq!(ev("sign(0)"), num(0.0));
    assert_eq!(ev("sign(3)"), num(1.0));
    assert_eq!(ev("floor(2.7)"), num(2.0));
    assert_eq!(ev("ceil(2.1)"), num(3.0));
    assert_eq!(ev("round(2.5)"), num(3.0));
    assert_eq!(ev("round(-2.5)"), num(-3.0)); // half AWAY from zero
    assert_eq!(ev("sqrt(16)"), num(4.0));
    assert_eq!(ev("pow(2, 10)"), num(1024.0));
    assert_eq!(ev("exp(0)"), num(1.0));
    assert_eq!(ev("ln(1)"), num(0.0));
    assert_eq!(ev("log(100)"), num(2.0)); // base 10
    assert_eq!(ev("min(3, 1, 2)"), num(1.0)); // several args
    assert_eq!(ev("min(7)"), num(7.0)); // single number
    assert_eq!(ev("max([1, 5, 2])"), num(5.0)); // a list arg
    assert_eq!(ev("norm([3, 4])"), num(5.0));
    assert_eq!(ev("cross([1, 0, 0], [0, 1, 0])"), list(&[0.0, 0.0, 1.0]));
    assert_eq!(ev("cross([1, 0], [0, 1])"), num(1.0)); // 2D cross → scalar
    // trig in DEGREES — exact at the quadrants (trig.rs), approx elsewhere
    assert_eq!(ev("sin(90)"), num(1.0));
    assert_eq!(ev("cos(0)"), num(1.0));
    assert_eq!(ev("cos(90)"), num(0.0));
    approx(ev("sin(30)"), 0.5);
    approx(ev("tan(45)"), 1.0);
    approx(ev("asin(1)"), 90.0);
    approx(ev("acos(0)"), 90.0);
    approx(ev("atan(1)"), 45.0);
    approx(ev("atan2(1, 1)"), 45.0);
    // undef propagation: non-number / missing / empty / wrong-shape
    assert_eq!(ev(r#"sqrt("a")"#), Value::Undef);
    assert_eq!(ev("abs()"), Value::Undef); // no args
    assert_eq!(ev("pow(2)"), Value::Undef); // missing 2nd arg
    assert_eq!(ev("min()"), Value::Undef); // no numbers
    assert_eq!(ev("norm(5)"), Value::Undef); // not a vector
    assert_eq!(ev("cross([1], [2])"), Value::Undef); // wrong-dimension cross
    assert_eq!(ev("cross(1, 2)"), Value::Undef); // non-vector cross
    assert_eq!(ev(r#"min(1, "a")"#), Value::Undef); // a non-number among several args
    // A builtin has NO declared parameter names — OpenSCAD reads every arg by SOURCE POSITION and ignores
    // the name (`func.cc` never consults `.name`), so `abs(x = -5)` is `abs(-5)` → 5, not undef. (Same rule
    // that lets BOSL2 call `search(..., index_col_num=1)`: the name is decorative, the position is real.)
    assert_eq!(ev("abs(x = -5)"), Value::Num(5.0));
    // a user function may SHADOW a builtin (resolution order).
    // An unimplemented/unknown function is warn-and-undef (I.5/L.5.7, OpenSCAD-faithful), not loud.
    assert_eq!(ev("nope_fn(1)"), Value::Undef);
}

#[test]
fn list_string_builtins() {
    let str = |x: &str| Value::string(x); // shorthand for expected string values
    let strs = |xs: &[&str]| Value::list(xs.iter().map(|s| Value::string(*s)).collect::<Vec<_>>());

    // len — element count, or CHARACTER count for a string (é is 1 char, 2 bytes).
    assert_eq!(ev("len([1, 2, 3])"), num(3.0));
    assert_eq!(ev(r#"len("héllo")"#), num(5.0));
    assert_eq!(ev("len([])"), num(0.0));
    assert_eq!(ev(r#"len(["a", [1, 2]])"#), num(2.0));
    assert_eq!(ev("len(5)"), Value::Undef); // a number has no length

    // concat — flatten ONE level; non-lists appended whole (strings NOT expanded).
    assert_eq!(ev("concat([1, 2], [3, 4])"), list(&[1.0, 2.0, 3.0, 4.0]));
    assert_eq!(ev("concat(1, [2, 3], 4)"), list(&[1.0, 2.0, 3.0, 4.0]));
    assert_eq!(ev(r#"concat("a", "b")"#), strs(&["a", "b"])); // strings appended, not split
    assert_eq!(ev("concat()"), list(&[])); // nothing → empty
    assert_eq!(
        ev("concat([1], [[2, 3]])"), // a nested list stays nested (one level only)
        Value::list(vec![num(1.0), list(&[2.0, 3.0])])
    );

    // str — concatenate string forms; top-level string RAW, nested strings QUOTED.
    assert_eq!(ev(r#"str("x=", 5)"#), str("x=5"));
    assert_eq!(ev("str(1, 2, 3)"), str("123"));
    assert_eq!(ev("str(true, false)"), str("truefalse"));
    assert_eq!(ev("str(undef)"), str("undef"));
    assert_eq!(ev("str(1.5)"), str("1.5"));
    assert_eq!(ev("str(-0)"), str("0")); // -0 normalizes
    assert_eq!(ev("str([1, 2])"), str("[1, 2]"));
    assert_eq!(ev(r#"str(["a", "b"])"#), str(r#"["a", "b"]"#)); // nested strings quoted
    assert_eq!(ev("str([0:2:6])"), str("[0 : 2 : 6]"));
    assert_eq!(ev("str()"), str("")); // no args → empty string
    assert_eq!(ev("str(function(x) x)"), str("function(x) x")); // function value → its source (L.2.6)

    // chr — codepoints → string; sub-1 / non-scalar codepoints SKIPPED; string arg → undef.
    assert_eq!(ev("chr(65)"), str("A"));
    assert_eq!(ev("chr([72, 105])"), str("Hi"));
    assert_eq!(ev(r#"chr([72, "x", 105])"#), str("Hi")); // non-number entries skipped
    assert_eq!(ev("chr([97:99])"), str("abc")); // a range of codepoints
    assert_eq!(ev("chr(0)"), str("")); // codepoint < 1 → skipped
    assert_eq!(ev("chr(1114112)"), str("")); // above U+10FFFF → not a scalar → skipped
    assert_eq!(ev(r#"chr("A")"#), Value::Undef); // chr wants numbers

    // ord — first char's codepoint.
    assert_eq!(ev(r#"ord("A")"#), num(65.0));
    assert_eq!(ev(r#"ord("abc")"#), num(97.0)); // first char only
    assert_eq!(ev(r#"ord("é")"#), num(233.0)); // U+00E9
    assert_eq!(ev(r#"ord("")"#), Value::Undef); // empty → undef
    assert_eq!(ev("ord(5)"), Value::Undef); // non-string → undef

    // reverse — list or string.
    assert_eq!(ev("reverse([1, 2, 3])"), list(&[3.0, 2.0, 1.0]));
    assert_eq!(ev(r#"reverse("abc")"#), str("cba"));
    assert_eq!(
        ev(r#"reverse(["a", 1])"#),
        Value::list(vec![num(1.0), str("a")])
    );
    assert_eq!(ev("reverse(5)"), Value::Undef); // not a list/string

    // lookup — linear interpolation, CLAMPED at the ends.
    assert_eq!(
        ev("lookup(2, [[0, 0], [1, 10], [2, 20], [3, 30]])"),
        num(20.0)
    ); // exact
    assert_eq!(ev("lookup(1.5, [[0, 0], [1, 10], [2, 20]])"), num(15.0)); // interpolated
    assert_eq!(ev("lookup(-5, [[0, 0], [1, 10]])"), num(0.0)); // below all → clamp to first
    assert_eq!(ev("lookup(100, [[0, 0], [1, 10]])"), num(10.0)); // above all → clamp to last
    assert_eq!(ev(r#"lookup("a", [[0, 0]])"#), Value::Undef); // non-numeric key
    assert_eq!(ev("lookup(1, [])"), Value::Undef); // no valid pairs

    // search — func.cc's find-indices protocol.
    assert_eq!(ev("search(3, [1, 2, 3, 4, 5])"), list(&[2.0])); // number → flat index list
    assert_eq!(ev("search(3, [3, 3, 3, 3], 2)"), list(&[0.0, 1.0])); // capped at num_returns
    assert_eq!(ev(r#"search("b", "abcabc")"#), list(&[1.0])); // string, first hit per char
    assert_eq!(ev(r#"search("bc", "abcabc")"#), list(&[1.0, 2.0])); // one index per search char
    assert_eq!(ev(r#"search("e", "abc")"#), list(&[])); // no match, num_returns=1 → dropped
    assert_eq!(
        ev(r#"search("a", "abcabc", 0)"#), // num_returns=0 → ALL, nested
        Value::list(vec![list(&[0.0, 3.0])])
    );
    assert_eq!(
        ev(r#"search("ab", "abcabc", 0)"#),
        Value::list(vec![list(&[0.0, 3.0]), list(&[1.0, 4.0])])
    );
    assert_eq!(ev("search([1, 3], [1, 2, 3, 4])"), list(&[0.0, 2.0])); // vector find
    // index_col_num: search a specific column of table rows.
    assert_eq!(
        ev(r#"search(3, [[1, "a"], [3, "b"], [3, "c"]], 0, 0)"#),
        list(&[1.0, 2.0])
    );
    assert_eq!(
        ev(r#"search("b", [["a", 1], ["b", 2], ["c", 3]], 1, 0)"#),
        list(&[1.0])
    );
    assert_eq!(ev(r#"search(2, [["a", 1], ["b", 2]], 1, 1)"#), list(&[1.0])); // column 1
    assert_eq!(ev("search(3, [[1, 2], [3, 4]])"), list(&[1.0])); // numeric ROW, column-0 match
}

/// `search`'s `num_returns=1` MISS asymmetry (verified vs the oracle 2026.06.12) — the fix that unblocked
/// BOSL2's `list_remove` → `str_split` → screw-table chain. A LIST match keeps a miss as `[]` POSITIONALLY
/// (so `list_remove`'s `sres[i] == []` aligns); a STRING match DROPS it (length shrinks). OpenSCAD quirk.
#[test]
fn search_num_returns_one_miss_asymmetry() {
    let empty = || Value::num_list(Vec::new());
    // list match: 0↛, 1→0, 2↛, 3↛ → misses kept as `[]`
    assert_eq!(
        ev("search([0, 1, 2, 3], [1], 1)"),
        Value::list(vec![empty(), num(0.0), empty(), empty()])
    );
    // all miss → all `[]`, LENGTH preserved (what list_remove counts on)
    assert_eq!(
        ev("search([3, 6, 9, 12], [1], 1)"),
        Value::list(vec![empty(), empty(), empty(), empty()])
    );
    // string match DROPS the miss (`e` vanishes)
    assert_eq!(ev(r#"search("abe", "abcabc", 1)"#), list(&[0.0, 1.0]));
    assert_eq!(ev("search(1)"), Value::Undef); // missing the table arg → undef
    assert_eq!(ev("search(1, 5)"), list(&[])); // a non-list table yields no matches
    assert_eq!(ev("search(undef, [1, 2])"), Value::Undef); // a non-searchable find → undef
    assert_eq!(ev(r#"search("a", "aaa", -1)"#), list(&[0.0])); // a bad num_returns falls back to 1
    // A builtin reads args by SOURCE POSITION, name ignored — so the DOCUMENTED names for `search`'s 3rd/4th
    // args (`num_returns_per_match`, `index_col_num`) resolve positionally. BOSL2's `in_list(v,list,idx)`
    // lives on this: `search([v], rows, num_returns_per_match=1, index_col_num=1)` must search COLUMN 1.
    assert_eq!(
        ev(
            r#"search(["bar"], [[2,"foo"],[4,"bar"],[3,"baz"]], num_returns_per_match=1, index_col_num=1)"#
        ),
        Value::list(vec![num(1.0)]) // "bar" is row 1's column 1 → first-hit index 1
    );
    // Positional-ONLY, taken literally: a name does NOT rescue an out-of-order arg. `index_col_num=1`
    // alone sits at POSITION 2, so it's read as `num_returns_per_match=1` and `index_col_num` stays 0 →
    // "bar" is searched against WHOLE rows, none match → a kept miss `[]`. (OpenSCAD behaves the same; this
    // is why BOSL2 always passes BOTH names, in order.)
    assert_eq!(
        ev(r#"search(["bar"], [[2,"foo"],[4,"bar"]], index_col_num=1)"#),
        Value::list(vec![Value::num_list(Vec::new())])
    );

    // lookup edge cases: labeled tables, malformed rows, missing/degenerate inputs.
    assert_eq!(
        ev(r#"lookup(1.5, [[1, 10, "a"], [2, 20, "b"]])"#),
        num(15.0)
    ); // extra label column ignored
    assert_eq!(
        ev(r#"lookup(1.5, [["x", 0], [1, 10], [2, 20]])"#),
        num(15.0)
    ); // a non-pair row is skipped
    assert_eq!(ev("lookup(5)"), Value::Undef); // missing the table arg
    assert_eq!(ev("lookup(1, [[5]])"), Value::Undef); // a too-short row is not a pair
    assert_eq!(ev(r#"lookup(1, "abc")"#), Value::Undef); // no pairs in a string table
}

#[test]
fn type_predicate_builtins() {
    let t = Value::Bool(true);
    let f = Value::Bool(false);

    // is_undef treats a MISSING or unbound arg as undef; the positive predicates need the value present.
    assert_eq!(ev("is_undef(undef)"), t);
    assert_eq!(ev("is_undef(nope)"), t); // an unbound name is undef
    assert_eq!(ev("is_undef()"), t); // no arg → undef-like
    assert_eq!(ev("is_undef(5)"), f);

    assert_eq!(ev("is_bool(true)"), t);
    assert_eq!(ev("is_bool(1)"), f); // 1 is a number, not a bool (no coercion)

    assert_eq!(ev("is_num(5)"), t);
    assert_eq!(ev("is_num(0/0)"), f); // NaN is NOT is_num in OpenSCAD (`type==NUMBER && !isnan`) → is_nan catches it
    assert_eq!(ev(r#"is_num("a")"#), f);
    assert_eq!(ev("is_num([1, 2])"), f);
    assert_eq!(ev("is_num()"), f); // no arg → false

    assert_eq!(ev(r#"is_string("a")"#), t);
    assert_eq!(ev("is_string(5)"), f);

    assert_eq!(ev("is_list([1, 2])"), t); // NumList fast path
    assert_eq!(ev(r#"is_list([1, "a"])"#), t); // heterogeneous List
    assert_eq!(ev("is_list([])"), t); // empty is a list
    assert_eq!(ev(r#"is_list("a")"#), f); // a string is NOT a list
    assert_eq!(ev("is_list([0:2])"), f); // a range is NOT a list

    assert_eq!(ev("is_function(function(x) x)"), t);
    assert_eq!(ev("is_function(5)"), f);

    // version — a PINNED constant (last stable 2021.01), deterministic by doctrine.
    assert_eq!(ev("version()"), list(&[2021.0, 1.0, 0.0]));
    assert_eq!(ev("version_num()"), num(20_210_100.0));

    // rands: boost-MT19937 bug-for-bug (L.2.2). Seeded → deterministic + byte-exact vs the oracle; the RNG
    // internals + the byte-exact proof live in the `rng` module — here just the builtin shape.
    match ev("rands(0, 1, 5, 42)") {
        Value::NumList(v) => {
            assert_eq!(v.len(), 5);
            assert!(v.iter().all(|&x| (0.0..1.0).contains(&x)));
            assert!((v[0] - 0.796_543).abs() < 1e-5); // matches OpenSCAD 2026.06.12
        }
        other => panic!("rands should return a NumList, got {other:?}"),
    }
    assert_eq!(ev("rands(0, 1)"), Value::Undef); // missing count → undef

    // Seedless rands ADVANCES one per-eval stream (L.2.8b): two seedless calls DIFFER — OpenSCAD draws
    // them from a single global engine, so BOSL2 can build a non-degenerate random line from two rands()
    // calls (a fresh engine per call would repeat and collapse it to a point). Seeded stays a pure fn.
    let adv = fab_lang::evaluate_full("a = rands(-1, 1, 3); b = rands(-1, 1, 3); echo(a == b);")
        .expect("evaluates");
    assert_eq!(adv.echos(), ["false"]); // consecutive seedless draws differ
    let seeded = fab_lang::evaluate_full("echo(rands(0, 1, 3, 42) == rands(0, 1, 3, 42));")
        .expect("evaluates");
    assert_eq!(seeded.echos(), ["true"]); // an explicit seed is reproducible (fresh engine, oracle-exact)
}

#[test]
fn let_expressions() {
    assert_eq!(ev("let(a = 1) a + 1"), num(2.0));
    assert_eq!(ev("let(a = 1, b = 2) a + b"), num(3.0));
    assert_eq!(ev("let(a = 1, b = a + 1) b"), num(2.0)); // SEQUENTIAL: b sees a
    assert_eq!(ev("let(a = 1, a = 2) a"), num(2.0)); // a later binding shadows an earlier one
    assert_eq!(ev("let() 5"), num(5.0)); // no bindings → just the body
    assert_eq!(ev("let(a = 10) let(b = a) b"), num(10.0)); // nested lets: inner sees the outer binding
    assert_eq!(ev("let(a = 1) (a) + nope"), Value::Undef); // a bound inside; an outer-unbound name is undef
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
    // DUPLICATE param name: the explicit `a = 3` wins over the trailing defaultless `a` slot (defaults bind
    // first, args second — same OpenSCAD two-phase rule as modules). Without it the trailing undef clobbered
    // the arg and the body saw `a = undef`.
    assert_eq!(ev("(function(a, b, a) a)(a = 3, b = 2)"), num(3.0));
    assert_eq!(ev("(function(a, b, a) a)(3, 2)"), num(3.0)); // positional fills the FIRST `a`
}

/// `str()` of a function VALUE renders its SOURCE, OpenSCAD-style (verified vs oracle) — the fnliterals
/// corpus (62 tests) asserts these exact strings. Prints the closure's params + body as written, so a
/// captured variable shows as its NAME (`function() target_func(a)` — `a`, not its value). Rendering the
/// literal AST (not the runtime value) also means `str()` of a recursive closure is finite (fixes #162).
#[test]
fn str_of_a_function_value() {
    assert_eq!(
        ev(r"str(function(x) target_func(x))"),
        Value::string("function(x) target_func(x)")
    );
    assert_eq!(
        ev(r"str(function(x, y) target_func(x, y))"),
        Value::string("function(x, y) target_func(x, y)")
    );
    assert_eq!(
        ev(r"str(function() target_func(a))"),
        Value::string("function() target_func(a)")
    );
    // a captured value does NOT substitute into the rendering — it's the source `a`, not 3
    assert_eq!(
        ev("let(a = 3) str(function() f(a))"),
        Value::string("function() f(a)")
    );
    // NESTED function literals render BARE too — no wrapping parens, no space after `function`
    // (L.2.8g): OpenSCAD's `str()` format, vs the canonical printer's parenthesized `(function (x) …)`.
    // This is what BOSL2's fnliterals `f_1arg`/`f_2arg`/… str-equality tests assert.
    assert_eq!(
        ev(r"str(function(a) (a == undef ? function(x) g(x) : function() g(a)))"),
        Value::string("function(a) ((a == undef) ? function(x) g(x) : function() g(a))")
    );
}

/// Letrec: a function literal bound to a NAME can call ITSELF by that name (verified vs the oracle — both
/// forms echo 15/10). Our COW frames can't self-reference at capture time, so the closure carries its
/// definition name and re-injects it at call time. BOSL2 leans on this (gears.scad's `strip_left`,
/// fnliterals' partial applications).
#[test]
fn recursive_function_literals() {
    assert_eq!(
        ev("let(g = function(m) m <= 0 ? 0 : m + g(m - 1)) g(5)"),
        num(15.0)
    );
    // deep recursion (proves it's a real fixpoint, not one-level): sum 1..100
    assert_eq!(
        ev("let(s = function(m) m <= 0 ? 0 : m + s(m - 1)) s(100)"),
        num(5050.0)
    );
    // an ANONYMOUS literal has no self-name → an unbound inner call is undef, not itself
    assert_eq!(ev("(function(m) m)(7)"), num(7.0));
}

#[test]
fn program_level_unknowns_warn_use_include_still_defers() {
    // L.5.7: an unknown module/function that PARSES (a typo / unimplemented builtin) is NOT loud — it
    // warns "Ignoring unknown module/function 'name'" and yields nothing / undef, so eval SUCCEEDS and
    // renders the rest (a corpus using a newer-BOSL2 symbol still loads). The named symbol surfaces in
    // the console for the evaluator-gap worklist. (A user module DEFINITION + INSTANTIATION both work —
    // see module_corpus.rs.)
    for (src, warning) in [
        ("nope_module();", "Ignoring unknown module 'nope_module'"),
        ("x = zz(1);", "Ignoring unknown function 'zz'"), // an unknown-fn assignment RHS
        ("{ y = zz(1); }", "Ignoring unknown function 'zz'"), // …inside a block too
    ] {
        let full = fab_lang::evaluate_full(src).expect("warn-and-continue, not an error");
        assert!(
            full.warnings().contains(&warning),
            "expected warning {warning:?} for {src:?}, got {:?}",
            full.warnings()
        );
    }
    // A raw `eval_program` still can't resolve `use`/`include` (the loader does) → an actual defer.
    for src in ["use <lib.scad>", "include <lib.scad>"] {
        let prog = parse(src).expect("parses");
        let err = eval_program(&prog, &Scope::new()).unwrap_err();
        assert!(
            matches!(&err, Error::Unimplemented(m) if m.contains("use/include")),
            "expected Unimplemented(…use/include…) for {src:?}, got {err:?}"
        );
    }
}

#[test]
fn user_module_via_eval_program() {
    // The NON-loader path (eval_program → build_ctx) also registers + calls a user module: define,
    // instantiate, get the body's geometry. (The loader path is covered in module_corpus.rs.)
    let mesh = eval_program(
        &parse("module box(s) cube(s); box(3);").expect("parses"),
        &Scope::new(),
    )
    .expect("a user module call flattens to its body's mesh");
    assert!(mesh.tri_count() > 0);
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

#[test]
fn let_assert_echo_chain_evaluates_as_a_series() {
    // A let/assert/echo chain as ONE expression — bind, check, echo, return: OpenSCAD's
    // series-of-statements idiom that `chain_expr` folds. This proves the fold's SEMANTICS in one shot:
    // the lets bind sequentially (b = a*3 = 6, so a's binding reached b), the assert HOLDS (else `ev`
    // would panic on the eval error), the echo is a no-op on the value, and the body is `b + 1` = 7.
    assert_eq!(
        ev("let(a=2) let(b=a*3) assert(b==6) echo(b) b + 1"),
        num(7.0)
    );
    // a FAILING assert in the chain aborts eval — LOUD, never a wrong value.
    let _ = ev_err("let(a=1) assert(a==2) a");
}
