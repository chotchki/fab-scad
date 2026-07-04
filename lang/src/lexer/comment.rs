//! Comment lexing — PRESERVED (the customizer needs them). Non-nesting block comments.
//!
//! Line `//` runs to (not including) the newline; block `/* */` closes on the FIRST `*/` (flex
//! block comments do not nest — lexer.l:205-219). Comments become tokens (kept in [`Lexed::all`],
//! filtered from [`Lexed::code`]) so H.4 can bind them to parameters.
//!
//! [`Lexed::all`]: super::Lexed::all
//! [`Lexed::code`]: super::Lexed::code

use winnow::Parser;
use winnow::combinator::{alt, cut_err, not, repeat, trace};
use winnow::error::{ModalResult, StrContext, StrContextValue};
use winnow::token::{any, literal, take_while};

use super::Input;
use super::token::TokenKind;

/// Dispatch target for `/`: `//` line comment, `/* */` block comment, or the bare `Slash` operator.
/// The two-char openers are tried before lone `/` — longest-match.
pub(crate) fn lex_slash_or_comment<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace(
        "slash-or-comment",
        alt((
            line_comment,
            block_comment,
            literal("/").value(TokenKind::Slash),
        )),
    )
    .parse_next(i)
}

/// `// …` up to (not including) the newline; the raw slice keeps the leading `//`. EOF is clean —
/// `take_while` simply stops.
fn line_comment<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace(
        "line-comment",
        ("//", take_while(0.., |c: char| c != '\n'))
            .take()
            .map(TokenKind::LineComment),
    )
    .parse_next(i)
}

/// `/* … */`, non-nesting (first `*/` closes). Unterminated ⇒ HARD error via `cut_err` — without
/// it, an unterminated `/*…EOF` would backtrack to a bare `Slash` + `Star`, silently wrong.
fn block_comment<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("block-comment", |i: &mut Input<'s>| {
        (
            "/*",
            repeat::<_, _, (), _, _>(0.., (not("*/"), any)).take(),
            cut_err(
                literal("*/").context(StrContext::Expected(StrContextValue::StringLiteral("*/"))),
            ),
        )
            .take()
            .map(TokenKind::BlockComment)
            .context(StrContext::Label("block comment"))
            .parse_next(i)
    })
    .parse_next(i)
}
