//! The scad-rs evaluator (v0 skeleton).
//!
//! Expression evaluation runs on an EXPLICIT STACK — no host recursion, so evaluation depth is
//! bounded by the heap (the task/value `Vec`s), not the call stack. This is where the SPEC's "the
//! Safari class of failure becomes structurally impossible" actually lands, and it's the sibling of
//! the parser's non-recursive `Drop`. (I.7's Kani proofs target this machine's push/pop discipline.)
//!
//! v0 scope: the expression subset producing [`Value`] v0 (`Num`/`Bool`/`Str`/`NumList`/`Undef`),
//! plus `$fn`/`$fa`/`$fs` → fragment resolution. Functions, indexing, member access, ranges, and
//! heterogeneous/nested vectors fail LOUD ([`Error::Unimplemented`](crate::Error::Unimplemented)) —
//! I.1/I.4. Arithmetic/undef semantics are bug-for-bug OpenSCAD (`ops`).

mod builtins;
mod config;
mod eval_cache;
mod fmt;
mod fnprofile;
mod fragments;
mod geo;
mod geo2d;
mod geo_drop;
mod geo_stack;
mod geometry;
mod intrinsics;
pub(crate) mod io;
pub(crate) mod jit_abi;
mod loader;
mod message;
mod metrics;
mod mod_cache;
mod mod_redundancy;
mod module;
mod object;
mod ops;
mod redundancy;
pub(crate) mod rng;
mod scope;
mod text;
mod trace;
mod trig;
mod value;

pub use config::Config;
pub use fragments::fragments;
pub use geo::GeoNode;
pub use geo2d::{Contour, ExtrudeKind, Geo, Join2D, Shape2D};
pub use message::{Evaluation, Message};
pub use scope::Scope;
pub use value::{RANGE_MAX, RANGE_TOO_MANY, RangeIter, Value, range_iter, range_len};

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::Mesh;
use crate::geom::{Affine, Affine2};
use crate::parser::{Arg, BinOp, Expr, ExprKind, Parameter, Program, Stmt, StmtKind, UnOp};

/// The caller-supplied table that fulfills `import`/`surface` [`SourceNeed::File`]s (M.3): the literal
/// `file=` path a call named → the [`Imported`] payload the caller read for it. fab-lang does ZERO IO, so it
/// never reads these files itself — the impure caller (the M.4 shell, via M.5's STL/3MF/SVG readers) reads
/// them and hands the payloads back through this table, keyed by the EXACT `raw` string the need carried.
pub type FileTable = BTreeMap<String, Imported>;

/// A read `import()`/`surface()` file — dimension-TAGGED, because a `.stl`/`.3mf` is 3D but a `.svg`/`.dxf`
/// is 2D, and the evaluator must wrap each as the RIGHT geometry leaf (a [`GeoNode::Leaf`] mesh vs a
/// [`Shape2D::Polygon`] of contours). The impure reader (M.5, fab-scad side) decides dimension by EXTENSION
/// and hands back the tagged payload; [`eval_module`](super::module) unwraps it at the `import`/`surface`
/// dispatch. Widening the table off a bare `Mesh` is what lets 2D vector import (Q.4) exist at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Imported {
    /// A 3D mesh — `.stl`/`.3mf`/`.off` and `surface()`'s `.dat`/`.png` heightmaps.
    Mesh(Mesh),
    /// 2D contours — `.svg`/`.dxf` vector art, an even-odd-filled [`Shape2D::Polygon`]. Outer boundary and
    /// holes are all just contours in the one vec (the backend's fill rule resolves them), exactly like the
    /// glyph outlines `text()` produces.
    Contours(Vec<Contour>),
}

impl Imported {
    /// An EMPTY placeholder of the dimension `raw`'s extension implies — the stand-in [`Ctx::request_file`]
    /// returns on the FIRST fixpoint pass, before the caller has read the file. The dimension MATTERS even
    /// for the empty: a `.svg` in a 2D context (`linear_extrude() import("logo.svg")`) must present as 2D,
    /// or the run would dimension-error on the mixed tree and abort BEFORE the `File` need ever surfaces —
    /// the fixpoint would never close. `.svg`/`.dxf` → empty 2D; everything else → empty 3D (mirroring the
    /// reader's own extension demux, [`crate::eval::io`]'s fab-scad-side sibling).
    fn empty_for(raw: &str) -> Self {
        let ext = std::path::Path::new(raw)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match ext.as_str() {
            "svg" | "dxf" => Imported::Contours(Vec::new()),
            _ => Imported::Mesh(Mesh::new()),
        }
    }
}

/// A source the pure evaluator needs but can't produce — the caller reads it, adds it, and re-runs (the
/// needs fixpoint). Two kinds, one per discovery phase: a `Scad` reference (a `use`/`include` target, found
/// STATICALLY by the loader) and a `File` reference (an `import`/`surface` mesh path, found only by
/// EXECUTING — the path is a runtime expression, not a static token). M.3 emits `File`; the loader's own
/// Scad channel folds into this same enum in M.4, when its fixpoint loop lifts out of `loader::load`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceNeed {
    /// A `use`/`include` target: the literal `<...>` path `raw`, resolved against the requesting file's
    /// directory `from_dir` (the base for the library-path search).
    Scad {
        /// The requesting file's directory — the base `raw` resolves against.
        from_dir: std::path::PathBuf,
        /// The literal `<...>` reference text.
        raw: String,
    },
    /// An `import`/`surface` mesh path: the literal `file=` string, resolved + read by the caller.
    File {
        /// The literal `file=` path the call named.
        raw: String,
    },
}

/// The outcome of a pure evaluation (M.1): either it CLOSED — every referenced source was present, so here's
/// the geometry tree + its ordered `echo`/warning messages — or it's still missing sources, which the caller
/// fulfills and re-runs. [`Resolution::Incomplete`] deliberately carries NO geo/messages: the caller re-runs
/// from scratch with a fuller [`FileTable`], which re-emits them on the closing pass, so surfacing them here
/// would only double-count. A mesh rarely gates control flow, so one re-run usually closes the fixpoint.
#[derive(Debug)]
pub enum Resolution {
    /// Nothing left to resolve — the geometry tree plus the run's ordered console messages.
    Complete {
        /// The evaluated geometry tree.
        geo: Geo,
        /// The run's `echo`/warning messages, in emission order.
        messages: Vec<Message>,
    },
    /// Still-missing sources; the caller reads them, adds them to the table, and evaluates again.
    Incomplete {
        /// The sources this run asked for and couldn't get, deduped + deterministically ordered.
        needs: Vec<SourceNeed>,
    },
}

/// SU.2 (sustainment): how one audited intrinsic (or dep PIN) relates to the library a program loaded.
/// The matrix is the drift detector for a BOSL2 bump: `Changed`/`Missing` mean the intrinsic silently
/// stops dispatching there — correct (the interpreter runs upstream's real body) but slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicMatrixStatus {
    /// The library defines this name and it fingerprints to the registered reference — dispatch fires.
    Matched,
    /// Defined, but the body/params drifted from the reference — the intrinsic no longer dispatches.
    Changed,
    /// The library defines no function of this name at all (renamed or removed upstream).
    Missing,
}

/// SU.2: one row of the intrinsic parity matrix. `pin` rows are reference-only dep anchors (no native
/// impl of their own, but a `Changed`/`Missing` pin blocks every entry that dep-pins it).
#[derive(Debug, Clone)]
pub struct IntrinsicMatrixRow {
    /// The audited function name.
    pub name: String,
    /// Reference-only dep anchor (true) vs a real registry entry (false).
    pub pin: bool,
    /// The verdict for this name against the loaded library.
    pub status: IntrinsicMatrixStatus,
    /// What the library's definition fingerprints to (`None` when [`IntrinsicMatrixStatus::Missing`]).
    pub defined_fp: Option<u64>,
    /// The fingerprint the intrinsic was verified against — the one dispatch demands.
    pub reference_fp: u64,
}

/// [`resolve_intrinsic_matrix`]'s outcome — the matrix, or the sources still needed to load the library.
#[derive(Debug)]
enum MatrixResolution {
    /// The `use`/`include` graph closed; here's the audit.
    Complete(Vec<IntrinsicMatrixRow>),
    /// Still-missing sources, exactly as [`Resolution::Incomplete`].
    Incomplete {
        /// The sources this pass asked for and couldn't get.
        needs: Vec<SourceNeed>,
    },
}

/// SU.2: audit the intrinsic registry against the library `source` loads — the STATIC half of
/// [`resolve_source`] (close the `use`/`include` graph, hoist the function map) with the matrix walk in
/// place of evaluation. Nothing executes, so `File` needs can't surface and no geometry exists; the map
/// audited here is byte-for-byte the one [`build_intrinsics`] would consume, include order, last-wins
/// redefinitions and all.
fn resolve_intrinsic_matrix(
    source: &str,
    base_dir: &std::path::Path,
    scad_sources: &loader::SourceMap,
) -> crate::Result<MatrixResolution> {
    let loaded = match loader::resolve_graph(source, base_dir, None, scad_sources)? {
        loader::GraphOutcome::Complete(loaded) => loaded,
        loader::GraphOutcome::Incomplete(scad_needs) => {
            return Ok(MatrixResolution::Incomplete {
                needs: scad_needs
                    .into_iter()
                    .map(|n| SourceNeed::Scad {
                        from_dir: n.from_dir,
                        raw: n.raw,
                    })
                    .collect(),
            });
        }
    };
    let islands = loader::islands(&loaded);
    let functions = tagged_functions(&islands);
    let rows = intrinsics::matrix_targets()
        .map(|(name, reference_fp, pin)| {
            let defined_fp = functions
                .get(name)
                .map(|&((params, body), _home)| intrinsics::fingerprint(params, body));
            let status = match defined_fp {
                Some(fp) if fp == reference_fp => IntrinsicMatrixStatus::Matched,
                Some(_) => IntrinsicMatrixStatus::Changed,
                None => IntrinsicMatrixStatus::Missing,
            };
            IntrinsicMatrixRow {
                name: name.to_string(),
                pin,
                status,
                defined_fp,
                reference_fp,
            }
        })
        .collect();
    Ok(MatrixResolution::Complete(rows))
}

/// A desktop-only numeric JIT hook (P.1.2). The interpreter offers a user-function call to this BEFORE
/// interpreting the body, but ONLY when the call is all-positional and arity-exact (a named arg sets
/// `jit=None` at dispatch). The hook itself decides whether the ARGUMENTS are scalarizable — a number or a
/// fixed-small numeric vector (P.1.6 rung B) — and DECLINES (`None`) anything it can't compile for this
/// arg shape; the interpreter then runs the real body.
///
/// Defined here so the eval loop can dispatch to it, but the crate stays wasm-clean: the trait carries no
/// Cranelift, and the native `fab-jit` crate implements it over its compiled `JitRegistry`. The contract
/// is `fast == JIT` — a `Some(r)` MUST be bit-identical to interpreting the body (the JIT crate's
/// differential proves it), so routing a call here can only change SPEED, never the result. wasm builds
/// (which can't JIT in-sandbox) simply leave [`Ctx::jit`] `None` and interpret everything.
pub trait NumericJit {
    /// The compiled result of `name(args)` if a specialization for this arg SHAPE is registered (or compiles
    /// on demand), else `None`. The hook receives the evaluated [`Value`] args and scalarizes them itself
    /// (P.1.6 rung B): a [`Value::Num`] is a scalar, a [`Value::NumList`] a fixed-length vector; a
    /// non-scalarizable arg (a nested list, string, undef, range, function, or an over-long vector) → `None`.
    /// A [`JitOutcome`] carries the result's TYPE tag (P.1.4e) — the JIT return ABI is untyped `f64`, so the
    /// compiled function reports whether that `f64` is a number/boolean/vector and the dispatch re-wraps it in
    /// the matching [`Value`]. `None` means "not compiled / declined / raised" — the interpreter takes over.
    ///
    /// `rand` is the eval's live [`rng::RandStream`] woven in by pointer (P.1.6 rung-D piece 1): a JIT'd
    /// SEEDLESS `rands()` advances it through [`jit_rand_next`], exactly as the interpreter would, so the draw
    /// sequence stays bit-identical. The dispatch passes `Ctx::rand_stream`'s cell pointer; a body that never
    /// calls `rands` leaves it untouched. It is a raw pointer (not `&mut`) because the native ABI carries it as
    /// one, and the caller guarantees exclusive single-threaded access for the call's duration.
    fn call_numeric(
        &self,
        name: &str,
        args: &[Value],
        rand: *mut core::ffi::c_void,
    ) -> Option<JitOutcome>;
}

/// A JIT-compiled call's result, TYPE-TAGGED (P.1.4e). The native return ABI is a single untyped `f64` (plus
/// the raise out-byte, plus — for a vector result — a sink buffer the JIT writes); this reconstructs the tag
/// that [`Value`] carries for free in the interpreter but that evaporates crossing `extern "C"`, so the
/// dispatch wraps a `Num` result in [`Value::Num`], a `Bool` (a comparison / `&&`/`||`/`!` / bool literal —
/// the JIT computes these as an `i8`, returned as `0.0`/`1.0`) in [`Value::Bool`], and a `Vec` (a fixed-shape
/// vector return, P.1.6 rung C) in [`Value::NumList`]. Returning the wrong tag would DIVERGE (`Num(1.0) !=
/// Bool(true)`), so the tag is load-bearing, not cosmetic.
#[derive(Debug, Clone, PartialEq)]
pub enum JitOutcome {
    /// A numeric result → [`Value::Num`].
    Num(f64),
    /// A boolean result → [`Value::Bool`].
    Bool(bool),
    /// A fixed-shape numeric vector (rung C) → [`Value::NumList`]. Owned — the JIT wrote it into a sink buffer.
    Vec(Vec<f64>),
    /// A fixed-shape NESTED value — a matrix / list-of-vectors (P.1.6 rung-D 2c.1). The JIT wrote its flat leaf
    /// scalars to the sink buffer and rebuilt the nesting from a compile-time shape tree, applying the SAME
    /// `build_vector` rule (all-`Num` children → `NumList`, else `List`) the interpreter does — so the value is
    /// bit-identical, and the dispatch just hands it through. Carries a fully-built [`Value`] rather than a flat
    /// buffer because the nesting can't survive the flat-`f64` return ABI a [`JitOutcome::Vec`] rides.
    Nested(Value),
}

/// One user function offered to the JIT factory: its name, its parameter names (in order), and its body
/// AST. The wasm-clean hand-off shape — the factory ([`NumericJitFactory`]) compiles the numeric-subset
/// ones and ignores the rest. Borrowed from the loaded program; the compiled result retains no AST refs.
pub struct JitDef<'a> {
    /// The function's name (its registry key).
    pub name: &'a str,
    /// The parameters, in declaration order — names AND defaults, so the JIT can bind an unfilled param to
    /// its default when INLINING a short call.
    pub params: &'a [Parameter],
    /// The function body to compile.
    pub body: &'a Expr,
}

/// One top-level CONSTANT offered to the JIT factory (P.1.4 globals): its name and its value expression. A
/// numeric function body that references a top-level constant — BOSL2's `_EPSILON`, `INF`, `PHI`, all over
/// its math — resolves it by INLINING the constant's value-expr (`_EPSILON` → `1e-9`, `INF` → `1/0` = +inf),
/// so the function COMPILES instead of declining on the free variable. The JIT compiles the value-expr in an
/// EMPTY scope: a self-contained numeric constant inlines; one that references ANOTHER global (or a vector /
/// string constant like `LEFT = [-1,0,0]`) makes the referrer DECLINE — harmless, so the factory is handed
/// EVERY top-level assignment, unfiltered. Borrowed from the loaded program; the compiled result keeps no refs.
pub struct JitConst<'a> {
    /// The constant's name — the free variable a function body reads.
    pub name: &'a str,
    /// The value expression the reference resolves to.
    pub value: &'a Expr,
}

/// Builds a [`NumericJit`] from a program's function set (P.1.2). Called ONCE per eval round, after the
/// loader closes the `use`/`include` graph so every function is known. The native `fab-jit` crate
/// implements this over Cranelift; `None` return means "nothing compiled, interpret everything". Kept a
/// trait (not a closure) so the crate boundary carries no Cranelift and the method's elided lifetime lets
/// it accept defs of any lifetime. wasm passes no factory at all → [`Ctx::jit`] stays `None`.
pub trait NumericJitFactory {
    /// Compile the numeric-subset functions in `defs` to a [`NumericJit`], or `None` if none compiled / the
    /// JIT is disabled. `enabled` is the RUN gate ([`Config::jit`]) — the caller's authoritative on/off, so the
    /// factory no longer sniffs its own env; a factory MAY still compile-for-coverage when `enabled` is false
    /// (its own report-only probe) but must return `None` there. `consts` are the program's top-level constants
    /// (P.1.4 globals) — a body reading one inlines its value-expr instead of declining on the free variable.
    fn compile(
        &self,
        defs: &[JitDef<'_>],
        consts: &[JitConst<'_>],
        enabled: bool,
    ) -> Option<Box<dyn NumericJit>>;
}

/// The evaluation context, borrowed from the `Program`:
/// - `functions`: the user-function store (name → params + body). Functions live in their OWN
///   namespace (separate from variables), so a call resolves by name — which is why recursion and
///   mutual recursion work regardless of scope. Built once per program (`build_ctx`).
/// - `closures`: function-literal VALUES registered as they evaluate (indexed by [`Value::Function`]'s
///   `closure_id`). `&'a` AST refs, so a [`Value`] holding a `closure_id` stays `'static`.
/// - `messages`: `echo`/warning console output, accumulated in EMISSION order (I.5) — a shared buffer
///   because echo can fire deep in an expression, not just at a statement. Extracted into
///   [`Evaluation`] at the end; the mesh-only `evaluate*` sugar drops it.
#[derive(Default)]
pub(super) struct Ctx<'a> {
    /// User FUNCTION definitions, name → (def, HOME ISLAND). Resolution is the root file's flat view
    /// (island 0's own defs override its `use`-imported ones — the common precedence); the home island
    /// tag is what the use-scope fix rides on: a called function's body evaluates with its home island's
    /// constants ([`Ctx::island_globals`]) as the lexical base, so a `use`d function reads its OWN file's
    /// top-level constants (which `use` never imports into the caller), not the caller's. (Fully LEXICAL
    /// per-call-site function resolution — like modules' — stays deferred; functions aren't shadowed
    /// across files the way `builtins.scad` shadows modules, so the flat view holds for the corpus.)
    functions: BTreeMap<&'a str, (loader::FnDef<'a>, usize)>,
    /// Registered INTRINSICS (O.1): function name → a native impl that replaces the interpreted body,
    /// matched by AST FINGERPRINT once at build time (never per call), so it's never applied to a function
    /// it wasn't verified against. Empty for the common program that defines nothing the registry covers.
    intrinsics: BTreeMap<&'a str, intrinsics::Intrinsic>,
    /// Per-island CONSTANT scope: `island_globals[i]` is island `i`'s top-level assignments hoisted into a
    /// scope (whole-scope, last-wins), seeded with the `$fn`/`PI` defaults. Index 0 is the ROOT file's
    /// global (built by [`run_stmts`]); each `use` target's is built from its [`loader::Island`]
    /// assignments. A function/module body evaluates against its HOME island's entry. A `RefCell` only
    /// because it's populated AFTER the `Ctx` exists (building an island global needs the `Ctx` to call
    /// functions); it's write-once-per-island at setup, read-only during geometry eval.
    island_globals: std::cell::RefCell<Vec<Scope>>,
    /// User MODULE definitions, as per-file scope ISLANDS (I.9.5) — module resolution is LEXICAL, not
    /// global. `islands[0]` is the root file; each `use` target gets its own island. A module CALL
    /// resolves against the CURRENT island (its own file's defs + the files it uses + builtins) via
    /// [`Ctx::resolve_module`], before the builtin-primitive fallthrough (I.2.4). This is what lets a
    /// `use`d module see the BUILTIN behind a name the including program has redefined (BOSL2's
    /// `builtins.scad` `_cube → cube` trick), instead of recursing into the redefinition.
    islands: loader::Islands<'a>,
    closures: RefCell<Vec<(&'a [Parameter], &'a Expr)>>,
    messages: RefCell<Vec<Message>>,
    /// The `!` ROOT modifier's captured subtrees (`control.cc`'s root-modifier). When any node is `!`-tagged,
    /// OpenSCAD renders ONLY those subtrees — ancestors + siblings discarded — so [`eval_stmt`] diverts a
    /// `!`-node's geometry HERE instead of into the local `nodes`, and [`run_stmts`] uses this as the whole
    /// program output whenever it's non-empty. A shared buffer because `!` can sit arbitrarily deep in the
    /// tree, not just at the top level. Empty in the overwhelmingly common no-`!` program.
    root_override: RefCell<Vec<Geo>>,
    /// The caller-supplied mesh table an `import`/`surface` resolves its `file=` path against (M.3). `None`
    /// on the non-loader `build_ctx`/`default` paths — no table means every import is a need. Read-only
    /// during geometry eval.
    files: Option<&'a FileTable>,
    /// The `file=` paths this run asked for but the table didn't have (M.3): `import`/`surface` records each
    /// here and keeps going on an EMPTY placeholder mesh, so ONE run surfaces ALL its needs (a mesh rarely
    /// gates control flow). A `BTreeSet` dedups + orders them deterministically; drained into
    /// [`Resolution::Incomplete`] (or a LOUD error on the no-table entries).
    file_needs: RefCell<BTreeSet<String>>,
    /// Live user-module call depth — the Safari-cliff guard. Statement eval is HOST-recursive (a module
    /// body re-enters `eval_stmt`), so a self-recursive module could overflow; this bounds it, LOUD
    /// ([`MAX_MODULE_DEPTH`]), never a silent stack crash.
    module_depth: Cell<usize>,
    /// The children-frame STACK for `children()` (I.2.5): each active module call pushes its call-site
    /// children + the caller's scope, so a `children()` in the body renders them LATE-bound. A stack, so
    /// nested module calls each see their own children; `children()` pops during eval so a `children()`
    /// inside the rendered children refers to the ENCLOSING call, not this one.
    children_stack: RefCell<Vec<ChildrenFrame<'a>>>,
    /// The STACK of scope-LOCAL module definitions (L.2.8m), each with the DEFINING scope it was hoisted
    /// in: a `module f(){…}` inside a module body / block is visible only within that scope (can't go in the
    /// per-file `islands`), AND its body must CLOSE OVER that scope — BOSL2's `testvercmp` calls a sibling
    /// nested `function diversify`, which only exists in the enclosing body scope. Entering a block with
    /// nested module defs pushes `(store, defining_scope)` (see [`eval_nodes`]); [`Ctx::resolve_module`]
    /// checks the stack (innermost first) BEFORE the island and hands back the captured scope as the local
    /// module's lexical base. Dynamically scoped for VISIBILITY (a nested module reaches a module CALLED
    /// during the body), a v1 simplification — real code never names a local module the same as a global
    /// one, so the dynamic reach never resolves the wrong def. Popped on body exit.
    local_modules: RefCell<Vec<(loader::ModStore<'a>, Scope)>>,
    /// The NAMES of the currently-active user-module instantiations, innermost last — OpenSCAD's module
    /// call stack, for `parent_module(n)` / `$parent_modules` (`control.cc`). `call_user_module` pushes the
    /// callee's name before its body runs, pops after; `parent_module(n)` reads `stack[len-1-n]` (0 = the
    /// current module, 1 = its parent). BOSL2's `deprecate()` echoes `parent_module(1)` to name the
    /// deprecated module. `&'a str` — the name is borrowed from the call-site AST.
    module_stack: RefCell<Vec<&'a str>>,
    /// The evaluator's ONE advancing RNG for SEEDLESS `rands()` (I.2.8b). OpenSCAD draws every seedless
    /// call from a single global engine, so consecutive `rands()` DIFFER; a fresh engine per call would
    /// repeat and collapse BOSL2's random line/triangle to a degenerate case. Seeded once per evaluation
    /// with a fixed default (→ reproducible, bit-identical) then advanced per seedless draw — the one
    /// deliberately eval-order-stateful builtin (see [`rng::RandStream`]). Seeded `rands(…, seed=k)`
    /// bypasses this (a fresh engine → oracle-exact + pure).
    rand_stream: RefCell<rng::RandStream>,
    /// The eval-memo cache (N.2c): user-function-call results keyed on (fn, env, args, reaching `$`-context).
    /// Per-program (dies with the `Ctx`); off under `FAB_EVAL_CACHE=0`. See [`eval_cache`].
    cache: eval_cache::CacheCell,
    /// The CSG-memo cache (J.5.2a): a child-less user-module call's produced `Geo` subtree keyed on
    /// (body, home, params, reaching `$`-context). Per-program; gated by [`config`](Self::config). See [`mod_cache`].
    mod_cache: mod_cache::CacheCell,
    /// The execution knobs — JIT + the two memo caches (their gates + tuning caps). One place, threaded from the
    /// entry (env-read or embedder-set) instead of a dozen per-module `OnceLock`s. See [`Config`].
    config: Config,
    /// Monotone count of IMPURE READS a call's subtree performed — currently only `parent_module`, which
    /// reads the module-instantiation stack (state NOT in the cache key + no message/rand delta to betray it).
    /// The purity fence snapshots this before/after a call and DECLINES memoization if it moved, so any call
    /// that (transitively) reads `parent_module` re-runs every time — closing the one wrong-hit class the
    /// message/rand fence can't see. Transitive for free: a nested read bumps it, the outer call sees the delta.
    impure_reads: std::cell::Cell<u64>,
    /// Running count of deterministic EVAL-STEPS this evaluation has burned — the Q.5 resource budget's
    /// accumulator, charged at the stack machine's per-task chokepoint + the `each`-splice path. Checked
    /// against [`Config::eval_budget`](Config); when a bound is set and this exceeds it, [`Ctx::charge`]
    /// fails LOUD ([`Error::Eval`](crate::Error::Eval)) instead of letting an untrusted input burn 10s/2GB.
    /// A `Cell` (single-threaded interior mutability), same as [`impure_reads`](Self::impure_reads); starts
    /// at 0. No-op when the budget is `None` (the default), so the trusted path pays only one predictable
    /// not-taken branch per task.
    eval_steps: std::cell::Cell<u64>,
    /// Count of user-function calls currently IN FLIGHT (a [`Task::Apply`] committed to interpreting its
    /// body, not yet balanced by its [`Task::CallReturn`]) — the runaway-recursion detector's accumulator,
    /// checked against [`MAX_CALL_DEPTH`]. JIT hits and cache hits never enter (their result lands
    /// immediately), so the count is exactly the interpreted call depth. A `Cell`, same as the others.
    live_calls: std::cell::Cell<u32>,
    /// The desktop numeric-JIT hook (P.1.2), or `None` (wasm, or a program with nothing compiled). Built
    /// ONCE at setup by the caller-supplied [`NumericJitFactory`] and OWNED here — the registry CLONES the
    /// AST it needs (P.1.6 rung B on-demand recompile), so a `Box<dyn NumericJit>` still needs no `'a`. When
    /// present, an eligible user-function call ([`Task::Apply`] with a `jit` name — all-positional) is offered
    /// to it before the body is interpreted; the hook scalarizes the args and declines what it can't compile.
    /// A `Some` result is bit-identical to interpreting, so this only ever changes speed. `None` everywhere
    /// the interpreter is the whole story (raw-AST eval, wasm, no factory).
    jit: Option<Box<dyn NumericJit>>,
}

/// One active module call's children context: the call-site child statements (borrowed from the AST) +
/// the CALLER's scope AND module ISLAND they evaluate in (OpenSCAD renders `children()` in the
/// instantiation context — same lexical scope AND same module-resolution scope as the call site, I.9.5).
struct ChildrenFrame<'a> {
    /// The call-site GEOMETRY children — lone-`;` empties AND child-block `assignment`s filtered out. Neither
    /// is a child in OpenSCAD: an `Empty`/`Assignment` counts toward neither `$children` nor `children(i)`,
    /// so keeping either here would misalign both (L.5.2 — a `{ shape; x = 5; shape; }` block is 2 children,
    /// not 3, and `children(1)` is the second SHAPE). This is also what BOSL2's `attachable(){ shape;
    /// union(){}; }` needs to see as exactly 2 children (the terminating `;` after the empty union is not a
    /// third).
    stmts: Vec<&'a Stmt>,
    /// The child-block's `assignment` statements, in source order — NOT children, but their bindings ARE in
    /// scope for every geometry child (OpenSCAD child-block locals, e.g. `BOSL2logo`'s `sbez = …;` read by a
    /// sibling `path_sweep(...)`). Prepended to the rendered stmts in `children()` so `hoist_scope` binds them
    /// (whole-scope, last-wins) before the selected geometry evaluates.
    assigns: Vec<&'a Stmt>,
    scope: Scope,
    island: usize,
}

