//! AR.5a / AR.14.3 — the audit of the HAND subjects's guard lists, and of its references against
//! the pinned library.
//!
//! Lives in fab-lang rather than in the transpiler for a reason worth stating: these tests are about
//! the subjects, not about the compiler. They ask whether the hand-maintained `consts`/`deps`/
//! `builtins` lists agree with what the source actually reaches, and whether the transcribed
//! references still match upstream — both questions about a table fab-lang owns and AR.21 deletes.
//! The transpiler is merely the instrument (`fab_lib::emit::analyze_closed` does the walk), which is
//! why fab-lang carries fab-lib as a DEV-dependency.
//!
//! An INTEGRATION test, not a unit test, and that is forced rather than stylistic: fab-lang carries
//! fab-lib as a dev-dependency, and a dev-dep cycle links the PUBLISHED rlib rather than the
//! `cfg(test)` build — so a unit test consuming fab-lib types that wrap fab-lang types sees two
//! distinct `Expr` types and will not compile. An integration test links the normal rlib and the
//! types unify. That is also why the registry reaches it through `fab_lang::bootstrap_all` rather
//! than directly: an external auditor can only see public API, which is the honest constraint.
//!
//! They came here when the transpiler moved out, and they should die with the registry, not with it.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: expect/panic ARE the assertions"
)]

use std::collections::BTreeSet;

use fab_lib::emit::analyze_closed;
use fab_lib::library::Library;

/// Names the syntactic walk reaches that the HAND lists prune, each with the reason. A pruned
/// name is unreachable FOR THAT ENTRY's accepted arg shapes, so its whole SUBTREE is pruned —
/// the comparison's resolver refuses to walk it, exactly as the author's transitive exclusion
/// does (select drops `all_nonzero` AND therefore `all_nonzero`'s own `abs`). The direction
/// matters: an over-approximation left in place would only make a guard CHECK MORE, never
/// answer wrong; a name missing from DERIVED is a red test and an analyzer bug.
/// Names a hand list deliberately omits because the native DISPATCHES to them rather than inlining
/// them — AR.26.4.4's cycle routing, and a genuinely different KIND of exclusion from
/// [`pruned_by_author`].
///
/// Every arm there prunes a name the author proved UNREACHABLE. These are very much reached, and
/// still must not be deps: a dep pin exists to guard BAKED semantics, and a cycle-internal call
/// bakes nothing. The emitter drops each subject's cycle group from its sibling table, so those
/// calls compile to `fx.call_named` and resolve against whatever the running program defines —
/// which is what the interpreter does, leaving no second implementation for a fingerprint to prove
/// equal. Same argument AR.27 made for an out-of-batch callee; different cause.
///
/// The names here are the CYCLE-MATES only. Pruning a name prunes its whole subtree, which is how
/// one entry covers the builtins that used to ride in underneath — exactly the closure the emitter
/// computes.
///
/// TWO GROUPS in fab-lang's own batch: `approx` ↔ `idx` ↔ `posmod` (BOSL2's documented 3-cycle) and
/// `is_vector` ↔ `all_nonzero`. The rest of the list is entries that merely CALL into one of those
/// groups and therefore stopped inheriting what the group used to contribute.
fn routed_through_dispatch(entry: &str) -> &'static [&'static str] {
    match entry {
        // The `approx` ↔ `idx` ↔ `posmod` group: its three members, plus `_get_ear`, which calls
        // in and loses `approx` outright. Merged because the SETS are equal, not the reasons.
        "approx" | "idx" | "posmod" | "_get_ear" => &["approx", "idx", "posmod"],
        // The `is_vector` ↔ `all_nonzero` group, and the three entries that call into it. Same:
        // one member and three callers, one set.
        "is_vector" | "is_vnf" | "determinant" | "constrain" => &["all_nonzero"],
        "all_nonzero" => &["is_vector"],
        // `_vnf_centroid` still INLINES `approx` (and so still guards its `abs`/`is_bool`); it only
        // stops inheriting what `approx` itself used to reach.
        "_vnf_centroid" => &["idx", "posmod"],
        "affine3d_rot_from_to" => &["all_nonzero", "idx", "posmod"],
        _ => &[],
    }
}

