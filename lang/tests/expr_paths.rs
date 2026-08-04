//! AR.17.2 — CHILD-INDEX PATH ADDRESSING, the invariant the transpiled band's function literals ride on.
//!
//! `expr_children` is the single canonical enumeration of an expression's children, and two
//! independent consumers depend on it agreeing with itself across processes:
//!
//!   - the TRANSPILER (`fab-lib`) computes a child-index path to a `FunctionLiteral` against ITS
//!     parse of the reference source, at build time, and bakes that path into the emitted crate;
//!   - the EVALUATOR resolves the same path against the USER's definition, at run time.
//!
//! Fingerprint equality proves the two parses are structurally identical, so the path transfers iff
//! both sides enumerate children in the same order. There is one `expr_children`, so "both sides
//! agree" is really "this function is deterministic and its inverse is `expr_child`" — which is what
//! this file asserts, on real parser output rather than hand-built nodes.
//!
//! WHY THIS FILE EXISTS NOW. These three helpers used to be covered incidentally: the Cranelift JIT
//! lived inside fab-lang and walked the AST constantly, so fab-lang's own tests exercised every
//! match arm. AR.21 deleted the JIT (11,306 lines) and the helpers kept every one of their callers —
//! `lib/src/emit.rs`, `eval/mod.rs`, `eval/module_rt.rs` — but ALL of them now live outside this
//! crate, where `cargo llvm-cov -p fab-lang` cannot see them. `parser/ast.rs` fell to 78% line
//! coverage and took the 99% gate with it. The honest fix is not a lower floor: it is that a
//! function the transpiler's correctness rests on had no direct test, and now does.
//!
//! A wrong ordering here is not a crash. It silently resolves a path to the WRONG subexpression, so
//! a native gets wired to a function literal that is not the one it was compiled from — which the
//! fingerprint gate cannot catch, because the fingerprint is of the whole definition.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test: expect/unwrap/panic ARE the assertions"
)]

use fab_lang::{Expr, ExprKind, StmtKind, expr_child, expr_children, find_expr_path, parse};

/// Parse a single expression out of `v = <src>;`.
fn parse_expr(src: &str) -> Expr {
    let prog = parse(&format!("v={src};")).expect("expression parses");
    match prog.stmts.into_iter().next().map(|s| s.kind) {
        Some(StmtKind::Assignment { value, .. }) => value,
        other => panic!("expected an assignment, got {other:?}"),
    }
}

/// Every node in the tree, parents before children.
fn all_nodes(root: &Expr) -> Vec<&Expr> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        out.extend(expr_children(out[i]));
        i += 1;
    }
    out
}

/// Resolve a child-index path, the way the evaluator does.
fn resolve<'a>(root: &'a Expr, path: &[usize]) -> Option<&'a Expr> {
    let mut node = root;
    for &i in path {
        node = expr_child(node, i)?;
    }
    Some(node)
}

/// One expression per `ExprKind` variant, so the walk below covers every match arm in
/// `expr_children`. Ranges appear BOTH with and without a step, and `assert`/`echo`/`LcIf` both with
/// and without their optional tail — those `Option` branches are separate arms in the enumeration
/// and a path lands differently depending on which way they went.
const SOURCES: &[&str] = &[
    // leaves — no children at all
    "1",
    "\"s\"",
    "true",
    "undef",
    "x",
    // operators
    "-x",
    "a + b",
    "a ? b : c",
    "v[1]",
    "p.x",
    // calls, vectors, ranges (step present AND absent)
    "f(1, k = 2)",
    "[1, 2, 3]",
    "[0 : 10]",
    "[0 : 2 : 10]",
    // binding forms
    "function (a, b = 2) a + b",
    "let (a = 1, b = 2) a + b",
    "assert(a > 0) x",
    "assert(a > 0)",
    "echo(a, b) x",
    "echo(a)",
    // list-comprehension forms
    "[for (i = [0:3]) i * 2]",
    "[for (i = 0; i < 5; i = i + 1) i]",
    "[each [1, 2], 3]",
    "[for (i = [0:3]) if (i > 1) i]",
    "[for (i = [0:3]) if (i > 1) i else -i]",
    // a nest deep enough that paths are more than one element long
    "let (f = function (n) [for (i = [0:n]) if (i % 2) i * 2 else 0]) f(4)",
];

/// `expr_child(e, i)` IS `expr_children(e)[i]`, by identity, for every node and every index — and
/// `None` exactly past the end. `expr_child` is a one-line `nth`, so this is really asserting that
/// the two never drift apart if one of them is ever optimised.
#[test]
fn expr_child_indexes_expr_children_exactly() {
    for src in SOURCES {
        let root = parse_expr(src);
        for node in all_nodes(&root) {
            let kids = expr_children(node);
            for (i, kid) in kids.iter().enumerate() {
                let got = expr_child(node, i)
                    .unwrap_or_else(|| panic!("{src}: expr_child({i}) is None but a child exists"));
                assert!(
                    core::ptr::eq(got, *kid),
                    "{src}: expr_child({i}) is a different node than expr_children()[{i}]"
                );
            }
            assert!(
                expr_child(node, kids.len()).is_none(),
                "{src}: expr_child past the last child must be None, got a node ({} children)",
                kids.len()
            );
        }
    }
}

