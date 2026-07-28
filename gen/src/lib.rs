//! K.3 → AJ — a grammar-directed OpenSCAD program generator covering the WHOLE language surface.
//!
//! A seed picks a deterministic walk through the grammar (via fab-lang's own MT19937 `RandStream`,
//! so a seed replays the exact program on every platform), emitting a VALID-by-construction
//! program: bounded depth + fuel, scope-tracked variables, and ONLY calls to known builtins /
//! already-defined functions and modules. Range magnitudes, `$fn`, recursion and tree depth are
//! all bounded, so a generated program never reproduces the unbounded-comprehension DoS class —
//! the whole corpus stays cheap to evaluate.
//!
//! COVERAGE IS GATED (AJ.1): `grammar_covers_the_language_surface` asserts every construct family
//! appears across the first seeds — a language feature that ships without a production here fails
//! CI with the family named. The v0 grammar covered 7 of 62 families; this is the AJ.2-5 fill.
//!
//! HERMETIC: the file-value builtins (`import()`, `dxf_dim`) are emitted against paths that never
//! exist — they exercise the needs channel + warn-and-undef only, no filesystem dependence. Only
//! SEEDED `rands` is emitted. Dimension-homogeneous geometry trees (2D and 3D never mix in one
//! boolean); extrudes bridge 2D→3D, `projection` 3D→2D.
//!
//! This is the "higher-level fuzzer" / ML-corpus half of the plan: where cargo-fuzz mutates BYTES
//! (dense but adversarial), this emits PROGRAMS (valid, diverse, labelable). The binary runs each
//! through the evaluator + the JIT bit-identity check to attach labels; the `gen_diff` fuzz target
//! (AJ.6) drives the same walk from fuzzer bytes with the Config A/B contract as its oracle.

use fab_lang::RandStream;

/// The bounds a generation runs under (AO.1) — what used to be four `const`s, so a second, HEAVIER
/// profile can exist without forking the grammar.
///
/// [`Profile::CHEAP`] is the original set, and it is FROZEN: `gen_diff` and `jit_dispatch_diff` seed their
/// corpora from these programs and the AJ.1 coverage gate asserts against them, so a shifted bound does not
/// fail anything — it silently moves what those campaigns explore. `cheap_profile_is_frozen` hashes 2000
/// seeds to make that checkable rather than assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Expression nesting cap.
    pub max_expr_depth: u32,
    /// Total emission budget; the walk stops when it runs out.
    pub start_fuel: u32,
    /// Upper bound on top-level statements.
    pub max_stmts: i64,
    /// Range endpoints stay in `[-range_bound, range_bound]`.
    pub range_bound: i64,
    /// Upper bound on an emitted `$fn` — the facet count, and so the main dial on CURVED geometry cost.
    pub max_fn: i64,
    /// Geometry TREE depth cap (AO.3).
    ///
    /// EXPONENTIAL, unlike the others: a boolean level emits 2-3 children that each recurse, so leaves
    /// grow ~3^depth. Depth 3 is ≤27 primitives, depth 6 already ≤729 — which is why the curve is driven
    /// by `max_fn` (linear in facets) and this moves in coarse, capped steps.
    pub max_geo_depth: u32,
    /// Operands a generated `hull()` gets. Safe to scale — hull is ~O(n log n) in its input points.
    pub hull_operands: i64,
    /// Put `$fn` on the PRIMITIVE, not just the program (AO.2).
    ///
    /// This is the knob that actually lands the dial on the mesh, and its absence is why AO.12 measured a
    /// flat curve. Measured over 300 seeds per dial: the global `$fn = …` statement fires in only 118 of
    /// them (39%), and when it fires it draws UNIFORM FROM ZERO, so its mean is half the cap — 15/32 at
    /// dial 2, 124/256 at dial 16. The other 61% of programs tessellate at the `$fa`/`$fs` defaults no
    /// matter how high the dial goes, and `cube`/`sphere`/`cylinder` never carried a `$fn` of their own at
    /// all. Raising `max_fn` was therefore moving a number that mostly did not reach the geometry.
    ///
    /// ON, every curved primitive draws its own `$fn` from the TOP HALF of `max_fn` — a floor, not a
    /// uniform draw, so the dial is a lower bound on facets rather than an average of one.
    ///
    /// OFF in [`Profile::CHEAP`], which is frozen bit-for-bit by `the_cheap_corpus_has_not_moved`: the
    /// flag gates the extra RNG draw as well as the extra text, so the cheap stream is untouched.
    pub prim_fn: bool,
    /// `$fn` for MINKOWSKI operands, which deliberately does NOT scale with the dial (AO.3).
    ///
    /// Minkowski cost is MULTIPLICATIVE in the operands' vertex counts, so letting these follow `max_fn`
    /// turns one seed into a nightly-eating outlier — `minkowski(cube, sphere($fn=256))` is not a bigger
    /// version of `minkowski(cube, sphere($fn=6))`, it is a different order of magnitude. The pin is the
    /// guard; the existing hardcoded `6` was already doing this job informally.
    pub minkowski_fn: i64,
    /// Domain-typed builtin arguments (AR.4).
    ///
    /// OFF, a builtin call's arguments are arbitrary expressions — the cheap lane's frozen bytes, and
    /// where the type-mismatch FINDINGS live. ON, each argument is drawn from the parameter's declared
    /// [`Domain`] so the call COMPUTES: a wrong-typed argument returns `undef` in ~0 time, so a
    /// domain-blind corpus measures error handling while looking like it measures work — and a
    /// regression in call-generation quality reads as a performance WIN, because failing calls get
    /// faster. Like [`Profile::prim_fn`], the flag gates the RNG draws as well as the text, so the
    /// cheap stream is untouched.
    pub domains: bool,
}

impl Profile {
    /// The original bounds. Every field here is load-bearing for an existing corpus; see the type docs.
    pub const CHEAP: Self = Self {
        max_expr_depth: 4,
        start_fuel: 90,
        max_stmts: 9,
        range_bound: 32,
        max_fn: 12,
        max_geo_depth: 3,
        hull_operands: 2,
        prim_fn: false,
        minkowski_fn: 6,
        domains: false,
    };

