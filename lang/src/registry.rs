//! AR.26.1 — the dispatch REGISTRY, as a value a consumer builds and hands in.
//!
//! Two things live here. The ROW TYPES ([`Entry`], [`ModuleEntry`]) — what one compiled callable
//! declares about itself and everything that has to be true before it may stand in for a library
//! function. And the [`Registry`] — an accumulated INDEX over rows from one or more libraries, which
//! is what dispatch actually consults.
//!
//! WHY IT IS A VALUE. Until now the index was three process-lifetime `OnceLock`s keyed on nothing,
//! which is sound only while exactly one immutable registry exists per process — and that is the
//! assumption a library-per-crate topology exists to break. BOSL2 plus machineblocks plus a user's
//! own library is the normal case, not the exception. So a consumer accumulates the libraries it
//! loaded and hands the result to evaluation; `Config::intrinsics` stays the per-eval ON/OFF toggle,
//! because AR.2's differential has to turn natives off without changing which libraries are PRESENT.
//!
//! THE ROWS ARE `&'static`, THE INDEX IS NOT. A generated library's rows live in its own statics, so
//! the index can hold `&'static Entry` while the map itself is per-instance — which is what keeps
//! every lookup's return type unchanged and keeps a second lifetime out of `Ctx`.
//!
//! ROWS CARRY SOURCE, NOT A HASH. A row hands over the VERBATIM reference it was generated from and
//! the registry parses and fingerprints it here. The gate hash is therefore computed by our own
//! parser from bytes the row author supplied, never asserted by the row author — which is the whole
//! never-silently-wrong contract in one sentence, and the reason [`crate::surface::Fingerprint`]
//! needs no public constructor.

use std::collections::BTreeMap;
use std::sync::{LazyLock, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::surface::Fingerprint;
use crate::{Expr, Parameter, Scope, Value};

/// A native implementation of a specific user function. Receives the call's POSITIONAL argument
/// VALUES (already evaluated, in source order) and returns the result — the same `Value` the interpreted body
/// would, or the same ERROR (a BOSL2 function with an inline `assert(…)` raises when the assert fails, so the
/// ABI is fallible; the native reproduces the assert's CONTROL FLOW — it errors where the body errors — not
/// its diagnostic string, which is a locator, not output). The dispatch gate only routes
/// all-positional calls here, so the args stay a flat slice. An intrinsic implements the WHOLE function for
/// the arg shapes it accepts; it hardcodes the reference's parameter defaults (it matches that exact source),
/// so a short positional call still gets it.
///
/// AR.17: the ctx is the ONE capability a native may use beyond its args — invoking a function
/// VALUE ([`crate::surface::FnCtx`], deliberately that narrow). A native that never reaches a
/// closure ignores it (`_fx`), and purity is preserved WHERE IT MATTERS by the trait's surface,
/// not by the absence of a parameter — the module side's `ModuleCtx` discipline one level down.
pub type Intrinsic = fn(&dyn crate::surface::FnCtx, &[Value]) -> crate::Result<Value>;

/// A named top-level constant + a builder for its expected `Value` (statics can't hold one directly).
pub type ValueConst = (&'static str, fn() -> Value);

/// A compiled module: geometry out, with the evaluator reachable through `ModuleCtx`.
pub type ModuleNative = fn(&dyn crate::surface::ModuleCtx) -> crate::Result<crate::Geo>;

/// One registered intrinsic: the exact function it stands in for. `reference` is the VERBATIM source of that
/// function (one `function name(params) = body;`) — the single source of truth: its fingerprint gates
/// dispatch, and the fast==slow harness runs its interpreted body as the oracle the `func` must bit-match.
///
/// FIELDS ARE PUBLIC and stay that way: a generated library crate writes these as struct literals into
/// its own `OUT_DIR`, so anything that made a row unconstructible from outside would make the whole
/// topology impossible.
pub struct Entry {
    /// The function name the intrinsic implements (registry bucket key).
    pub name: &'static str,
    /// The verbatim reference source of that function — fingerprinted + run as the harness oracle.
    /// Public for the transpiler (AR.5): the analysis pass derives the guard sets below FROM
    /// this source, and the registry's hand lists are its acceptance oracle.
    pub reference: &'static str,
    /// Named TOP-LEVEL CONSTANTS the reference hardcodes (default exprs like `eps=_EPSILON`, or body reads
    /// — `PI` counts too, it's just a seeded binding), with the value the native impl bakes in. Empty =
    /// self-contained. Non-empty makes the entry CONST-GUARDED (O.5.1): the fingerprint proves the FUNCTION
    /// source, not the constants it names, so a user override (`_EPSILON = 1e-6;`) would make the baked
    /// value silently wrong. Guarded entries never wire at ctx build and arm ONLY after island globals are
    /// built, when each named constant's BOUND value in the fn's home-island global bit-matches — see
    /// `arm_guarded_intrinsics`. Mismatch (or mid-hoist, before globals exist) → interpreted: the
    /// worst case stays "missed speedup, never a wrong answer".
    pub consts: &'static [(&'static str, f64)],
    /// The VALUE-typed half of the const guard (O.8): named top-level constants whose baked value is NOT a
    /// number — BOSL2's direction vectors (`UP`/`RIGHT`) and sentinels (`_NO_ARG`). Each `fn()` builds the
    /// expected `Value` (statics can't hold one); the arm step compares it against the home-scope binding
    /// BIT-level (`value_bits_eq`: f64s by `to_bits`, exact variant, recursive) — same
    /// wire-only-if-proven contract as `consts`, same post-hoist arm timing.
    pub consts_v: &'static [ValueConst],
    /// USER-FUNCTION names interpreting the reference can reach (O.5.2 dep pins), TRANSITIVELY CLOSED by the
    /// author over every arg shape the native accepts (`select` → `is_vector`/`is_range` → `is_finite` →
    /// `is_nan`; a branch no accepted arg shape can reach — `all_nonzero` behind select's fixed 1-arg
    /// `is_vector(start)` — is excluded). The entry wires only if each dep's DEFINED body fingerprints to
    /// that dep's own registry/pin reference — the fingerprint gate extended one hop, because the
    /// native bakes the dep's semantics without the dep's own fingerprint ever being consulted at dispatch.
    pub deps: &'static [&'static str],
    /// BUILTIN names interpreting the reference (or a pinned dep) can reach. A user function may SHADOW a
    /// builtin (dispatch resolves user fns first — BOSL2 itself shadows `reverse`), which would reroute the
    /// interpreted body while the native keeps the real builtin. The entry wires only if none of these names
    /// has a user-function definition.
    pub builtins: &'static [&'static str],
    /// The native implementation.
    pub func: Intrinsic,
}

