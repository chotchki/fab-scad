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
    /// Emit SEEDLESS 3-arg `rands` (AS.5) — the ONE impure builtin, drawing from the evaluator's
    /// advancing stream.
    ///
    /// OFF in every oracle-diffed lane and ON only in [`Profile::AB`], because the two sides of
    /// that fence disagree about determinism: OUR seedless stream starts from a fixed seed (the
    /// determinism doctrine), while upstream's global engine is seeded from entropy — so a seedless
    /// draw echoed at the oracle is a PERMANENT gen-diff divergence, but echoed across an A/B run
    /// of our own evaluator it is bit-stable unless something real broke (a cache memoizing across
    /// a stream advance, a tier compiling the stream wrong). Before this flag NOTHING ever
    /// generated the seedless form — `compile_seedless_rands` had zero fuzz coverage. Like the
    /// other flags, OFF gates the RNG draw too, so the cheap stream is untouched.
    pub seedless_rands: bool,
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
        seedless_rands: false,
    };

    /// The A/B differential surface: CHEAP plus seedless `rands`. The three A/B fuzz targets
    /// (`gen_diff`, `jit_dispatch_diff`, `intrinsics_dispatch_diff`) generate against THIS — both
    /// of their legs run our evaluator, where seedless draws are deterministic — while every
    /// oracle-diffed lane stays on CHEAP. See [`Profile::seedless_rands`] for why the split exists.
    pub const AB: Self = Self {
        seedless_rands: true,
        ..Self::CHEAP
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
            // Heavy is the gen-perf lane, which diffs echoes against the REAL oracle — where
            // seedless rands is a permanent divergence. A/B-only, see the field docs.
            seedless_rands: false,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::CHEAP
    }
}

// AR.14.1 — the declaration types live in fab-lang now, because a GENERATED library crate has to
// implement this trait and must not depend on the fuzzer to describe itself. Re-exported rather
// than re-declared: two copies of `Domain` in one tree is precisely the drift this phase exists to
// kill, and the AR.5a finding is what it costs when three hand-kept lists disagree.
pub use fab_lang::surface::{
    ConstDecl, Decl, Domain, Kind, LibrarySurface, Param, Root, satisfies,
};

use fab_lang::surface::builtin_decl;

