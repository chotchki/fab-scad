//! M.3 — statement/geometry eval runs on the explicit-stack driver, so it's HEAP-bounded (joining the
//! expression machine + tree `Drop`). [`MAX_MODULE_DEPTH`] no longer guards a CRASH (there's no host recursion
//! to overflow) — it's a runaway DETECTOR, turning an infinite recursive MODULE into a fast LOUD error before
//! OOM. The payoff tests below run on a 512 KiB stack — a depth that needed tens of MiB pre-M.3 (M.2's finding)
//! now lives on the heap work stack. Because we're heap-bounded and OpenSCAD's C++ tree-walker is not, we accept
//! recursion deeper than OpenSCAD (which errors ~5–8 k).

#![allow(clippy::unwrap_used, reason = "test harness: unwrap IS the assertion")]

use fab_lang::{Error, evaluate_geometry};

/// Run `f` on a stack deep enough to actually REACH the module-depth guard (256 levels ≈ tens of MiB in debug).
fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("module-recursion".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// Run `f` on a deliberately SMALL 512 KiB stack — the recursive evaluator overflows a deep module here; the
/// explicit-stack driver (M.3) does not.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("module-recursion-small".into())
        .stack_size(512 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// THE M.3 PAYOFF (B1) — a deeply-recursive MODULE evaluates on a 512 KiB stack under the explicit-stack driver:
/// the body runs on the heap-allocated work stack, not the host stack. The recursive path needs ~32 MiB (debug)
/// for this same `r(200)` (M.2's measurement), so it would OVERFLOW here — which is exactly the exposure M.3
/// closes. Skipped under `FAB_GEO_DRIVER=0` (that A/B run IS the recursive path this test would crash).
#[test]
fn deep_recursive_module_is_heap_bounded_under_the_driver() {
    on_small_stack(|| {
        let g = evaluate_geometry("module r(n) { if (n > 0) r(n - 1); else cube(1); } r(200);");
        assert!(g.is_ok(), "deep recursion should eval heap-bounded: {:?}", g.err());
    });
}

/// Recursion THROUGH a `for` loop is heap-bounded too (B3): the loop body runs on the work stack, not the host
/// stack. Before B3 the `for` arm shimmed, so a recursion threaded through it still overflowed. On a 512 KiB
/// stack under the driver it doesn't. (Only `r` recurses here — a `children()` wrapper would DOUBLE the
/// module-depth per level and trip the 256 guard, a separate concern from stack-boundedness.)
#[test]
fn deep_recursion_through_for_is_heap_bounded() {
    on_small_stack(|| {
        let g = evaluate_geometry(
            "module r(n) { if (n > 0) for (i = [0:0]) r(n - 1); else cube(1); } r(200);",
        );
        assert!(g.is_ok(), "deep for recursion should eval heap-bounded: {:?}", g.err());
    });
}

/// EXCEED OpenSCAD (the moat) — a recursion 10 000 deep evaluates fine, where OpenSCAD 2026.06.12 errors
/// "Recursion detected calling module" by ~8 000 (its C++ tree-walker is host-stack-bound; ours is
/// heap-bounded, so `MAX_MODULE_DEPTH` sits well above OpenSCAD's limit). Runs on a 512 KiB stack — the depth
/// lives on the heap work stack, not the host stack. Same language, deeper limit.
#[test]
fn recursion_deeper_than_openscad_still_evaluates() {
    on_small_stack(|| {
        let g = evaluate_geometry("module r(n) { if (n > 0) r(n - 1); else cube(1); } r(10000);");
        assert!(
            g.is_ok(),
            "10k-deep recursion should evaluate (OpenSCAD errors by ~8k): {:?}",
            g.err()
        );
    });
}

#[test]
fn runaway_module_recursion_is_loud_not_a_crash() {
    // `module r() { r(); }` has no base case — the guard bails at MAX_MODULE_DEPTH and the error unwinds out
    // instead of the process SIGABRT'ing on a stack overflow.
    let err = on_big_stack(|| evaluate_geometry("module r() { r(); } r();").unwrap_err());
    assert!(
        matches!(&err, Error::Unimplemented(m) if m.contains("recursion too deep")),
        "expected the LOUD module-depth guard, got {err:?}"
    );
}

#[test]
fn legal_finite_recursion_still_evaluates() {
    // A module that recurses to a FINITE, under-cap depth builds fine — the guard never false-trips on legal
    // recursion (r(200) is well under the 256 cap).
    let ok = on_big_stack(|| {
        evaluate_geometry("module r(n) { if (n > 0) r(n - 1); else cube(1); } r(200);")
    });
    assert!(ok.is_ok(), "legal 200-deep recursion should evaluate: {ok:?}");
}