/// One module the registry implements natively. Same contract as [`Entry`] one tier up, and the same
/// reason its fields are public.
pub struct ModuleEntry {
    /// The module name dispatch keys on.
    pub name: &'static str,
    /// Verbatim source of the module it stands in for — fingerprinted, and run as the harness
    /// oracle. Same contract as [`Entry::reference`]: the native wires only against a definition that
    /// matches this structurally.
    pub reference: &'static str,
    /// The compiled implementation.
    pub func: ModuleNative,
    /// Named top-level constants the emitted body BAKES (AR.14.4 band 2) — the module twin of
    /// [`Entry::consts_v`], value-typed from the start because the library fold hands back whole
    /// `Value`s (BOSL2's direction vectors, not just numbers). The fingerprint proves the module's
    /// SOURCE, not the constants it names, so [`Registry::resolve_module`] refuses to wire unless each
    /// name's binding in the body's lexical base bit-matches the baked expectation. Checked per
    /// RESOLUTION rather than at a separate arm step, because module dispatch already holds the home
    /// scope at the call site — there is no earlier moment with more information. Empty = band 1.
    pub consts: &'static [ValueConst],
}

/// Everything ONE library contributes to a registry, as static data.
///
/// A library hands over three lists because they answer three different questions: which functions it
/// COMPILED, which modules it compiled, and which functions it merely PINS — reference-only anchors it
/// ships no native for, present so a depending row's dep-fingerprint gate has something to check
/// against.
#[derive(Clone, Copy, Default)]
pub struct Rows {
    /// How the library names itself in a fault. `""` for an anonymous set (tests, probes).
    pub name: &'static str,
    /// Compiled functions.
    pub functions: &'static [Entry],
    /// Compiled modules.
    pub modules: &'static [ModuleEntry],
    /// `(name, verbatim source)` anchors for functions a row DEPENDS on without our compiling them.
    pub pins: &'static [(&'static str, &'static str)],
}

/// A row that could not be indexed, and why. Recorded rather than thrown, because the safe direction
/// is unambiguous: a row that does not enter the index simply never dispatches, which costs a
/// speedup and can never cost an answer. Recorded rather than SWALLOWED, because a native that
/// quietly stops firing is the exact failure this phase exists to make visible — see
/// [`Registry::faults`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// ONE library declares the same function name twice. The FIRST is kept: a later row silently
    /// winning is the last-wins trap that already cost a bug upstream (`_sort_vectors`), and within a
    /// single library there is no principled way to pick — a library that cannot say which of two
    /// rows implements a name has an authoring bug, not a topology.
    ///
    /// WITHIN a library, deliberately. Two DIFFERENT libraries declaring the same name is normal and
    /// is not a fault — see [`Registry::eclipsed`].
    DuplicateFunction {
        /// The library that lost the collision.
        library: &'static str,
        /// The colliding name.
        name: &'static str,
    },
    /// Two rows claim the same module name. First wins, same reasoning.
    ///
    /// Still cross-library, unlike [`Fault::DuplicateFunction`]: a module native has no fingerprint
    /// SELECTION step to disambiguate with — [`Registry::resolve_module`] fetches one row by name and
    /// then checks it — so two libraries claiming one module name really is unresolvable here.
    DuplicateModule {
        /// The library that lost the collision.
        library: &'static str,
        /// The colliding name.
        name: &'static str,
    },
    /// ONE library pins the same name twice. First wins, same reasoning as
    /// [`Fault::DuplicateFunction`]; pins are library-local, so two libraries pinning one name is
    /// not a collision at all.
    DuplicatePin {
        /// The library that lost the collision.
        library: &'static str,
        /// The colliding name.
        name: &'static str,
    },
    /// A `reference` did not parse to exactly one function (or module) definition, so there is
    /// nothing to fingerprint and the row can never wire. An authoring bug in the emitting library.
    UnparseableReference {
        /// The library that supplied it.
        library: &'static str,
        /// The row's name.
        name: &'static str,
    },
}

/// How a defined function relates to a registry — the EXPLAIN classification (O.3).
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// An intrinsic is registered for this name AND the body fingerprint matches → native dispatch will fire.
    Wired,
    /// An intrinsic is registered for this NAME, but the defined body fingerprints DIFFERENTLY (a BOSL2
    /// revision the intrinsic's reference doesn't match) → it silently INTERPRETS. The actionable case:
    /// either the user's library drifted, or the intrinsic's reference source is stale and needs updating.
    Drift,
    /// No intrinsic registered for this name — the ordinary interpreted function (the vast majority).
    NotRegistered,
}

/// How many registries this process has built. The anti-regression instrument for the one way this
/// design silently costs real money: a registry's index parses and fingerprints every reference it was
/// handed, so a refactor that moved construction inside an evaluation loop would pay a full library
/// parse per run. Counts row-set HAND-OVERS ([`Registry::with`] calls) rather than finished
/// registries — the two differ only by a constant per registry, and it is the one number available
/// before the lazy indexes decide whether to do any work at all. A test asserts it does not move
/// across ordinary evaluations.
#[doc(hidden)]
#[must_use]
pub fn build_count() -> u64 {
    BUILDS.load(Ordering::Relaxed)
}

static BUILDS: AtomicU64 = AtomicU64::new(0);