/// Builtins the generator emits calls to — the GENERATION surface, as DECLS.
///
/// ORDER AND LENGTH ARE FROZEN and owned HERE: `pick_builtin` indexes this table with the RNG, so
/// reordering or resizing it moves every subsequent draw and breaks the `cheap` corpus digest.
/// Additions are APPEND-ONLY and each is a deliberate re-baseline event — `below(len)` puts the
/// LENGTH in the RNG math, so even an append re-points accumulated corpora.
///
/// The CONTENT — name, arity, parameter names, domains — comes from the ONE declaration (AS.5,
/// `fab_lang::surface::BUILTIN_SURFACE`): a builtin cannot be described here differently from what
/// the evaluator implements, and a typo'd or vanished name is a COMPILE error, because
/// [`builtin_decl`] is a const lookup that panics at const-eval. The domains are GENERATION
/// domains, not acceptance claims — they answer "what argument makes this call compute" (AR.4);
/// the rationale per entry (`len` wants `VecN`, `pow`'s base is `Pos`, `search`'s key is `Num`
/// because of upstream #5017) lives ON the declaration rows now. Only the heavy lane reads them —
/// cheap's arbitrary arguments still cover the mismatch space, where the FINDINGS live.
const BUILTINS: &[Decl] = &[
    builtin_decl("sin"),
    builtin_decl("cos"),
    builtin_decl("tan"),
    builtin_decl("asin"),
    builtin_decl("acos"),
    builtin_decl("atan"),
    builtin_decl("sqrt"),
    builtin_decl("abs"),
    builtin_decl("floor"),
    builtin_decl("ceil"),
    builtin_decl("round"),
    builtin_decl("ln"),
    builtin_decl("exp"),
    builtin_decl("sign"),
    builtin_decl("norm"),
    builtin_decl("len"),
    builtin_decl("pow"),
    builtin_decl("atan2"),
    builtin_decl("min"),
    builtin_decl("max"),
    builtin_decl("cross"),
    // list + string group (AJ.4)
    builtin_decl("str"),
    builtin_decl("chr"),
    builtin_decl("ord"),
    builtin_decl("concat"),
    builtin_decl("search"),
    builtin_decl("lookup"),
    // type predicates — genuinely Any, that is the point of them
    builtin_decl("is_num"),
    builtin_decl("is_undef"),
    builtin_decl("is_string"),
    builtin_decl("is_list"),
    builtin_decl("is_bool"),
    builtin_decl("is_object"),
    // ── APPENDED 2026-07-28 (AS.5): the pure builtins nothing had ever generated a call to.
    // The append re-pointed every accumulated corpus (below(len) — see the table doc) and the
    // digest + fingerprints were re-baselined in the same commit, deliberately.
    builtin_decl("log"),
    builtin_decl("is_function"),
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

/// The impl `fab_lang::surface`'s module doc promised (AS.5, closing the D4 gap): builtins as a
/// [`LibrarySurface`] — no roots, no preamble, no constants, no natives, just callables.
///
/// `callables` is the GENERATION-SAFE subset in the frozen corpus order, not the full declared
/// surface: the trait's contract is a stable RNG-indexable table, and the context builtins
/// (`rands`, `parent_module`, the metrics pair, `object`) are generated by dedicated productions
/// instead — see `fab_lang::surface::BUILTIN_SURFACE` for the complete declaration.
impl LibrarySurface for Builtins {
    fn name(&self) -> &'static str {
        "builtins"
    }

    fn callables(&self) -> &'static [Decl] {
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

/// [`generate`] for the A/B differential lanes ([`Profile::AB`]): CHEAP plus seedless `rands`,
/// which is deterministic when BOTH legs are our evaluator and a permanent divergence against the
/// real oracle — see [`Profile::seedless_rands`]. A separate entry point for the same reason as
/// [`generate_against`]: an AB seed and a CHEAP seed are different programs, and each lane's
/// corpus must keep meaning what it meant.
#[must_use]
pub fn generate_ab(seed: u32) -> String {
    generate_with(seed, Profile::AB)
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

    /// textmetrics/fontmetrics (deterministic — bundled font) and rands: SEEDED everywhere (a pure
    /// function of its args, oracle-exact), SEEDLESS only when the profile allows it (A/B lanes —
    /// see [`Profile::seedless_rands`] for the determinism fence that keeps it out of oracle runs).
    fn metrics_or_rands(&mut self) -> String {
        let arms = if self.profile.seedless_rands { 5 } else { 4 };
        match self.below(arms) {
            0 => format!(
                "textmetrics(\"{}\", size = {}).advance",
                self.pick_str(&["hi", "fab"]),
                self.int_between(2, 12)
            ),
            1 => "fontmetrics().interline".to_string(),
            2 | 3 => format!(
                "rands(0, 1, {}, {})",
                self.int_between(1, 3),
                self.int_between(0, 999)
            ),
            // Seedless: advances the evaluator's ONE stream — the draw the JIT compiles via
            // compile_seedless_rands and the eval-memo fences on, neither of which any generated
            // program exercised before AS.5.
            _ => format!("rands(0, 1, {})", self.int_between(1, 3)),
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
    use super::{
        BUILTINS, Builtins, Domain, Gen, NativeSurface, Profile, Surface, generate, generate_ab,
    };

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

    /// AR.14.1 — seed stability ACROSS VERSIONS, which the determinism test above cannot see: it
    /// compares a run against itself, so a refactor that re-points every seed at a different
    /// program passes it unchanged.
    ///
    /// That is not hypothetical. `pick_builtin` indexes the surface with the RNG, so ANY change to
    /// the decl table's contents or order silently re-points every accumulated fuzz corpus — the
    /// seeds still generate, still label, still report a ratio, and no longer mean what they meant
    /// when they were minimized. Pinning the bytes is the only thing that notices.
    ///
    /// Baselined 2026-07-28 against commit 4f49c264, before the declaration types moved into
    /// fab-lang. A deliberate generation change updates these hashes IN THE SAME COMMIT that
    /// explains why; an accidental one fails here by name.
    #[test]
    fn seeds_still_generate_the_same_bytes() {
        // (seed, len, first 16 bytes) — a cheap fingerprint that needs no hasher dependency and
        // still pins both the shape and the content of the emitted program.
        // Re-baselined 2026-07-28 (AS.5): appending log/is_function to BUILTINS changed
        // `floor(u * len)` on builtin picks — seeds 7 and 1337 moved, the other three did not
        // (the draw COUNT per program is unchanged, only where a pick lands).
        const PINNED: &[(u32, usize, &str)] = &[
            (0, 838, "union() {\n  sphe"),
            (7, 649, "$fn = 4;\ncube([6"),
            (42, 854, "intersection_for"),
            (1337, 798, "v2 = [for (i0 = "),
            (99_991, 545, "$fn = 9;\nv0 = -2"),
        ];
        let mut drifted = Vec::new();
        for &(seed, len, head) in PINNED {
            let got = generate(seed);
            let got_head: String = got.chars().take(16).collect();
            if got.len() != len || got_head != head {
                drifted.push(format!(
                    "seed {seed}: len {} head {got_head:?} (pinned len {len} head {head:?})",
                    got.len()
                ));
            }
        }
        assert!(
            drifted.is_empty(),
            "generated programs CHANGED — every accumulated fuzz corpus now means something \
             different than when it was minimized:\n  {}",
            drifted.join("\n  ")
        );
    }

    /// AS.5's payoff, as a RATCHET: every builtin the ONE declaration lists is REACHABLE by the
    /// generator — through the RNG-indexed table or a dedicated production — or carries a
    /// documented exemption. Before this, five pure builtins had never had a generated call and no
    /// artifact could see it, because nothing ever compared the fuzzer's list to the evaluator's.
    #[test]
    fn every_declared_builtin_is_reachable_by_the_generator() {
        // Emitted by dedicated grammar PRODUCTIONS rather than the table, because each needs a
        // specific shape: object/has_key (`object_expr`), the metrics pair + rands
        // (`metrics_or_rands` — seeded everywhere, seedless under `Profile::AB`).
        const PRODUCTIONS: &[&str] = &["object", "has_key", "textmetrics", "fontmetrics", "rands"];
        // Deliberately NOT generated, each for a stated reason. Shrink this list; never grow it
        // silently.
        // - version/version_num: pinned to 2021.01 while the nightly oracle reports its build
        //   date, so any generated echo of them is a PERMANENT gen-diff divergence. They are
        //   constants — pinned by fab-lang's `pure_rows_dispatch_through_the_declaration` instead.
        // - parent_module: answers off the module-instantiation stack, which a generated program's
        //   top level does not have — an emitted call would be undef-only, which
        //   `every_declared_call_computes` rightly rejects. Generatable once the walk emits module
        //   DEFINITIONS (the AR.20 family's territory).
        const EXEMPT: &[&str] = &["version", "version_num", "parent_module"];
        let table: std::collections::BTreeSet<&str> = BUILTINS.iter().map(|d| d.name).collect();
        for b in fab_lang::surface::BUILTIN_SURFACE {
            let name = b.decl.name;
            let covered = usize::from(table.contains(name))
                + usize::from(PRODUCTIONS.contains(&name))
                + usize::from(EXEMPT.contains(&name));
            assert_eq!(
                covered, 1,
                "`{name}` must be exactly one of table-generated / production-generated / exempt \
                 (found {covered} of those) — a new builtin gets a table append (a deliberate \
                 re-baseline), a production, or a REASON here"
            );
        }
        // The productions really fire: each claimed name appears in emitted AB-surface programs
        // (AB so the seedless rands arm is reachable). 512 seeds is far past the point where every
        // production arm has drawn.
        for name in PRODUCTIONS {
            let marker = format!("{name}(");
            assert!(
                (0..512).any(|s| generate_ab(s).contains(&marker)),
                "production-claimed `{name}` never appears in 512 AB programs"
            );
        }
        // And the SEEDLESS form specifically (3-arg — no trailing seed argument): the whole reason
        // Profile::AB exists. Matches `rands(0, 1, N)` where N is a single digit.
        let seedless = (0..512).map(generate_ab).any(|p| {
            p.match_indices("rands(0, 1, ")
                .any(|(i, _)| p[i..].chars().nth(13) == Some(')'))
        });
        assert!(
            seedless,
            "the seedless rands arm never fired in 512 AB programs"
        );
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
