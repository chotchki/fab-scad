//! AN.15.1 — STATIC diagnostics: the warnings upstream emits while BUILDING scopes, before a single
//! statement executes.
//!
//! Only one lives here so far, `"a" was assigned on line N but was overwritten`. It belongs in a static
//! pass rather than in the evaluator because of two oracle-observed facts that eval simply cannot
//! reproduce: it fires for a module body that is never CALLED, and it lands ahead of ALL console output
//! even when the program's first statement is an `echo`. Upstream gets both for free by warning in
//! `LocalScope::addAssignment` as the parser reduces; we get them by walking the AST once, up front, and
//! seeding the message log with the result.
//!
//! The line in the message is the FIRST assignment's — upstream keeps the original `Assignment` object
//! and swaps only its expression, so three assignments to one name produce two warnings that both cite
//! line 1. The trailing ` in file …, line N` (the OVERWRITING site) is upstream's uniform location
//! suffix, which `differ::normalize_warning` strips, so we don't render it.
//!
//! MULTI-FILE (AQ.1): a collision can straddle files, because `include` splices into the INCLUDING
//! file's scope. Which of four things upstream then prints is decided by [`verdict`], transcribed from
//! `handle_assignment` in `src/core/parser.y` — a branch chain whose behaviour is NOT guessable from the
//! output alone (a dozen probes failed; the source took minutes). Node index stands in for upstream's
//! path comparison, since the loader dedups files by canonical path and node 0 is `mainFilePath`.

use crate::parser::{Stmt, StmtKind};

use super::Message;

/// One name assigned more than once in a single scope.
struct Overwrite<'a> {
    /// Byte offset of the OVERWRITING assignment — the emission-order key (see [`overwritten_assignments`]).
    at: usize,
    name: &'a str,
    /// Byte offset of the FIRST assignment, the one the message names.
    first: usize,
    /// File the FIRST assignment came from (`prevFile` upstream).
    first_file: usize,
    /// File the OVERWRITING assignment came from (`currFile` upstream).
    at_file: usize,
}

/// Which of upstream's four `handle_assignment` outcomes this collision takes (AQ.1).
///
/// Transcribed from `src/core/parser.y`, in that function's exact order — the branches are NOT
/// independent and reordering them changes the answer. Node index stands in for upstream's path
/// comparison: the loader dedups files by canonical path, so "same node" IS "same file", and node 0 IS
/// `mainFilePath`.
enum Verdict {
    /// Both sides in the MAIN file → the bare message, no path.
    Bare,
    /// Same file, different lines → the message names that file. The EQUAL-line case is upstream's
    /// explicit guard ("the line number being equal happens, when a file is included multiple times")
    /// and produces [`Verdict::Silent`] — which is why including a one-assignment file twice says nothing.
    Named(usize),
    /// Nothing is printed. Covers the equal-line multi-include case AND the fall-off-the-end case
    /// upstream leaves with no `else`: a first assignment OUTSIDE main overwritten from INSIDE main. That
    /// absent branch is why a root's own duplicate goes quiet once an `include` precedes it.
    Silent,
}

/// Apply upstream's branch chain to one collision.
fn verdict(o: &Overwrite<'_>, first_line: u32, at_line: u32) -> Verdict {
    const MAIN: usize = 0;
    if o.first_file == MAIN && o.at_file == MAIN {
        Verdict::Bare
    } else if o.first_file == o.at_file {
        // Same file, not main. Upstream names it — even though both sides ARE that one file, which is
        // why a library-internal duplicate carries a path where a root-internal one does not.
        if first_line == at_line {
            Verdict::Silent
        } else {
            Verdict::Named(o.first_file)
        }
    } else if o.first_file == MAIN {
        // Main's assignment overwritten from an include. Upstream passes `uncPathPrev` here — the path of
        // the FIRST side, i.e. MAIN's own — so the message names the root file, not the include.
        Verdict::Named(MAIN)
    } else {
        Verdict::Silent // no `else` upstream
    }
}

