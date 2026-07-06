//! The `use`/`include` loader — resolves H's zero-IO AST nodes into a real file graph, then flattens
//! it for evaluation. The parser stays PURE (no file IO ever touches it); this module is the ONE place
//! the outside filesystem enters the language, and it enters bug-for-bug against OpenSCAD's own
//! `find_valid_path` + lexer token-splice (`src/core/parsersettings.cc`, `src/core/lexer.l`).
//!
//! Two mechanisms, verified against the OpenSCAD source:
//! - **`include <f>`** is a LEXER TOKEN SPLICE upstream — `f`'s tokens become part of the including
//!   file's parse, in source order at the include point. So `f`'s definitions AND its top-level
//!   assignments/geometry land in the including scope and execute where the `include` sits. We
//!   reproduce that by flattening: each `include` is replaced in place by its target's statements.
//! - **`use <f>`** imports only `f`'s function + module DEFINITIONS (never its variables, never its
//!   geometry) into a lower-priority namespace. No statements execute.
//!
//! Name precedence, straight from `FileContext::lookup_local_function` + `LocalScope::addFunction` +
//! `SourceFile::registerUse`: local/include definitions beat `use`-imported ones ALWAYS and
//! position-independently (the local scope is an `unordered_map` checked before `usedlibs`). Within
//! the local/include tier it's LAST-wins (a later def overwrites). `use`-vs-`use` is ALSO last-wins,
//! but by a sneakier route — `registerUse` dedups then FRONT-inserts each `use` into `usedlibs`, and
//! lookup returns the first hit, so the textually-LAST `use` sits at the front and wins. A `use`d file
//! exports its OWN flattened defs (its `include`s fold in, last-wins) but NOT its own `use`s — `use`
//! is not transitive. (`use` also shadows a builtin of the same name; that lookup-order nuance lives
//! in `dispatch_call`, not here.)
//!
//! USE-SCOPE (the per-file lexical base, J.3.7): a `use`d function/module evaluates its body against its
//! OWN file's top-level scope, not the caller's — so a used def that reads a constant defined at its
//! file's top level sees that constant, exactly as OpenSCAD does (it builds a `FileContext` over the used
//! file). Each [`Island`] therefore carries its file's `functions` + `assignments` (the constant scope)
//! alongside its `modules`; the evaluator hoists each island's assignments into a per-island global and
//! evaluates a call's body against its HOME island's global (see `Ctx::island_globals` in `mod.rs`). This
//! is what lets BOSL2's `use`-form functions (path/VNF returns reading `_ANCHOR_TYPES`-class constants)
//! resolve instead of asserting on `undef`. `include` never needed it — its assignments splice into the
//! shared scope directly.
//!
//! Resolution mirrors `find_valid_path_`: an absolute path is used as-is; a relative one resolves
//! against the INCLUDING file's directory first, then each library path in order (first existing
//! non-directory wins). We canonicalize on resolve — that canonical path is both the parse-once key
//! and the cycle key. A path already on the expansion stack is a CYCLE → skipped (so cycles break);
//! a path merely seen-before is a DIAMOND → re-expanded (duplicated), faithful to the textual paste.
//!
//! Determinism: the crate stays PURE — the caller passes explicit `library_paths`; we never read
//! `OPENSCADPATH` (a hidden input would dent the "same input → bit-identical" doctrine). The app/
//! harness reads the env + knows the BOSL2 dir and hands the paths down.
//!
//! Scope note (I.2.4): def-collection is FUNCTIONS **and** MODULES — both flow through [`Defs`] with the
//! same use-tier/local-override precedence, so a `use`d library's modules import exactly like its
//! functions (the evaluator's module-call machinery lives in `mod.rs`). A missing library fails LOUD
//! (`Error::Load`) rather than OpenSCAD's
//! warn-and-continue — a missing lib in a correct corpus is OUR resolution bug, and we want it loud
//! until I.5's warning buffer can match the oracle's warn-and-render bug-for-bug.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::parser::{Expr, Parameter, Program, Stmt, StmtKind, parse};