/// The accumulated index dispatch consults: `name → (reference fingerprint, row)` for every library
/// handed in.
///
/// The cross-links between rows stay BY NAME rather than becoming pointers, and that is not a
/// simplification: of the three uses a dep name has, two cannot be an address at all — it keys into
/// the USER's function table, which does not exist until a program is loaded, and it is compared
/// against the native's own PARAMETER names for the AN.10 shadow check. Only fetching the expected
/// fingerprint addresses the registry, and that is a map hit.
///
/// NO `Default` IMPL, deliberately: an empty registry and the builtin one are both defensible
/// defaults and they mean opposite things, so `Registry::default()` would be a coin flip nobody
/// reading the call site could resolve. [`Registry::new`] is empty and says so; the shim default
/// lives on `&Registry` (below) where it has exactly one meaning.
pub struct Registry {
    /// The row sets as handed in, UNINDEXED. Indexing means parsing and fingerprinting every
    /// `reference`, and both halves below defer it — see [`Registry::fn_index`].
    rows: Vec<Rows>,
    /// The function + pin index, built on first function-tier question.
    fns: OnceLock<FnIndex>,
    /// The module index, built on first module-tier question — SEPARATELY, because the two halves
    /// cost wildly different amounts and are wanted by different programs: 117 function references
    /// against BOSL2's 419 module ones, and a value-level battery never asks the second question.
    modules: OnceLock<ModuleIndex>,
}

/// One library's claim on a function name: the fingerprint its `reference` hashes to, WHICH row set
/// handed it over, and the row.
///
/// The library index is the load-bearing field. A dep name is not a global address — it names a
/// function the ASKING library declared or pinned, whose semantics its native inlined — so every
/// anchor question is answered inside one row set and never across the accumulated index.
#[derive(Clone, Copy)]
struct Cand {
    fp: Fingerprint,
    lib: usize,
    entry: &'static Entry,
}

/// The lazily-built function half: rows keyed by name AND fingerprint, plus library-local pins.
///
/// A NAME MAPS TO A LIST, and that is the whole AR.26.4.1 change. Two libraries declaring `is_def`
/// is the normal case, not a collision: the program defines exactly one `is_def`, and the
/// fingerprint gate already decides which reference that body matches. Keying on the name alone
/// forced a choice between them at ACCUMULATION time — before the program that settles it even
/// exists — and dropping the loser took its library's dependents down with it, since a row whose own
/// library's dep vanished from the index can never anchor.
#[derive(Default)]
struct FnIndex {
    /// name → every library's candidate, in accumulation order. Dispatch takes the first whose
    /// fingerprint matches the running body; anchoring takes the one from its own library.
    map: BTreeMap<&'static str, Vec<Cand>>,
    /// `(row set, name)` → pinned reference fingerprint. LIBRARY-LOCAL, because a pin exists for one
    /// library's rows to anchor against and means nothing to anyone else's.
    pins: BTreeMap<(usize, &'static str), Fingerprint>,
    faults: Vec<Fault>,
}

/// The lazily-built module half. Carries its own faults, which is why [`Registry::faults`] forces it.
#[derive(Default)]
struct ModuleIndex {
    map: BTreeMap<&'static str, (Fingerprint, &'static ModuleEntry)>,
    faults: Vec<Fault>,
}

