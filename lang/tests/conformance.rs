//! H.5.3 — the bison-derived conformance suite.
//!
//! One minimal example per `parser.y` production (see `lang/docs/grammar-inventory.md`), asserted to
//! PARSE. This is the executable form of "every production accounted for" (H.1): a green run here is
//! the inventory proven, not asserted. Behavior/precedence/AST-shape live in `parser_corpus.rs`; this
//! file is the COMPLETENESS checklist — each row cites its `parser.y` production.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration-test helpers: unwrap/panic ARE the assertions"
)]

use fab_lang::{StmtKind, parse};

/// Assert a snippet parses (the production is reachable + accepted).
#[track_caller]
fn ok(production: &str, src: &str) {
    if let Err(e) = parse(src) {
        panic!("production {production} — {src:?} should parse, got {e:?}");
    }
}

#[test]
fn every_production_parses() {
    // ── top level + statements (parser.y:174-232) ──
    ok("input:empty", "");
    ok("input:TOK_USE", "use <lib.scad>");
    ok("statement:';'", ";");
    ok("statement:block", "{ a(); }");
    ok("statement:module_instantiation", "cube(1);");
    ok("statement:assignment", "x = 1;");
    ok("statement:TOK_MODULE", "module m(a, b=1) cube(a);");
    ok("statement:TOK_FUNCTION", "function f(x) = x*x;");

    // ── module instantiation + if/else (parser.y:234-332) ──
    ok("module_instantiation:'!'", "!cube(1);");
    ok("module_instantiation:'#'", "#cube(1);");
    ok("module_instantiation:'%'", "%cube(1);");
    ok("module_instantiation:'*'", "*cube(1);");
    ok("single_module_instantiation", "translate([1,0,0]) cube(1);");
    ok("ifelse_statement:if", "if (x) a();");
    ok("ifelse_statement:if-else", "if (x) a(); else b();");
    ok("child_statement:';'", "translate([0,0,0]);");
    ok("child_statement:block", "translate([0,0,0]) { a(); b(); }");
    for id in ["for", "let", "assert", "echo", "each"] {
        ok("module_id:keyword", &format!("{id}();"));
    }

    // ── expression tier cascade (parser.y:334-518) ──
    ok("logic_or", "v = a || b;");
    ok("logic_and", "v = a && b;");
    ok("equality", "v = a == b; w = a != b;");
    ok(
        "comparison",
        "v = a < b; w = a <= b; x = a > b; y = a >= b;",
    );
    ok("binaryor", "v = a | b;");
    ok("binaryand", "v = a & b;");
    ok("shift", "v = a << b; w = a >> b;");
    ok("addition", "v = a + b; w = a - b;");
    ok("multiplication", "v = a * b; w = a / b; x = a % b;");
    ok("unary", "v = -a; w = +a; x = !a; y = ~a;");
    ok("exponent", "v = a ^ b;");
    ok("call:call", "v = f(1, x=2);");
    ok("call:index", "v = a[0];");
    ok("call:member", "v = a.field;");
    ok("expr:ternary", "v = c ? t : e;");

    // ── expression non-cascade forms (parser.y:336-359) ──
    ok("expr:TOK_FUNCTION", "v = function(x) x + 1;");
    ok("expr:TOK_LET", "v = let(a=1) a;");
    ok("expr:TOK_ASSERT", "v = assert(c) x;");
    ok("expr:TOK_ASSERT:empty", "v = assert(c);");
    ok("expr:TOK_ECHO", "v = echo(m) x;");

    // ── primary + collections (parser.y:520-643) ──
    ok("primary:TOK_TRUE", "v = true;");
    ok("primary:TOK_FALSE", "v = false;");
    ok("primary:TOK_UNDEF", "v = undef;");
    ok(
        "primary:TOK_NUMBER",
        "v = 42; w = 0x1F; x = 1.5e-3; y = .5;",
    );
    ok("primary:TOK_STRING", r#"v = "hi\n";"#);
    ok("primary:TOK_ID", "v = foo; w = $fn;");
    ok("primary:paren", "v = (1 + 2);");
    ok("primary:range2", "v = [0:5];");
    ok("primary:range3", "v = [0:2:10];");
    ok("primary:empty_vector", "v = [];");
    ok("primary:vector", "v = [1, 2, 3,];");

    // ── list comprehensions (parser.y:582-643) ──
    ok("lc:for", "v = [for(i=[0:3]) i];");
    ok("lc:forc", "v = [for(i=0; i<5; i=i+1) i];");
    ok("lc:each", "v = [each list];");
    ok("lc:let", "v = [for(i=r) let(j=i) j];");
    ok("lc:if", "v = [for(i=r) if(i>0) i];");
    ok("lc:if-else", "v = [for(i=r) if(i>0) i else -i];");
    ok("lc:paren", "v = [(for(i=r) i)];");
    ok("lc:nested", "v = [for(i=r) for(j=s) [i,j]];");

    // ── parameters + arguments (parser.y:645-710) ──
    ok("parameter:plain", "module m(a) ;");
    ok("parameter:default", "module m(a = 1) ;");
    ok("argument:positional", "cube(1);");
    ok("argument:named", "cube(size = 1, center = true);");
}

#[test]
fn empty_program_is_no_statements() {
    // `input : /*empty*/` (parser.y:174) — a hole the inventory owed a dedicated anchor.
    assert!(parse("").expect("empty parses").stmts.is_empty());
    assert!(
        parse("   \n\t  ")
            .expect("whitespace-only parses")
            .stmts
            .is_empty()
    );
}

#[test]
fn eot_marker_is_an_empty_statement() {
    // `statement : TOK_EOT` (parser.y:215) — the ETX (\x03) streamed-input end marker parses to an
    // empty statement, not an error.
    let prog = parse("\u{3}").expect("EOT parses");
    assert!(matches!(prog.stmts.as_slice(), [s] if matches!(s.kind, StmtKind::Empty)));
}