    /// A program heavy enough to TIME, scaled by `dial` (1 = mildly heavy; higher = more work).
    ///
    /// `dial` is the x-axis of AO.7's scaling curve, so the knobs it moves have to make cost grow for a
    /// reason we can name. `max_fn` is the big one: facets per curve drive real tessellation and then real
    /// boolean work. `max_stmts` and `start_fuel` buy more geometry rather than bigger geometry.
    ///
    /// `range_bound` deliberately does NOT scale. Widening it inflates comprehension LISTS, and then the
    /// lane measures list evaluation — the value-side cost AO.2 exists to suppress as timing noise. Cost
    /// has to come from geometry or the number means nothing.
    #[must_use]
    pub fn heavy(dial: u32) -> Self {
        let d = dial.max(1);
        Self {
            max_expr_depth: Self::CHEAP.max_expr_depth + d,
            start_fuel: Self::CHEAP.start_fuel * d,
            // Every knob is CHEAP-plus-something, never a bare multiple: `6 * d` read fine and gave 6 at
            // dial 1, BELOW cheap's 9, so the first dial step emitted LIGHTER programs than the profile it
            // is supposed to exceed. Nothing caught it — the freeze test only guards `cheap`, and the parse
            // and coverage tests only run `cheap`. `heavy` was unverified code until it was measured.
            max_stmts: Self::CHEAP.max_stmts + 6 * i64::from(d),
            range_bound: Self::CHEAP.range_bound,
            max_fn: 16 * i64::from(d),
            // Coarse + CAPPED: leaves grow ~3^depth, so this is the one knob that can turn a dial step
            // into an outlier that eats the nightly budget on its own.
            max_geo_depth: (Self::CHEAP.max_geo_depth + d / 2).min(6),
            hull_operands: Self::CHEAP.hull_operands + i64::from(d),
            // The dial reaches the MESH through this, not through max_fn alone — see the field docs.
            prim_fn: true,
            // PINNED at every dial — see the field docs. Multiplicative cost does not get a dial.
            minkowski_fn: Self::CHEAP.minkowski_fn,
            // Heavy calls must DO WORK to be worth timing — see the field docs.
            domains: true,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::CHEAP
    }
}

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
fn satisfies(ret: Domain, want: Domain) -> bool {
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

/// Shorthand for a required parameter.
const fn p(name: &'static str, domain: Domain) -> Param {
    Param {
        name,
        domain,
        required: true,
    }
}

/// A builtin FUNCTION declaration — `names_bind: false` for every one of them, verified.
const fn bf(name: &'static str, ret: Domain, params: &'static [Param]) -> Decl {
    Decl {
        name,
        kind: Kind::Function,
        names_bind: false,
        ret,
        params,
    }
}

/// Builtins SAFE to call with any args (a type mismatch yields `undef`, never an error), as DECLS.
///
/// ORDER AND LENGTH ARE FROZEN: `pick_builtin` indexes this table with the RNG, so reordering or
/// resizing it moves every subsequent draw and breaks the `cheap` corpus digest. The migration from
/// `&[(&str, usize)]` to `&[Decl]` was deliberately output-NEUTRAL — same entries, same order, same
/// arity, still called positionally in the cheap lane.
///
/// The DOMAINS here are GENERATION domains, not acceptance claims: they answer "what argument makes
/// this call compute" (AR.4), which is narrower than what the builtin tolerates. `len` accepts
/// anything and measures only sized values, so it declares `VecN`; `asin` accepts any number and is
/// NaN outside `[-1, 1]`, so it declares `Unit`. Only the heavy lane reads them — cheap's arbitrary
/// arguments still cover the mismatch space, where the FINDINGS live.
const BUILTINS: &[Decl] = &[
    bf("sin", Domain::Unit, &[p("x", Domain::Deg)]),
    bf("cos", Domain::Unit, &[p("x", Domain::Deg)]),
    bf("tan", Domain::Num, &[p("x", Domain::Deg)]),
    bf("asin", Domain::Deg, &[p("x", Domain::Unit)]),
    bf("acos", Domain::Deg, &[p("x", Domain::Unit)]),
    bf("atan", Domain::Deg, &[p("x", Domain::Num)]),
    bf("sqrt", Domain::Num, &[p("x", Domain::Pos)]),
    bf("abs", Domain::Num, &[p("x", Domain::Num)]),
    bf("floor", Domain::Num, &[p("x", Domain::Num)]),
    bf("ceil", Domain::Num, &[p("x", Domain::Num)]),
    bf("round", Domain::Num, &[p("x", Domain::Num)]),
    bf("ln", Domain::Num, &[p("x", Domain::Pos)]),
    bf("exp", Domain::Num, &[p("x", Domain::Num)]),
    bf("sign", Domain::Num, &[p("x", Domain::Num)]),
    bf("norm", Domain::Num, &[p("v", Domain::VecN)]),
    // VecN, not Any: `len(5)` is undef upstream — a generated call must MEASURE something.
    bf("len", Domain::Num, &[p("value", Domain::VecN)]),
    // base Pos: a negative base under a fractional exponent is NaN.
    bf(
        "pow",
        Domain::Num,
        &[p("base", Domain::Pos), p("exponent", Domain::Num)],
    ),
    bf(
        "atan2",
        Domain::Deg,
        &[p("y", Domain::Num), p("x", Domain::Num)],
    ),
    // VARIADIC upstream; pinned at 2 because that is what the corpus has always generated.
    bf(
        "min",
        Domain::Num,
        &[p("a", Domain::Num), p("b", Domain::Num)],
    ),
    bf(
        "max",
        Domain::Num,
        &[p("a", Domain::Num), p("b", Domain::Num)],
    ),
    bf(
        "cross",
        Domain::Vec3,
        &[p("a", Domain::Vec3), p("b", Domain::Vec3)],
    ),
    // list + string group (AJ.4)
    bf(
        "str",
        Domain::Str,
        &[p("a", Domain::Any), p("b", Domain::Any)],
    ),
    bf("chr", Domain::Str, &[p("n", Domain::Pos)]),
    bf("ord", Domain::Num, &[p("c", Domain::Str)]),
    bf(
        "concat",
        Domain::List,
        &[p("a", Domain::Any), p("b", Domain::Any)],
    ),
    bf(
        "search",
        Domain::List,
        &[
            // Num, NOT Any — a STRING key over a non-string column ABORTS the oracle (upstream
            // #5017, docs/openscad-search-crash.md). A generation choice until their fix ships:
            // cheap's arbitrary args still reach the crash shape, in the lane that handles crashes.
            p("match_value", Domain::Num),
            p("string_or_vector", Domain::Table),
        ],
    ),
    bf(
        "lookup",
        Domain::Num,
        &[p("key", Domain::Num), p("pairs", Domain::Table)],
    ),
    // type predicates — genuinely Any, that is the point of them
    bf("is_num", Domain::Bool, &[p("value", Domain::Any)]),
    bf("is_undef", Domain::Bool, &[p("value", Domain::Any)]),
    bf("is_string", Domain::Bool, &[p("value", Domain::Any)]),
    bf("is_list", Domain::Bool, &[p("value", Domain::Any)]),
    bf("is_bool", Domain::Bool, &[p("value", Domain::Any)]),
    bf("is_object", Domain::Bool, &[p("value", Domain::Any)]),
];

/// The builtin call surface (AR.3) — exposed so tests and, later, the heavy lane can walk it.
///
/// Read-only on purpose: the table's ORDER and LENGTH are load-bearing (`pick_builtin` indexes it
/// with the RNG), so it is published as a slice rather than something a caller could rearrange.
#[must_use]
pub fn builtins() -> &'static [Decl] {
    BUILTINS
}

/// What a callable surface DECLARES about itself — the AR.3 seam, so the generator stops being
/// hardcoded to the builtin table and can be pointed at a library.
///
/// The point of a trait rather than a second const: a transpiled library (AR.1) must be able to say
/// what it hosts without the generator knowing anything about it, and the SAME declaration has to feed
/// the dispatch registry and the differential. One description, three consumers.
///
/// SEED STABILITY IS PART OF THE CONTRACT. `pick_builtin` indexes the surface with the RNG, so a
/// surface's order and length determine what every seed generates. Changing either re-points the whole
/// accumulated fuzz corpus at different programs — the inputs survive, their MEANING doesn't. Hence
/// `decls` returns a slice the implementor promises to keep stable, and a library surface is ADDITIVE
/// (a separate surface, chosen explicitly) rather than something merged into `BUILTINS`.
pub trait Surface {
    /// The callables this surface hosts, in a stable order.
    ///
    /// `'static` on purpose: the generator holds the slice for its whole walk and indexes it with the
    /// RNG. A borrowed surface would push a lifetime through every `Gen` method for a table that is
    /// built once per process — [`NativeSurface`] leaks its derived decls rather than pay that.
    fn decls(&self) -> &'static [Decl];

    /// SCAD that must precede a generated call — a library's `include`/`use`, or its definitions
    /// inline. Empty for builtins, which need no preamble. Without this a generated program calls
    /// functions that don't exist and every seed fails identically on both legs of a differential,
    /// which LOOKS like agreement.
    fn preamble(&self) -> &str {
        ""
    }
}

/// The builtin surface — today's table, unchanged, so existing seeds keep their meaning.
#[derive(Clone, Copy, Debug, Default)]
pub struct Builtins;

impl Surface for Builtins {
    fn decls(&self) -> &'static [Decl] {
        BUILTINS
    }
}