impl Registry {
    /// An empty registry — every function interprets. The base a consumer accumulates onto.
    #[allow(
        clippy::new_without_default,
        reason = "there is no defensible Default: EMPTY and BUILTIN are both plausible and mean \
                  opposite things, so the choice has to be spelled at the call site. The shim \
                  default lives on `&Registry`, where it has exactly one meaning"
    )]
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            fns: OnceLock::new(),
            modules: OnceLock::new(),
        }
    }

    /// Add one library's rows.
    ///
    /// CHEAP BY CONSTRUCTION — it stores slices and nothing else. The parsing and fingerprinting
    /// happens in whichever index is first ASKED a question, because the two halves are wanted by
    /// different programs and cost an order of magnitude apart. MEASURED, not assumed: eager
    /// indexing put 17 ms of BOSL2 module parsing in front of every process, and 1.7 ms of function
    /// parsing in front of programs that define no function at all — which the old three-`OnceLock`
    /// design skipped entirely, since nothing ever looked a name up.
    ///
    /// Rows that cannot be indexed are recorded in [`Registry::faults`] and skipped rather than
    /// aborting the build — see [`Fault`] for why that direction is the safe one.
    ///
    /// Adding rows DISCARDS any index already built, rather than serving a stale one: a consumer
    /// that asks a question mid-build and then hands over another library would otherwise get an
    /// index that has never heard of it, and `faults()` would certify it clean. Costs nothing on the
    /// normal path, which builds the whole registry before asking anything.
    #[must_use]
    pub fn with(mut self, rows: Rows) -> Self {
        self.rows.push(rows);
        self.fns = OnceLock::new();
        self.modules = OnceLock::new();
        BUILDS.fetch_add(1, Ordering::Relaxed);
        self
    }

    /// The function + pin index, built on first use. See [`Registry::with`].
    fn fn_index(&self) -> &FnIndex {
        self.fns.get_or_init(|| {
            let mut idx = FnIndex::default();
            for (lib, rows) in self.rows.iter().enumerate() {
                for entry in rows.functions {
                    let Some(fp) = function_fingerprint(entry.reference) else {
                        idx.faults.push(Fault::UnparseableReference {
                            library: rows.name,
                            name: entry.name,
                        });
                        continue;
                    };
                    let cands = idx.map.entry(entry.name).or_default();
                    // WITHIN this row set only. A candidate from another library is a coexisting
                    // claim, not a duplicate — and it must stay in the list even when it can never
                    // be selected, because its own library's rows anchor their deps against it.
                    if cands.iter().any(|c| c.lib == lib) {
                        idx.faults.push(Fault::DuplicateFunction {
                            library: rows.name,
                            name: entry.name,
                        });
                        continue;
                    }
                    cands.push(Cand { fp, lib, entry });
                }
                for &(name, reference) in rows.pins {
                    let Some(fp) = function_fingerprint(reference) else {
                        idx.faults.push(Fault::UnparseableReference {
                            library: rows.name,
                            name,
                        });
                        continue;
                    };
                    // `contains_key` then insert, never `insert().is_some()` — the latter keeps the
                    // LAST pin, which is the trap this fault is named after.
                    if idx.pins.contains_key(&(lib, name)) {
                        idx.faults.push(Fault::DuplicatePin {
                            library: rows.name,
                            name,
                        });
                        continue;
                    }
                    idx.pins.insert((lib, name), fp);
                }
            }
            idx
        })
    }

    /// The module index, built on first use. See [`Registry::with`].
    fn module_index(&self) -> &ModuleIndex {
        self.modules.get_or_init(|| {
            let mut idx = ModuleIndex::default();
            for rows in &self.rows {
                let library = rows.name;
                for entry in rows.modules {
                    let Some((params, body)) = parse_module_reference(entry.reference) else {
                        idx.faults.push(Fault::UnparseableReference {
                            library,
                            name: entry.name,
                        });
                        continue;
                    };
                    if idx.map.contains_key(entry.name) {
                        idx.faults.push(Fault::DuplicateModule {
                            library,
                            name: entry.name,
                        });
                        continue;
                    }
                    idx.map
                        .insert(entry.name, (module_fingerprint(&params, &body), entry));
                }
            }
            idx
        })
    }

    /// Rows that did not make it into the index. EMPTY is the only acceptable answer for a shipping
    /// library, and a test says so — a non-empty list means some native quietly stopped firing.
    ///
    /// FORCES BOTH lazy halves, because a fault it has not looked for is a fault it cannot report.
    /// Diagnostic path, never the hot one.
    #[must_use]
    pub fn faults(&self) -> Vec<Fault> {
        let mut all = self.fn_index().faults.clone();
        all.extend(self.module_index().faults.iter().cloned());
        all
    }

    /// The registry fab-lang itself ships: its own natives, pins and POC modules, plus the generated
    /// BOSL2 module band — TWO row sets accumulated, which is the composition path taking its own
    /// medicine rather than a special case that only foreign libraries walk.
    ///
    /// Built ONCE per process because it is literally the same compile-time data every time. The
    /// memoization is of a named DEFAULT INSTANCE, not of "the only table there is", which is the
    /// distinction the whole inversion turns on: a consumer that loads other libraries builds its own
    /// and hands it in, and the two coexist.
    #[must_use]
    pub fn builtin() -> &'static Registry {
        static BUILTIN: LazyLock<Registry> = LazyLock::new(|| {
            Registry::new()
                .with(crate::eval::intrinsics::builtin_rows())
                .with(crate::eval::intrinsics::generated_bosl2_module_rows())
        });
        &BUILTIN
    }

    /// Resolve a defined function to its registry row, if one is registered for EXACTLY this body. Called
    /// ONCE per function at `build_ctx` time (never per call): fingerprint the running `(params, body)`,
    /// then match on (name, fingerprint). A miss — no row for the name, or the name matches but the body
    /// doesn't — returns `None`, so the interpreter runs the real body. This is the never-silently-wrong
    /// gate's FIRST hop; the caller must still clear the row's `deps`/`builtins` guards (and, for a
    /// non-empty `consts`, arm post-hoist) before wiring `func`.
    #[must_use]
    pub fn resolve(&self, name: &str, params: &[Parameter], body: &Expr) -> Option<&'static Entry> {
        self.resolve_fp(name, crate::fingerprint_of(params, body))
    }

    /// [`Registry::resolve`] with the fingerprint ALREADY COMPUTED — the AR.19 seam.
    ///
    /// Arming a library asks about the same body many times (its own resolve, then once per row
    /// that names it as a dep, then again post-hoist), and hashing an AST is not free: at 1260 rows
    /// the repeated walks measured at +55 ms per evaluation. The caller memoizes and hands the hash
    /// in. Nothing about the GATE changes — the fingerprint is still ours, computed by our parser
    /// from the program's own body — only how many times we compute it.
    ///
    /// FIRST MATCH among the libraries that declare `name`. A tie means two libraries transcribed
    /// the same upstream source — each row was proved against the SAME body by the same gate, so
    /// either answers correctly and the tie-break is arbitrary rather than a loss. Accumulation
    /// order settles it deterministically; [`Registry::eclipsed`] names the ones it passed over.
    #[must_use]
    pub fn resolve_fp(&self, name: &str, fp: Fingerprint) -> Option<&'static Entry> {
        self.fn_index()
            .map
            .get(name)?
            .iter()
            .find(|c| c.fp == fp)
            .map(|c| c.entry)
    }

    /// Which row set handed over this row. `None` for a row no registry indexed (a caller holding an
    /// `Entry` from somewhere else entirely), which makes every library-scoped question below answer
    /// `None` — decline, never a guess.
    fn lib_of(&self, entry: &Entry) -> Option<usize> {
        self.fn_index()
            .map
            .get(entry.name)?
            .iter()
            .find(|c| std::ptr::eq(c.entry, entry))
            .map(|c| c.lib)
    }

    /// The row `asking`'s OWN LIBRARY declares for `dep` — the row whose semantics `asking`'s native
    /// inlined, which is the only one any guard question about `dep` is about.
    ///
    /// `None` if that library declares no row for the name (it may still PIN it — see
    /// [`Registry::anchor_fp`] — and a pin has no consts to check, so `None` is the right answer for
    /// the const guards that call this).
    #[must_use]
    pub fn dep_row(&self, asking: &Entry, dep: &str) -> Option<&'static Entry> {
        let lib = self.lib_of(asking)?;
        self.fn_index()
            .map
            .get(dep)?
            .iter()
            .find(|c| c.lib == lib)
            .map(|c| c.entry)
    }

    /// A registry row by NAME alone — no fingerprint, no program.
    ///
    /// Deliberately weaker than [`Registry::resolve`]: the question it answers is about the REGISTRY's
    /// shape ("does this dep bake a constant?"), not about a particular program's definitions, and it is
    /// asked in `build_intrinsics` before any island scope exists to check a constant against (AN.17's
    /// `needs_post_hoist`). A name is unique in the registry even though fingerprints are not.
    ///
    /// The FIRST library's row when several declare the name — which makes it a question about the
    /// index's shape and not about any one library's semantics. Guard code wants
    /// [`Registry::dep_row`] instead, which asks the library that actually inlined the dep.
    #[must_use]
    pub fn entry_by_name(&self, name: &str) -> Option<&'static Entry> {
        self.fn_index().map.get(name)?.first().map(|c| c.entry)
    }

    /// The reference fingerprint a DEP name must match to satisfy `asking`'s dep pin: the dep's own
    /// row if it has one, else its pin. `None` = the dep isn't anchored — an authoring bug in the
    /// depending library, which then never wires over it.
    ///
    /// SCOPED TO `asking`'S OWN LIBRARY, and that is load-bearing rather than tidy. A dep name is not
    /// a global address: it names a function the ASKING library declared or pinned, whose semantics
    /// its native inlined. Answering from another library's row that happens to share the name gates
    /// the native on a function nobody proved it equivalent to — the same wrong ANSWER the
    /// fingerprint gate exists to prevent, one hop out. Unanchored-in-my-library therefore means
    /// `None` (decline → interpret), never "try the neighbours".
    ///
    /// TAKES THE ROW, NOT ITS NAME (AR.26.4.1). With several libraries free to declare one name, a
    /// name no longer identifies a library — and the caller has always held the row.
    #[must_use]
    pub fn anchor_fp(&self, asking: &Entry, dep: &str) -> Option<Fingerprint> {
        let idx = self.fn_index();
        let lib = self.lib_of(asking)?;
        idx.map
            .get(dep)
            .and_then(|cs| cs.iter().find(|c| c.lib == lib).map(|c| c.fp))
            .or_else(|| idx.pins.get(&(lib, dep)).copied())
    }

    /// The registered REFERENCE fingerprint for `name` — the hash a running function must match to WIRE — or
    /// `None` if nothing is registered under that name. Feeds the EXPLAIN DRIFT diagnostic, which prints it
    /// next to the running function's own fingerprint so an author can SEE how the two differ (stale reference
    /// vs a genuinely different library version).
    ///
    /// The FIRST library's, when several declare the name — a diagnostic, so "one of the references
    /// this name could have matched" is the useful answer and a list would only bury it.
    #[must_use]
    pub fn reference_fp(&self, name: &str) -> Option<Fingerprint> {
        self.fn_index().map.get(name)?.first().map(|c| c.fp)
    }

    /// Classify a defined function against the registry (O.3 EXPLAIN). Pure + testable; the `FAB_EXPLAIN`
    /// stderr report is just this plus a print.
    #[must_use]
    pub fn classify(&self, name: &str, params: &[Parameter], body: &Expr) -> Plan {
        if !self.fn_index().map.contains_key(name) {
            return Plan::NotRegistered;
        }
        // Fingerprint-level truth: a guarded match is WIRED here (the source matched); whether its deps/consts
        // guards then clear is a separate, per-program verdict the build/arm steps print under the same EXPLAIN.
        if self.resolve(name, params, body).is_some() {
            Plan::Wired
        } else {
            Plan::Drift
        }
    }

    /// SU.2 (sustainment): every audited `(name, reference-fingerprint, is_pin)` — rows first, then pins.
    /// The parity matrix walks these against whatever library a program actually loaded; the fingerprints
    /// are the SAME ones dispatch uses, so the audit can never disagree with the wire gate about what
    /// "matched" means. The `_fab_` namespace (the O.1 proof-of-concept trio) is fab-authored — no upstream
    /// defines it, so upstream parity doesn't apply and it's excluded.
    pub fn matrix_targets(&self) -> impl Iterator<Item = (&'static str, Fingerprint, bool)> + '_ {
        let idx = self.fn_index();
        // DEDUPED on `(name, fingerprint)`: two libraries transcribing one upstream function is one
        // parity question, and auditing it twice would inflate every count the matrix reports.
        let mut seen = std::collections::BTreeSet::new();
        idx.map
            .values()
            .flatten()
            .filter(|c| !c.entry.name.starts_with("_fab_"))
            .map(|c| (c.entry.name, c.fp, false))
            .chain(idx.pins.iter().map(|(&(_, n), &fp)| (n, fp, true)))
            .filter(move |&(n, fp, is_pin)| seen.insert((n, fp, is_pin)))
    }

    /// Rows that can never be SELECTED: a candidate whose fingerprint an earlier library already
    /// answers for, as `(library, name)`.
    ///
    /// NOT a fault, and the distinction is the point. An eclipsed row is one whose reference is
    /// STRUCTURALLY IDENTICAL to a live one — same body, same gate, same verdict for any program —
    /// so nothing was lost by passing over it. It stays in the index, because its own library's rows
    /// anchor their deps against it; only dispatch passes it by. A test asserts the identity holds,
    /// which is what makes "eclipsed is safe" a checked claim rather than this paragraph.
    #[must_use]
    pub fn eclipsed(&self) -> Vec<(&'static str, &'static str)> {
        let idx = self.fn_index();
        let mut out = Vec::new();
        for cands in idx.map.values() {
            for (i, c) in cands.iter().enumerate() {
                if cands[..i].iter().any(|e| e.fp == c.fp) {
                    out.push((self.rows[c.lib].name, c.entry.name));
                }
            }
        }
        out
    }

    /// The compiled module for `name`, IFF one is registered, the definition in this program
    /// fingerprints to the reference it was generated from, AND every constant the native bakes
    /// bit-matches its binding in `base` — the body's lexical base, the same scope the interpreted
    /// body's free reads resolve against (`bind_values` receives exactly this scope).
    ///
    /// Same wire-only-if-proven contract as [`Registry::resolve`]: a library that drifted from the pinned
    /// source interprets instead, so the worst case stays a missed compilation rather than a wrong answer.
    /// A user override of a baked constant (`_EPSILON = 1e-6;`) is the same story one level down —
    /// the fingerprint cannot see it, the guard here can.
    #[must_use]
    pub fn resolve_module(
        &self,
        name: &str,
        params: &[Parameter],
        body: &crate::Stmt,
        base: &Scope,
    ) -> Option<ModuleNative> {
        let &(fp, entry) = self.module_index().map.get(name)?;
        if module_fingerprint(params, body) != fp {
            return None;
        }
        entry
            .consts
            .iter()
            .all(|&(cname, expected)| {
                base.lookup_opt(cname)
                    .is_some_and(|v| crate::eval::intrinsics::value_bits_eq(&v, &expected()))
            })
            .then_some(entry.func)
    }

    /// How many function ROWS are indexed — candidates, not distinct names, so a registry that
    /// accepted every row it was handed reports exactly the length of what it was handed. The
    /// counterpart to [`Registry::faults`] for the every-shipped-row-is-accepted gate.
    #[must_use]
    pub fn function_count(&self) -> usize {
        self.fn_index().map.values().map(Vec::len).sum()
    }

    /// How many pins are indexed.
    #[must_use]
    pub fn pin_count(&self) -> usize {
        self.fn_index().pins.len()
    }

    /// How many module rows are indexed.
    #[must_use]
    pub fn module_count(&self) -> usize {
        self.module_index().map.len()
    }
}