impl<'a> Ctx<'a> {
    /// Charge `n` eval-steps against the Q.5 resource budget; fail LOUD once the running total exceeds
    /// [`Config::eval_budget`](Config). A NO-OP when the budget is `None` (the default) — one not-taken
    /// branch, so the trusted/unbounded path pays nothing measurable. The count is DETERMINISTIC (eval-steps,
    /// never wall-time), so a bounded `(program, budget)` fails at the exact same step on every machine — the
    /// reproducibility doctrine #36 + the fuzzers depend on. `saturating_add` so a pathological charge can't
    /// wrap the counter past the bound.
    #[inline]
    fn charge(&self, n: u64) -> crate::Result<()> {
        if let Some(budget) = self.config.eval_budget {
            let used = self.eval_steps.get().saturating_add(n);
            self.eval_steps.set(used);
            if used > budget {
                return Err(crate::Error::Eval(format!(
                    "eval budget exceeded: {used} steps > {budget} (untrusted-input resource limit; raise \
                     FAB_EVAL_BUDGET or Config::eval_budget)"
                )));
            }
        }
        Ok(())
    }

    /// Charge the iteration COUNT of `iterable` up front — BEFORE [`iter_values`] materializes it into a
    /// `Vec` — so a giant range/list is rejected before its (RANGE_MAX-bounded, but still large) allocation,
    /// not merely as the loop walks it. The count is exactly the length `iter_values` would yield (a range's
    /// is `RANGE_MAX`-capped like its iterator). No-op when the budget is `None`. Pairs with the per-task
    /// charge: this bounds the loop's SIZE + its up-front alloc, the task charge bounds each body's WORK.
    fn charge_iterable(&self, iterable: &Value) -> crate::Result<()> {
        let n = match iterable {
            Value::NumList(xs) => xs.len() as u64,
            Value::List(xs) => xs.len() as u64,
            Value::Range { start, step, end } => match range_len(*start, *step, *end) {
                n if n >= RANGE_TOO_MANY => 0, // AD.3: expansion warns + yields nothing — charge nothing
                n => n.min(RANGE_MAX),
            },
            Value::Str(s) => s.chars().count() as u64,
            Value::Object(o) => o.len() as u64, // key iteration (AF.4)
            _ => 1, // a scalar iterates as a single element (iter_values' `other` arm)
        };
        self.charge(n)
    }

    /// Resolve a MODULE name against `island`'s lexical scope (I.9.5): the island's OWN defs first (a
    /// local/`include` def always beats a `use`-imported one), then each `use`d island in reverse source
    /// order (textually-last `use` wins). Returns the def PLUS its home island — the body must evaluate
    /// with the home as its current island so ITS calls resolve where the module was defined, not where
    /// it was called. `None` → no user module by that name here, so the call falls through to a builtin
    /// primitive (this is the fallthrough that turns `builtins.scad`'s `_cube`-body `cube` into the
    /// BUILTIN cube instead of the program's redefinition).
    fn resolve_module(
        &self,
        island: usize,
        name: &str,
    ) -> Option<(loader::ModDef<'a>, usize, Option<Scope>)> {
        // Scope-LOCAL module defs (L.2.8m) win first, innermost scope out — a module-body `module f(){…}`
        // shadows any file/`use` def of the same name within that body. Its home island stays the CURRENT
        // island (textually part of that file, so its own calls resolve where it was written), and it
        // carries its DEFINING scope as its lexical base (so its body sees sibling nested funcs/vars).
        if let Some((def, base)) = self
            .local_modules
            .borrow()
            .iter()
            .rev()
            .find_map(|(store, base)| store.get(name).map(|&def| (def, base.clone())))
        {
            return Some((def, island, Some(base)));
        }
        let here = &self.islands[island];
        if let Some(&def) = here.modules.get(name) {
            return Some((def, island, None));
        }
        here.uses
            .iter()
            .rev()
            .find_map(|&u| self.islands[u].modules.get(name).map(|&def| (def, u, None)))
    }

    /// Push a [`Message::Warning`] onto the ordered console log — the same buffer `echo` writes to, so
    /// warnings and echoes keep their emission order (I.5).
    fn warn(&self, message: String) {
        self.messages.borrow_mut().push(Message::Warning(message));
    }

    /// Resolve an `import`/`surface` `file=` path to an [`Imported`] payload (M.3): the caller-supplied one
    /// if the [`FileTable`] has it, else an EMPTY placeholder of the extension's dimension ([`Imported::empty_for`])
    /// — recording `raw` as a [`SourceNeed::File`] so the caller can read it and re-run. A `None` path (an
    /// absent or non-string `file=`, e.g. `import(undef)`) has nothing to name, so it's an empty 3D result
    /// with no need — matching the oracle's warn-and-render on a bad path (the warning TEXT is #94 / M.6).
    /// Never silently WRONG: a real missing file becomes a LOUD need (or, on the no-table paths, a LOUD error
    /// downstream), not a quietly-empty result.
    fn request_file(&self, raw: Option<String>) -> Imported {
        let Some(raw) = raw else {
            return Imported::Mesh(Mesh::new());
        };
        if let Some(imported) = self.files.and_then(|t| t.get(&raw)) {
            imported.clone()
        } else {
            let placeholder = Imported::empty_for(&raw);
            self.file_needs.borrow_mut().insert(raw);
            placeholder
        }
    }

    /// Drain the File needs discovered this run into the ordered [`SourceNeed`] set (M.3). Empty → the run
    /// closed; non-empty → the caller must supply the meshes and evaluate again.
    fn take_file_needs(&self) -> Vec<SourceNeed> {
        std::mem::take(&mut *self.file_needs.borrow_mut())
            .into_iter()
            .map(|raw| SourceNeed::File { raw })
            .collect()
    }
}

/// Max nested user-module call depth before we bail LOUD. Since M.3, statement/geometry eval is HEAP-bounded
/// (the explicit-stack driver — no host recursion), so this is NO LONGER crash-safety; it's a runaway DETECTOR,
/// turning an infinite `module m() { m(); }` into a fast LOUD error instead of a slow crawl to OOM. Set WELL
/// ABOVE OpenSCAD's own module-recursion limit (empirically ~5–8 k on 2026.06.12, where it errors "Recursion
/// detected") — because we're heap-bounded and OpenSCAD's C++ tree-walker is host-stack-bound, we accept
/// recursion depths OpenSCAD refuses. (A children()/wrapper chain doubles the depth per level, so headroom
/// matters for deep attachable chains.) A memory/step budget could replace this later.
const MAX_MODULE_DEPTH: usize = 100_000;

/// Max IN-FLIGHT user-function calls (entered, result not yet landed) before we bail with upstream's
/// "Recursion detected" verdict. The task machine accidentally TAIL-COLLAPSES a call whose body IS the
/// next call (`function crash() = crash();` — the outer body-eval BECOMES the inner dispatch, so the
/// task stack never grows), which is why no stack/depth guard ever fired on the census's zero-progress
/// recursion files: they grind flat for hours, growing only the dynamic scope chain. Counting calls
/// IN FLIGHT catches them all — zero-progress AND progressing-but-unbounded (`add_up_to(-1)` changes
/// its arg every call, so cycle detection on args would miss it; upstream still errors, proving THEIR
/// guard is a counter too: the 2026 TCO loop caps at 1,000,000 "Recursion detected"). Ceiling matches
/// upstream's; the corpus's deepest LEGIT recursion (the 500 k mutual-closure chain) sits at 2× headroom.
const MAX_CALL_DEPTH: u32 = 1_000_000;

/// AD.4: upstream's C-style-for iteration limit, oracle-probed EXACTLY — 1,000,000 iterations complete
/// clean, 1,000,001 is "ERROR: For loop counter exceeded limit". Replaces the old silent `RANGE_MAX`
/// break, which returned a 10M-element PARTIAL result for an infinite `for(b=0; b!=1; b=0)` — slow AND
/// wrong twice over (upstream errors, and a silent truncation is the never-silently-wrong doctrine's
/// exact villain).
const MAX_CFOR_ITERATIONS: u64 = 1_000_000;

