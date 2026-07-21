//! The crate's public error type.
//!
//! Three failure stages — parse, evaluate, lower — plus a LOUD "not yet implemented" for
//! deferred constructs and tracer-bullet stubs (SPEC: deferred features blow up, never wrong
//! silently). `#[non_exhaustive]` because the payloads gain structure as phases land: the `Parse`
//! variant will carry a caret-rendered winnow diagnostic (G.3.3), not a bespoke error tree — the
//! parser stays winnow-native.

use thiserror::Error;

/// The crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure somewhere in the parse → evaluate → lower pipeline.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Source failed to parse. Payload is a human-rendered diagnostic (a plain message until
    /// G.3.3 wires winnow's context stack + spans into caret output).
    #[error("parse error:\n{0}")]
    Parse(String),

    /// A well-formed program failed at evaluation time (arity, undef misuse, …).
    #[error("evaluation error: {0}")]
    Eval(String),

    /// A user `assert` (module or expression form) failed. Distinct from [`Eval`](Error::Eval) because
    /// OpenSCAD prints the assert ERROR but STILL exports the top-level geometry accumulated BEFORE the
    /// failing statement — so the geometry driver catches THIS specifically to warn + halt + keep what it has
    /// (L.5.8), matching the oracle's partial render. A genuine `Eval` fault stays fatal. Display is identical
    /// to `Eval` so console/log text ("evaluation error: assertion failed …") is unchanged.
    #[error("evaluation error: {0}")]
    Assert(String),

    /// A CSG node could not be lowered to a `kernel::Solid`.
    #[error("geometry error: {0}")]
    Lower(String),

    /// A `use`/`include` target could not be resolved or read — bad path, missing library, or an
    /// I/O failure reading a resolved file. OpenSCAD WARNS and renders on without the file; we fail
    /// LOUD instead (never-silently-wrong doctrine — a missing lib in a correct corpus is a
    /// resolution BUG on our side, and we want it loud). Revisit once I.5's warning buffer lands and
    /// we can match the oracle's warn-and-continue bug-for-bug.
    #[error("load error: {0}")]
    Load(String),

    /// A deferred construct or an unbuilt pipeline stage was reached — fail LOUD, never silently
    /// wrong (SPEC deferral doctrine; `text()`/`minkowski()`/`surface()` land here).
    #[error("not yet implemented: {0}")]
    Unimplemented(&'static str),

    /// A call to a name we don't recognize — not a user function/module, not a builtin. The payload
    /// NAMES the symbol (e.g. "function foo" / "module bar"). Distinct from `Unimplemented` (a KNOWN
    /// construct we deliberately deferred): this is a missing builtin or a typo. OpenSCAD warns +
    /// returns `undef` (I.5); we fail LOUD for now — and naming the symbol turns the BOSL2 corpus's
    /// one generic "unknown function" cluster into a per-symbol burn-down worklist (L.2).
    #[error("unknown {0}")]
    Unknown(String),

    /// A fault stamped with the SOURCE SPAN of the TOP-LEVEL construct that triggered it (W.3.37): the eval
    /// driver + the top-level hoist wrap the error as it unwinds past the failing statement / assignment, so
    /// a caller can map the span to the user's line and point the editor at it. Display DELEGATES to the
    /// inner error, so the console/log message text is byte-for-byte unchanged — the span rides alongside,
    /// invisible to text consumers, read via [`Error::span`].
    #[error("{source}")]
    Spanned {
        /// Byte range into the eval'd source of the failing top-level construct.
        span: core::ops::Range<usize>,
        /// The underlying fault, whose message this delegates to.
        #[source]
        source: Box<Error>,
    },
}

impl Error {
    /// Stamp `span` onto this error, IF it doesn't already carry one. The innermost stamp wins (the two
    /// top-level seams — hoist + geometry driver — never nest, so a later outer stamp is a harmless no-op),
    /// and an already-`Spanned` error passes through unchanged. Never wraps an [`Assert`](Error::Assert)
    /// naked so the driver's L.5.8 warn-and-continue match still sees a raw `Assert` — callers stamp only
    /// the fatal (non-Assert) paths.
    #[must_use]
    pub fn at(self, span: core::ops::Range<usize>) -> Error {
        match self {
            Error::Spanned { .. } => self,
            other => Error::Spanned {
                span,
                source: Box::new(other),
            },
        }
    }

    /// The source span stamped onto this error (the failing top-level construct), if any.
    #[must_use]
    pub fn span(&self) -> Option<core::ops::Range<usize>> {
        match self {
            Error::Spanned { span, .. } => Some(span.clone()),
            _ => None,
        }
    }

    /// The underlying fault, peeling any [`Spanned`](Error::Spanned) wrapper — match on THIS to classify by
    /// variant (the corpus bucketing) without a `Spanned` arm swallowing every stamped error into the
    /// catch-all. Display already delegates, so text consumers never need it.
    #[must_use]
    pub fn root(&self) -> &Error {
        match self {
            Error::Spanned { source, .. } => source.root(),
            other => other,
        }
    }
}
