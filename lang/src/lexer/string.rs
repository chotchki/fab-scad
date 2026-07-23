//! String-literal lexing (capture the RAW body) + [`decode_str`] (apply escapes on demand).
//!
//! The lexer keeps the raw body so the customizer (H.4) can rewrite source; escapes are applied
//! only when a value is needed. Escape semantics: lexer.l:183-197.

use winnow::Parser;
use winnow::combinator::{alt, cut_err, repeat, trace};
use winnow::error::{ModalResult, StrContext, StrContextValue};
use winnow::token::{any, literal, none_of};

use super::Input;
use super::token::TokenKind;

/// Lex a `"…"` string: capture the RAW body (escapes applied later by [`decode_str`]). Past the
/// opening quote the closing quote is a `cut_err` — an unterminated string is a HARD error.
pub(crate) fn lex_string<'s>(i: &mut Input<'s>) -> ModalResult<TokenKind<'s>> {
    trace("string", |i: &mut Input<'s>| {
        literal("\"").parse_next(i)?; // opening quote — commit point
        let body = string_body_raw.parse_next(i)?;
        cut_err(literal("\"").context(StrContext::Expected(StrContextValue::CharLiteral('"'))))
            .context(StrContext::Label("string literal"))
            .parse_next(i)?;
        Ok(TokenKind::Str(body))
    })
    .parse_next(i)
}

/// Everything up to the unescaped closing quote, RAW. `('\\', any)` swallows an escaped pair whole
/// (so `\"` doesn't end the scan); `none_of(['\\','"'])` takes normal bytes (including a raw
/// newline, which [`decode_str`] later drops). EOF ⇒ both arms fail, `repeat` stops, the caller's
/// `cut_err` fires "expected `\"`".
fn string_body_raw<'s>(i: &mut Input<'s>) -> ModalResult<&'s str> {
    trace(
        "string-body",
        repeat::<_, _, (), _, _>(0.., alt((('\\', any).void(), none_of(['\\', '"']).void())))
            .take(),
    )
    .parse_next(i)
}

/// Decode a raw string body to its value, applying OpenSCAD's escapes (lexer.l:183-197).
///
/// `\n \t \r \\ \"` → their bytes; `\x[0-7]{H}` → one ASCII byte (`\x00` → SPACE, per flex); `\u{H}{4}`
/// / `\U{H}{6}` → a UTF-8 codepoint; an UNDEFINED escape drops the backslash and keeps the next char
/// (`\q` → `q`); a RAW (unescaped) newline is DROPPED. Exact malformed-escape conformance is I.5.
#[must_use]
pub fn decode_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c != '\n' {
                out.push(c); // raw newline (c == '\n') is dropped (lexer.l:193)
            }
            continue;
        }
        let Some(e) = chars.next() else { break }; // dangling backslash: unreachable via the lexer
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'x' => decode_hex_byte(&mut chars, &mut out),
            'u' => decode_unicode(&mut chars, &mut out, 4, 'u'),
            'U' => decode_unicode(&mut chars, &mut out, 6, 'U'),
            other => out.push(other), // undefined escape: drop the backslash, keep the char
        }
    }
    out
}

/// `\x[0-7]{H}`: exactly two chars, first octal (so value ≤ 0x7F, ASCII). `\x00` → SPACE. A
/// malformed `\x` falls back to the undefined-escape rule (keep the `x`).
fn decode_hex_byte(chars: &mut core::str::Chars<'_>, out: &mut String) {
    let mut probe = chars.clone();
    match (probe.next(), probe.next()) {
        (Some(d1), Some(d2)) if d1.is_digit(8) && d2.is_ascii_hexdigit() => {
            *chars = probe;
            // d1 octal (0..=7), d2 hex (0..=15) => byte in 0..=0x7F: always a valid ASCII char, so
            // `char::from(u8)` is total — no fallback branch.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "d1 is octal so the value is <= 0x7F and fits u8 losslessly"
            )]
            let byte = (d1.to_digit(8).unwrap_or(0) * 16 + d2.to_digit(16).unwrap_or(0)) as u8;
            out.push(if byte == 0 { ' ' } else { char::from(byte) });
        }
        _ => out.push('x'),
    }
}

/// `\u{H}{4}` / `\U{H}{6}`: exactly `n` hex digits → a codepoint. Malformed → keep `esc`.
fn decode_unicode(chars: &mut core::str::Chars<'_>, out: &mut String, n: usize, esc: char) {
    let mut probe = chars.clone();
    let mut val: u32 = 0;
    for _ in 0..n {
        let Some(d) = probe.next().and_then(|c| c.to_digit(16)) else {
            out.push(esc);
            return;
        };
        val = val * 16 + d;
    }
    *chars = probe;
    // Codepoint 0 becomes a SPACE, same as `\x00` above (AH.2.2, the unicode-tests golden):
    // upstream can't carry a NUL through its C strings and substitutes ' '.
    if val == 0 {
        out.push(' ');
    } else if let Some(ch) = char::from_u32(val) {
        out.push(ch);
    }
}