/// The registry reference AS A FIELD TYPE — what `Ctx` actually holds.
///
/// It exists so `Ctx` can keep `#[derive(Default)]` WITHOUT an `impl Default for &Registry` on the
/// public API. `&T` is `#[fundamental]`, so such an impl would be visible to every downstream crate
/// and impossible to opt out of: any consumer's own `#[derive(Default)]` struct holding a
/// `&Registry` would silently come up holding fab-lang's shipped library rather than nothing.
/// MEASURED, not argued — a probe consumer crate reproduced exactly that, at arbitrary lifetimes.
/// Crate-private, so the one place that default applies is the one place it was written for.
///
/// The default is the BUILTIN set rather than an EMPTY one, and that direction is the load-bearing
/// half: an empty default would silently disable every native, and each tier differential in the
/// tree would go on passing while comparing the interpreter against itself.
#[derive(Clone, Copy)]
pub(crate) struct RegistryRef<'a>(pub(crate) &'a Registry);

impl Default for RegistryRef<'_> {
    fn default() -> Self {
        Self(Registry::builtin())
    }
}

impl std::ops::Deref for RegistryRef<'_> {
    type Target = Registry;

    fn deref(&self) -> &Registry {
        self.0
    }
}

/// Parse a `reference` (one `function` def) and fingerprint it, or `None` if it isn't exactly that.
fn function_fingerprint(reference: &str) -> Option<Fingerprint> {
    use crate::{StmtKind, parse};
    let program = parse(reference).ok()?;
    let stmt = program.stmts.into_iter().next()?;
    if let StmtKind::FunctionDef { params, body, .. } = stmt.kind {
        Some(crate::fingerprint_of(&params, &body))
    } else {
        None
    }
}