/// Two hard caps on `use`/`include` graph expansion, BOTH failing LOUD ([`Error::Load`](crate::Error::Load))
/// — a silent drop would be silently-wrong output, the exact thing the doctrine forbids. Only a true
/// CYCLE is skipped silently (that's the intended break, matching OpenSCAD's open-file stack).
///
/// `MAX_INCLUDE_DEPTH` bounds the include CHAIN length so deep-but-narrow recursion can't overflow the
/// host stack — the Safari-cliff sibling of the parser's `MAX_DEPTH`. But the depth cap alone does NOT
/// bound total work: a diamond where each file `include`s the next TWICE fans out to 2^N splices with a
/// chain depth of only N. `MAX_EXPANSION` bounds the TOTAL statements expanded, catching that fan-out
/// bomb. Real graphs are a handful of levels + a few thousand statements; both caps sit far above that.
const MAX_INCLUDE_DEPTH: usize = 256;
const MAX_EXPANSION: usize = 1_000_000;

/// Spend one unit of the expansion budget, failing LOUD when the total-work cap is hit (the fan-out
/// guard `MAX_INCLUDE_DEPTH` can't provide). Called once per statement visited across the whole graph.
fn spend(budget: &mut usize) -> crate::Result<()> {
    *budget = budget.checked_sub(1).ok_or_else(|| {
        crate::Error::Load(format!(
            "use/include expansion exceeds {MAX_EXPANSION} statements (fan-out bomb?)"
        ))
    })?;
    Ok(())
}

/// The LOUD error for an include/use chain deeper than [`MAX_INCLUDE_DEPTH`].
fn too_deep() -> crate::Error {
    crate::Error::Load(format!(
        "use/include chain exceeds max depth {MAX_INCLUDE_DEPTH}"
    ))
}

/// One resolved top-level `use`/`include`, as an index into [`Loaded::programs`] (indices dodge the
/// borrow-checker fight that `&Program` refs would pick during the load phase's `Vec` growth).
#[derive(Clone, Copy)]
enum Link {
    /// `include <f>` → splice `f`'s statements here.
    Include(usize),
    /// `use <f>` → import `f`'s exported defs (no statements execute).
    Use(usize),
}

/// A parsed file plus, per top-level `use`/`include` statement, where it resolved to. Keyed by the
/// statement's index in `program.stmts` so expansion can walk statements and consult the link inline.
struct Node {
    program: Program,
    /// This file's directory — the base for resolving ITS relative `use`/`include`s (OpenSCAD resolves
    /// against the including file's dir, not the root's).
    dir: PathBuf,
    /// stmt-index → resolved target. Only top-level `use`/`include` statements appear here.
    links: BTreeMap<usize, Link>,
}

/// The frozen file graph: the root at index 0, every transitively-reachable `use`/`include` target
/// appended (parse-once, deduped by canonical path). Owning all of them here is what lets the
/// evaluator's `&'a`-into-the-AST `Ctx`/`Task` borrow uniformly across files — the root is owned too,
/// so there's no borrowed-root-vs-owned-deps split.
pub(super) struct Loaded {
    programs: Vec<Node>,
}

/// Whether a scanned top-level file-reference statement was a `use` or an `include`.
enum RefKind {
    Include,
    Use,
}

