//! AN.17 — an intrinsic must not bake a constant its DEP reads, when the dep's own island binds that
//! constant to something else.
//!
//! AN.11 added exactly that check (`dep_const_veto`) and shipped it "sound by construction, but NOT
//! demonstrated by a failing repro". This file is that repro — and building it turned up the reason none
//! existed: the check sits in `arm_guarded_intrinsics`, which opens with
//!
//! ```text
//! if entry.consts.is_empty() && entry.consts_v.is_empty() { continue; }
//! ```
//!
//! so an entry with NO constant of its own never reaches it. Those entries wire earlier, in
//! `build_intrinsics`, behind `guard_veto` — which checks that each dep is defined, fingerprint-matches
//! its pin, isn't shadowed by a parameter (AN.10) and doesn't shadow a builtin, but says nothing about
//! the dep's CONSTANTS. And "no consts of its own, a dep that carries one" is not a corner: `select`,
//! `is_matrix`, `sum`, `_apply`, `_bt_search`, `vector_angle`, `_point_dist`, `is_path`, `v_abs`,
//! `v_theta` and `apply` are all that shape, every one of them via a dep that bakes `_EPSILON`.
//!
//! The fixture is the `_fab_poc_*` pair rather than real BOSL2 because BOSL2 keeps such a function and
//! its dep in the SAME file — the cross-island case needs them split, so any "real" repro would be
//! hand-authored source anyway, just more fragile against the pin.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::PathBuf;

use fab_lang::{Message, evaluate_geometry_with_base_full};

/// Materialize a two-file graph and return the root's echo lines.
///
/// `lib_inner.scad` binds `_EPSILON = 1e-3` and defines the dep; the ROOT defines the outer function and
/// calls it. Two files, so the dep's home island is genuinely not the caller's — which is the only
/// configuration where a dep-constant check can matter.
fn echos_of_split_graph(x: &str) -> Vec<String> {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("an17-dep-islands-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        base.join("lib_inner.scad"),
        "_EPSILON = 1e-3;\nfunction _fab_poc_near0(x) = abs(x) < _EPSILON;\n",
    )
    .unwrap();
    let root = format!(
        "use <lib_inner.scad>\nfunction _fab_poc_outer(x) = _fab_poc_near0(x);\necho(_fab_poc_outer({x}));\ncube(1);\n"
    );
    let (_, messages) = evaluate_geometry_with_base_full(&root, &base, &[]).expect("evaluates");
    messages
        .iter()
        .filter_map(Message::echo)
        .map(str::to_string)
        .collect()
}

#[test]
fn an_intrinsic_does_not_bake_a_constant_its_dep_reads_from_another_island() {
    // `_fab_poc_near0` lives in an island where `_EPSILON` is 1e-3, so INTERPRETED it answers
    // `abs(1e-6) < 1e-3` → true. A native that inlined the dep with `_EPSILON` baked at 1e-9 would
    // answer `1e-6 < 1e-9` → false. The interpreter is the oracle here: `true` is the only correct
    // answer, and `false` means a native fired on constants that do not hold.
    assert_eq!(
        echos_of_split_graph("0.000001"),
        ["true"],
        "the dep's OWN island binds _EPSILON = 1e-3, so 1e-6 is near zero; `false` means an intrinsic \
         baked 1e-9 from somewhere else"
    );
}

#[test]
fn the_same_split_graph_agrees_on_a_value_no_epsilon_can_straddle() {
    // A control: 1.0 is not near zero under EITHER epsilon, so this passes whether or not the guard
    // works. It is here so a failure of the test above reads as "the epsilon was wrong" rather than
    // "the whole two-file fixture is broken".
    assert_eq!(echos_of_split_graph("1.0"), ["false"]);
}
