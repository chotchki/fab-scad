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
//! exists and how to call it — and [`LibrarySurface::rows`] is IMPLEMENTATION — what we compiled and
//! what has to be true for it to be legal, as the [`crate::registry::Rows`] a consumer accumulates.
//! They are different lengths ON PURPOSE: BOSL2 declares 1335 functions and the emitter compiles
//! 1072 of them. Folding them into one list with an optional function pointer would assert those are
//! the same set, which is exactly the drift this phase exists to kill.
//!
//! Declaring MODULES earns something before a single module is transpiled: the fuzzer can generate
//! calls against BOSL2's 416-module surface as soon as it is declared. That coverage is not gated on
//! whether modules ever get natives.

use crate::eval::geo2d::Geo;
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

/// The CAPABILITY a builtin needs from the evaluator — in the TYPE, not in a list beside it (AS.2).
///
/// The context builtins are not an exception to enumerate, they are a KIND: `textmetrics`/
/// `fontmetrics` and `object` need the argument NAMES every other builtin drops, `rands` needs the
/// run's one advancing `RandStream`, `parent_module` needs the live module-instantiation stack.
/// Before this enum that partition lived in FOUR hand-synced places — `run_builtin`'s if-chain, the
/// emitter's `CONTEXT_BUILTINS`, the JIT's `rands` special case, and `apply`'s missing arms — and
/// two of them disagreed about `rands` (the JIT wove the stream correctly while `rt::builtin`
/// answered `undef`). A consumer now matches on THIS and cannot re-derive the names wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinCapability {
    /// A pure function of its argument values — the only kind callable from generated code
    /// (`rt::bi`).
    Pure,
    /// Reads the argument NAMES (and may warn): `textmetrics`/`fontmetrics` (upstream's only
    /// builtins with DECLARED named parameters) and `object` (member names ARE the arg names).
    Named,
    /// Draws from the evaluator's one advancing `RandStream`: seedless `rands`, the ONE impure
    /// builtin.
    Stream,
    /// Reads the live module-instantiation stack: `parent_module`. Impure to the eval memo.
    Stack,
}

/// One OpenSCAD builtin as DECLARED once (AS.2) and consumed by everyone: the evaluator derives
/// membership + dispatch from the same rows, the emitter derives callability-from-generated-code,
/// the fuzzer derives its generation surface, and a conformance suite derives probe programs from
/// the parameter domains.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinDecl {
    /// The call surface: name, return domain, parameters with names + domains.
    pub decl: Decl,
    /// What the implementation needs from the evaluator.
    pub capability: BuiltinCapability,
}

pub use crate::eval::builtins::{BUILTIN_SURFACE, builtin_decl};

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

    /// What this library actually COMPILED, as opposed to what it declares — [`crate::registry::Rows`],
    /// the same rows a consumer accumulates into a [`crate::registry::Registry`].
    ///
    /// Deliberately a different list from [`LibrarySurface::callables`], and deliberately a
    /// different LENGTH: BOSL2 declares 1335 functions and the emitter compiles 1072 of them, plus
    /// 416 modules on their own curve. Folding the two into one list with an optional function
    /// pointer would assert they are the same set, which is exactly the drift this phase exists to
    /// kill — and it is the drift AR.5a found three times in the hand-maintained guard lists.
    ///
    /// Every row carries the guards that decide whether it may WIRE at all. None of that is
    /// optional: the reference proves the definition, the const guards prove the values it baked,
    /// and the dep/builtin guards prove nothing it calls has been shadowed.
    fn rows(&self) -> crate::registry::Rows {
        crate::registry::Rows::default()
    }

    /// Everything callable, functions AND modules, in a stable order.
    ///
    /// `'static` on purpose: the generator holds the slice for its whole walk and indexes it with
    /// the RNG, so a borrowed surface would push a lifetime through every generator method for a
    /// table that is built once. Order is part of the contract — reordering silently re-points
    /// every accumulated fuzz corpus at different programs.
    fn callables(&self) -> &'static [Decl];
}