/// One step on the evaluator's explicit work-stack. Each `Eval` carries the [`Scope`] it evaluates
/// in (an `Rc<Frame>` clone — cheap), so a call's body can evaluate in the callee's scope while the
/// caller's continuation waits on the same stack (I.2.3). Value-combining tasks need no scope.
enum Task<'a> {
    /// Evaluate this expression in this scope, pushing its result onto the value stack.
    Eval(&'a Expr, Scope),
    /// Push a ready-made value — the result of a call the dispatcher short-circuits (an unknown function →
    /// `undef`, warn-and-continue like OpenSCAD, L.5.7) without evaluating a body.
    Const(Value),
    /// Pop two values, apply the binary op, push the result.
    Binary(BinOp),
    /// Pop one value, apply the unary op, push the result.
    Unary(UnOp),
    /// Pop one value per element and build a vector — a COMPREHENSION element's value is SPLICED (its
    /// list's elements appended), a plain element is appended as one.
    VectorSplice(&'a [Expr]),
    /// Pop the index then the base, apply `base[index]`.
    Index,
    /// Pop the base, apply member access `base.field` (`.x`/`.y`/`.z` → index 0/1/2).
    Member(&'a str),
    /// Pop end, (step if `stepped`), start; build a range value.
    Range { stepped: bool },
    /// Pop the condition, then schedule the taken branch (in `scope`).
    Ternary {
        then: &'a Expr,
        els: &'a Expr,
        scope: Scope,
    },
    /// Pop `names.len()` values and bind them (params, then `$`-args) into a fresh child of `base`,
    /// seeded first with the CALLER's dynamic `$`-context, then evaluate `body` in that call scope. The
    /// heart of a call — no host recursion, so recursion depth is bounded by the heap (`corner_brace`).
    /// `provided[i]` marks a name that came from an explicit ARG (vs a default/undef): bind the defaults
    /// FIRST, then the args, so an argument wins over a default even when a param NAME is DUPLICATED (see
    /// [`bind_module_scope`] — same OpenSCAD two-phase rule, here for functions).
    Apply {
        names: Vec<Rc<str>>,
        provided: Vec<bool>,
        body: &'a Expr,
        base: Scope,
        caller: Scope,
        /// The called function's NAME, or `None` for a closure / a `$`-arg call (`push_call` clears it
        /// when dollars append past the params — the one JIT arity hazard). Two consumers: the numeric-JIT
        /// offer (P.1.2 — [`Ctx::jit`] gets first refusal when this is `Some`; only a shape hint, the JIT
        /// still checks its own registry + the runtime `all-Num` guard) and the AD.2 recursion verdict
        /// (upstream names the function in "Recursion detected calling function 'x'").
        name: Option<&'a str>,
    },
    /// Pop an evaluated CALLEE; if it's a [`Value::Function`], invoke it (its body evaluates in the
    /// captured env). Anything else → `undef` (calling a non-function). The dynamic-callee path:
    /// `(expr)(args)`, or a variable holding a closure.
    CallValue { args: &'a [Arg], caller: Scope },
    /// Pop the builtin's argument values, split into positional/named, and apply the builtin `name`.
    Builtin { name: &'a str, args: &'a [Arg] },
    /// Apply a registered INTRINSIC (O.1): pop its `nargs` positional arg values (evaluated by the preceding
    /// `Eval` tasks, exactly like `Builtin`), call the native impl, push its result. Reached only for an
    /// all-positional call to a function whose body fingerprint-matched the registry.
    Intrinsic {
        func: intrinsics::Intrinsic,
        nargs: usize,
    },
    /// AB.2: pop `args.len()` evaluated echo-arg values, emit the `ECHO:` line, then schedule `body`
    /// (or push undef). Scheduling the body as a TASK is the point — an `echo(…) body` chain (or a
    /// recursive function whose body is one) used to re-enter the evaluator on the HOST stack per
    /// link, which is exactly how tail-recursion-tests.scad overflowed.
    EchoEmit {
        args: &'a [Arg],
        body: Option<&'a Expr>,
        scope: Scope,
    },
    /// AB.2: pop the assert's evaluated CONDITION (iff one was scheduled), evaluate the message
    /// (eagerly, impure-reads rolled back — a documented depth-1 re-entry; recursion routed through
    /// an assert MESSAGE stays cliffy and absurd), fail LOUD on falsy, else schedule `body`.
    AssertCheck {
        cond: Option<&'a Expr>,
        msg: Option<&'a Expr>,
        body: Option<&'a Expr>,
        scope: Scope,
    },
    /// AB.3: dispatch ONE comprehension element. A GENERATOR (`for`/`each`/`if`, or a `let` whose
    /// body is one) leaves its CONTRIBUTION VECTOR on the value stack; a PLAIN element leaves its
    /// raw value. Consumers know which statically via [`is_comprehension`].
    LcElem(&'a Expr, Scope),
    /// AB.3: pop a plain element's value and wrap it as a one-element contribution vector — used
    /// where a GENERATOR result is contractually required (an `if` branch, a bindingless `for` body).
    WrapContribution,
    /// AB.3: pop the comprehension-`if` condition; schedule the taken branch's contribution (or push
    /// the empty contribution).
    LcIfBranch {
        then: &'a Expr,
        els: Option<&'a Expr>,
        scope: Scope,
    },
    /// AB.3: pop the `each` inner result and splice ONE level; `splice_inner` = the inner was a
    /// generator (its result is a contribution vector whose ELEMENTS are the contributions).
    LcEachSplice { splice_inner: bool },
    /// AB.3: a `for` binding chain — split the next binding (evaluating its iterable via a task
    /// would reorder against `charge_iterable`; the iterable evals here as a bounded re-entry), or
    /// schedule the body when none remain.
    LcForBindings {
        bindings: &'a [Arg],
        body: &'a Expr,
        scope: Scope,
    },
    /// AB.3: one `for` binding level mid-iteration. `pending` = the previous item's result awaits
    /// popping into `acc` (`splice_item` says splice-vs-append); then bind the next item into the
    /// REUSED `frame` (N.2 in-place rebind) and schedule its work, or push `acc` when exhausted.
    LcForNext {
        rest: &'a [Arg],
        body: &'a Expr,
        var: Rc<str>,
        items: std::vec::IntoIter<Value>,
        frame: Scope,
        acc: Vec<Value>,
        splice_item: bool,
        pending: bool,
    },
    /// AB.3: a C-style `for` iteration head — build the loop scope, test `cond`, schedule body +
    /// update (clause exprs evaluate as bounded re-entries; loop NESTING is task-framed).
    LcForCStep {
        cond: &'a Expr,
        update: &'a [Arg],
        body: &'a Expr,
        outer: Scope,
        vars: Vec<(String, Value)>,
        iterations: u64,
        acc: Vec<Value>,
        splice_item: bool,
    },
    /// AB.3: after a C-style body — pop its result into `acc`, apply the update clause
    /// SEQUENTIALLY, and loop.
    LcForCUpdate {
        cond: &'a Expr,
        update: &'a [Arg],
        body: &'a Expr,
        outer: Scope,
        loop_scope: Scope,
        vars: Vec<(String, Value)>,
        iterations: u64,
        acc: Vec<Value>,
        splice_item: bool,
    },
    /// Pop the just-evaluated binding value for `bindings[idx]`, bind it in a child of `scope`,
    /// then either evaluate the next `let` binding in that scope or (no bindings left) evaluate
    /// `body`. `let` bindings are SEQUENTIAL — a later one sees the earlier ones — EXCEPT a
    /// duplicate of a name already bound in this SAME `let`, which upstream IGNORES (first wins,
    /// warning; AH.2.3 — `let($a=2,b=3,$a=4) $a*b` is 6, not 12). The full slice + index ride
    /// along so the duplicate check is a static look-back, no runtime name set.
    LetStep {
        bindings: &'a [Arg],
        idx: usize,
        body: &'a Expr,
        scope: Scope,
    },
    /// Push an `undef` — the value of an unfilled, defaultless parameter slot.
    PushUndef,
    /// Short-circuit a `&&`/`||`: the LHS is on the value stack. `||` on a TRUTHY LHS yields `true` and
    /// `&&` on a FALSY LHS yields `false` — the RHS is NEVER evaluated (so its asserts / recursion don't
    /// run). Otherwise the RHS is evaluated and combined with the LHS by the normal op. This is
    /// load-bearing for OpenSCAD: BOSL2 guards recursion base-cases + assertions behind `a || b` / `a &&
    /// b`, so eager evaluation makes guarded asserts fire and guarded recursion never terminate.
    ShortCircuit {
        op: BinOp,
        rhs: &'a Expr,
        scope: Scope,
    },
    /// DEBUG-only ([`trace`]): peek the top value (a call's just-produced return) and echo `name => value`
    /// without consuming it. Pushed BELOW a call's tasks so it fires the instant the return lands, before
    /// the caller reads it. Only ever pushed when the `FAB_TRACE` trace is on, so it's absent otherwise.
    TraceReturn { name: &'a str },
    /// Dev probe (`FAB_PROFILE_FNS`): close the [`fnprofile`] shadow-stack window the dispatch site's
    /// [`fnprofile::enter_fn`] opened — books the call's self + outermost-inclusive time. Pushed like
    /// [`TraceReturn`] (below the call's tasks → fires the instant the return lands); nameless because the
    /// task stack's LIFO order makes windows strictly well-nested. Absent when the probe is off.
    FnTimeReturn,
    /// N.2c eval-memo: peek the top value (a memoizable call's just-produced result — like [`TraceReturn`],
    /// pushed below the body so it fires the instant the result lands) and, IF the call's subtree left no
    /// observable side effect (the `snap` counters are unmoved), store it under `key`. NEVER a `geo_stack`
    /// cleanup task — it must not fire on the error path (an errored `?` abandons the whole task stack, so an
    /// errored call is structurally uncacheable). Absent when the cache is off.
    CacheStore {
        key: eval_cache::Key,
        snap: PuritySnap,
    },
    /// AD.2: balance the [`Ctx::live_calls`] increment an interpreted [`Task::Apply`] made — pushed below
    /// the body (like [`FnTimeReturn`]) so it fires the instant the call's result lands. The error path
    /// abandons the task stack WITHOUT firing these; fine, because a fatal `Eval` error ends the whole
    /// evaluation — the counter dies with the run (and an inflated count could only ever fire the guard
    /// EARLIER, never wrongly pass a runaway).
    CallReturn,
}

/// The side-effect counters snapshotted at a memoizable call's MISS; [`Task::CacheStore`] re-reads them when
/// the body's value lands and stores only if NONE moved (the call was pure). See [`Ctx::impure_reads`].
struct PuritySnap {
    messages: usize,
    draws: u64,
    closures: usize,
    impure_reads: u64,
}

/// Evaluate an expression to a [`Value`] on the explicit stack.
///
/// # Errors
/// [`Error::Unimplemented`](crate::Error::Unimplemented) for constructs deferred past v0 (function
/// calls, indexing, member access, ranges, heterogeneous/nested vectors).
pub fn eval_expr(root: &Expr, scope: &Scope) -> crate::Result<Value> {
    eval_with_ctx(root, scope, &Ctx::default())
}

/// Interpret a call to `program`'s function `name` with numeric `args` — the interpreter ORACLE the numeric
/// JIT validates against (`fast == JIT`). Unlike [`eval_expr`], it builds the program's function store AND
/// publishes its top-level CONSTANTS, so the body can call OTHER user functions AND read top-level constants
/// (exactly the call chains + globals the JIT inlines). Args bind to the leading parameters in order; unfilled
/// params take their defaults / `undef` as in a normal call. Intended for the `fab-jit` differential — a
/// single self-contained program (no `use`/`include` graph). For a whole library (many parsed files), build a
/// [`FnOracle`] once and reuse it.
///
/// # Errors
/// [`Error::Unknown`](crate::Error::Unknown) if no function named `name` is defined; any evaluation error
/// from the body (e.g. an `assert` failure — the JIT's raise path corresponds to this).
pub fn interpret_fn(program: &Program, name: &str, args: &[Value]) -> crate::Result<Value> {
    let functions: Vec<(&str, &[Parameter], &Expr)> = program
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::FunctionDef { name, params, body } => {
                Some((name.as_str(), params.as_slice(), body))
            }
            _ => None,
        })
        .collect();
    let stmt_refs: Vec<&Stmt> = program.stmts.iter().collect();
    let globals = hoisted_assignments(&stmt_refs);
    FnOracle::new(&functions, &globals)?.call(name, args)
}

/// A build-ONCE interpreter oracle for the numeric-JIT differential (`fast == JIT`): construct it from a
/// program's functions + top-level constants once, then interpret any function many times. The battery
/// hammers each compiled function across dozens of input rows, and rebuilding the function store + republishing
/// the constants per row — as [`interpret_fn`] does — is O(constants) each call, quadratic over a whole
/// library. This pays that setup once. The constants are hoisted (whole-scope, last-wins, first-occurrence
/// order) and PUBLISHED into island 0's global, so a called function's body resolves them — matching the JIT,
/// which inlines each constant's value-expr. A constant whose RHS ERRORS under the interpreter is skipped (the
/// flat cross-file merge the corpus differential feeds isn't a real single program; only self-contained
/// NUMERIC constants ever feed a compiled function, and those never error), keeping the oracle robust without
/// masking a numeric divergence — a compiled function only reads a constant the JIT could also compile.
pub struct FnOracle<'a> {
    ctx: Ctx<'a>,
    /// The published top-level constant scope — the lexical base a function body binds its params onto.
    global: Scope,
    /// name → (params, body), for finding the function to interpret.
    functions: BTreeMap<&'a str, (&'a [Parameter], &'a Expr)>,
}

impl<'a> FnOracle<'a> {
    /// Build the oracle from borrowed functions + top-level constants (the exact shape [`JitDef`]/[`JitConst`]
    /// carry, so both sides of the differential see identical inputs). Publishes the constants into island 0.
    ///
    /// # Errors
    /// Never returns `Err` today (a constant whose RHS errors is skipped, not fatal); `Result` so a future
    /// setup failure can surface LOUD rather than silently.
    pub fn new(
        functions: &[(&'a str, &'a [Parameter], &'a Expr)],
        globals: &[(&'a str, &'a Expr)],
    ) -> crate::Result<Self> {
        let fn_map: BTreeMap<&str, (&[Parameter], &Expr)> =
            functions.iter().map(|&(n, p, b)| (n, (p, b))).collect();
        // The `Ctx` function store carries the home-island tag every function needs; a flat single-scope oracle
        // homes them all at island 0 (its published global holds the constants).
        let ctx_functions: BTreeMap<&str, (loader::FnDef, usize)> = functions
            .iter()
            .map(|&(n, p, b)| (n, ((p, b), 0usize)))
            .collect();
        let intrinsics = build_intrinsics(&ctx_functions);
        let mut ctx = Ctx {
            functions: ctx_functions,
            intrinsics,
            island_globals: RefCell::new(vec![Scope::new()]),
            islands: vec![loader::Island {
                modules: BTreeMap::new(),
                functions: BTreeMap::new(),
                assignments: Vec::new(),
                uses: Vec::new(),
            }],
            closures: RefCell::default(),
            messages: RefCell::default(),
            root_override: RefCell::default(),
            files: None,
            file_needs: RefCell::default(),
            module_depth: Cell::default(),
            children_stack: RefCell::default(),
            local_modules: RefCell::default(),
            module_stack: RefCell::default(),
            rand_stream: RefCell::new(rng::RandStream::new()),
            cache: eval_cache::CacheCell::default(),
            mod_cache: mod_cache::CacheCell::default(),
            // The oracle is the pure-interpreter baseline (jit: None above) — no accelerators.
            config: Config::default(),
            impure_reads: std::cell::Cell::new(0),
            eval_steps: std::cell::Cell::new(0),
            live_calls: std::cell::Cell::new(0),
            jit: None, // the oracle IS the interpreter baseline — never route it through the JIT
        };
        // Publish the constants into island 0's global (whole-scope, last-wins, first-occurrence order), so a
        // called function's body resolves them (`dispatch_call` bases a call on `island_globals[home = 0]`).
        // Publishing incrementally makes a forward/self-reference read `undef`, the interpreter's whole-scope
        // rule. An RHS that errors under the flat merge is skipped (see the type doc).
        let mut scope = Scope::new();
        for &(name, expr) in globals {
            if let Ok(v) = eval_with_ctx(expr, &scope, &ctx) {
                scope.bind(name.to_string(), name_closure(v, name));
                if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(0) {
                    *slot = scope.clone();
                }
            }
        }
        // O.5.1: constants are published → arm the const-guarded intrinsics (they count as "the
        // interpreter" here exactly like the unguarded ones `build_intrinsics` wired above).
        for (name, func) in arm_guarded_intrinsics(&ctx) {
            ctx.intrinsics.insert(name, func);
        }
        Ok(Self {
            ctx,
            global: scope,
            functions: fn_map,
        })
    }

    /// Interpret `name(args)` — the slow side of the differential. Params bind onto the published constant
    /// scope (shadowing a like-named constant), then the body evaluates with calls + constants resolving.
    ///
    /// # Errors
    /// [`Error::Unknown`](crate::Error::Unknown) if `name` isn't defined; any body evaluation error (an
    /// `assert` failure — the JIT's raise/`None` path corresponds to this).
    pub fn call(&self, name: &str, args: &[Value]) -> crate::Result<Value> {
        let Some(&(params, body)) = self.functions.get(name) else {
            return Err(crate::Error::Unknown(format!("function `{name}`")));
        };
        let mut scope = self.global.clone();
        for (p, v) in params.iter().zip(args) {
            scope.bind(p.name.clone(), v.clone());
        }
        eval_with_ctx(body, &scope, &self.ctx)
    }
}

/// Illustration/bench seam (P.1.5): the native INTRINSIC registered for a function's exact `(name, params,
/// body)`, or `None`. It's the very fn-pointer [`Task::Intrinsic`] dispatches — exposed so a benchmark can
/// time the intrinsic tier in isolation, the same way the JIT registry's `CompiledFn` is timed. Not for
/// production use (the interpreter reaches intrinsics through the fingerprint gate at `build_ctx`).
#[doc(hidden)]
#[must_use]
pub fn bench_intrinsic(name: &str, params: &[Parameter], body: &Expr) -> Option<IntrinsicFn> {
    // Bench seam only — no dep/const guard here (the bench times the fn pointer, it doesn't dispatch it).
    intrinsics::resolve(name, params, body).map(|e| e.func)
}

/// The intrinsic tier's entry shape: the call's evaluated args in, value out — the very fn pointer
/// [`Task::Intrinsic`] dispatches.
pub type IntrinsicFn = fn(&[Value]) -> crate::Result<Value>;

/// Evaluate an expression with a function-store [`Ctx`] in scope (so calls resolve). At the top level
/// the lexical `global` (the base for function bodies) IS the eval scope.
pub(super) fn eval_with_ctx<'a>(
    root: &'a Expr,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Value> {
    eval_with_global(root, scope, scope, ctx)
}

/// Evaluate `root` in `scope`, with `global` as the LEXICAL base for any function body called during
/// it (a call's body evaluates in `global.child()` + its params, NOT the caller's locals — OpenSCAD
/// functions are lexically scoped; `$`-var dynamic override is I.2.2). `global` is threaded (not
/// re-derived from `scope`) so a nested eval — a comprehension body carrying loop variables — still
/// resolves function bodies against the TOP-LEVEL globals, not the loop scope.
#[allow(
    clippy::too_many_lines,
    reason = "the explicit-stack work-loop: one match arm per Task variant — splitting it would just \
    scatter the machine across helpers that each need the shared tasks/values stacks"
)]
fn eval_with_global<'a>(
    root: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Value> {
    let global = global.clone();
    let mut tasks: Vec<Task<'a>> = vec![Task::Eval(root, scope.clone())];
    let mut values: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        // Q.5 resource budget: one charge per task — the universal chokepoint. Every expression eval and
        // every comprehension trip pushes/pops tasks here (a per-element comprehension body is its own
        // `eval_with_global` → ≥1 task → ≥1 charge), so a nested/runaway comprehension that would burn
        // 10s/2GB trips the budget in bounded steps instead. No-op unless a bound is set (default None).
        ctx.charge(1)?;
        match task {
            Task::Eval(e, s) => eval_node(e, &s, ctx, &mut tasks, &mut values)?,
            Task::Const(v) => values.push(v),
            Task::Binary(op) => {
                // pop order: rhs was pushed after lhs, so it's on top.
                let rhs = values.pop().unwrap_or(Value::Undef);
                let lhs = values.pop().unwrap_or(Value::Undef);
                values.push(ops::apply_binary(op, lhs, rhs));
            }
            Task::Unary(op) => {
                let v = values.pop().unwrap_or(Value::Undef);
                values.push(ops::apply_unary(op, v));
            }
            Task::VectorSplice(elems) => {
                let vals = values.split_off(values.len().saturating_sub(elems.len()));
                let mut out = Vec::new();
                for (elem, val) in elems.iter().zip(vals) {
                    if is_comprehension(elem) {
                        splice_into(val, &mut out);
                    } else {
                        out.push(val);
                    }
                }
                values.push(build_vector(out));
            }
            Task::Index => {
                // index was pushed after base, so it's on top.
                let index = values.pop().unwrap_or(Value::Undef);
                let base = values.pop().unwrap_or(Value::Undef);
                values.push(ops::index(base, &index));
            }
            Task::Member(field) => {
                let base = values.pop().unwrap_or(Value::Undef);
                values.push(ops::member(base, field));
            }
            Task::Range { stepped } => {
                // pushed start, [step], end → pop end, [step], start.
                let end = values.pop().unwrap_or(Value::Undef);
                let step = if stepped {
                    values.pop().unwrap_or(Value::Undef)
                } else {
                    Value::Num(1.0)
                };
                let start = values.pop().unwrap_or(Value::Undef);
                values.push(build_range(&start, &step, &end));
            }
            Task::Ternary { then, els, scope } => {
                let cond = values.pop().unwrap_or(Value::Undef);
                let branch = if cond.is_truthy() { then } else { els };
                tasks.push(Task::Eval(branch, scope));
            }
            Task::Apply {
                names,
                provided,
                body,
                base,
                caller,
                name,
            } => {
                let vals = values.split_off(values.len().saturating_sub(names.len()));
                // Numeric-JIT fast path (P.1.2): a compiled numeric function, offered the call BEFORE any
                // interpretation, IFF this call is JIT-eligible (`jit` name set at dispatch — all-positional).
                // The hook SCALARIZES the args itself (P.1.6 rung B): a `Num` is a scalar, a `NumList` a
                // fixed-small vector; a non-scalarizable arg (nested list, string, over-long vector, …) makes
                // it decline (`None`) and we interpret. A `Some(r)` is bit-identical to interpreting `body`,
                // so this is pure speed. The registry decides membership + shape; no work when the hook is off.
                // Weave the eval's live RandStream in by pointer (P.1.6 rung-D piece 1): a JIT'd seedless
                // `rands()` advances it through `jit_rand_next`, in the same eval order the interpreter draws.
                // `as_ptr()` hands out the cell's pointer WITHOUT a `borrow`, so no `RefMut` guard aliases the
                // `&mut` the helper transiently makes — sound because the compiled function is the sole
                // (single-threaded) accessor while it runs, and a non-rands body never dereferences it.
                if let (Some(name), Some(j)) = (name, ctx.jit.as_deref())
                    && let Some(out) = j.call_numeric(
                        name,
                        &vals,
                        ctx.rand_stream.as_ptr().cast::<core::ffi::c_void>(),
                    )
                {
                    // Re-tag the untyped native return into a `Value` (P.1.4e): a bool-returning function
                    // (a predicate, a comparison body) yields `Value::Bool`, not `Value::Num` — matching the
                    // interpreter, which the JIT crate's differential proves bit-for-bit.
                    values.push(match out {
                        JitOutcome::Num(n) => Value::Num(n),
                        JitOutcome::Bool(b) => Value::Bool(b),
                        JitOutcome::Vec(xs) => Value::num_list(xs),
                        // Already the reconstructed nested value (2c.1) — the JIT applied `build_vector`'s rule.
                        JitOutcome::Nested(v) => v,
                    });
                    continue;
                }
                // Dev probe (off unless FAB_REDUNDANCY=1): would an eval-memo cache pay? Key this call on
                // (fn, captured-env, args, reaching $-context) and count repeats — the cache's hit-rate ceiling.
                // `base` (the captured env) is load-bearing: a closure shares its body AST with siblings but
                // captures a distinct env, so without it the count OVER-states the safe ceiling (review B1).
                redundancy::record(body, &base, &vals, &caller);
                // N.2c eval-memo: on a HIT, push the cached result and skip binding + body entirely (the whole
                // point — the redundant subtree never runs). On a MISS, snapshot the side-effect counters and
                // queue a `CacheStore` that memoizes the result IFF the subtree turns out pure.
                // Gate the cache: enabled (config) AND not auto-OFF (N.2c.2 — a net-negative program disables
                // the whole gate after a bounded warmup, so this collapses to a single `state` read + branch)
                // AND args small enough to key cheaply (arg-cap — keeps a 300k-element `gaussian_rands`
                // comprehension from a giant per-call key hash). During WARMUP the gate also SAMPLES cost
                // (`key_work` scanned) vs benefit (a hit) so the auto-off can decide.
                let cache_state = ctx.config.eval_cache.then(|| ctx.cache.borrow().state());
                let warming = cache_state == Some(eval_cache::CacheState::Warmup);
                let cacheable = matches!(
                    cache_state,
                    Some(eval_cache::CacheState::Warmup | eval_cache::CacheState::Live)
                ) && eval_cache::worth_caching(&vals, ctx.config.eval_cache_argcap);
                let store = if cacheable {
                    let key = eval_cache::Key::new(body, &base, &vals, &caller);
                    let hit = {
                        let mut c = ctx.cache.borrow_mut();
                        let h = c.get(&key);
                        if warming {
                            c.note(
                                h.is_some(),
                                eval_cache::key_work(&vals, ctx.config.eval_cache_argcap),
                            );
                        }
                        h
                    };
                    if let Some(v) = hit {
                        values.push(v);
                        continue;
                    }
                    Some((
                        key,
                        PuritySnap {
                            messages: ctx.messages.borrow().len(),
                            draws: ctx.rand_stream.borrow().draws(),
                            closures: ctx.closures.borrow().len(),
                            impure_reads: ctx.impure_reads.get(),
                        },
                    ))
                } else {
                    // An uncacheable call (big args) still PAID the gate scan — charge it to the warmup so a
                    // mostly-uncacheable program (the `under_sink_guide` class) auto-disables.
                    if warming {
                        ctx.cache.borrow_mut().note(
                            false,
                            eval_cache::key_work(&vals, ctx.config.eval_cache_argcap),
                        );
                    }
                    None
                };
                // AD.2 runaway-recursion guard: this call is COMMITTED to interpretation (JIT and cache
                // hits `continue`d above), so it goes in flight. See [`MAX_CALL_DEPTH`] for why in-flight
                // count (not task depth — tail-collapse; not arg cycles — `add_up_to(-1)` progresses) is
                // the guard that matches upstream's "Recursion detected".
                let depth = ctx.live_calls.get() + 1;
                if depth > MAX_CALL_DEPTH {
                    return Err(crate::Error::Eval(match name {
                        Some(name) => format!("Recursion detected calling function '{name}'"),
                        None => format!(
                            "Recursion detected calling function (over {MAX_CALL_DEPTH} calls in flight)"
                        ),
                    }));
                }
                ctx.live_calls.set(depth);
                // Fires LAST of the call's completion tasks (LIFO; below CacheStore) → the decrement
                // brackets the whole call including its memoization.
                tasks.push(Task::CallReturn);
                if let Some((key, snap)) = store {
                    // Pushed BEFORE the body eval → fires (LIFO) once the result lands, like `TraceReturn`.
                    tasks.push(Task::CacheStore { key, snap });
                }
                // The call scope is lexically a child of `base` (the callee's home global — hygiene) but
                // DYNAMICALLY a child of `caller`, so it inherits the caller's reaching $-context by
                // reference (no per-call $-copy — the L.2.7 fix). A call's own $-args (bound below) land in
                // this frame's specials, shadowing the inherited ones.
                let mut call = Scope::call_frame(&base, &caller);
                // Two-phase (OpenSCAD): bind DEFAULTS first, then the passed ARGS, so an arg wins over a
                // default even when a param NAME repeats (a defaultless duplicate can't clobber a real arg).
                for ((name, &prov), value) in names.iter().zip(&provided).zip(&vals) {
                    if !prov {
                        call.bind(Rc::clone(name), value.clone());
                    }
                }
                for ((name, prov), value) in names.iter().zip(provided).zip(vals) {
                    if prov {
                        call.bind(Rc::clone(name), value);
                    }
                }
                tasks.push(Task::Eval(body, call));
            }
            Task::CallValue { args, caller } => {
                let callee = values.pop().unwrap_or(Value::Undef);
                match &callee {
                    Value::Function {
                        closure_id,
                        env,
                        self_name,
                        group,
                        bound_this,
                        ..
                    } => {
                        // AF.5: an extracted method carries its receiver — hand it to push_call so
                        // a `this` param binds it.
                        let this_receiver =
                            bound_this.as_ref().map(|o| Value::Object(Rc::clone(o)));
                        let (params, body) = ctx.closures.borrow()[*closure_id];
                        // a closure's body is lexically scoped to its captured env, not the caller's. Re-inject
                        // the letrec bindings NOW, where we hold the closure value (our COW frames can't
                        // self-reference at capture time): NAME→itself so it can recurse, AND every sibling in
                        // its GROUP (L.5.4) — each reconstructed with THIS env — so a forward/mutual body-fn
                        // call resolves. Every call re-injects, so recursion depth stays unbounded. A lone
                        // anonymous literal (no name, no group) skips the child frame entirely.
                        let needs_inject =
                            self_name.is_some() || group.as_ref().is_some_and(|g| !g.is_empty());
                        let base = if needs_inject {
                            let mut b = env.child();
                            if let Some(g) = group {
                                for s in g.iter() {
                                    b.bind(Rc::clone(&s.name), nested_fn_value(s, env, Some(g)));
                                }
                            }
                            // self LAST → the exact invoked value (its own group intact) wins over the group's
                            // reconstructed self-entry; both are call-equivalent, this just keeps identity.
                            if let Some(name) = self_name {
                                b.bind(Rc::clone(name), callee.clone());
                            }
                            b
                        } else {
                            env.clone()
                        };
                        // A closure has no static name to look up in the JIT registry → never JIT-eligible.
                        push_call(
                            params,
                            body,
                            args,
                            &caller,
                            &base,
                            None,
                            this_receiver,
                            &mut tasks,
                        );
                    }
                    _ => values.push(Value::Undef), // calling a non-function → undef
                }
            }
            Task::TraceReturn { name } => {
                if let Some(v) = values.last() {
                    trace::ret(name, v);
                }
            }
            Task::FnTimeReturn => fnprofile::exit_fn(),
            Task::CallReturn => {
                // The call's result landed — it's no longer in flight. Never touches the value stack.
                ctx.live_calls.set(ctx.live_calls.get() - 1);
            }
            Task::CacheStore { key, snap } => {
                // Peek the result (never consume — the caller reads it, like `TraceReturn`). Store ONLY if the
                // body left NO observable side effect: unchanged message log, RNG draw count, closure table,
                // and impure-read counter. An impure subtree (echo/warning, seedless `rands`, a created
                // closure, a `parent_module` read) re-runs every time instead of serving a stale result.
                if let Some(result) = values.last() {
                    let pure = ctx.messages.borrow().len() == snap.messages
                        && ctx.rand_stream.borrow().draws() == snap.draws
                        && ctx.closures.borrow().len() == snap.closures
                        && ctx.impure_reads.get() == snap.impure_reads;
                    if pure {
                        ctx.cache.borrow_mut().put(key, result.clone());
                    }
                }
            }
            Task::LetStep {
                bindings,
                idx,
                body,
                scope,
            } => {
                let binding = &bindings[idx];
                let name: Rc<str> = binding.name.clone().unwrap_or_else(|| Rc::from("_"));
                let value = name_closure(values.pop().unwrap_or(Value::Undef), &name);
                // AH.2.3: a duplicate of a name already bound in this SAME let is IGNORED (first
                // wins, upstream warns) — the RHS still evaluated above, only the bind is dropped.
                // Unnamed bindings (`_`) are exempt: they were never addressable to begin with.
                let dup = binding.name.is_some()
                    && bindings[..idx].iter().any(|b| b.name == binding.name);
                let inner = if dup {
                    ctx.messages.borrow_mut().push(Message::Warning(format!(
                        "Ignoring duplicate variable assignment \"{name}\""
                    )));
                    scope
                } else {
                    trace::bind('l', &name, &value);
                    let mut inner = scope.child();
                    inner.bind(name, value);
                    inner
                };
                if idx + 1 < bindings.len() {
                    tasks.push(Task::LetStep {
                        bindings,
                        idx: idx + 1,
                        body,
                        scope: inner.clone(),
                    });
                    tasks.push(Task::Eval(&bindings[idx + 1].value, inner));
                } else {
                    tasks.push(Task::Eval(body, inner));
                }
            }
            Task::Builtin { name, args } => run_builtin(name, args, &mut values, ctx),
            Task::Intrinsic { func, nargs } => {
                // Same shape as run_builtin: the args are the top `nargs` of the value stack. Fallible — an
                // intrinsic for a function with an inline `assert` raises exactly where the interpreted body
                // would (the `?` aborts the whole eval, same as a failed interpreted assert).
                let start = values.len().saturating_sub(nargs);
                let result = func(&values[start..])?;
                values.truncate(start);
                values.push(result);
            }
            Task::EchoEmit { args, body, scope } => {
                let vals = values.split_off(values.len().saturating_sub(args.len()));
                ctx.messages
                    .borrow_mut()
                    .push(Message::Echo(format_echo_line(args, &vals)?));
                match body {
                    Some(b) => tasks.push(Task::Eval(b, scope)),
                    None => values.push(Value::Undef),
                }
            }
            Task::AssertCheck {
                cond,
                msg,
                body,
                scope,
            } => {
                let cond_val = cond.map(|_| values.pop().unwrap_or(Value::Undef));
                assert_verdict(cond, cond_val.as_ref(), msg, &scope, &global, ctx)?;
                match body {
                    Some(b) => tasks.push(Task::Eval(b, scope)),
                    None => values.push(Value::Undef),
                }
            }
            Task::LcElem(e, scope) => match &e.kind {
                ExprKind::LcFor { bindings, body } => tasks.push(Task::LcForBindings {
                    bindings,
                    body,
                    scope,
                }),
                ExprKind::LcForC {
                    init,
                    cond,
                    update,
                    body,
                } => {
                    // Init assignments bind SEQUENTIALLY (`let`-style): a later one sees the
                    // earlier ones, so `for(a=1, b=a+1; …)` gives `b==2`.
                    let mut vars: Vec<(String, Value)> = Vec::new();
                    let mut init_scope = scope.child();
                    for arg in init {
                        let name = arg.name.as_deref().unwrap_or("_").to_string();
                        let value = eval_with_global(&arg.value, &init_scope, &global, ctx)?;
                        init_scope.bind(name.clone(), value.clone());
                        vars.push((name, value));
                    }
                    let splice_item = is_comprehension(body);
                    tasks.push(Task::LcForCStep {
                        cond,
                        update,
                        body,
                        outer: scope,
                        vars,
                        iterations: 0,
                        acc: Vec::new(),
                        splice_item,
                    });
                }
                // `each E` splices ONE level; `E` is itself an element (`each if(c) X` distributes
                // the splice INTO the guard/loop).
                ExprKind::LcEach(inner) => {
                    tasks.push(Task::LcEachSplice {
                        splice_inner: is_comprehension(inner),
                    });
                    tasks.push(Task::LcElem(inner, scope));
                }
                ExprKind::LcIf { cond, then, els } => {
                    tasks.push(Task::LcIfBranch {
                        then,
                        els: els.as_deref(),
                        scope: scope.clone(),
                    });
                    tasks.push(Task::Eval(cond, scope));
                }
                // A `let` element is TRANSPARENT (splices iff its body does — is_comprehension
                // follows the body), so the body's own result shape is exactly right.
                ExprKind::Let { bindings, body } => {
                    let inner = comprehension_let_scope(bindings, &scope, &global, ctx)?;
                    tasks.push(Task::LcElem(body, inner));
                }
                // A plain element — its raw value (N.2: no wrapper).
                _ => tasks.push(Task::Eval(e, scope)),
            },
            Task::WrapContribution => {
                let v = values.pop().unwrap_or(Value::Undef);
                values.push(build_vector(vec![v]));
            }
            Task::LcIfBranch { then, els, scope } => {
                let cond = values.pop().unwrap_or(Value::Undef);
                let taken = if cond.is_truthy() { Some(then) } else { els };
                match taken {
                    Some(branch) if is_comprehension(branch) => {
                        tasks.push(Task::LcElem(branch, scope));
                    }
                    Some(branch) => {
                        // A plain branch's value wraps into a one-element contribution.
                        tasks.push(Task::WrapContribution);
                        tasks.push(Task::Eval(branch, scope));
                    }
                    None => values.push(build_vector(Vec::new())),
                }
            }
            Task::LcEachSplice { splice_inner } => {
                let inner = values.pop().unwrap_or(Value::Undef);
                let contributions: Vec<Value> = if splice_inner {
                    iter_values(&inner, ctx)
                } else {
                    vec![inner]
                };
                let mut out = Vec::new();
                for contribution in contributions {
                    // Q.5: `each <range/list>` splices bulk elements with NO per-element eval —
                    // charge the splice count so a giant `each [0:9e9]` stays bounded.
                    ctx.charge_iterable(&contribution)?;
                    out.extend(iter_values(&contribution, ctx));
                }
                values.push(build_vector(out));
            }
            Task::LcForBindings {
                bindings,
                body,
                scope,
            } => match bindings.split_first() {
                None => {
                    // `for()` — the body's contribution IS the for's (wrap a plain body's value).
                    if is_comprehension(body) {
                        tasks.push(Task::LcElem(body, scope));
                    } else {
                        tasks.push(Task::WrapContribution);
                        tasks.push(Task::Eval(body, scope));
                    }
                }
                Some((binding, rest)) => {
                    // The loop var as an `Rc<str>` computed ONCE per binding level (N.2b).
                    let var: Rc<str> = binding.name.clone().unwrap_or_else(|| Rc::from("_"));
                    let iterable = eval_with_global(&binding.value, &scope, &global, ctx)?;
                    // Q.5: charge BEFORE `iter_values` materializes the range.
                    ctx.charge_iterable(&iterable)?;
                    let splice_item = !rest.is_empty() || is_comprehension(body);
                    // ONE child frame, REUSED across iterations (N.2): `bind` is `Rc::make_mut` —
                    // in-place while uniquely held, cloned the instant an iteration's work (or a
                    // captured closure) still holds it. Bit-identical to a fresh frame per item.
                    let frame = scope.child();
                    tasks.push(Task::LcForNext {
                        rest,
                        body,
                        var,
                        items: iter_values(&iterable, ctx).into_iter(),
                        frame,
                        acc: Vec::new(),
                        splice_item,
                        pending: false,
                    });
                }
            },
            Task::LcForNext {
                rest,
                body,
                var,
                mut items,
                mut frame,
                mut acc,
                splice_item,
                pending,
            } => {
                if pending {
                    let result = values.pop().unwrap_or(Value::Undef);
                    if splice_item {
                        acc.extend(iter_values(&result, ctx));
                    } else {
                        acc.push(result);
                    }
                }
                match items.next() {
                    Some(value) => {
                        frame.bind(Rc::clone(&var), value);
                        let iter_scope = frame.clone();
                        // Self BELOW the item's work: the item runs first, then the next iteration.
                        tasks.push(Task::LcForNext {
                            rest,
                            body,
                            var,
                            items,
                            frame,
                            acc,
                            splice_item,
                            pending: true,
                        });
                        if rest.is_empty() {
                            if is_comprehension(body) {
                                tasks.push(Task::LcElem(body, iter_scope));
                            } else {
                                tasks.push(Task::Eval(body, iter_scope));
                            }
                        } else {
                            tasks.push(Task::LcForBindings {
                                bindings: rest,
                                body,
                                scope: iter_scope,
                            });
                        }
                    }
                    None => values.push(build_vector(acc)),
                }
            }
            Task::LcForCStep {
                cond,
                update,
                body,
                outer,
                vars,
                iterations,
                acc,
                splice_item,
            } => {
                let mut loop_scope = outer.child();
                for (name, value) in &vars {
                    loop_scope.bind(name.clone(), value.clone());
                }
                if eval_with_global(cond, &loop_scope, &global, ctx)?.is_truthy() {
                    tasks.push(Task::LcForCUpdate {
                        cond,
                        update,
                        body,
                        outer,
                        loop_scope: loop_scope.clone(),
                        vars,
                        iterations,
                        acc,
                        splice_item,
                    });
                    if splice_item {
                        tasks.push(Task::LcElem(body, loop_scope));
                    } else {
                        tasks.push(Task::Eval(body, loop_scope));
                    }
                } else {
                    values.push(build_vector(acc));
                }
            }
            Task::LcForCUpdate {
                cond,
                update,
                body,
                outer,
                mut loop_scope,
                mut vars,
                iterations,
                mut acc,
                splice_item,
            } => {
                let result = values.pop().unwrap_or(Value::Undef);
                if splice_item {
                    acc.extend(iter_values(&result, ctx));
                } else {
                    acc.push(result);
                }
                // Update assignments bind SEQUENTIALLY within the clause: `x=i*10, y=x+1` lets `y`
                // see the NEW `x` (OpenSCAD-verified; BOSL2's `_dp_distance_row` DP relies on it).
                for arg in update {
                    let name = arg.name.as_deref().unwrap_or("_");
                    let value = eval_with_global(&arg.value, &loop_scope, &global, ctx)?;
                    loop_scope.bind(name.to_string(), value.clone());
                    match vars.iter_mut().find(|(n, _)| n == name) {
                        Some(entry) => entry.1 = value,
                        None => vars.push((name.to_string(), value)),
                    }
                }
                let iterations = iterations + 1;
                if iterations > MAX_CFOR_ITERATIONS {
                    // AD.4: upstream's hard limit, verbatim verdict (boundary probed: 1e6 ok, 1e6+1 errors).
                    return Err(crate::Error::Eval(
                        "For loop counter exceeded limit".to_string(),
                    ));
                }
                tasks.push(Task::LcForCStep {
                    cond,
                    update,
                    body,
                    outer,
                    vars,
                    iterations,
                    acc,
                    splice_item,
                });
            }
            Task::PushUndef => values.push(Value::Undef),
            Task::ShortCircuit { op, rhs, scope } => {
                let lhs = values.pop().unwrap_or(Value::Undef);
                let or = matches!(op, BinOp::Or);
                if lhs.is_truthy() == or {
                    values.push(Value::Bool(or)); // `||` on truthy → true; `&&` on falsy → false
                } else {
                    // Not short-circuited: evaluate the RHS and combine it with the kept LHS.
                    values.push(lhs);
                    tasks.push(Task::Binary(op));
                    tasks.push(Task::Eval(rhs, scope));
                }
            }
        }
    }
    Ok(values.pop().unwrap_or(Value::Undef))
}

