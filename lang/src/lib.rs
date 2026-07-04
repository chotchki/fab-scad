//! scad-rs — the OpenSCAD language, in Rust, over the Manifold kernel.
//!
//! Stock `.scad` in, the same mesh OpenSCAD produces out. Not a dialect, not a new language: a
//! reimplementation whose correctness is DEFINED against stock OpenSCAD running stock BOSL2 (the
//! differential oracle). `SPEC.md` carries the why; the short version:
//!
//! - **One module everywhere.** A `wasm32-unknown-unknown` front end kills the emscripten
//!   two-module split the official wasm forces on us.
//! - **The Safari recursion cliff dies by construction.** Evaluation runs on an EXPLICIT stack,
//!   so recursion is bounded by memory — not by whatever JS engine's frame budget we land under.
//! - **Caching, the customizer, and per-node progress are ours**, because evaluation happens in
//!   our process against an AST we own.
//!
//! # Pipeline
//!
//! ```text
//! source ──parse──▶ AST ──eval──▶ CSG node tree ──lower──▶ kernel::Solid (Manifold)
//! ```
//!
//! [`evaluate`] is the tracer-bullet spine tying those stages together. Each fills in as Phase G
//! lands (parse: G.3.2/3, eval: G.3.4, lower: G.3.5); until a stage exists the spine returns
//! [`Error::Unimplemented`] — LOUD, never silently wrong (SPEC deferral doctrine).
//!
//! # Observability
//!
//! Parse decisions and evaluation are each observable from their idiomatic tool, both free in
//! release. The parser leans on winnow's OWN tooling first — every named production wraps in
//! winnow's `trace()` (gated behind the `trace` feature → `winnow/debug`); we only reach for the
//! [`tracing`] crate on the parse side if winnow's isn't enough. The evaluator is the tracing
//! crate's home turf: spans on the call path that double as the per-call benchmark corpus. In
//! release those spans compile out when the linking binary sets `tracing`'s `release_max_level_off`
//! — compile-out-like-a-logger, the doctrine both sides share.
//!
//! License: GPL-2.0-or-later — OpenSCAD's exact license, on purpose (frictionless upstreaming; v3
//! rules apply in distributed builds via Apache-2.0 Manifold). See `README.md`.

mod error;
mod eval;
mod lexer;
mod mesh;
mod parser;

pub use error::{Error, Result};
pub use eval::{Scope, Value, eval_expr, fragments};
pub use lexer::{Lexed, Token, TokenKind, decode_str, lex, num_value};
pub use mesh::Mesh;
pub use parser::{
    Arg, BinOp, Expr, ExprKind, Modifiers, ModuleInstantiation, Program, Span, Stmt, StmtKind,
    UnOp, parse,
};

/// Evaluate OpenSCAD source to a triangle [`Mesh`] — the end-to-end tracer-bullet spine.
///
/// Currently a stub: the parse/eval/lower stages land across Phase G, wired end to end for
/// `sphere()`/`cube()`/`cylinder()` at G.3.5.
///
/// # Errors
///
/// Returns [`Error::Unimplemented`] until the pipeline is wired. Thereafter: [`Error::Parse`] for
/// malformed source, [`Error::Eval`] for a well-formed program that fails at runtime, and
/// [`Error::Lower`] when a CSG node cannot be realized as geometry.
pub fn evaluate(source: &str) -> Result<Mesh> {
    parse(source)?; // stages 1-2 (G.3.2 lex + G.3.3 parse), surfacing Error::Parse on bad source
    // Evaluator `tracing` spans (the per-call benchmark corpus) arrive with the real evaluator at
    // G.3.4/I.6 — instrumenting this stub would only add uncoverable disabled-span branches.
    Err(Error::Unimplemented(
        "evaluate: eval + lower stages land in Phase G",
    ))
}

#[cfg(test)]
mod tests {
    use super::{Error, evaluate};

    #[test]
    fn evaluate_is_a_loud_stub() {
        // The tracer-bullet spine exists and fails LOUD, not silent, until Phase G wires it.
        let err = evaluate("sphere(1);").unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
    }

    #[test]
    fn evaluate_surfaces_lex_errors() {
        // Malformed source fails at the now-wired lex stage — not the Unimplemented stub.
        let err = evaluate("\"unterminated").unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }
}
