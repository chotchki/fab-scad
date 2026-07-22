//! Identifier / keyword / `$`-var lexing, and the context-sensitive `use`/`include <path>`.
//!
//! Lex the whole `{IDSTART}{IDREST}*` lexeme, THEN classify by exact whole-lexeme match — that's
//! flex's longest-match-then-rule-order, so `modulefoo` is an `Ident`, not `module` + `foo`.
//! `use`/`include` are keywords ONLY when directly followed (modulo whitespace) by `<`.

use winnow::Parser;
use winnow::combinator::{cut_err, opt, peek, trace};
use winnow::error::{ModalResult, StrContext, StrContextValue};
use winnow::token::{literal, one_of, take_while};

use super::Input;
use super::chars::{is_ident_continue, is_ident_start, is_use_ws};
use super::token::TokenKind;

/// Dispatch target for an identifier-start char: lex the lexeme, classify by exact match.
pub(crate) fn lex_word<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("word", |i: &mut Input<'s>| {
        let word: &str = (one_of(is_ident_start), take_while(0.., is_ident_continue))
            .take()
            .parse_next(i)?;
        Ok(match word {
            "module" => TokenKind::Module,
            "function" => TokenKind::Function,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "let" => TokenKind::Let,
            "assert" => TokenKind::Assert,
            "echo" => TokenKind::Echo,
            "for" => TokenKind::For,
            "each" => TokenKind::Each,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "undef" => TokenKind::Undef,
            "use" | "include" => return lex_use_or_include(word, i),
            _ if word.starts_with('$') => TokenKind::DollarIdent(word),
            _ => TokenKind::Ident(word),
        })
    })
    .parse_next(i)
}

/// `use`/`include` are keywords ONLY when the next non-whitespace char is `<` (flex `use[ \t\r\n]*"<"`);
/// otherwise a plain identifier. Inside `<…>` the ONLY terminator is `>` (AA.3, linenumber.scad):
/// SPACE, TAB and NEWLINES are all path content. Upstream's flex ignores `\t\r\n` here and keeps the
/// LAST newline-separated segment as the filename — while warning that newlines in `include<>` are
/// "not defined - behavior may change"; we keep the RAW slice instead (zero-copy, and the loader's
/// can't-open warning then shows exactly what the source said). Divergent only in the munging of a
/// construct upstream itself declares undefined. The WHOLE path is captured (no `/` split — that's
/// H.2). Unterminated ⇒ hard error via `cut_err` (recovery is AA.2's issue1890 territory).
fn lex_use_or_include<'s>(word: &'s str, i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("use-or-include", |i: &mut Input<'s>| {
        let is_file = opt(peek((take_while(0.., is_use_ws), '<')))
            .parse_next(i)?
            .is_some();
        if !is_file {
            return Ok(TokenKind::Ident(word)); // `use x;` → Ident("use")
        }
        (take_while(0.., is_use_ws), '<').void().parse_next(i)?; // consume ws + '<' — commit point
        let path = take_while(0.., |c: char| c != '>').parse_next(i)?;
        cut_err(literal(">").context(StrContext::Expected(StrContextValue::CharLiteral('>'))))
            .context(StrContext::Label(if word == "use" {
                "use path"
            } else {
                "include path"
            }))
            .parse_next(i)?;
        Ok(if word == "use" {
            TokenKind::Use(path)
        } else {
            TokenKind::Include(path)
        })
    })
    .parse_next(i)
}