/// Dispatch one AST node: leaves push a value directly; composites push their sub-tasks (children
/// first, so they evaluate before the combining task).
#[allow(
    clippy::too_many_lines,
    reason = "the expression-node dispatch: one match arm per ExprKind — a cohesive jump table, not \
    separable without threading the tasks stack through every helper"
)]
fn eval_node<'a>(
    e: &'a Expr,
    scope: &Scope,
    ctx: &Ctx<'a>,
    tasks: &mut Vec<Task<'a>>,
    values: &mut Vec<Value>,
) -> crate::Result<()> {
    match &e.kind {
        ExprKind::Num(n) => values.push(Value::Num(*n)),
        ExprKind::Bool(b) => values.push(Value::Bool(*b)),
        ExprKind::Undef => values.push(Value::Undef),
        ExprKind::Str(s) => values.push(Value::string(s.as_str())),
        ExprKind::Ident(name) => values.push(resolve_ident(name, scope, ctx)),
        ExprKind::Unary { op, operand } => {
            tasks.push(Task::Unary(*op));
            tasks.push(Task::Eval(operand, scope.clone()));
        }
        // `&&` / `||` SHORT-CIRCUIT (OpenSCAD semantics): evaluate the LHS, then a `ShortCircuit` task
        // decides whether the RHS runs at all — so a guarded assert or recursion behind it stays guarded.
        ExprKind::Binary {
            op: op @ (BinOp::And | BinOp::Or),
            lhs,
            rhs,
        } => {
            tasks.push(Task::ShortCircuit {
                op: *op,
                rhs,
                scope: scope.clone(),
            });
            tasks.push(Task::Eval(lhs, scope.clone()));
        }
        ExprKind::Binary { op, lhs, rhs } => {
            tasks.push(Task::Binary(*op));
            tasks.push(Task::Eval(rhs, scope.clone()));
            tasks.push(Task::Eval(lhs, scope.clone())); // popped (and evaluated) first
        }
        ExprKind::Ternary { cond, then, els } => {
            tasks.push(Task::Ternary {
                then,
                els,
                scope: scope.clone(),
            });
            tasks.push(Task::Eval(cond, scope.clone()));
        }
        ExprKind::Vector(elems) => {
            tasks.push(Task::VectorSplice(elems));
            for el in elems.iter().rev() {
                tasks.push(Task::Eval(el, scope.clone())); // reversed pushes → forward eval order
            }
        }
        ExprKind::Call { callee, args } => dispatch_call(callee, args, scope, ctx, tasks)?,
        ExprKind::Index { base, index } => {
            tasks.push(Task::Index);
            tasks.push(Task::Eval(index, scope.clone()));
            tasks.push(Task::Eval(base, scope.clone())); // evaluated first → base under index
        }
        ExprKind::Member { base, field } => {
            tasks.push(Task::Member(field));
            tasks.push(Task::Eval(base, scope.clone())); // base evaluated first, then `.field`
        }
        ExprKind::Range { start, step, end } => {
            // pushed so start evaluates first (bottom of the value stack), end last (top).
            tasks.push(Task::Range {
                stepped: step.is_some(),
            });
            tasks.push(Task::Eval(end, scope.clone()));
            if let Some(step) = step {
                tasks.push(Task::Eval(step, scope.clone()));
            }
            tasks.push(Task::Eval(start, scope.clone()));
        }
        ExprKind::FunctionLiteral { params, body } => {
            // register the literal's &'a params + body in the closure table; the VALUE holds just the
            // index + the captured env, so it stays 'static.
            let closure_id = {
                let mut closures = ctx.closures.borrow_mut();
                closures.push((params.as_slice(), body.as_ref()));
                closures.len() - 1
            };
            values.push(Value::Function {
                closure_id,
                env: scope.clone(),
                self_name: None, // set when bound to a name (`g = function…`) — see `name_closure`
                // OpenSCAD's `str()` rendering, computed here where the AST is in hand (str can't reach it).
                repr: crate::parser::print::function_value_repr(params, body).into(),
                group: None,      // a plain literal has no letrec siblings (L.5.4)
                bound_this: None, // binding happens at member EXTRACTION (AF.5)
            });
        }
        ExprKind::Let { bindings, body } => {
            if bindings.is_empty() {
                tasks.push(Task::Eval(body, scope.clone())); // `let() body` → just the body
            } else {
                tasks.push(Task::LetStep {
                    bindings,
                    idx: 0,
                    body,
                    scope: scope.clone(),
                });
                tasks.push(Task::Eval(&bindings[0].value, scope.clone()));
            }
        }
        ExprKind::Echo { args, body } => {
            // `echo(args) body?` — args evaluate as TASKS (left-to-right), then `EchoEmit` pops
            // them, prints, and schedules the body. Fully on the machine (AB.2): chains and
            // echo-bodied recursion cost heap, never host stack.
            tasks.push(Task::EchoEmit {
                args,
                body: body.as_deref(),
                scope: scope.clone(),
            });
            for arg in args.iter().rev() {
                tasks.push(Task::Eval(&arg.value, scope.clone()));
            }
        }
        ExprKind::Assert { args, body } => {
            // `assert(cond, msg?) body?` — the condition evaluates as a TASK, then `AssertCheck`
            // verdicts and schedules the body (AB.2: assert-chained recursion cost heap, not host
            // stack — the tail-recursion-tests crasher).
            let (cond, msg) = split_assert_args(args);
            tasks.push(Task::AssertCheck {
                cond,
                msg,
                body: body.as_deref(),
                scope: scope.clone(),
            });
            if let Some(c) = cond {
                tasks.push(Task::Eval(c, scope.clone()));
            }
        }
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => {
            // a comprehension element evaluates to its CONTRIBUTION list (spliced by the enclosing
            // VectorSplice). Only reached as a vector element (parser invariant). Scheduled ON the
            // main machine (AB.3): generator nesting AND vector/generator ALTERNATION cost heap,
            // never host stack — the old eval_comprehension↔eval_node alternation was one host
            // frame per nesting level, a cliff once the AA.4 spine made deep elements parseable.
            tasks.push(Task::LcElem(e, scope.clone()));
        }
    }
    Ok(())
}

/// Pop a builtin call's argument values, split them into positional/named, and push the builtin result.
fn run_builtin(name: &str, args: &[Arg], values: &mut Vec<Value>, ctx: &Ctx<'_>) {
    // A benchmark span per builtin application (I.6); `builtin` field lets a layer break cost down by
    // name. All the tracing spans sit at TRACE level — the "compile-out-like-a-logger" doctrine.
    let _span = tracing::trace_span!("builtin", builtin = name).entered();
    // OpenSCAD builtins declare NO parameter names: every argument — named or positional — is read by
    // SOURCE POSITION and any name ignored (`func.cc` reads `arguments[i].value`, never `.name`). BOSL2's
    // `search([v], list, num_returns_per_match=1, index_col_num=idx)` works ONLY because those trailing
    // names sit at positions 2 and 3. The evaluated values are already on the value stack in source order,
    // so we BORROW them in place as the positional slice — no `split_off` of a throwaway Vec per call. A
    // builtin call is the interpreter's hottest event (is_num/is_undef/len run into the millions on BOSL2),
    // and the split-off 1-element Vec was a per-call heap alloc for nothing (N.2a). We read the slice, then
    // truncate the stack back and push the result. (Splitting the NAMED args off — as an even-older cut
    // did — dropped them entirely, silently defaulting `search`'s `index_col_num` to 0; we keep all of them.)
    let start = values.len().saturating_sub(args.len());
    // `rands` is the one STATEFUL builtin: seedless draws advance the evaluator's `rand_stream` (I.2.8b),
    // so it's routed here where the `Ctx` is in scope rather than through the pure `builtins::apply`.
    let result = if name == "textmetrics" || name == "fontmetrics" {
        // AG: the metrics builtins have DECLARED named parameters upstream (unlike every other
        // builtin) — routed here where the `Arg` names are in hand, like `object`.
        builtins::metrics_call(name, args, &values[start..], &mut ctx.messages.borrow_mut())
    } else if name == "object" {
        // `object()` is the ONE builtin that reads argument NAMES (AF.4): `object(a=1, b=2)`'s
        // member names ARE the names. Routed here where the `Arg` list is in hand.
        builtins::object(args, &values[start..])
    } else if name == "rands" {
        builtins::rands(&values[start..], &mut ctx.rand_stream.borrow_mut())
    } else if name == "parent_module" {
        // Reads the live module-instantiation name stack (control.cc) — stateful, like `rands`. This read
        // depends on the module-call context, which the eval-memo cache key does NOT capture, so mark the
        // subtree impure: the fence then declines to memoize any call that (transitively) reads it (N.2c).
        ctx.impure_reads.set(ctx.impure_reads.get() + 1);
        builtins::parent_module(&values[start..], &ctx.module_stack.borrow())
    } else {
        builtins::apply(name, &values[start..])
    };
    trace::builtin(name, &values[start..], &result); // gated inside; shows `name(args) => result`
    values.truncate(start);
    values.push(result);
}

/// Resolve a bare identifier to its value, WARNING on a genuinely-unknown name — OpenSCAD's "Ignoring
/// unknown variable" (`Expression.cc` `Lookup::evaluate`). A `$`-special stays SILENT when unbound: it's
/// dynamically scoped, so absence is normal (BOSL2 reads many optional `$`-vars). An explicit `x = undef`
/// (or an unfilled defaultless param) is BOUND, so it doesn't warn either. The value is `undef` in every
/// unbound case. NOTE: OpenSCAD also appends `in file …, line …` — deferred with source-position
/// threading; the warning CONTENT matches, the location suffix doesn't yet (flagged for the K oracle).
fn resolve_ident(name: &str, scope: &Scope, ctx: &Ctx<'_>) -> Value {
    if let Some(value) = scope.lookup_opt(name) {
        return value;
    }
    if name.starts_with('$') {
        // OpenSCAD is silent on unbound `$`-specials; WE trace them — a `$`-var that hits nothing may be
        // one we haven't implemented, and the trace is how that surfaces during bring-up.
        trace::unbound_special(name);
    } else {
        trace::unbound_var(name); // dev trace: surface WHERE a value went undef (root, not the distant assert)
        ctx.messages.borrow_mut().push(Message::Warning(format!(
            "Ignoring unknown variable '{name}'"
        )));
    }
    Value::Undef
}

/// Dispatch a call `callee(args)`: a NAMED user function (own namespace) resolves first; an UNBOUND
/// identifier callee is a builtin or genuinely unknown → warn-and-`undef` (L.5.7, OpenSCAD-faithful);
/// otherwise the callee is a value — evaluate it and apply it (a closure in a variable, or `(expr)(args)`).
#[allow(
    clippy::unnecessary_wraps,
    reason = "shares the fallible crate::Result<()> dispatcher contract with the eval_node call paths; \
              regains an Err path when arity/charge checks land — kept for signature symmetry"
)]
fn dispatch_call<'a>(
    callee: &'a Expr,
    args: &'a [Arg],
    scope: &Scope,
    ctx: &Ctx<'a>,
    tasks: &mut Vec<Task<'a>>,
) -> crate::Result<()> {
    if let ExprKind::Ident(name) = &callee.kind {
        // AD.1 (oracle-pinned): an INNER-scope binding holding a function value shadows a named
        // function in call position — let-bound (p3), module-local (p6/p9) and PARAMETER (p11)
        // closures all win; top-level variable closures do NOT (p1/p2 — global frames are excluded
        // by `lookup_local_function`), and a non-function local doesn't shadow either (p8). Routing
        // through the dynamic-callee task keeps one closure-call path.
        if let Some(f) = scope.lookup_local_function(name) {
            tasks.push(Task::CallValue {
                args,
                caller: scope.clone(),
            });
            tasks.push(Task::Const(f));
            return Ok(());
        }
        // resolution order (OpenSCAD): a user function may shadow a builtin.
        if let Some(&((params, body), home)) = ctx.functions.get(name.as_str()) {
            // O.1: a registered intrinsic replaces the interpreted body — but ONLY for an all-positional
            // call (v1 ABI: the native fn takes a flat positional slice, so a named-arg call falls through
            // to the interpreter below rather than needing post-eval rebinding). The fingerprint match that
            // authorized this intrinsic happened once at `build_intrinsics`; here it's a name lookup.
            if let Some(&func) = ctx.intrinsics.get(name.as_str()) {
                if args.iter().all(|a| a.name.is_none()) {
                    fnprofile::record_intrinsic(name.as_str()); // dev probe: the already-native side of the worklist
                    // EXTRA positional args (beyond arity) are dropped UNEVALUATED, like `push_call`'s
                    // slot filling — evaluating them here would fire side effects (echo, seedless rands)
                    // the interpreter never runs.
                    let nargs = args.len().min(params.len());
                    tasks.push(Task::Intrinsic { func, nargs });
                    for arg in args[..nargs].iter().rev() {
                        tasks.push(Task::Eval(&arg.value, scope.clone()));
                    }
                    return Ok(());
                }
                // O.6: NAMED-ARG rebind — BOSL2 calls the hot predicates with named args (`is_vector(v,
                // zero=true)`, `unit(v, error=…)`), which the v1 all-positional gate sent to the
                // interpreter. [`fill_arg_slots`] mirrors `push_call` exactly (incl. the AH.2.4
                // lowest-unfilled positional rule); a hole evaluates the param's real DEFAULT expr
                // in the definition `base` (bit-identical to the baked value — the const guard
                // proved it) or pushes undef for a defaultless slot. Value-sources evaluate in
                // PARAM order, `push_call`'s own order. Any INJECTION — a `$`-arg or an unknown
                // named arg — declines to the interpreter: both must evaluate AND bind into a real
                // call scope, which the flat ABI can't honor.
                let (arg_slots, injections) = fill_arg_slots(params, args);
                if injections.is_empty() {
                    fnprofile::record_intrinsic(name.as_str());
                    let base = ctx.island_globals.borrow()[home].clone();
                    tasks.push(Task::Intrinsic {
                        func,
                        nargs: params.len(),
                    });
                    for (slot, param) in arg_slots.into_iter().zip(params).rev() {
                        match (slot, &param.default) {
                            (Some(expr), _) => tasks.push(Task::Eval(expr, scope.clone())),
                            (None, Some(default)) => {
                                tasks.push(Task::Eval(default, base.clone()));
                            }
                            (None, None) => tasks.push(Task::PushUndef),
                        }
                    }
                    return Ok(());
                }
                // a `$`-arg call falls through to the interpreted path below
            }
            // A call-path EVENT, not a span: the call's body evaluates across later loop iterations on
            // the explicit stack (no host recursion), so its subtree isn't scope-bounded here — the
            // event marks WHICH function was entered, the enclosing `eval_program` span times the whole.
            tracing::trace!(function = name.as_str(), "call");
            fnprofile::record_fn(name.as_str()); // dev probe (FAB_PROFILE_FNS): per-name call counts
            if fnprofile::enter_fn(name.as_str()) {
                // Books self + outermost-inclusive time when the return lands (LIFO, like TraceReturn).
                // The window opens HERE — before the arg tasks push — so arg eval books to the callee.
                tasks.push(Task::FnTimeReturn);
            }
            if trace::on() {
                tasks.push(Task::TraceReturn { name }); // fires when the body's value lands (peek-only)
            }
            // The body's lexical base is the function's HOME ISLAND global (its own file's constants), NOT
            // the caller's `global` — the use-scope fix. For a root-defined function home is 0 (the root
            // global), so this is a no-op there; for a `use`d function it swaps in the library's constants.
            let base = ctx.island_globals.borrow()[home].clone();
            // The name rides into `Task::Apply` unconditionally (AD.2 wants it for the recursion
            // verdict); the JIT offer there re-gates on `ctx.jit` being present, so this stays exactly
            // the P.1.4-recut eligibility — named args included (task #66): the hook fires on `vals`,
            // the FINAL param-order slot values `push_call` bound, so how the caller SPELLED the args is
            // invisible to the compiled body. The one real arity hazard — `$`-args appending to `names`
            // past the params — is `push_call`'s own gate (it clears the name when dollars exist).
            push_call(
                params,
                body,
                args,
                scope,
                &base,
                Some(name.as_str()),
                None,
                tasks,
            );
            return Ok(());
        }
        if builtins::is_builtin(name) {
            // (no TraceReturn — `run_builtin` traces the builtin's args + result inline)
            fnprofile::record_builtin(name); // dev probe (FAB_PROFILE_FNS): per-name call counts
            tasks.push(Task::Builtin { name, args });
            for arg in args.iter().rev() {
                tasks.push(Task::Eval(&arg.value, scope.clone()));
            }
            return Ok(());
        }
        if matches!(scope.lookup(name), Value::Undef) {
            // not a user fn, not a builtin, not a bound function-value → a missing builtin or a typo. WARN
            // and evaluate to `undef`, exactly as OpenSCAD does ("Ignoring unknown function 'name'",
            // `Expression.cc` FunctionCall::evaluate) — faithful-to-oracle so a corpus that names a
            // newer-BOSL2 function still renders the REST instead of hard-failing (L.5.7). The named symbol
            // still surfaces in the console log for the evaluator-gap worklist.
            ctx.warn(format!("Ignoring unknown function '{name}'"));
            tasks.push(Task::Const(Value::Undef));
            return Ok(());
        }
    }
    tasks.push(Task::CallValue {
        args,
        caller: scope.clone(),
    });
    tasks.push(Task::Eval(callee, scope.clone()));
    Ok(())
}

/// Push the tasks for a function call (a named user function OR a closure): one value-source per
/// parameter — an arg expr (in the CALLER scope), a default (in the lexical `base` scope), or `undef` —
/// then an [`Task::Apply`] that binds them and evaluates the body. `base` is the lexical base of the
/// body: the top-level `global` for a named function, the captured `env` for a closure. OpenSCAD
/// arg-matching: positional args fill params left-to-right, named args fill by name (extra/unknown args
/// are dropped). Two documented first-cut simplifications: `$`-arg injection is I.2.2, and defaults
/// evaluate in the definition scope, not the partially-bound call scope (so a default can't reference
/// an earlier param — rare; defaults are usually constants).
/// Which explicit-arg expr fills each param slot — `None` = the slot falls to its default/undef.
type ArgSlots<'a> = Vec<Option<&'a Expr>>;
/// Args that bind into the call scope OUTSIDE the param slots: `$`-args (per-call dynamic
/// `$`-var injections) and named args matching NO declared param — upstream binds those as plain
/// call-scope variables anyway ("variable not specified as parameter" + usable; AH.2.5, the
/// variable-scope-tests golden's `undeclared_var(d=6)`). `Scope::bind` routes each by prefix.
type DollarArgs<'a> = Vec<(Rc<str>, &'a Expr)>;

/// Fill each param slot from the call args, LEFT TO RIGHT (AH.2.4, the arg-permutations golden):
/// a named arg takes its slot (a later duplicate overwrites), a `$`-arg splits out as a dynamic
/// injection, and a POSITIONAL fills the LOWEST currently-unfilled slot — `f(a=1,2)` puts `2` in
/// `b`, and `f(a=1,3,b=2)` lands `3` on `b` only for `b=2` to overwrite it. A positional with no
/// unfilled slot left is dropped (upstream warns "too many arguments").
fn fill_arg_slots<'a>(params: &'a [Parameter], args: &'a [Arg]) -> (ArgSlots<'a>, DollarArgs<'a>) {
    let mut arg_slots: Vec<Option<&'a Expr>> = vec![None; params.len()];
    let mut dollars: Vec<(Rc<str>, &'a Expr)> = Vec::new();
    for arg in args {
        match &arg.name {
            None => {
                if let Some(slot) = arg_slots.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(&arg.value);
                }
            }
            // a $-arg is a per-call dynamic override — injected into the call scope, not param-matched.
            Some(name) if name.starts_with('$') => dollars.push((Rc::clone(name), &arg.value)),
            Some(name) => {
                if let Some(i) = params.iter().position(|p| p.name == *name) {
                    arg_slots[i] = Some(&arg.value);
                } else {
                    // No such param — still binds as a call-scope variable (upstream warns + uses).
                    dollars.push((Rc::clone(name), &arg.value));
                }
            }
        }
    }
    (arg_slots, dollars)
}

#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "the call context, one slot per concern — the AF.5 receiver made it seven; the \
              receiver is cloned per this-slot schedule, an owned Option is the honest shape"
)]
fn push_call<'a>(
    params: &'a [Parameter],
    body: &'a Expr,
    args: &'a [Arg],
    caller: &Scope,
    base: &Scope,
    name: Option<&'a str>,
    this_value: Option<Value>,
    tasks: &mut Vec<Task<'a>>,
) {
    // Which explicit-arg expr fills each param slot ([`fill_arg_slots`]). `None` = the param takes
    // its default / undef. Kept separate from defaults so a DUPLICATE param name binds
    // arg-over-default in the two-phase `Task::Apply` (an unfilled second slot can't clobber a real arg).
    let (arg_slots, dollars) = fill_arg_slots(params, args);
    // AF.5: a bound method's receiver fills a param NAMED `this` — iff declared and not
    // explicitly passed (the opt-in mechanic; an explicit arg wins, a this-less fn never sees it).
    let this_idx = this_value.as_ref().and_then(|_| {
        params
            .iter()
            .position(|p| &*p.name == "this")
            .filter(|&i| arg_slots[i].is_none())
    });
    // bind order: params first, then $-args (bound last → they override the inherited $-context). A param
    // filled by an arg is `provided`; a param on its default (or a defaultless-unfilled undef) is not.
    // `$`-args are always provided. `Task::Apply` binds the non-provided (defaults) before the provided.
    // Names are `Rc<str>` cloned from the AST (a refcount bump) so the per-call bind never allocates (N.2b).
    let mut names: Vec<Rc<str>> = params.iter().map(|p| Rc::clone(&p.name)).collect();
    names.extend(dollars.iter().map(|(name, _)| Rc::clone(name)));
    let mut provided: Vec<bool> = arg_slots.iter().map(Option::is_some).collect();
    if let Some(i) = this_idx {
        provided[i] = true; // the injected receiver counts as a passed arg
    }
    provided.extend(std::iter::repeat_n(true, dollars.len()));
    // A `$`-arg appends names beyond the params, so `names.len()` would no longer equal the compiled
    // function's arity — clear the name (the JIT-offer key) in that (rare) case so an eligible call is
    // only ever a param-shaped one. The caller already passes `None` for closures; this guards dollars.
    let name = if dollars.is_empty() { name } else { None };
    tasks.push(Task::Apply {
        names,
        provided,
        body,
        base: base.clone(),
        caller: caller.clone(),
        name,
    });
    // push evals so the popped run is [params.., dollars..]: dollars first (deeper → on top), then
    // params reversed (param 0 evaluates first, lands at the bottom of the run). An arg evaluates in the
    // CALLER scope; a default in the function's lexical `base`; an unfilled defaultless slot → undef.
    for (_, expr) in dollars.iter().rev() {
        tasks.push(Task::Eval(expr, caller.clone()));
    }
    for (i, (slot, param)) in arg_slots.into_iter().zip(params).enumerate().rev() {
        if Some(i) == this_idx {
            // the receiver is already a VALUE — no expr to evaluate.
            tasks.push(Task::Const(this_value.clone().unwrap_or(Value::Undef)));
            continue;
        }
        match (slot, &param.default) {
            (Some(expr), _) => tasks.push(Task::Eval(expr, caller.clone())),
            (None, Some(default)) => tasks.push(Task::Eval(default, base.clone())),
            (None, None) => tasks.push(Task::PushUndef),
        }
    }
}

/// Is this expression a list-comprehension element (spliced into the enclosing vector) rather than a
/// plain element (appended as one)? `let` in a vector is a comprehension-`let`.
fn is_comprehension(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => true,
        // A `let` in a vector is TRANSPARENT: it splices IFF its body does. `[let(x=…) [a,b]]`
        // contributes the vector as ONE element (`[[a,b]]`), while `[let(x=…) each L]` splices — OpenSCAD-
        // verified. Unlike `if`/`for`/`each` (which route through `eval_comprehension`, adding a wrapper
        // `splice_into` then removes), a bare `let` evaluates its body DIRECTLY, so the splice decision
        // has to follow the body, not the `let` node. Without this, `(let(i) [pt])` in a path builder (e.g.
        // BOSL2's trapezoid corners) unwrapped its single-point list and flattened the whole path.
        ExprKind::Let { .. } => {
            // Loop, not recursion — deep let-chains are parseable post-AA.4.
            let mut cur = e;
            loop {
                match &cur.kind {
                    ExprKind::Let { body, .. } => cur = body,
                    ExprKind::LcFor { .. }
                    | ExprKind::LcForC { .. }
                    | ExprKind::LcEach(_)
                    | ExprKind::LcIf { .. } => return true,
                    _ => return false,
                }
            }
        }
        _ => false,
    }
}

/// Splice a comprehension element's value into the vector accumulator: a list contributes its
/// elements; a scalar (e.g. `each 5`) contributes itself.
fn splice_into(val: Value, out: &mut Vec<Value>) {
    match val {
        Value::NumList(xs) => out.extend(xs.iter().map(|&x| Value::Num(x))),
        Value::List(xs) => out.extend(xs.iter().cloned()),
        other => out.push(other),
    }
}

/// The values a `for`/`each` iterable yields: a list's elements, a range's values (capped by
/// `range_iter`), a string's characters, or a scalar as a single value. A range past
/// [`RANGE_TOO_MANY`] warns and yields NOTHING (AD.3 — upstream's warn-and-skip).
fn iter_values(v: &Value, ctx: &Ctx) -> Vec<Value> {
    match v {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        Value::Range { start, step, end } => range_values(*start, *step, *end, ctx),
        Value::Str(s) => s.chars().map(|c| Value::string(c.to_string())).collect(),
        // An OBJECT iterates its KEYS in insertion order (AF.4, the object-tests golden's
        // `[for (i = o1) i]`).
        Value::Object(o) => o.keys().map(|k| Value::Str(Rc::clone(k))).collect(),
        other => vec![other.clone()],
    }
}

/// [`iter_values`] minus the too-many-elements warning — the builtins' (`chr`/`lookup`/`search`) and
/// intrinsics' expansion seam. Those aren't "for statement" contexts upstream, so they keep the plain
/// `RANGE_MAX`-capped expansion; only the for/each seams get AD.3's warn-and-skip.
fn iter_values_raw(v: &Value) -> Vec<Value> {
    match v {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        Value::Range { start, step, end } => {
            range_iter(*start, *step, *end).map(Value::Num).collect()
        }
        Value::Str(s) => s.chars().map(|c| Value::string(c.to_string())).collect(),
        other => vec![other.clone()],
    }
}