fn pruned_by_author(entry: &str) -> &'static [&'static str] {
    match entry {
        // ── AR.27: the OUTWARD call, and it is a different KIND of exclusion ─────────────────
        // Every other arm here prunes a name the author proved UNREACHABLE. This one prunes a
        // name that is very much reached — and still must not be a dep, because a dep pin exists
        // to guard BAKED semantics and nothing is baked here. `_fab_poc_absent` is not in the
        // batch, so the emitter dispatches to it through `fx.call_named`, which resolves against
        // whatever the running program defines. That is what the INTERPRETER does, so there is
        // no second implementation for a fingerprint to prove equal.
        //
        // Pinning it anyway would be worse than noise: `anchor_fp` would find no anchor for a
        // name the library never compiled, and the entry would never wire at all.
        "_fab_poc_outward" => &["_fab_poc_absent"],
        // ── the `is_vector` family ───────────────────────────────────────────────────────────
        // `is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON)` has two dead tails for
        // every entry here, because none of them passes anything past the SECOND parameter:
        //   * `zero` stays undef, so `is_undef(zero) ||` short-circuits before `norm(v)`;
        //   * `all_nonzero` keeps its `false` default, so `!all_nonzero ||` short-circuits before
        //     `all_nonzero(v)` — and with it that function's own `abs`.
        // A pruned name prunes its whole SUBTREE, which is why `abs`/`norm` come along. AR.5a
        // adjudicated each of these against the entry's accepted arg shapes; the per-entry arms
        // below are DELIBERATELY not collapsed into one, because the sets differ and a shared arm
        // would silently prune a name for an entry nobody checked.
        "select" | "_none_inside" | "_get_ear" | "vector_axis" => &["all_nonzero"],
        "unit" | "_bt_search" | "_point_dist" => &["all_nonzero", "abs"],
        "_vnf_centroid" | "v_abs" => &["all_nonzero", "norm"],
        "is_matrix" | "sum" | "_apply" | "is_path" | "v_theta" => &["all_nonzero", "abs", "norm"],
        // `apply` adds the sum family: its `is_matrix`/`is_vector` shape tests never reach the
        // point-list branch that would call `sum`/`_sum`.
        "apply" => &["all_nonzero", "abs", "norm", "sum", "_sum"],
        // `vector_angle`/`affine3d_rot_from_to` reach `flatten`/`list_to_matrix` only through a
        // `list_to_matrix` branch their fixed call shapes never take.
        "vector_angle" => &["all_nonzero", "abs", "flatten", "list_to_matrix"],
        "affine3d_rot_from_to" => &["flatten", "list_to_matrix"],
        // posmod's `approx(m, 0)` sits behind `is_finite(m) &&` — the short-circuit proves
        // approx only ever sees SCALARS from posmod. `idx` lives in approx's list branch, and
        // `is_list`/`len` are that branch's own guard condition, equally dead for numbers
        // (evaluation reaches `is_num(a) && is_num(b)?` first and takes it).
        "posmod" => &["idx", "is_list", "len"],
        // affine3d_rot_by_axis takes the SCALAR approx lane only (`assert(is_finite(ang))` pins
        // it), so approx's list branch — `idx`, and `posmod`/`is_string` under it — is dead.
        // NOTE `abs` is NOT here: that one is reachable, and AR.5a moved it into the hand list.
        "affine3d_rot_by_axis" => &["all_nonzero", "idx", "posmod", "is_string"],
        // `rot` is a DISPATCHER: its body picks one affine lane per call shape, and the
        // point-list lane (centroid/mean/pointlist_bounds/transpose/in_list/force_list/sum/_sum/
        // flatten/list_to_matrix/_all_func/is_path, plus the `search` builtin) belongs to the
        // `p=` argument the native declines.
        "rot" => &[
            "search",
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
        // `is_def` is reached only through a defaulted parameter this entry always supplies.
        "_region_region_intersections" => &["is_def"],
        _ => &[],
    }
}

/// AR.5a: this table is EMPTY, and that is the point. It parked derived-minus-hand deltas that
/// nobody had reasoned about yet; every one has now been adjudicated into either a documented
/// pruning above or a hand-list FIX (three were real missing guards — `affine3d_rot_by_axis`
/// wanted `abs`, `_region_region_intersections` wanted `is_bool` + `is_string`).
///
/// It stays as a named seam rather than being deleted, because the next widening of the codegen
/// subset will surface new deltas and they should land HERE — visible and tracked — rather than
/// in `pruned_by_author`, which is for names somebody proved unreachable. The two directions are
/// not symmetric: a wrong pruning deletes a correctness guard and lets a native wire where the
/// interpreter would diverge; a wrong hand-list entry only makes the guard check more.
fn unadjudicated(_entry: &str, _kind: &str) -> &'static [&'static str] {
    &[]
}

