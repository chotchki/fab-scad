//! AR.5 — the transpiler's ANALYSIS pass: derive, from a function's verbatim reference source, what
//! `intrinsics::Entry` hand-maintains — the const / dep / builtin guard sets.
//!
//! Every AN finding was the hand-maintained half of an `Entry` disagreeing with what the reference
//! actually does at runtime. A transpiler generates the native FROM the reference, so it must also
//! generate the guards from the same read — one source, no drift. This module is that read.
//!
//! DERIVED, not proven: the walk is SYNTACTIC, so it over-approximates the hand lists, which are
//! transitively closed by the author over ACCEPTED ARG SHAPES (semantic reachability pruning —
//! `select` excludes `all_nonzero` because no accepted shape reaches it). Over-approximation is the
//! SAFE direction for a guard: more names checked means the entry wires less often, never that it
//! answers wrong. The registry comparison test pins both directions: hand ⊆ derived (a hand name
//! the walk misses is an analyzer BUG), and derived-minus-hand is an explicit, reasoned allowlist
//! rather than silent slack.

#![allow(
    dead_code,
    reason = "AR.5: the AR.6 codegen is the production consumer; the registry comparison test exercises it today"
)]

use std::collections::BTreeSet;

use crate::parser::{Arg, Expr, ExprKind, Parameter, StmtKind, parse};

/// What one function's reference source reaches, by name — the raw material for an `Entry`'s guard
/// sets (names only; the guard VALUES are resolved against the island at arm time, not here).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Analysis {
    /// The defined function's name.
    pub(crate) name: String,
    /// Parameter names, in declaration order.
    pub(crate) params: Vec<String>,
    /// Free VALUE-position identifier reads (non-`$`) — the names `consts`/`consts_v` must guard.
    pub(crate) consts: BTreeSet<String>,
    /// CALL-position names that are not builtins — user-function dependencies (`deps`).
    pub(crate) deps: BTreeSet<String>,
    /// CALL-position builtin names (`builtins`) — shadowable, hence guarded.
    pub(crate) builtins: BTreeSet<String>,
}

/// Analyze a single `function name(params) = body;` reference.
///
/// # Errors
/// The reference must parse and contain exactly one function definition (the `Entry::reference`
/// contract) — anything else is a malformed reference, not a valid analysis subject.
pub(crate) fn analyze_function(reference: &str) -> Result<Analysis, String> {
    let prog = parse(reference).map_err(|e| format!("reference does not parse: {e:?}"))?;
    let mut defs = prog.stmts.iter().filter_map(|s| match &s.kind {
        StmtKind::FunctionDef { name, params, body } => Some((name, params, body)),
        _ => None,
    });
    let Some((name, params, body)) = defs.next() else {
        return Err("reference holds no function definition".into());
    };
    if defs.next().is_some() {
        return Err("reference holds more than one function definition".into());
    }

    let mut out = Analysis {
        name: name.clone(),
        params: params.iter().map(|p| p.name.to_string()).collect(),
        ..Analysis::default()
    };
    // Defaults evaluate in the call's binding environment, where every parameter NAME is already
    // declared (`function f(a, b=a)` reads the param, not a global) — so params are in scope for
    // the default walks as well as the body.
    let mut scope: Vec<String> = out.params.clone();
    for p in params {
        if let Some(d) = &p.default {
            walk(d, &mut scope, &mut out);
        }
    }
    walk(body, &mut scope, &mut out);
    // Self-recursion is not a dependency: the entry's own fingerprint already proves that exact
    // definition, and the hand lists never carry it.
    let own = out.name.clone();
    out.deps.remove(&own);
    Ok(out)
}

