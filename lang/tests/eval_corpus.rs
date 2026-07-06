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
    // (function literals + calling a function VALUE now evaluate — I.2.3.3; let → I.3.1; list
    // comprehensions → I.3.2; assert / echo → I.5 — see echo_and_assert_evaluate below.)
}

#[test]
fn echo_and_assert_evaluate() {
    // I.5: assert passes through its trailing value (a falsy condition is LOUD); echo emits an ECHO
    // line then passes through. Expression forms via the ev helper:
    assert_eq!(ev("assert(true) 1"), num(1.0));
    assert_eq!(ev("echo(1) 2"), num(2.0));
    assert_eq!(ev("echo(1)"), Value::Undef); // no trailing body → undef
    assert_eq!(ev("assert(true)"), Value::Undef);
    assert!(matches!(ev_err("assert(false)"), Error::Eval(_)));
    // assert arg forms: named condition/message, an unknown named (dropped), a non-string message.
    assert!(matches!(
        ev_err("assert(condition = false, message = \"m\")"),
        Error::Eval(m) if m.contains('m')
    ));
    assert!(matches!(ev_err("assert(false, foo = 1)"), Error::Eval(_))); // unknown named dropped
    assert!(matches!(
        ev_err("assert(false, 42)"),
        Error::Eval(m) if m.contains("42") // non-string message
    ));
    // Echo OUTPUT via the program path — evaluate_full captures the ordered message log; numbers are
    // formatted bug-for-bug (0.333333), strings quoted, named args as `a = 5`.
    let full =
        fab_lang::evaluate_full("echo(9); echo(1 / 3); echo(\"hi\", a = 5);").expect("evaluates");
    assert_eq!(full.echos(), ["9", "0.333333", "\"hi\", a = 5"]);
    assert!(full.warnings().is_empty());
    // A top-level assert/echo is NOT geometry; a falsy assert is loud.
    assert!(fab_lang::evaluate("assert(true); sphere(1, $fn = 8);").is_ok());
    assert!(matches!(
        fab_lang::evaluate("assert(1 == 2, \"nope\");"),
        Err(Error::Eval(_))
    ));
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
    assert_eq!(ev("abs(x = -5)"), Value::Undef); // math builtins take positional args (named → undef)
    // a user function may SHADOW a builtin (resolution order).
    // (unimplemented/unknown functions stay LOUD until I.5's warn-and-undef.)
    assert!(matches!(ev_err("nope_fn(1)"), Error::Unimplemented(m) if m.contains("I.4")));
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
    assert_eq!(ev("str(function(x) x)"), str("function ...")); // function form deferred to I.5

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
    assert_eq!(ev("search(1)"), Value::Undef); // missing the table arg → undef
    assert_eq!(ev("search(1, 5)"), list(&[])); // a non-list table yields no matches
    assert_eq!(ev("search(undef, [1, 2])"), Value::Undef); // a non-searchable find → undef
    assert_eq!(ev(r#"search("a", "aaa", -1)"#), list(&[0.0])); // a bad num_returns falls back to 1

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
    assert_eq!(ev("is_num(0/0)"), t); // NaN is still a number
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

    // rands is a DELIBERATE loud defer (non-deterministic seedless; seeded needs boost's RNG bug-for-bug).
    assert!(matches!(ev_err("rands(0, 1, 5)"), Error::Unimplemented(m) if m.contains("I.4")));
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
}

#[test]
fn program_level_defers_are_loud() {
    // Constructs that parse (H.2) but eval defers loudly. NOTE the moving line: as of I.2.4 a user
    // module DEFINITION and its INSTANTIATION both work (see module_corpus.rs); what's still loud here is
    // an UNKNOWN module (a typo / unimplemented builtin) and a RAW eval_program on `use`/`include` (the
    // loader resolves those via evaluate_file / evaluate_with_base — happy path in loader_corpus.rs).
    for (src, needle) in [
        ("nope_module();", "user module"), // an unknown module name — loud (typo / unimplemented)
        ("use <lib.scad>", "use/include"), // raw eval_program can't resolve it — loud
        ("include <lib.scad>", "use/include"),
        ("x = a.b;", "I.1"), // an erroring assignment RHS propagates out of eval_stmt
        ("{ y = a.b; }", "I.1"), // …and out of a block's inner statement
    ] {
        let prog = parse(src).expect("parses");
        let err = eval_program(&prog, &Scope::new()).unwrap_err();
        assert!(
            matches!(&err, Error::Unimplemented(m) if m.contains(needle)),
            "expected Unimplemented(…{needle}…) for {src:?}, got {err:?}"
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
