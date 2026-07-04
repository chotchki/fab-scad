//! G.3.3 parser conformance corpus — asserted against OpenSCAD's `parser.y` semantics.
//!
//! Expressions are checked as S-expressions (readable precedence assertions); statements + errors +
//! the depth guards + drop-safety are checked directly. These snippets seed the H.6 fuzz corpus.

#![allow(
    clippy::enum_glob_use,
    reason = "AST assertions read far better with bare variant names"
)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration-test helpers: expect/unwrap/panic ARE the assertions"
)]

use fab_lang::ExprKind::*;
use fab_lang::{Arg, Error, Expr, StmtKind, parse};

// ─────────────────────────────── S-expression pretty-printer (test-only) ───────────────────────

fn sx(e: &Expr) -> String {
    match &e.kind {
        Num(n) => format!("{n}"),
        Str(s) => format!("{s:?}"),
        Bool(b) => b.to_string(),
        Undef => "undef".to_string(),
        Ident(n) => n.clone(),
        Unary { op, operand } => format!("({op:?} {})", sx(operand)),
        Binary { op, lhs, rhs } => format!("({op:?} {} {})", sx(lhs), sx(rhs)),
        Ternary { cond, then, els } => format!("(?: {} {} {})", sx(cond), sx(then), sx(els)),
        Index { base, index } => format!("(index {} {})", sx(base), sx(index)),
        Member { base, field } => format!("(member {} {field})", sx(base)),
        Call { callee, args } => {
            format!(
                "(call {} [{}])",
                sx(callee),
                args.iter().map(sa).collect::<Vec<_>>().join(" ")
            )
        }
        Vector(v) => format!("[{}]", v.iter().map(sx).collect::<Vec<_>>().join(" ")),
        Range {
            start,
            step: Some(s),
            end,
        } => format!("(range {} {} {})", sx(start), sx(s), sx(end)),
        Range {
            start,
            step: None,
            end,
        } => format!("(range {} {})", sx(start), sx(end)),
    }
}

fn sa(a: &Arg) -> String {
    match &a.name {
        Some(n) => format!("{n}={}", sx(&a.value)),
        None => sx(&a.value),
    }
}

/// Parse a bare expression by wrapping it in an assignment and pulling the value out.
fn parse_expr(src: &str) -> Expr {
    let prog = parse(&format!("v={src};")).expect("expression parses");
    match prog.stmts.into_iter().next().map(|s| s.kind) {
        Some(StmtKind::Assignment { value, .. }) => value,
        other => panic!("expected an assignment, got {other:?}"),
    }
}

fn e(src: &str) -> String {
    sx(&parse_expr(src))
}

// ─────────────────────────────── precedence (parser.y cascade) ─────────────────────────────────

#[test]
fn arithmetic_precedence() {
    assert_eq!(e("1+2*3"), "(Add 1 (Mul 2 3))");
    assert_eq!(e("1*2+3"), "(Add (Mul 1 2) 3)");
    assert_eq!(e("1-2-3"), "(Sub (Sub 1 2) 3)"); // left-assoc
    assert_eq!(e("10%3*2"), "(Mul (Mod 10 3) 2)");
}

#[test]
fn power_is_right_assoc_between_unary_and_call() {
    assert_eq!(e("2^3^2"), "(Pow 2 (Pow 3 2))"); // right-assoc
    assert_eq!(e("-2^2"), "(Neg (Pow 2 2))"); // unary looser than ^
    assert_eq!(e("2^-3"), "(Pow 2 (Neg 3))"); // right operand is a unary
}

#[test]
fn ternary_is_right_assoc() {
    assert_eq!(e("a?b:c?d:e"), "(?: a b (?: c d e))");
    assert_eq!(e("1<2?3:4"), "(?: (Lt 1 2) 3 4)"); // condition is a comparison
}