/// Load `source` (base directory `base_dir`) and everything it reaches via `use`/`include`, resolving
/// against `library_paths` after the including file's own directory. `root_path` is the root's own
/// file path when it has one (`evaluate_file`) so a dependency that references the root back resolves
/// to the SAME node — parse-once and cycle-break instead of a re-parse; `None` for an in-memory buffer
/// (`evaluate_with_base`), which nothing on disk can name.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if a `use`/`include` target can't be resolved or read;
/// [`Error::Parse`](crate::Error::Parse) if the root or any loaded file fails to parse.
pub(super) fn load(
    source: &str,
    base_dir: &Path,
    root_path: Option<&Path>,
    library_paths: &[PathBuf],
) -> crate::Result<Loaded> {
    // The IO SHELL (M.4): the ONE place `use`/`include` touches the filesystem. It runs the pure
    // [`resolve_graph`] fixpoint — resolve-graph names the references it can't satisfy, the shell reads
    // them (find_valid_path + read + canonicalize), re-runs — until the graph closes. `use`/`include` is
    // STATIC (literal top-level refs), so this is a parse-time fixpoint over the include graph; no eval.
    let root_id = root_path.and_then(|p| std::fs::canonicalize(p).ok());
    let mut provided = SourceMap::new();
    loop {
        match resolve_graph(source, base_dir, root_id.as_deref(), &provided)? {
            GraphOutcome::Complete(loaded) => return Ok(loaded),
            GraphOutcome::Incomplete(needs) => {
                for need in needs {
                    let key = (need.from_dir.clone(), need.raw.clone());
                    if provided.contains_key(&key) {
                        continue; // already satisfied this round (a duplicate need in the same pass)
                    }
                    let id =
                        resolve(&need.from_dir, &need.raw, library_paths).ok_or_else(|| {
                            crate::Error::Load(format!(
                                "can't find '{}' from {}",
                                need.raw,
                                need.from_dir.display()
                            ))
                        })?;
                    let source = std::fs::read_to_string(&id).map_err(|e| {
                        // Defensive, never-panic: `resolve` already canonicalized this as a readable file,
                        // so a failure here is a TOCTOU race (deleted / perms changed between resolve and
                        // read). This — and the canonicalize/read above — are the SHELL's IO, off the pure
                        // core's coverage.
                        crate::Error::Load(format!("{}: {e}", id.display()))
                    })?;
                    let dir = id.parent().unwrap_or(Path::new(".")).to_path_buf();
                    provided.insert(key, ProvidedSource { id, dir, source });
                }
            }
        }
    }
}

/// A `use`/`include` reference the pure resolver couldn't satisfy from the [`SourceMap`] — the shell
/// resolves + reads it, adds it, and re-runs. `from_dir` is the requesting file's directory (the base for
/// resolving `raw` against the library paths); `raw` is the literal `<...>` path.
pub(super) struct ScadNeed {
    pub from_dir: PathBuf,
    pub raw: String,
}

/// A source the shell has resolved + read: `id` is its canonical path (the opaque dedup/cycle-break key
/// the pure resolver compares by, never canonicalizing itself), `dir` its directory (the base for ITS own
/// relative refs), `source` the text.
struct ProvidedSource {
    id: PathBuf,
    dir: PathBuf,
    source: String,
}

/// The sources the shell has supplied so far, keyed by the `(requesting dir, raw ref)` the resolver asks
/// with — so the pure resolver looks a reference up WITHOUT resolving it (no IO in the core).
type SourceMap = BTreeMap<(PathBuf, String), ProvidedSource>;

/// The pure graph resolver's result: the fully-closed graph, or the references it still needs.
enum GraphOutcome {
    Complete(Loaded),
    Incomplete(Vec<ScadNeed>),
}

/// PURE (zero IO): build the `use`/`include` graph from `source` + the shell-supplied [`SourceMap`],
/// naming any reference not yet in the map as a [`ScadNeed`] instead of reading it. A single BFS pass
/// (parse-once via the canonical-`id` index, which also breaks cycles) — either every reference resolves
/// (→ [`GraphOutcome::Complete`]) or the pass collects ALL the still-missing ones at once (→ `Incomplete`,
/// which the shell fills before re-running). `root_id` is the root file's canonical id, so a dependency
/// referencing the root back dedups to node 0 rather than re-parsing it.
fn resolve_graph(
    source: &str,
    base_dir: &Path,
    root_id: Option<&Path>,
    provided: &SourceMap,
) -> crate::Result<GraphOutcome> {
    let mut programs = vec![Node {
        program: parse(source)?,
        dir: base_dir.to_path_buf(),
        links: BTreeMap::new(),
    }];
    let mut index: BTreeMap<PathBuf, usize> = BTreeMap::new();
    if let Some(id) = root_id {
        index.insert(id.to_path_buf(), 0);
    }
    let mut queue: Vec<usize> = vec![0];
    let mut needs = Vec::new();

    while let Some(idx) = queue.pop() {
        // Scan into OWNED data first (path strings), dropping the `&programs[idx]` borrow before we
        // parse + push children — otherwise the push would alias the read.
        let refs = scan(&programs[idx].program);
        let dir = programs[idx].dir.clone();
        let mut links = BTreeMap::new();
        for (stmt_i, raw, kind) in refs {
            let Some(src) = provided.get(&(dir.clone(), raw.clone())) else {
                // Not supplied yet — collect the need and keep scanning (gather ALL of this pass's needs).
                needs.push(ScadNeed {
                    from_dir: dir.clone(),
                    raw,
                });
                continue;
            };
            let child = if let Some(&i) = index.get(&src.id) {
                i // parse-once: a diamond / back-reference reuses the existing node
            } else {
                let node = Node {
                    program: parse(&src.source)?,
                    dir: src.dir.clone(),
                    links: BTreeMap::new(),
                };
                let i = programs.len();
                programs.push(node);
                index.insert(src.id.clone(), i);
                queue.push(i);
                i
            };
            links.insert(
                stmt_i,
                match kind {
                    RefKind::Include => Link::Include(child),
                    RefKind::Use => Link::Use(child),
                },
            );
        }
        programs[idx].links = links; // assign after: borrow dropped, children pushed
    }

    if needs.is_empty() {
        Ok(GraphOutcome::Complete(Loaded { programs }))
    } else {
        Ok(GraphOutcome::Incomplete(needs))
    }
}