/// The NATIVE registry's surface, built from `fab_lang::native_surface()` — the AR.3 payoff.
///
/// Nobody writes this table. Names and required-ness are PARSED from each entry's `reference`, the
/// same verbatim source the fingerprint gate checks at dispatch, so the generator's picture of "what
/// can I call, and with which argument names" cannot drift from what the natives actually answer. That
/// is the maintenance win the AR bet is for, and AR.5a is why it matters: three of the registry's
/// hand-maintained guard lists were wrong.
///
/// `names_bind: true` on every entry, unlike [`Builtins`] — these stand in for BOSL2 user functions,
/// where parameter names really do bind. That single flag is what puts AN.1/AN.2/AN.3/AN.14's whole
/// diagnostic family in reach; a generator pointed only at builtins can never emit a named-arg call
/// that means anything.
///
/// Domains are DERIVED too (AR.3.3), from the type tests each reference applies to its own arguments —
/// `is_vector`, `is_list`, `is_num`, `len`, indexing, and the scalar-only math builtins. A parameter
/// the body never tests stays [`Domain::Num`], which is the generator's fallback rather than a claim.
///
/// A parameter carries a SET upstream, because these functions are polymorphic on purpose (`approx`
/// accepts bools, numbers AND lists). `Decl` holds one domain, so the widest observed type wins here —
/// generating the LIST arm of a polymorphic function exercises more than the scalar arm does.
pub struct NativeSurface {
    decls: &'static [Decl],
    preamble: String,
}

impl NativeSurface {
    /// Build it from the registry. Leaks the derived strings: a `Decl` holds `&'static str`, the
    /// surface lives for the process, and the alternative is threading a lifetime through the whole
    /// generator for a table built once.
    #[must_use]
    pub fn from_registry(preamble: impl Into<String>) -> Self {
        let decls: Vec<Decl> = fab_lang::native_surface()
            .into_iter()
            .map(|f| {
                let params: Vec<Param> = f
                    .params
                    .iter()
                    .map(|p| Param {
                        name: Box::leak(p.name.clone().into_boxed_str()),
                        domain: widest(&p.domains),
                        required: p.required,
                    })
                    .collect();
                Decl {
                    name: Box::leak(f.name.into_boxed_str()),
                    kind: Kind::Function,
                    ret: Domain::Num,
                    names_bind: true,
                    params: Box::leak(params.into_boxed_slice()),
                }
            })
            .collect::<Vec<Decl>>();
        NativeSurface {
            decls: Box::leak(decls.into_boxed_slice()),
            preamble: preamble.into(),
        }
    }
}

/// The widest type observed for a parameter, as a generator [`Domain`].
///
/// "Widest" rather than "first" because a polymorphic function's LIST arm does more work than its
/// scalar arm, and work is what the corpus is supposed to be measuring (AR.4). `Indexable` alone means
/// the body only ever did `len(p)` or `p[i]` — true of a string as well as a list, so it maps to the
/// list side, which is the useful guess. `Vector` maps to `VecN` rather than `Vec3` — the body
/// said "numeric list", not "exactly three". An empty set falls back to `Num`.
fn widest(domains: &[fab_lang::SurfaceDomain]) -> Domain {
    use fab_lang::SurfaceDomain as S;
    let mut best = Domain::Num;
    let mut rank = 0u8;
    for d in domains {
        let (cand, r) = match d {
            S::Bool => (Domain::Bool, 1),
            S::Str => (Domain::Str, 2),
            S::Num => (Domain::Num, 3),
            S::Indexable | S::List => (Domain::List, 4),
            S::Vector => (Domain::VecN, 5),
        };
        if r > rank {
            best = cand;
            rank = r;
        }
    }
    best
}

impl Surface for NativeSurface {
    fn decls(&self) -> &'static [Decl] {
        self.decls
    }
    fn preamble(&self) -> &str {
        &self.preamble
    }
}

/// The generator state: the RNG plus the lexical scope it's building up.
pub struct Gen {
    rng: RandStream,
    /// The bounds this walk runs under (AO.1).
    profile: Profile,
    fuel: u32,
    depth: u32,
    /// The call surface this walk generates against (AR.3.2). `Builtins` by default, so every
    /// pre-existing seed keeps its meaning — the RNG indexes this slice, so swapping it silently
    /// re-points the accumulated corpora at different programs.
    surface: &'static [Decl],
    /// Source the surface needs before its calls resolve (a library's `include`). Empty for builtins.
    preamble: String,
    vars: Vec<String>,                 // in-scope variable names
    funcs: Vec<(String, Vec<String>)>, // defined functions (name, param names)
    mods: Vec<(String, usize)>,        // defined modules (name, arity)
    next_id: u32,
}

/// Generate the program for `seed` — deterministic + reproducible (same seed → same bytes, every platform).
///
/// Runs under [`Profile::CHEAP`], which is frozen: this function's output feeds the fuzz corpora and the
/// AJ.1 coverage gate, so it must keep producing the exact bytes it always has.
#[must_use]
pub fn generate(seed: u32) -> String {
    generate_with(seed, Profile::CHEAP)
}

/// [`generate_with`] against an explicit call SURFACE (AR.3.2) — how a transpiled library gets fuzzed.
///
/// The surface's preamble is emitted first, so its calls actually resolve; without it every generated
/// program fails identically on both legs of a differential, which reads as agreement.
///
/// A seed means something DIFFERENT under a different surface, by construction — the RNG indexes the
/// decl table. That is why this is a separate entry point rather than a parameter on [`generate`]:
/// the existing corpora belong to the builtin surface and must keep meaning what they meant.
#[must_use]
pub fn generate_against(seed: u32, profile: Profile, surface: &dyn Surface) -> String {
    let mut g = Gen::with_profile(seed, profile);
    g.surface = surface.decls();
    g.preamble = surface.preamble().to_string();
    g.program()
}

/// [`generate`] under an explicit [`Profile`] — the heavy lane's entry (AO.1).
///
/// A seed replays deterministically WITHIN a profile; the same seed under two profiles is two different
/// programs, which is the point (AO.7 plots the same seeds across dials).
#[must_use]
pub fn generate_with(seed: u32, profile: Profile) -> String {
    Gen::with_profile(seed, profile).program()
}

impl Gen {
    #[must_use]
    fn with_profile(seed: u32, profile: Profile) -> Self {
        Self {
            rng: RandStream::seeded(seed),
            profile,
            fuel: profile.start_fuel,
            depth: 0,
            surface: BUILTINS,
            preamble: String::new(),
            vars: Vec::new(),
            funcs: Vec::new(),
            mods: Vec::new(),
            next_id: 0,
        }
    }

    // --- draw helpers (all on the one MT19937 stream) ---

