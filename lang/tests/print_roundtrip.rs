//! H.5.1/H.5.2 — pretty-printer + the print/parse roundtrip.
//!
//! The printer emits FULLY-PARENTHESIZED canonical source; the roundtrip property is that printing is
//! IDEMPOTENT after the first parse — `print(parse(print(parse(s)))) == print(parse(s))`. If parsing
//! then printing loses or reshuffles structure, the second print diverges. proptest (below) drives it
//! over generated core expressions; the hand-written cases pin the whole grammar + the exact canonical
//! form per construct.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test helpers: unwrap/expect/panic ARE the assertions"
)]

use fab_lang::{StmtKind, parse, print, print_expr};

/// Print a parsed program.
fn rt(src: &str) -> String {
    print(&parse(src).expect("parses"))
}

#[test]
fn print_expr_prints_a_bare_expression() {
    // print_expr is the expression-only entry (no statement scaffolding / trailing `;`).
    let prog = parse("v = 1 + 2 * 3;").expect("parses");
    let StmtKind::Assignment { value, .. } = &prog.stmts[0].kind else {
        panic!("assignment");
    };
    assert_eq!(print_expr(value), "(1 + (2 * 3))");
}

/// Assert the printer is idempotent: re-parsing + re-printing its output is a fixpoint.
fn roundtrips(src: &str) {
    let once = rt(src);
    let twice =
        print(&parse(&once).unwrap_or_else(|e| panic!("re-parse of\n{once}\nfailed: {e:?}")));
    assert_eq!(
        once, twice,
        "printer not idempotent for {src:?}\n first:\n{once}\n second:\n{twice}"
    );
}

#[test]
fn every_construct_roundtrips() {
    // One program touching every StmtKind + ExprKind variant — its idempotency exercises every
    // printer arm (⇒ full print.rs coverage) and proves the whole grammar survives print→parse.
    let src = "\
        use <mcad.scad> include <util.scad>\n\
        a = 1+2*3-4/5%6^7 << 8 >> 9 | 10 & 11 == 12 != 13 < 14 <= 15 > 16 >= 17 && 18 || 19;\n\
        b = -1 + +2 + !x + ~3;\n\
        c = p ? q : r;\n\
        d = [1,2,3]; e = []; f = [0:5]; g = [0:2:10];\n\
        h = arr[0].field(k=1, $fn=2); i = \"s\\n\\t\\\"q\"; j = undef; k = true; l = false; m = $t;\n\
        translate([1,0,0]) cube(2); #%*!sphere(); group(){ x(); } echo(\"z\"); ;\n\
        module box(w, d=1, $fn=8) { cube([w,d,1]); } function sq(x) = x*x;\n\
        if ($t > 0) cube(1); else if (n) sphere(); else ;\n\
        fl = function(x) let(a=x) assert(a>0, \"pos\") echo(a) a*a;\n\
        lc = [for(i=[0:2]) each [i, i*2], for(k=0;k<2;k=k+1) if(k) k else -k];\n\
        translate([0,0,0]) { { cube(1); } }\n";
    roundtrips(src);
}

#[test]
fn canonical_form_is_fully_parenthesized() {
    // Precedence never relies on the reader — every composite wraps in parens.
    assert_eq!(rt("v = 1+2*3;"), "v = (1 + (2 * 3));\n");
    assert_eq!(rt("v = -2^2;"), "v = (-(2 ^ 2));\n"); // unary looser than ^, made explicit
    assert_eq!(rt("v = a?b:c;"), "v = (a ? b : c);\n");
    assert_eq!(rt("v = a[0].f(1);"), "v = a[0].f(1);\n"); // postfix needs no parens
    assert_eq!(rt("v = [0:2:9];"), "v = [0 : 2 : 9];\n");
    // module children always print braced (round-trips a single nested-block child).
    assert_eq!(rt("cube(2);"), "cube(2){}\n");
    // strings re-escape so the VALUE round-trips — every escape arm: `\ " \n \t \r`, plus a plain char.
    assert_eq!(rt(r#"v = "\\\"\n\t\rz";"#), "v = \"\\\\\\\"\\n\\t\\rz\";\n");
}

#[test]
fn deferred_and_comprehension_forms_roundtrip() {
    for src in [
        "v = function(a, b=2) a+b;",
        "v = let(x=1, y=2) x*y;",
        "v = assert(c) x;",
        "v = assert(c);", // no body
        "v = echo(m) x;",
        "v = [for(i=r) i];",
        "v = [for(i=r) if(i>0) i else -i];",
        "v = [for(i=r) if(i>0) i];", // comprehension if WITHOUT else
        "v = [each list, 5];",
        "v = [(for(i=r) i)];", // parenthesized comprehension
        "v = [for(i=r) let(j=i) j];",
        "module m() ; function f() = 0;",
        "if (a) b(); else c();",
        "use <x.scad>",
    ] {
        roundtrips(src);
    }
}

// ─────────────────────────────── proptest: the roundtrip property over generated exprs ─────────────

mod strat {
    use proptest::prelude::*;

    /// A core expression source generator (no context-sensitive comprehensions — those get explicit
    /// roundtrip cases above). Builds valid OpenSCAD expression source, recursively + depth-bounded.
    pub fn core_expr() -> impl Strategy<Value = String> {
        // Curated identifiers — a regex could emit a KEYWORD (`for`/`if`/`let`/…), which isn't an
        // identifier and would break the roundtrip spuriously.
        let ident = prop::sample::select(vec!["x", "y", "z", "foo", "bar", "n"])
            .prop_map(|s: &str| s.to_string());
        let leaf = prop_oneof![
            (0u32..1000).prop_map(|n| n.to_string()),
            ident,
            Just("true".to_string()),
            Just("false".to_string()),
            Just("undef".to_string()),
        ];
        leaf.prop_recursive(4, 48, 4, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
                (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(a, b, c)| format!("({a} ? {b} : {c})")),
                inner.clone().prop_map(|a| format!("(-{a})")),
                prop::collection::vec(inner.clone(), 0..4)
                    .prop_map(|xs| format!("[{}]", xs.join(", "))),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("f({a}, {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("(let (x = {a}) {b})")),
            ]
        })
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(400))]

    /// The printer is idempotent over generated core expressions: parse→print→parse→print is a fixpoint.
    #[test]
    fn generated_expressions_roundtrip(src in strat::core_expr()) {
        let wrapped = format!("v = {src};");
        let once = rt(&wrapped);
        let twice = print(&parse(&once).expect("re-parse"));
        proptest::prop_assert_eq!(once, twice);
    }
}