/// A module reference parsed to its `(params, body)` — the shape the fingerprint takes.
fn parse_module_reference(reference: &str) -> Option<(Vec<Parameter>, crate::Stmt)> {
    let program = crate::parse(reference).ok()?;
    let stmt = program.stmts.into_iter().next()?;
    let crate::StmtKind::ModuleDef { params, body, .. } = stmt.kind else {
        return None;
    };
    Some((params, *body))
}

/// A module's structural identity. Distinct from the expression fingerprint because a module's body
/// is a STATEMENT, not an expression — but the same idea and the same guarantee: spans excluded, so
/// reformatting and comment edits survive while a semantic change does not.
fn module_fingerprint(params: &[Parameter], body: &crate::Stmt) -> Fingerprint {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    params.len().hash(&mut h);
    for p in params {
        p.name.hash(&mut h);
        p.default.is_some().hash(&mut h);
    }
    // The body via the parser's own printer: a canonical, span-free rendering of the statement tree.
    // Cheaper to trust than a hand-written statement walk, which is the thing AR.3.3 caught going
    // quietly stale when it stopped seeing a node type.
    let as_program = crate::Program {
        stmts: vec![body.clone()],
    };
    crate::print(&as_program).hash(&mut h);
    Fingerprint::new(h.finish())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unnecessary_wraps,
    reason = "test harness: expect IS the assertion; the probe native wraps in Ok to fit the \
              fallible native ABI"
)]
mod tests {
    use super::{Entry, Fault, Registry, Rows, function_fingerprint};
    use crate::Value;

    fn probe(_fx: &dyn crate::surface::FnCtx, args: &[Value]) -> crate::Result<Value> {
        // DELIBERATELY not what the reference computes — see `PROBE`.
        match args.first() {
            Some(Value::Num(x)) => Ok(Value::Num(x + 1000.0)),
            _ => Ok(Value::Undef),
        }
    }

    /// A row whose native DISAGREES with its own reference. That disagreement is the instrument:
    /// it is the only way a test can tell which tier ran, and it is quarantined to this module and
    /// to `lang/tests/registry_instance.rs`, which say so in the same words.
    static PROBE: &[Entry] = &[Entry {
        name: "_reg_probe",
        reference: "function _reg_probe(x) = x + 1;",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: probe,
    }];

    static PROBE_DUP: &[Entry] = &[
        Entry {
            name: "_reg_dup",
            reference: "function _reg_dup(x) = x + 1;",
            consts: &[],
            consts_v: &[],
            deps: &[],
            builtins: &[],
            func: probe,
        },
        Entry {
            name: "_reg_dup",
            reference: "function _reg_dup(x) = x + 2;",
            consts: &[],
            consts_v: &[],
            deps: &[],
            builtins: &[],
            func: probe,
        },
    ];

    static PROBE_BAD: &[Entry] = &[Entry {
        name: "_reg_bad",
        reference: "module _reg_bad() { cube(1); }", // not a function def
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: probe,
    }];

    fn probe_rows(name: &'static str, functions: &'static [Entry]) -> Rows {
        Rows {
            name,
            functions,
            ..Rows::default()
        }
    }

