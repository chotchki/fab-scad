//! Rendering a token-stream parse failure into a caret diagnostic against the ORIGINAL source.
//!
//! winnow's built-in caret renderer needs `I: AsBStr`, which a `TokenSlice` isn't — so this is ours.
//! `ParseError::offset()` over a token stream is a TOKEN INDEX; we map it to a byte offset through
//! the token spans, then draw the offending source line with a `^` and the accumulated context.

use winnow::error::{ContextError, ParseError, StrContext};

use super::Tokens;
use crate::lexer::Token;

/// Render a parse failure as a `line | source` + caret diagnostic followed by the context stack.
pub(crate) fn render(
    e: &ParseError<Tokens<'_, '_>, ContextError>,
    source: &str,
    tokens: &[Token<'_>],
) -> String {
    // Token index → byte offset (past-the-end / Eof both map to the source length).
    let byte = tokens
        .get(e.offset())
        .map_or(source.len(), |t| t.span.start);
    let (line_no, col, line_text) = locate(source, byte);
    let gutter = format!("{line_no} | ");
    let pad = " ".repeat(gutter.len() + col);
    format!("{gutter}{line_text}\n{pad}^\n{}", explain(e.inner()))
}

/// Turn winnow's context STACK into a human line. The stack (innermost failure → outermost construct)
/// carries two kinds of context: EXPECTED items name what token would have let parsing continue, LABEL
/// items are the grammar breadcrumb (`an expression` ‹ `a call argument` ‹ …). We lead with the concrete
/// "expected …" when winnow recorded one, else name what we were trying to parse, then append the
/// breadcrumb so the reader sees WHERE in the grammar it went wrong — the "where's it going" context.
fn explain(inner: &ContextError) -> String {
    let mut expected = Vec::new();
    let mut labels = Vec::new();
    for c in inner.context() {
        match c {
            StrContext::Expected(v) => expected.push(v.to_string()),
            StrContext::Label(l) => labels.push(*l),
            // winnow's `StrContext` is `#[non_exhaustive]`; `Label`/`Expected` are the only variants we
            // (or it) build, so this arm is an unreachable-by-design forward-compat guard.
            _ => {}
        }
    }
    // Head: the concrete "expected …" (an OR list — any one alternative would satisfy the parser), else
    // name the construct we were trying to parse. `expected` and `labels` are never BOTH empty — every
    // parser failure goes through `bail`/`expect`/`labeled`, each of which attaches a label or expected.
    let head = if expected.is_empty() {
        format!(
            "expected {}",
            labels.first().copied().unwrap_or("valid syntax")
        )
    } else {
        format!("expected {}", expected.join(" or "))
    };
    // The breadcrumb: the grammar constructs we were inside, outermost → innermost. Skip the leaf label
    // when it already IS the head, so we don't say "expected an expression … while parsing an expression".
    let skip_leaf = expected.is_empty();
    let trail: Vec<&str> = labels
        .iter()
        .skip(usize::from(skip_leaf))
        .rev()
        .copied()
        .collect();
    if trail.is_empty() {
        head
    } else {
        format!("{head}\n  while parsing {}", trail.join(" › "))
    }
}

/// Byte offset → (1-based line number, 0-based CHARACTER column, the line's text). Character (not
/// byte) column so the caret lands correctly under multibyte UTF-8.
fn locate(source: &str, byte: usize) -> (usize, usize, &str) {
    let byte = byte.min(source.len());
    let before = &source[..byte];
    let line_start = before.rfind('\n').map_or(0, |nl| nl + 1);
    let line_no = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = source[line_start..byte].chars().count();
    let line_end = source[byte..]
        .find('\n')
        .map_or(source.len(), |nl| byte + nl);
    (line_no, col, &source[line_start..line_end])
}
