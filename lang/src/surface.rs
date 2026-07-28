//! AR.14.1 — the LIBRARY SURFACE: what a library declares, in the one place every consumer can see.
//!
//! chotchki, settling the shape: *"the fab-lib-bosl2 library should be exporting the implementation
//! of a library surface trait that includes constants/functions/modules, that trait is what the
//! fuzzer/fab-gui/etc should be consuming."*
//!
//! Before this there were THREE descriptions of the same thing, maintained separately: `fab_gen`'s
//! `Decl` (the fuzzer's view), `intrinsics::surface::SurfaceFn` (derived from references at runtime,
//! then leaked to obtain `'static`), and `intrinsics::Entry` (the dispatch view). Three descriptions
//! of one library is three things to keep in agreement, and AR.5a is what that costs — three of the
//! hand-maintained guard lists were simply wrong.
//!
//! This module is in fab-lang rather than fab-gen for a dependency reason, not a taste one: every
//! crate that needs it already depends on fab-lang, and a GENERATED library crate must not have to
//! depend on the fuzzer to describe itself.
//!
//! TWO LISTS, NOT ONE WITH AN `Option`. [`LibrarySurface::callables`] is pure DECLARATION — what
//! exists and how to call it — and [`LibrarySurface::natives`] is IMPLEMENTATION — what we compiled
//! and what has to be true for it to be legal. They are different lengths ON PURPOSE: BOSL2 declares
//! 1335 functions and the emitter compiles 742 of them. Folding them into one list with an optional
//! function pointer would assert those are the same set, which is exactly the drift this phase
//! exists to kill.
//!
//! Declaring MODULES earns something before a single module is transpiled: the fuzzer can generate
//! calls against BOSL2's 416-module surface as soon as it is declared. That coverage is not gated on
//! whether modules ever get natives.

use crate::eval::value::Value;

/// What KIND of value a parameter accepts — the type information a call generator needs in order to
/// produce a call that does WORK rather than one that returns `undef`.
///
/// This is the AR.4 trap made structural: a wrongly-typed argument costs nothing, renders nothing and
/// times as ~0, so a domain-blind corpus measures ERROR HANDLING while looking like it measures
/// geometry — and the failure is invisible, because the programs still run, still agree with the
/// oracle and still report a ratio.
///
/// `Any` is used HONESTLY, for builtins that genuinely accept anything (the `is_*` predicates,
/// `str`) — not as a shrug. A guessed domain would generate confidently wrong calls, which is worse
/// than a general one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// A scalar.
    Num,
    /// A POSITIVE integer — `sqrt`/`ln` (a negative is instant NaN) and `chr` (a codepoint).
    Pos,
    /// A number in `[-1, 1]` — `asin`/`acos`'s real domain; anything wider is NaN half the time.
    Unit,
    /// Degrees — a `Num`, but flagged so a generator can stay in a sane angular range.
    Deg,
    /// A boolean.
    Bool,
    /// A string.
    Str,
    /// A numeric vector of EXACTLY 3 — `cross`, where mismatched lengths are undef and 2-vectors
    /// change the return type to a scalar.
    Vec3,
    /// A numeric vector of any length.
    VecN,
    /// A flat list of anything.
    List,
    /// A `[[key, value], …]` pairs table, keys ascending — `lookup`'s pairs, and the shape
    /// `search` indexes by column.
    Table,
    /// Genuinely any value.
    Any,
}

/// May a call RETURNING `ret` stand where `want` is expected? The scalar domains nest — a `Unit`,
/// `Pos` or `Deg` value IS a `Num`, and any scalar is a usable angle — which is what lets trig
/// compose (`sin(acos(…))`: `acos` returns degrees, `sin` wants them). Deliberately asymmetric:
/// a `Num` is not a `Unit`, and nothing stands in for `Pos` (no builtin's return is provably
/// positive AND integral — `chr` needs a codepoint, not `exp`'s 20.08).
#[must_use]
pub fn satisfies(ret: Domain, want: Domain) -> bool {
    match want {
        Domain::Num | Domain::Deg => {
            matches!(ret, Domain::Num | Domain::Deg | Domain::Unit | Domain::Pos)
        }
        _ => ret == want,
    }
}