/// A user function definition's slice-of-params + body, borrowed from the owning [`Program`].
pub(super) type FnDef<'a> = (&'a [Parameter], &'a Expr);
/// The function store the evaluator's [`Ctx`](super::Ctx) is built from: name → its definition.
pub(super) type FnStore<'a> = BTreeMap<&'a str, FnDef<'a>>;
/// A user module definition's params + body STATEMENT (usually a block), borrowed from the [`Program`].
pub(super) type ModDef<'a> = (&'a [Parameter], &'a Stmt);
/// The module store — name → its definition. Collected + merged EXACTLY like [`FnStore`] (`use` tier
/// base, local/`include` overrides), since `use` imports modules alongside functions.
pub(super) type ModStore<'a> = BTreeMap<&'a str, ModDef<'a>>;

/// One MODULE scope island (I.9.5): the module-name resolution scope of one file (the root or a `use`
/// target). Module resolution is LEXICAL, not global — a module defined in a `use`d file resolves ITS
/// OWN body's module calls against that file's island (its include-flattened defs + the files IT uses +
/// builtins), NOT the includer's redefinitions. This is precisely what makes BOSL2's `builtins.scad`
/// trick work: `module _cube(size,center) cube(size,center=center);` resolves `cube` to the BUILTIN
/// primitive, because builtins.scad's island defines no `cube` — even though the program (via `include`)
/// redefines `cube` as the attachable wrapper. A global store resolves that `cube` back to the wrapper →
/// unbounded `cube → … → _cube → cube` recursion (the exact I.9.5 symptom).
pub(super) struct Island<'a> {
    /// This file's include-flattened module defs (last-wins). A name here beats any `use`-imported one.
    pub modules: ModStore<'a>,
    /// This file's include-flattened FUNCTION defs (last-wins), collected alongside `modules` so a called
    /// function's body can be evaluated with THIS island's constants as its lexical base (the use-scope
    /// fix): a function reads the constants of the file it was DEFINED in, not the caller's.
    pub functions: FnStore<'a>,
    /// This file's include-flattened top-level ASSIGNMENTS (name → value expr), in first-occurrence order
    /// — the file's constant scope. Hoisted (whole-scope, last-wins) into this island's global by the
    /// evaluator so `use`d functions/modules see their own file's `_ANCHOR_TYPES`-class constants (which
    /// `use` does NOT import into the caller). OpenSCAD builds a per-file `FileContext` for exactly this.
    pub assignments: Vec<(&'a str, &'a Expr)>,
    /// Island indices of the files this one `use`s, in source order. Resolution scans them REVERSED so
    /// the textually-last `use` wins (matching OpenSCAD's front-inserted `usedlibs`). Non-transitive:
    /// resolution looks only at a used island's OWN `modules`, never its `uses`.
    pub uses: Vec<usize>,
}

/// The module scope islands of a load graph: index 0 is the ROOT file; each distinct `use` target gets
/// its own island. Functions stay a single global store for now — the `use`d files in the BOSL2 corpus
/// export only modules, so the function-side lexical scope is the still-deferred limitation (see the
/// module header's KNOWN LIMITATION note), not a live divergence.
pub(super) type Islands<'a> = Vec<Island<'a>>;

/// The function + module definitions collected from a load graph — both name→def, same precedence.
/// Bundled so the flatten recursion threads ONE accumulator per tier instead of two.
#[derive(Default)]
pub(super) struct Defs<'a> {
    pub functions: FnStore<'a>,
    pub modules: ModStore<'a>,
}