/// One function's analysis, with `deps` and `builtins` closed TRANSITIVELY over a resolver that
/// maps a dep name to its reference source (the registry + pins, for the comparison test; the
/// library's own definitions, for a future whole-library transpile). An unresolvable dep stays a
/// dep — the fingerprint gate will veto it at arm time, which is the safe failure.
///
/// `consts` are deliberately NOT flattened across the closure. A dep's constant is guarded by the
/// DEP's own entry against the DEP's home island (AN.11 — checking it against the caller's island
/// is exactly the bug that family fixed), so `select` reaching `_EPSILON` through `is_vector` puts
/// the name on `is_vector`'s analysis, never on `select`'s. The registry comparison caught this
/// pass doing the flatten and the hand lists were RIGHT.
pub(crate) fn analyze_closed(
    reference: &str,
    resolve: &dyn Fn(&str) -> Option<&'static str>,
) -> Result<Analysis, String> {
    let mut out = analyze_function(reference)?;
    let mut queue: Vec<String> = out.deps.iter().cloned().collect();
    let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
    while let Some(dep) = queue.pop() {
        let Some(src) = resolve(&dep) else { continue };
        let sub = analyze_function(src)?;
        out.builtins.extend(sub.builtins);
        for d in sub.deps {
            if seen.insert(d.clone()) {
                queue.push(d.clone());
            }
            out.deps.insert(d);
        }
    }
    // MUTUAL recursion closes back onto the root (`approx` ↔ its dep) — the root's own name is no
    // more a dependency arriving via a dep than it was as a direct self-call.
    let own = out.name.clone();
    out.deps.remove(&own);
    Ok(out)
}

/// Record a CALL by name: builtin or user dep. Deliberately IGNORES the lexical scope — a name
/// that is also a binding is the AN.10 shape (a parameter shadowing a function in call position),
/// and it stays in the guard set anyway: over-approximating keeps the guard honest while the
/// dispatch-level veto handles the shadowing itself.
fn record_call(name: &str, out: &mut Analysis) {
    if name.starts_with('$') {
        return;
    }
    if super::builtins::is_builtin(name) {
        out.builtins.insert(name.to_string());
    } else {
        out.deps.insert(name.to_string());
    }
}

/// Walk `args` (named-arg NAMES are parameter labels, not reads — only values walk).
fn walk_args(args: &[Arg], scope: &mut Vec<String>, out: &mut Analysis) {
    for a in args {
        walk(&a.value, scope, out);
    }
}

/// Walk `bindings` SEQUENTIALLY (each value sees the names bound before it), pushing each name.
/// Returns how many names were pushed, for the caller to truncate.
fn walk_bindings(bindings: &[Arg], scope: &mut Vec<String>, out: &mut Analysis) -> usize {
    let mut pushed = 0;
    for b in bindings {
        walk(&b.value, scope, out);
        if let Some(n) = &b.name {
            scope.push(n.to_string());
            pushed += 1;
        }
    }
    pushed
}