/// Function or module — a module takes CHILDREN, a function does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Callable in expression position.
    Function,
    /// Callable in statement position, may take children.
    Module,
}

/// One parameter of a declared callable.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    /// The declared name. DECORATIVE on builtins — upstream discards builtin argument names and binds
    /// POSITIONALLY (`pow(exp=3, base=2)` is 9, not 8; `sin(bogus=30)` is accepted). It is load-bearing
    /// only where [`Decl::names_bind`], i.e. on user-defined functions, which is what BOSL2 is.
    pub name: &'static str,
    /// What this parameter accepts.
    pub domain: Domain,
    /// No default — AN.3's case. An unfilled defaultless param must be `undef` and must NOT fall
    /// through to a like-named global, so a generator that never OMITS an argument cannot catch that
    /// regression.
    pub required: bool,
}

/// One callable a surface hosts, as DECLARED rather than rediscovered by each consumer.
///
/// See `docs/transpiler-design.md`. The point of declaring it once: the AO fuzzer, AR.1's
/// transpiled-library fuzzing, and the dispatch registry all want the same facts, and
/// `intrinsics::Entry` already carries the DISPATCH half (may this native be used here?) while
/// nothing carries the CALL half (how do I build a call that works?).
#[derive(Clone, Copy, Debug)]
pub struct Decl {
    /// The callable's name.
    pub name: &'static str,
    /// Function or module.
    pub kind: Kind,
    /// What a WELL-TYPED call returns — what makes calls COMPOSE. A generator needing a `Num` can
    /// nest any `Num`-returning call (`sin(acos(…))`), which is where eval work comes from; without
    /// this field every argument bottoms out at a literal after one hop. Declared conservatively:
    /// `sin`/`cos` return [`Domain::Unit`], `asin`..`atan2` return degrees.
    pub ret: Domain,
    /// Do parameter NAMES bind? FALSE for builtins (verified against the oracle — see [`Param::name`]),
    /// true for user/library functions. The whole AN.1/AN.2/AN.3/AN.14 diagnostic family is
    /// unreachable when this is false, so a generator that ignores the flag will believe it has
    /// covered the named-argument path without ever exercising it.
    pub names_bind: bool,
    /// The parameters, in DECLARATION order — which is the order positional args fill.
    pub params: &'static [Param],
}

impl Decl {
    /// How many arguments a generator should supply. Note several builtins (`str`, `concat`, `min`,
    /// `max`) are genuinely VARIADIC upstream; the surface pins them at the arity the corpus has
    /// always generated, so this stays a generation choice rather than a claim about the language.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.params.len()
    }
}

/// One top-level constant a library declares.
///
/// The value is a `fn()` rather than a stored `Value` because `Value` holds `Rc`s and so cannot sit
/// in a `static`. Same shape `intrinsics::ValueConst` already uses for its guard values, and the
/// same reason.
#[derive(Clone, Copy)]
pub struct ConstDecl {
    /// The declared name, as the library's own source spells it (`_EPSILON`, `UP`, `_NO_ARG`).
    pub name: &'static str,
    /// The value the library binds it to, built fresh on each call.
    pub value: fn() -> Value,
}

impl std::fmt::Debug for ConstDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `fn()` has no useful Debug; print what identifies the constant instead.
        f.debug_struct("ConstDecl")
            .field("name", &self.name)
            .field("value", &(self.value)())
            .finish()
    }
}

/// What one library declares, and what of it we compiled.
///
/// Implemented by a transpiled crate (`fab-lib-bosl2`) as STATIC data — a generated library
/// declares itself at build time, which is what lets the old runtime derivation and its leak go
/// away — and by `fab_gen::Builtins`, which declares OpenSCAD's own builtins and has no constants
/// and no natives.
pub trait LibrarySurface: Send + Sync {
    /// How the library identifies itself in diagnostics (`"BOSL2"`).
    fn name(&self) -> &'static str;

