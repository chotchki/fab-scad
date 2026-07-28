//! AR.12 — reading a LIBRARY, which is what the transpiler is actually pointed at.
//!
//! Until now the transpiler's input was `intrinsics::REGISTRY`: a hand-typed list of ~55
//! functions, each carrying a `reference` string somebody copied out of BOSL2 by hand. That input
//! cannot describe a library — it describes the functions a person already transcribed — so
//! "generate a crate per library" has nowhere to start. This module is the other input: point it
//! at a library and get back every top-level function and constant it declares, verbatim, with the
//! collisions called out.
//!
//! TWO UNITS, and the difference is not cosmetic. [`Library::read_from_roots`] follows
//! `include`/`use` from a set of ROOT files — what `fab_transpile!(["BOSL2/std.scad"])` is handed,
//! and what a user's own `include` line actually buys them. [`Library::read`] globs a directory,
//! which is what the coverage ratchet wants and what nobody's program looks like: `std.scad`
//! reaches 30 of BOSL2's 56 files, and gears/screws/threading/nurbs are opt-in roots included
//! separately. Those closures COMPOSE rather than nest — `gears.scad` has no includes at all and
//! assumes std is already there — which is why a root read takes a slice.
//!
//! COLLISIONS ARE THE INTERESTING PART. OpenSCAD hoists top-level definitions and lets the LAST
//! one win, and BOSL2 really does define the same name twice (`_sort_vectors` is the known case —
//! it cost us a real bug once already). A transpiler that picks either definition is guessing,
//! because which one a given user gets depends on their include graph, not on ours. So a colliding
//! name is recorded and REFUSED rather than resolved: the interpreter keeps handling it, which is
//! the same "worst case is a missed speedup, never a wrong answer" contract the fingerprint gate
//! has always run on.

#![allow(
    dead_code,
    reason = "AR.12 is the READ; AR.16 (const bakes) and AR.14 (the generated crate) are its \
              production consumers. The equivalence tests exercise it today."
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fab_lang::{Expr, Parameter, StmtKind, parse};

/// One top-level `function` the library declares.
#[derive(Debug, Clone)]
pub struct LibFn {
    /// The declared name.
    pub name: String,
    /// The file it came from, for diagnostics and for grouping generated output.
    pub file: String,
    /// Verbatim source of the whole `function … ;` statement — the same bytes the fingerprint
    /// gate would see, so a generated native and its reference cannot describe different code.
    pub source: String,
    /// Declared parameters, kept parsed so callers don't re-parse to learn the arity.
    pub params: Vec<Parameter>,
    /// The body expression.
    pub body: Expr,
}

/// One top-level `name = expr;` the library declares — the raw material for AR.16's const bakes.
#[derive(Debug, Clone)]
pub struct LibConst {
    pub name: String,
    pub file: String,
    /// Verbatim source of the right-hand side.
    pub source: String,
    pub value: Expr,
}

/// Everything one library declares at its top level, plus what it declares AMBIGUOUSLY.
#[derive(Debug, Default)]
pub struct Library {
    /// Unambiguous functions, by name.
    pub functions: BTreeMap<String, LibFn>,
    /// Unambiguous top-level constants, by name.
    pub constants: BTreeMap<String, LibConst>,
    /// Names declared more than once, each with every site that declared it. These are held OUT
    /// of `functions`/`constants` — see the module note; resolving them would be a guess.
    pub collisions: BTreeMap<String, Vec<String>>,
    /// Files that failed to parse, with the reason. Not fatal: a library may carry a file our
    /// grammar doesn't accept yet, and the rest of it is still transpilable.
    pub unparsed: Vec<(String, String)>,
    /// PROVENANCE: what each ROOT brings, keyed by the path a consumer writes in its `include`.
    ///
    /// Not bookkeeping — it is what makes a surface answerable. A user who writes
    /// `include <BOSL2/std.scad>` gets that file's whole transitive closure, 934 functions rather
    /// than the handful std.scad itself declares, so "include" means far more than "this file".
    /// A consumer asking "what may I call" has to be answered PER ROOT or the answer is wrong in
    /// both directions: a generated program that calls `spur_gear` after including only std.scad
    /// is broken (gears.scad is opt-in), and one restricted to std.scad's own declarations misses
    /// almost everything it should be exercising.
    ///
    /// BOSL2's opt-in files do NOT include std.scad — `gears.scad` has no includes at all and
    /// simply assumes std is already there — so these closures COMPOSE rather than nest, which is
    /// why a root read takes a slice.
    pub roots: BTreeMap<String, RootClosure>,
}