    /// A uniform index in `[0, n)` (0 when `n == 0`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "n is a tiny arm count; the draw is in [0, n)"
    )]
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            // next_one is [min, max) → floor stays < n; clamp guards the (unreachable) max endpoint.
            (self.rng.next_one(0.0, n as f64) as usize).min(n - 1)
        }
    }

    /// True with probability `p`.
    fn chance(&mut self, p: f64) -> bool {
        self.rng.next_one(0.0, 1.0) < p
    }

    /// A uniform integer in `[lo, hi]` (inclusive).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "spans here are tiny (bounded by the grammar's own constants)"
    )]
    fn int_between(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        lo + self.below((hi - lo + 1) as usize) as i64
    }

    /// Pick one of `xs` (borrowing it out so the borrow of `self` ends before the caller draws again).
    fn pick_str(&mut self, xs: &[&'static str]) -> &'static str {
        xs[self.below(xs.len())]
    }

    /// A fresh, collision-free identifier with the given prefix (prefixes avoid keyword/builtin clashes).
    fn fresh(&mut self, prefix: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{prefix}{id}")
    }

    // --- program + statements ---

    /// A whole program: an optional `$`-prologue, a handful of statements, at least one of them
    /// geometry (so `evaluate` yields real output often enough for meaningful labels).
    #[must_use]
    fn program(&mut self) -> String {
        let mut out = String::new();
        // AR.3.2 — the surface's own definitions first, or its calls resolve to nothing. Emitted
        // BEFORE any RNG draw, so a preamble cannot shift what a seed generates.
        if !self.preamble.is_empty() {
            out.push_str(&self.preamble);
            if !self.preamble.ends_with('\n') {
                out.push('\n');
            }
        }
        // $-assignments (dynamic-scope fallbacks) — SMALL $fn so any circle stays cheap.
        if self.chance(0.4) {
            out.push_str(&format!(
                "$fn = {};\n",
                self.int_between(0, self.profile.max_fn)
            ));
        }
        if self.chance(0.15) {
            out.push_str("$fa = 12;\n$fs = 2;\n");
        }
        let n = self.int_between(1, self.profile.max_stmts);
        for _ in 0..n {
            if self.fuel == 0 {
                break;
            }
            out.push_str(&self.statement());
            out.push('\n');
        }
        // Guarantee at least one geometry statement — otherwise a value-only program renders empty
        // and every render label collapses to 0 tris.
        out.push_str(&self.geometry3(0));
        out.push('\n');
        out
    }

    fn statement(&mut self) -> String {
        match self.below(10) {
            0 => self.assignment(),
            1 => self.function_def(),
            2 => self.module_def(),
            3 if !self.mods.is_empty() => self.module_call(),
            4 => {
                // let/echo/assert STATEMENT wrapping geometry (assert conds are always-true so the
                // rest of the program keeps evaluating — halting coverage is the byte-fuzzer's job).
                let g = self.geometry3(1);
                match self.below(3) {
                    0 => {
                        let e = self.expr();
                        let v = self.fresh("l");
                        format!("let ({v} = {e}) {g}")
                    }
                    1 => format!("echo(\"s\", {}) {g}", self.expr()),
                    _ => format!("assert(1 == 1) {g}"),
                }
            }
            5 => {
                let n = self.int_between(1, 3);
                let i = self.fresh("i");
                self.vars.push(i.clone());
                let body = self.geometry3(1);
                self.vars.pop();
                format!("intersection_for ({i} = [0:{n}]) {body}")
            }
            6 => {
                // a 2D tree — EXTRUDED at top level: the program always ends with a 3D statement,
                // and the oracle refuses mixed 2D+3D top-level unions (gen-diff seed 18 finding).
                format!(
                    "linear_extrude(height = {}) {}",
                    self.int_between(1, 6),
                    self.geometry2(0)
                )
            }
            _ => self.geometry3(0), // weight 3D geometry highest for render diversity
        }
    }

    /// `id = <expr>;` — binds a fresh variable into scope. Sometimes the RHS is a function LITERAL,
    /// which also registers the name as callable (the named-closure / letrec path).
    fn assignment(&mut self) -> String {
        if self.chance(0.2) {
            // f = function(p) <expr>;
            let name = self.fresh("g");
            let params: Vec<String> = (0..self.int_between(1, 2))
                .map(|_| self.fresh("p"))
                .collect();
            let mark = self.vars.len();
            self.vars.extend(params.iter().cloned());
            let body = self.expr();
            self.vars.truncate(mark);
            self.funcs.push((name.clone(), params.clone()));
            return format!("{name} = function({}) {body};", params.join(", "));
        }
        let e = self.expr();
        let name = self.fresh("v");
        self.vars.push(name.clone());
        format!("{name} = {e};")
    }

    /// `function id(p0, p1, ...) = <expr>;` — params are in scope ONLY for the body; the function
    /// joins the callable set afterward (so later statements can call it, exercising dispatch + the JIT).
    #[allow(clippy::cast_sign_loss, reason = "arity is drawn from [0, 3]")]
    fn function_def(&mut self) -> String {
        let arity = self.int_between(0, 3) as usize;
        let name = self.fresh("f");
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let mark = self.vars.len();
        self.vars.extend(params.iter().cloned());
        let body = self.expr();
        self.vars.truncate(mark); // params leave scope
        self.funcs.push((name.clone(), params.clone()));
        format!("function {name}({}) = {body};", params.join(", "))
    }

    /// `module id(p...) { ...; children(); }` — registers the module; bodies read their params,
    /// place geometry, and exercise the children machinery (`children()`, `children(i)`,
    /// `$children`) so call-site child blocks matter.
    #[allow(clippy::cast_sign_loss, reason = "arity is drawn from [0, 2]")]
    fn module_def(&mut self) -> String {
        let arity = self.int_between(0, 2) as usize;
        let name = self.fresh("m");
        let params: Vec<String> = (0..arity).map(|_| self.fresh("p")).collect();
        let mark = self.vars.len();
        self.vars.extend(params.iter().cloned());
        let inner = self.geometry3(2);
        let children = match self.below(3) {
            0 => "  children();\n",
            1 => "  if ($children > 0) children(0);\n",
            _ => "  for (ci = [0:$children-1]) children(ci);\n",
        };
        self.vars.truncate(mark);
        self.mods.push((name.clone(), arity));
        format!(
            "module {name}({}) {{\n  {inner}\n{children}}}",
            params.join(", ")
        )
    }

    /// A call to a previously-defined module, usually with a child block (feeding `children()`),
    /// sometimes with a `$`-arg (the dynamic-scope channel).
    fn module_call(&mut self) -> String {
        let idx = self.below(self.mods.len());
        let (name, arity) = self.mods[idx].clone();
        let mut args: Vec<String> = (0..arity).map(|_| self.expr()).collect();
        if self.chance(0.25) {
            args.push(format!("$fn={}", self.int_between(3, 10)));
        }
        let kids = match self.below(3) {
            0 => ";".to_string(),
            1 => format!(" {}", self.geometry3(2)),
            _ => {
                let a = self.geometry3(2);
                let b = self.geometry3(2);
                format!(" {{ {a} {b} }}")
            }
        };
        format!("{name}({}){kids}", args.join(", "))
    }

    // --- geometry: 3D and 2D channels, dimension-homogeneous by construction ---

    /// A 3D geometry statement/child at nesting `d`: a leaf primitive, a wrapper, a boolean of a
    /// couple of children, an extrusion of a 2D tree, or a bounded `for`/`if`.
    fn geometry3(&mut self, d: u32) -> String {
        if d >= self.profile.max_geo_depth || self.fuel == 0 || self.chance(0.3) {
            return self.primitive3();
        }
        self.fuel = self.fuel.saturating_sub(1);
        match self.below(10) {
            0 => {
                let v = self.vec3_pos_small();
                format!("translate({v}) {}", self.geometry3(d + 1))
            }
            1 => {
                let v = self.vec3_angle();
                format!("rotate({v}) {}", self.geometry3(d + 1))
            }
            2 => {
                let v = self.vec3_scale();
                format!("scale({v}) {}", self.geometry3(d + 1))
            }
            3 => {
                let op = self.pick_str(&["union", "difference", "intersection"]);
                let k = self.int_between(2, 3);
                let mut kids = String::new();
                for _ in 0..k {
                    kids.push_str("  ");
                    kids.push_str(&self.geometry3(d + 1));
                    kids.push('\n');
                }
                format!("{op}() {{\n{kids}}}")
            }
            4 => {
                // for(i=[0:n]) child — bounded range, i in scope for the child
                let n = self.int_between(0, 4);
                let i = self.fresh("i");
                self.vars.push(i.clone());
                let body = self.geometry3(d + 1);
                self.vars.pop();
                format!("for ({i} = [0:{n}]) {body}")
            }
            5 => {
                // extrusions bridge 2D → 3D
                let flat = self.geometry2(d + 1);
                if self.chance(0.5) {
                    format!("linear_extrude(height = {}) {flat}", self.int_between(1, 8))
                } else {
                    // rotate_extrude needs an all-positive-x profile: shift the 2D tree right.
                    format!(
                        "rotate_extrude(angle = {}) translate([{}, 0]) {flat}",
                        self.int_between(30, 360),
                        self.int_between(6, 15)
                    )
                }
            }
            6 => {
                // wrappers: color / resize / mirror / multmatrix / hull / minkowski (tiny kids —
                // minkowski cost is multiplicative).
                match self.below(6) {
                    0 => format!(
                        "color(\"{}\") {}",
                        self.pick_str(&["red", "lime", "steelblue"]),
                        self.geometry3(d + 1)
                    ),
                    1 => format!(
                        "resize({}) {}",
                        self.vec3_pos_small(),
                        self.geometry3(d + 1)
                    ),
                    2 => format!(
                        "mirror([{}, {}, 1]) {}",
                        i64::from(self.chance(0.5)),
                        i64::from(self.chance(0.5)),
                        self.geometry3(d + 1)
                    ),
                    3 => {
                        let tx = self.int_between(-5, 5);
                        format!(
                            "multmatrix([[1, 0, 0, {tx}], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]) {}",
                            self.geometry3(d + 1)
                        )
                    }
                    4 => {
                        let n = self.profile.hull_operands.max(2);
                        let mut kids = String::new();
                        for _ in 0..n {
                            kids.push_str(&self.primitive3());
                            kids.push(' ');
                        }
                        format!("hull() {{ {} }}", kids.trim_end())
                    }
                    // The operands stay PINNED at `minkowski_fn` on every dial — multiplicative cost
                    // (AO.3). Everything else here scales; this deliberately does not.
                    _ => format!(
                        "minkowski() {{ cube(2); sphere(r = 1, $fn = {}); }}",
                        self.profile.minkowski_fn
                    ),
                }
            }
            _ => {
                // if(cond) child [else child] — sometimes with an instantiation modifier prefix:
                // `if` takes `! # % *` like any module call (the AA.1 census gap). `!` stays rare —
                // root-capture rewrites the whole render's output.
                let m = if self.chance(0.2) {
                    self.pick_str(&["*", "#", "%", "*#", "%*"])
                } else if self.chance(0.02) {
                    "!"
                } else {
                    ""
                };
                let c = self.expr();
                let then = self.geometry3(d + 1);
                if self.chance(0.5) {
                    let els = self.geometry3(d + 1);
                    format!("{m}if ({c}) {then} else {els}")
                } else {
                    format!("{m}if ({c}) {then}")
                }
            }
        }
    }

    /// A 2D geometry tree (used top-level or under an extrusion — NEVER mixed into a 3D boolean).
    fn geometry2(&mut self, d: u32) -> String {
        if d >= self.profile.max_geo_depth || self.fuel == 0 || self.chance(0.4) {
            return self.primitive2();
        }
        self.fuel = self.fuel.saturating_sub(1);
        match self.below(5) {
            0 => format!(
                "offset(r = {}) {}",
                self.int_between(1, 3),
                self.geometry2(d + 1)
            ),
            1 => format!(
                "translate([{}, {}]) {}",
                self.int_between(-8, 8),
                self.int_between(-8, 8),
                self.geometry2(d + 1)
            ),
            2 => {
                let op = self.pick_str(&["union", "difference", "intersection"]);
                format!(
                    "{op}() {{ {} {} }}",
                    self.geometry2(d + 1),
                    self.geometry2(d + 1)
                )
            }
            3 => format!("projection() {}", self.primitive3()), // 3D → 2D bridge
            _ => self.primitive2(),
        }
    }

    /// A 3D leaf primitive with POSITIVE bounded dimensions.
    fn primitive3(&mut self) -> String {
        match self.below(3) {
            0 => format!("cube({});", self.vec3_pos_small()),
            1 => {
                let r = self.int_between(1, 20);
                format!("sphere(r = {r}{});", self.prim_fn_arg())
            }
            _ => {
                let h = self.int_between(1, 20);
                let r = self.int_between(1, 10);
                format!("cylinder(h = {h}, r = {r}{});", self.prim_fn_arg())
            }
        }
    }

    /// `, $fn = N` for a curved primitive, or nothing when the profile leaves it to the program.
    ///
    /// Drawn from the TOP HALF of `max_fn` so the dial is a floor on facets. Emits NOTHING — and draws
    /// NOTHING — under [`Profile::CHEAP`], which keeps that corpus's RNG stream bit-identical.
    fn prim_fn_arg(&mut self) -> String {
        if !self.profile.prim_fn {
            return String::new();
        }
        let cap = self.profile.max_fn.max(4);
        format!(", $fn = {}", self.int_between(cap / 2, cap))
    }

    /// A 2D leaf primitive — square / circle / polygon / text, all tiny.
    fn primitive2(&mut self) -> String {
        match self.below(4) {
            0 => format!(
                "square([{}, {}]);",
                self.int_between(1, 12),
                self.int_between(1, 12)
            ),
            1 => {
                let r = self.int_between(1, 10);
                // A circle is the profile an extrude sweeps, so its facet count multiplies into the
                // solid's triangle count. Hardcoding it at 3..10 capped every extrusion in the corpus
                // no matter what the dial said.
                let fn_ = if self.profile.prim_fn {
                    let cap = self.profile.max_fn.max(4);
                    self.int_between(cap / 2, cap)
                } else {
                    self.int_between(3, 10)
                };
                format!("circle(r = {r}, $fn = {fn_});")
            }
            2 => {
                let w = self.int_between(2, 10);
                let h = self.int_between(2, 10);
                format!("polygon(points = [[0, 0], [{w}, 0], [0, {h}]]);")
            }
            _ => format!(
                "text(\"{}\", size = {});",
                self.pick_str(&["hi", "A9", "fab"]),
                self.int_between(2, 6)
            ),
        }
    }

    fn vec3_pos_small(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(1, 12),
            self.int_between(1, 12),
            self.int_between(1, 12)
        )
    }

    fn vec3_angle(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(-180, 180),
            self.int_between(-180, 180),
            self.int_between(-180, 180)
        )
    }

    fn vec3_scale(&mut self) -> String {
        format!(
            "[{}, {}, {}]",
            self.int_between(1, 3),
            self.int_between(1, 3),
            self.int_between(1, 3)
        )
    }

    // --- expressions ---

    fn expr(&mut self) -> String {
        self.fuel = self.fuel.saturating_sub(1);
        if self.depth >= self.profile.max_expr_depth || self.fuel == 0 || self.chance(0.3) {
            return self.atom();
        }
        self.depth += 1;
        let e = match self.below(15) {
            0 => self.binary(),
            1 => self.unary(),
            2 => self.ternary(),
            3 => self.vector(),
            4 => self.range(),
            5 => self.comprehension(),
            6 => self.builtin_call(),
            7 => self.chain_expr(),
            8 => self.index_or_swizzle(),
            9 => self.object_expr(),
            10 => self.fn_literal_call(),
            11 => self.metrics_or_rands(),
            12 if self.chance(0.25) => self.file_fn(), // rare: warn+undef channel only
            _ => self.user_call(),
        };
        self.depth -= 1;
        e
    }

    fn binary(&mut self) -> String {
        let op = self.pick_str(&[
            "+", "-", "*", "/", "%", "<", "<=", ">", ">=", "==", "!=", "&&", "||", "&", "|", "<<",
            ">>", "^",
        ]);
        format!("({} {} {})", self.expr(), op, self.expr())
    }

    fn unary(&mut self) -> String {
        let op = self.pick_str(&["-", "!"]);
        format!("{}({})", op, self.expr())
    }

    fn ternary(&mut self) -> String {
        format!("({} ? {} : {})", self.expr(), self.expr(), self.expr())
    }

    /// A vector literal, sometimes with an `each` splice mixed in (AJ.3).
    fn vector(&mut self) -> String {
        let k = self.int_between(2, 4);
        let mut items: Vec<String> = (0..k).map(|_| self.expr()).collect();
        if self.chance(0.25) {
            let n = self.int_between(0, 3);
            items.push(format!("each [0:{n}]"));
        }
        format!("[{}]", items.join(", "))
    }

    /// `[lo:hi]` or `[lo:step:hi]` with SMALL, bounded endpoints — never a runaway range.
    fn range(&mut self) -> String {
        let lo = self.int_between(-self.profile.range_bound, self.profile.range_bound);
        let hi = self.int_between(lo, lo + self.profile.range_bound);
        if self.chance(0.5) {
            format!("[{lo}:{}:{hi}]", self.int_between(1, 4))
        } else {
            format!("[{lo}:{hi}]")
        }
    }

    /// Comprehensions in all their forms: plain, if-filtered, C-style, and let-carrying.
    fn comprehension(&mut self) -> String {
        let i = self.fresh("i");
        match self.below(4) {
            0 => {
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                format!("[for ({i} = {r}) {body}]")
            }
            1 => {
                // filtered: [for (i = r) if (cond) body]
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                let m = self.int_between(2, 3);
                format!("[for ({i} = {r}) if ({i} % {m} == 0) {body}]")
            }
            2 => {
                // C-style: bounded by construction (i strictly increases to a small cap)
                let n = self.int_between(1, 6);
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                format!("[for ({i} = 0; {i} < {n}; {i} = {i} + 1) {body}]")
            }
            _ => {
                // let-carrying: [for (i = r) let (t = body) t]
                let r = self.range();
                self.vars.push(i.clone());
                let body = self.expr();
                self.vars.pop();
                let t = self.fresh("t");
                format!("[for ({i} = {r}) let ({t} = {body}) {t}]")
            }
        }
    }

    /// `let`/`assert`/`echo` EXPRESSION chains (AJ.3) — assert conds always-true, and an
    /// occasional DUPLICATE let name (the AH.2.3 first-wins rule). PARENTHESIZED: chains are
    /// legal only at expression-HEAD positions — UPSTREAM TOO (AK.1 probed the oracle: binary/
    /// unary operand positions are syntax errors there as well), so the parens are the honest
    /// grammar, not a workaround.
    fn chain_expr(&mut self) -> String {
        match self.below(4) {
            0 => {
                let v = self.fresh("a");
                let e1 = self.expr();
                self.vars.push(v.clone());
                let body = self.expr();
                self.vars.pop();
                format!("(let({v} = {e1}) {body})")
            }
            1 => {
                // duplicate binding in ONE let: first wins (upstream-pinned)
                let v = self.fresh("a");
                let e1 = self.expr();
                let e2 = self.expr();
                self.vars.push(v.clone());
                let body = self.expr();
                self.vars.pop();
                format!("(let({v} = {e1}, {v} = {e2}) {body})")
            }
            2 => format!("(assert(1 == 1) {})", self.expr()),
            _ => format!("(echo(\"e\", {}) {})", self.expr(), self.expr()),
        }
    }

    /// Postfix access: indexing a vector, or swizzling one (single + multi-letter, both sets).
    fn index_or_swizzle(&mut self) -> String {
        if self.chance(0.5) {
            let v = self.vector();
            let i = self.int_between(0, 3);
            format!("({v})[{i}]")
        } else {
            let sw = self.pick_str(&[".x", ".y", ".z", ".wy", ".rgba", ".xyz"]);
            format!(
                "([{}, {}, {}, {}]){sw}",
                self.expr(),
                self.expr(),
                self.expr(),
                self.expr()
            )
        }
    }

    /// The object family (AJ.4): constructor forms (named members, copy + edit lists with removes),
    /// member access, methods (`this`), `has_key`.
    fn object_expr(&mut self) -> String {
        match self.below(4) {
            0 => format!("object(a = {}, b = {}).a", self.expr(), self.expr()),
            1 => {
                // copy + edit list: remove one member, set another, append a named one
                format!(
                    "object(object(a = 1, b = {}), [[\"a\"], [\"b\", {}]], c = {})",
                    self.expr(),
                    self.expr(),
                    self.expr()
                )
            }
            2 => {
                // method: receiver bound at extraction, `this` injected at call
                format!("object(a = {}, f = function(this) this.a).f()", self.expr())
            }
            _ => format!("has_key(object(k = 1), \"{}\")", self.pick_str(&["k", "z"])),
        }
    }

    /// A function LITERAL applied immediately (closure over the argument).
    fn fn_literal_call(&mut self) -> String {
        let p = self.fresh("p");
        self.vars.push(p.clone());
        let body = self.expr();
        self.vars.pop();
        format!("(function({p}) {body})({})", self.expr())
    }

    /// textmetrics/fontmetrics (deterministic — bundled font) and SEEDED rands.
    fn metrics_or_rands(&mut self) -> String {
        match self.below(4) {
            0 => format!(
                "textmetrics(\"{}\", size = {}).advance",
                self.pick_str(&["hi", "fab"]),
                self.int_between(2, 12)
            ),
            1 => "fontmetrics().interline".to_string(),
            _ => format!(
                "rands(0, 1, {}, {})",
                self.int_between(1, 3),
                self.int_between(0, 999)
            ),
        }
    }

    /// The file-value builtins against paths that NEVER exist — hermetic: exercises the needs
    /// channel + warn-and-undef, no filesystem dependence, rare by weight.
    fn file_fn(&mut self) -> String {
        if self.chance(0.5) {
            "import(\"__fab_gen_no_such__.json\")".to_string()
        } else {
            "dxf_dim(file = \"__fab_gen_no_such__.dxf\", name = \"d\")".to_string()
        }
    }

    /// A call to a KNOWN builtin (arity-correct), so it never trips the unknown-call error.
    fn builtin_call(&mut self) -> String {
        let d = *self.pick_builtin();
        // POSITIONAL in both lanes — builtin names do not bind (`pow(exp=3, base=2)` is 9), so a
        // named form would test nothing new about binding.
        let args: Vec<String> = if self.profile.domains {
            // Domain-typed (AR.4): the call COMPUTES. A wrong-typed argument is an instant `undef`
            // that times as ~0, so a domain-blind heavy corpus measures error handling while
            // looking like it measures work.
            d.params
                .iter()
                .map(|pm| self.domain_expr(pm.domain))
                .collect()
        } else {
            // Arbitrary expressions — cheap's frozen bytes, and the type-MISMATCH coverage.
            (0..d.arity()).map(|_| self.expr()).collect()
        };
        format!("{}({})", d.name, args.join(", "))
    }

    /// An argument VALUE-CONFORMANT to `want` (AR.4): literal-leaning, with COMPOSITION where a
    /// builtin's declared return fits — nesting (`asin(sin(…))`) is where eval work comes from,
    /// and without it every argument bottoms out at a literal after one hop.
    fn domain_expr(&mut self, want: Domain) -> String {
        if self.depth < self.profile.max_expr_depth
            && self.chance(0.3)
            && let Some(call) = self.compose_call(want)
        {
            return call;
        }
        match want {
            Domain::Num => {
                if self.chance(0.3) {
                    let n = self.int_between(-500, 500);
                    format!("{}.{}", n / 10, n.abs() % 10)
                } else {
                    self.int_between(-50, 50).to_string()
                }
            }
            Domain::Pos => self.int_between(1, 50).to_string(),
            // A tenths FRACTION rather than a decimal literal: exact in [-1, 1] by construction.
            Domain::Unit => format!("({} / 10)", self.int_between(-10, 10)),
            Domain::Deg => self.int_between(-360, 360).to_string(),
            Domain::Bool => if self.chance(0.5) { "true" } else { "false" }.to_string(),
            // Non-empty on purpose: `ord("")` is undef, and the empty string is cheap's beat.
            Domain::Str => format!("\"{}\"", self.pick_str(&["a", "x", "hello", "fab", "A9"])),
            Domain::Vec3 => format!(
                "[{}, {}, {}]",
                self.domain_expr(Domain::Num),
                self.domain_expr(Domain::Num),
                self.domain_expr(Domain::Num)
            ),
            Domain::VecN => {
                let k = self.int_between(2, 4);
                let items: Vec<String> = (0..k).map(|_| self.domain_expr(Domain::Num)).collect();
                format!("[{}]", items.join(", "))
            }
            // Flat + shallow: elements draw from the scalar domains only, so an `Any → List → Any`
            // cycle cannot recurse unboundedly.
            Domain::List => {
                let k = self.int_between(2, 4);
                let items: Vec<String> = (0..k)
                    .map(|_| match self.below(3) {
                        0 => self.domain_expr(Domain::Num),
                        1 => self.domain_expr(Domain::Str),
                        _ => self.domain_expr(Domain::Bool),
                    })
                    .collect();
                format!("[{}]", items.join(", "))
            }
            // Keys ASCENDING — `lookup` interpolates over ordered keys.
            Domain::Table => {
                let k = self.int_between(2, 4);
                let base = self.int_between(-20, 20);
                let rows: Vec<String> = (0..k)
                    .map(|i| format!("[{}, {}]", base + 3 * i, self.int_between(-50, 50)))
                    .collect();
                format!("[{}]", rows.join(", "))
            }
            Domain::Any => match self.below(5) {
                0 => self.domain_expr(Domain::Num),
                1 => self.domain_expr(Domain::Str),
                2 => self.domain_expr(Domain::Bool),
                3 => self.domain_expr(Domain::Vec3),
                _ => self.domain_expr(Domain::List),
            },
        }
    }

    /// A nested call whose declared return fits `want`, if any builtin qualifies. Depth-bounded on
    /// the same counter as `expr`, so composition cannot outrun the profile's nesting cap.
    fn compose_call(&mut self, want: Domain) -> Option<String> {
        let fits: Vec<&'static Decl> = self
            .surface
            .iter()
            .filter(|d| satisfies(d.ret, want))
            .collect();
        let d = *fits.get(self.below(fits.len()))?;
        self.depth += 1;
        let args: Vec<String> = d
            .params
            .iter()
            .map(|pm| self.domain_expr(pm.domain))
            .collect();
        self.depth -= 1;
        Some(format!("{}({})", d.name, args.join(", ")))
    }

    fn pick_builtin(&mut self) -> &'static Decl {
        &self.surface[self.below(self.surface.len())]
    }

    /// A call to a previously-defined function (arity-correct) if any exist, else fall back to an
    /// atom. Sometimes NAMED-arg form (`f(p0=…, …)` — the positional-after-named binding rules).
    fn user_call(&mut self) -> String {
        if self.funcs.is_empty() {
            return self.atom();
        }
        let idx = self.below(self.funcs.len());
        let (name, params) = self.funcs[idx].clone();
        if params.is_empty() {
            return format!("{name}()");
        }
        if self.chance(0.3) {
            // named first param + positionals for the rest — the AH.2.4 lowest-unfilled rule
            let named = format!("{}={}", params[0], self.expr());
            let rest: Vec<String> = (1..params.len()).map(|_| self.expr()).collect();
            let mut args = vec![named];
            args.extend(rest);
            return format!("{name}({})", args.join(", "));
        }
        let args: Vec<String> = (0..params.len()).map(|_| self.expr()).collect();
        format!("{name}({})", args.join(", "))
    }

    /// A leaf: an in-scope variable, or a literal (number / bool / string / undef / IEEE specials).
    fn atom(&mut self) -> String {
        if !self.vars.is_empty() && self.chance(0.5) {
            let idx = self.below(self.vars.len());
            return self.vars[idx].clone();
        }
        match self.below(9) {
            0..=2 => self.int_between(-50, 50).to_string(),
            3 => {
                // a small decimal
                let n = self.int_between(-500, 500);
                format!("{}.{}", n / 10, (n.abs() % 10))
            }
            4 => if self.chance(0.5) { "true" } else { "false" }.to_string(),
            5 => "undef".to_string(),
            6 => if self.chance(0.5) {
                "(1 / 0)"
            } else {
                "(0 / 0)"
            }
            .to_string(),
            7 => format!(
                "\"{}\"",
                self.pick_str(&["a", "x", "hello", "", "\u{2192}", "\\u0041"])
            ),
            _ => format!("\"{}\"", self.pick_str(&["a", "x", "hello", ""])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BUILTINS, Builtins, Domain, Gen, NativeSurface, Profile, Surface, generate};

    /// Determinism: a seed maps to exactly one program, always (the reproducible-replay guarantee).
    #[test]
    fn seed_is_reproducible() {
        for seed in [0u32, 1, 42, 9999, u32::MAX] {
            assert_eq!(
                generate(seed),
                generate(seed),
                "seed {seed} must replay identically"
            );
        }
    }

    /// Different seeds generally differ (a sanity check that the walk actually branches on the RNG).
    #[test]
    fn seeds_differ() {
        let a = generate(1);
        let b = generate(2);
        assert_ne!(
            a, b,
            "distinct seeds should (almost always) give distinct programs"
        );
    }

    /// AJ.1 — the COVERAGE GATE: every language-construct family must appear somewhere in the
    /// first N seeds' output. This is what keeps the grammar honest: a language feature that
    /// ships without a generator production fails HERE, in CI, with the missing family named.
    /// Markers are cheap substrings, deliberately chosen to be unambiguous in emitted text.
    #[test]
    fn grammar_covers_the_language_surface() {
        const SEEDS: u32 = 4000;
        let mut corpus = String::new();
        for seed in 0..SEEDS {
            corpus.push_str(&generate(seed));
            corpus.push('\n');
        }
        let families: &[(&str, &str)] = &[
            // statements + geometry tree
            ("user module def", "module "),
            ("children()", "children("),
            ("$children read", "$children"),
            ("intersection_for", "intersection_for"),
            ("modifier %", "%"),
            ("modifier #", "#"),
            ("if statement", "if ("),
            ("$-assignment", "$fn = "),
            ("$-arg at call", "$fn="),
            // 2D + extrudes + wrappers
            ("square", "square("),
            ("circle", "circle("),
            ("polygon", "polygon("),
            ("text", "text("),
            ("linear_extrude", "linear_extrude("),
            ("rotate_extrude", "rotate_extrude("),
            ("offset", "offset("),
            ("projection", "projection("),
            ("color", "color("),
            ("resize", "resize("),
            ("mirror", "mirror("),
            ("multmatrix", "multmatrix("),
            ("hull", "hull("),
            ("minkowski", "minkowski("),
            // expression forms
            ("let chain", "let("),
            ("assert chain", "assert("),
            ("echo chain", "echo("),
            ("each splice", "each "),
            ("comprehension", "[for ("),
            ("C-style for", " = 0; "),
            ("comprehension if", ") if ("),
            ("indexing", ")["),
            ("swizzle .x", ".x"),
            ("multi-swizzle", ".wy"),
            ("function literal", "function("),
            ("named user arg", "0="),
            ("bit and", " & "),
            ("bit or", " | "),
            ("shift left", " << "),
            ("shift right", " >> "),
            ("power", " ^ "),
            ("undef literal", "undef"),
            ("inf arithmetic", "(1 / 0)"),
            ("nan arithmetic", "(0 / 0)"),
            ("unicode string", "\u{2192}"),
            // objects + methods + metrics + string/table builtins
            ("object constructor", "object("),
            ("object edit list", "[[\""),
            ("method this", "this"),
            ("has_key", "has_key("),
            ("is_object", "is_object("),
            ("textmetrics", "textmetrics("),
            ("fontmetrics", "fontmetrics("),
            ("str", "str("),
            ("chr", "chr("),
            ("ord", "ord("),
            ("search", "search("),
            ("lookup", "lookup("),
            ("concat", "concat("),
            ("seeded rands", "rands("),
            ("type predicate", "is_num("),
            ("expression import", "import("),
            ("dxf_dim", "dxf_dim("),
        ];
        let missing: Vec<&str> = families
            .iter()
            .filter(|(_, marker)| !corpus.contains(marker))
            .map(|(family, _)| *family)
            .collect();
        assert!(
            missing.is_empty(),
            "grammar never emitted {} of {} families across {SEEDS} seeds: {missing:?}",
            missing.len(),
            families.len()
        );
    }

    /// AR.4, made executable per-decl: a domain-generated call to EVERY builtin evaluates to a
    /// VALUE, never `undef`. This is the whole point of carrying domains — a wrong-typed argument
    /// is an instant `undef` that times as ~0, so if any decl's domains drift into producing undef,
    /// the heavy lane silently goes back to measuring error handling. NaN/inf stay legal (`tan(90)`
    /// is a number); `undef` is the "did no work" tell.
    #[test]
    fn every_declared_call_computes() {
        use fab_lang::{Scope, StmtKind, Value, eval_expr, parse};
        let profile = Profile::heavy(2);
        for (i, d) in BUILTINS.iter().enumerate() {
            for seed in 0..40u32 {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "i is a table index under 64"
                )]
                let mut g = Gen::with_profile(seed * 100 + i as u32, profile);
                let args: Vec<String> =
                    d.params.iter().map(|pm| g.domain_expr(pm.domain)).collect();
                let call = format!("{}({})", d.name, args.join(", "));
                let src = format!("function __t() = {call};");
                let prog = parse(&src)
                    .unwrap_or_else(|e| panic!("{}: `{call}` fails to parse: {e:?}", d.name));
                let StmtKind::FunctionDef { body, .. } = &prog.stmts[0].kind else {
                    panic!("{src} did not parse as a function def");
                };
                let v = eval_expr(body, &Scope::new())
                    .unwrap_or_else(|e| panic!("{}: `{call}` errored: {e:?}", d.name));
                assert!(
                    !matches!(v, Value::Undef),
                    "{}: `{call}` evaluated to undef — a timed call that did NO work (AR.4); its \
                     declared domains have drifted out of usefulness",
                    d.name
                );
            }
        }
    }

    /// Every generated program PARSES — the "valid by construction" contract. If this ever fails, the grammar
    /// emitted something the parser rejects, which is a generator bug, not an evaluator finding.
    #[test]
    fn generated_programs_parse() {
        for seed in 0..2000u32 {
            let src = generate(seed);
            assert!(
                fab_lang::parse(&src).is_ok(),
                "seed {seed} produced an unparseable program:\n{src}"
            );
        }
    }

    /// AR.3 — the seam must not have moved the generator. `pick_builtin` indexes the surface with the
    /// RNG, so if `Builtins` disagreed with the old const by a single position, every accumulated fuzz
    /// seed would silently start generating a DIFFERENT program: the corpus files survive, their
    /// meaning doesn't. Cheapest possible guard, and the one that actually matters.
    #[test]
    fn the_builtin_surface_is_byte_for_byte_the_old_table() {
        let s = Builtins;
        assert_eq!(s.decls().len(), BUILTINS.len());
        for (a, b) in s.decls().iter().zip(BUILTINS) {
            assert_eq!(a.name, b.name, "order is load-bearing — RNG indexes it");
            assert_eq!(a.params.len(), b.params.len());
        }
        assert_eq!(s.preamble(), "", "builtins need no preamble");
    }

    /// The same seeds must still produce the same programs. A surface refactor that changed generated
    /// output would invalidate every corpus in `lang/fuzz/corpus` without failing anything.
    #[test]
    fn seeds_still_generate_the_same_programs() {
        for seed in [0u32, 1, 7, 42, 1337, 99_991] {
            let a = generate(seed);
            let b = generate(seed);
            assert_eq!(a, b, "generation is deterministic");
            assert!(!a.is_empty(), "seed {seed} generated nothing");
        }
    }

    /// AR.3's payoff: the native surface is DERIVED, so it describes the registry without anyone
    /// maintaining a second table. The properties that matter downstream are that it is non-empty,
    /// that names bind (the AN.14 family is unreachable otherwise), and that defaults survived — a
    /// generator that always fills every argument cannot reach AN.3.
    #[test]
    fn the_native_surface_is_derived_from_the_registry() {
        let s = NativeSurface::from_registry("include <BOSL2/std.scad>\n");
        assert!(!s.decls().is_empty(), "the registry has entries");
        assert_eq!(s.decls().len(), fab_lang::native_surface().len());
        assert!(
            s.decls().iter().all(|d| d.names_bind),
            "these stand in for USER functions — names bind, unlike builtins"
        );
        assert!(
            s.decls()
                .iter()
                .any(|d| d.params.iter().any(|p| !p.required)),
            "defaulted params survived the derivation"
        );
        assert!(
            s.decls().iter().all(|d| !d.name.is_empty()),
            "every decl names itself"
        );
        assert!(s.preamble().contains("BOSL2"), "the preamble rides along");
    }

    /// AR.3.2 — generating INTO a surface, the thing the whole declaration exists for. A program that
    /// calls the natives by name, with their real parameter names, is what a transpiled-library
    /// differential needs; before this the generator could only ever call builtins.
    #[test]
    fn generating_against_the_native_surface_calls_the_natives() {
        let surface = NativeSurface::from_registry("include <BOSL2/std.scad>\n");
        let names: Vec<&str> = surface.decls().iter().map(|d| d.name).collect();
        let mut saw_a_native = false;
        for seed in 0u32..40 {
            let src = super::generate_against(seed, Profile::CHEAP, &surface);
            assert!(
                src.starts_with("include <BOSL2/std.scad>"),
                "seed {seed}: the preamble must lead, or the calls resolve to nothing"
            );
            if names.iter().any(|n| src.contains(&format!("{n}("))) {
                saw_a_native = true;
            }
        }
        assert!(
            saw_a_native,
            "40 seeds against the native surface and not one call into it — the surface is not wired"
        );
    }

    /// A surface swap MUST change what a seed means, and that is why `generate_against` is a separate
    /// entry point rather than a parameter on `generate`: the accumulated corpora belong to the builtin
    /// surface. If this ever passes by accident (identical output), the surface isn't reaching the RNG.
    #[test]
    fn a_seed_means_something_different_under_a_different_surface() {
        let native = NativeSurface::from_registry("");
        let differ = (0u32..20)
            .any(|seed| super::generate_against(seed, Profile::CHEAP, &native) != generate(seed));
        assert!(differ, "the surface never reached the generator");
    }

    /// AR.3.3 — the derived domains reach the generated CALL. `v_theta`'s body tests `is_vector` on
    /// its argument, so a heavy-lane call must hand it a numeric vector rather than whatever the RNG
    /// felt like. This is the difference between a corpus that measures work and one that measures
    /// `undef` (AR.4): a wrongly-typed argument bails instantly, and the seed still "passes".
    ///
    /// Heavy profile on purpose — `CHEAP` sets `domains: false` deliberately, to keep its frozen bytes
    /// and to cover type MISMATCH. Asserting against CHEAP would test the opposite of the intent.
    #[test]
    fn derived_domains_reach_the_generated_call() {
        let surface = NativeSurface::from_registry("include <BOSL2/std.scad>\n");
        let vector_params: Vec<&str> = surface
            .decls()
            .iter()
            .filter(|d| d.params.iter().any(|p| p.domain == Domain::VecN))
            .map(|d| d.name)
            .collect();
        assert!(
            !vector_params.is_empty(),
            "no native derived a vector parameter — the domain walk is not reaching is_vector"
        );

        // Over a spread of seeds, at least one call to a vector-taking native must pass a LIST.
        let mut saw_typed_call = false;
        for seed in 0u32..60 {
            let src = super::generate_against(seed, Profile::heavy(2), &surface);
            for name in &vector_params {
                if let Some(i) = src.find(&format!("{name}(")) {
                    let tail = &src[i + name.len() + 1..];
                    if tail.starts_with('[') {
                        saw_typed_call = true;
                    }
                }
            }
        }
        assert!(
            saw_typed_call,
            "60 heavy seeds and not one vector-taking native got a list — domains are not reaching \
             the call, so the corpus is measuring undef"
        );
    }
}