/// Every `"name" was assigned on line N but was overwritten` warning `stmts` earns, in upstream's order.
///
/// ORDER is the subtle part: warnings sort by the OVERWRITING assignment's position, across scopes, not
/// by scope. Upstream emits as the parser reduces, so in
/// `a = 1; module m() { b = 1; b = 2; } a = 2;` the `b` warning (overwritten on line 2) precedes the `a`
/// one (overwritten on line 3) even though `a`'s scope is the outer one. A scope-at-a-time walk gets that
/// backwards; sorting the whole set by byte offset reproduces parse order exactly.
pub(super) fn overwritten_assignments(
    scopes: Vec<Vec<(&Stmt, usize)>>,
    files: &[FileInfo<'_>],
) -> Vec<Message> {
    // The ROOT scope first (its `include`s spliced in, so a collision can straddle two files), then each
    // directly-`use`d file's own scope. Within a scope, sort by the OVERWRITING position; ACROSS scopes,
    // keep the order the loader hands them back — root, then uses in reverse source order, which is
    // upstream's `usedlibs` front-insert (oracle-verified: two `use`s report last-first).
    let mut out = Vec::new();
    for scope in scopes {
        let mut found = scan_scopes(scope);
        found.sort_by_key(|o| o.at);
        out.extend(found.into_iter().filter_map(|o| render(&o, files)));
    }
    out
}

/// What a diagnostic needs to know about one file: its text (the line table) and how upstream NAMES it.
pub(super) struct FileInfo<'a> {
    pub source: &'a str,
    /// The file's path relative to the MAIN file's parent dir; `None` for a root with no path of its own.
    pub path: Option<String>,
}

/// One collision → its message, or `None` when upstream stays quiet.
fn render(o: &Overwrite<'_>, files: &[FileInfo<'_>]) -> Option<Message> {
    let text = |i: usize| files.get(i).map_or("", |f| f.source);
    let first_line = crate::offset_to_line(text(o.first_file), o.first);
    let at_line = crate::offset_to_line(text(o.at_file), o.at);
    let name = o.name;
    match verdict(o, first_line, at_line) {
        Verdict::Silent => None,
        Verdict::Bare => Some(Message::Warning(format!(
            "\"{name}\" was assigned on line {first_line} but was overwritten"
        ))),
        // A file with no path (a ROOT handed in as a bare string) can't be named; upstream would have
        // substituted the CWD there. Falling back to the bare form keeps the line right rather than
        // inventing a path — the naming half of this case simply isn't reachable without a real root.
        Verdict::Named(file) => Some(Message::Warning(
            match files.get(file).and_then(|f| f.path.clone()) {
                Some(path) => format!(
                    "\"{name}\" was assigned on line {first_line} of \"{path}\" but was overwritten"
                ),
                None => format!("\"{name}\" was assigned on line {first_line} but was overwritten"),
            },
        )),
    }
}

/// Scan every scope reachable from `stmts` for repeated assignment names.
///
/// What counts as a scope is oracle-pinned: a module BODY, each branch of an `if`, and a module call's
/// CHILDREN each get their own (an assignment in one never collides with the enclosing file's). A bare
/// `{ … }` does NOT — it folds into its parent, which is exactly what [`flatten_blocks`] already does for
/// the hoist, so the two agree by construction instead of by coincidence.
///
/// A WORKLIST of scopes, not a recursive descent: statement nesting is source-controlled, and this
/// crate's rule (the M.3 explicit-stack driver, the iterative `Drop`s) is that AST walks stay off the
/// host stack. Discovery order doesn't matter — the caller sorts by source position.
fn scan_scopes<'a>(stmts: Vec<(&'a Stmt, usize)>) -> Vec<Overwrite<'a>> {
    let mut found = Vec::new();
    let mut pending = vec![stmts];
    while let Some(scope) = pending.pop() {
        let flat = flatten_tagged(scope);
        // First occurrence of each name in THIS scope, with the file it came from. A Vec, not a map:
        // scopes are small, and it keeps the source order the caller's sort keys off.
        let mut first: Vec<(&'a str, usize, usize)> = Vec::new();
        for (stmt, file) in flat {
            match &stmt.kind {
                StmtKind::Assignment { name, .. } => {
                    match first.iter().find(|(seen, ..)| *seen == &**name) {
                        Some(&(name, at, first_file)) => found.push(Overwrite {
                            at: stmt.span.start,
                            name,
                            first: at,
                            first_file,
                            at_file: file,
                        }),
                        None => first.push((name, stmt.span.start, file)),
                    }
                }
                // A nested scope lives wholly inside ONE file — `include` is top-level only — so every
                // statement in it inherits the enclosing statement's file.
                StmtKind::ModuleDef { body, .. } => pending.push(vec![(&**body, file)]),
                StmtKind::If { then, els, .. } => {
                    pending.push(then.iter().map(|s| (s, file)).collect());
                    pending.push(els.iter().map(|s| (s, file)).collect());
                }
                StmtKind::Module(mi) => {
                    pending.push(mi.children.iter().map(|s| (s, file)).collect());
                }
                _ => {}
            }
        }
    }
    found
}