/// The names one root's include closure brings into scope.
#[derive(Debug, Default, Clone)]
pub struct RootClosure {
    /// The files reached, in read order — the root itself first.
    pub files: Vec<String>,
    pub functions: BTreeSet<String>,
    pub constants: BTreeSet<String>,
}

impl Library {
    /// Read a library from its ROOT file, following `include`/`use` transitively — the unit
    /// `fab_transpile!("BOSL2/std.scad")` will be handed, and the honest one: a user writes
    /// `include <BOSL2/std.scad>` and gets exactly this closure, not the directory.
    ///
    /// The distinction is not academic for BOSL2. `std.scad` reaches 30 of the 56 files; gears,
    /// screws, threading, nurbs and the rest are OPT-IN roots a user includes separately. A
    /// directory scan unions all 56 and so describes a program nobody has.
    ///
    /// `include` versus `use` is honored: `include` splices the file whole (functions AND
    /// top-level constants), `use` imports only its modules and functions and deliberately NOT its
    /// variables. Getting that backwards would bake a constant into a native that the user's
    /// program never binds.
    ///
    /// # Errors
    /// Every ROOT must be readable, and an empty root list is an error — both are the caller
    /// naming something that does not exist, and the alternative is handing back an empty library
    /// that looks like a successful read of a library with nothing in it. `fab_transpile!` with a
    /// typo'd path has to fail at the macro, not produce a crate with no functions in it.
    ///
    /// Files reached THROUGH a root are treated the other way: a read or parse failure lands in
    /// [`Library::unparsed`] and the walk continues, because a library may carry one file our
    /// grammar does not accept yet and the rest of it is still transpilable.
    pub fn read_from_roots(roots: &[&Path]) -> Result<Self, String> {
        if roots.is_empty() {
            return Err("no roots given — a library is read from at least one entry file".into());
        }
        for root in roots {
            if !root.is_file() {
                return Err(format!("root {} is not a readable file", root.display()));
            }
        }
        // A SLICE, not one path: `std.scad` reaches 30 of BOSL2's 56 files and the opt-in roots
        // (gears, screws, threading, nurbs …) are how a user gets the rest, so a real crate is
        // `fab_transpile!(["BOSL2/std.scad", "BOSL2/gears.scad"])`. The roots share one read and
        // one collision check — two roots that disagree about a name collide exactly as two
        // definitions in one file do — but each keeps its OWN closure, because that is what a
        // consumer's `include` line actually buys.
        let per_root: Vec<(String, Vec<(std::path::PathBuf, bool)>)> = roots
            .iter()
            .map(|r| (r.display().to_string(), Self::include_closure(r)))
            .collect();

        // The union, deduped, for the single read pass. A file reached by BOTH `include` and `use`
        // keeps the wider treatment: `include` brings constants, `use` does not.
        let mut union: BTreeMap<std::path::PathBuf, bool> = BTreeMap::new();
        for (_, files) in &per_root {
            for (path, take_consts) in files {
                let slot = union.entry(path.clone()).or_insert(false);
                *slot = *slot || *take_consts;
            }
        }
        let files: Vec<(std::path::PathBuf, bool)> = union.into_iter().collect();
        let mut out = Self::read_files(&files);

        // Attribute each declaration back to every root whose closure reaches its file.
        for (root, closure) in per_root {
            let names: BTreeSet<String> = closure
                .iter()
                .filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            let mut rc = RootClosure {
                files: names.iter().cloned().collect(),
                ..RootClosure::default()
            };
            for (name, f) in &out.functions {
                if names.contains(&f.file) {
                    rc.functions.insert(name.clone());
                }
            }
            for (name, c) in &out.constants {
                if names.contains(&c.file) {
                    rc.constants.insert(name.clone());
                }
            }
            out.roots.insert(root, rc);
        }
        Ok(out)
    }

