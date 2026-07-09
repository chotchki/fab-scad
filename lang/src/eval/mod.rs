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
mod eval_cache;
mod fmt;
mod fragments;
mod geo;
mod geo2d;
mod geo_drop;
mod fnprofile;
mod geo_stack;
mod geometry;
mod intrinsics;
pub(crate) mod io;
mod loader;
mod message;
mod module;
mod text;
mod rng;
mod ops;
mod redundancy;
mod scope;
mod trace;
mod trig;
mod value;

pub use fragments::fragments;
pub use geo::GeoNode;
pub use geo2d::{Contour, ExtrudeKind, Geo, Join2D, Shape2D};
pub use message::{Evaluation, Message};
pub use scope::Scope;
pub use value::{RANGE_MAX, RangeIter, Value, range_iter, range_len};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::collections::{BTreeMap, BTreeSet};

use crate::Mesh;
use crate::geom::{Affine, Affine2};
use crate::parser::{
    Arg, BinOp, Expr, ExprKind, Parameter, Program, Stmt, StmtKind, UnOp,
};

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

/// A desktop-only numeric JIT hook (P.1.2). The interpreter offers a user-function call to this BEFORE
/// interpreting the body, but ONLY when the call is all-positional, arity-exact, and every argument
/// evaluated to a number. If a compiled version of `name` with matching arity exists, the impl returns
/// its `f64` result; otherwise `None` and the interpreter runs the real body.
///
/// Defined here so the eval loop can dispatch to it, but the crate stays wasm-clean: the trait carries no
/// Cranelift, and the native `fab-jit` crate implements it over its compiled `JitRegistry`. The contract
/// is `fast == JIT` — a `Some(r)` MUST be bit-identical to interpreting the body (the JIT crate's
/// differential proves it), so routing a call here can only change SPEED, never the result. wasm builds
/// (which can't JIT in-sandbox) simply leave [`Ctx::jit`] `None` and interpret everything.
pub trait NumericJit {
    /// The compiled result of `name(args)` if one is registered for this exact arity, else `None`.
    fn call_numeric(&self, name: &str, args: &[f64]) -> Option<f64>;
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
    /// Monotone count of IMPURE READS a call's subtree performed — currently only `parent_module`, which
    /// reads the module-instantiation stack (state NOT in the cache key + no message/rand delta to betray it).
    /// The purity fence snapshots this before/after a call and DECLINES memoization if it moved, so any call
    /// that (transitively) reads `parent_module` re-runs every time — closing the one wrong-hit class the
    /// message/rand fence can't see. Transitive for free: a nested read bumps it, the outer call sees the delta.
    impure_reads: std::cell::Cell<u64>,
    /// The desktop numeric-JIT hook (P.1.2), or `None` (wasm, or a program with nothing compiled). When
    /// present, an eligible user-function call ([`Task::Apply`] with a `jit` name + all-`Num` args) is
    /// offered to it before the body is interpreted; a `Some` result is bit-identical to interpreting, so
    /// this only ever changes speed. Injected by the native shell at the eval entry; `None` everywhere the
    /// interpreter is the whole story (raw-AST eval, wasm).
    jit: Option<&'a dyn NumericJit>,
}

/// One active module call's children context: the call-site child statements (borrowed from the AST) +
/// the CALLER's scope AND module ISLAND they evaluate in (OpenSCAD renders `children()` in the
/// instantiation context — same lexical scope AND same module-resolution scope as the call site, I.9.5).
struct ChildrenFrame<'a> {
    /// The call-site children WITH lone-`;` empties filtered out — a `StmtKind::Empty` is not a child in
    /// OpenSCAD (it neither counts toward `$children` nor is reachable via `children(i)`), so keeping it
    /// here would misalign both. The filtered list is what BOSL2's `attachable(){ shape; union(){}; }`
    /// needs to see as exactly 2 children (the terminating `;` after the empty union is not a third).
    stmts: Vec<&'a Stmt>,
    scope: Scope,
    island: usize,
}

