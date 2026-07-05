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
pub use eval::{Scope, Value, eval_expr, eval_program, fragments};
pub use lexer::{Lexed, Token, TokenKind, decode_str, lex, num_value};
pub use mesh::Mesh;
pub use parser::{
    Arg, BinOp, Expr, ExprKind, Modifiers, ModuleInstantiation, Parameter, Program, Span, Stmt,
    StmtKind, UnOp, parse,
};

/// Evaluate OpenSCAD source to a triangle [`Mesh`] — the end-to-end tracer-bullet spine.
///
/// Wired end to end for the G.3.5 subset (`sphere`/`cube`/`cylinder`, expressions, `$fn/$fa/$fs`);
/// everything past that fails LOUD ([`Error::Unimplemented`]) — transforms, booleans, user modules,
/// multiple top-level objects, functions, and so on land across the later phases.
///
/// # Errors
///
/// [`Error::Parse`] for malformed source, and [`Error::Unimplemented`] for a well-formed program
/// that uses a construct beyond the G.3.5 subset.
pub fn evaluate(source: &str) -> Result<Mesh> {
    let program = parse(source)?; // G.3.2 lex → G.3.3 parse
    eval_program(&program, &Scope::new()) // G.3.4 eval → G.3.5 tessellate to a Mesh
}

#[cfg(test)]
mod tests {
    use super::{Error, evaluate};

    #[test]
    fn evaluate_produces_a_mesh() {
        // The tracer-bullet spine reaches geometry: source → a real triangle mesh.
        let mesh = evaluate("sphere(5, $fn = 8);").expect("sphere evaluates");
        assert!(mesh.tri_count() > 0 && mesh.vert_count() > 0);
    }

    #[test]
    fn evaluate_defers_transforms_loud() {
        // Beyond the G.3.5 subset (a transform) → LOUD, never silently wrong.
        let err = evaluate("translate([1,0,0]) cube(1);").unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
    }

    #[test]
    fn evaluate_surfaces_parse_errors() {
        let err = evaluate("\"unterminated").unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "got {err:?}");
    }
}
