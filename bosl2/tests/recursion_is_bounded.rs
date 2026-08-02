//! AR.26.4.4 — the compiled tier stops recursing on the HOST stack.
//!
//! A generated native calls an in-batch sibling with a plain Rust call, so the emitted call graph is
//! a stack-depth profile: a DAG's height is a build-time constant (asserted in the emitter's own
//! `the_static_call_graph_is_measured`), but a CYCLE is unbounded, which is the exact class the
//! interpreter designed out with its explicit stack. `MAX_NATIVE_DEPTH` was the patch — a counter
//! that notices after 32 frames are already spent — and it is denominated in the wrong unit: what
//! overflows is BYTES, and a level costs ~28 KiB in release and ~200 KiB in debug.
//!
//! The fix routes cycle-internal calls through `fx.call_named`, which lands in the LIVE evaluator
//! with natives suppressed — one machine, one explicit stack. 19 of 1260 functions are affected and
//! 4360 of 4379 static edges are untouched, so the DAG keeps its plain Rust calls.
//!
//! THESE TESTS RUN ON A DELIBERATELY SMALL STACK. That is the whole point: a test on the default
//! stack proves nothing, because the default is large enough to hide the bug that motivated this.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_lang::Config;
use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;

/// Small enough that an unbounded native ladder cannot survive it, large enough for the DAG's
/// measured height (22 levels) plus the evaluator. In DEBUG a native level is ~200 KiB, so this
/// holds roughly 20 of them — a recursion that scaled with the DATA would blow it long before the
/// old depth counter's 32 could fire.
const SMALL_STACK: usize = 4 * 1024 * 1024;

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

fn have_bosl2() -> bool {
    fab_bosl2::transpiled() && libs().join("BOSL2/std.scad").exists()
}

/// Evaluate `src` on a bounded stack, both tiers, and return `(value-bearing console, peak depth)`.
fn run_small(src: String) -> (String, String, u32) {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let reg = Registry::new().with(fab_bosl2::Bosl2.rows());
            let go = |intrinsics: bool| {
                let config = Config {
                    intrinsics,
                    ..Config::default()
                };
                let (_geo, msgs) = fab_lang::evaluate_geometry_with_registry(
                    &src,
                    Path::new("."),
                    &[libs()],
                    &reg,
                    config,
                )
                .expect("renders");
                msgs.iter()
                    .map(fab_lang::Message::render)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let on = go(true);
            let peak = fab_lang::peak_native_depth();
            let off = go(false);
            (on, off, peak)
        })
        .expect("spawn")
        .join()
        .expect("the recursion must not overflow a bounded stack")
}

/// `lcm` OVER A LONG LIST. `_lcmlist` recurses once per element and is one of the 19 names in a
/// cycle group, so before the routing this was a native ladder as deep as the input — data-driven
/// host recursion, which no level counter can bound because the counter fires only after the frames
/// are already on the stack.
///
/// 400 elements: far past the old `MAX_NATIVE_DEPTH` of 32, so the old build reached the counter and
/// then paid `run_interpreted`'s throwaway island; far past what 4 MiB could hold if it had not.
///
/// The values REPEAT (2, 3, 4, 6) so the running LCM stays 12. `1..=400` would overflow f64's
/// integer range partway and trip BOSL2's own `is_int` assert, which ends the recursion early and
/// would have made this measure nothing.
#[test]
fn a_data_driven_recursion_does_not_grow_the_host_stack() {
    if !have_bosl2() {
        return;
    }
    let list = (0..400)
        .map(|i| [2, 3, 4, 6][i % 4].to_string())
        .collect::<Vec<_>>()
        .join(",");
    let src = format!("include <BOSL2/std.scad>\necho(lcm([{list}]));\n");
    let (on, off, peak) = run_small(src);

    assert_eq!(on, off, "tiers disagree on a deep recursion");
    assert!(
        on.contains("ECHO: 12"),
        "lcm of 2/3/4/6 repeated is 12: {on}"
    );
    // THE DAG'S HEIGHT, NOT THE DATA'S LENGTH — and the bound is deliberately far below the old
    // `MAX_NATIVE_DEPTH` of 32, because "under the counter" is not the property being tested. 400
    // elements buying a constant handful of frames is. Measured: 2.
    assert!(
        peak <= 8,
        "the native ladder tracked the DATA ({peak} deep on 400 elements), so the recursion is \
         still on the host stack"
    );
}

/// `compare_lists` on NESTED lists — the other shape, where depth follows the data's STRUCTURE
/// rather than its length, and the recursion is mutual (`compare_lists` ↔ `compare_vals`) rather
/// than direct. A cycle group is the unit precisely because a mutual pair has no single
/// self-referencing function to spot.
#[test]
fn a_mutually_recursive_pair_does_not_grow_the_host_stack() {
    if !have_bosl2() {
        return;
    }
    // 100 levels of nesting: [[[[…1…]]]] against its twin. NOT more, and the ceiling is not ours:
    // at ~300 the PARSER's recursive descent overflows a 4 MiB stack before evaluation starts, on
    // both tiers alike (verified with natives off). A pre-existing limit in a different subsystem,
    // and staying under it is what keeps this test measuring the thing it names.
    let deep = |leaf: &str| {
        let mut s = leaf.to_string();
        for _ in 0..100 {
            s = format!("[{s}]");
        }
        s
    };
    let src = format!(
        "include <BOSL2/std.scad>\necho(compare_lists({}, {}));\n",
        deep("1"),
        deep("2")
    );
    let (on, off, peak) = run_small(src);

    assert_eq!(on, off, "tiers disagree on a deep mutual recursion");
    assert!(on.contains("ECHO: -1"), "1 sorts before 2, so -1: {on}");
    assert!(
        peak <= 8,
        "the native ladder tracked the nesting ({peak} deep on 100 levels)"
    );
}

/// NON-VACUITY, and it is the assertion this file would be worthless without: the natives really do
/// arm for these programs. Two interpreted runs agree perfectly, so a tier comparison that armed
/// nothing passes while testing nothing — the failure shape this codebase has found more often than
/// any other.
#[test]
fn the_recursive_natives_actually_wire() {
    if !have_bosl2() {
        return;
    }
    let reg = Registry::new().with(fab_bosl2::Bosl2.rows());
    let src = "include <BOSL2/std.scad>\necho(lcm([4, 6, 8]));\n";
    let (_geo, msgs) = fab_lang::evaluate_geometry_with_registry(
        src,
        Path::new("."),
        &[libs()],
        &reg,
        Config::default(),
    )
    .expect("renders");

    assert!(
        fab_lang::wired_count() > 800,
        "only {} natives wired — the band did not arm",
        fab_lang::wired_count()
    );
    // And it answers: lcm(4, 6, 8) = 24. Pinned as a VALUE rather than only as tier agreement,
    // because two tiers both answering `undef` agree with each other and are both wrong.
    let echoed = msgs
        .iter()
        .map(fab_lang::Message::render)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(echoed.contains("24"), "lcm([4,6,8]) should be 24: {echoed}");
}