/// Expand a range for iteration, or warn + empty when its count overflows uint32 (upstream's
/// "too many elements" verdict — see [`RANGE_TOO_MANY`]). The one range→values seam both
/// [`iter_values`] and [`iterate_values`] share.
fn range_values(start: f64, step: f64, end: f64, ctx: &Ctx) -> Vec<Value> {
    let len = range_len(start, step, end);
    if len >= RANGE_TOO_MANY {
        ctx.warn(format!(
            "Bad range parameter in for statement: too many elements ({len})"
        ));
        return Vec::new();
    }
    range_iter(start, step, end).map(Value::Num).collect()
}

/// Bind a comprehension `let`'s bindings SEQUENTIALLY (a later one sees the earlier), returning the
/// extended scope in which the `let` body's contribution is then evaluated.
fn comprehension_let_scope<'a>(
    bindings: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Scope> {
    let mut s = scope.clone();
    for (i, binding) in bindings.iter().enumerate() {
        let name: Rc<str> = binding.name.clone().unwrap_or_else(|| Rc::from("_"));
        let value = name_closure(eval_with_global(&binding.value, &s, global, ctx)?, &name);
        // AH.2.3: same first-wins duplicate rule as [`Task::LetStep`] — RHS evaluated, bind dropped.
        if binding.name.is_some() && bindings[..i].iter().any(|b| b.name == binding.name) {
            ctx.messages.borrow_mut().push(Message::Warning(format!(
                "Ignoring duplicate variable assignment \"{name}\""
            )));
            continue;
        }
        let mut next = s.child();
        next.bind(name, value);
        s = next;
    }
    Ok(s)
}

/// Build a vector value: the all-numeric `NumList` fast path when every element is a number, else the
/// general heterogeneous `List`. The two compare EQUAL element-for-element (see `Value`'s `PartialEq`).
/// Tag a function literal with the NAME it's being bound to (`g = function…` / `let(g = function…)`), so it
/// can call ITSELF by that name (letrec — the [`Task::CallValue`] injection uses it). Only tags an as-yet
/// unnamed `Function`, preserving the ORIGINAL definition name if the same closure value is re-bound
/// elsewhere. Non-functions pass through untouched.
fn name_closure(value: Value, name: &str) -> Value {
    match value {
        Value::Function {
            closure_id,
            env,
            self_name: None,
            repr,
            group,
            bound_this,
        } => Value::Function {
            closure_id,
            env,
            self_name: Some(std::rc::Rc::from(name)),
            repr,
            group,
            bound_this,
        },
        other => other,
    }
}

fn build_vector(items: Vec<Value>) -> Value {
    match items.iter().map(as_num).collect::<Option<Vec<f64>>>() {
        Some(nums) => Value::num_list(nums),
        None => Value::list(items),
    }
}

/// A number's `f64`, else `None` — the all-numeric test for the `NumList` fast path.
fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        _ => None,
    }
}

/// Build a range value from its (already-evaluated) bounds — non-numeric bounds make the whole range
/// `undef` (OpenSCAD requires numeric range bounds).
fn build_range(start: &Value, step: &Value, end: &Value) -> Value {
    match (start, step, end) {
        (&Value::Num(start), &Value::Num(step), &Value::Num(end)) => {
            Value::Range { start, step, end }
        }
        _ => Value::Undef,
    }
}

/// Evaluate a whole program to a [`Mesh`] — the tracer-bullet spine's tail. Assignments bind into
/// the scope; a single top-level object produces its mesh.
///
/// # Errors
/// Deferred constructs fail LOUD: unknown modules / transforms / booleans (module eval), and
/// multiple top-level objects (implicit union — J.2).
pub fn eval_program(program: &Program, scope: &Scope) -> crate::Result<Mesh> {
    // The top-of-tree benchmark span (I.6): its busy-time is the whole evaluation. Everything below
    // nests under it, so a subscriber can attribute cost to `builtin`/`module` children. TRACE level →
    // free with no subscriber, compiled out in release under `release_max_level_off`.
    let _span = tracing::trace_span!("eval_program").entered();
    let ctx = build_ctx(program, Config::from_env());
    let tree = run_stmts(program.stmts.iter(), &ctx, scope)?;
    // The raw AST path has no file table (`build_ctx` sets `files: None`), so an `import`/`surface` here
    // can't be fulfilled — fail LOUD naming the files rather than return a silently-empty mesh. Real import
    // resolution goes through the file-table entries (`resolve_geometry_*`) + the M.4 shell.
    let needs = ctx.take_file_needs();
    if !needs.is_empty() {
        return Err(unresolved_files(&needs));
    }
    mesh_of(tree)
}

/// Evaluate a SELF-CONTAINED program to its geometry tree AND its deterministic eval-COST — the R.1 perf
/// success-function primitive. The cost is the Q.5 `eval_steps` counter (stack-machine steps + comprehension
/// materialization): a machine-INDEPENDENT proxy for INTERPRETER work — NOT geometry-kernel cost, since the
/// `Geo` tree isn't tessellated here (that axis is native-only + non-deterministic, J.4.5, so it's the
/// OpenSCAD-differential's job, R.2). Runs under `budget` so a pathological input is BOUNDED: a budget-hit
/// returns `Err` with `steps ≈ budget`, which is exactly the TOP of the worst-case ranking. The step count
/// comes back even on `Err` (the ranking wants it regardless). No import resolution — a program that names
/// `import`/`use`/`surface` fails LOUD like [`eval_program`]; the grammar emits self-contained programs, so
/// that path is unreached in practice.
#[must_use = "the step count comes back even on Err — the ranking reads it"]
pub fn evaluate_geometry_metered(program: &Program, budget: u64) -> (crate::Result<Geo>, u64) {
    let config = Config {
        eval_budget: Some(budget),
        ..Config::default()
    };
    let ctx = build_ctx(program, config);
    let result = run_stmts(program.stmts.iter(), &ctx, &Scope::new()).and_then(|tree| {
        let needs = ctx.take_file_needs();
        if needs.is_empty() {
            Ok(tree)
        } else {
            Err(unresolved_files(&needs))
        }
    });
    (result, ctx.eval_steps.get())
}

/// Resolve `source` against caller-supplied source tables to a [`Resolution`] — the PURE inner step of the
/// needs fixpoint (M.4). ZERO IO: it consults `scad_sources` (the `use`/`include` graph the shell has read
/// so far) and `files` (the `import`/`surface` meshes) and NAMES what's still missing. Three outcomes,
/// staged because the two discovery phases can't interleave — a program can't RUN until its libraries LOAD:
/// (1) the `use`/`include` graph isn't closed → [`Resolution::Incomplete`] with `Scad` needs, returned
/// BEFORE any eval; (2) the graph closed but an `import`/`surface` referenced a mesh the table lacks →
/// `Incomplete` with `File` needs (the run substituted empty placeholders + kept going, so ONE call surfaces
/// them all); (3) nothing missing → [`Resolution::Complete`]. The impure [`io`] shell (or an async host)
/// fulfills the needs and calls again. `root_id` is the root's CANONICAL path when it's a file (the shell
/// canonicalizes) so a dependency referencing the root back dedups to the same node.
///
/// # Errors
/// Parse errors and any evaluation error from the flattened program. A missing source is a NEED, not an
/// error — the shell decides whether it can fulfill it.
fn resolve_source(
    source: &str,
    base_dir: &std::path::Path,
    root_id: Option<&std::path::Path>,
    scad_sources: &loader::SourceMap,
    files: &FileTable,
    jit_factory: Option<&dyn NumericJitFactory>,
    config: Config,
) -> crate::Result<Resolution> {
    let _span = tracing::trace_span!("eval_program").entered();
    // Phase 1 (STATIC): close the `use`/`include` graph. A reference not yet in the source table surfaces as
    // a `Scad` need and we return BEFORE eval — the program can't execute until its libraries are present.
    let loaded = match loader::resolve_graph(source, base_dir, root_id, scad_sources)? {
        loader::GraphOutcome::Complete(loaded) => loaded,
        loader::GraphOutcome::Incomplete(scad_needs) => {
            return Ok(Resolution::Incomplete {
                needs: scad_needs
                    .into_iter()
                    .map(|n| SourceNeed::Scad {
                        from_dir: n.from_dir,
                        raw: n.raw,
                    })
                    .collect(),
            });
        }
    };
    // Phase 2 (DYNAMIC): eval. `import`/`surface` `File` needs surface here — only executing reaches them.
    // `flatten` gives the executable statement stream (its own function/module stores are now unused —
    // both are island-scoped). `islands` gives the per-file MODULE scopes AND (the use-scope fix) each
    // file's FUNCTION defs + top-level CONSTANTS, so a `use`d function's body sees its own file's scope.
    let (exec, _defs) = loader::flatten(&loaded)?;
    let islands = loader::islands(&loaded);
    let functions = tagged_functions(&islands);
    let intrinsics = build_intrinsics(&functions);
    // Build the numeric-JIT registry ONCE, now the graph is closed and every function is known (P.1.2b).
    // The factory (native fab-jit) compiles the numeric-subset bodies and returns a `NumericJit`; `None`
    // when there's no factory (wasm / raw path) or nothing compiled. Done here, borrowing `functions`
    // before it moves into the `Ctx`.
    let jit = jit_factory.and_then(|f| {
        let defs: Vec<JitDef<'_>> = functions
            .iter()
            .map(|(name, (fndef, _home))| JitDef {
                name,
                params: fndef.0,
                body: fndef.1,
            })
            .collect();
        // Top-level CONSTANTS the numeric subset can inline (P.1.4 globals): the root file's flat constant
        // view (its `use`d islands' assignments, then the root's own — root wins, mirroring
        // `tagged_functions`). A body reading `_EPSILON`/`INF`/`PHI` compiles the constant instead of declining.
        let consts: Vec<JitConst<'_>> = tagged_globals(&islands)
            .into_iter()
            .map(|(name, value)| JitConst { name, value })
            .collect();
        // `config.jit` is the RUN gate (was the jit crate's own `FAB_JIT` read); the factory keeps its
        // report-only EXPLAIN probe, so it may still compile-for-coverage when this is false.
        f.compile(&defs, &consts, config.jit)
    });
    let n = islands.len();
    let mut ctx = Ctx {
        functions,
        intrinsics,
        jit,
        // Island 0 (root) is filled by `run_stmts`; the rest are seeded empty and built just below.
        island_globals: RefCell::new(vec![Scope::new_with_preview(config.preview); n]),
        islands,
        closures: RefCell::default(),
        messages: RefCell::default(),
        root_override: RefCell::default(),
        files: Some(files),
        file_needs: RefCell::default(),
        module_depth: Cell::default(),
        children_stack: RefCell::default(),
        local_modules: RefCell::default(),
        module_stack: RefCell::default(),
        rand_stream: RefCell::new(rng::RandStream::new()),
        cache: eval_cache::CacheCell::default(),
        mod_cache: mod_cache::CacheCell::default(),
        config,
        impure_reads: std::cell::Cell::new(0),
        eval_steps: std::cell::Cell::new(0),
        live_calls: std::cell::Cell::new(0),
    };
    // Hoist the ROOT (island 0) FIRST, THEN the `use`-island constant scopes. ORDER is load-bearing: the
    // root's include-flattened functions (all of BOSL2's) are tagged home=0, so a `use`-island constant that
    // calls one — monitor's `vnf = heightfield(...)` reaching into BOSL2 — resolves that function's body
    // against `island_globals[0]`. Building the use-islands FIRST (the old order) left 0 empty there, so
    // BOSL2's own constants (`UP`/`_EPSILON`/`CENTER`) read `undef` and silently poisoned the result until a
    // distant `assert(is_vector(axis))` blew up (the remindwall transitive-`use` divergence).
    // `hoist_scope_publishing` publishes into `island_globals[0]` as it binds; hoisting ONCE here (then
    // [`eval_top`], NOT a second `run_stmts` hoist) avoids re-evaluating the ~hundreds of BOSL2 root constants
    // twice — the double-build that tips a borderline model over its budget. (The reverse — a root constant
    // calling a not-yet-built use-island function — stays a gap; a lazy/fixpoint island build is the general
    // answer, but root-homed BOSL2 is the overwhelmingly common case.)
    let global =
        hoist_scope_publishing(&exec, &Scope::new_with_preview(ctx.config.preview), &ctx, 0)?;
    for i in 1..n {
        let island_global = build_island_global(i, &ctx)?;
        if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(i) {
            *slot = island_global;
        }
    }
    // O.5.1: island globals now exist, so the const-guarded intrinsics can prove their baked constants
    // against the real bound values and arm. Before this point (incl. the hoist above) they interpreted.
    for (name, func) in arm_guarded_intrinsics(&ctx) {
        ctx.intrinsics.insert(name, func);
    }
    redundancy::reset(); // dev probe: fresh count per run so the import fixpoint's partial runs don't bleed in
    mod_redundancy::reset(); // dev probe (J.5.1): fresh module-call ceiling per run (FAB_CSG_REDUNDANCY)
    mod_cache::reset_thread_state(); // a panic-unwound PRIOR eval on a reused thread must not leak captures
    fnprofile::reset(); // dev probe: same — fresh per-name call counts per run (FAB_PROFILE_FNS)
    let tree = eval_top(&exec, &global, &ctx)?;
    redundancy::report(); // prints to stderr only under FAB_REDUNDANCY=1
    mod_redundancy::report(); // prints to stderr only under FAB_CSG_REDUNDANCY=1
    if ctx.config.csg_cache {
        ctx.mod_cache.borrow().report(); // realized CSG-cache hit-rate
    }
    fnprofile::report(); // prints to stderr only under FAB_PROFILE_FNS=1
    let needs = ctx.take_file_needs();
    if needs.is_empty() {
        Ok(Resolution::Complete {
            geo: tree,
            messages: ctx.messages.into_inner(),
        })
    } else {
        // A run that named files it couldn't get — the caller reads them + re-runs. The partial `tree`
        // (empty placeholders where the meshes go) is discarded: the closing pass rebuilds it whole.
        Ok(Resolution::Incomplete { needs })
    }
}

/// The no-import spine behind the mesh/geometry convenience entries ([`evaluate`](crate::evaluate) /
/// [`evaluate_geometry`](crate::evaluate_geometry) and kin): drive the fixpoint (the [`io`] shell reads the
/// `use`/`include` graph) with a reader that REFUSES `import`/`surface` files. So those entries stay
/// pure-geometry, and an import through them fails LOUD naming the file rather than returning a
/// silently-empty mesh. Import resolution with real meshes goes through the reader-based
/// [`resolve_geometry_*`](crate::resolve_geometry_with_base) entries + the M.5 backend.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) for an unresolvable `use`/`include` or any `import`/`surface`
/// (no reader here); [`Error::Parse`](crate::Error::Parse) for malformed source; any evaluation error.
pub(crate) fn evaluate_source(
    source: &str,
    base_dir: &std::path::Path,
    root_path: Option<&std::path::Path>,
    library_paths: &[std::path::PathBuf],
    config: Config,
) -> crate::Result<(Geo, Vec<Message>)> {
    // The no-import spine (tests + the pure-geometry `evaluate*` sugar) is interpreter-only — its callers
    // are fab-lang-internal and can't build a JIT. The desktop JIT rides the import-capable
    // `resolve_geometry_*` entries the native shell drives models through.
    io::drive(
        source,
        base_dir,
        root_path,
        library_paths,
        None,
        config,
        io::no_import_reader,
    )
}

/// The LOUD error the raw-AST path ([`eval_program`]) raises when `import`/`surface` executed with no file
/// table (`build_ctx` sets `files: None`) — a named error beats a silently-empty mesh. The loader paths
/// route imports through a mesh reader instead ([`io::drive`]); this covers only the table-less direct eval.
fn unresolved_files(needs: &[SourceNeed]) -> crate::Error {
    crate::Error::Load(format!(
        "import/surface referenced {} file(s) with no mesh reader (raw eval_program) — evaluate through \
         resolve_geometry_* with a reader to supply the meshes: {needs:?}",
        needs.len()
    ))
}

/// The root file's flat FUNCTION view with home-island tags: island 0's `use`d islands FIRST in source
/// order (a later `use` overwrites an earlier one → textually-last `use` wins, matching module
/// resolution), then island 0's OWN defs overriding any `use`-imported name — the precedence
/// [`loader::flatten`] bakes into its function store, but carrying each def's home island so its body can
/// evaluate against that island's constants (the use-scope fix). Fully lexical per-call-site resolution
/// stays deferred; this flat root view is correct for a call from the root, and close enough for a call
/// inside a `use`d function (which almost never hits a name the root also defines).
fn tagged_functions<'a>(
    islands: &loader::Islands<'a>,
) -> BTreeMap<&'a str, (loader::FnDef<'a>, usize)> {
    let mut out = BTreeMap::new();
    // `islands` always has island 0 (the root), so `first()` is the whole population here — no early
    // return, the `if let` just avoids indexing that could theoretically panic.
    if let Some(root) = islands.first() {
        for &u in &root.uses {
            for (&name, &def) in &islands[u].functions {
                out.insert(name, (def, u));
            }
        }
        for (&name, &def) in &root.functions {
            out.insert(name, (def, 0));
        }
    }
    out
}

/// The root file's flat CONSTANT view (name → value expr), the [`tagged_functions`] analogue for top-level
/// assignments: island 0's `use`d islands' assignments FIRST (source order, later overwrites earlier), then
/// island 0's OWN assignments overriding — the same local-over-use precedence. Handed to the JIT so a numeric
/// function referencing a top-level constant resolves it (P.1.4 globals). Every assignment is included,
/// numeric or not; a non-numeric one (a vector/string constant) just makes any referrer DECLINE when the JIT
/// compiles its value-expr. `last`-wins within an island matches how [`build_island_global`] re-binds.
///
/// EXCEPT `$`-assignments (task #51): a `$`-variable is dynamically scoped — a top-level `$fn = 32;` is only
/// the fallback the runtime call chain overrides — so it must never reach the JIT as an inlinable lexical
/// constant. The compiler's `Ident` arm independently declines every `$`-read (the authoritative guard);
/// filtering here keeps the registry from even holding one. The interpreter's own hoist reads
/// [`loader::Island::assignments`] directly, so top-level `$`-bindings still work everywhere else.
fn tagged_globals<'a>(islands: &loader::Islands<'a>) -> BTreeMap<&'a str, &'a Expr> {
    let mut out = BTreeMap::new();
    if let Some(root) = islands.first() {
        for &u in &root.uses {
            for &(name, expr) in &islands[u].assignments {
                if !name.starts_with('$') {
                    out.insert(name, expr);
                }
            }
        }
        for &(name, expr) in &root.assignments {
            if !name.starts_with('$') {
                out.insert(name, expr);
            }
        }
    }
    out
}

/// A fingerprint-matched entry's REMAINING guards (O.5.2): the dep pins (each user fn the reference can
/// reach must fingerprint to ITS OWN registry/pin reference — the fingerprint gate extended one hop) and the
/// builtin-shadow check (a user fn shadowing a builtin the reference leans on reroutes the interpreted body
/// while the native keeps the real builtin — BOSL2 itself shadows `reverse`, so this is a per-entry check,
/// never a blanket). Returns the EXPLAIN reason the entry can't wire, or `None` when clear.
fn guard_veto<'a>(
    entry: &intrinsics::Entry,
    functions: &BTreeMap<&'a str, (loader::FnDef<'a>, usize)>,
) -> Option<String> {
    for &dep in entry.deps {
        let Some(&((p, b), _)) = functions.get(dep) else {
            return Some(format!("dep `{dep}` is not defined in this program"));
        };
        if intrinsics::anchor_fp(dep) != Some(intrinsics::fingerprint(p, b)) {
            return Some(format!("dep `{dep}` drifted from its pinned reference"));
        }
    }
    for &b in entry.builtins {
        if functions.contains_key(b) {
            return Some(format!("builtin `{b}` is shadowed by a user function"));
        }
    }
    None
}

/// Resolve each defined function to a registered INTRINSIC (O.1) — the fingerprint gate, run ONCE here at
/// ctx build so call-time dispatch is a cheap name lookup. A function whose `(params, body)` fingerprints to
/// a registry entry AND clears its dep/builtin guards ([`guard_veto`]) gets a native impl; everything else
/// (the vast majority) is absent → interpreted. Built from the SAME resolved `functions` map the interpreter
/// dispatches against, so the body matched is the body that would run. CONST-GUARDED entries (non-empty
/// `consts`) never wire here — they arm post-hoist in [`arm_guarded_intrinsics`].
fn build_intrinsics<'a>(
    functions: &BTreeMap<&'a str, (loader::FnDef<'a>, usize)>,
) -> BTreeMap<&'a str, intrinsics::Intrinsic> {
    let explain = intrinsics::explain_on();
    let mut out = BTreeMap::new();
    for (&name, &((params, body), _home)) in functions {
        // EXPLAIN report (O.3): under FAB_EXPLAIN, say whether each registry-covered function will fire
        // natively (WIRED) or silently interprets because its body drifted from the intrinsic's reference
        // (DRIFT) — the answer to "is my intrinsic actually getting used on this program?".
        if explain {
            // Print the fingerprints so an author can diagnose a DRIFT: `defined` is what the user's actual
            // library hashes to; `reference` is what the intrinsic was written against. If they differ, EITHER
            // paste `defined` as an updated reference (library moved) OR fix a stale reference. (chotchki's ask.)
            match intrinsics::classify(name, params, body) {
                intrinsics::Plan::Wired => {
                    eprintln!(
                        "+ [intrinsic WIRED] {name} (fp {:#018x})",
                        intrinsics::fingerprint(params, body)
                    );
                }
                intrinsics::Plan::Drift => eprintln!(
                    "+ [intrinsic DRIFT] {name} — defined fp {:#018x} != reference fp {} → INTERPRETED \
                     (library drift, or a stale reference)",
                    intrinsics::fingerprint(params, body),
                    intrinsics::reference_fp(name)
                        .map_or_else(|| "?".to_string(), |fp| format!("{fp:#018x}")),
                ),
                intrinsics::Plan::NotRegistered => {}
            }
        }
        let Some(entry) = intrinsics::resolve(name, params, body) else {
            continue;
        };
        if !entry.consts.is_empty() || !entry.consts_v.is_empty() {
            continue; // const-guarded (numeric or Value): arms post-hoist (arm_guarded_intrinsics)
        }
        match guard_veto(entry, functions) {
            None => {
                out.insert(name, entry.func);
            }
            Some(why) => {
                if explain {
                    eprintln!("+ [intrinsic GUARD-DECLINED] {name} — {why} → INTERPRETED");
                }
            }
        }
    }
    out
}

/// Arm the CONST-GUARDED intrinsics (O.5.1): registry entries whose native impl BAKES a named top-level
/// constant (`eps=_EPSILON`) — the fingerprint proves the function's source, not the constants it names, so
/// these can't wire at [`build_intrinsics`] time. Called AFTER island globals are built (they don't exist
/// earlier — and mid-hoist a partially-bound scope could make even a correct bake diverge, so the interpreter
/// runs there); wires each fingerprint-matched entry ONLY if its dep/builtin guards clear ([`guard_veto`])
/// AND every guarded constant's BOUND value in the fn's home-island global is bit-exactly the baked one.
/// Nothing rebinds a top-level global after the hoist (a module-local shadow can't reach a default, which
/// evaluates in the DEFINITION scope), so a verdict here holds for the whole eval. Returns the additions for
/// the caller to insert — only the loader path and the JIT oracle arm; the raw-AST path (tests,
/// single-program evals) fuses hoist+eval so guarded entries simply stay interpreted there (correct, just
/// not accelerated).
fn arm_guarded_intrinsics<'a>(ctx: &Ctx<'a>) -> Vec<(&'a str, intrinsics::Intrinsic)> {
    let explain = intrinsics::explain_on();
    let mut out = Vec::new();
    for (&name, &((params, body), home)) in &ctx.functions {
        let Some(entry) = intrinsics::resolve(name, params, body) else {
            continue;
        };
        if entry.consts.is_empty() && entry.consts_v.is_empty() {
            continue; // unguarded: already wired (or vetoed) at build_intrinsics
        }
        if let Some(why) = guard_veto(entry, &ctx.functions) {
            if explain {
                eprintln!("+ [intrinsic GUARD-DECLINED] {name} — {why} → INTERPRETED");
            }
            continue;
        }
        let globals = ctx.island_globals.borrow();
        let Some(scope) = globals.get(home) else {
            continue;
        };
        let bad = entry.consts.iter().find(|&&(cname, expected)| {
            !matches!(scope.lookup_opt(cname), Some(Value::Num(n)) if n.to_bits() == expected.to_bits())
        });
        // the VALUE-typed half (O.8): the bound value must bit-match the built expectation exactly
        let bad_v = entry.consts_v.iter().find(|&&(cname, expected)| {
            !scope
                .lookup_opt(cname)
                .is_some_and(|v| intrinsics::value_bits_eq(&v, &expected()))
        });
        match (bad, bad_v) {
            (None, None) => {
                if explain {
                    eprintln!("+ [intrinsic ARMED] {name} — const guard ok");
                }
                out.push((name, entry.func));
            }
            (Some(&(cname, expected)), _) => {
                if explain {
                    eprintln!(
                        "+ [intrinsic CONST-DECLINED] {name} — `{cname}` in its home scope is {:?}, \
                         intrinsic baked {expected} → INTERPRETED",
                        scope.lookup_opt(cname),
                    );
                }
            }
            (None, Some(&(cname, _))) => {
                if explain {
                    eprintln!(
                        "+ [intrinsic CONST-DECLINED] {name} — `{cname}` in its home scope is {:?}, \
                         which is not the baked value → INTERPRETED",
                        scope.lookup_opt(cname),
                    );
                }
            }
        }
    }
    out
}

/// Build island `i`'s CONSTANT scope: its top-level assignments hoisted (whole-scope, last-wins, in
/// first-occurrence order) into a fresh `$fn`/`PI`-seeded scope — so a `use`d function/module body reads
/// its own file's constants. Evaluated with `ctx` (constants can call functions). PUBLISHES the growing
/// scope into `island_globals[i]` after each bind — so a constant whose RHS calls a same-island function
/// lets that function read the constants bound SO FAR (its home-island lexical base). Without it the
/// function resolves against the not-yet-stored island global (empty during the very hoist that builds
/// it) → the constant reads `undef`. A constant reading a LATER same-island constant still sees `undef`
/// (only constants bound BEFORE it are published) — the same whole-scope forward-reference rule the root
/// global follows (`n = 1; n = n + 1;` → undef).
fn build_island_global(island: usize, ctx: &Ctx<'_>) -> crate::Result<Scope> {
    let mut scope = Scope::new_with_preview(ctx.config.preview);
    for &(name, expr) in &ctx.islands[island].assignments {
        let value = name_closure(eval_with_ctx(expr, &scope, ctx)?, name);
        scope.bind(name.to_string(), value);
        if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(island) {
            *slot = scope.clone();
        }
    }
    Ok(scope)
}