impl<'a> Defs<'a> {
    /// Merge `other` INTO self (other overrides on a name clash) — the local-over-use precedence step.
    fn extend(&mut self, other: Defs<'a>) {
        self.functions.extend(other.functions);
        self.modules.extend(other.modules);
    }
}

/// Flatten the graph for evaluation: the executable statement stream (includes spliced in place, uses
/// dropped) plus the merged function store with OpenSCAD's precedence baked in.
///
/// The return borrows `&'a Loaded`, so the caller must hold `loaded` alive across the whole evaluation
/// — the `&'a Stmt` exec stream and the `&'a`-into-the-AST function store are what the explicit-stack
/// machine runs on.
///
/// # Errors
/// [`Error::Load`](crate::Error::Load) if the graph is pathological — a `use`/`include` chain deeper
/// than [`MAX_INCLUDE_DEPTH`] or a fan-out exceeding [`MAX_EXPANSION`] total statements.
pub(super) fn flatten(loaded: &Loaded) -> crate::Result<(Vec<&Stmt>, Defs<'_>)> {
    let mut exec = Vec::new();
    let mut local = Defs::default(); // local/include tier — last-wins
    let mut used = Defs::default(); // use tier — first-wins
    let mut stack = Vec::new();
    let mut budget = MAX_EXPANSION;
    expand(
        loaded,
        0,
        &mut exec,
        &mut local,
        &mut used,
        &mut stack,
        0,
        &mut budget,
    )?;

    // Precedence: use-tier is the base, local/include OVERRIDES it (local always beats use).
    let mut defs = used;
    defs.extend(local);
    Ok((exec, defs))
}

/// Recursively expand program `idx` into the exec stream + local def tier. An `include` recurses (its
/// statements + defs fold in here); a `use` collects its exported defs into the use tier without
/// executing anything. A CYCLE (`idx` already on `stack`) is skipped silently — the intended break;
/// exceeding [`MAX_INCLUDE_DEPTH`] or the `budget` fails LOUD ([`spend`] / [`too_deep`]).
#[allow(
    clippy::too_many_arguments,
    reason = "flat recursion state (accumulators + guards); a struct \
    would just move the same fields behind indirection"
)]
fn expand<'a>(
    loaded: &'a Loaded,
    idx: usize,
    exec: &mut Vec<&'a Stmt>,
    local: &mut Defs<'a>,
    used: &mut Defs<'a>,
    stack: &mut Vec<usize>,
    depth: usize,
    budget: &mut usize,
) -> crate::Result<()> {
    if stack.contains(&idx) {
        return Ok(()); // cycle: skip silently (OpenSCAD's open-file-stack break — intended)
    }
    if depth > MAX_INCLUDE_DEPTH {
        return Err(too_deep());
    }
    stack.push(idx);
    let node = &loaded.programs[idx];
    for (i, stmt) in node.program.stmts.iter().enumerate() {
        spend(budget)?; // fan-out guard: bound TOTAL work, which the depth cap alone doesn't
        // A top-level `use`/`include` has a resolved link (load guarantees it); every other statement
        // has none. Dispatching off the link — not re-matching `stmt.kind` — means there's no
        // can't-happen "use statement without a link" branch to leave uncovered.
        match node.links.get(&i) {
            Some(Link::Include(target)) => {
                expand(loaded, *target, exec, local, used, stack, depth + 1, budget)?;
            }
            Some(Link::Use(target)) => {
                // Overwrite in source order → the textually-LAST `use` wins (OpenSCAD front-inserts
                // into usedlibs then takes the first hit; same result without the front-insert).
                used.extend(exported_defs(loaded, *target, budget)?);
            }
            None => match &stmt.kind {
                StmtKind::FunctionDef { name, params, body } => {
                    local
                        .functions
                        .insert(name.as_str(), (params.as_slice(), body)); // last-wins
                    exec.push(stmt); // eval_stmt no-ops it; kept for stream parity
                }
                StmtKind::ModuleDef { name, params, body } => {
                    local
                        .modules
                        .insert(name.as_str(), (params.as_slice(), &**body)); // last-wins
                    exec.push(stmt); // eval_stmt no-ops it; kept for stream parity
                }
                _ => exec.push(stmt),
            },
        }
    }
    stack.pop();
    Ok(())
}

