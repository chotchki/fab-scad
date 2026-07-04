//! The winnow lexer: OpenSCAD source → a spanned [`Lexed`] token stream.
//!
//! Two-phase by design (SPEC + winnow's `TokenSlice`): this stage emits `Vec<Token>`; G.3.3 runs
//! the parser over the [`Lexed::code`] view. Comments are PRESERVED as tokens (kept in
//! [`Lexed::all`]) so the customizer (H.4) can bind them to parameters.
//!
//! **Unicode:** we operate on Unicode CODE POINTS, not bytes — the input is `&str` (UTF-8) and
//! winnow steps it `char` by `char`. Spans are byte offsets, always on char boundaries. The
//! *grammar* (identifiers/keywords/numbers) is ASCII, enforced on code points exactly as OpenSCAD
//! does; string bodies and comments carry arbitrary UTF-8 verbatim. Non-UTF-8 source is rejected at
//! the `&str` boundary by the caller, not tokenized (a deliberate divergence from flex's byte scan).
//!
//! Conformance reference: OpenSCAD `src/core/lexer.l`. Every named production is wrapped in winnow's
//! `trace()` — the parser's native observability, zero-cost unless the `trace` feature (→
//! `winnow/debug`) is on.

mod chars;
mod comment;
mod number;
mod string;
mod token;
mod word;

pub use number::num_value;
pub use string::decode_str;
pub use token::{Lexed, Token, TokenKind};

use winnow::Parser;
use winnow::combinator::{alt, cut_err, dispatch, fail, peek, preceded, repeat, terminated, trace};
use winnow::error::{ContextError, ErrMode, ModalResult, StrContext};
use winnow::stream::LocatingSlice;
use winnow::token::{any, literal, take_while};

use chars::is_ident_start;
use comment::lex_slash_or_comment;
use number::{lex_digit_start, lex_dot};
use string::lex_string;
use word::lex_word;

/// The lexer's winnow input: `&str` with byte-offset tracking for `.with_span()`.
pub(crate) type Input<'s> = LocatingSlice<&'s str>;

/// Match a fixed lexeme and yield a unit [`TokenKind`].
fn lit<'s>(
    s: &'static str,
    k: TokenKind<'s>,
) -> impl Parser<Input<'s>, TokenKind<'s>, ErrMode<ContextError>> {
    literal(s).value(k)
}

/// One token = its kind bracketed by `.with_span()`, so every token gets its span in one place.
fn lex_token<'s>(i: &mut Input<'s>) -> ModalResult<Token<'s>> {
    lex_kind
        .with_span()
        .map(|(kind, span)| Token { kind, span })
        .parse_next(i)
}

/// The top-level token dispatch. `peek(any)` reads the discriminator char without consuming; each
/// arm re-parses and consumes. Multi-char operators are `alt`'d before their single-char prefixes,
/// so first-match yields longest-match (`<=`/`<<` beat `<`). A bad byte is `cut_err` — a HARD error
/// at the right offset, not a backtrack read as end-of-stream.
fn lex_kind<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    #[allow(
        clippy::enum_glob_use,
        reason = "the dispatch table is a wall of token variants; the glob keeps it legible and is scoped to this fn"
    )]
    use TokenKind::*;
    trace("token", |i: &mut Input<'s>| {
        dispatch! { peek(any);
            '0'..='9' => lex_digit_start,
            '.' => lex_dot,
            '"' => lex_string,
            '/' => lex_slash_or_comment,
            c if is_ident_start(c) => lex_word,
            '<' => alt((lit("<=", Le), lit("<<", Shl), lit("<", Lt))),
            '>' => alt((lit(">=", Ge), lit(">>", Shr), lit(">", Gt))),
            '=' => alt((lit("==", EqEq), lit("=", Eq))),
            '!' => alt((lit("!=", Ne), lit("!", Bang))),
            '&' => alt((lit("&&", AndAnd), lit("&", Amp))),
            '|' => alt((lit("||", OrOr), lit("|", Pipe))),
            '+' => lit("+", Plus),
            '-' => lit("-", Minus),
            '*' => lit("*", Star),
            '%' => lit("%", Percent),
            '^' => lit("^", Caret),
            '~' => lit("~", Tilde),
            '?' => lit("?", Question),
            ':' => lit(":", Colon),
            ',' => lit(",", Comma),
            ';' => lit(";", Semi),
            '(' => lit("(", LParen),
            ')' => lit(")", RParen),
            '[' => lit("[", LBracket),
            ']' => lit("]", RBracket),
            '{' => lit("{", LBrace),
            '}' => lit("}", RBrace),
            '#' => lit("#", Hash),
            '\u{3}' => lit("\u{3}", Eot),
            _ => cut_err(fail).context(StrContext::Label("token")),
        }
        .parse_next(i)
    })
    .parse_next(i)
}

/// Skip inter-token whitespace (NOT emitted as tokens — recoverable from span gaps) plus the
/// invisible-space code points OpenSCAD ignores anywhere: U+00A0 (nbsp) and U+FEFF (BOM/ZWNBSP).
fn skip_trivia_ws(i: &mut Input<'_>) -> ModalResult<()> {
    trace(
        "trivia-ws",
        take_while(0.., |c: char| {
            matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{A0}' | '\u{FEFF}')
        })
        .void(),
    )
    .parse_next(i)
}

/// Lex the whole source into an ordered `Vec<Token>` (whitespace skipped, comments kept). The
/// trace-tree root: `lex-all` → one `token` node per lexeme.
fn lex_all<'s>(i: &mut Input<'s>) -> ModalResult<Vec<Token<'s>>> {
    trace(
        "lex-all",
        terminated(
            repeat(0.., preceded(skip_trivia_ws, lex_token)),
            skip_trivia_ws,
        ),
    )
    .parse_next(i)
}

/// Lex OpenSCAD source into a lossless token stream plus the parser's comment-free view.
///
/// Pure and deterministic; allocates only the two output `Vec`s (tokens borrow `source`).
///
/// # Errors
/// Returns [`Error::Parse`](crate::Error::Parse) with a caret-rendered diagnostic on an
/// unterminated string / block comment / `use`/`include` path, or an unexpected byte. Deprecated
/// digit-leading identifiers and undefined string escapes are WARNINGS (not yet surfaced) — they
/// still tokenize.
pub fn lex(source: &str) -> crate::Result<Lexed<'_>> {
    let mut all = lex_all
        .parse(LocatingSlice::new(source))
        .map_err(|e| crate::Error::Parse(e.to_string()))?;
    all.push(Token {
        kind: TokenKind::Eof,
        span: source.len()..source.len(),
    });
    let code = all
        .iter()
        .filter(|t| !t.kind.is_comment())
        .cloned()
        .collect();
    Ok(Lexed { all, code })
}