/// [`flatten_blocks`] carrying each statement's file along — a bare `{ … }` folds into its parent scope
/// and keeps the file it came from.
fn flatten_tagged(stmts: Vec<(&Stmt, usize)>) -> Vec<(&Stmt, usize)> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Stmt, usize)> = stmts.into_iter().rev().collect();
    while let Some((s, file)) = stack.pop() {
        if let StmtKind::Block(inner) = &s.kind {
            stack.extend(inner.iter().rev().map(|i| (i, file)));
        } else {
            out.push((s, file));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::overwritten_assignments;
    use crate::parser::{Stmt, parse};

    /// The warnings a SINGLE-file program earns — everything tagged file 0, which is `mainFilePath`, so
    /// every collision takes the `Bare` branch. The multi-file branches need a real loader graph and are
    /// covered end-to-end in `lang/tests/dup_assign_multifile.rs`.
    fn warnings(src: &str) -> Vec<String> {
        let program = parse(src).expect("parses");
        let scope: Vec<(&Stmt, usize)> = program.stmts.iter().map(|s| (s, 0)).collect();
        let files = [super::FileInfo {
            source: src,
            path: None,
        }];
        overwritten_assignments(vec![scope], &files)
            .iter()
            .map(crate::eval::Message::render)
            .collect()
    }

    #[test]
    fn a_repeat_names_the_first_assignments_line() {
        assert_eq!(
            warnings("a = 1;\nb = 2;\na = 3;\n"),
            ["WARNING: \"a\" was assigned on line 1 but was overwritten"]
        );
    }

    #[test]
    fn three_assignments_warn_twice_both_citing_the_first() {
        // Upstream keeps the original Assignment and swaps its expr, so the cited line never advances.
        assert_eq!(
            warnings("a = 1;\na = 2;\na = 3;\n"),
            [
                "WARNING: \"a\" was assigned on line 1 but was overwritten",
                "WARNING: \"a\" was assigned on line 1 but was overwritten",
            ]
        );
    }

    #[test]
    fn an_uncalled_module_body_still_warns() {
        // The whole reason this is a static pass and not an eval-time warning.
        assert_eq!(
            warnings("module m() { a = 1; a = 3; }\n"),
            ["WARNING: \"a\" was assigned on line 1 but was overwritten"]
        );
    }

    #[test]
    fn emission_order_follows_the_overwriting_site_across_scopes() {
        // `b`'s overwrite is on line 2, `a`'s on line 3 — so `b` comes first, even though `a`'s scope
        // is the outer one and opened earlier. A scope-at-a-time walk would emit these backwards.
        assert_eq!(
            warnings("a = 1;\nmodule m() { b = 1; b = 2; }\na = 2;\n"),
            [
                "WARNING: \"b\" was assigned on line 2 but was overwritten",
                "WARNING: \"a\" was assigned on line 1 but was overwritten",
            ]
        );
    }

    #[test]
    fn nested_scopes_do_not_collide_but_a_bare_block_folds_in() {
        assert!(warnings("a = 1;\nif (true) { a = 2; }\n").is_empty());
        assert!(warnings("a = 1;\nmodule m() { a = 2; }\n").is_empty());
        assert!(warnings("a = 1;\ntranslate([0, 0, 0]) { a = 2; }\n").is_empty());
        // A bare block is NOT a scope — it flattens into its parent, so this one DOES collide.
        assert_eq!(
            warnings("a = 1;\n{ a = 2; }\n"),
            ["WARNING: \"a\" was assigned on line 1 but was overwritten"]
        );
    }

    #[test]
    fn each_nested_scope_is_scanned_on_its_own() {
        assert_eq!(
            warnings("if (true) { a = 1; a = 2; }\n"),
            ["WARNING: \"a\" was assigned on line 1 but was overwritten"]
        );
        assert_eq!(
            warnings("translate([0, 0, 0]) { a = 1; a = 2; }\n"),
            ["WARNING: \"a\" was assigned on line 1 but was overwritten"]
        );
    }
}
