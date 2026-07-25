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
//! ROOT FILE ONLY. A `use`d/`include`d file's duplicates warn upstream too, but with the defining path
//! spliced INTO the body (`… was assigned on line 1 of "lib/dup.scad" but …`) — a different string, on a
//! different line table, emitted once per inclusion. That's AN.15.3.

use crate::parser::{Stmt, StmtKind};

use super::Message;
use super::flatten_blocks;

/// One name assigned more than once in a single scope.
struct Overwrite<'a> {
    /// Byte offset of the OVERWRITING assignment — the emission-order key (see [`overwritten_assignments`]).
    at: usize,
    name: &'a str,
    /// Byte offset of the FIRST assignment, the one the message names.
    first: usize,
}

/// Every `"name" was assigned on line N but was overwritten` warning `stmts` earns, in upstream's order.
///
/// ORDER is the subtle part: warnings sort by the OVERWRITING assignment's position, across scopes, not
/// by scope. Upstream emits as the parser reduces, so in
/// `a = 1; module m() { b = 1; b = 2; } a = 2;` the `b` warning (overwritten on line 2) precedes the `a`
/// one (overwritten on line 3) even though `a`'s scope is the outer one. A scope-at-a-time walk gets that
/// backwards; sorting the whole set by byte offset reproduces parse order exactly.
pub(super) fn overwritten_assignments(stmts: &[Stmt], source: &str) -> Vec<Message> {
    let refs: Vec<&Stmt> = stmts.iter().collect();
    let mut found = scan_scopes(refs);
    found.sort_by_key(|o| o.at);
    found
        .into_iter()
        .map(|o| {
            let line = crate::offset_to_line(source, o.first);
            Message::Warning(format!(
                "\"{}\" was assigned on line {line} but was overwritten",
                o.name
            ))
        })
        .collect()
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
fn scan_scopes<'a>(stmts: Vec<&'a Stmt>) -> Vec<Overwrite<'a>> {
    let mut found = Vec::new();
    let mut pending = vec![stmts];
    while let Some(scope) = pending.pop() {
        let flat = flatten_blocks(&scope);
        // First occurrence of each name in THIS scope. A Vec, not a map: scopes are small, and it keeps
        // the source order the caller's sort keys off.
        let mut first: Vec<(&'a str, usize)> = Vec::new();
        for stmt in flat {
            match &stmt.kind {
                StmtKind::Assignment { name, .. } => {
                    match first.iter().find(|(seen, _)| *seen == &**name) {
                        Some(&(name, at)) => found.push(Overwrite {
                            at: stmt.span.start,
                            name,
                            first: at,
                        }),
                        None => first.push((name, stmt.span.start)),
                    }
                }
                StmtKind::ModuleDef { body, .. } => pending.push(vec![body]),
                StmtKind::If { then, els, .. } => {
                    pending.push(then.iter().collect());
                    pending.push(els.iter().collect());
                }
                StmtKind::Module(mi) => pending.push(mi.children.iter().collect()),
                _ => {}
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::overwritten_assignments;
    use crate::parser::parse;

    fn warnings(src: &str) -> Vec<String> {
        let program = parse(src).expect("parses");
        overwritten_assignments(&program.stmts, src)
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