#[test]
fn bitwise_sits_between_comparison_and_shift() {
    // parser.y quirk: `a < b | c` == `a < (b|c)`, and `a | b == c` == `(a|b) == c`.
    assert_eq!(e("a<b|c"), "(Lt a (BitOr b c))");
    assert_eq!(e("a|b==c"), "(Eq (BitOr a b) c)");
    assert_eq!(e("1&2<<3"), "(BitAnd 1 (Shl 2 3))");
}

#[test]
fn unary_and_logical() {
    assert_eq!(e("-+~1"), "(Neg (Pos (BitNot 1)))");
    assert_eq!(e("!a&&b"), "(And (Not a) b)");
    assert_eq!(e("a||b&&c"), "(Or a (And b c))");
}

// ─────────────────────────────── atoms + postfix ───────────────────────────────────────────────

#[test]
fn literals() {
    assert_eq!(e("42"), "42");
    assert_eq!(e("1.5"), "1.5");
    assert_eq!(e(r#""hi""#), "\"hi\"");
    assert_eq!(e("true"), "true");
    assert_eq!(e("false"), "false");
    assert_eq!(e("undef"), "undef");
    assert_eq!(e("foo"), "foo");
    assert_eq!(e("$fn"), "$fn"); // dollar-ident is an Ident
    assert_eq!(e("(1+2)"), "(Add 1 2)"); // parens group, no node
}

#[test]
fn vectors_and_ranges() {
    assert_eq!(e("[]"), "[]");
    assert_eq!(e("[1,2,3]"), "[1 2 3]");
    assert_eq!(e("[1,2,]"), "[1 2]"); // trailing comma
    assert_eq!(e("[0:5]"), "(range 0 5)"); // [start:end]
    assert_eq!(e("[0:2:10]"), "(range 0 2 10)"); // [start:STEP:end] — middle is step
    assert_eq!(e("[[1,2],[3,4]]"), "[[1 2] [3 4]]"); // nested
}

#[test]
fn postfix_chains() {
    assert_eq!(e("a[0]"), "(index a 0)");
    assert_eq!(e("a.x"), "(member a x)");
    assert_eq!(e("f(1,2)"), "(call f [1 2])");
    assert_eq!(e("f()"), "(call f [])"); // empty args
    assert_eq!(e("a[0].x(1)"), "(call (member (index a 0) x) [1])"); // left-assoc chain
    assert_eq!(e("f(x=1,$fn=8)"), "(call f [x=1 $fn=8])"); // named + $-arg
    assert_eq!(e("f(1,x=2,)"), "(call f [1 x=2])"); // positional + named + trailing comma
}

// ─────────────────────────────── statements ────────────────────────────────────────────────────

#[test]
fn assignment_and_empty() {
    let p = parse("x = 1 + 2;;").expect("parses");
    assert!(matches!(p.stmts[0].kind, StmtKind::Assignment { .. }));
    assert!(matches!(p.stmts[1].kind, StmtKind::Empty));
    assert_eq!(parse("").expect("empty").stmts.len(), 0);
}

#[test]
fn module_instantiation_forms() {
    // single child, modifier, block child, keyword module-id, empty child
    let p = parse("translate([1,0,0]) cube(2); #sphere($fn=8); group(){ a(); b(); } echo(\"x\");")
        .expect("parses");
    assert_eq!(p.stmts.len(), 4);
    let StmtKind::Module(m0) = &p.stmts[0].kind else {
        panic!("module");
    };
    assert_eq!(m0.name, "translate");
    assert_eq!(m0.children.len(), 1); // single child
    let StmtKind::Module(m1) = &p.stmts[1].kind else {
        panic!("module");
    };
    assert!(m1.modifiers.highlight); // `#`
    assert_eq!(m1.children.len(), 0); // `;` empty child
    let StmtKind::Module(m2) = &p.stmts[2].kind else {
        panic!("module");
    };
    assert_eq!(m2.children.len(), 2); // `{ a(); b(); }`
    let StmtKind::Module(m3) = &p.stmts[3].kind else {
        panic!("module");
    };
    assert_eq!(m3.name, "echo"); // keyword module-id
}

#[test]
fn top_level_block() {
    let p = parse("{ a(); b(); }").expect("parses");
    let StmtKind::Block(stmts) = &p.stmts[0].kind else {
        panic!("expected a block statement");
    };
    assert_eq!(stmts.len(), 2);
}

#[test]
fn all_modifiers_stack() {
    let p = parse("#%*!cube();").expect("parses");
    let StmtKind::Module(m) = &p.stmts[0].kind else {
        panic!("module");
    };
    let mods = m.modifiers;
    assert!(mods.root && mods.highlight && mods.background && mods.disable);
}

#[test]
fn keyword_module_ids() {
    for kw in ["for", "let", "assert", "each"] {
        let p = parse(&format!("{kw}();")).expect("parses");
        let StmtKind::Module(m) = &p.stmts[0].kind else {
            panic!("module");
        };
        assert_eq!(m.name, kw);
    }
}

// ─────────────────────────────── spans ─────────────────────────────────────────────────────────

#[test]
fn nodes_carry_byte_spans() {
    // In "v=1+2;" the `1+2` value spans bytes 2..5.
    let ex = parse_expr("1+2");
    assert_eq!(ex.span, 2..5);
    let Binary { lhs, .. } = &ex.kind else {
        panic!("binary");
    };
    assert_eq!(lhs.span, 2..3); // the `1`
}

// ─────────────────────────────── errors (each commit point / bail) ──────────────────────────────

fn err(src: &str) -> String {
    match parse(src) {
        Err(Error::Parse(msg)) => msg,
        other => panic!("expected a parse error for {src:?}, got {other:?}"),
    }
}

#[test]
fn syntax_errors_point_and_name() {
    assert!(err("v=f(1;").contains(')')); // missing ')'
    assert!(err("v=a[0;").contains(']')); // missing ']'
    assert!(err("v=[1:2;").contains(']')); // missing ']' of range
    assert!(err("v=[1,2;").contains(']')); // missing ']' of vector
    assert!(err("v=a?b;").contains(':')); // missing ':'
    assert!(err("v=1").contains(';')); // missing ';'
    assert!(err("v=(1;").contains(')')); // missing ')'
    assert!(err("v=a.;").contains("member")); // bad member name
    assert!(err("*;").contains("module name")); // modifier then no name
    assert!(err("{ a(); ").contains('}')); // unclosed block
    assert!(err("v=;").contains("expression")); // no expression
    assert!(err("1;").contains("statement")); // number can't start a statement
    assert!(err("cube(2) 5;").contains("module name")); // bad single child
}

#[test]
fn deferred_constructs_fail_loud() {
    assert!(err("module m(){}").contains("H.2"));
    assert!(err("function f()=1;").contains("H.2"));
    assert!(err("if(true) a();").contains("H.2"));
    assert!(err("use <lib.scad>").contains("H.2"));
    assert!(err("include <lib.scad>").contains("H.2"));
    assert!(err("v=function(x)x;").contains("H.2"));
    assert!(err("v=let(a=1)a;").contains("H.2"));
    assert!(err("v=assert(true)1;").contains("H.2"));
    assert!(err("v=echo(1)2;").contains("H.2"));
    assert!(err("v=[for(i=[0:3])i];").contains("H.3")); // list comprehension
    assert!(err("v=[1,each[2,3]];").contains("H.3")); // comprehension mid-vector
}

// ─────────────────────────────── depth guards (nesting → LOUD, not overflow) ────────────────────

#[test]
fn deep_nesting_errors_not_overflows() {
    let deep_parens = format!("v={}1{};", "(".repeat(500), ")".repeat(500));
    assert!(err(&deep_parens).contains("deeply")); // expr guard
    let deep_unary = format!("v={}1;", "-".repeat(500));
    assert!(err(&deep_unary).contains("deeply")); // unary guard
    let deep_blocks = format!("{}a();{}", "group(){".repeat(500), "}".repeat(500));
    assert!(err(&deep_blocks).contains("deeply")); // statement guard
    // arg-less chain so nothing recurses through `expr` — isolates the module_instantiation guard
    // (a chain WITH args like `translate([…])` hits the expr guard via the args first).
    let deep_children = format!("{}cube();", "a() ".repeat(500));
    assert!(err(&deep_children).contains("module calls")); // module_instantiation guard
}

// ─────────────────────────────── drop-safety (the non-recursive Drop) ───────────────────────────

#[test]
fn deep_left_chain_parses_and_frees_without_overflow() {
    // A 200k-term chain parses ITERATIVELY (no parse overflow) and frees via the explicit-stack
    // Drop (no teardown overflow). Would SIGABRT on a naive recursive Drop.
    let chain = format!("v={}1;", "1+".repeat(200_000));
    let prog = parse(&chain).expect("deep chain parses");
    drop(prog); // <- the load-bearing free
    // deep postfix + member chains too
    let idx = format!("v=a{};", "[0]".repeat(200_000));
    drop(parse(&idx).expect("deep index chain parses"));
    let mem = format!("v=a{};", ".b".repeat(200_000));
    drop(parse(&mem).expect("deep member chain parses"));
}

// ─────────────────────────────── never-panic ───────────────────────────────────────────────────

#[test]
fn never_panics_on_adversarial_input() {
    for src in [
        "",
        ";",
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "v=",
        "v=(",
        "v=[",
        "v=[1:",
        "v=f(",
        "v=a.",
        "cube(",
        "cube()",
        "*",
        "#",
        "!;",
        "v=1?2",
        "v=[1,",
        "module",
        "function",
        "if",
        "use",
        "\u{3}",
        "v=$",
        "1+",
        "]]]",
        "v=1++2",
        "translate()",
        "{{{",
        "a()a()",
    ] {
        let _ = parse(src); // must return, never panic
    }
}

// ─────────────────────────────── diagnostics (caret rendering) ──────────────────────────────────

#[test]
fn caret_diagnostic_points_at_the_line() {
    let msg = err("a = 1;\nb = f(2;\nc = 3;"); // error on line 2
    assert!(msg.contains("2 | "), "want line-2 gutter in:\n{msg}");
    assert!(msg.contains('^'), "want a caret in:\n{msg}");
    // error at end-of-input maps to source length, not a panic
    assert!(!err("v=1").is_empty());
    // multibyte before the caret keeps it aligned (no byte/char confusion, no panic)
    assert!(!err("v=\"héllo→\"+;").is_empty());
}

// ─────────────────────────────── derives + determinism ─────────────────────────────────────────

#[test]
fn ast_clones_compares_debugs_and_is_deterministic() {
    // One comprehensive program exercising every variant, then Clone + PartialEq + Debug.
    let src = "\
        a = 1+2*3-4/5%6^7 << 8 >> 9 | 10 & 11 == 12 != 13 < 14 <= 15 > 16 >= 17 && 18 || 19;\n\
        b = -1 + +2 + !x + ~3;\n\
        c = p ? q : r;\n\
        d = [1,2,3]; e = []; f = [0:5]; g = [0:2:10];\n\
        h = arr[0].field(k=1, $fn=2); i = \"s\"; j = undef; k = true; l = false; m = $t;\n\
        translate([1,0,0]) cube(2); #%*!sphere(); group(){ x(); } echo(\"z\"); ;\n";
    let a = parse(src).expect("parses");
    let b = parse(src).expect("parses");
    assert_eq!(a, b); // deterministic + PartialEq over all variants
    assert_eq!(a, a.clone()); // Clone
    assert!(format!("{a:?}").contains("Program")); // Debug
}