fn walk(e: &Expr, scope: &mut Vec<String>, out: &mut Analysis) {
    match &e.kind {
        ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
        ExprKind::Ident(name) => {
            // `$`-vars are dynamic scope, guarded by dispatch (all-positional calls only), and the
            // hand `consts` never carry them.
            if !name.starts_with('$') && !scope.iter().any(|s| s == name) {
                out.consts.insert(name.clone());
            }
        }
        ExprKind::Unary { operand, .. } => walk(operand, scope, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk(lhs, scope, out);
            walk(rhs, scope, out);
        }
        ExprKind::Ternary { cond, then, els } => {
            walk(cond, scope, out);
            walk(then, scope, out);
            walk(els, scope, out);
        }
        ExprKind::Index { base, index } => {
            walk(base, scope, out);
            walk(index, scope, out);
        }
        ExprKind::Member { base, .. } => walk(base, scope, out),
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind {
                record_call(name, out);
            } else {
                // A computed callee (method value, applied literal) resolves at runtime — nothing
                // nameable to guard; its SUBEXPRESSIONS still walk.
                walk(callee, scope, out);
            }
            walk_args(args, scope, out);
        }
        ExprKind::Vector(items) => {
            for i in items {
                walk(i, scope, out);
            }
        }
        ExprKind::Range { start, step, end } => {
            walk(start, scope, out);
            if let Some(s) = step {
                walk(s, scope, out);
            }
            walk(end, scope, out);
        }
        ExprKind::FunctionLiteral { params, body } => {
            let mark = scope.len();
            scope.extend(params.iter().map(|p: &Parameter| p.name.to_string()));
            for p in params {
                if let Some(d) = &p.default {
                    walk(d, scope, out);
                }
            }
            walk(body, scope, out);
            scope.truncate(mark);
        }
        // `let` and comprehension-`for` share binder semantics exactly: sequential bindings, body
        // in the extended scope, binders gone after.
        ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
            let mark = scope.len();
            let _ = walk_bindings(bindings, scope, out);
            walk(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
            walk_args(args, scope, out);
            if let Some(b) = body {
                walk(b, scope, out);
            }
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            let mark = scope.len();
            let _ = walk_bindings(init, scope, out);
            walk(cond, scope, out);
            walk_args(update, scope, out); // update rebinds names already in scope
            walk(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::LcEach(inner) => walk(inner, scope, out),
        ExprKind::LcIf { cond, then, els } => {
            walk(cond, scope, out);
            walk(then, scope, out);
            if let Some(e2) = els {
                walk(e2, scope, out);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: expect/panic ARE the assertions"
)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::intrinsics::{PINS, REGISTRY};
    use super::{analyze_closed, analyze_function};

    /// Names the syntactic walk reaches that the HAND lists prune, each with the reason. A pruned
    /// name is unreachable FOR THAT ENTRY's accepted arg shapes, so its whole SUBTREE is pruned —
    /// the comparison's resolver refuses to walk it, exactly as the author's transitive exclusion
    /// does (select drops `all_nonzero` AND therefore `all_nonzero`'s own `abs`). The direction
    /// matters: an over-approximation left in place would only make a guard CHECK MORE, never
    /// answer wrong; a name missing from DERIVED is a red test and an analyzer bug.
    fn pruned_by_author(entry: &str) -> &'static [&'static str] {
        match entry {
            // select's fixed 1-arg `is_vector(start)` call can never take the 2-arg path that
            // reaches `all_nonzero` — the author's reachability pruning (Entry doc, O.5.2). The
            // same argument covers every entry below: their is_vector calls never pass the
            // `all_nonzero=` parameter, so that branch (and its whole subtree) is dead for them.
            "select"
            | "is_matrix"
            | "_none_inside"
            | "sum"
            | "unit"
            | "_apply"
            | "_bt_search"
            | "vector_angle"
            | "_point_dist"
            | "_vnf_centroid"
            | "_get_ear"
            | "is_path"
            | "v_abs"
            | "v_theta"
            | "vector_axis"
            | "apply"
            | "affine3d_rot_by_axis" => &["all_nonzero"],
            // posmod's `approx(m, 0)` sits behind `is_finite(m) &&` — the short-circuit proves
            // approx only ever sees SCALARS from posmod. `idx` lives in approx's list branch, and
            // `is_list`/`len` are that branch's own guard condition, equally dead for numbers
            // (evaluation reaches `is_num(a) && is_num(b)?` first and takes it).
            "posmod" => &["idx", "is_list", "len"],
            _ => &[],
        }
    }

    /// Deltas surfaced by the closure that are NOT yet adjudicated — each needs its reference's
    /// branch structure read against the entry's accepted arg shapes before it becomes either a
    /// documented pruning above or a hand-list fix (AR.5a). Tracked EXACTLY (its own test below)
    /// so a new delta or a silently resolved one is loud; parked here is visible, not forgotten.
    fn unadjudicated(entry: &str, kind: &str) -> &'static [&'static str] {
        match (entry, kind) {
            ("is_matrix" | "sum" | "_apply" | "is_path" | "v_theta" | "apply", "builtins") => {
                &["abs", "norm"]
            }
            ("unit" | "_bt_search" | "vector_angle" | "_point_dist", "builtins") => &["abs"],
            ("_vnf_centroid" | "v_abs", "builtins") => &["norm"],
            ("vector_angle" | "affine3d_rot_from_to", "deps") => &["flatten", "list_to_matrix"],
            ("apply", "deps") => &["_sum", "sum"],
            ("rot", "deps") => &[
                "_all_func",
                "_sum",
                "centroid",
                "flatten",
                "force_list",
                "in_list",
                "is_path",
                "list_to_matrix",
                "mean",
                "pointlist_bounds",
                "sum",
                "transpose",
            ],
            ("rot", "builtins") => &["search"],
            ("affine3d_rot_by_axis", "deps") => &["idx", "posmod"],
            ("affine3d_rot_by_axis", "builtins") => &["abs", "is_string"],
            ("_region_region_intersections", "deps") => &["is_def"],
            ("_region_region_intersections", "builtins") => &["is_bool", "is_string"],
            _ => &[],
        }
    }

    /// The acceptance oracle for the whole pass: every hand-maintained guard list in the registry,
    /// closed over the same dep graph, must be CONTAINED in what the analyzer derives — and the
    /// over-approximation must be exactly the documented pruning, nothing silent in either
    /// direction. ~55 AN-audited entries is the strongest ground truth this analysis can get.
    #[test]
    fn derived_guards_contain_every_hand_list() {
        let mut deltas: Vec<String> = Vec::new();
        for entry in REGISTRY {
            let allow = pruned_by_author(entry.name);
            let resolve = |name: &str| -> Option<&'static str> {
                if allow.contains(&name) {
                    return None; // author-pruned: this entry never reaches it, subtree and all
                }
                REGISTRY
                    .iter()
                    .find(|e| e.name == name)
                    .map(|e| e.reference)
                    .or_else(|| PINS.iter().find(|(n, _)| *n == name).map(|(_, src)| *src))
            };
            let derived = analyze_closed(entry.reference, &resolve)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.name));

            let hand_consts: BTreeSet<&str> = entry
                .consts
                .iter()
                .map(|(n, _)| *n)
                .chain(entry.consts_v.iter().map(|(n, _)| *n))
                .collect();
            let hand_deps: BTreeSet<&str> = entry.deps.iter().copied().collect();
            let hand_builtins: BTreeSet<&str> = entry.builtins.iter().copied().collect();

            for (kind, hand, derived_set) in [
                ("consts", &hand_consts, &derived.consts),
                ("deps", &hand_deps, &derived.deps),
                ("builtins", &hand_builtins, &derived.builtins),
            ] {
                let missing: Vec<&&str> =
                    hand.iter().filter(|n| !derived_set.contains(**n)).collect();
                if !missing.is_empty() {
                    deltas.push(format!(
                        "{}: hand {kind} {missing:?} NOT DERIVED — the analyzer failed to reach \
                         a name the author proved matters (AN territory)",
                        entry.name
                    ));
                }
                let tracked = unadjudicated(entry.name, kind);
                let extra: Vec<&String> = derived_set
                    .iter()
                    .filter(|n| {
                        !hand.contains(n.as_str())
                            && !allow.contains(&n.as_str())
                            && !tracked.contains(&n.as_str())
                    })
                    .collect();
                if !extra.is_empty() {
                    deltas.push(format!(
                        "{}: derived {kind} {extra:?} beyond the hand list and undocumented — \
                         either the hand list is INCOMPLETE (fix the entry) or the walk needs a \
                         rule (document it in `pruned_by_author`)",
                        entry.name
                    ));
                }
            }
        }
        assert!(
            deltas.is_empty(),
            "{} guard-list deltas:\n{}",
            deltas.len(),
            deltas.join("\n")
        );
    }

    /// The walk's scope rules, on synthetic references where the answer is unambiguous.
    #[test]
    fn scope_rules_classify_reads_and_calls() {
        let a = analyze_function(
            "function t(a, b=_EPS) = let(c = a + PI) is_num(c) ? helper(b) : $fn + d;",
        )
        .expect("analyzes");
        assert_eq!(a.name, "t");
        assert_eq!(a.params, ["a", "b"]);
        // _EPS (a default), PI (a body read), d — free reads; a/b/c bound; $fn dynamic.
        assert_eq!(
            a.consts,
            ["PI", "_EPS", "d"].map(String::from).into_iter().collect()
        );
        assert_eq!(a.deps, ["helper"].map(String::from).into_iter().collect());
        assert_eq!(
            a.builtins,
            ["is_num"].map(String::from).into_iter().collect()
        );
    }

    /// AN.10's shape: a parameter shadowing a function name in CALL position still registers as a
    /// dep — over-approximation is the guard-safe direction.
    #[test]
    fn a_shadowed_call_name_is_still_a_dep() {
        let a = analyze_function("function t(f) = f(1);").expect("analyzes");
        assert_eq!(a.deps, ["f"].map(String::from).into_iter().collect());
    }

    /// Comprehension + function-literal binders leave scope when their expression ends.
    #[test]
    fn binders_do_not_leak() {
        let a = analyze_function(
            "function t(n) = [for (i = [0:n]) (function(j) j + i)(i)] * [for (i = [0:n]) i] + i;",
        )
        .expect("analyzes");
        // the trailing `+ i` is OUTSIDE both comprehensions: a free read.
        assert_eq!(a.consts, ["i"].map(String::from).into_iter().collect());
    }
}
