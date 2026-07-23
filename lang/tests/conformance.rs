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

/// Assert a snippet is REJECTED — for shapes upstream also hard-errors on (verdict parity).
#[track_caller]
fn rejected(production: &str, src: &str) {
    assert!(
        parse(src).is_err(),
        "production {production} — {src:?} must NOT parse (upstream errors too)"
    );
}

/// AA.2 (issue1890): an unterminated `include <`/`use <` at EOF swallows the REST OF THE FILE as
/// path content and the program still parses — one directive statement, nothing after it (the
/// `sphere()` never becomes a statement). Oracle-verified: upstream accepts + discards the same way.
#[test]
fn unterminated_include_swallows_to_eof() {
    let p = parse("include <file.scad\n\nsphere();\n").expect("parses");
    assert_eq!(p.stmts.len(), 1, "the swallowed sphere is path, not a stmt");
    match &p.stmts[0].kind {
        StmtKind::Include(path) => assert!(path.contains("sphere()"), "path {path:?}"),
        k => panic!("expected Include, got {k:?}"),
    }
}

/// AA.2's other half: unterminated COMMENT and STRING stay HARD parse errors — upstream errors on
/// both ("Unterminated comment" / "Unterminated string"), so rejection IS the parity.
#[test]
fn unterminated_comment_and_string_stay_rejected() {
    rejected("comment:unterminated", "/* comment\n\nsphere();\n");
    rejected("string:unterminated", "a = \"text;\n\ntext(a);\n");
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
    // `if` IS a module_instantiation (parser.y:271), so the `! # % *` prefixes apply to it — the
    // AA.1 sustainment-census gap (openscad's disable/highlight/background-modifier corpus files).
    ok("ifelse_statement:'*'", "*if (x) a();");
    ok("ifelse_statement:'!'", "!if (x) a();");
    ok("ifelse_statement:'#'", "#if (x) a(); else b();");
    ok("ifelse_statement:'%'", "%if (x) a(); else b();");
    ok("ifelse_statement:stacked-mods", "%*if (x) a();");
    ok("ifelse_statement:spaced-mod", "* if (x) a();");

    // ── include/use path lexing (AA.3, linenumber.scad) — `>` is the ONLY terminator inside `<…>` ──
    ok("include:space-in-path", "use <line 1> include <line 1>");
    ok("include:newline-before-bracket", "include\n<a.scad>");
    ok(
        "include:newlines-inside-brackets",
        "include\n< line 6\nline 7\nline 8\n>\ncube(1);",
    );
    ok("use:newlines-inside-brackets", "use\n< a\nb\n>");
    // Unterminated `<…` at EOF (AA.2, issue1890): upstream consumes to EOF and discards the
    // directive with NO parse error (oracle-verified) — we accept it too, the rest of the file
    // becoming path content. (Unterminated COMMENT and STRING stay hard parse errors — upstream
    // errors on those as well: "Unterminated comment"/"Unterminated string".)
    ok(
        "include:unterminated-at-eof",
        "include <file.scad\n\nsphere();\n",
    );
    ok("use:unterminated-at-eof", "use <file.scad\n\nsphere();\n");
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

/// AA.5: non-UTF-8 source decodes via the Latin-1 fallback at the fs seam — a raw `0xA0` NBSP byte
/// becomes U+00A0, which lexes as whitespace (oracle-verified on the nbsp twin tests: `ECHO: 1`,
/// clean run). fab-lang's core stays str/UTF-8; only the byte→text decode is lenient.
#[test]
fn latin1_source_decodes_and_parses() {
    let latin1: Vec<u8> = b"a\xa0=\xa01;\xa0//\xa0nbsp\necho(a);\n".to_vec();
    let src = fab_lang::decode_scad_source(latin1);
    assert!(src.contains('\u{a0}'), "0xA0 maps to U+00A0, not U+FFFD");
    let p = parse(&src).expect("latin-1 NBSP source parses (NBSP is whitespace)");
    assert_eq!(p.stmts.len(), 2);
}

/// AA.4 coverage sweep: statement-depth overflow INTO an expression position — 63 chained module
/// calls put the ARG at the expr entry's depth limit, so the spine's statement-tier guard (its one
/// depth bail) fires with the "expression" wording, not the module-chain one.
#[test]
fn expr_entry_depth_guard_fires_from_deep_statement_chains() {
    let src = format!("{}b(1);", "a() ".repeat(63));
    let e = parse(&src).expect_err("must bail");
    assert!(format!("{e}").contains("deeply"), "got: {e}");
}

/// AC.1: `use <font.ttf>` is a FONT registration upstream, not scad source — our loader contributes
/// the empty program SILENTLY (no can't-open, no parse-failed warning; `text()` draws the bundled
/// Liberation face regardless, per the determinism doctrine). Pinned against a real dummy file so
/// the resolve path runs, not the tolerated-missing path.
#[test]
fn use_of_a_font_file_is_a_silent_no_op() {
    let dir = std::env::temp_dir().join(format!("fab-font-noop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk temp");
    std::fs::write(dir.join("fake-font.ttf"), b"\x00\x01\x00\x00not-scad").expect("write ttf");
    let scad = dir.join("uses-font.scad");
    std::fs::write(&scad, "use <fake-font.ttf>\ncube(1);").expect("write scad");
    let (geo, messages) = fab_lang::resolve_geometry_with_base_full(
        &std::fs::read_to_string(&scad).expect("read"),
        &dir,
        &[],
        None,
        fab_lang::Config::default(),
        |_| Err(fab_lang::Error::Load("no meshes in this test".into())),
    )
    .expect("evaluates");
    assert!(!geo.is_null(), "the cube renders");
    assert!(
        messages.is_empty(),
        "font use is SILENT (upstream registers silently): {messages:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