/// Evaluate a statement stream to a dimension-tagged geometry TREE ([`Geo`]) — shared by
/// [`eval_program`] and the loader path. The result is the implicit union of the top-level objects (2D or
/// 3D per their dimension, mixing warned). The tree keeps fab-lang backend-agnostic: a single 3D primitive
/// is a `Leaf` [`mesh_of`] can flatten with no kernel; anything with a transform, a boolean, or any 2D
/// geometry needs the downstream Manifold backend (J.2 / J.3).
fn run_stmts<'a>(
    stmts: impl Iterator<Item = &'a Stmt>,
    ctx: &Ctx<'a>,
    scope: &Scope,
) -> crate::Result<Geo> {
    let stmts: Vec<&Stmt> = stmts.collect();
    // The top-level hoisted scope IS the GLOBAL base for module bodies (a user module evaluates in
    // `global.child()` + its params — OpenSCAD's lexical hygiene). Hoist ONCE (not a pre-hoist +
    // re-hoist — that would let a forward reference see the pre-bound value, breaking `a = b; b = 5` →
    // `a` is undef), then evaluate the geometry in that same scope. The root file IS island 0, so this
    // hoist PUBLISHES the growing global into `island_globals[0]` after each bind (see
    // [`hoist_scope_publishing`]) — a top-level `x = <lib-fn-using-a-constant>` (e.g.
    // `x = turtle([arc...])`, whose `arc` reads the library constant `_EPSILON`) must let that function
    // resolve island-0 constants DURING the hoist that builds them, not against the empty pre-publish global.
    let global = hoist_scope_publishing(&stmts, scope, ctx, 0)?;
    eval_top(&stmts, &global, ctx)
}

/// Evaluate the (already-HOISTED) root statement stream to the geometry tree. Split out of [`run_stmts`] so
/// the loader path can hoist island 0 ONCE — BEFORE building the `use`-island globals that depend on it — and
/// then eval here without a second hoist (Q.3: a `use`-island constant like monitor's `vnf = heightfield(...)`
/// calls a root-homed BOSL2 function, which must resolve BOSL2's constants against a POPULATED
/// `island_globals[0]`; building the root LAST left them `undef`).
fn eval_top<'a>(stmts: &[&'a Stmt], global: &Scope, ctx: &Ctx<'a>) -> crate::Result<Geo> {
    // Clear any `!`-override residue from a prior resolution attempt (the loader re-runs on file-needs), then
    // eval. Top-level statements resolve modules against island 0 (the root file, I.9.5).
    ctx.root_override.borrow_mut().clear();
    let nodes = eval_geometry(stmts, global, global, 0, ctx)?;
    // `!` ROOT modifier: if any subtree was `!`-tagged, the program renders ONLY those (ancestors + siblings
    // discarded — `eval_stmt` diverted them into `root_override`). Otherwise the implicit union of top-level
    // objects. `split_off(0)` drains the buffer so a re-run starts clean.
    let root = ctx.root_override.borrow_mut().split_off(0);
    let items = if root.is_empty() { nodes } else { root };
    // MARK the implicit union of MULTIPLE top-level statements as `Parts` (W.3.34): geometrically it's the
    // same union `union_of` builds, but the marker lets the parts splitter treat each top-level item as its
    // own printable part WITHOUT splitting an explicit `union(){…}` the user wrote as a single statement.
    // Only ≥2 items qualify — a lone statement (even an explicit union) is one part, so it stays a plain
    // node. If `union_of` collapsed the ≥2 items to a single non-`Union` (all but one were Empty), there's
    // nothing to split, so leave it be.
    let multi = items.len() > 1;
    let mut merged = union_of(items, ctx);
    // `GeoNode` has a custom (iterative) `Drop`, so the kids can't be moved out by pattern — `mem::take`
    // the Vec (ends the borrow), then rebuild as `Parts`.
    let parts_kids = match &mut merged {
        Geo::D3(GeoNode::Union(kids)) if multi => Some(std::mem::take(kids)),
        _ => None,
    };
    if let Some(kids) = parts_kids {
        merged = Geo::D3(GeoNode::Parts(kids));
    }
    Ok(merged)
}

/// Collect the scope-LOCAL `module` definitions of a statement list (last-wins by name) — the module-side
/// analogue of [`hoisted_bindings`]'s function handling. Kept a stmt-list pure pass; [`eval_nodes`] pushes
/// the result for the block's eval so a body-local `module f(){…}` resolves (L.2.8m).
fn collect_module_defs<'a>(stmts: &[&'a Stmt]) -> loader::ModStore<'a> {
    let mut store = loader::ModStore::new();
    for stmt in flatten_blocks(stmts) {
        if let StmtKind::ModuleDef { name, params, body } = &stmt.kind {
            store.insert(name.as_str(), (params.as_slice(), body.as_ref()));
        }
    }
    store
}

/// Hoist a statement list's assignments into a fresh working scope (a clone of `scope`): OpenSCAD's
/// whole-scope, last-assignment-wins rule, evaluating them in first-occurrence order so a forward /
/// self-reference sees `undef`. Returns the bound scope — the pure prefix `eval_nodes` and `run_stmts`
/// share. Hoisting into a FRESH scope (nothing pre-bound) is what keeps `a = b; b = 5` → `a` undef.
fn hoist_scope<'a>(stmts: &[&'a Stmt], scope: &Scope, ctx: &Ctx<'a>) -> crate::Result<Scope> {
    // A CHILD, not a clone (BU.8 review finding 1): binding into a clone COWs the SHARED frame — for a
    // cached module body that frame is the capture's entry, and the COW replaced it mid-capture. The
    // boundary id now survives COW regardless; the child keeps the binds BELOW the boundary so an in-body
    // `$x = …` KILLS instead of recording its own bound value (the read-set stays caller-facing).
    let mut scope = scope.child();
    let items = hoisted_bindings(stmts);
    // Register the body's nested functions as ONE letrec GROUP up front (L.5.4), so each — bound below in
    // textual order (capturing the enclosing locals hoisted before it) — carries the WHOLE sibling set and
    // can call a function defined LATER in the same body (`_gather_contiguous_edges` → the `_r` below it).
    let group = register_fn_group(&items, ctx);
    // The group holds one entry per `Func` item in the SAME order `register_fn_group` walked `items`, so a
    // single forward iterator stays aligned with the `Func` arms below — no per-name lookup, no `expect`.
    let mut siblings = group.iter().flat_map(|g| g.iter());
    for item in &items {
        // sigil `=` for an assignment, `f` for a hoisted fn-shaped binding (so the trace tells
        // them apart). A module-body-LOCAL `function f(x)=…` — or an assignment whose RHS is a
        // function LITERAL ([`fn_shape`], AH.2.7) — becomes a closure VALUE in the body scope,
        // capturing the CURRENT scope so it sees the enclosing locals hoisted before it (BOSL2's
        // `make_path` closes over `steps`/`ang`), PLUS the shared group so it reaches every
        // sibling (`self_name` still gives it direct self-recursion). `dispatch_call`'s
        // function-value path applies it.
        let (sigil, name, value) = if let Some((name, ..)) = fn_shape(item) {
            match siblings.next() {
                Some(s) => ('f', name, nested_fn_value(s, &scope, group.as_ref())),
                None => continue, // register_fn_group emits one entry per fn-shape — unreachable
            }
        } else if let HoistItem::Assign(name, expr) = *item {
            (
                '=',
                name,
                name_closure(eval_with_ctx(expr, &scope, ctx)?, name),
            )
        } else {
            continue; // Func is always fn-shaped — unreachable
        };
        trace::bind(sigil, name, &value);
        scope.bind(name.to_string(), value);
    }
    Ok(scope)
}

/// Like [`hoist_scope`], but PUBLISH the growing scope into `island_globals[island]` after each bind —
/// so a top-level constant whose RHS calls a same-island function (e.g. `x = turtle([arc...])`, whose
/// `arc` reads the library constant `_EPSILON`) lets that function resolve the island's constants bound
/// SO FAR (its home-island lexical base, the use-scope-hygiene lookup in [`dispatch_call`]). Without it
/// the function resolves against the not-yet-published island global (empty during the hoist that builds
/// it) → the constant reads `undef`, and BOSL2's arc asserts on the undef epsilon. Forward references
/// still see `undef` (only constants bound BEFORE the caller are published) — the same whole-scope rule
/// [`hoist_scope`] follows. Used for island 0 (the root) in [`run_stmts`]; the `use`d islands get the
/// identical treatment in [`build_island_global`].
fn hoist_scope_publishing<'a>(
    stmts: &[&'a Stmt],
    scope: &Scope,
    ctx: &Ctx<'a>,
    island: usize,
) -> crate::Result<Scope> {
    let mut scope = scope.clone();
    let items: Vec<HoistItem> = hoisted_assignments(stmts)
        .into_iter()
        .map(|(n, e)| HoistItem::Assign(n, e))
        .collect();
    // Top-level function-LITERAL assignments form a letrec group too (AH.2.7): upstream's shared
    // file context makes `chaining1 = function(x) … chaining2(x-1)` resolve a sibling defined
    // BELOW it at call time — same mechanism as module-body nested defs.
    let group = register_fn_group(&items, ctx);
    let mut siblings = group.iter().flat_map(|g| g.iter());
    for item in &items {
        let (name, value) = if let Some((name, ..)) = fn_shape(item) {
            match siblings.next() {
                Some(s) => (name, nested_fn_value(s, &scope, group.as_ref())),
                None => continue, // register_fn_group emits one entry per fn-shape — unreachable
            }
        } else if let HoistItem::Assign(name, expr) = *item {
            // W.3.37: stamp the ROOT file's top-level assignment span so a hoist-time fault (a bad value fed
            // deep into a library fn — e.g. `p = bezpath_curve(pts, $fn, 2)` with $fn=0) points at the USER's
            // line, not a library one. Root (island 0) ONLY — a use-island's spans are library-file-local and
            // would mis-map against the editor buffer. `at` is a no-op if the error is already spanned.
            let evaluated = eval_with_ctx(expr, &scope, ctx);
            let evaluated = if island == 0 {
                evaluated.map_err(|e| e.at(expr.span.clone()))?
            } else {
                evaluated?
            };
            (name, name_closure(evaluated, name))
        } else {
            continue; // Func never comes through hoisted_assignments
        };
        trace::bind('=', name, &value);
        scope.bind(name.to_string(), value);
        if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(island) {
            *slot = scope.clone();
        }
    }
    Ok(scope)
}

/// Evaluate the GEOMETRY statements of a list (assignments already hoisted into `scope`) → their nodes,
/// threading `global` unchanged for any module body's lexical base and `island` for module resolution
/// (I.9.5 — the module scope of the file these statements were textually defined in).
fn eval_geometry<'a>(
    stmts: &[&'a Stmt],
    scope: &Scope,
    global: &Scope,
    island: usize,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Geo>> {
    // M.3: geometry eval runs on the explicit-stack DRIVER — heap-bounded eval depth, no host recursion. The
    // former recursive tree-walk (eval_stmt/eval_stmt_dispatch/call_user_module/eval_children/for_product) was
    // retired once the driver proved bit-identical across the corpus + the models oracle-differential (A/B).
    geo_stack::eval_geometry_driver(stmts, scope, global, island, ctx)
}

/// A dimension-homogeneous child list — the output of [`partition_children`], ready to become a boolean
/// or a union node of the right dimension. Exactly one dimension survives a group (OpenSCAD picks the
/// first child's), so this is a 2-way split, not a pair of lists.
enum Children {
    /// The kept children are all 3D.
    D3(Vec<GeoNode>),
    /// The kept children are all 2D.
    D2(Vec<Shape2D>),
}

/// Filter a group's children to a SINGLE dimension, warning on (and dropping) any mismatch — OpenSCAD's
/// "Mixing 2D and 3D objects is not supported". This is the shared choke point for every N-ary grouping
/// op (implicit union, `union`/`difference`/`intersection`, `for`), so the rule lives in one place.
///
/// The dimension is set by the FIRST non-null child; each later NON-NULL child whose dimension differs is
/// dropped with an "Ignoring {n}D child object for {m}D operation" warning, and the "Mixing…" warning
/// fires ONCE (on the first mismatch). A matching child AFTER a mismatch still survives. Null children
/// ([`Geo::is_null`] — a `{}` / never-run `for`) are dim-neutral: dropped, never dimension-fixing, never
/// warned. Every clause here is pinned against OpenSCAD 2026.06.12 (see the `mixing_*` tests).
///
/// NOTE: the warning text matches OpenSCAD's core message; the ` in file …, line N` suffix it appends is
/// deferred with the rest of location-aware diagnostics (I.5 / #94) — the geometry tree carries no spans.
fn partition_children(children: Vec<Geo>, ctx: &Ctx) -> Children {
    let mut d3: Vec<GeoNode> = Vec::new();
    let mut d2: Vec<Shape2D> = Vec::new();
    let mut dim: Option<u8> = None;
    let mut warned_mixing = false;
    for child in children {
        if child.is_null() {
            continue; // a `{}` / never-run `for` — no geometry object, so dimension-neutral
        }
        let cdim = child.dim();
        match dim {
            None => {
                dim = Some(cdim); // the first present child fixes the group's dimension
                push_child(child, &mut d2, &mut d3);
            }
            Some(d) if d == cdim => push_child(child, &mut d2, &mut d3),
            Some(d) => {
                if !warned_mixing {
                    ctx.warn("Mixing 2D and 3D objects is not supported".to_string());
                    warned_mixing = true;
                }
                ctx.warn(format!("Ignoring {cdim}D child object for {d}D operation"));
                // dropped — the mismatched child contributes nothing to this operation
            }
        }
    }
    // No present child → an empty 3D result (the historical `Empty`, dimension-agnostic for export).
    if matches!(dim, Some(2)) {
        Children::D2(d2)
    } else {
        Children::D3(d3)
    }
}

/// Route a kept child into its dimension's bucket (the mismatched dimension's bucket stays empty, since
/// [`partition_children`] only keeps one dimension).
fn push_child(child: Geo, d2: &mut Vec<Shape2D>, d3: &mut Vec<GeoNode>) {
    match child {
        Geo::D2(s) => d2.push(s),
        Geo::D3(n) => d3.push(n),
    }
}

/// Wrap a group's children in the implicit union of their (single) dimension: none → `Empty`, one →
/// itself, many → `Union` (OpenSCAD unions multiple top-level objects + a block's children). The
/// dimension mix is resolved first by [`partition_children`]. The collapse to `Empty` on an EMPTY group is
/// deliberate — a `{}` / never-run `for` / not-taken `if` is null (dim-neutral); it means `for(i=[]) …`
/// drops out of a CSG operand list rather than acting as an empty operand (OpenSCAD keeps a bare `{}` out
/// of the list the same way, though it treats an empty `for` as a present empty operand — a node-identity
/// quirk we don't reproduce; no real program relies on it).
fn union_of(children: Vec<Geo>, ctx: &Ctx) -> Geo {
    collapse(
        partition_children(children, ctx),
        GeoNode::Union,
        Shape2D::Union,
    )
}

/// The implicit-INTERSECTION combinator — `intersection_for`'s per-dimension collapse (none → `Empty`,
/// one → itself, many → `Intersection`). The intersection sibling of [`union_of`], same null-collapse rule.
fn intersection_of(children: Vec<Geo>, ctx: &Ctx) -> Geo {
    collapse(
        partition_children(children, ctx),
        GeoNode::Intersection,
        Shape2D::Intersection,
    )
}

/// Collapse a dimension-resolved child list into ONE node of that dimension: none → `Empty`, one → the
/// child itself, many → the N-ary node built by `mk3`/`mk2`. Shared by [`union_of`] and [`intersection_of`]
/// (they differ only in the many-child constructor). Only the 3D side needs an empty case: a `D2` tag
/// means the first non-null child was 2D and got kept, so a 2D list is NEVER empty (see
/// [`partition_children`]) — the 2D side is a two-way split, no dead zero-arm.
fn collapse(
    children: Children,
    mk3: fn(Vec<GeoNode>) -> GeoNode,
    mk2: fn(Vec<Shape2D>) -> Shape2D,
) -> Geo {
    match children {
        Children::D3(mut nodes) => Geo::D3(match nodes.len() {
            0 => GeoNode::Empty,
            1 => nodes.pop().unwrap_or(GeoNode::Empty),
            _ => mk3(nodes),
        }),
        Children::D2(mut shapes) => Geo::D2(if shapes.len() == 1 {
            shapes.pop().unwrap_or(Shape2D::Empty)
        } else {
            mk2(shapes) // ≥ 2 — a D2 partition never yields an empty list
        }),
    }
}

/// Build an EXPLICIT CSG boolean node (`union` / `difference` / `intersection` module) of its children's
/// single dimension — no single-child collapse (an explicit `union(){ a; }` keeps its node, unlike the
/// implicit [`union_of`]). `difference` is first-minus-rest, resolved by the backend's fold.
fn boolean_of(name: &str, children: Vec<Geo>, ctx: &Ctx) -> Geo {
    match partition_children(children, ctx) {
        Children::D3(kids) => Geo::D3(match name {
            "difference" => GeoNode::Difference(kids),
            "intersection" => GeoNode::Intersection(kids),
            _ => GeoNode::Union(kids),
        }),
        Children::D2(kids) => Geo::D2(match name {
            "difference" => Shape2D::Difference(kids),
            "intersection" => Shape2D::Intersection(kids),
            _ => Shape2D::Union(kids),
        }),
    }
}

/// Wrap a single (already dimension-resolved) child in an affine transform of its dimension: a 3D child
/// gets a [`GeoNode::Transform`] with the full 3×4 matrix; a 2D child a [`Shape2D::Transform`] with the
/// matrix's 2D restriction ([`Affine2::from_affine3`] — a 2D shape lives in the `z = 0` plane, so only the
/// in-plane part applies, matching OpenSCAD; verified vs 2026.06.12 for translate/scale/rotate).
fn transform_of(matrix: Affine, child: Geo) -> Geo {
    match child {
        Geo::D3(node) => Geo::D3(GeoNode::Transform {
            matrix,
            child: Box::new(node),
        }),
        Geo::D2(shape) => Geo::D2(Shape2D::Transform {
            matrix: Affine2::from_affine3(&matrix),
            child: Box::new(shape),
        }),
    }
}

/// Coerce a child to 2D for a FIXED-2D operation (`offset`; later `linear_extrude` / `rotate_extrude`).
/// A 2D child is taken as-is. A real 3D child is IGNORED with OpenSCAD's `Ignoring 3D child object for 2D
/// operation` warning — note NO `Mixing` warning, unlike a dimension-DISCOVERING group: the op's
/// dimension is fixed, so there's no mix to report — and the result is the empty region. A null child
/// (`{}`) is the empty region, silently. Verified vs OpenSCAD 2026.06.12 (`offset(2) cube(5)` → exactly
/// that one warning + an empty 2D result).
fn force_2d(child: Geo, ctx: &Ctx) -> Shape2D {
    match child {
        Geo::D2(shape) => shape,
        Geo::D3(GeoNode::Empty) => Shape2D::Empty, // a null child → empty, no warning
        Geo::D3(_) => {
            ctx.warn("Ignoring 3D child object for 2D operation".to_string());
            Shape2D::Empty
        }
    }
}

/// Coerce a child to 3D for a FIXED-3D operation (`projection`, which consumes a solid and flattens it).
/// A 3D child is taken as-is — INCLUDING a null `{}` (which arrives as `Geo::D3(GeoNode::Empty)`, so the
/// empty node passes silently, no warning). A real 2D child is IGNORED with OpenSCAD's `Ignoring 2D child
/// object for 3D operation` warning (verified vs 2026.06.12 — `projection() square(5)` → that warning +
/// an empty result). Simpler than the [`force_2d`] null special-case: there the null child comes in on the
/// OPPOSITE dimension (a 3D null under a 2D op), so it must dodge the warning explicitly; here the null is
/// already 3D and rides the first arm.
fn force_3d(child: Geo, ctx: &Ctx) -> GeoNode {
    match child {
        Geo::D3(node) => node,
        Geo::D2(_) => {
            ctx.warn("Ignoring 2D child object for 3D operation".to_string());
            GeoNode::Empty
        }
    }
}

/// A child count as a `Num` — the child list is tiny, so the `usize → f64` widening is exact.
#[allow(
    clippy::cast_precision_loss,
    reason = "a call's child count is small; the widening is exact"
)]
fn child_count(n: usize) -> f64 {
    n as f64
}

/// A `children(i)` index: a non-negative WHOLE number → its `usize`, else `None` (dropped).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: only a non-negative integer-valued f64 converts; everything else is None"
)]
fn child_at(i: f64) -> Option<usize> {
    (i >= 0.0 && i.fract() == 0.0).then_some(i as usize)
}

/// Build a user module's call scope: match `args` to `params` (positional fill left-to-right, named by
/// name, defaults for the rest), then bind them + the `$`-args into a fresh child of `global`. Mirrors
/// the function-call arg-match ([`push_call`]) but EAGER (statement level, no `Task` machine): arg exprs
/// evaluate in the CALLER scope, defaults in `global` (the definition scope), `$`-args bind LAST so they
/// override the inherited dynamic `$`-context.
fn bind_module_scope<'a>(
    params: &'a [Parameter],
    args: &'a [Arg],
    caller: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Scope> {
    // Which explicit-arg expr fills each param slot ([`fill_arg_slots`] — positionals take the
    // lowest unfilled slot, AH.2.4). `None` = the param took no arg → its default in phase 1.
    let (arg_slots, dollars) = fill_arg_slots(params, args);

    // Lexically a child of the module's home `global` (hygiene), dynamically a child of `caller` (inherits
    // the caller's $-context by reference — no per-call $-copy). $-args (bound last) shadow the inherited.
    let mut call = Scope::call_frame(global, caller);
    // OpenSCAD binds in TWO phases — ALL defaults first (declaration order), THEN the passed args on top —
    // so an argument always wins over a default regardless of param order. That ordering is load-bearing
    // when a param NAME is DUPLICATED (BOSL2's `rounding_edge_mask` lists `r` twice, once defaultless): the
    // unfilled second `r` writes `undef` in phase 1, and the explicit `r=2` overwrites it in phase 2. A
    // single interleaved pass instead let that trailing `undef` clobber the real value → get_radius(undef).
    // Phase 1 — defaults (eval'd in the library global) / undef for a defaultless unfilled param.
    for (param, slot) in params.iter().zip(&arg_slots) {
        if slot.is_none() {
            let value = match &param.default {
                Some(default) => eval_with_ctx(default, global, ctx)?,
                None => Value::Undef,
            };
            call.bind(Rc::clone(&param.name), value);
        }
    }
    // Phase 2 — passed args (eval'd in the caller scope) override, in declaration order.
    for (param, slot) in params.iter().zip(&arg_slots) {
        if let Some(expr) = slot {
            let value = eval_with_ctx(expr, caller, ctx)?;
            call.bind(Rc::clone(&param.name), value);
        }
    }
    for (name, expr) in dollars {
        let value = eval_with_ctx(expr, caller, ctx)?;
        call.bind(name, value); // $-args last → override the inherited $-context
    }
    Ok(call)
}

/// The values a `for` binding iterates: a range → its (capped) values, a vector → its elements, a
/// scalar → a single iteration (OpenSCAD's `for(i = 5)`). A too-many range warns + iterates zero
/// times, like [`iter_values`].
fn iterate_values(v: &Value, ctx: &Ctx) -> Vec<Value> {
    match v {
        Value::Range { start, step, end } => range_values(*start, *step, *end, ctx),
        Value::NumList(xs) => xs.iter().map(|&n| Value::Num(n)).collect(),
        Value::List(xs) => xs.to_vec(),
        // AH.2.6 (the for-tests golden): `for (c = "a↑b😀")` iterates CHARACTERS (each a one-char
        // string); an empty string iterates ZERO times — never the scalar one-iteration fallback.
        Value::Str(s) => s.chars().map(|c| Value::string(c.to_string())).collect(),
        // `for (j = undef)` iterates ZERO times upstream (special-consts golden: an invalid
        // `[a:b]` range collapses to undef and the loop is silent) — undef is not a scalar value.
        Value::Undef => Vec::new(),
        // An OBJECT iterates its KEYS in insertion order (AF.4).
        Value::Object(o) => o.keys().map(|k| Value::Str(Rc::clone(k))).collect(),
        other => vec![other.clone()],
    }
}

/// Flatten a geometry tree WITHOUT a backend: `Empty` → an empty mesh, a single 3D `Leaf` → its mesh.
/// Anything with a transform, a boolean, or ANY 2D geometry needs the Manifold backend (fab-scad), so it
/// errors LOUD — callers reach for [`evaluate_geometry`](crate::evaluate_geometry) + a backend instead.
pub(crate) fn mesh_of(mut tree: Geo) -> crate::Result<Mesh> {
    // Match by `&mut` and `mem::replace` the pieces out — `GeoNode` now has an iterative `Drop` (M.1), so a
    // by-value move out of it is E0509. Leaving `Empty` behind lets `tree` drop trivially here.
    match &mut tree {
        Geo::D3(GeoNode::Empty) => Ok(Mesh::new()),
        Geo::D3(GeoNode::Leaf(mesh)) => Ok(std::mem::replace(mesh, Mesh::new())),
        // Color is a display property, not geometry — a colored PRIMITIVE still flattens with no backend.
        Geo::D3(GeoNode::Color { child, .. }) => {
            mesh_of(Geo::D3(std::mem::replace(&mut **child, GeoNode::Empty)))
        }
        Geo::D3(_) => Err(crate::Error::Unimplemented(
            "geometry with transforms or booleans needs a backend — use evaluate_geometry (J.2)",
        )),
        // 2D geometry can't become a 3D mesh — it lowers to a Manifold CrossSection in the backend (J.3).
        Geo::D2(_) => Err(crate::Error::Unimplemented(
            "2D geometry (square/circle/polygon/…) has no 3D mesh — use evaluate_geometry + a backend, or \
             extrude it into 3D (J.3)",
        )),
    }
}

