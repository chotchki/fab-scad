//! M.2 — statement/geometry eval assembly is HOST-recursive (unlike the explicit-stack EXPRESSION machine
//! and, since M.1/M.1b, tree `Drop` — both heap-bounded). [`MAX_MODULE_DEPTH`] is the guard that turns a
//! runaway recursive MODULE into a LOUD error instead of a silent stack crash. It is the safety mechanism the
//! eval-thread reserve (`fab_scad::EVAL_STACK`, ~½ GiB — sized in M.2 against the guard-limit worst case) is
//! paired with, UNTIL the M.3 explicit-stack conversion removes host recursion from the geometry pipeline and
//! lets eval run on a default — and wasm-small — stack.
//!
//! These run on a BIG stack on purpose: 256 levels is tens of MiB deep in a debug build, so a default 2 MiB
//! test thread overflows (~15 levels in) long BEFORE the guard is reached — which is exactly the M.2 finding.
//! The guard only saves you when the stack can hold its whole depth, so the guard and the reserve are COUPLED;
//! decoupling them is M.3's job.

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