impl<'a> Ctx<'a> {
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

/// One step on the evaluator's explicit work-stack. Each `Eval` carries the [`Scope`] it evaluates
/// in (an `Rc<Frame>` clone — cheap), so a call's body can evaluate in the callee's scope while the
/// caller's continuation waits on the same stack (I.2.3). Value-combining tasks need no scope.
enum Task<'a> {
    /// Evaluate this expression in this scope, pushing its result onto the value stack.
    Eval(&'a Expr, Scope),
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
        /// The function's NAME when this call is numeric-JIT-eligible (all-positional, exact arity), else
        /// `None` (a closure, a defaulted/named/variadic call). When `Some` AND every evaluated arg is a
        /// `Num`, [`Ctx::jit`] gets first refusal before the body is interpreted (P.1.2). Only a shape
        /// hint — the JIT still checks its own registry and the runtime `all-Num` guard.
        jit: Option<&'a str>,
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
    Intrinsic { func: intrinsics::Intrinsic, nargs: usize },
    /// Pop the just-evaluated binding value, bind it as `name` in a child of `scope`, then either
    /// evaluate the next `let` binding in that scope or (no bindings left) evaluate `body`. `let`
    /// bindings are SEQUENTIAL — a later one sees the earlier ones.
    LetStep {
        name: Rc<str>,
        rest: &'a [Arg],
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
    /// N.2c eval-memo: peek the top value (a memoizable call's just-produced result — like [`TraceReturn`],
    /// pushed below the body so it fires the instant the result lands) and, IF the call's subtree left no
    /// observable side effect (the `snap` counters are unmoved), store it under `key`. NEVER a `geo_stack`
    /// cleanup task — it must not fire on the error path (an errored `?` abandons the whole task stack, so an
    /// errored call is structurally uncacheable). Absent when the cache is off.
    CacheStore { key: eval_cache::Key, snap: PuritySnap },
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
        match task {
            Task::Eval(e, s) => eval_node(e, &s, &global, ctx, &mut tasks, &mut values)?,
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
                jit,
            } => {
                let vals = values.split_off(values.len().saturating_sub(names.len()));
                // Numeric-JIT fast path (P.1.2): a compiled numeric function, offered the call BEFORE any
                // interpretation, IFF this call is JIT-eligible (`jit` name set at dispatch) AND every
                // evaluated arg is a plain `Num` (the compiled body is all-`f64`; a vector arg — BOSL2's
                // numeric fns are often scalar-or-vector polymorphic — falls through to the interpreter).
                // A `Some(r)` is bit-identical to interpreting `body`, so this is pure speed. The registry
                // decides membership + arity (returns `None` to defer); no wasted work when the hook is off.
                if let (Some(name), Some(j)) = (jit, ctx.jit)
                    && let Some(nums) = all_nums(&vals)
                    && let Some(r) = j.call_numeric(name, &nums)
                {
                    values.push(Value::Num(r));
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
                // Gate the cache: enabled (opt-in) and args small enough to key cheaply (arg-cap — keeps a
                // 300k-element `gaussian_rands` comprehension from paying a giant per-call key hash).
                let store = if eval_cache::enabled() && eval_cache::worth_caching(&vals) {
                    let key = eval_cache::Key::new(body, &base, &vals, &caller);
                    let hit = ctx.cache.borrow_mut().get(&key);
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
                    None
                };
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
                    Value::Function { closure_id, env, self_name, .. } => {
                        let (params, body) = ctx.closures.borrow()[*closure_id];
                        // a closure's body is lexically scoped to its captured env, not the caller's. If it
                        // was defined with a name, re-inject NAME→itself so it can recurse (letrec) — our
                        // COW frames can't self-reference at capture time, so we do it here, where we hold
                        // the closure value. Every recursive call re-injects, so depth is unbounded.
                        let base = match self_name {
                            Some(name) => {
                                let mut b = env.child();
                                b.bind(Rc::clone(name), callee.clone());
                                b
                            }
                            None => env.clone(),
                        };
                        // A closure has no static name to look up in the JIT registry → never JIT-eligible.
                        push_call(params, body, args, &caller, &base, None, &mut tasks);
                    }
                    _ => values.push(Value::Undef), // calling a non-function → undef
                }
            }
            Task::TraceReturn { name } => {
                if let Some(v) = values.last() {
                    trace::ret(name, v);
                }
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
                name,
                rest,
                body,
                scope,
            } => {
                let value = name_closure(values.pop().unwrap_or(Value::Undef), &name);
                trace::bind('l', &name, &value);
                let mut inner = scope.child();
                inner.bind(name, value);
                match rest.split_first() {
                    Some((next, remaining)) => {
                        tasks.push(Task::LetStep {
                            name: next.name.clone().unwrap_or_else(|| Rc::from("_")),
                            rest: remaining,
                            body,
                            scope: inner.clone(),
                        });
                        tasks.push(Task::Eval(&next.value, inner));
                    }
                    None => tasks.push(Task::Eval(body, inner)),
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
    global: &Scope,
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
            });
        }
        ExprKind::Let { bindings, body } => match bindings.split_first() {
            Some((first, rest)) => {
                tasks.push(Task::LetStep {
                    name: first.name.clone().unwrap_or_else(|| Rc::from("_")),
                    rest,
                    body,
                    scope: scope.clone(),
                });
                tasks.push(Task::Eval(&first.value, scope.clone()));
            }
            None => tasks.push(Task::Eval(body, scope.clone())), // `let() body` → just the body
        },
        ExprKind::Echo { args, body } => {
            // `echo(args) body?` — emit the ECHO line (side effect), then yield `body` (or undef). The
            // args + body sub-evaluate off the stack (bounded, like comprehensions); echo is rare + cold.
            emit_echo(args, scope, global, ctx)?;
            let value = match body {
                Some(b) => eval_with_global(b, scope, global, ctx)?,
                None => Value::Undef,
            };
            values.push(value);
        }
        ExprKind::Assert { args, body } => {
            // `assert(cond, msg?) body?` — LOUD on a falsy condition, else yield `body` (or undef).
            check_assert(args, scope, global, ctx)?;
            let value = match body {
                Some(b) => eval_with_global(b, scope, global, ctx)?,
                None => Value::Undef,
            };
            values.push(value);
        }
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => {
            // a comprehension element evaluates to its CONTRIBUTION list (spliced by the enclosing
            // VectorSplice). Only reached as a vector element (parser invariant).
            let contribution = eval_comprehension(e, scope, global, ctx)?;
            values.push(build_vector(contribution));
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
    let result = if name == "rands" {
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
/// identifier callee is a builtin or genuinely unknown → LOUD (I.4); otherwise the callee is a value —
/// evaluate it and apply it (a closure in a variable, or `(expr)(args)`).
fn dispatch_call<'a>(
    callee: &'a Expr,
    args: &'a [Arg],
    scope: &Scope,
    ctx: &Ctx<'a>,
    tasks: &mut Vec<Task<'a>>,
) -> crate::Result<()> {
    if let ExprKind::Ident(name) = &callee.kind {
        // resolution order (OpenSCAD): a user function may shadow a builtin.
        if let Some(&((params, body), home)) = ctx.functions.get(name.as_str()) {
            // O.1: a registered intrinsic replaces the interpreted body — but ONLY for an all-positional
            // call (v1 ABI: the native fn takes a flat positional slice, so a named-arg call falls through
            // to the interpreter below rather than needing post-eval rebinding). The fingerprint match that
            // authorized this intrinsic happened once at `build_intrinsics`; here it's a name lookup.
            if let Some(&func) = ctx.intrinsics.get(name.as_str())
                && args.iter().all(|a| a.name.is_none())
            {
                tasks.push(Task::Intrinsic { func, nargs: args.len() });
                for arg in args.iter().rev() {
                    tasks.push(Task::Eval(&arg.value, scope.clone()));
                }
                return Ok(());
            }
            // A call-path EVENT, not a span: the call's body evaluates across later loop iterations on
            // the explicit stack (no host recursion), so its subtree isn't scope-bounded here — the
            // event marks WHICH function was entered, the enclosing `eval_program` span times the whole.
            tracing::trace!(function = name.as_str(), "call");
            fnprofile::record_fn(name.as_str()); // dev probe (FAB_PROFILE_FNS): per-name call counts
            if trace::on() {
                tasks.push(Task::TraceReturn { name }); // fires when the body's value lands (peek-only)
            }
            // The body's lexical base is the function's HOME ISLAND global (its own file's constants), NOT
            // the caller's `global` — the use-scope fix. For a root-defined function home is 0 (the root
            // global), so this is a no-op there; for a `use`d function it swaps in the library's constants.
            let base = ctx.island_globals.borrow()[home].clone();
            // JIT-eligible when a hook is present AND the call is all-positional (no named/`$`-args): then
            // `names.len()` equals the compiled arity, so `Task::Apply`'s all-`Num` guard can offer it to
            // the JIT. A `None` hook (wasm / raw-AST) or a named call passes the name as `None` → interpret.
            let jit = (ctx.jit.is_some() && args.iter().all(|a| a.name.is_none()))
                .then_some(name.as_str());
            push_call(params, body, args, scope, &base, jit, tasks);
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
            // not a user fn, not a builtin, not a bound function-value → a missing builtin or a typo.
            // LOUD for now (catches missing builtins); OpenSCAD's warn-and-undef is I.5. Naming the
            // symbol is what makes the corpus's "unknown function" cluster a per-symbol worklist (L.2).
            return Err(crate::Error::Unknown(format!("function `{name}`")));
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
/// The `f64`s of `vals` iff EVERY value is a plain `Num`, else `None` — the numeric-JIT type guard. A
/// single non-`Num` (a vector, `undef`, a string) means the compiled all-`f64` function doesn't apply, so
/// the call falls back to the interpreted body. Allocates a small `Vec` (arity-sized) per eligible call;
/// a stack buffer for the common low-arity case is a later micro-opt.
fn all_nums(vals: &[Value]) -> Option<Vec<f64>> {
    vals.iter()
        .map(|v| match v {
            Value::Num(n) => Some(*n),
            _ => None,
        })
        .collect()
}

fn push_call<'a>(
    params: &'a [Parameter],
    body: &'a Expr,
    args: &'a [Arg],
    caller: &Scope,
    base: &Scope,
    jit: Option<&'a str>,
    tasks: &mut Vec<Task<'a>>,
) {
    // Which explicit-arg expr fills each param slot (positional by position, named by name). `None` = the
    // param takes its default / undef. Kept separate from defaults so a DUPLICATE param name binds
    // arg-over-default in the two-phase `Task::Apply` (an unfilled second slot can't clobber a real arg).
    let mut arg_slots: Vec<Option<&'a Expr>> = vec![None; params.len()];
    let mut dollars: Vec<(Rc<str>, &'a Expr)> = Vec::new(); // $-args → dynamic $-var injections
    let mut positional = 0;
    for arg in args {
        match &arg.name {
            None => {
                if let Some(slot) = arg_slots.get_mut(positional) {
                    *slot = Some(&arg.value);
                }
                positional += 1;
            }
            // a $-arg is a per-call dynamic override — injected into the call scope, not param-matched.
            Some(name) if name.starts_with('$') => dollars.push((Rc::clone(name), &arg.value)),
            Some(name) => {
                if let Some(i) = params.iter().position(|p| p.name == *name) {
                    arg_slots[i] = Some(&arg.value);
                }
            }
        }
    }
    // bind order: params first, then $-args (bound last → they override the inherited $-context). A param
    // filled by an arg is `provided`; a param on its default (or a defaultless-unfilled undef) is not.
    // `$`-args are always provided. `Task::Apply` binds the non-provided (defaults) before the provided.
    // Names are `Rc<str>` cloned from the AST (a refcount bump) so the per-call bind never allocates (N.2b).
    let mut names: Vec<Rc<str>> = params.iter().map(|p| Rc::clone(&p.name)).collect();
    names.extend(dollars.iter().map(|(name, _)| Rc::clone(name)));
    let mut provided: Vec<bool> = arg_slots.iter().map(Option::is_some).collect();
    provided.extend(std::iter::repeat_n(true, dollars.len()));
    // A `$`-arg appends names beyond the params, so `names.len()` would no longer equal the compiled
    // function's arity — clear the JIT hint in that (rare) case so an eligible call is only ever an
    // all-positional one. The caller already passes `None` for closures; this guards the dollar path.
    let jit = if dollars.is_empty() { jit } else { None };
    tasks.push(Task::Apply {
        names,
        provided,
        body,
        base: base.clone(),
        caller: caller.clone(),
        jit,
    });
    // push evals so the popped run is [params.., dollars..]: dollars first (deeper → on top), then
    // params reversed (param 0 evaluates first, lands at the bottom of the run). An arg evaluates in the
    // CALLER scope; a default in the function's lexical `base`; an unfilled defaultless slot → undef.
    for (_, expr) in dollars.iter().rev() {
        tasks.push(Task::Eval(expr, caller.clone()));
    }
    for (slot, param) in arg_slots.into_iter().zip(params).rev() {
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
        ExprKind::LcFor { .. } | ExprKind::LcForC { .. } | ExprKind::LcEach(_) | ExprKind::LcIf { .. } => {
            true
        }
        // A `let` in a vector is TRANSPARENT: it splices IFF its body does. `[let(x=…) [a,b]]`
        // contributes the vector as ONE element (`[[a,b]]`), while `[let(x=…) each L]` splices — OpenSCAD-
        // verified. Unlike `if`/`for`/`each` (which route through `eval_comprehension`, adding a wrapper
        // `splice_into` then removes), a bare `let` evaluates its body DIRECTLY, so the splice decision
        // has to follow the body, not the `let` node. Without this, `(let(i) [pt])` in a path builder (e.g.
        // BOSL2's trapezoid corners) unwrapped its single-point list and flattened the whole path.
        ExprKind::Let { body, .. } => is_comprehension(body),
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
/// `range_iter`), a string's characters, or a scalar as a single value.
fn iter_values(v: &Value) -> Vec<Value> {
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

/// Evaluate a comprehension element to its CONTRIBUTION — the values it splices into the enclosing
/// vector. A plain expr contributes `[value]`; `for`/`each`/`if`/`let` flatmap/splice/filter/scope.
///
/// Comprehension NESTING is parse-bounded (`MAX_DEPTH`), so this bounded host recursion can't overflow;
/// iteration is capped (`RANGE_MAX`, list length). Each sub-expression re-enters the explicit-stack
/// evaluator carrying the TOP-LEVEL `global` (so a function called in a body resolves against globals,
/// not the loop scope) — a fresh stack per step; folding it onto one explicit stack is a deferred perf
/// optimization, and the element-cap WARNING text is I.5.
fn eval_comprehension<'a>(
    elem: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    match &elem.kind {
        ExprKind::LcFor { bindings, body } => lc_for(bindings, body, scope, global, ctx),
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => lc_for_c(init, cond, update, body, scope, global, ctx),
        // `each E` splices ONE level: for every value element `E` would contribute, iterate it in. `E`
        // is itself a comprehension element, so evaluate it as one — `each if(c) X` / `each for(…) X`
        // must distribute the splice INTO the guard/loop (OpenSCAD: `each if(true) [1,2,3]` → `[1,2,3]`,
        // not `[[1,2,3]]`). Evaluating `E` as a plain expression (the old path) wrapped an `if`'s
        // contribution in a vector, so `each` only peeled the wrapper and left the list nested.
        ExprKind::LcEach(e) => {
            let mut out = Vec::new();
            for contribution in eval_comprehension(e, scope, global, ctx)? {
                out.extend(iter_values(&contribution));
            }
            Ok(out)
        }
        ExprKind::LcIf { cond, then, els } => {
            if eval_with_global(cond, scope, global, ctx)?.is_truthy() {
                eval_comprehension(then, scope, global, ctx)
            } else {
                match els {
                    Some(e) => eval_comprehension(e, scope, global, ctx),
                    None => Ok(Vec::new()),
                }
            }
        }
        ExprKind::Let { bindings, body } => {
            let inner = comprehension_let_scope(bindings, scope, global, ctx)?;
            eval_comprehension(body, &inner, global, ctx)
        }
        _ => Ok(vec![eval_with_global(elem, scope, global, ctx)?]), // a plain element → [value]
    }
}

/// `for (name = iterable, …) body` — iterate each binding (multiple bindings NEST), evaluate `body`'s
/// contribution per step, concatenate.
fn lc_for<'a>(
    bindings: &'a [Arg],
    body: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    match bindings.split_first() {
        None => eval_comprehension(body, scope, global, ctx),
        Some((binding, rest)) => {
            // The loop var as an `Rc<str>` computed ONCE per binding, so the per-ITERATION bind is a refcount
            // bump, not a fresh `String` (N.2b) — `lc_for` is the hottest bind path (64% of a real model).
            let var: Rc<str> = binding.name.clone().unwrap_or_else(|| Rc::from("_"));
            let iterable = eval_with_global(&binding.value, scope, global, ctx)?;
            let mut out = Vec::new();
            for value in iter_values(&iterable) {
                let mut inner = scope.child();
                inner.bind(Rc::clone(&var), value);
                out.extend(lc_for(rest, body, &inner, global, ctx)?);
            }
            Ok(out)
        }
    }
}

/// C-style `for (init; cond; update) body`: the loop variables live in a flat map (each iteration a
/// fresh `scope.child()`, so no chain accumulation), `cond`/`update` see the current values, and
/// `update` MERGES into them (unmentioned vars persist). Capped at `RANGE_MAX` iterations.
fn lc_for_c<'a>(
    init: &'a [Arg],
    cond: &'a Expr,
    update: &'a [Arg],
    body: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    // Init assignments bind SEQUENTIALLY (`let`-style): a later one sees the earlier ones, so
    // `for(a=1, b=a+1; …)` gives `b==2`. Accumulate into a child scope as we go.
    let mut vars: Vec<(String, Value)> = Vec::new();
    let mut init_scope = scope.child();
    for arg in init {
        let name = arg.name.as_deref().unwrap_or("_").to_string();
        let value = eval_with_global(&arg.value, &init_scope, global, ctx)?;
        init_scope.bind(name.clone(), value.clone());
        vars.push((name, value));
    }
    let mut out = Vec::new();
    let mut iterations = 0u64;
    loop {
        let mut loop_scope = scope.child();
        for (name, value) in &vars {
            loop_scope.bind(name.clone(), value.clone());
        }
        if !eval_with_global(cond, &loop_scope, global, ctx)?.is_truthy() {
            break;
        }
        out.extend(eval_comprehension(body, &loop_scope, global, ctx)?);
        // Update assignments also bind SEQUENTIALLY within the clause: `x=i*10, y=x+1` must let `y`
        // see the NEW `x` (OpenSCAD-verified; BOSL2's `_dp_distance_row` DP does exactly this with
        // `costs=…, newrow=…min(costs)…`). Bind each into `loop_scope` as we go so the next update sees
        // it; `vars` carries the results to the next iteration.
        for arg in update {
            let name = arg.name.as_deref().unwrap_or("_");
            let value = eval_with_global(&arg.value, &loop_scope, global, ctx)?;
            loop_scope.bind(name.to_string(), value.clone());
            match vars.iter_mut().find(|(n, _)| n == name) {
                Some(entry) => entry.1 = value,
                None => vars.push((name.to_string(), value)),
            }
        }
        iterations += 1;
        if iterations >= RANGE_MAX {
            // The runaway-`for(i=0; 1; …)` guard. Reaching it needs RANGE_MAX (1e7) real iterations, so
            // it's the single line the corpus can't cover — a defensive limit, equivalent-mutant class.
            // (Eval isn't under the parser/lexer mandatory-100% rule; the warning TEXT is I.5.)
            break;
        }
    }
    Ok(out)
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
    for binding in bindings {
        let name: Rc<str> = binding.name.clone().unwrap_or_else(|| Rc::from("_"));
        let value = name_closure(eval_with_global(&binding.value, &s, global, ctx)?, &name);
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
        } => Value::Function {
            closure_id,
            env,
            self_name: Some(std::rc::Rc::from(name)),
            repr,
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
    let ctx = build_ctx(program);
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
    let n = islands.len();
    let ctx = Ctx {
        functions,
        intrinsics,
        // Island 0 (root) is filled by `run_stmts`; the rest are seeded empty and built just below.
        island_globals: RefCell::new(vec![Scope::new(); n]),
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
        impure_reads: std::cell::Cell::new(0),
        jit: None, // native shell injects the JIT after building it (P.1.2b); the loader path stays interp
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
    let global = hoist_scope_publishing(&exec, &Scope::new(), &ctx, 0)?;
    for i in 1..n {
        let island_global = build_island_global(i, &ctx)?;
        if let Some(slot) = ctx.island_globals.borrow_mut().get_mut(i) {
            *slot = island_global;
        }
    }
    redundancy::reset(); // dev probe: fresh count per run so the import fixpoint's partial runs don't bleed in
    fnprofile::reset(); // dev probe: same — fresh per-name call counts per run (FAB_PROFILE_FNS)
    let tree = eval_top(&exec, &global, &ctx)?;
    redundancy::report(); // prints to stderr only under FAB_REDUNDANCY=1
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
) -> crate::Result<(Geo, Vec<Message>)> {
    io::drive(source, base_dir, root_path, library_paths, io::no_import_reader)
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

/// Resolve each defined function to a registered INTRINSIC (O.1) — the fingerprint gate, run ONCE here at
/// ctx build so call-time dispatch is a cheap name lookup. A function whose `(params, body)` fingerprints to
/// a registry entry gets a native impl; everything else (the vast majority) is absent → interpreted. Built
/// from the SAME resolved `functions` map the interpreter dispatches against, so the body matched is the body
/// that would run.
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
                    eprintln!("+ [intrinsic WIRED] {name} (fp {:#018x})", intrinsics::fingerprint(params, body));
                }
                intrinsics::Plan::Drift => eprintln!(
                    "+ [intrinsic DRIFT] {name} — defined fp {:#018x} != reference fp {} → INTERPRETED \
                     (library drift, or a stale reference)",
                    intrinsics::fingerprint(params, body),
                    intrinsics::reference_fp(name).map_or_else(|| "?".to_string(), |fp| format!("{fp:#018x}")),
                ),
                intrinsics::Plan::NotRegistered => {}
            }
        }
        if let Some(func) = intrinsics::lookup(name, params, body) {
            out.insert(name, func);
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
    let mut scope = Scope::new();
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
    Ok(union_of(if root.is_empty() { nodes } else { root }, ctx))
}


/// Collect the scope-LOCAL `module` definitions of a statement list (last-wins by name) — the module-side
/// analogue of [`hoisted_bindings`]'s function handling. Kept a stmt-list pure pass; [`eval_nodes`] pushes
/// the result for the block's eval so a body-local `module f(){…}` resolves (L.2.8m).
fn collect_module_defs<'a>(stmts: &[&'a Stmt]) -> loader::ModStore<'a> {
    let mut store = loader::ModStore::new();
    for stmt in stmts {
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
    let mut scope = scope.clone();
    for item in hoisted_bindings(stmts) {
        // sigil `=` for an assignment, `f` for a hoisted function def (so the trace tells them apart).
        let (sigil, name, value) = match item {
            HoistItem::Assign(name, expr) => {
                ('=', name, name_closure(eval_with_ctx(expr, &scope, ctx)?, name))
            }
            // A module-body-LOCAL `function f(x)=…` becomes a closure VALUE in the body scope, captured
            // AT THIS POINT so it sees the enclosing locals hoisted before it (BOSL2's `make_path` closes
            // over `steps`/`ang`). `dispatch_call`'s function-value path then applies it; `self_name`
            // gives it recursion.
            HoistItem::Func(name, params, body) => {
                ('f', name, function_def_closure(name, params, body, &scope, ctx))
            }
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
    for (name, expr) in hoisted_assignments(stmts) {
        let value = name_closure(eval_with_ctx(expr, &scope, ctx)?, name);
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
    // Which explicit-arg expr fills each param slot (positional by position, named by name). `None` = the
    // param took no arg → it falls to its default in phase 1.
    let mut arg_slots: Vec<Option<&'a Expr>> = vec![None; params.len()];
    let mut dollars: Vec<(Rc<str>, &'a Expr)> = Vec::new();
    let mut positional = 0;
    for arg in args {
        match &arg.name {
            None => {
                if let Some(slot) = arg_slots.get_mut(positional) {
                    *slot = Some(&arg.value);
                }
                positional += 1;
            }
            Some(name) if name.starts_with('$') => dollars.push((Rc::clone(name), &arg.value)),
            Some(name) => {
                if let Some(i) = params.iter().position(|p| p.name == *name) {
                    arg_slots[i] = Some(&arg.value);
                }
            }
        }
    }

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
/// scalar → a single iteration (OpenSCAD's `for(i = 5)`).
fn iterate_values(v: &Value) -> Vec<Value> {
    match v {
        Value::Range { start, step, end } => {
            range_iter(*start, *step, *end).map(Value::Num).collect()
        }
        Value::NumList(xs) => xs.iter().map(|&n| Value::Num(n)).collect(),
        Value::List(xs) => xs.to_vec(),
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
fn hoisted_assignments<'a>(stmts: &[&'a Stmt]) -> Vec<(&'a str, &'a Expr)> {
    let mut order: Vec<(&'a str, &'a Expr)> = Vec::new();
    let mut index: BTreeMap<&'a str, usize> = BTreeMap::new();
    for stmt in stmts {
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
    for stmt in stmts {
        let (name, item) = match &stmt.kind {
            StmtKind::Assignment { name, value } => (&**name, HoistItem::Assign(name, value)),
            StmtKind::FunctionDef { name, params, body } => {
                (name.as_str(), HoistItem::Func(name, params.as_slice(), body))
            }
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
fn function_def_closure<'a>(
    name: &str,
    params: &'a [Parameter],
    body: &'a Expr,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> Value {
    let closure_id = {
        let mut closures = ctx.closures.borrow_mut();
        closures.push((params, body));
        closures.len() - 1
    };
    Value::Function {
        closure_id,
        env: scope.clone(),
        self_name: Some(std::rc::Rc::from(name)),
        repr: crate::parser::print::function_value_repr(params, body).into(),
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
    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        let value = eval_with_global(&arg.value, scope, global, ctx)?;
        parts.push(match &arg.name {
            Some(name) => format!("{name} = {}", fmt::format_value(&value)),
            None => fmt::format_value(&value),
        });
    }
    ctx.messages
        .borrow_mut()
        .push(Message::Echo(parts.join(", ")));
    Ok(())
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
    // Keep each condition's EXPR alongside its value: on failure we pretty-print the condition back to
    // source (`print_expr`) as a `[assert(…)]` locator. BOSL2's asserts are usually message-less, so
    // without this a failure is a blank "assertion failed" — the printed condition is grep-able straight
    // into the library (e.g. `assert(is_finite(r) && !approx(r,0))` → one hit in shapes3d.scad). It's
    // reconstructed from the AST, so it needs no retained source (true file:line is a separate feature).
    let mut positional: Vec<(&Expr, Value)> = Vec::new();
    let mut named_condition = None;
    let mut named_message = None;
    for arg in args {
        let value = eval_with_global(&arg.value, scope, global, ctx)?;
        match arg.name.as_deref() {
            None => positional.push((&arg.value, value)),
            Some("condition") => named_condition = Some((&arg.value, value)),
            Some("message") => named_message = Some(value),
            Some(_) => {} // unknown named arg — dropped, as OpenSCAD arg-matching does
        }
    }
    // A named `condition`/`message` beats the positional slot (params are `condition`, then `message`).
    let condition = named_condition.or_else(|| positional.first().cloned());
    let message = named_message.or_else(|| positional.get(1).map(|(_, v)| v.clone()));
    let passed = matches!(&condition, Some((_, c)) if c.is_truthy());
    // Pretty-print the condition back to source ONLY when it's actually consumed — the trace line (off in
    // release) or a FAILURE locator. BOSL2 is assert-DENSE and its asserts overwhelmingly PASS, so building
    // this string on every passing assert with tracing off was pure churn (N.2a: `write_expr` showed up at
    // ~1.5% of a real model's allocation, all of it thrown away). `""` covers the degenerate `assert()`.
    let cond_src = if trace::on() || !passed {
        condition.map_or_else(String::new, |(e, _)| crate::parser::print_expr(e))
    } else {
        String::new()
    };
    trace::assert(passed, &cond_src); // gated inside (like bind/ret/module) — free when the trace is off
    if passed {
        return Ok(());
    }
    let locator = format!(" [assert({cond_src})]");
    Err(crate::Error::Eval(match message {
        Some(Value::Str(s)) => format!("assertion failed: {s}{locator}"),
        Some(other) => format!("assertion failed: {}{locator}", fmt::format_value(&other)),
        None => format!("assertion failed{locator}"),
    }))
}

/// Collect user function definitions into the [`Ctx`] store (their own namespace). A pre-pass over the
/// whole program, so a call can resolve a function defined anywhere (whole-program visibility, like
/// OpenSCAD); a duplicate name — last definition wins (`BTreeMap::insert`).
fn build_ctx(program: &Program) -> Ctx<'_> {
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
        impure_reads: std::cell::Cell::new(0),
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
        let scad = resolve_source("use <lib.scad>\ncube(1);", here, None, &no_scad, &no_files)
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
        )
        .expect("resolves");
        assert!(
            matches!(&files_wanted, Resolution::Incomplete { needs }
                if needs == &[file_need("a.stl"), file_need("b.stl")]),
            "expected two File needs in one round, got {files_wanted:?}"
        );

        // Supply the mesh → the run CLOSES (Complete). An empty placeholder mesh stands in for a read STL.
        let mut have = FileTable::new();
        have.insert("a.stl".to_string(), super::Imported::Mesh(crate::Mesh::new()));
        let closed =
            resolve_source("import(\"a.stl\");", here, None, &no_scad, &have).expect("resolves");
        assert!(
            matches!(&closed, Resolution::Complete { .. }),
            "expected Complete, got {closed:?}"
        );
    }

    /// Evaluate a program's assignments in order (binding each), returning the LAST assignment's value
    /// — with the program's function store in scope. The end-to-end call test harness.
    fn eval_last(src: &str) -> Value {
        let prog = parse(src).expect("parses");
        let ctx = build_ctx(&prog);
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
        fn call_numeric(&self, name: &str, args: &[f64]) -> Option<f64> {
            match (name, args) {
                ("sq", [x]) => Some(x * x + 1000.0),
                _ => None,
            }
        }
    }

    /// [`eval_last`] with a numeric-JIT hook injected into the ctx.
    fn eval_last_jit(src: &str, jit: &dyn super::NumericJit) -> Value {
        let prog = parse(src).expect("parses");
        let mut ctx = build_ctx(&prog);
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
        let jit = MarkerJit;
        // (1) all-positional + all-Num → the JIT fires: 5*5 + 1000 marker.
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq(5);", &jit),
            Value::Num(1025.0),
            "an eligible numeric call takes the JIT path"
        );
        // (2) a NON-number arg (a vector) → the all-Num guard fails → interpreter runs x*x = dot = 5.
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq([1,2]);", &jit),
            Value::Num(5.0),
            "a vector arg falls back to the interpreted body (no marker)"
        );
        // (3) a NAMED arg → not JIT-eligible (dispatch passes jit=None) → interpreter runs → 25. This is
        // the BOSL2-loves-named-args gap: the fast path is declined, correctness is preserved. (follow-on)
        assert_eq!(
            eval_last_jit("function sq(x) = x*x; y = sq(x = 5);", &jit),
            Value::Num(25.0),
            "a named-arg call falls back to the interpreter (no marker)"
        );
        // (4) a function the registry doesn't know → call_numeric returns None → interpreter runs → 27.
        assert_eq!(
            eval_last_jit("function cube(x) = x*x*x; y = cube(3);", &jit),
            Value::Num(27.0),
            "a registry miss falls back to the interpreter"
        );
        // (5) no hook at all (the wasm/raw path) → everything interprets → 25.
        assert_eq!(eval_last("function sq(x) = x*x; y = sq(5);"), Value::Num(25.0));
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