/// A `use`d file's exported function defs: its OWN flattened defs (its `include`s fold in, last-wins),
/// but NOT its own `use`s — `use` is not transitive. Shares the caller's `budget` so a fan-out inside a
/// used file's include graph is bounded too.
fn exported_defs<'a>(
    loaded: &'a Loaded,
    idx: usize,
    budget: &mut usize,
) -> crate::Result<Defs<'a>> {
    let mut defs = Defs::default();
    let mut stack = Vec::new();
    collect_exported(loaded, idx, &mut defs, &mut stack, 0, budget)?;
    Ok(defs)
}

/// Walk `idx`'s flattened statement stream collecting function defs (last-wins), following `include`s
/// but ignoring `use`s (non-transitive). Cycle-/depth-/budget-guarded exactly like [`expand`].
fn collect_exported<'a>(
    loaded: &'a Loaded,
    idx: usize,
    defs: &mut Defs<'a>,
    stack: &mut Vec<usize>,
    depth: usize,
    budget: &mut usize,
) -> crate::Result<()> {
    if stack.contains(&idx) {
        return Ok(()); // cycle: skip silently
    }
    if depth > MAX_INCLUDE_DEPTH {
        return Err(too_deep());
    }
    stack.push(idx);
    let node = &loaded.programs[idx];
    for (i, stmt) in node.program.stmts.iter().enumerate() {
        spend(budget)?;
        match node.links.get(&i) {
            Some(Link::Include(target)) => {
                collect_exported(loaded, *target, defs, stack, depth + 1, budget)?;
            }
            Some(Link::Use(_)) => {} // `use` is NOT transitive — don't follow the used file's own uses
            None => match &stmt.kind {
                StmtKind::FunctionDef { name, params, body } => {
                    defs.functions
                        .insert(name.as_str(), (params.as_slice(), body)); // last-wins
                }
                StmtKind::ModuleDef { name, params, body } => {
                    defs.modules
                        .insert(name.as_str(), (params.as_slice(), &**body)); // last-wins
                }
                _ => {}
            },
        }
    }
    stack.pop();
    Ok(())
}

/// Build the MODULE scope [`Islands`] of a load graph (I.9.5). Two passes: (1) assign an island index to
/// the root (node 0) and to every node that is the TARGET of a `use` link anywhere in the graph — those
/// are the only files whose module scope is ever entered as a lexical base. (2) For each island-root node,
/// collect its include-flattened module defs (follow `include`, STOP at `use`) plus the island indices of
/// the files it `use`s.
///
/// PRECONDITION: called only AFTER [`flatten`] on the same graph (see `evaluate_source`). Flatten's
/// `expand`/`exported_defs` already walked this exact include structure under [`MAX_INCLUDE_DEPTH`] +
/// [`MAX_EXPANSION`] and rejected any over-deep chain or fan-out bomb — so by the time we get here the
/// graph is KNOWN-bounded, and [`collect_island`] needs no depth/budget guard of its own, only the
/// cycle-break for termination. That's why this is infallible where `flatten` returns a `Result`.
pub(super) fn islands(loaded: &Loaded) -> Islands<'_> {
    // Pass 1: node index → island index. The root is island 0; each fresh `use` target gets the next.
    let mut node_to_island: BTreeMap<usize, usize> = BTreeMap::new();
    node_to_island.insert(0, 0);
    let mut roots = vec![0usize]; // island i ↔ node roots[i]
    for node in &loaded.programs {
        for link in node.links.values() {
            if let Link::Use(target) = link
                && !node_to_island.contains_key(target)
            {
                node_to_island.insert(*target, roots.len());
                roots.push(*target);
            }
        }
    }
    // Pass 2: collect each island's include-flattened modules/functions/assignments + its used islands.
    let mut islands = Vec::with_capacity(roots.len());
    for &root_node in &roots {
        let mut scope = IslandScope::default();
        let mut stack = Vec::new();
        collect_island(loaded, root_node, &node_to_island, &mut scope, &mut stack);
        islands.push(Island {
            modules: scope.modules,
            functions: scope.functions,
            assignments: scope.assignments,
            uses: scope.uses,
        });
    }
    islands
}