    /// One root's transitive `include`/`use` closure, as `(path, do constants come along)`.
    /// Unreadable or unparseable files stop the walk at that edge and are reported by the read
    /// pass; they are not an error here, because a library with one bad file is still transpilable.
    fn include_closure(root: &Path) -> Vec<(std::path::PathBuf, bool)> {
        let mut pending: Vec<(std::path::PathBuf, bool)> = vec![(root.to_path_buf(), true)];
        let mut seen: BTreeMap<std::path::PathBuf, bool> = BTreeMap::new();
        while let Some((path, take_consts)) = pending.pop() {
            if let Some(prev) = seen.get_mut(&path) {
                *prev = *prev || take_consts;
                continue;
            }
            seen.insert(path.clone(), take_consts);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(prog) = parse(&text) else { continue };
            // Relative to the INCLUDING file, not to the root — BOSL2's files include their
            // siblings by bare name, and a multi-root read may span directories.
            let base = path
                .parent()
                .map_or_else(std::path::PathBuf::new, Path::to_path_buf);
            for stmt in &prog.stmts {
                let (rel, consts) = match &stmt.kind {
                    StmtKind::Include(p) => (p, take_consts),
                    StmtKind::Use(p) => (p, false),
                    _ => continue,
                };
                pending.push((base.join(rel), consts));
            }
        }
        seen.into_iter().collect()
    }

    /// Read every `*.scad` directly inside `dir` (NOT recursive — BOSL2 keeps its examples and
    /// tests in subdirectories and neither is part of the library surface).
    ///
    /// The WHOLE-directory view, which is what the coverage ratchet wants: it measures how much of
    /// the library the emitter owns, including the opt-in roots no single `include` reaches. Not
    /// what a transpiled crate is built from — see [`Library::read_from_root`].
    ///
    /// # Errors
    /// The directory must be readable. Individual files that fail to parse are collected into
    /// [`Library::unparsed`] rather than failing the read.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("read {}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .map(|p| (p, true))
            .collect();
        // Sorted so the read is deterministic — a BTreeMap keyed by name would hide file order,
        // but `collisions` records SITES and those must not reshuffle run to run.
        files.sort();
        Ok(Self::read_files(&files))
    }

    /// The shared pass: parse each file in order and collect its top-level declarations.
    /// `take_consts` is false for a file reached only by `use`, whose variables do not come along.
    fn read_files(files: &[(std::path::PathBuf, bool)]) -> Self {
        let mut out = Self::default();
        // Seen-anywhere sets, so a name that collides across two files is caught the same way as
        // one that collides inside a file. Kept separate from the output maps because a colliding
        // name has to LEAVE the output, and it may already have been inserted.
        let mut fn_sites: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut const_sites: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (path, take_consts) in files {
            let file = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            );
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    out.unparsed.push((file, format!("read: {e}")));
                    continue;
                }
            };
            let prog = match parse(&text) {
                Ok(p) => p,
                Err(e) => {
                    out.unparsed.push((file, format!("parse: {e:?}")));
                    continue;
                }
            };
            for stmt in &prog.stmts {
                match &stmt.kind {
                    StmtKind::FunctionDef { name, params, body } => {
                        fn_sites.entry(name.clone()).or_default().push(file.clone());
                        out.functions.insert(
                            name.clone(),
                            LibFn {
                                name: name.clone(),
                                file: file.clone(),
                                source: text[stmt.span.clone()].to_string(),
                                params: params.clone(),
                                body: body.clone(),
                            },
                        );
                    }
                    // `use <f>` imports a file's modules and functions and deliberately NOT its
                    // variables (lexer.l:153). Recording them anyway would let AR.16 bake a value
                    // the consumer's program never binds — the native would answer with a constant
                    // that, in the interpreter, is `undef`.
                    StmtKind::Assignment { name, value } if *take_consts => {
                        const_sites
                            .entry(name.to_string())
                            .or_default()
                            .push(file.clone());
                        out.constants.insert(
                            name.to_string(),
                            LibConst {
                                name: name.to_string(),
                                file: file.clone(),
                                source: text[value.span.clone()].to_string(),
                                value: value.clone(),
                            },
                        );
                    }
                    _ => {}
                }
            }
        }

        // Withdraw every ambiguous name. Done as a second pass because a collision is only known
        // once the whole library has been read, and the first definition was inserted before the
        // second one existed to contradict it.
        for (name, sites) in fn_sites {
            if sites.len() > 1 {
                out.functions.remove(&name);
                out.collisions.insert(name, sites);
            }
        }
        for (name, sites) in const_sites {
            if sites.len() > 1 {
                out.constants.remove(&name);
                out.collisions.insert(name, sites);
            }
        }
        out
    }

    /// A resolver for [`super::transpile::analyze_closed`]: a dep name to its verbatim source.
    /// Returns `None` for anything the library doesn't unambiguously declare — including every
    /// COLLIDING name, which the closure then treats as an unresolved dep. That is the safe
    /// direction: an unresolved dep stays on the guard list and the fingerprint gate settles it at
    /// arm time, rather than the analyzer silently closing over a body the user may not have.
    pub fn resolver<'a>(&'a self) -> impl Fn(&str) -> Option<&'a str> + 'a {
        move |name: &str| self.functions.get(name).map(|f| f.source.as_str())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: expect/panic ARE the assertions"
)]
mod tests {
    use std::path::PathBuf;

