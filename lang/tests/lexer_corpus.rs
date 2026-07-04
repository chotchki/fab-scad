//! G.3.2 lexer conformance corpus — asserted against OpenSCAD's `lexer.l` semantics.
//!
//! Each case cites the lexer.l provenance it pins. These snippets are ALSO the seed corpus for the
//! cargo-fuzz target (H.6). The never-panics + lossless-round-trip properties here are the
//! hand-written stand-ins for the proptest strategies H.5 will generalize.

#![allow(
    clippy::enum_glob_use,
    reason = "token-kind assertions read far better with bare variant names than TokenKind::-qualified"
)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration-test helpers: expect/unwrap ARE the assertions; clippy's allow-*-in-tests doesn't reach non-#[test] helper fns"
)]

use fab_lang::TokenKind::*;
use fab_lang::{TokenKind, decode_str, lex, num_value};

/// Token kinds of the CODE view (comments filtered), WITHOUT the trailing `Eof` — readable asserts.
fn kinds(src: &str) -> Vec<TokenKind<'_>> {
    let lexed = lex(src).expect("expected clean lex");
    lexed
        .code
        .iter()
        .filter(|t| t.kind != Eof)
        .map(|t| t.kind)
        .collect()
}

fn assert_kinds(src: &str, expected: &[TokenKind<'_>]) {
    assert_eq!(kinds(src).as_slice(), expected, "for source {src:?}");
}

// ─────────────────────────────── numbers (lexer.l:287-322) ───────────────────────────────

#[test]
fn hex_lowercase_only() {
    assert_kinds("0x1F", &[Num("0x1F")]); // #1 lowercase hex
    assert_kinds("0X1F", &[Ident("0X1F")]); // #2 uppercase 0X is NOT hex -> deprecated id
}

#[test]
fn floats_leading_and_trailing_dot() {
    assert_kinds(".5e-3", &[Num(".5e-3")]); // #3 leading dot + signed exponent
    assert_kinds("5.", &[Num("5.")]); // #4 trailing dot
    assert_kinds("5.e3", &[Num("5.e3")]); // #5 trailing dot + exponent
    assert_kinds("1e5", &[Num("1e5")]); // #6 exponent-only float
    assert_kinds("1.5", &[Num("1.5")]); // #11 ordinary float
}

#[test]
fn number_vs_digit_leading_id_longest_match() {
    assert_kinds("1e", &[Ident("1e")]); // #7 no digit after e -> id (2 > 1)
    assert_kinds("1e5x", &[Ident("1e5x")]); // #8 float 3 < id 4 -> id
    assert_kinds("123abc", &[Ident("123abc")]); // #9 digit-leading id
    assert_kinds("1..5", &[Num("1."), Num(".5")]); // #10 no `..` token; two floats
}

#[test]
fn unary_minus_is_its_own_token() {
    assert_kinds("x=-1", &[Ident("x"), Eq, Minus, Num("1")]); // #12
    assert_kinds("x =- 1", &[Ident("x"), Eq, Minus, Num("1")]); // #13 no `=-` operator
}

#[test]
fn num_value_matches_openscad() {
    assert!((num_value("0x1F") - 31.0).abs() < 1e-9);
    assert!((num_value("5.") - 5.0).abs() < 1e-9);
    assert!((num_value("5.e3") - 5000.0).abs() < 1e-6);
    assert!((num_value("1e5") - 100_000.0).abs() < 1e-6);
    assert!((num_value(".5e-3") - 0.0005).abs() < 1e-12);
    assert!((num_value("1.5") - 1.5).abs() < 1e-12);
}

// ─────────────────────────────── operators (longest-match) ───────────────────────────────

#[test]
fn multi_char_operators_beat_prefixes() {
    assert_kinds("a<=b<c", &[Ident("a"), Le, Ident("b"), Lt, Ident("c")]); // #14
    assert_kinds("a<<b", &[Ident("a"), Shl, Ident("b")]); // #15
    assert_kinds("==!=&&||>>>=", &[EqEq, Ne, AndAnd, OrOr, Shr, Ge]);
    assert_kinds("a.b", &[Ident("a"), Dot, Ident("b")]); // bare dot is member access
}

// ─────────────────────────────── strings + escapes (lexer.l:183-197) ─────────────────────

#[test]
fn string_escapes_decode() {
    assert_eq!(decode_str(r"aéb"), "aéb"); // #16 \u 4-hex -> é
    assert_eq!(decode_str(r"\x41\x00Z"), "A Z"); // #17 \x41=A, \x00=SPACE (not NUL)
    assert_eq!(decode_str(r"bad \q escape"), "bad q escape"); // #18 undefined escape drops backslash
    assert_eq!(decode_str("line1\nline2"), "line1line2"); // #19 raw newline DROPPED
    assert_eq!(decode_str(r"\U01F600"), "😀"); // \U reads EXACTLY 6 hex -> U+1F600
    assert_eq!(decode_str(r"\U0001F600"), "Ƕ00"); // 6 hex = U+01F6, then literal "00" (bug-for-bug)
    assert_eq!(decode_str(r"tab\tnl\n"), "tab\tnl\n");
    assert_eq!(decode_str(r"\r"), "\r"); // carriage return
    assert_eq!(decode_str(r"\\"), "\\"); // escaped backslash -> one backslash
    assert_eq!(decode_str(r#"\""#), "\""); // escaped quote -> one quote
}

#[test]
fn string_token_captures_raw_body() {
    // The token holds the RAW body between the quotes; escapes are applied only by decode_str.
    assert_kinds(r#""aéb""#, &[Str(r"aéb")]);
    assert_kinds(r#""plain""#, &[Str("plain")]);
}

// ─────────────────────────── identifiers / keywords / $-vars ─────────────────────────────

#[test]
fn keyword_only_as_whole_lexeme() {
    assert_kinds("module", &[Module]);
    assert_kinds("modulefoo", &[Ident("modulefoo")]); // #24 keyword needs whole-lexeme match
    assert_kinds(
        "for(i=[0:5])",
        &[
            For,
            LParen,
            Ident("i"),
            Eq,
            LBracket,
            Num("0"),
            Colon,
            Num("5"),
            RBracket,
            RParen,
        ],
    );
}

#[test]
fn every_keyword_lexes() {
    assert_kinds(
        "function if else let assert echo for each true false undef module",
        &[
            Function, If, Else, Let, Assert, Echo, For, Each, True, False, Undef, Module,
        ],
    );
}

#[test]
fn decode_and_num_edge_paths() {
    // num_value robustness on the public API (defensive fallbacks).
    assert!(num_value("not-a-number").is_nan()); // non-number -> NaN guard
    assert!(num_value("0xFFFFFFFFFFFFFFFFF").is_finite()); // 17 hex digits overflow u64 -> saturate
    // decode_str edge escapes (lexer.l fallbacks).
    assert_eq!(decode_str(r"\"), ""); // dangling backslash -> nothing
    assert_eq!(decode_str(r"\x8F"), "x8F"); // first hex digit not octal -> undefined-escape fallback
    assert_eq!(decode_str(r"\x4"), "x4"); // too few chars for \x -> fallback
    assert_eq!(decode_str(r"\x00"), " "); // \x00 -> SPACE
    assert_eq!(decode_str(r"\u12"), "u12"); // too few hex for \u -> fallback keeps 'u'
    assert_eq!(decode_str(r"\uD800"), ""); // lone surrogate -> char::from_u32 None -> nothing pushed
}

#[test]
fn dollar_vars_and_id_splitting() {
    // #23: $fn is a DollarIdent; `$` terminates a preceding id, so a$b splits.
    assert_kinds(
        "$fn=8; a$b",
        &[
            DollarIdent("$fn"),
            Eq,
            Num("8"),
            Semi,
            Ident("a"),
            DollarIdent("$b"),
        ],
    );
    assert_kinds("$", &[DollarIdent("$")]); // lone `$` is a legal identifier
}

// ─────────────────────────── use / include context-sensitivity ──────────────────────────

#[test]
fn use_include_filename_tokens() {
    assert_kinds("use <MCAD/gears.scad>", &[Use("MCAD/gears.scad")]); // #20 path kept whole
    assert_kinds("include <../lib/foo.scad>", &[Include("../lib/foo.scad")]); // #21
    assert_kinds("use <with space.scad>", &[Use("with space.scad")]); // SPACE allowed in path
    assert_kinds("use<no_space.scad>", &[Use("no_space.scad")]); // zero whitespace before `<`
}

#[test]
fn use_without_angle_is_identifier() {
    // #22: `use`/`include` are keywords ONLY before `<`; otherwise plain identifiers.
    assert_kinds("use x;", &[Ident("use"), Ident("x"), Semi]);
    assert_kinds(
        "a = include + 1;",
        &[Ident("a"), Eq, Ident("include"), Plus, Num("1"), Semi],
    );
}

// ─────────────────────────────── comments PRESERVED ─────────────────────────────────────

#[test]
fn comments_kept_in_all_filtered_from_code() {
    let lexed = lex("x = 10; // [1:100] radius").expect("clean"); // #29 customizer annotation
    // comment absent from the parser view...
    assert_eq!(
        lexed
            .code
            .iter()
            .map(|t| t.kind)
            .filter(|k| *k != Eof)
            .collect::<Vec<_>>(),
        vec![Ident("x"), Eq, Num("10"), Semi],
    );
    // ...but present, raw, in the lossless view for H.4.
    assert!(
        lexed
            .all
            .iter()
            .any(|t| t.kind == LineComment("// [1:100] radius"))
    );
}

#[test]
fn block_comments_do_not_nest() {
    // #27: first */ closes; the trailing ` */` re-lexes as Star, Slash.
    assert_kinds("/* /* */ */", &[Star, Slash]);
    let lexed = lex("/* /* */ */").expect("clean");
    assert!(lexed.all.iter().any(|t| t.kind == BlockComment("/* /* */")));
}

#[test]
fn line_comment_at_eof_is_clean() {
    let lexed = lex("// eof-comment").expect("clean"); // #28 no trailing newline -> no error
    assert!(lexed.all.iter().any(|t| matches!(t.kind, LineComment(_))));
    assert_eq!(kinds("// eof-comment"), Vec::<TokenKind>::new()); // no code tokens
}

// ─────────────────────────────── Unicode / UTF-8 (code points) ──────────────────────────

#[test]
fn utf8_passes_through_strings_and_comments() {
    // Raw multibyte UTF-8 in a string body survives verbatim (winnow steps code points, not bytes).
    assert_kinds(r#""héllo→世界""#, &[Str("héllo→世界")]);
    assert_eq!(decode_str("héllo→世界"), "héllo→世界");
    // Multibyte inside a line comment is preserved too.
    let lexed = lex("// café ☕").expect("clean");
    assert!(
        lexed
            .all
            .iter()
            .any(|t| t.kind == LineComment("// café ☕"))
    );
}

#[test]
fn bom_and_nbsp_are_skipped() {
    assert_kinds("\u{FEFF}x", &[Ident("x")]); // #30 BOM skipped
    assert_kinds("a\u{A0}b", &[Ident("a"), Ident("b")]); // U+00A0 nbsp is trivia
}

#[test]
fn non_ascii_identifier_is_a_hard_error() {
    // OpenSCAD rejects non-ASCII identifier chars (TOK_ERROR); we do too — but LOUD, never a panic.
    let err = lex("é = 1;").expect_err("non-ASCII ident must error");
    assert!(matches!(err, fab_lang::Error::Parse(_)), "got {err:?}");
}

// ─────────────────────────────── error paths (LOUD, never silent) ───────────────────────

#[test]
fn unterminated_constructs_error_with_context() {
    let e = lex("/* unterminated").expect_err("unterminated block comment"); // #25
    assert!(e.to_string().contains("*/"), "want '*/' in {e}");

    let e = lex("\"unterminated").expect_err("unterminated string"); // #26
    assert!(e.to_string().contains('"'), "want quote in {e}");

    let e = lex("use <no-close").expect_err("unterminated use path");
    assert!(e.to_string().contains('>'), "want '>' in {e}");

    let e = lex("include <no-close").expect_err("unterminated include path");
    assert!(e.to_string().contains('>'), "want '>' in {e}");
}

// ─────────────────────────────── properties (H.5 will generalize) ───────────────────────

/// Lossless: every source byte is accounted for by exactly one token span or an inter-token gap.
fn assert_roundtrip(src: &str) {
    let lexed = lex(src).expect("clean");
    let mut out = String::new();
    let mut pos = 0;
    for t in &lexed.all {
        if t.kind == Eof {
            break;
        }
        out.push_str(&src[pos..t.span.start]); // skipped trivia
        out.push_str(&src[t.span.clone()]); // the token itself
        pos = t.span.end;
    }
    out.push_str(&src[pos..]); // trailing trivia
    assert_eq!(out, src, "round-trip mismatch");
}

#[test]
fn lossless_roundtrip_over_corpus() {
    for src in [
        "x = 10; // radius\nsphere(r = 5);",
        "/* a */ module m() { cube([1, 2, 3]); }",
        "use <MCAD/gears.scad>\ninclude <lib.scad>",
        "a<=b && c!=d || e<<2",
        "v = [for (i = [0:2:10]) i * $fn];",
        ".5e-3 + 0x1F - 5.",
    ] {
        assert_roundtrip(src);
    }
}

#[test]
fn never_panics_on_adversarial_input() {
    // The fuzz invariant (H.6 scales it up): bytes in -> Ok or Err, never a panic, never a hang.
    for src in [
        "",
        "\0",
        "\u{3}",
        "\\",
        "\"",
        "/*",
        "*/",
        "//",
        "0x",
        "0xG",
        ".",
        "..",
        "1e+",
        "use",
        "use<",
        "use <",
        "include <",
        "$",
        "$$",
        "\"\\",
        "\"\\x",
        "\"\\u12",
        "{[(<",
        "\u{FEFF}",
        "é",
        "1.2.3",
        "\"\n\"",
        "// x",
        "/* /* */",
    ] {
        let _ = lex(src); // must return, not panic
    }
}

#[test]
fn lexing_is_deterministic() {
    let src = "module m() { for (i = [0:$fn]) sphere(r = i * .5e-3); } // note";
    let a = lex(src).expect("clean");
    let b = lex(src).expect("clean");
    assert_eq!(
        a.all
            .iter()
            .map(|t| (t.kind, t.span.clone()))
            .collect::<Vec<_>>(),
        b.all
            .iter()
            .map(|t| (t.kind, t.span.clone()))
            .collect::<Vec<_>>(),
    );
}
