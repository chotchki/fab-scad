//! The crate's public error type.
//!
//! Three failure stages — parse, evaluate, lower — plus a LOUD "not yet implemented" for
//! deferred constructs and tracer-bullet stubs (SPEC: deferred features blow up, never wrong
//! silently). `#[non_exhaustive]` because the payloads gain structure as phases land: the `Parse`
//! variant will carry a caret-rendered winnow diagnostic (G.3.3), not a bespoke error tree — the
//! parser stays winnow-native.

use thiserror::Error;

use crate::eval::Message;

/// The crate result alias — the INTERIOR one, unchanged. Every `?` in the evaluator keeps propagating a
/// bare [`Error`]; only the public entry points widen to [`RunResult`], so threading the console cost the
/// interior nothing.
pub type Result<T> = std::result::Result<T, Error>;

/// The result of a whole evaluation RUN: the value plus its console on success, a [`Failure`] carrying the
/// fault AND the console it had already printed on failure.
pub type RunResult<T> = std::result::Result<T, Failure>;

/// A fault plus the console output that preceded it.
///
/// The console is an OUTPUT of evaluation, not a reward for succeeding. Before this existed, warnings and
/// echoes lived in a `Ctx` side buffer that only escaped on the success path — so the moment a `?` fired,
/// everything the program had printed was discarded. Callers then could not tell "this program printed
/// nothing" from "this program failed", which is precisely how `differ`'s `unwrap_or_default()` managed to
/// hide a whole class of divergence (AN.18).
///
/// A STRUCT wrapping [`Error`], deliberately not a new `Error` VARIANT. The variant would have been a
/// smaller diff and a worse one: `Error` is `#[non_exhaustive]`, so every `match` over it already ends in a
/// `_ =>` arm, and a new variant slides into those catch-alls silently — mis-bucketing without a compile
/// error, exactly the trap `gen/src/main.rs` was already sitting in. A new TYPE makes every call site that
/// must now say `.error` a compile failure instead. Loud beats small.
///
/// The `ERROR:` console line is NOT stored here — [`Failure::console`] derives it from `error`, so the fault
/// has one representation rather than two that can drift.
#[derive(Debug)]
pub struct Failure {
    /// The fault that stopped the run.
    pub error: Error,
    /// Echo + warnings emitted BEFORE the fault, in order. Never contains the fault itself.
    pub messages: Vec<Message>,
}

impl Failure {
    /// The full console this run produced, fault included as the final line.
    ///
    /// Appending is faithful rather than a simplification: upstream's `ERROR:` is TERMINAL — evaluation
    /// halts, at most one is ever printed, nothing follows it, and no geometry is exported (verified
    /// against the binary: a failing assert inside `for (i = [0:3])` prints two echoes, one ERROR, then
    /// stops). A `WARNING:` is the opposite — any number, interleaved, and the render still completes.
    #[must_use]
    pub fn console(&self) -> Vec<Message> {
        let mut out = self.messages.clone();
        out.push(Message::Error(self.error.to_string()));
        out
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Lets a bare [`Error`] cross a `?` into a [`RunResult`] — with an EMPTY console, since a raw error is by
/// definition one nobody had a console to attach. The seam that owns the `Ctx` attaches the real one; this
/// impl exists so the paths that genuinely have no console (loader/IO failures before eval starts) stay
/// one-line `?` propagation instead of ceremony.
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Failure {
            error,
            messages: Vec::new(),
        }
    }
}

/// Drop the console back off a [`Failure`], for the entry points that don't deal in one.
///
/// The `evaluate_geometry` / `resolve_geometry_*` family returns geometry ALONE — it discards the console
/// on success, so discarding it on failure is the same contract, not a loss. Callers that want it ask for
/// the `_full` sibling, which is exactly the choice they already make today.
impl From<Failure> for Error {
    fn from(failure: Failure) -> Self {
        failure.error
    }
}

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
