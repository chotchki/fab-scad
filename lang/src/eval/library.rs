//! AR.12 — reading a LIBRARY, which is what the transpiler is actually pointed at.
//!
//! Until now the transpiler's input was `intrinsics::REGISTRY`: a hand-typed list of ~55
//! functions, each carrying a `reference` string somebody copied out of BOSL2 by hand. That input
//! cannot describe a library — it describes the functions a person already transcribed — so
//! "generate a crate per library" has nowhere to start. This module is the other input: point it
//! at a directory of `.scad` and get back every top-level function and constant the library
//! declares, verbatim, with the collisions called out.
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

use std::collections::BTreeMap;
use std::path::Path;

use crate::parser::{Expr, Parameter, StmtKind, parse};

/// One top-level `function` the library declares.
#[derive(Debug, Clone)]
pub(crate) struct LibFn {
    /// The declared name.
    pub(crate) name: String,
    /// The file it came from, for diagnostics and for grouping generated output.
    pub(crate) file: String,
    /// Verbatim source of the whole `function … ;` statement — the same bytes the fingerprint
    /// gate would see, so a generated native and its reference cannot describe different code.
    pub(crate) source: String,
    /// Declared parameters, kept parsed so callers don't re-parse to learn the arity.
    pub(crate) params: Vec<Parameter>,
    /// The body expression.
    pub(crate) body: Expr,
}

/// One top-level `name = expr;` the library declares — the raw material for AR.16's const bakes.
#[derive(Debug, Clone)]
pub(crate) struct LibConst {
    pub(crate) name: String,
    pub(crate) file: String,
    /// Verbatim source of the right-hand side.
    pub(crate) source: String,
    pub(crate) value: Expr,
}

/// Everything one library declares at its top level, plus what it declares AMBIGUOUSLY.
#[derive(Debug, Default)]
pub(crate) struct Library {
    /// Unambiguous functions, by name.
    pub(crate) functions: BTreeMap<String, LibFn>,
    /// Unambiguous top-level constants, by name.
    pub(crate) constants: BTreeMap<String, LibConst>,
    /// Names declared more than once, each with every site that declared it. These are held OUT
    /// of `functions`/`constants` — see the module note; resolving them would be a guess.
    pub(crate) collisions: BTreeMap<String, Vec<String>>,
    /// Files that failed to parse, with the reason. Not fatal: a library may carry a file our
    /// grammar doesn't accept yet, and the rest of it is still transpilable.
    pub(crate) unparsed: Vec<(String, String)>,
}

impl Library {
    /// Read every `*.scad` directly inside `dir` (NOT recursive — BOSL2 keeps its examples and
    /// tests in subdirectories and neither is part of the library surface).
    ///
    /// # Errors
    /// The directory must be readable. Individual files that fail to parse are collected into
    /// [`Library::unparsed`] rather than failing the read.
    pub(crate) fn read(dir: &Path) -> Result<Self, String> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("read {}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .collect();
        // Sorted so the read is deterministic — a BTreeMap keyed by name would hide file order,
        // but `collisions` records SITES and those must not reshuffle run to run.
        files.sort();

        let mut out = Self::default();
        // Seen-anywhere sets, so a name that collides across two files is caught the same way as
        // one that collides inside a file. Kept separate from the output maps because a colliding
        // name has to LEAVE the output, and it may already have been inserted.
        let mut fn_sites: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut const_sites: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for path in &files {
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
                    StmtKind::Assignment { name, value } => {
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
        Ok(out)
    }

    /// A resolver for [`super::transpile::analyze_closed`]: a dep name to its verbatim source.
    /// Returns `None` for anything the library doesn't unambiguously declare — including every
    /// COLLIDING name, which the closure then treats as an unresolved dep. That is the safe
    /// direction: an unresolved dep stays on the guard list and the fingerprint gate settles it at
    /// arm time, rather than the analyzer silently closing over a body the user may not have.
    pub(crate) fn resolver<'a>(&'a self) -> impl Fn(&str) -> Option<&'a str> + 'a {
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

    /// The library read reproduces what the HAND registry says — which is AR.12's whole claim, and
    /// the only way to know the new input is equivalent to the old one before the old one goes
    /// away. For every registry entry whose function BOSL2 declares, the pinned `reference` must
    /// fingerprint identically to what the library read found.
    ///
    /// Fingerprint rather than byte-compare on purpose: the hand references were transcribed, so
    /// some carry reflowed whitespace, and the fingerprint is exactly the identity the dispatch
    /// gate uses. Two functions fingerprinting equal ARE the same function to us.
    #[test]
    fn hand_references_match_the_pinned_library() {
        let dir = bosl2();
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = Library::read(&dir).expect("BOSL2 reads");
        let mut checked = 0_usize;
        let mut drifted = Vec::new();
        for entry in super::super::intrinsics::REGISTRY {
            let Some(found) = lib.functions.get(entry.name) else {
                continue; // our own POCs, and anything the library declares ambiguously
            };
            let hand = crate::parser::parse(entry.reference).expect("reference parses");
            let Some(crate::parser::StmtKind::FunctionDef { params, body, .. }) =
                hand.stmts.first().map(|s| &s.kind)
            else {
                panic!("{}: reference holds no function definition", entry.name);
            };
            checked += 1;
            if super::super::intrinsics::fingerprint::fingerprint(params, body)
                != super::super::intrinsics::fingerprint::fingerprint(&found.params, &found.body)
            {
                drifted.push(format!("{} (from {})", entry.name, found.file));
            }
        }
        println!(
            "\n=== hand references vs pinned BOSL2 ===\n{checked} of {} registry entries resolve \
             against the library, {} drifted",
            super::super::intrinsics::REGISTRY.len(),
            drifted.len()
        );
        assert!(
            drifted.is_empty(),
            "{} hand reference(s) no longer match the pinned BOSL2: {drifted:?}. \
             A drifted reference is a native that is gated on source the library does not \
             contain, so it never wires — a silent dead intrinsic, not a wrong answer.",
            drifted.len()
        );
        assert!(
            checked > 40,
            "only {checked} registry entries resolved against the library — the read is finding \
             far too little to be a meaningful equivalence check"
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
            let Ok(a) = super::super::transpile::analyze_function(&f.source) else {
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
