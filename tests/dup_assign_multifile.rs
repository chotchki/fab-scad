//! AQ.1 — the multi-file `"x" was assigned on line N … but was overwritten` rule, against the oracle.
//!
//! Every case here is one I probed against the binary while trying (and failing) to derive this rule from
//! output alone. The rule turned out to live in `handle_assignment` in upstream's `src/core/parser.y` as a
//! four-branch chain with an explicit special case and one deliberately-absent `else`; the two silences
//! below are the parts no amount of black-box probing was going to produce.
//!
//! These run through `differ::diff_warnings`, so they are checked against the ACTUAL binary rather than
//! against my reading of the source — the reading is a hypothesis, and the oracle is what settles it.

use std::path::PathBuf;

/// Materialize a file graph under a fresh subdir and assert its warnings match the oracle's.
fn agree_graph_warnings(subdir: &str, files: &[(&str, &str)], root: &str) {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("dup_assign_multifile")
        .join(subdir);
    let _ = std::fs::remove_dir_all(&base);
    for (rel, contents) in files {
        let path = base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    if let Err(why) = fab_scad::differ::diff_warnings_file(&base.join(root), &[]) {
        panic!("{subdir}: warning divergence: {why}");
    }
}

/// The dependency every case shares: a library with a duplicate assignment of its own.
const DUP_LIB: (&str, &str) = ("sub/dup.scad", "d = 1;\nd = 2;\nfunction lf() = d;\n");
/// A library with exactly ONE assignment — the fixture for the two silences.
const ONE_LIB: (&str, &str) = ("sub/single.scad", "z = 9;\nfunction zf() = z;\n");

#[test]
fn a_library_internal_duplicate_names_that_library() {
    // Branch 3. Both sides are in dup.scad, yet upstream still prints the path — which is exactly why
    // "name the file only when it differs from the overwrite site" is the wrong model.
    agree_graph_warnings(
        "lib_internal",
        &[
            DUP_LIB,
            ("root.scad", "use <sub/dup.scad>\necho(lf());\ncube(1);\n"),
        ],
        "root.scad",
    );
}

#[test]
fn a_root_internal_duplicate_names_nothing() {
    // Branch 2 — the one case that omits the path, and the reason the root-only form exists at all.
    agree_graph_warnings(
        "root_internal",
        &[("root.scad", "a = 1;\na = 2;\necho(a);\ncube(1);\n")],
        "root.scad",
    );
}

#[test]
fn a_root_assignment_overwritten_by_an_include_names_the_root() {
    // Branch 4. Upstream passes the FIRST side's path here, and the first side is main — so the message
    // names root.scad even though the overwrite happened inside the include.
    agree_graph_warnings(
        "root_then_include",
        &[
            ONE_LIB,
            (
                "root.scad",
                "z = 1;\ninclude <sub/single.scad>\necho(z);\ncube(1);\n",
            ),
        ],
        "root.scad",
    );
}

#[test]
fn including_a_single_assignment_file_twice_is_silent() {
    // The equal-line guard. Both sides are line 1 of single.scad, so upstream suppresses it — its comment
    // says so outright. Probing produced "include twice warns twice" from a DIFFERENT fixture (a library
    // with an internal duplicate), which is what made this look contradictory.
    agree_graph_warnings(
        "include_twice",
        &[
            ONE_LIB,
            (
                "root.scad",
                "include <sub/single.scad>\ninclude <sub/single.scad>\necho(z);\ncube(1);\n",
            ),
        ],
        "root.scad",
    );
}

#[test]
fn an_include_before_a_root_duplicate_silences_it() {
    // The MISSING else. First assignment is in the include, overwrite is in main — that combination falls
    // off the end of upstream's chain, so the root's own `z=1; z=2` says nothing. This is the case that
    // defeated every model I built from output alone.
    agree_graph_warnings(
        "include_then_root_dup",
        &[
            ONE_LIB,
            (
                "root.scad",
                "include <sub/single.scad>\nz = 1;\nz = 2;\necho(z);\ncube(1);\n",
            ),
        ],
        "root.scad",
    );
}

#[test]
fn two_used_libraries_report_in_reverse_use_order() {
    // `registerUse` FRONT-inserts into usedlibs, so the textually-LAST `use` reports first.
    agree_graph_warnings(
        "two_uses",
        &[
            DUP_LIB,
            (
                "sub/deep/deeper.scad",
                "g = 1;\ng = 2;\nfunction gf() = g;\n",
            ),
            (
                "root.scad",
                "use <sub/dup.scad>\nuse <sub/deep/deeper.scad>\necho(lf(), gf());\ncube(1);\n",
            ),
        ],
        "root.scad",
    );
}

#[test]
fn a_root_duplicate_and_a_library_one_both_report() {
    // Both branches in one program, so this pins that the two forms coexist and keep their order.
    agree_graph_warnings(
        "root_and_lib",
        &[
            DUP_LIB,
            (
                "root.scad",
                "a = 1;\na = 2;\nuse <sub/dup.scad>\necho(a, lf());\ncube(1);\n",
            ),
        ],
        "root.scad",
    );
}
