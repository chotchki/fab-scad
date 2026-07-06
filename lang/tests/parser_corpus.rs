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
use fab_lang::{Arg, Error, Expr, Parameter, StmtKind, parse};

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
        FunctionLiteral { params, body } => {
            format!(
                "(fn [{}] {})",
                params.iter().map(sp).collect::<Vec<_>>().join(" "),
                sx(body)
            )
        }
        Let { bindings, body } => format!("(let [{}] {})", sargs(bindings), sx(body)),
        Assert { args, body } => format!("(assert [{}]{})", sargs(args), sbody(body.as_deref())),
        Echo { args, body } => format!("(echo [{}]{})", sargs(args), sbody(body.as_deref())),
        LcFor { bindings, body } => format!("(for [{}] {})", sargs(bindings), sx(body)),
        LcForC {
            init,
            cond,
            update,
            body,
        } => format!(
            "(forc [{}] {} [{}] {})",
            sargs(init),
            sx(cond),
            sargs(update),
            sx(body)
        ),
        LcEach(body) => format!("(each {})", sx(body)),
        LcIf {
            cond,
            then,
            els: Some(e),
        } => format!("(lcif {} {} {})", sx(cond), sx(then), sx(e)),
        LcIf {
            cond,
            then,
            els: None,
        } => format!("(lcif {} {})", sx(cond), sx(then)),
    }
}

/// Format a parameter: `name` or `name=default`.
fn sp(p: &Parameter) -> String {
    match &p.default {
        Some(d) => format!("{}={}", p.name, sx(d)),
        None => p.name.clone(),
    }
}

/// Format an argument list (space-joined).
fn sargs(args: &[Arg]) -> String {
    args.iter().map(sa).collect::<Vec<_>>().join(" ")
}