/// The mutable accumulators [`collect_island`] fills for one island — a file's own module/function defs,
/// its top-level constant assignments (in order), and the islands it `use`s. Bundled so the include walk
/// threads one struct instead of four out-params.
#[derive(Default)]
struct IslandScope<'a> {
    modules: ModStore<'a>,
    functions: FnStore<'a>,
    assignments: Vec<(&'a str, &'a Expr)>,
    uses: Vec<usize>,
}

/// Walk island-root `idx`'s include subtree, collecting module defs (last-wins) and recording each `use`
/// target as an island index (via `node_to_island`, which pass 1 guarantees is populated for every `use`
/// target). Follows `include` (same island), stops at `use` (a scope boundary). The `stack` cycle-break is
/// the only guard needed — the graph is already flatten-validated (see [`islands`]), so it's bounded.
fn collect_island<'a>(
    loaded: &'a Loaded,
    idx: usize,
    node_to_island: &BTreeMap<usize, usize>,
    scope: &mut IslandScope<'a>,
    stack: &mut Vec<usize>,
) {
    if stack.contains(&idx) {
        return; // cycle: skip silently (the intended include-stack break)
    }
    stack.push(idx);
    let node = &loaded.programs[idx];
    for (i, stmt) in node.program.stmts.iter().enumerate() {
        match node.links.get(&i) {
            Some(Link::Include(target)) => {
                collect_island(loaded, *target, node_to_island, scope, stack);
            }
            // A `use` is a scope boundary: record the used file's island (its defs resolve THERE, not
            // here). `node_to_island` has every `use` target from pass 1; the `if let` is a defensive
            // no-panic guard for a can't-happen miss, not a real branch.
            Some(Link::Use(target)) => {
                if let Some(&island) = node_to_island.get(target) {
                    scope.uses.push(island);
                }
            }
            None => match &stmt.kind {
                StmtKind::ModuleDef { name, params, body } => {
                    scope
                        .modules
                        .insert(name.as_str(), (params.as_slice(), &**body)); // last-wins
                }
                StmtKind::FunctionDef { name, params, body } => {
                    scope
                        .functions
                        .insert(name.as_str(), (params.as_slice(), body)); // last-wins
                }
                // Top-level constants — kept in source order for whole-scope hoisting (last-wins on a
                // repeated name is the hoister's job, so DON'T dedup here; a later assign must appear later).
                StmtKind::Assignment { name, value } => {
                    scope.assignments.push((name.as_str(), value));
                }
                _ => {}
            },
        }
    }
    stack.pop();
}

/// Extract every top-level `use`/`include` as `(stmt-index, raw path, kind)` — OWNED, so the caller can
/// mutate the program `Vec` afterward. Nested `use`/`include` (inside a block/module body) is not
/// scanned; such a node stays LOUD-deferred at eval (top-level is the OpenSCAD norm anyway).
fn scan(program: &Program) -> Vec<(usize, String, RefKind)> {
    program
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match &s.kind {
            StmtKind::Include(p) => Some((i, p.clone(), RefKind::Include)),
            StmtKind::Use(p) => Some((i, p.clone(), RefKind::Use)),
            _ => None,
        })
        .collect()
}

/// Resolve a `use`/`include` path reference to a canonical file, mirroring OpenSCAD's `find_valid_path_`
/// (`parsersettings.cc`): an absolute reference is checked directly; a relative one resolves against
/// `base_dir` first, then each library path in order — first existing non-directory wins. `None` if no
/// candidate is a readable file. Canonicalizing here makes the result the parse-once + cycle key.
fn resolve(base_dir: &Path, raw: &str, library_paths: &[PathBuf]) -> Option<PathBuf> {
    let local = Path::new(raw);
    if local.is_absolute() {
        return check_file(local);
    }
    if let Some(found) = check_file(&base_dir.join(local)) {
        return Some(found);
    }
    library_paths
        .iter()
        .find_map(|lib| check_file(&lib.join(local)))
}

/// A path is valid iff it canonicalizes (so it exists) to a regular file (OpenSCAD rejects
/// directories). The canonical form dedups symlinks/`..` for the parse-once + cycle keys.
fn check_file(p: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(p) {
        Ok(canon) if canon.is_file() => Some(canon),
        _ => None,
    }
}