/// The hoisted assignment order of a scope, as a PURE function (statements in → ordered `(name, expr)`
/// out, no evaluation, no side effects): a scope's assignments deduped by name in FIRST-occurrence
/// order, each carrying the LAST assignment's expr. Mirrors OpenSCAD's parser (`handle_assignment`
/// overwrites a duplicate's expr in place, keeping its position) feeding `ScopeContext::init`, which
/// evaluates them in that order. The caller evaluates + binds; keeping the ORDER pure makes the
/// last-assignment-wins + forward-ref-is-undef rules unit-testable without a scope.
/// The statement list with bare `{ … }` blocks flattened INLINE (AH.2.5, the scope-assignment
/// golden): upstream "anonymous scopes are not supported" — a bare block is geometry GROUPING,
/// never a variable scope, so its assignments and nested defs participate in the ENCLOSING
/// scope's whole-scope last-wins hoist (`{ f=2; }` overwrites an outer `f=1`). Iterative — a
/// fuzzer-deep block nest must not cost host stack.
pub(crate) fn flatten_blocks<'a>(stmts: &[&'a Stmt]) -> Vec<&'a Stmt> {
    let mut out = Vec::new();
    let mut stack: Vec<&'a Stmt> = stmts.iter().rev().copied().collect();
    while let Some(s) = stack.pop() {
        if let StmtKind::Block(inner) = &s.kind {
            stack.extend(inner.iter().rev());
        } else {
            out.push(s);
        }
    }
    out
}

fn hoisted_assignments<'a>(stmts: &[&'a Stmt]) -> Vec<(&'a str, &'a Expr)> {
    let mut order: Vec<(&'a str, &'a Expr)> = Vec::new();
    let mut index: BTreeMap<&'a str, usize> = BTreeMap::new();
    for stmt in flatten_blocks(stmts) {
        if let StmtKind::Assignment { name, value } = &stmt.kind {
            if let Some(&i) = index.get(&**name) {
                order[i].1 = value; // seen: last expr wins, first-occurrence position kept
            } else {
                index.insert(&**name, order.len());
                order.push((&**name, value));
            }
        }
    }
    order
}

/// One hoisted binding of a scope: a variable ASSIGNMENT, or a nested `function` DEFINITION. Both land in
/// the scope's variable namespace in our model — a module-body `function f(x)=…` binds a closure VALUE
/// named `f` (see [`hoist_scope`]). (OpenSCAD keeps functions in a separate namespace; collapsing them here
/// only misbehaves if a scope names a var AND a function the same, which real code doesn't.)
#[derive(Clone, Copy)]
enum HoistItem<'a> {
    Assign(&'a str, &'a Expr),
    Func(&'a str, &'a [Parameter], &'a Expr),
}

/// The hoisted binding order of a scope — its assignments AND nested `function` definitions, deduped by
/// name in FIRST-occurrence order carrying the LAST definition (OpenSCAD whole-scope, last-wins). The
/// generalization of [`hoisted_assignments`] the module-body path needs: a nested function must be bound
/// IN TEXTUAL ORDER so it captures the enclosing locals hoisted before it and a later assignment can call
/// it. PURE (no eval), so the order rules stay unit-testable. Top-level defs don't come through here —
/// they're registered globally by [`build_ctx`]; this is for module bodies / blocks / comprehension scopes.
fn hoisted_bindings<'a>(stmts: &[&'a Stmt]) -> Vec<HoistItem<'a>> {
    let mut order: Vec<HoistItem<'a>> = Vec::new();
    let mut index: BTreeMap<&'a str, usize> = BTreeMap::new();
    for stmt in flatten_blocks(stmts) {
        let (name, item) = match &stmt.kind {
            StmtKind::Assignment { name, value } => (&**name, HoistItem::Assign(name, value)),
            StmtKind::FunctionDef { name, params, body } => (
                name.as_str(),
                HoistItem::Func(name, params.as_slice(), body),
            ),
            _ => continue,
        };
        if let Some(&i) = index.get(name) {
            order[i] = item; // seen: last definition wins, first-occurrence position kept
        } else {
            index.insert(name, order.len());
            order.push(item);
        }
    }
    order
}

/// Build a NAMED-function closure `Value` from a nested `function` definition: register its params+body in
/// the closure table, capture the (partially-hoisted) `scope` as its lexical env, and stamp its name for
/// recursion (`self_name`) + `str()` rendering. The nested-def analogue of the `FunctionLiteral` eval arm.
/// Register every nested `function` in `items` into the `Ctx` closure table ONCE, returning the shared
/// letrec GROUP — the sibling list each body function carries so it can call the others regardless of
/// textual order (L.5.4). `None` when the body defines no functions (the overwhelmingly common block, so
/// the group machinery costs nothing there).
/// The fn-shaped half of a [`HoistItem`]: a nested `function f(x)=…` def, or an ASSIGNMENT whose
/// RHS is syntactically a function LITERAL (`f = function(x) …`) — upstream's shared-context
/// letrec makes those mutually visible too (AH.2.7: `chaining1 = function(x) … chaining2(x-1)`
/// with `chaining2` defined BELOW resolves at call time).
fn fn_shape<'a>(item: &HoistItem<'a>) -> Option<(&'a str, &'a [Parameter], &'a Expr)> {
    match *item {
        HoistItem::Func(name, params, body) => Some((name, params, body)),
        HoistItem::Assign(name, expr) => match &expr.kind {
            ExprKind::FunctionLiteral { params, body } => Some((name, params.as_slice(), body)),
            _ => None,
        },
    }
}

fn register_fn_group<'a>(items: &[HoistItem<'a>], ctx: &Ctx<'a>) -> Option<Rc<[value::SiblingFn]>> {
    let mut group: Vec<value::SiblingFn> = Vec::new();
    for item in items {
        if let Some((name, params, body)) = fn_shape(item) {
            let closure_id = {
                let mut closures = ctx.closures.borrow_mut();
                closures.push((params, body));
                closures.len() - 1
            };
            group.push(value::SiblingFn {
                name: Rc::from(name),
                closure_id,
                repr: crate::parser::print::function_value_repr(params, body).into(),
            });
        }
    }
    (!group.is_empty()).then(|| Rc::from(group))
}

/// Build a body-local function's [`Value::Function`] from its already-registered group entry `s`, capturing
/// `env` (the enclosing locals hoisted so far) and carrying the shared `group` (all siblings), so it resolves
/// a forward/mutual sibling call at invoke time (L.5.4).
fn nested_fn_value(
    s: &value::SiblingFn,
    env: &Scope,
    group: Option<&Rc<[value::SiblingFn]>>,
) -> Value {
    Value::Function {
        closure_id: s.closure_id,
        env: env.clone(),
        self_name: Some(Rc::clone(&s.name)),
        repr: Rc::clone(&s.repr),
        group: group.cloned(),
        bound_this: None,
    }
}

/// Evaluate an `echo`'s arguments and push the formatted `ECHO:` content onto the message log — named
/// args render `name = value`, positional just `value`, joined by `, ` (OpenSCAD's echo order). The
/// value form is the shared [`fmt::format_value`] (strings QUOTED), so it's bug-for-bug with the oracle.
fn emit_echo<'a>(
    args: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    let mut vals = Vec::with_capacity(args.len());
    for arg in args {
        vals.push(eval_with_global(&arg.value, scope, global, ctx)?);
    }
    ctx.messages
        .borrow_mut()
        .push(Message::Echo(format_echo_line(args, &vals)?));
    Ok(())
}

/// AD.5: how deep a value may nest before echo REFUSES to render it — upstream's `EchoString` seam, made
/// deterministic. OpenSCAD converts echo values to string HOST-RECURSIVELY and stack-exhausts on deep
/// nesting ("Stack exhausted while trying to convert a vector to `EchoString`" — issue4172's whole point,
/// and recursion-test-vector's eventual death rattle). Our formatter is iterative (AA.4.3) and could
/// render ANY depth — but a growing-value echo LOOP (issue4172 adds 12 levels per module call) then costs
/// O(depth) per call, quadratic total, and the message BUFFER eats gigabytes before the 100k module guard
/// is reached. Erroring at the same seam upstream does keeps the verdict AND the economics. 10k is 5× the
/// deepest legit formatted value in the corpus (the 2000-deep `deep_value` conformance case); real
/// echo output nests < 100.
const MAX_ECHO_NESTING: usize = 10_000;

/// Does `v` nest `List`s deeper than `limit`? Iterative DFS carrying depth, early-exit on the first
/// too-deep node — visits only `List` spines, so a big FLAT echo arg costs nothing extra.
fn nests_deeper_than(v: &Value, limit: usize) -> bool {
    let mut stack = vec![(v, 1usize)];
    while let Some((v, depth)) = stack.pop() {
        if let Value::List(xs) = v {
            if depth >= limit {
                return true;
            }
            stack.extend(
                xs.iter()
                    .filter(|x| matches!(x, Value::List(_)))
                    .map(|x| (x, depth + 1)),
            );
        }
    }
    false
}

/// Format an `ECHO:` line from pre-evaluated arg values — named args render `name = value`,
/// positional just `value`, joined by `, `. Shared by the statement path ([`emit_echo`]) and the
/// task path ([`Task::EchoEmit`]) so the two can't drift. Errs on a past-[`MAX_ECHO_NESTING`] value
/// (upstream's `EchoString` stack exhaust, as a deterministic bound).
fn format_echo_line(args: &[Arg], vals: &[Value]) -> crate::Result<String> {
    if vals.iter().any(|v| nests_deeper_than(v, MAX_ECHO_NESTING)) {
        return Err(crate::Error::Eval(format!(
            "echo value nests deeper than {MAX_ECHO_NESTING} levels (upstream stack-exhausts converting this to an EchoString)"
        )));
    }
    let parts: Vec<String> = args
        .iter()
        .zip(vals)
        .map(|(arg, value)| match &arg.name {
            Some(name) => format!("{name} = {}", fmt::format_value(value)),
            None => fmt::format_value(value),
        })
        .collect();
    Ok(parts.join(", "))
}

/// Evaluate an `assert`'s arguments and fail LOUD if the condition is falsy: `assert(cond)`,
/// `assert(cond, msg)`, or the named `assert(condition = …, message = …)`. The failure text is NOT
/// matched to the oracle word-for-word (an agreed non-goal); it carries the user's message when given.
fn check_assert<'a>(
    args: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    let (cond_expr, msg_expr) = split_assert_args(args);
    let cond_val = match cond_expr {
        Some(e) => Some(eval_with_global(e, scope, global, ctx)?),
        None => None,
    };
    assert_verdict(cond_expr, cond_val.as_ref(), msg_expr, scope, global, ctx)
}

/// Split an assert's condition from its message SLOT — static, no evaluation. Named
/// `condition`/`message` beat the positional slots (OpenSCAD's params are `condition`, `message`);
/// unknown named args and extras drop, as OpenSCAD arg-matching does.
fn split_assert_args(args: &[Arg]) -> (Option<&Expr>, Option<&Expr>) {
    let mut cond_expr: Option<&Expr> = None;
    let mut msg_expr: Option<&Expr> = None;
    let mut positional = 0;
    for arg in args {
        match arg.name.as_deref() {
            Some("condition") => cond_expr = Some(&arg.value),
            Some("message") => msg_expr = Some(&arg.value),
            Some(_) => {}
            None => {
                match positional {
                    0 if cond_expr.is_none() => cond_expr = Some(&arg.value),
                    1 if msg_expr.is_none() => msg_expr = Some(&arg.value),
                    _ => {}
                }
                positional += 1;
            }
        }
    }
    (cond_expr, msg_expr)
}

/// The assert's verdict half: evaluate the MESSAGE (eagerly, impure-reads rolled back — OpenSCAD
/// fires its warnings even on a PASS, verified bug-for-bug), trace, and fail LOUD on a falsy
/// condition with the pretty-printed `[assert(…)]` locator. Shared by the statement path
/// ([`check_assert`]) and the task path ([`Task::AssertCheck`]); the message eval is a documented
/// depth-1 host re-entry (recursion routed through an assert MESSAGE stays out of scope — AB.2).
fn assert_verdict<'a>(
    cond_expr: Option<&Expr>,
    cond_val: Option<&Value>,
    msg_expr: Option<&'a Expr>,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    // Evaluate the message EAGERLY (OpenSCAD does — its warnings must fire on a pass), but ROLL BACK impure_reads
    // around it: the message VALUE is discarded on a pass and eval ABORTS on a fail, so a `parent_module` read
    // in it (BOSL2's `no_children`/`req_children` name themselves via `parent_module(1)` in the message) can
    // never reach a cached geometry — yet the raw counter bump would wrongly fence the CSG memo (J.5.2a) off
    // every 98%-redundant leaf. `parent_module` emits NO console output, so suppressing its fence bit changes
    // nothing observable; message/rand-draw deltas (a warning, a `rands` draw) STAY — those ARE observable and
    // must still decline a memo whose hit would drop them.
    let message = match msg_expr {
        Some(e) => {
            let impure_before = ctx.impure_reads.get();
            let value = eval_with_global(e, scope, global, ctx)?;
            ctx.impure_reads.set(impure_before);
            Some(value)
        }
        None => None,
    };
    let passed = matches!(&cond_val, Some(c) if c.is_truthy());
    // Pretty-print the condition back to source ONLY when it's actually consumed — the trace line (off in
    // release) or a FAILURE locator. BOSL2 is assert-DENSE and its asserts overwhelmingly PASS, so building
    // this string on every passing assert with tracing off was pure churn (N.2a). `""` covers `assert()`.
    let cond_src = if trace::on() || !passed {
        cond_expr.map_or_else(String::new, crate::parser::print_expr)
    } else {
        String::new()
    };
    trace::assert(passed, &cond_src); // gated inside (like bind/ret/module) — free when the trace is off
    if passed {
        return Ok(());
    }
    let locator = format!(" [assert({cond_src})]");
    // Error::Assert (NOT Eval): OpenSCAD prints the assert ERROR but still exports the geometry built before
    // it, so the top-level geometry driver catches this to warn + halt + keep the partial (L.5.8).
    Err(crate::Error::Assert(match message {
        Some(Value::Str(s)) => format!("assertion failed: {s}{locator}"),
        Some(other) => format!("assertion failed: {}{locator}", fmt::format_value(&other)),
        None => format!("assertion failed{locator}"),
    }))
}

/// Collect user function definitions into the [`Ctx`] store (their own namespace). A pre-pass over the
/// whole program, so a call can resolve a function defined anywhere (whole-program visibility, like
/// OpenSCAD); a duplicate name — last definition wins (`BTreeMap::insert`).
fn build_ctx(program: &Program, config: Config) -> Ctx<'_> {
    let mut functions = BTreeMap::new();
    let mut modules = BTreeMap::new();
    for stmt in &program.stmts {
        match &stmt.kind {
            StmtKind::FunctionDef { name, params, body } => {
                // Home island 0 — a single-program eval is all one island, so every function's body
                // evaluates against the root global (island 0), exactly the old behavior.
                functions.insert(name.as_str(), ((params.as_slice(), body), 0usize));
            }
            StmtKind::ModuleDef { name, params, body } => {
                modules.insert(name.as_str(), (params.as_slice(), &**body));
            }
            _ => {}
        }
    }
    // A raw single-program eval (no loader) has no `use`/`include` graph → one island (the whole
    // program), used by nothing. Module resolution against island 0 is exactly the old global lookup.
    // The island's own function/assignment stores stay empty — island 0's global (constants) is the root
    // global that `run_stmts` hoists + publishes, not something built from `Island::assignments` here.
    let intrinsics = build_intrinsics(&functions);
    Ctx {
        functions,
        intrinsics,
        island_globals: RefCell::new(vec![Scope::new()]),
        islands: vec![loader::Island {
            modules,
            functions: BTreeMap::new(),
            assignments: Vec::new(),
            uses: Vec::new(),
        }],
        closures: RefCell::default(),
        messages: RefCell::default(),
        root_override: RefCell::default(),
        // No file table on the raw AST path — an import/surface here becomes a need `eval_program` then
        // rejects LOUD (a silently-empty mesh is the thing the doctrine forbids).
        files: None,
        file_needs: RefCell::default(),
        module_depth: Cell::default(),
        children_stack: RefCell::default(),
        local_modules: RefCell::default(),
        module_stack: RefCell::default(),
        rand_stream: RefCell::new(rng::RandStream::new()),
        cache: eval_cache::CacheCell::default(),
        mod_cache: mod_cache::CacheCell::default(),
        config,
        impure_reads: std::cell::Cell::new(0),
        eval_steps: std::cell::Cell::new(0),
        live_calls: std::cell::Cell::new(0),
        jit: None, // the raw-AST path (no loader) is interpreter-only; the JIT rides the loader entry
    }
}

// I.7 — Kani proof of the stack machine's pop-N discipline (docs/testing-cards.md: "push/pop
// discipline", panic-freedom on the exact loop that runs untrusted SCAD). Compiled only under
// `cargo kani`.
#[cfg(kani)]
mod proofs {
    /// The multi-value pops — `VectorSplice` / `Apply` / `Builtin` all do
    /// `values.split_off(values.len().saturating_sub(n))` — can NEVER underflow the value stack: the
    /// split index is always `<= len` (saturating_sub can't wrap below 0), so `split_off` never panics,
    /// for ANY stack depth and ANY requested arity `n`. This is the push/pop discipline's safety core.
    #[kani::proof]
    fn stack_pop_n_never_underflows() {
        let depth: usize = kani::any();
        kani::assume(depth <= 8); // bounded model; the invariant is depth-independent (saturating_sub)
        let mut values: Vec<u8> = vec![0; depth];
        let n: usize = kani::any();
        let popped = values.split_off(values.len().saturating_sub(n)); // must not panic
        assert!(popped.len() <= depth);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "unit-test helpers: unwrap/expect/panic ARE the assertions; exact float asserts are deterministic"
)]
mod tests {
    use super::{Scope, Value, build_ctx, eval_with_ctx, tagged_functions};
    use crate::parser::{StmtKind, parse};

    /// The empty-graph guard in [`tagged_functions`]: no islands → no functions (in production `islands`
    /// always has the root, so this defensive branch is only reachable here).
    #[test]
    fn tagged_functions_of_no_islands_is_empty() {
        assert!(tagged_functions(&Vec::new()).is_empty());
    }

    /// O.5.2 mechanism, direct: [`super::guard_veto`] via [`super::build_intrinsics`] — a fingerprint-matched
    /// entry still declines when a DEP drifted/is absent (the interpreted reference would route through the
    /// changed dep; the native bakes the pinned one) or when a user fn SHADOWS a builtin the reference leans
    /// on (dispatch resolves user fns first).
    #[test]
    fn dep_drift_and_builtin_shadow_veto_wiring() {
        use super::intrinsics::reference_of;
        let is_finite = reference_of("is_finite").unwrap();
        let is_nan = reference_of("is_nan").unwrap();

        let wired = |src: &str, name: &str| {
            build_ctx(&parse(src).unwrap(), crate::Config::default())
                .intrinsics
                .contains_key(name)
        };
        assert!(
            wired(&format!("{is_nan}\n{is_finite}"), "is_finite"),
            "exact dep → wires"
        );
        assert!(
            !wired(
                &format!("function is_nan(x) = false;\n{is_finite}"),
                "is_finite"
            ),
            "a DRIFTED dep must veto (the interpreted body would call the new is_nan)"
        );
        assert!(
            !wired(is_finite, "is_finite"),
            "an ABSENT dep must veto (interpreting would error where the native wouldn't)"
        );

        let last = reference_of("last").unwrap();
        assert!(wired(last, "last"), "no shadow → wires");
        assert!(
            !wired(&format!("function len(x) = 99;\n{last}"), "last"),
            "a user fn shadowing `len` must veto (the interpreted body would call it)"
        );
    }

    /// The pinned-dep chain end-to-end: `select` (deps `is_vector`/`is_range`/`is_finite`/`is_nan`) wires only when
    /// every dep is present AND verbatim; dropping one pin declines it while the others stay wired.
    #[test]
    fn select_wires_only_with_its_pinned_deps_verbatim() {
        use super::intrinsics::{pin_reference_of, reference_of};
        let full = format!(
            "{}\n{}\n{}\n{}\n{}",
            reference_of("select").unwrap(),
            pin_reference_of("is_vector").unwrap(),
            pin_reference_of("is_range").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
        );
        let full = parse(&full).unwrap();
        let ctx = build_ctx(&full, crate::Config::default());
        assert!(
            ctx.intrinsics.contains_key("select"),
            "all deps verbatim → select wires"
        );
        let sans_vector = format!(
            "{}\n{}\n{}\n{}",
            reference_of("select").unwrap(),
            pin_reference_of("is_range").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
        );
        let sans_vector = parse(&sans_vector).unwrap();
        let ctx = build_ctx(&sans_vector, crate::Config::default());
        assert!(
            !ctx.intrinsics.contains_key("select"),
            "missing is_vector → select declines"
        );
        assert!(
            ctx.intrinsics.contains_key("is_finite"),
            "the other entries keep wiring independently"
        );
    }

    /// O.5.1 mechanism, direct: [`super::arm_guarded_intrinsics`] wires a fingerprint-matched guarded entry
    /// ONLY when every guarded constant's bound value bit-matches the bake — unbound, off-by-value, and even
    /// a bit-different same-magnitude float all DECLINE.
    #[test]
    fn arm_guarded_intrinsics_checks_bound_const_bits() {
        let program =
            parse("_EPSILON = 1e-9; function _fab_poc_near0(x) = abs(x) < _EPSILON;").unwrap();
        let ctx = build_ctx(&program, crate::Config::default());
        // Nothing hoisted yet → the const is UNBOUND → declines (this is the mid-hoist state).
        assert!(super::arm_guarded_intrinsics(&ctx).is_empty());

        let bind_eps = |v: Value| {
            let mut g = Scope::new();
            g.bind("_EPSILON".to_string(), v);
            *ctx.island_globals.borrow_mut().first_mut().unwrap() = g;
        };
        bind_eps(Value::Num(1e-9));
        let armed = super::arm_guarded_intrinsics(&ctx);
        assert_eq!(
            armed.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            ["_fab_poc_near0"],
            "exact const → arms"
        );
        bind_eps(Value::Num(1e-6));
        assert!(
            super::arm_guarded_intrinsics(&ctx).is_empty(),
            "overridden const → declines"
        );
        bind_eps(Value::Num(f64::from_bits(1e-9_f64.to_bits() + 1)));
        assert!(
            super::arm_guarded_intrinsics(&ctx).is_empty(),
            "one-ulp drift → declines (bit compare, not ==)"
        );
    }

    /// Run a program through [`super::resolve_source`] (the full loader path — intrinsics wire + arm) and
    /// return its rendered console output.
    fn run_echo(src: &str) -> String {
        use std::path::Path;

        use super::loader::SourceMap;
        use super::{FileTable, Resolution, resolve_source};
        match resolve_source(
            src,
            Path::new("."),
            None,
            &SourceMap::new(),
            &FileTable::new(),
            None,
            crate::Config::default(),
        )
        .expect("resolves")
        {
            Resolution::Complete { messages, .. } => messages
                .iter()
                .map(crate::Message::render)
                .collect::<Vec<_>>()
                .join("\n"),
            other @ Resolution::Incomplete { .. } => panic!("expected Complete, got {other:?}"),
        }
    }

    /// O.6 — the named-arg rebind at intrinsic dispatch mirrors `push_call`'s slot semantics: named args
    /// fill by name (overriding positional), holes take the param's DEFAULT (evaluated in the definition
    /// base), and dropped args (unknown named / extra positional / an overwritten positional) are never
    /// EVALUATED — pinned via the deterministic seedless-rands stream, which any stray evaluation would
    /// advance. The interpreted twin (a fingerprint-missing body computing the same values) must agree.
    #[test]
    fn intrinsic_named_arg_rebind_matches_push_call_semantics() {
        let point3d = super::intrinsics::reference_of("point3d").unwrap();
        // fingerprints DIFFERENTLY (p[i+0]) but computes the same values → the interpreted twin.
        let interp = "function point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i+0]];";

        // named args + a hole taking the default
        assert_eq!(
            run_echo(&format!("{point3d}\necho(point3d(fill=7, p=[1]));")),
            "ECHO: [1, 7, 7]"
        );
        assert_eq!(
            run_echo(&format!("{point3d}\necho(point3d(p=[1]));")),
            "ECHO: [1, 0, 0]"
        );
        // named overrides positional; the overwritten positional never evaluates
        assert_eq!(
            run_echo(&format!("{point3d}\necho(point3d([9,9,9], p=[1]));")),
            "ECHO: [1, 0, 0]"
        );
        // a `$`-arg declines to the interpreter — same value either way
        assert_eq!(
            run_echo(&format!("{point3d}\necho(point3d([1], $junk=2));")),
            "ECHO: [1, 0, 0]"
        );

        // Side-effect parity with the interpreter: dropped args must not advance the rand stream. Each
        // program's echoed draw must equal the interpreted twin's.
        let cases = [
            // unknown named: BINDS + evaluates since AH.2.5 — the intrinsic declines to the
            // interpreter (an injection), so both sides draw identically.
            "x = point3d([1,2,3], junk=rands(0,1,1)); echo(rands(0,1,1));",
            "x = point3d([1,2,3], 0, rands(0,1,1)); echo(rands(0,1,1));", // extra positional: dropped
            "x = point3d(rands(0,1,1), p=[1]); echo(rands(0,1,1));", // overwritten positional: dropped
            "x = point3d(fill=rands(0,1,1)[0], p=[1,undef,3]); echo(rands(0,1,1));", // named DOES evaluate
        ];
        for body in cases {
            assert_eq!(
                run_echo(&format!("{point3d}\n{body}")),
                run_echo(&format!("{interp}\n{body}")),
                "intrinsic vs interpreted rand-stream diverged on: {body}"
            );
        }
    }

    /// O.8 mechanism, direct: the VALUE-const guard ([`super::intrinsics::Entry::consts_v`]) arms only on a
    /// bit-exact, VARIANT-exact bound value — an overridden vector, a one-ulp drift, an element-wise-equal
    /// `List` (vs the baked `NumList`), and an unbound constant all DECLINE.
    #[test]
    fn value_const_guard_arms_on_exact_bits_and_variant() {
        let program = parse("UP=[0,0,1]; function _fab_poc_isup(v) = v == UP;").unwrap();
        let ctx = build_ctx(&program, crate::Config::default());
        let armed_isup = |ctx: &super::Ctx| {
            super::arm_guarded_intrinsics(ctx)
                .iter()
                .any(|(n, _)| *n == "_fab_poc_isup")
        };
        assert!(!armed_isup(&ctx), "unbound const → declines");

        let bind_up = |v: Value| {
            let mut g = Scope::new();
            g.bind("UP".to_string(), v);
            *ctx.island_globals.borrow_mut().first_mut().unwrap() = g;
        };
        bind_up(Value::num_list(vec![0.0, 0.0, 1.0]));
        assert!(armed_isup(&ctx), "exact NumList → arms");
        bind_up(Value::num_list(vec![0.0, 1.0, 0.0]));
        assert!(!armed_isup(&ctx), "overridden vector → declines");
        bind_up(Value::num_list(vec![
            0.0,
            0.0,
            f64::from_bits(1.0_f64.to_bits() + 1),
        ]));
        assert!(!armed_isup(&ctx), "one-ulp drift → declines");
        bind_up(Value::list(vec![
            Value::Num(0.0),
            Value::Num(0.0),
            Value::Num(1.0),
        ]));
        assert!(
            !armed_isup(&ctx),
            "element-wise-equal List → declines (variant-exact)"
        );
    }