    /// EVERY shipped row makes it into the index. The old `table()` skipped an unparseable
    /// reference with a `continue` behind a `debug_assert`, so a row that stopped registering cost
    /// a silently-missing native in release and nothing said so. This is that hole closed: the
    /// counts come from the row slices themselves, so adding a row that cannot be indexed fails
    /// here rather than quietly not dispatching.
    #[test]
    fn every_shipped_row_is_accepted() {
        let reg = Registry::builtin();
        let own = crate::eval::intrinsics::builtin_rows();
        let band = crate::eval::intrinsics::generated_bosl2_module_rows();
        assert_eq!(
            reg.faults(),
            Vec::new(),
            "the shipped registry must index cleanly"
        );
        assert_eq!(reg.function_count(), own.functions.len());
        assert_eq!(reg.pin_count(), own.pins.len());
        assert_eq!(
            reg.module_count(),
            own.modules.len() + band.modules.len(),
            "both module row sets must land in one index"
        );
    }

    /// A duplicate name is RECORDED and the FIRST row wins. Last-wins is the `_sort_vectors` trap
    /// that already cost a bug upstream, and a silent drop is a native that stops firing with no
    /// signal — so the rule is stated and the fault is the signal.
    #[test]
    fn a_duplicate_name_is_recorded_and_the_first_wins() {
        let reg = Registry::new().with(probe_rows("probe", PROBE_DUP));
        assert_eq!(
            reg.faults(),
            vec![Fault::DuplicateFunction {
                library: "probe",
                name: "_reg_dup",
            }]
        );
        assert_eq!(reg.function_count(), 1);
        let kept = reg.entry_by_name("_reg_dup").expect("the first row is kept");
        assert_eq!(kept.reference, "function _reg_dup(x) = x + 1;");
    }

    /// A reference that is not exactly one function def cannot be fingerprinted, so the row can
    /// never wire — recorded, not swallowed.
    #[test]
    fn an_unparseable_reference_is_recorded() {
        let reg = Registry::new().with(probe_rows("probe", PROBE_BAD));
        assert_eq!(
            reg.faults(),
            vec![Fault::UnparseableReference {
                library: "probe",
                name: "_reg_bad",
            }]
        );
        assert_eq!(reg.function_count(), 0);
    }

    /// THE CROSS-LIBRARY GATE. Library A pins `_reg_probe` as a dep anchor; library B then ships a
    /// ROW under that name, with a DIFFERENT body. Each library's rows must anchor against their
    /// OWN `_reg_probe` and never the neighbour's — otherwise B's native, which inlined B's version,
    /// would gate on A's and wire against a function nobody proved it equivalent to. A wrong ANSWER,
    /// not a missed speedup.
    ///
    /// AR.26.4.1 STRENGTHENED THIS RATHER THAN RELAXING IT. B's row used to be DROPPED on the theory
    /// that a contested name had to have one owner; that cost B its whole dependent cone, since a row
    /// whose own library's dep is not in the index can never anchor. Both rows are indexed now and
    /// the anchors stay library-local, which is the property that was ever load-bearing — the
    /// ownership claim was one implementation of it, and a lossy one.
    #[test]
    fn a_foreign_library_cannot_shadow_a_pinned_name() {
        static PINS: &[(&str, &str)] = &[("_reg_probe", "function _reg_probe(x) = x + 1;")];
        static A_ROWS: &[Entry] = &[Entry {
            name: "_reg_a",
            reference: "function _reg_a(x) = _reg_probe(x);",
            consts: &[],
            consts_v: &[],
            deps: &["_reg_probe"],
            builtins: &[],
            func: probe,
        }];
        static B_ROWS: &[Entry] = &[
            // Same NAME as A's pin, DIFFERENT source — the collision.
            Entry {
                name: "_reg_probe",
                reference: "function _reg_probe(x) = x + 99;",
                consts: &[],
                consts_v: &[],
                deps: &[],
                builtins: &[],
                func: probe,
            },
            Entry {
                name: "_reg_b",
                reference: "function _reg_b(x) = _reg_probe(x);",
                consts: &[],
                consts_v: &[],
                deps: &["_reg_probe"],
                builtins: &[],
                func: probe,
            },
        ];
        let a_rows = Rows {
            name: "A",
            functions: A_ROWS,
            pins: PINS,
            ..Rows::default()
        };
        let both = Registry::new().with(a_rows).with(probe_rows("B", B_ROWS));
        assert_eq!(
            both.faults(),
            Vec::new(),
            "two libraries declaring one name is a topology, not a fault"
        );
        assert!(
            both.entry_by_name("_reg_probe").is_some(),
            "B's row is dispatchable — the program's own body decides whether it fires"
        );

        let alone = Registry::new().with(a_rows);
        let a = &A_ROWS[0];
        assert_eq!(
            both.anchor_fp(a, "_reg_probe"),
            alone.anchor_fp(a, "_reg_probe"),
            "A's anchor must answer with A's own fingerprint, B present or not"
        );
        // THE CORE ASSERTION: the two anchors DIFFER. Equal would mean one library's guard is
        // checking the other's function, which is the wrong answer this scoping exists to prevent.
        let b = &B_ROWS[1];
        assert_eq!(
            both.anchor_fp(b, "_reg_probe"),
            Some(function_fingerprint("function _reg_probe(x) = x + 99;").expect("parses")),
            "B anchors on B's own row"
        );
        assert_ne!(both.anchor_fp(a, "_reg_probe"), both.anchor_fp(b, "_reg_probe"));
        assert_eq!(
            both.dep_row(a, "_reg_probe").map(|e| e.reference),
            None,
            "A only PINS the name, so it has no dep ROW for it — a pin has no consts to guard"
        );
        assert_eq!(
            both.dep_row(b, "_reg_probe").map(|e| e.reference),
            Some("function _reg_probe(x) = x + 99;"),
            "B's dep row is B's own"
        );
    }

