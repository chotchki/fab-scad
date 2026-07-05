//! The scad-rs parser: a lexed token stream → a [`Program`] AST.
//!
//! Hand-written recursive-descent over winnow's `TokenSlice` (the second phase of the two-phase
//! design). Operator precedence is REPRODUCED from OpenSCAD's `parser.y` — which has no precedence
//! table, only a structural rule cascade (ternary → binary-climb → unary → exponent → call →
//! primary), so we mirror that cascade in [`expr`]. Errors are winnow-native (ContextError +
//! StrContext + cut_err at commit points); the caret diagnostic is rendered by [`diag`] because a
//! `ParseError` over a token stream can't draw one itself (it lacks the source `&str`).
//!
//! Scope is the G.3.3 tracer bullet (see [`ast`]); deferred constructs fail LOUD, never silently.

mod ast;
mod diag;
mod expr;
mod print;
mod stmt;

pub use ast::{
    Arg, BinOp, Expr, ExprKind, Modifiers, ModuleInstantiation, Parameter, Program, Span, Stmt,
    StmtKind, UnOp,
};
pub use print::{print, print_expr};

use winnow::Parser;
use winnow::combinator::{cut_err, fail};
use winnow::error::{ContextError, ErrMode, ModalResult, StrContext, StrContextValue};
use winnow::stream::TokenSlice;
use winnow::token::any;

use crate::lexer::{Token, TokenKind};

/// Parser input: lexed tokens with byte-span tracking (via `impl Location for Token`).
pub(crate) type Tokens<'t, 's> = TokenSlice<'t, Token<'s>>;

/// Max recursive-descent nesting depth. Bounds parse nesting so pathological input (`[[[[…`,
/// `((((…`) errors LOUD instead of overflowing the host stack — the fuzz doctrine (no panic, no
/// hang) applies to the parser too. Set CONSERVATIVELY: each nesting level costs ~8 cascade frames,
/// so this must keep the deepest guarded recursion under the SMALLEST target stack (wasm ≈ 1 MiB,
/// test threads ≈ 2 MiB) — not the 8 MiB main thread. 64 is far above any real model's nesting;
/// true stack-independence would need the iterative (explicit-stack) parser deferred to I.2's
/// engine work. (Deep left-CHAINS are handled separately by the non-recursive `Drop` for `Expr`.)
pub(crate) const MAX_DEPTH: usize = 64;

// Byte spans flow from tokens to AST nodes: with these two methods `TokenSlice<_, Token>: Location`,
// so `i.current_token_start()` / `i.previous_token_end()` (and `.with_span()`) yield BYTE offsets.
impl winnow::stream::Location for Token<'_> {
    fn previous_token_end(&self) -> usize {
        self.span.end
    }
    fn current_token_start(&self) -> usize {
        self.span.start
    }
}

/// Peek the next token's kind without consuming (`None` at end of stream — before the `Eof`? no:
/// the stream ends WITH `Eof`, so this returns `Some(Eof)` there and `None` only past it).
pub(crate) fn peek_kind<'s>(i: &Tokens<'_, 's>) -> Option<TokenKind<'s>> {
    i.first().map(|t| t.kind)
}

/// Peek the kind of the token AFTER next — the one lookahead we need (assignment `id =` vs call `id (`).
pub(crate) fn peek_kind2<'s>(i: &Tokens<'_, 's>) -> Option<TokenKind<'s>> {
    i.get(1).map(|t| t.kind)
}

/// Consume the next token unconditionally, returning it. Callers peek first, so this succeeds; the
/// `Eof` guard means it is never called on an empty stream in practice.
pub(crate) fn bump<'t, 's>(i: &mut Tokens<'t, 's>) -> ModalResult<&'t Token<'s>> {
    any.parse_next(i)
}

/// A parser that consumes one token whose kind equals `k` — for unit-variant kinds (punctuation,
/// keywords). Payload kinds (Num/Str/Ident) are matched by peeking + [`bump`] instead.
pub(crate) fn tok<'t, 's>(
    k: TokenKind<'s>,
) -> impl Parser<Tokens<'t, 's>, &'t Token<'s>, ErrMode<ContextError>>
where
    's: 't,
{
    any.verify(move |t: &Token<'s>| t.kind == k)
}

/// Fail LOUD and unrecoverably (Cut) at the current position with a labeled diagnostic — used for
/// unexpected tokens, deferred constructs (H.2/H.3), and the depth guard. Always returns `Err`.
pub(crate) fn bail<T>(i: &mut Tokens<'_, '_>, label: &'static str) -> ModalResult<T> {
    cut_err(fail.context(StrContext::Label(label))).parse_next(i)
}

/// Consume a required token at a COMMIT point — a mismatch is a HARD (Cut) error naming what was
/// expected, so an enclosing alternative can't swallow it and mislocate the caret.
pub(crate) fn expect<'s>(
    i: &mut Tokens<'_, 's>,
    k: TokenKind<'s>,
    expected: &'static str,
) -> ModalResult<()> {
    cut_err(tok(k).context(StrContext::Expected(StrContextValue::Description(expected))))
        .void()
        .parse_next(i)
}

/// Parse OpenSCAD source into a [`Program`] AST.
///
/// # Errors
/// [`Error::Parse`](crate::Error::Parse) with a caret diagnostic on a syntax error OR a
/// recognized-but-deferred construct (module/function defs, `if`/`else`, `use`, list
/// comprehensions — H.2/H.3), and propagates the lexer's [`Error::Parse`] on a bad token.
pub fn parse(source: &str) -> crate::Result<Program> {
    let lexed = crate::lex(source)?;
    let tokens = lexed.code;
    stmt::program
        .parse(TokenSlice::new(&tokens))
        .map_err(|e| crate::Error::Parse(diag::render(&e, source, &tokens)))
}