/// THE RUN'S CONSOLE — what a native says, as opposed to what it computes. Both native shapes carry
/// it, which is the reason it is a supertrait rather than a method on each: `rt::binary`/`rt::unary`
/// are shared by the function and module emitters and must take whichever ctx the caller has.
///
/// The console is not a side channel here, it is half the answer. A tier that computed every value
/// correctly and printed nothing would still be WRONG, and it would be invisible to every mesh
/// comparison — which is exactly how `rt::apply_binary` came to discard the SV warning
/// `apply_binary_traced` produces, live since AR.6: a native computing `undef * undef` returned the
/// right value in silence while the interpreted twin printed `undefined operation (undefined *
/// undefined)`. Diff the echoes, not just the meshes.
pub trait Console {
    /// Emit `message` as a run warning, exactly where the interpreter would.
    fn warn(&self, message: String);

    /// Push an `echo` line through the evaluator's console. Args are (written name, value) pairs in
    /// SOURCE order — `$`-names included, formatted like any named arg — and the side effect lands
    /// BEFORE whatever the echo wraps: the interpreter's A3 order, which the I.5 string-equal
    /// console gate pins.
    ///
    /// One formatter behind it (`format_echo_pairs`), shared with the statement path and the
    /// expression task path, so the three consumers cannot drift about what an echo LOOKS like.
    ///
    /// # Errors
    /// Past the echo nesting guard (AD.5), exactly as the interpreted path errs.
    fn echo(&self, args: &[(Option<&'static str>, Value)]) -> crate::Result<()>;
}

/// What a generated FUNCTION native may ask of the evaluator (AR.17) — and it is exactly ONE
/// thing: invoke a function VALUE. A `Value::Function` carries a `closure_id` indexing the
/// evaluator's closure table — the body lives in evaluator context, not in the value — so a
/// ctx-less native structurally cannot call one, which is why computed callees (`f(a)(b)`),
/// function literals and the AN.10 local-binding shape all declined.
///
/// DELIBERATELY NARROW, the `ModuleCtx` discipline one level down: "takes a ctx" means precisely
/// "may call a closure", not "may reach the evaluator". A capability surface is bounded by what
/// the trait declares, not by whether a parameter exists. `&self` for `ModuleCtx`'s reason —
/// nested native calls (`f(fx, &[g(fx, x)?])`) fight the borrow checker under `&mut`, and the
/// evaluator's state is already behind `RefCell`/`Cell`.
pub trait FnCtx: Console {
    /// Invoke `callee` as a function with `args` (written names attached, `$`-names included),
    /// resolving through the interpreter's own `CallValue` machinery: letrec group re-injection,
    /// self LAST, defaults in the closure's lexical base, the name-less recursion verdict.
    ///
    /// # Errors
    /// Whatever the body raises, or a decline (`Unimplemented`) where no evaluator exists
    /// ([`NoClosures`]) — the whole call re-interprets, so the gap costs speed, never an answer.
    fn call_value(
        &self,
        callee: &Value,
        args: &[(Option<&'static str>, Value)],
    ) -> crate::Result<Value>;

    /// MINT a function value from the literal at `path` inside the fingerprint-proven definition
    /// of `def` (AR.17.2) — the creation half of the closure capability, `call_value`'s twin. The
    /// path is child indices under `parser::ast::expr_children`'s ordering, computed by the
    /// emitter against its parse of the reference; fingerprint equality makes it resolve to the
    /// structurally identical node in the program's own loaded definition. `captures` are the
    /// emitter-named free locals bound into a fresh child of the definition's island base —
    /// value AND call-position names, because the invoked body resolves both through the scope.
    /// `self_name` mirrors `name_closure`: the binder name when the literal is a binding's RHS,
    /// so self-recursion re-injects. Minting registers a FRESH closure-table entry per call —
    /// identity semantics (two mints compare unequal) and the memo caches' impurity signal both
    /// ride that, exactly as the interpreter's own literal evaluation does.
    ///
    /// # Errors
    /// A decline (`Unimplemented`) where no evaluator exists ([`NoClosures`]); a path that
    /// misses the proven definition is a fab BUG surfaced loudly, never a silent `undef`.
    fn mint_fn(
        &self,
        def: &str,
        path: &[usize],
        self_name: Option<&str>,
        captures: &[(&'static str, Value)],
    ) -> crate::Result<Value>;

    /// Re-interpret the NAMED, fingerprint-proven definition — the depth-decline fallback
    /// (AR.24). In the live evaluator the reference interprets with the intrinsic rung
    /// suppressed (one machine, explicit stack — per-level native re-entry would grow the Rust
    /// stack), closures mint into the REAL table, echoes land on the real console, and the memo
    /// caches see the interpreted twin's exact purity signals. With no evaluator
    /// ([`NoClosures`]) the batch's own `fallback_sources` interpret in a throwaway oracle
    /// instead — value-only; closures refuse that boundary loudly.
    ///
    /// # Errors
    /// Whatever the body raises — an assert propagates exactly as it would from the native.
    fn reinterpret(
        &self,
        name: &str,
        fallback_sources: &'static str,
        args: &[Value],
    ) -> crate::Result<Value>;

    /// Call the user function `name` with positional `args`, resolved AT RUNTIME against the
    /// running program — the outward-call capability (AR.27), and the function twin of what
    /// [`ModuleCtx::call`] has done for modules since AR.20.5.
    ///
    /// WHY IT EXISTS. A generated native calls its siblings with STATIC Rust calls, so the emitter
    /// can only compile a call whose callee it also compiled — and a function that declines takes
    /// its callers down with it. Measured on BOSL2: of 463 functions the fixpoint dropped, only
    /// ~88 declined for their own reasons and the rest were CASCADE. The module side never had
    /// that problem for one reason — it resolves callees through dispatch instead of inlining them
    /// — and this is that answer applied one tier down.
    ///
    /// IT IS ALSO THE STRICTER PATH, not a compromise. A static sibling call BAKES the callee's
    /// semantics, which is exactly why it needs a dep fingerprint pin; this resolves whatever the
    /// program actually defines, so it is right by construction and needs no pin at all. A user who
    /// redefines the callee gets their own version here, where a baked call would have handed them
    /// the library's.
    ///
    /// RESOLUTION ORDER IS THE INTERPRETER'S, and it must be: a user function, else a bound
    /// function VALUE of that name, else the unknown-function path — warn `Ignoring unknown
    /// function 'name'` and answer `undef`, which is what OpenSCAD does and what a corpus naming a
    /// newer-BOSL2 function depends on to render the rest.
    ///
    /// ARGUMENTS ARRIVE AS WRITTEN, names attached, and slot matching happens HERE against whatever
    /// the name actually resolved to. Positionalising at compile time would mean matching against
    /// the LIBRARY's parameter list for a callee that resolves against the USER's program — AN.10
    /// wearing a function's hat, and the same hazard AR.20.8 found and removed on the module side.
    /// The fix there was the same one: stop making a claim about the callee, and there is nothing
    /// left to violate.
    ///
    /// # Errors
    /// Whatever the callee's body raises. A decline (`Unimplemented`) where no evaluator exists
    /// ([`NoClosures`]) — the whole call re-interprets, so the gap costs speed, never an answer.
    fn call_named(
        &self,
        name: &str,
        args: &[(Option<&'static str>, Value)],
    ) -> crate::Result<Value>;
}

/// The [`FnCtx`] for call sites with NO evaluator — benches, oracles, value-level batteries. It
/// REFUSES loudly rather than answering `undef`: a native reaching a closure here is a visible
/// decline, not a silent wrong value.
pub struct NoClosures;

impl Console for NoClosures {
    fn warn(&self, _message: String) {
        // No run, no console. A bench or a value-level battery has nowhere to put this, and
        // inventing a sink would only hide that the caller has no evaluator.
    }

    fn echo(&self, _args: &[(Option<&'static str>, Value)]) -> crate::Result<()> {
        // Same story, and DROPPED rather than refused: an echo is an observation, so losing one
        // costs a ctx-less caller nothing it could have used. A refusal here would turn every
        // value-level probe of an echoing function into an error.
        Ok(())
    }
}

impl FnCtx for NoClosures {
    fn call_value(
        &self,
        _callee: &Value,
        _args: &[(Option<&'static str>, Value)],
    ) -> crate::Result<Value> {
        Err(crate::Error::Unimplemented(
            "a closure invocation with no evaluator — re-interpreting",
        ))
    }

    fn mint_fn(
        &self,
        _def: &str,
        _path: &[usize],
        _self_name: Option<&str>,
        _captures: &[(&'static str, Value)],
    ) -> crate::Result<Value> {
        Err(crate::Error::Unimplemented(
            "a closure mint with no evaluator — re-interpreting",
        ))
    }

    fn reinterpret(
        &self,
        name: &str,
        fallback_sources: &'static str,
        args: &[Value],
    ) -> crate::Result<Value> {
        crate::rt::run_interpreted(fallback_sources, name, args)
    }

    fn call_named(
        &self,
        _name: &str,
        _args: &[(Option<&'static str>, Value)],
    ) -> crate::Result<Value> {
        // No program, so no definition to resolve against — and unlike `reinterpret` there is no
        // batch-local island to fall back to, because the whole point of this capability is that
        // the callee is NOT in the batch. Refuses rather than answering `undef`, which would be
        // indistinguishable from the interpreter's unknown-function result and therefore silent.
        Err(crate::Error::Unimplemented(
            "an outward call with no evaluator — re-interpreting",
        ))
    }
}

/// The NATIVE REGISTRY's surface — the callables served from the STATIC table the transpiler
/// emits into `generated.rs` (AR.14.5), not derived-then-leaked at process start. The third
/// declaration of the same library (fab-gen's runtime `from_registry`) dies against this impl:
/// a generated library declares itself at build time instead of being read back at startup.
///
/// No `preamble` here: what must precede a generated call depends on the CONSUMER's include
/// layout (the fuzz harnesses pass their own `include <BOSL2/std.scad>` line), so the generator
/// keeps carrying it alongside rather than this surface guessing one.
pub struct Natives;

impl LibrarySurface for Natives {
    fn name(&self) -> &'static str {
        "natives"
    }

    fn rows(&self) -> crate::registry::Rows {
        crate::eval::intrinsics::builtin_rows()
    }

    fn callables(&self) -> &'static [Decl] {
        crate::eval::intrinsics::native_decls()
    }
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

/// What a generated MODULE is handed, and the only way it reaches back into the evaluator.
///
/// AR.20.1. A module is not a function with a different return type — three things it needs are
/// impossible to hand over as plain data, and each one shapes this trait:
///
/// **Children are a CALLBACK, and that is correct rather than a compromise.** `children()` may run
/// zero times or many, and it renders in the CALLER's scope and island, so the evaluator stashes
/// the child statements UNEVALUATED. They are the user's source and never the library's, so they
/// can never be compiled — rendering one means re-entering interpretation, by construction.
///
/// **The `$`-chain is inherited by reference, never copied.** L.2.7 is the scar: every call used to
/// clone the caller's reaching `$`-context, BOSL2 sets 42 of them at top level, and call-heavy
/// geometry paid 42 clones per call. A native that snapshots the chain reintroduces exactly that,
/// in compiled form where it is far harder to notice.
///
/// **Module calls DISPATCH.** chotchki: *"I really want dispatch, otherwise we're making an
/// interpreter with extra steps"* — a generated module that emitted work-stack tasks would keep
/// every bit of the interpretation overhead this phase exists to delete. So [`ModuleCtx::call`]
/// resolves through the registry and lands on another native where one exists. The cost is host
/// stack depth, which the interpreter's explicit stack deliberately does not use, so it is bounded
/// the same way AR.10 bounds function natives: a depth budget, and past it a decline back to the
/// interpreter, whose own limit is `100_000` because its depth is heap.
pub trait ModuleCtx: Console {
    /// The call's bound arguments, already matched to parameters (positional fill, named, defaults)
    /// by the evaluator's own two-phase rule — the AN.1/AN.2/AN.6 semantics a native must not
    /// reimplement.
    fn args(&self) -> &[Value];

    /// `$children` — how many geometry children the CALL SITE supplied. Empty statements and
    /// child-block assignments are not children; counting them would misalign `children(i)`.
    fn child_count(&self) -> usize;

    /// Render child `i` in the caller's scope. Out of range renders nothing, matching upstream.
    ///
    /// # Errors
    /// Whatever evaluating that child raises.
    fn child(&self, i: usize) -> crate::Result<Geo>;

    /// Render the children a `children(i)` / `children([i:j])` / `children([a,b])` selects.
    ///
    /// Takes a `Value` rather than an index because upstream accepts all three selector shapes and
    /// the index rules (out of range renders nothing, a wrong-way range yields nothing) are the
    /// evaluator's, not the caller's.
    ///
    /// # Errors
    /// Whatever evaluating the selected children raises.
    fn child_at(&self, selector: &Value) -> crate::Result<Geo>;

    /// Render every child, unioned — bare `children()`.
    ///
    /// # Errors
    /// Whatever evaluating the children raises.
    fn children(&self) -> crate::Result<Geo>;

    /// The implicit UNION of a statement list — what a `{ … }` block means in OpenSCAD, and what a
    /// module body's several statements collapse to.
    ///
    /// On the ctx rather than in `rt` because it needs the evaluator: 2D and 3D children partition
    /// differently and mixing them warns, which is behaviour a generated module must inherit rather
    /// than reimplement.
    fn group(&self, parts: Vec<Geo>) -> Geo;

    /// Read a `$`-variable off the inherited dynamic chain. `undef` when unbound, as in the
    /// interpreter — a missing `$`-var is not an error.
    fn dollar(&self, name: &str) -> Value;

    /// Call another module, dispatching through the registry: another native where one is armed,
    /// the interpreter otherwise.
    ///
    /// # Errors
    /// Whatever the called module raises, including the depth-budget decline.
    fn call(&self, call: &ModuleCall<'_>) -> crate::Result<Geo>;

    /// Bind a `$`-variable into THIS call's frame (AR.22): the compiled mirror of a hoisted
    /// in-body `$x = …`, which is a WRITE into the dynamic chain — every callee dispatched after
    /// it and every child rendered by `children()` sees the new binding, exactly as the
    /// interpreter's hoisted bind is seen. This is the attachment system's mechanism
    /// (`$transform`, `$parent_size`, the tag family), which is why the whole transform-wrapper
    /// chain waits on it. Top-of-body scope only — the emitter declines a `$`-set in a nested
    /// scope, whose bind would be branch-scoped.
    fn set_dollar(&self, name: &'static str, value: Value);

    /// Call a FUNCTION by name with evaluated arguments, resolving where the interpreted body
    /// would (AR.14.4.3): a local binding holding a function value is INVOKED (AD.1 — the
    /// interpreter's `CallValue` machinery; this is how a registered nested function, or a
    /// parameter holding a closure, gets called), then the island's user functions — which may
    /// SHADOW a builtin, the reroute `rt::bi` could never honor — then builtins by capability,
    /// then OpenSCAD's warn-and-`undef`. This is what makes a compiled module's function calls
    /// DISPATCH rather than baked assumptions: the callee is whatever the program actually
    /// defines at runtime, so a shadowed `sin` or a sibling defined three files away both just
    /// resolve.
    ///
    /// # Errors
    /// A decline (`Unimplemented`) for the shapes handed back to the interpreter — a bound-method
    /// value, a file-reading builtin — or whatever the resolved body raises (assert failures, the
    /// recursion guard).
    fn call_fn(&self, call: &FnCall<'_>) -> crate::Result<Value>;

    /// Register one of this module body's nested `function` definitions (AR.14.4.5), binding a
    /// closure VALUE by its name into this call's frame — the compiled mirror of the hoisted
    /// bind `hoist_scope` does at the same first-occurrence position. `frame` carries the body
    /// locals hoisted BEFORE this definition, materialized from the native's own bindings: the
    /// closure's lexical env is the call frame plus exactly those, which reproduces the
    /// interpreter's capture-at-bind-position view (a later hoisted local is invisible, a
    /// parameter always visible). The definition's AST comes from the runtime's own resolved
    /// body — the fingerprint gate proved it identical to what the emitter compiled — so the
    /// body may hold constructs no native could express; it is interpreted at invoke time.
    ///
    /// # Errors
    /// A decline (`Unimplemented`) when the runtime cannot find the named definition at the
    /// body's flattened top level — drift the gate should have caught, so falling back to the
    /// interpreter is the safe answer.
    fn register_local_fn(
        &self,
        name: &'static str,
        frame: &[(&'static str, Value)],
    ) -> crate::Result<()>;

    /// Register this module body's nested `module` definitions (AR.14.4.5) so calls to them —
    /// from the compiled body and from anything it calls, the interpreter's dynamically-scoped
    /// v1 visibility — resolve through `Ctx::resolve_module`'s local rung exactly as if the
    /// interpreter had pushed them at block entry. `frame` is the body's full hoisted locals
    /// (the emitter calls this AFTER the whole prelude, matching the interpreter's capture of
    /// the fully-hoisted block scope — cuboid's nested defs see the post-reassignment `size`).
    /// The registration lives until the native returns; the dispatch site pops it.
    ///
    /// # Errors
    /// A decline (`Unimplemented`) when the names the emitter compiled against do not match the
    /// definitions found in the resolved body — the registration would bind the wrong world.
    fn register_local_modules(
        &self,
        names: &[&'static str],
        frame: &[(&'static str, Value)],
    ) -> crate::Result<()>;
}

/// A function call a generated MODULE body makes — the expression twin of [`ModuleCall`], with the
/// same one-list argument shape and the same runtime-resolution contract (see
/// [`ModuleCtx::call_fn`]). No children: functions have none.
pub struct FnCall<'a> {
    /// The callee, as written. `'static` for [`ModuleCall::name`]'s reason.
    pub name: &'static str,
    /// The arguments IN SOURCE ORDER, each with the name it was written with if any. A `Some`
    /// name starting with `$` is a per-call dynamic override, exactly as in a module call.
    pub args: &'a [(Option<&'static str>, Value)],
}

/// One module call a generated module makes — the call site as WRITTEN, with its arguments
/// evaluated and nothing else decided.
///
/// The shape is deliberate and it is the second design this went through. The first carried
/// arguments already POSITIONALISED against the callee's parameter list, plus the parameter names
/// the emitter had assumed so the runtime could check them. That check turned out to be
/// insufficient while writing its own test, which is the useful part: matching parameter NAMES does
/// not catch a shadowing module that keeps the names and changes a DEFAULT, and the emitter had
/// baked the library's defaults into the argument list to fill holes. A user redefining
/// `module cyl(h, r=5)` as `module cyl(h, r=99)` would have taken the compiled path and silently
/// got `5`.
///
/// So the assumption is GONE rather than guarded. Slot matching happens at RUNTIME against whatever
/// the name actually resolved to, through the same `fill_slots` the interpreter uses, and unfilled
/// parameters take the REAL callee's defaults. There is no claim about the callee left to violate,
/// which is a better property than any check: AN.10 says a name can move at runtime, and this
/// simply does not care if it does.
pub struct ModuleCall<'a> {
    /// The callee.
    ///
    /// `'static` is the honest type, not a restriction — a generated module emits its callees as
    /// string LITERALS (knowing them at compile time is what makes this dispatch rather than
    /// interpretation), and the evaluator's instantiation stack borrows the name for the length of
    /// the call, which a shorter lifetime could not satisfy.
    pub name: &'static str,
    /// The arguments, IN SOURCE ORDER, each with the name it was written with if any.
    ///
    /// One list rather than three because that is what a call site is. A `None` name is positional,
    /// a `Some` name binds to that parameter, and a `Some` name starting with `$` is a dynamic
    /// override — exactly the partition `module::eval_args` and `fill_slots` already make, so a
    /// generated call needs no separate channels for named arguments or `$`-args.
    pub args: &'a [(Option<&'static str>, Value)],
    /// The children this call site supplies.
    pub children: Children<'a>,
}

/// The children a generated module passes ALONG to a module it calls.
///
/// Not a rendered `Geo`: passing geometry would flatten the laziness that makes `children()` mean
/// what it means upstream, where a callee may instantiate its children zero times or many and each
/// instantiation happens in ITS caller's scope.
///
/// TWO variants, not three. An `Inherited` variant sat here — "forward the children I received,
/// untouched" — and it was deleted because it MODELS NO SOURCE CONSTRUCT. OpenSCAD has no syntax
/// that splices a caller's children in as a callee's own; the shape it was meant for,
/// `foo() children();`, passes foo exactly ONE child (the `children()` node), which expands to
/// however many the caller got only when foo renders it. Forwarding the list directly made
/// `$children` report the expanded count, so `module w() { foo() children(); } w() { a(); b(); }`
/// saw 2 inside foo where the interpreter says 1. Caught by the console differential, which is the
/// argument for diffing echoes and not just meshes — the geometry happened to match.
#[derive(Clone, Copy)]
pub enum Children<'a> {
    /// The call supplies no children — a `;` body.
    None,
    /// A COMPILED child block: one thunk per geometry child, in source order.
    ///
    /// CORRECTED once already, from a `&[Geo]` of already-built subtrees. A callee may instantiate
    /// its children ZERO times or MANY — `if` guards them, `for` repeats them — and each
    /// instantiation happens fresh. Handing down finished geometry flattens exactly the laziness
    /// that makes `children()` mean what it means, so a callee that rendered its children twice
    /// would get one subtree duplicated rather than two evaluations, and a callee that rendered
    /// them zero times would still have paid for them.
    ///
    /// A SLICE rather than one thunk because `children(i)` selects, `$children` counts, and neither
    /// is answerable from a single closure over the whole block.
    ///
    /// The thunk receives the CALLER's ctx, not the callee's — that is what makes `foo() children();`
    /// expressible as `Compiled(&[&|c| c.children()])`, and it is why every method here takes
    /// `&self`: the callee runs a thunk that reaches back into its caller while the caller's own
    /// call is still on the stack, which `&mut self` cannot express. The evaluator's state is behind
    /// `RefCell`/`Cell` already, so shared access is sufficient — the same conclusion the function
    /// side reached for `FnCtx`.
    Compiled(&'a [ChildThunk<'a>]),
}

/// One child of a COMPILED call site: a closure that renders it against the CALLER's ctx.
///
/// Named because the bare type appears on both sides of the ABI and reads as noise inline.
pub type ChildThunk<'a> = &'a dyn Fn(&dyn ModuleCtx) -> crate::Result<Geo>;