    /// TWO ANONYMOUS LIBRARIES ARE STILL TWO LIBRARIES. Scoping keys on the row-set INDEX rather
    /// than `Rows::name`, because `Rows::default()` leaves the name empty and a string key collapsed
    /// every unnamed library into one — a gate that silently stops gating, which is worse than not
    /// having it. Asserted through ANCHORING now that a shared name is no longer a fault: two
    /// unnamed libraries with different sources must still answer their own fingerprints.
    #[test]
    fn scoping_keys_on_the_hand_over_not_the_name() {
        static X_ROWS: &[Entry] = &[
            Entry {
                name: "_reg_leaf",
                reference: "function _reg_leaf(x) = x + 7;",
                consts: &[],
                consts_v: &[],
                deps: &[],
                builtins: &[],
                func: probe,
            },
            Entry {
                name: "_reg_root",
                reference: "function _reg_root(x) = _reg_leaf(x);",
                consts: &[],
                consts_v: &[],
                deps: &["_reg_leaf"],
                builtins: &[],
                func: probe,
            },
        ];
        static Y_ROWS: &[Entry] = &[
            Entry {
                name: "_reg_leaf",
                reference: "function _reg_leaf(x) = x + 8;",
                consts: &[],
                consts_v: &[],
                deps: &[],
                builtins: &[],
                func: probe,
            },
            Entry {
                name: "_reg_root",
                reference: "function _reg_root(x) = _reg_leaf(x) * 2;",
                consts: &[],
                consts_v: &[],
                deps: &["_reg_leaf"],
                builtins: &[],
                func: probe,
            },
        ];
        let both = Registry::new()
            .with(probe_rows("", X_ROWS))
            .with(probe_rows("", Y_ROWS));
        assert_eq!(both.faults(), Vec::new());
        assert_eq!(both.function_count(), 4, "every row is indexed");
        assert_ne!(
            both.anchor_fp(&X_ROWS[1], "_reg_leaf"),
            both.anchor_fp(&Y_ROWS[1], "_reg_leaf"),
            "the second hand-over is a different library even with the same (empty) name"
        );
    }

    /// AN ECLIPSED ROW IS SAFE BECAUSE ITS TWIN IS IDENTICAL, and that is the claim under test —
    /// not that eclipsing happens, but that what it passes over could never have answered
    /// differently. Two libraries transcribing one upstream function is the normal case (fab-lang
    /// and fab-bosl2 overlap on 66 names); the second is unreachable for dispatch and still fully
    /// present for its own library's anchors, which is what keeps its dependent cone alive.
    #[test]
    fn an_eclipsed_row_has_an_identical_live_twin() {
        let both = Registry::new()
            .with(probe_rows("first", PROBE))
            .with(probe_rows("second", PROBE));
        assert_eq!(both.faults(), Vec::new(), "a shared name is not a fault");
        assert_eq!(both.function_count(), 2, "both rows are indexed");
        assert_eq!(both.eclipsed(), vec![("second", "_reg_probe")]);
        // The invariant that makes it safe: same fingerprint as the row that shadowed it.
        let fp = both.reference_fp("_reg_probe").expect("indexed");
        assert_eq!(
            fp,
            function_fingerprint(PROBE[0].reference).expect("parses"),
            "the eclipsed row's reference hashes to exactly what dispatch resolves"
        );
        // Anchoring still answers. (Both hand-overs share one static slice here, so the row is
        // literally the same address in both libraries — the aliasing is benign precisely because
        // the two claims are the same claim. `scoping_keys_on_the_hand_over_not_the_name` is where
        // two DISTINCT rows prove the scoping.)
        assert_eq!(both.anchor_fp(&PROBE[0], "_reg_probe"), Some(fp));
    }

    /// Adding rows after a question has been asked REBUILDS rather than serving a stale index. The
    /// `OnceLock`s make the stale variant easy to reach and impossible to see: `faults()` would
    /// certify a registry that had never looked at the last library handed to it.
    #[test]
    fn rows_added_after_a_query_are_still_indexed() {
        let mut reg = Registry::new().with(probe_rows("A", PROBE_BAD));
        assert_eq!(reg.function_count(), 0);
        reg = reg.with(probe_rows("B", PROBE));
        assert_eq!(
            reg.function_count(),
            1,
            "the second library must be visible after the first forced an index"
        );
        assert_eq!(reg.faults().len(), 1, "and the first library's fault survives");
    }

    /// The SAME library may declare a name as both a row and a pin — fab-lang ships nine that way
    /// (a row for dispatch, a pin so other rows can anchor against it), and `anchor_fp` searches
    /// both. The claim check is BETWEEN libraries only; making it stricter would fault the shipped
    /// registry on its own rows.
    #[test]
    fn one_library_may_declare_a_name_as_both_a_row_and_a_pin() {
        static PINS: &[(&str, &str)] = &[("_reg_probe", "function _reg_probe(x) = x + 1;")];
        let reg = Registry::new().with(Rows {
            name: "A",
            functions: PROBE,
            pins: PINS,
            ..Rows::default()
        });
        assert_eq!(reg.faults(), Vec::new());
        assert_eq!(reg.function_count(), 1);
        assert_eq!(reg.pin_count(), 1);
    }

    /// BOTH halves are built on first use, and SEPARATELY. Measured: 17 ms to index BOSL2's 419
    /// module references and 1.7 ms for the 117 function ones — pure waste for a program that
    /// instantiates no user module, or defines no function at all, which is what `eval_expr` and
    /// every value-level battery are. The three static `OnceLock`s this replaced got that right by
    /// accident (nothing looked a name up, so nothing built a table) and it would have been a silent
    /// regression to lose it. Asserting on the `OnceLock`s directly because the cost is invisible to
    /// any behavioural check.
    #[test]
    fn both_index_halves_are_lazy_and_independent() {
        let reg = Registry::new()
            .with(crate::eval::intrinsics::builtin_rows())
            .with(crate::eval::intrinsics::generated_bosl2_module_rows());
        assert!(
            reg.fns.get().is_none() && reg.modules.get().is_none(),
            "accumulating rows must not index them"
        );
        assert!(reg.function_count() > 0);
        assert!(
            reg.modules.get().is_none(),
            "a FUNCTION question must not pay for the module half"
        );
        assert!(reg.module_count() > 0);
        assert!(reg.modules.get().is_some());
    }
}
