//! Rendering a token-stream parse failure into a caret diagnostic against the ORIGINAL source.
//!
//! winnow's built-in caret renderer needs `I: AsBStr`, which a `TokenSlice` isn't — so this is ours.
//! `ParseError::offset()` over a token stream is a TOKEN INDEX; we map it to a byte offset through
//! the token spans, then draw the offending source line with a `^` and the accumulated context.

use winnow::error::{ContextError, ParseError};

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
    // ContextError's Display is "invalid <label>\nexpected <values>" — reuse it as the message body.
    format!("{gutter}{line_text}\n{pad}^\n{}", e.inner())
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
