//! Numeric-literal lexing + the number-vs-digit-leading-identifier longest-match tiebreak.
//!
//! OpenSCAD number grammar (lexer.l:117-119, 287-322): hex `0x{H}+` (LOWERCASE `0x` only), floats
//! with a leading OR trailing dot and an optional `[Ee][+-]?{D}+` exponent, and plain decimals. NO
//! octal/binary literals, NO `_` digit separators. A bare `.` is the `Dot` operator; `0X1F` is NOT
//! hex (uppercase) — it becomes a deprecated digit-leading `Ident`.

use winnow::Parser;
use winnow::combinator::{alt, opt, peek, trace};
use winnow::error::ModalResult;
use winnow::token::{one_of, take_while};

use super::Input;
use super::chars::is_ident_continue;
use super::token::TokenKind;

/// Dispatch target for a `[0-9]`-leading lexeme: a number OR a deprecated digit-leading identifier.
///
/// Flex rule distilled: the digit-id (`{D}{IDREST}*`) wins **iff strictly longer** than the best
/// number match; on a tie or number-longer, the number wins (number rules precede the id rule, so
/// flex's earliest-rule tiebreak favors the number).
pub(crate) fn lex_digit_start<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("number-or-digit-id", |i: &mut Input<'s>| {
        let id_len = peek(take_while(1.., is_ident_continue))
            .parse_next(i)?
            .len();
        let num_len = peek(opt(recognize_number))
            .parse_next(i)?
            .map_or(0, str::len);
        if num_len >= id_len {
            recognize_number.map(TokenKind::Num).parse_next(i)
        } else {
            // Deprecated digit-leading identifier (lexer.l:325): a warning at I.5, a token now.
            take_while(1.., is_ident_continue)
                .map(TokenKind::Ident)
                .parse_next(i)
        }
    })
    .parse_next(i)
}

/// Dispatch target for `.`: a leading-dot float (`.5`, `.5e-3`) or the bare `Dot` operator.
pub(crate) fn lex_dot<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("dot", |i: &mut Input<'s>| {
        let is_float = opt(peek(('.', one_of(|c: char| c.is_ascii_digit()))))
            .parse_next(i)?
            .is_some();
        if is_float {
            recognize_float.map(TokenKind::Num).parse_next(i)
        } else {
            '.'.value(TokenKind::Dot).parse_next(i)
        }
    })
    .parse_next(i)
}

/// Longest number lexeme, tried in flex rule order: hex ⟶ float ⟶ decimal. First-match `alt`
/// yields the flex-longest here because hex/float only match when they can, and when they match
/// they are ≥ the decimal match.
fn recognize_number<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "num-lexeme",
        alt((recognize_hex, recognize_float, recognize_decimal)),
    )
    .parse_next(i)
}

/// `0x{H}+` — lowercase `0x` prefix then ≥1 hex digit.
fn recognize_hex<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "hex",
        ("0x", take_while(1.., |c: char| c.is_ascii_hexdigit())).take(),
    )
    .parse_next(i)
}

/// `{D}+` — one or more decimal digits.
fn recognize_decimal<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace("decimal", take_while(1.., |c: char| c.is_ascii_digit())).parse_next(i)
}

/// A dotted float (`{D}*.{D}*` with ≥1 digit on some side, optional exponent) OR an exponent-only
/// float (`{D}+{E}`). No leading sign — unary `-`/`+` is a SEPARATE token; a sign appears only
/// inside the exponent.
fn recognize_float<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace("float", alt((recognize_dotted, recognize_exp_only))).parse_next(i)
}

/// A dotted float, as flex's TWO rules (lexer.l:302-303) — expressed directly so there is no
/// "≥1 digit somewhere" guard branch: each alternative already mandates a digit, and a bare `.`
/// (which `lex_dot` intercepts anyway) matches neither.
fn recognize_dotted<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "dotted",
        alt((
            // {D}*\.{D}+{E}?  — optional leading, dot, ≥1 trailing (`.5`, `0.5`, `12.34`)
            (
                take_while(0.., |c: char| c.is_ascii_digit()),
                '.',
                take_while(1.., |c: char| c.is_ascii_digit()),
                opt(recognize_exp),
            )
                .take(),
            // {D}+\.{D}*{E}?  — ≥1 leading, dot, optional trailing (`5.`, `5.e3`)
            (
                take_while(1.., |c: char| c.is_ascii_digit()),
                '.',
                take_while(0.., |c: char| c.is_ascii_digit()),
                opt(recognize_exp),
            )
                .take(),
        )),
    )
    .parse_next(i)
}

/// `{D}+{E}` — digits then a MANDATORY exponent, no dot (`1e5`, `10E-3`).
fn recognize_exp_only<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "exp-only",
        (take_while(1.., |c: char| c.is_ascii_digit()), recognize_exp).take(),
    )
    .parse_next(i)
}

/// `[Ee][+-]?{D}+` — exponent marker, optional sign, ≥1 digit.
fn recognize_exp<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "exp",
        (
            one_of(['e', 'E']),
            opt(one_of(['+', '-'])),
            take_while(1.., |c: char| c.is_ascii_digit()),
        )
            .take(),
    )
    .parse_next(i)
}

/// Decode a [`TokenKind::Num`](super::TokenKind::Num) lexeme to `f64`, replicating OpenSCAD.
///
/// `0x…` parses base-16 (overflow saturates to `u64::MAX`, as `strtoull` does); everything else is
/// correctly-rounded `f64::from_str` (matching boost `lexical_cast<double>`).
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "OpenSCAD stores every number as f64; a hex literal past 2^53 loses precision there too — matched bug-for-bug"
)]
pub fn num_value(raw: &str) -> f64 {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_or(u64::MAX as f64, |v| v as f64)
    } else {
        // The lexeme already matched a number rule, so this is total; NaN is an unreachable guard.
        raw.parse::<f64>().unwrap_or(f64::NAN)
    }
}