    /// The ROOTS this surface was built from — the paths a consumer writes in its `include` line.
    ///
    /// PROVENANCE, per chotchki: *"the registry needs to know what root emitted what, so when a
    /// downstream consumer includes it, it gets what it asked for, which make include more than
    /// just itself."* A root brings its whole transitive closure — `BOSL2/std.scad` is 934
    /// functions, not the handful that file itself declares — and the closures COMPOSE rather than
    /// nest, because BOSL2's opt-in files do not include `std.scad` back.
    ///
    /// Empty for a surface that is not include-based, like the builtins.
    fn roots(&self) -> &'static [Root] {
        &[]
    }

    /// SCAD that must precede a generated call — the library's own `include` lines. Empty for
    /// builtins, which need no preamble. Without this a generated program calls functions that do
    /// not exist and every seed fails identically on both legs of a differential, which LOOKS like
    /// agreement.
    fn preamble(&self) -> &'static str {
        ""
    }

    /// The top-level constants the library binds. The raw material for a native's baked values, and
    /// the reason each one still needs a guard: the fingerprint proves the FUNCTION, never the
    /// constants it names, so a user's `_EPSILON = 1e-6;` has to disarm every native that baked the
    /// old value.
    fn constants(&self) -> &'static [ConstDecl] {
        &[]
    }

    /// Everything callable, functions AND modules, in a stable order.
    ///
    /// `'static` on purpose: the generator holds the slice for its whole walk and indexes it with
    /// the RNG, so a borrowed surface would push a lifetime through every generator method for a
    /// table that is built once. Order is part of the contract — reordering silently re-points
    /// every accumulated fuzz corpus at different programs.
    fn callables(&self) -> &'static [Decl];
}

/// One include root and what it brings into scope. See [`LibrarySurface::roots`].
#[derive(Clone, Copy, Debug)]
pub struct Root {
    /// The path as a consumer writes it: `"BOSL2/std.scad"`.
    pub path: &'static str,
    /// The names this root's closure reaches, sorted. A consumer that includes only this root may
    /// call these and nothing else — a generated program that calls `spur_gear` having included
    /// only `std.scad` is broken, and a missing function costs a silently absent PART, not an error.
    pub declares: &'static [&'static str],
}

/// A function's structural identity: a hash over its `(params, body)` AST shape with SPANS
/// EXCLUDED, so it survives reformatting and comment edits but not a semantic change.
///
/// A NEWTYPE rather than a bare `u64` deliberately. This value is the entire basis on which a
/// native is allowed to stand in for a library function, and it travels through registry lookups,
/// dep anchors and the sustainment matrix — all of which also carry ordinary `u64`s. Nothing but
/// the type system stops one being passed where the other belongs, and the failure would be a
/// native wiring against a function it does not implement, which is a wrong ANSWER rather than a
/// missed speedup.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(u64);

impl Fingerprint {
    /// Wrap a hash that was computed by the fingerprint walk. Not public API surface for anyone
    /// else: constructing one from an arbitrary `u64` is exactly the confusion the newtype exists
    /// to prevent, which is why this is crate-internal.
    pub(crate) const fn new(bits: u64) -> Self {
        Self(bits)
    }

    /// The underlying bits, for serialization and diagnostics only.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hex, because a fingerprint is compared and quoted, never read as a quantity.
        write!(f, "fp:{:016x}", self.0)
    }
}

impl std::fmt::Display for Fingerprint {
    /// `0x` + 16 hex digits — BYTE-IDENTICAL to the `{:#018x}` every call site used before the
    /// newtype existed. Deliberate: the type change is a refactor, and a refactor that quietly
    /// reformats the sustainment matrix and the `FAB_EXPLAIN` report is a refactor whose output
    /// nobody can diff against the previous run.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}