/// The acceptance oracle for the whole pass: every hand-maintained guard list in the registry,
/// closed over the same dep graph, must be CONTAINED in what the analyzer derives — and the
/// over-approximation must be exactly the documented pruning, nothing silent in either
/// direction. ~55 AN-audited entries is the strongest ground truth this analysis can get.
#[test]
fn derived_guards_contain_every_hand_list() {
    let (subjects, pins) = fab_lang::bootstrap_all();
    let mut deltas: Vec<String> = Vec::new();
    for entry in &subjects {
        // Two exclusion reasons, kept SEPARATE in their sources and unioned only here: one prunes
        // what the author proved unreachable, the other what the emitter routes through dispatch.
        let allow: Vec<&str> = pruned_by_author(entry.name)
            .iter()
            .chain(routed_through_dispatch(entry.name))
            .copied()
            .collect();
        let resolve = |name: &str| -> Option<&'static str> {
            if allow.contains(&name) {
                return None; // author-pruned: this entry never reaches it, subtree and all
            }
            subjects
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.source)
                .or_else(|| pins.iter().find(|(n, _)| *n == name).map(|(_, src)| *src))
        };
        let derived = analyze_closed(entry.source, &resolve)
            .unwrap_or_else(|e| panic!("{}: {e}", entry.name));

        // Every guarded constant NAME, whatever its type — see `BootstrapSubject::const_names`.
        let hand_consts: BTreeSet<&str> = entry.const_names.iter().copied().collect();
        let hand_deps: BTreeSet<&str> = entry.deps.iter().copied().collect();
        let hand_builtins: BTreeSet<&str> = entry.builtins.iter().copied().collect();

        for (kind, hand, derived_set) in [
            ("consts", &hand_consts, &derived.consts),
            ("deps", &hand_deps, &derived.deps),
            ("builtins", &hand_builtins, &derived.builtins),
        ] {
            let missing: Vec<&&str> = hand.iter().filter(|n| !derived_set.contains(**n)).collect();
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
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libs/BOSL2");
    if !dir.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let lib = Library::read(&dir).expect("BOSL2 reads");
    let (subjects, _pins) = fab_lang::bootstrap_all();
    let mut checked = 0_usize;
    let mut drifted = Vec::new();
    for entry in &subjects {
        let Some(found) = lib.functions.get(entry.name) else {
            continue; // our own POCs, and anything the library declares ambiguously
        };
        let hand = fab_lang::parse(entry.source).expect("reference parses");
        let Some(fab_lang::StmtKind::FunctionDef { params, body, .. }) =
            hand.stmts.first().map(|s| &s.kind)
        else {
            panic!("{}: reference holds no function definition", entry.name);
        };
        checked += 1;
        if fab_lang::fingerprint_of(params, body)
            != fab_lang::fingerprint_of(&found.params, &found.body)
        {
            drifted.push(format!("{} (from {})", entry.name, found.file));
        }
    }
    println!(
        "\n=== hand references vs pinned BOSL2 ===\n{checked} of {} registry entries resolve \
         against the library, {} drifted",
        subjects.len(),
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