/// Format an optional `expr_or_empty` body — a leading space + the body, or nothing.
fn sbody(body: Option<&Expr>) -> String {
    body.map_or(String::new(), |b| format!(" {}", sx(b)))
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

/// A parse error carries a "while parsing …" BREADCRUMB — the grammar path the parser was on, so a
/// deep failure reports WHERE it went wrong, not just the leaf (BOSL2 diagnosis needs this).
#[test]
fn syntax_errors_carry_a_context_breadcrumb() {
    // a broken argument names both the leaf reason AND that we were in a call argument.
    let arg = err("cube(1+);");
    assert!(arg.contains("expected"), "reworded head: {arg}");
    assert!(
        arg.contains("while parsing a call argument"),
        "breadcrumb: {arg}"
    );
    // a broken assignment value / function body point at their construct too.
    assert!(err("x = 1 + ;").contains("while parsing an assignment value"));
    assert!(err("function f() = 1 + ;").contains("while parsing a function body"));
}

#[test]
fn list_comprehensions_parse_every_form() {
    // With H.3.2, the whole grammar parses — no parse-level construct bails anymore.
    assert_eq!(e("[for(i=[0:3]) i]"), "[(for [i=(range 0 3)] i)]"); // basic for
    assert_eq!(e("[each [1,2], 3]"), "[(each [1 2]) 3]"); // each splices; mixed with a plain elem
    assert_eq!(
        e("[for(i=0; i<5; i=i+1) i]"),
        "[(forc [i=0] (Lt i 5) [i=(Add i 1)] i)]" // C-style for
    );
    assert_eq!(e("[for(i=r) if(i>0) i]"), "[(for [i=r] (lcif (Gt i 0) i))]"); // comprehension if
    assert_eq!(
        e("[for(i=r) if(i>0) i else -i]"),
        "[(for [i=r] (lcif (Gt i 0) i (Neg i)))]" // if/else
    );
    assert_eq!(
        e("[for(i=r) let(j=i*2) j]"),
        "[(for [i=r] (let [j=(Mul i 2)] j))]" // let in a comprehension
    );
    assert_eq!(
        e("[for(i=r) for(j=r) [i,j]]"),
        "[(for [i=r] (for [j=r] [i j]))]" // nesting
    );
    assert_eq!(e("[(for(i=r) i)]"), "[(for [i=r] i)]"); // parenthesized comprehension element
    assert_eq!(e("[(1+2), 3]"), "[(Add 1 2) 3]"); // a plain `(expr)` element is NOT a comprehension
    // a vector `let` whose body IS a comprehension takes the lc_let path (Let{body: LcFor}), which the
    // plain let-expr path (body: expr) can't parse — `for` isn't an expression.
    assert_eq!(e("[let(a=1) for(i=r) a]"), "[(let [a=1] (for [i=r] a))]");
    // a parenthesized expr FOLLOWED BY an operator is a plain expr, not a paren-comprehension — the
    // `(`-guard must peek at what's inside, not fire on every `(`.
    assert_eq!(e("[(1)+2, 3]"), "[(Add 1 2) 3]");
    assert_eq!(e("[for(i=r, j=s) i+j]"), "[(for [i=r j=s] (Add i j))]"); // multiple bindings
    assert_eq!(e("[0, for(i=r) i, 9]"), "[0 (for [i=r] i) 9]"); // mixed plain + comprehension
}

#[test]
fn ranges_and_string_escapes_pinned() {
    // H.3.6 — ranges + string escapes/unicode were already implemented (G.3.3 + the lexer); this pins
    // them as part of the "expressions complete" audit. Ranges: 2-part and 3-part, expr bounds.
    assert_eq!(e("[a:b]"), "(range a b)");
    assert_eq!(e("[a:s:b]"), "(range a s b)");
    // String literals arrive DECODED (lexer `decode_str`): `\n`→newline, `\u{H}{4}`→codepoint,
    // undefined escape drops the backslash. The Debug print re-escapes, so this round-trips the value.
    assert_eq!(parse_str(r#""tab\tend""#), "tab\tend");
    assert_eq!(parse_str(r#""é!""#), "é!"); // non-ASCII UTF-8 passes through a string body verbatim
    assert_eq!(parse_str(r#""\q""#), "q"); // undefined escape → the bare char
}

/// Parse a string-literal expression and return its DECODED value.
fn parse_str(src: &str) -> String {
    match &parse_expr(src).kind {
        Str(s) => s.clone(),
        other => panic!("expected a string literal, got {other:?}"),
    }
}

#[test]
fn comprehension_syntax_errors() {
    assert!(err("v=[for i=r) i];").contains('(')); // missing '(' after for
    assert!(err("v=[for(i=r i];").contains(')')); // missing ')' of for bindings
    assert!(err("v=[for(i=0; i<5 i=i+1) i];").contains(';')); // missing ';' between C-clauses
    assert!(err("v=[if(x) 1;").contains(')')); // missing ')' of comprehension if
    assert!(err("v=[(for(i=r) i];").contains(')')); // missing ')' of a parenthesized comprehension
    // a pathologically-nested comprehension errors LOUD via the depth guard, never overflows.
    let deep = format!("v=[{}x];", "each ".repeat(80));
    assert!(err(&deep).contains("deeply"));
}

#[test]
fn function_let_assert_echo_expressions_parse() {
    // function literal — body greedily takes a full expr (`function(x) x + 1` is `function(x)(x+1)`).
    assert_eq!(e("function(x) x + 1"), "(fn [x] (Add x 1))");
    assert_eq!(e("function(a, b=2) a*b"), "(fn [a b=2] (Mul a b))");
    assert_eq!(e("function() 0"), "(fn [] 0)"); // no params
    // let expression.
    assert_eq!(e("let(a=1, b=2) a+b"), "(let [a=1 b=2] (Add a b))");
    // assert / echo WITH a pass-through body.
    assert_eq!(e("assert(x>0) y"), "(assert [(Gt x 0)] y)");
    assert_eq!(e(r#"echo("hi", x) y"#), r#"(echo ["hi" x] y)"#);
    // assert / echo with NO body (expr_or_empty empty — next token is `;` from the wrapper).
    assert_eq!(e("assert(x)"), "(assert [x])");
    assert_eq!(e("echo(x)"), "(echo [x])");
    // the forms nest as bodies of one another.
    assert_eq!(
        e("let(a=1) function(x) a+x"),
        "(let [a=1] (fn [x] (Add a x)))"
    );
    // they are NOT valid as a cascade operand — `1 + function(x) x` is a syntax error.
    assert!(err("v=1+function(x)x;").contains("expression"));
    assert!(err("v=1+let(a=1)a;").contains("expression"));
}

#[test]
fn module_and_function_defs_parse() {
    // module def: plain param, defaulted param, $-var param; body is a block (inner_input).
    let p = parse("module box(w, h=1, $fn=8) { cube([w,h,1]); }").expect("parses");
    let StmtKind::ModuleDef { name, params, body } = &p.stmts[0].kind else {
        panic!("module def");
    };
    assert_eq!(name, "box");
    assert_eq!(params.len(), 3);
    assert_eq!(params[0].name, "w");
    assert!(params[0].default.is_none());
    assert_eq!(params[1].name, "h");
    assert!(params[1].default.is_some());
    assert_eq!(params[2].name, "$fn"); // special-variable parameter
    assert!(matches!(body.kind, StmtKind::Block(_)));

    // empty params + single-statement (non-block) body.
    let p = parse("module unit() cube(1);").expect("parses");
    let StmtKind::ModuleDef { params, body, .. } = &p.stmts[0].kind else {
        panic!("module def");
    };
    assert!(params.is_empty());
    assert!(matches!(body.kind, StmtKind::Module(_)));

    // trailing comma in the parameter list; `;` (empty) body.
    let p = parse("module m(a, b,) ;").expect("parses");
    let StmtKind::ModuleDef { params, .. } = &p.stmts[0].kind else {
        panic!("module def");
    };
    assert_eq!(params.len(), 2);

    // function def: expression body, defaulted param.
    let p = parse("function sq(x, k=2) = x*x + k;").expect("parses");
    let StmtKind::FunctionDef { name, params, body } = &p.stmts[0].kind else {
        panic!("function def");
    };
    assert_eq!(name, "sq");
    assert_eq!(params.len(), 2);
    assert_eq!(sx(body), "(Add (Mul x x) k)");

    // nested def inside a module body — inner_input admits defs (parser.y:221).
    let p = parse("module outer() { function inner() = 1; }").expect("parses");
    let StmtKind::ModuleDef { body, .. } = &p.stmts[0].kind else {
        panic!("module def");
    };
    let StmtKind::Block(stmts) = &body.kind else {
        panic!("block body");
    };
    assert!(matches!(stmts[0].kind, StmtKind::FunctionDef { .. }));
}

#[test]
fn defs_are_rejected_inside_child_blocks() {
    // child_statements ⊂ inner_input (H.2.6): a module/function def inside a module-call or `if`
    // child block is a parse error — matching parser.y's split grammar.
    assert!(err("translate() { module m(){} }").contains("child block")); // Module
    assert!(err("group() { function f() = 1; }").contains("child block")); // Function
    assert!(err("if (x) { module m(){} }").contains("child block")); // if children are child_statements
    assert!(err("a() { { function f()=1; } }").contains("child block")); // nested child block too
    // ...but a def in a TOP-LEVEL block (inner_input) or a module BODY is legal.
    assert!(parse("{ module m(){} }").is_ok());
    assert!(parse("module outer() { function inner() = 1; }").is_ok());
}

#[test]
fn use_and_include_parse_to_nodes_with_the_raw_path() {
    let p = parse("use <mcad/gears.scad>\ncube(1);").expect("parses");
    let StmtKind::Use(path) = &p.stmts[0].kind else {
        panic!("use node");
    };
    assert_eq!(path, "mcad/gears.scad");
    assert!(matches!(p.stmts[1].kind, StmtKind::Module(_))); // code after the use still parses

    let p = parse("include <util.scad>").expect("parses");
    let StmtKind::Include(path) = &p.stmts[0].kind else {
        panic!("include node");
    };
    assert_eq!(path, "util.scad");
}

#[test]
fn if_else_parses_in_every_position() {
    // bare if, no else → empty els.
    let p = parse("if (x) cube(1);").expect("parses");
    let StmtKind::If { then, els, .. } = &p.stmts[0].kind else {
        panic!("if");
    };
    assert_eq!(then.len(), 1);
    assert!(els.is_empty());

    // if/else, block branches.
    let p = parse("if (x > 0) { a(); b(); } else { c(); }").expect("parses");
    let StmtKind::If { then, els, .. } = &p.stmts[0].kind else {
        panic!("if");
    };
    assert_eq!(then.len(), 2);
    assert_eq!(els.len(), 1);

    // else-if chain: els is a single nested If (dangling-else binds to the nearest if).
    let p = parse("if (a) x(); else if (b) y(); else z();").expect("parses");
    let StmtKind::If { els, .. } = &p.stmts[0].kind else {
        panic!("if");
    };
    assert!(matches!(
        els.first().map(|s| &s.kind),
        Some(StmtKind::If { .. })
    ));

    // if in CHILD position — `if` is a module_instantiation, so this is free.
    let p = parse("translate([1,0,0]) if (t) cube(1);").expect("parses");
    let StmtKind::Module(m) = &p.stmts[0].kind else {
        panic!("module");
    };
    assert!(matches!(
        m.children.first().map(|s| &s.kind),
        Some(StmtKind::If { .. })
    ));

    // empty then-branch (`;`).
    let p = parse("if (x) ;").expect("parses");
    let StmtKind::If { then, .. } = &p.stmts[0].kind else {
        panic!("if");
    };
    assert!(then.is_empty());
}

#[test]
fn if_syntax_errors_and_depth_guard() {
    assert!(err("if x) a();").contains('(')); // missing '('
    assert!(err("if (x a();").contains(')')); // missing ')'
    assert!(err("if () a();").contains("expression")); // empty condition
    // A pathological else-if chain errors LOUD via the if/else depth guard, never overflows. Empty
    // (`;`) then-branches keep the recursion purely in the else-chain, so the if/else guard fires
    // FIRST — a `b()` then-branch would trip the module-call guard at depth before this one.
    let deep = format!("{};", "if (x) ; else ".repeat(60));
    assert!(err(&deep).contains("deeply"));
}

#[test]
fn def_syntax_errors() {
    // Every commit point in the def parsers names what it expected.
    assert!(err("module {}").contains("module name")); // def_name bail (module)
    assert!(err("module m {}").contains('(')); // missing '(' after the name
    assert!(err("module m(a {}").contains(')')); // missing ')' of the param list
    assert!(err("module m(1){}").contains("parameter name")); // parameter bail (a number)
    assert!(err("function (x)=1;").contains("function name")); // def_name bail (function)
    assert!(err("function f(x) 1;").contains('=')); // missing '='
    assert!(err("function f(x)=1").contains(';')); // missing ';'
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

// ─────────────────────────────── depth guards, EVERY recursive construct ────────────────────────

#[test]
fn every_recursive_construct_guards_depth() {
    // Each self-nesting parser path must trip a MAX_DEPTH guard (error contains "deeply"), never
    // overflow — the Safari-cliff discipline, per construct. A broken `depth + 1` on any of these is
    // a real stack-overflow vuln (cargo-mutants H.5.4 surfaced them): a mutant that stops the depth
    // growing recurses the full input, which either parses (so `err` panics on the unexpected Ok) or
    // overflows — caught either way.
    let n = 300;
    let deep: Vec<(String, &str)> = vec![
        (
            format!("v={}0;", "function(x) ".repeat(n)),
            "function-literal",
        ),
        (format!("v={}0;", "let(a=1) ".repeat(n)), "let-expr"),
        (format!("v={}0;", "assert(1) ".repeat(n)), "assert-expr"),
        (format!("v={}0;", "echo(1) ".repeat(n)), "echo-expr"),
        (
            format!("v={}0{};", "1?".repeat(n), ":0".repeat(n)),
            "ternary",
        ),
        (
            format!("v={}0{};", "[".repeat(n), "]".repeat(n)),
            "nested vector",
        ),
        (
            format!("v={}0{};", "f(".repeat(n), ")".repeat(n)),
            "call args",
        ),
        (format!("v={}2;", "2^".repeat(n)), "exponent"),
        (
            format!("v=[{}0];", "for(i=r) ".repeat(n)),
            "comprehension for",
        ),
        (format!("v=[{}0];", "each ".repeat(n)), "comprehension each"),
        (format!("v=[{}0];", "if(x) ".repeat(n)), "comprehension if"),
        (
            format!("v=[{}0];", "let(a=1) ".repeat(n)),
            "comprehension let",
        ),
        (
            format!("{}cube();", "module m() ".repeat(n)),
            "module def body",
        ),
    ];
    for (src, what) in deep {
        assert!(
            err(&src).contains("deeply"),
            "{what} must trip the depth guard, never overflow"
        );
    }
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
    let msg = err("a = 1;\nb = f(2;\nc = 3;"); // error on line 2, under the `;` where `)` was expected
    // The gutter + the FULL offending line (not truncated — this pins `line_end`; a mutation there
    // drops the trailing `;`).
    assert!(
        msg.contains("2 | b = f(2;"),
        "want the full line-2 source in:\n{msg}"
    );
    // The caret aligns under the `;`: the 4-char gutter + the line prefix before `;` (this pins the
    // character `col`, so a mutation that shifts it is caught).
    let caret = format!("\n{}^", " ".repeat("2 | b = f(2".len()));
    assert!(
        msg.contains(&caret),
        "want the caret aligned under `;` in:\n{msg}"
    );
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
        use <mcad.scad>\n include <util.scad>\n\
        a = 1+2*3-4/5%6^7 << 8 >> 9 | 10 & 11 == 12 != 13 < 14 <= 15 > 16 >= 17 && 18 || 19;\n\
        b = -1 + +2 + !x + ~3;\n\
        c = p ? q : r;\n\
        d = [1,2,3]; e = []; f = [0:5]; g = [0:2:10];\n\
        h = arr[0].field(k=1, $fn=2); i = \"s\"; j = undef; k = true; l = false; m = $t;\n\
        translate([1,0,0]) cube(2); #%*!sphere(); group(){ x(); } echo(\"z\"); ;\n\
        module box(w, d=1, $fn=8) { cube([w,d,1]); } function sq(x) = x*x;\n\
        if ($t > 0) cube(1); else if (n) sphere(); else ;\n\
        fl = function(x) let(a=x) assert(a>0, \"pos\") echo(a) a*a;\n\
        lc = [for(i=[0:2]) each [i, i*2], for(k=0;k<2;k=k+1) if(k) k else -k];\n";
    let a = parse(src).expect("parses");
    let b = parse(src).expect("parses");
    assert_eq!(a, b); // deterministic + PartialEq over all variants
    assert_eq!(a, a.clone()); // Clone
    assert!(format!("{a:?}").contains("Program")); // Debug
}