    use super::Library;

    fn bosl2() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libs/BOSL2")
    }

    /// The ROOT-relative read is a different library from the directory scan, and the difference is
    /// the point: `fab_transpile!(["BOSL2/std.scad"])` gets what `include <BOSL2/std.scad>` gets,
    /// which is 30 of the 56 files. The opt-in roots — gears, screws, threading, nurbs — are how a
    /// user reaches the rest, so a crate built from the directory would declare functions no single
    /// include ever brings into scope.
    #[test]
    fn a_root_read_is_the_include_closure_not_the_directory() {
        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let std_scad = dir.join("std.scad");
        let from_root = Library::read_from_roots(&[&std_scad]).expect("std.scad reads");
        let whole = Library::read(&dir).expect("directory reads");
        println!(
            "\n=== std.scad closure vs directory ===\nroot   {} functions, {} constants\ndir    {} functions, {} constants",
            from_root.functions.len(),
            from_root.constants.len(),
            whole.functions.len(),
            whole.constants.len()
        );
        assert!(
            from_root.functions.len() < whole.functions.len(),
            "std.scad must reach FEWER functions than the whole directory — if it reaches all of \
             them the include walk is not filtering and the two reads are the same thing"
        );
        assert!(
            from_root.functions.contains_key("is_finite"),
            "utility.scad is on std.scad's include path; missing it means the walk stopped short"
        );
        // gears.scad is opt-in: no `include` from std.scad reaches it.
        assert!(
            !from_root.functions.contains_key("spur_gear"),
            "gears.scad is NOT included by std.scad, so a root read must not declare its functions"
        );
        assert!(
            whole.functions.contains_key("spur_gear"),
            "the directory scan must still see the opt-in roots — that is what it is for"
        );
    }

    /// PROVENANCE: the registry knows which ROOT emitted what, so a consumer that includes one
    /// gets what it asked for. Two roots read together keep SEPARATE closures rather than merging
    /// into one bag — otherwise a generated program that includes only `std.scad` would be told it
    /// may call `spur_gear`, and a missing function costs a silently-absent PART rather than an
    /// error, which is the failure mode that is hardest to notice.
    #[test]
    fn each_root_reports_its_own_closure() {
        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let std_scad = dir.join("std.scad");
        let gears = dir.join("gears.scad");
        let lib = Library::read_from_roots(&[&std_scad, &gears]).expect("both roots read");

        let std_key = std_scad.display().to_string();
        let gears_key = gears.display().to_string();
        let s = lib.roots.get(&std_key).expect("std closure recorded");
        let g = lib.roots.get(&gears_key).expect("gears closure recorded");
        println!(
            "\n=== per-root closures ===\n{:>4} files {:>5} fns  std.scad\n{:>4} files {:>5} fns  gears.scad",
            s.files.len(),
            s.functions.len(),
            g.files.len(),
            g.functions.len()
        );

        // The union is readable as one library — both roots' functions are present…
        assert!(lib.functions.contains_key("spur_gear"), "gears was read");
        assert!(lib.functions.contains_key("is_finite"), "std was read");
        // …but the closures do NOT merge. gears.scad has no includes at all (BOSL2's opt-in files
        // assume std is already there), so its closure is itself alone.
        assert!(
            g.functions.contains("spur_gear") && !g.functions.contains("is_finite"),
            "gears.scad's closure must be gears.scad, not everything that was read alongside it"
        );
        assert!(
            s.functions.contains("is_finite") && !s.functions.contains("spur_gear"),
            "std.scad must not claim the opt-in roots' functions"
        );
        assert_eq!(
            g.files.len(),
            1,
            "gears.scad includes nothing: {:?}",
            g.files
        );
        assert!(
            s.files.len() > 25,
            "std.scad reaches ~30 files, found {}",
            s.files.len()
        );
    }

    /// A root that does not exist is an ERROR, not an empty library. `fab_transpile!` with a
    /// typo'd path must fail at the macro; handing back a library with no functions in it reads as
    /// a successful transpile of a library that declares nothing, and the cost of that mistake is
    /// every native silently not existing.
    #[test]
    fn a_missing_root_fails_rather_than_reading_empty() {
        let dir = bosl2();
        let err = Library::read_from_roots(&[&dir.join("no_such_file.scad")])
            .expect_err("a missing root must not read as empty");
        assert!(err.contains("not a readable file"), "{err}");
        let err = Library::read_from_roots(&[]).expect_err("no roots must not read as empty");
        assert!(err.contains("no roots given"), "{err}");
    }

    use fab_lang::{Expr, ExprKind, Stmt, StmtKind};

    /// AR.20 census helper: which of the module-only features does this expression reach?
    fn walk_expr(e: &Expr, seen: &mut std::collections::BTreeSet<&'static str>) {
        match &e.kind {
            ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
            ExprKind::Ident(n) => {
                if n.starts_with('$') {
                    seen.insert("reads a $-var");
                }
            }
            ExprKind::Unary { operand, .. } => walk_expr(operand, seen),
            ExprKind::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, seen);
                walk_expr(rhs, seen);
            }
            ExprKind::Ternary { cond, then, els } => {
                walk_expr(cond, seen);
                walk_expr(then, seen);
                walk_expr(els, seen);
            }
            ExprKind::Index { base, index } => {
                walk_expr(base, seen);
                walk_expr(index, seen);
            }
            ExprKind::Member { base, .. } => walk_expr(base, seen),
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident(n) = &callee.kind
                    && (n == "$children" || n == "children")
                {
                    seen.insert("children()/$children");
                }
                walk_expr(callee, seen);
                for a in args {
                    walk_expr(&a.value, seen);
                }
            }
            ExprKind::Vector(items) => {
                for i in items {
                    walk_expr(i, seen);
                }
            }
            ExprKind::Range { start, step, end } => {
                walk_expr(start, seen);
                if let Some(s) = step {
                    walk_expr(s, seen);
                }
                walk_expr(end, seen);
            }
            ExprKind::FunctionLiteral { params, body } => {
                for p in params {
                    if let Some(d) = &p.default {
                        walk_expr(d, seen);
                    }
                }
                walk_expr(body, seen);
            }
            ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
                for b in bindings {
                    walk_expr(&b.value, seen);
                }
                walk_expr(body, seen);
            }
            ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
                for a in args {
                    walk_expr(&a.value, seen);
                }
                if let Some(b) = body {
                    walk_expr(b, seen);
                }
            }
            ExprKind::LcForC {
                init,
                cond,
                update,
                body,
            } => {
                for b in init.iter().chain(update.iter()) {
                    walk_expr(&b.value, seen);
                }
                walk_expr(cond, seen);
                walk_expr(body, seen);
            }
            ExprKind::LcEach(inner) => walk_expr(inner, seen),
            ExprKind::LcIf { cond, then, els } => {
                walk_expr(cond, seen);
                walk_expr(then, seen);
                if let Some(e2) = els {
                    walk_expr(e2, seen);
                }
            }
        }
    }

    /// The statement half of the census walk — see [`walk_expr`].
    fn walk_stmt(s: &Stmt, seen: &mut std::collections::BTreeSet<&'static str>) {
        match &s.kind {
            // Nothing module-specific in any of these: an empty statement, and the two
            // file-level forms the census reads from the top level rather than from a body.
            StmtKind::Empty | StmtKind::Use(_) | StmtKind::Include(_) => {}
            StmtKind::Assignment { value, .. } => walk_expr(value, seen),
            StmtKind::Block(kids) => {
                for k in kids {
                    walk_stmt(k, seen);
                }
            }
            StmtKind::If {
                cond, then, els, ..
            } => {
                seen.insert("statement `if`");
                walk_expr(cond, seen);
                for k in then.iter().chain(els.iter()) {
                    walk_stmt(k, seen);
                }
            }
            StmtKind::Module(m) => {
                match m.name.as_str() {
                    "children" => {
                        seen.insert("children()/$children");
                    }
                    "for" | "intersection_for" => {
                        seen.insert("statement `for`");
                    }
                    "let" => {
                        seen.insert("statement `let`");
                    }
                    "assert" | "echo" => {
                        seen.insert("statement assert/echo");
                    }
                    _ => {
                        seen.insert("calls another module");
                    }
                }
                let md = m.modifiers;
                if md.root || md.highlight || md.background || md.disable {
                    seen.insert("modifier `! # % *`");
                }
                for a in &m.args {
                    if a.name.as_ref().is_some_and(|n| n.starts_with('$')) {
                        seen.insert("passes a $-var");
                    }
                    walk_expr(&a.value, seen);
                }
                for k in &m.children {
                    walk_stmt(k, seen);
                }
            }
            StmtKind::ModuleDef { body, .. } => {
                seen.insert("nested module def");
                walk_stmt(body, seen);
            }
            StmtKind::FunctionDef { body, .. } => {
                seen.insert("nested function def");
                walk_expr(body, seen);
            }
        }
    }

    /// AR.20's work list. Modules are IN SCOPE (chotchki: "yes modules are completely in scope and
    /// BOSL2 is worthless without them"), and the emission model has none of the evaluator context
    /// they need — so before designing that, count what they actually use.
    ///
    /// Same move that reframed the function side: the v0 emitter turned out to already handle 47%
    /// of functions, and the residual was four named bands rather than an open problem. This is the
    /// equivalent census for the statement half.
    ///
    /// `cargo nextest run -p fab-lang -E 'test(module_census)' --no-capture`
    #[test]
    fn the_module_census_says_what_statement_support_costs() {
        use std::collections::BTreeMap;

        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }

        // Tally per FEATURE, counting the modules that use it at least once — "how many modules
        // does this construct block" is the schedulable number, not the raw occurrence count.
        let mut used: BTreeMap<&'static str, usize> = BTreeMap::new();
        // (module name, its feature set) — the histogram says what is common, this says which
        // modules can be attempted FIRST.
        let mut combos: Vec<(String, std::collections::BTreeSet<&'static str>)> = Vec::new();
        let mut modules = 0_usize;
        let mut files = 0_usize;

        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .expect("BOSL2 checked out")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .collect();
        paths.sort();

        for path in &paths {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(prog) = fab_lang::parse(&text) else {
                continue;
            };
            files += 1;
            for stmt in &prog.stmts {
                let StmtKind::ModuleDef { name, body, .. } = &stmt.kind else {
                    continue;
                };
                modules += 1;
                let mut seen: std::collections::BTreeSet<&'static str> =
                    std::collections::BTreeSet::new();
                walk_stmt(body, &mut seen);
                for f in &seen {
                    *used.entry(*f).or_default() += 1;
                }
                combos.push((name.clone(), seen));
            }
        }

        let mut hist: Vec<_> = used.into_iter().collect();
        hist.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!("\n=== BOSL2 module census ===\n{modules} modules across {files} files");
        for (feature, n) in &hist {
            let pct = n * 100 / modules.max(1);
            println!("  {n:4} ({pct:3}%)  {feature}");
        }
        // THE SCHEDULING NUMBER. The per-feature counts above say what is common; this says
        // whether an INCREMENTAL start exists at all. On the function side it did — 47% compiled
        // before anything was widened — so bands could land one at a time. A module that neither
        // takes children nor calls another module is the equivalent starting set.
        let leaves: Vec<&str> = combos
            .iter()
            .filter(|(_, f)| {
                !f.contains("children()/$children") && !f.contains("calls another module")
            })
            .map(|(n, _)| n.as_str())
            .collect();
        let no_children: usize = combos
            .iter()
            .filter(|(_, f)| !f.contains("children()/$children"))
            .count();
        println!(
            "\nLEAF SET (no children, calls no module): {} of {modules}\n  {}",
            leaves.len(),
            leaves.join(", ")
        );
        println!("no children at all: {no_children} of {modules}");
        assert!(
            modules > 300,
            "BOSL2 has ~416 modules; found {modules} — the walk is not seeing them"
        );
    }

    /// AR.16's work list, derived rather than guessed: of the names BOSL2 functions read freely,
    /// which ones does the library DECLARE as a top-level constant (bakeable, once the emitter can
    /// evaluate them) and which does it not (a genuinely free read — a `$`-var, an upstream typo,
    /// or a name a user is expected to supply)?
    ///
    /// The split matters because the two have opposite fixes. A declared name is a bake plus a
    /// const guard. An undeclared one can NEVER be baked, so those functions stay interpreted no
    /// matter how good the emitter gets, and counting them tells us where AR.16's ceiling is.
    #[test]
    fn the_free_reads_split_into_bakeable_and_not() {
        use std::collections::BTreeMap;

        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = Library::read(&dir).expect("BOSL2 reads");
        let mut declared: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut undeclared: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for f in lib.functions.values() {
            let Ok(a) = crate::emit::analyze_function(&f.source) else {
                continue;
            };
            for name in a.consts {
                let bucket = if lib.constants.contains_key(&name) {
                    &mut declared
                } else {
                    &mut undeclared
                };
                let slot = bucket.entry(name).or_insert((0, String::new()));
                slot.0 += 1;
                if slot.1.is_empty() {
                    slot.1 = format!("{}:{}", f.file, f.name);
                }
            }
        }
        let rank = |m: &BTreeMap<String, (usize, String)>| {
            let mut v: Vec<_> = m
                .iter()
                .map(|(k, (n, ex))| (*n, k.clone(), ex.clone()))
                .collect();
            v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            v
        };
        println!(
            "\n=== free reads across {} BOSL2 functions ===",
            lib.functions.len()
        );
        println!(
            "BAKEABLE (the library declares it): {} distinct names",
            declared.len()
        );
        for (n, name, ex) in rank(&declared).into_iter().take(20) {
            println!("  {n:5}  {name}   e.g. {ex}");
        }
        println!(
            "NOT BAKEABLE (nothing declares it): {} distinct names",
            undeclared.len()
        );
        for (n, name, ex) in rank(&undeclared).into_iter().take(25) {
            println!("  {n:5}  {name}   e.g. {ex}");
        }
        assert!(
            !declared.is_empty(),
            "no free read resolves to a library constant — either the read or the analysis is \
             broken, since BOSL2's whole numeric surface is written against `_EPSILON`"
        );
    }

    /// The known trap, pinned: BOSL2 declares `_sort_vectors` twice, and OpenSCAD's last-wins
    /// hoisting means which body a user gets depends on their include graph. The read must REFUSE
    /// the name rather than pick one, because picking is a guess that reads correct in testing and
    /// wrong in somebody else's program.
    #[test]
    fn a_doubly_declared_name_is_refused_not_resolved() {
        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = Library::read(&dir).expect("BOSL2 reads");
        println!(
            "\n=== BOSL2 library read ===\n{} functions, {} constants, {} collisions, {} unparsed files",
            lib.functions.len(),
            lib.constants.len(),
            lib.collisions.len(),
            lib.unparsed.len()
        );
        for (name, sites) in &lib.collisions {
            println!("  COLLIDES  {name}  {sites:?}");
        }
        assert!(
            !lib.collisions.is_empty(),
            "BOSL2 has known double-declarations; finding none means the read is not seeing them"
        );
        for name in lib.collisions.keys() {
            assert!(
                !lib.functions.contains_key(name) && !lib.constants.contains_key(name),
                "`{name}` collides yet is still offered for transpilation"
            );
        }
    }
}