    /// O.8 end-to-end through [`super::resolve_source`]: an `UP` override must flip the echo — a
    /// stale-baked intrinsic would answer `true` for `[0,0,1]` where the interpreter's truth is `false`.
    #[test]
    fn value_const_override_declines_end_to_end() {
        let poc = "function _fab_poc_isup(v) = v == UP;\n\
                   echo(_fab_poc_isup([0,0,1]), _fab_poc_isup([0,1,0]));";
        assert_eq!(
            run_echo(&format!("UP=[0,0,1];\n{poc}")),
            "ECHO: true, false"
        );
        assert_eq!(
            run_echo(&format!("UP=[0,1,0];\n{poc}")),
            "ECHO: false, true"
        );
    }

    /// O.5.1 end-to-end through [`super::resolve_source`] (the path that ARMS): the program's `_EPSILON`
    /// decides the answer. With the stock 1e-9 the intrinsic arms and must agree with the interpreter; with
    /// a user OVERRIDE to 1e-6 the guard declines — a stale-baked intrinsic would answer `false` for 5e-7,
    /// the interpreter's `true` is the required output.
    #[test]
    fn const_override_declines_the_guarded_intrinsic_end_to_end() {
        use std::path::Path;

        use super::loader::SourceMap;
        use super::{FileTable, Resolution, resolve_source};

        let run = |src: &str| -> String {
            match resolve_source(
                src,
                Path::new("."),
                None,
                &SourceMap::new(),
                &FileTable::new(),
                None,
                crate::Config::default(),
            )
            .expect("resolves")
            {
                Resolution::Complete { messages, .. } => messages
                    .iter()
                    .map(crate::Message::render)
                    .collect::<Vec<_>>()
                    .join("\n"),
                other @ Resolution::Incomplete { .. } => panic!("expected Complete, got {other:?}"),
            }
        };
        let poc = "function _fab_poc_near0(x) = abs(x) < _EPSILON;\n\
                   echo(_fab_poc_near0(0.0000000005), _fab_poc_near0(0.0000005));";

        // Stock: 5e-10 < 1e-9 → true; 5e-7 → false. (The intrinsic ARMS here — same answer by the harness.)
        assert_eq!(
            run(&format!("_EPSILON = 1e-9;\n{poc}")),
            "ECHO: true, false"
        );
        // Override: the guard DECLINES, the interpreter answers per 1e-6 → 5e-7 is near-zero now.
        assert_eq!(run(&format!("_EPSILON = 1e-6;\n{poc}")), "ECHO: true, true");
    }

    /// SU.2: a synthetic library built from EVERY registered reference source (entries + pins) audits as
    /// 100% Matched — the matrix agrees with the dispatch gate on what "the pinned revision" means.
    #[test]
    fn intrinsic_matrix_all_matched_on_reference_library() {
        let lib = super::intrinsics::all_reference_sources().iter().fold(
            String::new(),
            |mut acc, &(_, src)| {
                acc.push_str(src);
                acc.push('\n');
                acc
            },
        );
        let rows = super::io::drive_intrinsic_matrix(&lib, std::path::Path::new("."), &[])
            .expect("audits");
        assert!(!rows.is_empty(), "registry can't be empty");
        for row in &rows {
            assert_eq!(
                row.status,
                super::IntrinsicMatrixStatus::Matched,
                "{} (pin={}) should match its own reference: {row:?}",
                row.name,
                row.pin
            );
        }
    }

    /// SU.2: drift shapes. A LAST-WINS redefinition with a different body flags `Changed` (the upstream-
    /// revised-a-function case); a reference absent from the library flags `Missing` (renamed/removed).
    #[test]
    fn intrinsic_matrix_flags_changed_and_missing() {
        use std::fmt::Write as _;

        let refs = super::intrinsics::all_reference_sources();
        let (dropped, _) = refs[0];
        let (revised, revised_src) = refs[1];
        let mut lib = refs
            .iter()
            .skip(1) // refs[0] never defined → Missing
            .fold(String::new(), |mut acc, &(_, src)| {
                acc.push_str(src);
                acc.push('\n');
                acc
            });
        // Same name, structurally different body, defined LAST → last-wins → Changed.
        let _ = writeln!(lib, "function {revised}() = \"sustainment-drift\";");
        let rows = super::io::drive_intrinsic_matrix(&lib, std::path::Path::new("."), &[])
            .expect("audits");
        let status = |name: &str| {
            rows.iter()
                .find(|r| r.name == name)
                .expect("row exists")
                .status
        };
        assert_eq!(status(dropped), super::IntrinsicMatrixStatus::Missing);
        assert_eq!(status(revised), super::IntrinsicMatrixStatus::Changed);
        // The perturbations are targeted: everything else still matches.
        let off: Vec<_> = rows
            .iter()
            .filter(|r| {
                r.status != super::IntrinsicMatrixStatus::Matched
                    && r.name != dropped
                    && r.name != revised
            })
            .collect();
        assert!(off.is_empty(), "collateral drift: {off:?}");
        let _ = revised_src;
    }

    /// SU.2 strictness: an unresolvable `include` must ERROR the audit, not report everything Missing
    /// against a half-loaded tree (the mistyped-root failure mode).
    #[test]
    fn intrinsic_matrix_loud_on_missing_library() {
        let err = super::io::drive_intrinsic_matrix(
            "include <no-such-dir/std.scad>\n",
            std::path::Path::new("."),
            &[],
        )
        .expect_err("must refuse a broken tree");
        assert!(
            format!("{err}").contains("complete library tree"),
            "unexpected error: {err}"
        );
    }

    /// The PURE inner step ([`super::resolve_source`], M.4): with empty source tables it surfaces NEEDS
    /// rather than doing IO — a `use`/`include` reference the source table lacks comes back as a `Scad`
    /// need (before any eval), and an `import`/`surface` a `File` need (after the graph closes). The [`io`]
    /// shell + integration tests exercise the fulfilling loop; here we pin that the core NAMES the right
    /// needs and CLOSES when the tables hold them.
    #[test]
    fn resolve_source_surfaces_needs_then_closes() {
        use std::path::Path;

        use super::loader::SourceMap;
        use super::{FileTable, Resolution, SourceNeed, resolve_source};

        let here = Path::new(".");
        let no_scad = SourceMap::new();
        let no_files = FileTable::new();
        let scad_need = |raw: &str| SourceNeed::Scad {
            from_dir: here.to_path_buf(),
            raw: raw.to_string(),
        };
        let file_need = |raw: &str| SourceNeed::File {
            raw: raw.to_string(),
        };

        // Phase 1: an unloaded `use` surfaces a Scad need — BEFORE eval (the program can't run yet).
        let scad = resolve_source(
            "use <lib.scad>\ncube(1);",
            here,
            None,
            &no_scad,
            &no_files,
            None,
            crate::Config::default(),
        )
        .expect("resolves");
        assert!(
            matches!(&scad, Resolution::Incomplete { needs } if needs == &[scad_need("lib.scad")]),
            "expected a Scad need, got {scad:?}"
        );

        // Phase 2: no `use`, so the graph closes; imports with no mesh surface File needs. Two imports
        // surface in ONE round (placeholder-continue) — deduped + sorted by the BTreeSet.
        let files_wanted = resolve_source(
            "import(\"a.stl\"); import(\"b.stl\"); import(\"a.stl\");",
            here,
            None,
            &no_scad,
            &no_files,
            None,
            crate::Config::default(),
        )
        .expect("resolves");
        assert!(
            matches!(&files_wanted, Resolution::Incomplete { needs }
                if needs == &[file_need("a.stl"), file_need("b.stl")]),
            "expected two File needs in one round, got {files_wanted:?}"
        );

        // Supply the mesh → the run CLOSES (Complete). An empty placeholder mesh stands in for a read STL.
        let mut have = FileTable::new();
        have.insert(
            "a.stl".to_string(),
            super::Imported::Mesh(crate::Mesh::new()),
        );
        let closed = resolve_source(
            "import(\"a.stl\");",
            here,
            None,
            &no_scad,
            &have,
            None,
            crate::Config::default(),
        )
        .expect("resolves");
        assert!(
            matches!(&closed, Resolution::Complete { .. }),
            "expected Complete, got {closed:?}"
        );
    }

    /// Evaluate a program's assignments in order (binding each), returning the LAST assignment's value
    /// — with the program's function store in scope. The end-to-end call test harness.
    fn eval_last(src: &str) -> Value {
        let prog = parse(src).expect("parses");
        let ctx = build_ctx(&prog, crate::Config::default());
        let mut scope = Scope::new();
        let mut last = Value::Undef;
        for stmt in &prog.stmts {
            if let StmtKind::Assignment { name, value } = &stmt.kind {
                // Publish the current scope as island 0's global so a function call sees the top-level
                // bindings (production drives this through `run_stmts`; this helper hand-rolls the loop).
                if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(0) {
                    *slot = scope.clone();
                }
                last = eval_with_ctx(value, &scope, &ctx).expect("evaluates");
                scope.bind(name.clone(), last.clone());
            }
        }
        last
    }

    /// Like [`eval_last`] but with an explicit Q.5 `eval_budget` and the eval `Result` PROPAGATED (not
    /// `expect`ed), so a budget-exceeded error is observable. The budget counter lives on the fresh `Ctx`, so
    /// each call starts at 0 — a given `(src, budget)` is reproducible.
    fn eval_budgeted(src: &str, budget: Option<u64>) -> crate::Result<Value> {
        let prog = parse(src).expect("parses");
        let cfg = crate::Config {
            eval_budget: budget,
            ..crate::Config::default()
        };
        let ctx = build_ctx(&prog, cfg);
        let mut scope = Scope::new();
        let mut last = Value::Undef;
        for stmt in &prog.stmts {
            if let StmtKind::Assignment { name, value } = &stmt.kind {
                if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(0) {
                    *slot = scope.clone();
                }
                last = eval_with_ctx(value, &scope, &ctx)?;
                scope.bind(name.clone(), last.clone());
            }
        }
        Ok(last)
    }

    /// A budget of `None` (the default) is UNLIMITED — a big-but-bounded comprehension evaluates fine, exactly
    /// as it does today. The `DoS` guard must never touch the trusted path.
    #[test]
    fn budget_none_is_unlimited() {
        let v = eval_budgeted("x = len([for (i = [0:200000]) i * 2]);", None).expect("no budget");
        assert_eq!(v, Value::Num(200_001.0));
    }

    /// The eval-trophy class (TROPHIES.md): a huge-range comprehension builds a RANGE_MAX-capped
    /// 10M-element list (>10s / lots of RAM) — but under a budget it's rejected UP FRONT
    /// (`charge_iterable` charges the ~1e7 count before `iter_values` even allocates), LOUD, not a
    /// hang. The range sits BELOW `RANGE_TOO_MANY` on purpose: the original `[0:9e9]` trophy input is
    /// now AD.3's warn-and-skip (zero iterations, no budget needed) — see the too-many tests.
    #[test]
    fn budget_stops_the_range_comprehension_trophy() {
        let err = eval_budgeted("x = [for (i = [0:2e9]) i];", Some(1_000)).unwrap_err();
        match err.root() {
            crate::Error::Eval(m) => assert!(m.contains("budget exceeded"), "got {m}"),
            other => panic!("expected Error::Eval, got {other:?}"),
        }
    }

    /// The class a PER-LOOP cap (`RANGE_MAX`) misses: nested comprehensions each under the cap but MULTIPLYING
    /// past it. The global counter bounds the TOTAL, so the product trips even though neither loop alone would.
    #[test]
    fn budget_stops_nested_comprehension() {
        let err = eval_budgeted(
            "x = [for (i = [0:1000000]) for (j = [0:1000000]) 0];",
            Some(3_000_000),
        )
        .unwrap_err();
        assert!(matches!(err.root(), crate::Error::Eval(_)), "got {err:?}");
    }

    /// `each <huge range>` splices in bulk with no per-element eval — charged up front like a `for`, so it's
    /// bounded too (the second charge site). Below `RANGE_TOO_MANY`, same as the trophy test.
    #[test]
    fn budget_stops_each_splice() {
        let err = eval_budgeted("x = [each [0:2e9]];", Some(1_000)).unwrap_err();
        assert!(matches!(err.root(), crate::Error::Eval(_)), "got {err:?}");
    }

    /// A program WITHIN budget yields exactly the unbounded result — the bound only ever converts a
    /// would-be-huge success into an error, never perturbs a within-budget value.
    #[test]
    fn budget_within_bound_matches_unbounded() {
        let src = "x = 2 + 3 * 4 - 1;";
        let bounded = eval_budgeted(src, Some(10_000)).expect("within budget");
        let unbounded = eval_budgeted(src, None).expect("unbounded");
        assert_eq!(bounded, unbounded);
        assert_eq!(bounded, Value::Num(13.0));
    }

    /// Deterministic: the SAME `(program, budget)` fails identically every run — the count is eval-steps, not
    /// wall-time, so the error message (which carries the step total) is byte-identical across runs.
    #[test]
    fn budget_failure_is_reproducible() {
        let src = "x = [for (i = [0:1000000]) for (j = [0:1000000]) 0];";
        let a = eval_budgeted(src, Some(2_500_000)).unwrap_err();
        let b = eval_budgeted(src, Some(2_500_000)).unwrap_err();
        assert_eq!(format!("{a}"), format!("{b}"));
    }

    /// R.1.1 — the perf success-function metric: `evaluate_geometry_metered` returns a deterministic
    /// eval-COST that (a) is MONOTONE (heavier interpreter work costs strictly more), (b) is REPRODUCIBLE
    /// (same program → same cost), and (c) is BUDGET-BOUNDED (a pathological input caps at the budget, so it
    /// ranks at the TOP of the worst-case list instead of hanging the measurement).
    #[test]
    fn metered_cost_is_monotone_deterministic_and_bounded() {
        let cost = |src: &str, budget: u64| {
            let prog = parse(src).expect("parses");
            super::evaluate_geometry_metered(&prog, budget).1
        };
        // Monotone: a 10k-element comprehension costs strictly more than a 100-element one.
        let small = cost("x = len([for (i = [0:100]) i]);", 50_000_000);
        let big = cost("x = len([for (i = [0:10000]) i]);", 50_000_000);
        assert!(
            big > small,
            "bigger comprehension must cost more: {big} vs {small}"
        );
        // Deterministic: same program, same cost.
        assert_eq!(big, cost("x = len([for (i = [0:10000]) i]);", 50_000_000));
        // Bounded: a past-budget program caps at ~budget — above any completing program's cost, so it sorts
        // to the top of the ranking rather than running away. (Below `RANGE_TOO_MANY` on purpose: a
        // `[0:9e9]` range is AD.3's warn-and-skip now — zero iterations, nothing to meter.)
        let capped = cost("x = [for (i = [0:2e9]) i];", 10_000);
        assert!(
            capped >= 10_000 && capped > big,
            "budget-hit caps high (worst-case rank): {capped}"
        );
    }

    /// The `set -x` trace (`super::trace`), forced on so its output paths + the evaluator's hooks all run.
    /// The ONLY test that touches the process-global force flag — kept to one test so nothing races on it
    /// (other tests may briefly see the trace on, but the emit is stderr-only and never alters a result).
    /// Direct calls cover the emit branches; the eval calls cover the `TraceReturn` push/handler + the
    /// `check_assert` trace. No captured output to assert — this proves the debug paths don't panic.
    #[test]
    fn trace_hooks_and_emit_paths() {
        super::trace::set_enabled(true);
        // emit branches for bind/ret/module (the eval hooks below cover ret + assert, but not these)
        super::trace::bind('=', "x", &Value::Num(1.0));
        super::trace::ret("f", &Value::Undef);
        super::trace::module(1, "cuboid");
        // eval hooks: a user-fn return + a builtin return each push/fire TraceReturn; the assert traces
        assert_eq!(
            eval_last("function f(x) = x + 1; y = f(2);"),
            Value::Num(3.0)
        );
        assert_eq!(eval_last("y = max(1, 2, 3);"), Value::Num(3.0)); // builtin, positional args
        assert_eq!(eval_last("y = max(2, 3, extra = 1);"), Value::Num(3.0)); // + a NAMED arg (traced)
        assert_eq!(eval_last("y = assert(true) 5;"), Value::Num(5.0));
        assert_eq!(eval_last("y = $unset;"), Value::Undef); // unbound $-special → dev trace line
        super::trace::set_enabled(false);
    }

    #[test]
    fn positional_named_and_default_args() {
        assert_eq!(
            eval_last("function f(x) = x + 1; y = f(2);"),
            Value::Num(3.0)
        );
        assert_eq!(
            eval_last("function f(x, y = 10) = x + y; a = f(5);"),
            Value::Num(15.0)
        ); // default
        assert_eq!(
            eval_last("function f(x, y = 10) = x + y; a = f(5, 20);"),
            Value::Num(25.0)
        ); // override
        assert_eq!(
            eval_last("function f(a, b) = a - b; y = f(b = 1, a = 10);"),
            Value::Num(9.0)
        ); // named, reordered
        assert_eq!(eval_last("function f(x, y) = y; a = f(1);"), Value::Undef); // unfilled, no default → undef
        assert_eq!(
            eval_last("function f(x) = x; y = f(1, 2, 3);"),
            Value::Num(1.0)
        ); // extra positional dropped
        assert_eq!(
            eval_last("function f(x) = x; y = f(x = 1, z = 9);"),
            Value::Num(1.0)
        ); // unknown named dropped
    }

    /// A mock [`NumericJit`] that "compiles" `sq(x) = x*x` — but returns `x*x + 1000`, a WRONG value on
    /// purpose. A real intrinsic/JIT is bit-identical (unobservable); the marker makes the mock's firing
    /// VISIBLE, so a `1000+`-shifted result proves the call took the JIT path, and a plain result proves
    /// it fell back to the interpreter. Any other function/arity → `None` (defer to the interpreter).
    struct MarkerJit;
    impl super::NumericJit for MarkerJit {
        fn call_numeric(
            &self,
            name: &str,
            args: &[Value],
            _rand: *mut core::ffi::c_void,
        ) -> Option<super::JitOutcome> {
            // Scalar `sq(x)` only — a NumList (or any non-Num) arg declines here, so the interpreter runs the
            // real body (the rung-B mock stays scalar; the real registry handles vector shapes). The mock
            // never draws, so it ignores the woven RandStream pointer.
            match (name, args) {
                ("sq", [Value::Num(x)]) => Some(super::JitOutcome::Num(x * x + 1000.0)),
                _ => None,
            }
        }
    }

    /// [`eval_last`] with a numeric-JIT hook injected into the ctx.
    fn eval_last_jit(src: &str, jit: Box<dyn super::NumericJit>) -> Value {
        let prog = parse(src).expect("parses");
        let mut ctx = build_ctx(&prog, crate::Config::default());
        ctx.jit = Some(jit);
        let mut scope = Scope::new();
        let mut last = Value::Undef;
        for stmt in &prog.stmts {
            if let StmtKind::Assignment { name, value } = &stmt.kind {
                if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(0) {
                    *slot = scope.clone();
                }
                last = eval_with_ctx(value, &scope, &ctx).expect("evaluates");
                scope.bind(name.clone(), last.clone());
            }
        }
        last
    }

    #[test]
    fn numeric_jit_dispatches_eligible_calls_and_falls_back_otherwise() {
        // (1) all-positional + all-Num → the JIT fires: 5*5 + 1000 marker.
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq(5);", Box::new(MarkerJit)),
            Value::Num(1025.0),
            "an eligible numeric call takes the JIT path"
        );
        // (2) a NON-number arg (a vector) → the mock declines this shape → interpreter runs x*x = dot = 5.
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq([1,2]);", Box::new(MarkerJit)),
            Value::Num(5.0),
            "a vector arg the mock doesn't handle falls back to the interpreted body (no marker)"
        );
        // (3) a NAMED arg → JIT-eligible since the P.1.4 recut (task #66): `push_call` binds the slot by
        // name and the hook sees the same param-order `vals` as a positional spelling — the marker fires.
        // (BOSL2 calls named everywhere; this was the whole `offered 0` finding.)
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq(x = 5);", Box::new(MarkerJit)),
            Value::Num(1025.0),
            "a named-arg call takes the JIT path (rebind at push_call, same slot values)"
        );
        // (3b) a `$`-ARG call stays interpreted — the one real arity hazard: dollars append to `names`
        // past the params, so `push_call` clears the hint (the load-bearing half of the old gate).
        assert_eq!(
            eval_last_jit(
                "function sq(x) = x*x; y = sq(5, $fn = 16);",
                Box::new(MarkerJit)
            ),
            Value::Num(25.0),
            "a $-arg call falls back to the interpreter (no marker)"
        );
        // (4) a function the registry doesn't know → call_numeric returns None → interpreter runs → 27.
        assert_eq!(
            eval_last_jit(
                "function cube(x) = x*x*x; y = cube(3);",
                Box::new(MarkerJit)
            ),
            Value::Num(27.0),
            "a registry miss falls back to the interpreter"
        );
        // (5) no hook at all (the wasm/raw path) → everything interprets → 25.
        assert_eq!(
            eval_last("function sq(x) = x*x; y = sq(5);"),
            Value::Num(25.0)
        );
    }

    #[test]
    fn functions_are_lexically_scoped() {
        assert_eq!(
            eval_last("g = 7; function f() = g; y = f();"),
            Value::Num(7.0)
        ); // sees the global
        // a caller's LOCAL does NOT leak into the callee (lexical, not dynamic): inner sees no `x`.
        assert_eq!(
            eval_last("function inner() = x; function outer(x) = inner(); y = outer(99);"),
            Value::Undef
        );
    }

    #[test]
    fn recursion_and_mutual_recursion() {
        assert_eq!(
            eval_last("function fac(n) = n <= 1 ? 1 : n * fac(n - 1); y = fac(5);"),
            Value::Num(120.0)
        );
        let mutual = "function even(n) = n == 0 ? true : odd(n - 1); \
                      function odd(n) = n == 0 ? false : even(n - 1); \
                      y = even(10);";
        assert_eq!(eval_last(mutual), Value::Bool(true));
    }

    #[test]
    fn closures_capture_their_env_and_are_higher_order() {
        // a closure CAPTURES the scope at its definition (k = 100 is closed over).
        assert_eq!(
            eval_last("k = 100; g = function(x) x + k; y = g(1);"),
            Value::Num(101.0)
        );
        // a closure bound to a variable is called through the variable (the CallValue path).
        assert_eq!(
            eval_last("g = function(x) x * 2; y = g(21);"),
            Value::Num(42.0)
        );
        // higher-order: pass a closure as an argument, call it inside.
        assert_eq!(
            eval_last(
                "function apply(f, x) = f(x); double = function(n) n * 2; y = apply(double, 7);"
            ),
            Value::Num(14.0)
        );
        // calling a NON-function value → undef (not an error).
        assert_eq!(eval_last("g = 5; y = g(1);"), Value::Undef);
    }

    #[test]
    fn dollar_vars_are_dynamically_scoped() {
        // a $-arg injects into the call scope (per-call override), visible in the body.
        assert_eq!(
            eval_last("function f() = $fn; y = f($fn = 8);"),
            Value::Num(8.0)
        );
        // with no override, the callee sees the CALLER's reaching $-context (here the root $fn = 0).
        assert_eq!(eval_last("function f() = $fn; y = f();"), Value::Num(0.0));
        // DOWN the call tree: outer's injected $fn propagates to inner (dynamic, not lexical).
        assert_eq!(
            eval_last("function inner() = $fn; function outer() = inner(); y = outer($fn = 8);"),
            Value::Num(8.0)
        );
        // a nested per-call override WINS over the inherited $-context.
        assert_eq!(
            eval_last(
                "function inner() = $fn; function outer() = inner($fn = 3); y = outer($fn = 8);"
            ),
            Value::Num(3.0)
        );
    }

    #[test]
    fn viewport_specials_are_seeded() {
        // L.5.3 — the camera specials resolve to OpenSCAD's no-`--camera` defaults, not `undef`, so a model
        // that reads them as a geometry input (BOSL2's orientations.scad) gets a finite number. `$vpr[2]` is
        // the one orientations.scad actually bakes into a rotation; the rest just need to be present + finite.
        assert_eq!(eval_last("x = $vpr[2];"), Value::Num(25.0));
        assert_eq!(eval_last("x = $vpr[0];"), Value::Num(55.0));
        assert_eq!(
            eval_last("x = $vpt;"),
            Value::NumList([0.0, 0.0, 0.0].as_slice().into())
        );
        assert_eq!(eval_last("x = $vpd;"), Value::Num(140.0));
        assert_eq!(eval_last("x = $vpf;"), Value::Num(22.5));
        // and they're dynamically scoped like every other $-var (overridable per call).
        assert_eq!(
            eval_last("function f() = $vpd; y = f($vpd = 500);"),
            Value::Num(500.0)
        );
    }

    #[test]
    fn deep_non_tail_recursion_is_heap_bounded() {
        // The corner_brace-class proof: 100k-deep NON-tail recursion — each level parks a pending `+`
        // on the stack — would blow a recursive tree-walker's HOST stack. On the explicit stack it's
        // just heap. sum(n) = n(n+1)/2, so sum(100000) = 5000050000 (exact in f64).
        let deep = "function sum(n) = n <= 0 ? 0 : n + sum(n - 1); y = sum(100000);";
        assert_eq!(eval_last(deep), Value::Num(5_000_050_000.0));
    }

    #[test]
    fn hoisted_assignments_dedup_first_occurrence_last_expr() {
        // The PURE override resolver: `a = 1; b = 2; a = 3;` → order [a, b] (FIRST-occurrence position),
        // and a carries the LAST expr (3, not 1). This is the whole rule the run_stmts hoist rides on.
        use crate::parser::{ExprKind, Stmt};
        let prog = parse("a = 1; b = 2; a = 3;").expect("parses");
        let stmts: Vec<&Stmt> = prog.stmts.iter().collect();
        let order = super::hoisted_assignments(&stmts);
        assert_eq!(
            order.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(matches!(order[0].1.kind, ExprKind::Num(n) if n == 3.0)); // a's expr is the last (3)
    }
}