/// THE ROUND TRIP, which is the property the transpiler actually needs: every node in the tree is
/// reachable by the path `find_expr_path` reports for it, and that path resolves back to the SAME
/// node. Identity, not equality — `[f(), f()]` holds two structurally equal literals that are
/// different mint targets, and confusing them is precisely the bug this addressing scheme exists to
/// avoid.
#[test]
fn every_node_round_trips_through_its_path() {
    for src in SOURCES {
        let root = parse_expr(src);
        for node in all_nodes(&root) {
            let path = find_expr_path(&root, node)
                .unwrap_or_else(|| panic!("{src}: no path to a node that IS in the tree"));
            let back = resolve(&root, &path)
                .unwrap_or_else(|| panic!("{src}: path {path:?} does not resolve"));
            assert!(
                core::ptr::eq(back, node),
                "{src}: path {path:?} resolved to a DIFFERENT node — a native would wire to the \
                 wrong subexpression here, and the fingerprint gate cannot see it"
            );
        }
    }
}

/// Identity, not structural equality. Both elements of `[f(x), f(x)]` are equal trees; they must get
/// DIFFERENT paths, and a node from one tree must not be found in another that merely looks alike.
#[test]
fn paths_discriminate_structurally_equal_siblings() {
    let root = parse_expr("[f(x), f(x)]");
    let kids = expr_children(&root);
    assert_eq!(kids.len(), 2, "the vector has two elements");
    let (a, b) = (
        find_expr_path(&root, kids[0]).expect("first element has a path"),
        find_expr_path(&root, kids[1]).expect("second element has a path"),
    );
    assert_ne!(
        a, b,
        "two structurally equal siblings got the SAME path — they are different mint targets"
    );

    // A node from an equal-but-separate parse is not in this tree.
    let other = parse_expr("[f(x), f(x)]");
    assert!(
        find_expr_path(&root, &other).is_none(),
        "a node from a DIFFERENT parse must not be found by identity"
    );
}

/// The documented orderings, spelled out. The walk above proves self-consistency; this pins the
/// actual order, which is the half that has to match the transpiler's expectations rather than just
/// match itself.
#[test]
fn children_come_out_in_source_order() {
    let cases: &[(&str, usize)] = &[
        ("1", 0),
        ("-x", 1),
        ("a + b", 2),
        ("a ? b : c", 3),
        ("v[1]", 2),
        ("p.x", 1),
        ("f(1, k = 2)", 3), // callee + two args
        ("[1, 2, 3]", 3),
        ("[0 : 10]", 2),      // start + end, no step
        ("[0 : 2 : 10]", 3),  // start + step + end
        ("assert(a > 0)", 1), // no body
        ("assert(a > 0) x", 2),
        ("[each [1, 2], 3]", 2), // the comprehension vector: an Each and a literal
    ];
    for (src, want) in cases {
        let root = parse_expr(src);
        assert_eq!(
            expr_children(&root).len(),
            *want,
            "{src}: wrong child count"
        );
    }

    // Ternary is (cond, then, els) in that order — an ordering the transpiler assumes.
    let t = parse_expr("a ? b : c");
    let kids = expr_children(&t);
    let name = |e: &Expr| match &e.kind {
        ExprKind::Ident(n) => n.clone(),
        other => panic!("expected an identifier, got {other:?}"),
    };
    assert_eq!(
        [name(kids[0]), name(kids[1]), name(kids[2])],
        ["a".to_string(), "b".to_string(), "c".to_string()],
        "ternary children must enumerate cond, then, els"
    );

    // Binary is (lhs, rhs), not the other way round.
    let b = parse_expr("a - b");
    let kids = expr_children(&b);
    assert_eq!(
        [name(kids[0]), name(kids[1])],
        ["a".to_string(), "b".to_string()],
        "binary children must enumerate lhs then rhs"
    );

    // A function literal enumerates its parameter DEFAULTS before its body — the ordering that
    // decides where a literal nested in a default is addressed.
    let f = parse_expr("function (a, b = 2) a + b");
    assert_eq!(
        expr_children(&f).len(),
        2,
        "one defaulted parameter plus the body"
    );
}

/// A node that is not in the tree has no path — the negative arm of `find_expr_path`, and the one
/// that keeps the round-trip test from being vacuous.
#[test]
fn a_foreign_node_has_no_path() {
    let root = parse_expr("a + b");
    let foreign = parse_expr("c * d");
    assert!(
        find_expr_path(&root, &foreign).is_none(),
        "a node from another tree must not resolve"
    );
    for child in expr_children(&foreign) {
        assert!(
            find_expr_path(&root, child).is_none(),
            "a CHILD from another tree must not resolve either"
        );
    }
}
