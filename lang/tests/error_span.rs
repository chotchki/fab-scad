//! W.3.37: a fatal eval error carries the SOURCE SPAN of the top-level construct that triggered it, so the
//! GUI can point the editor at the user's line. The seam that matters in practice is the top-level HOIST: a
//! `x = f(...)` whose RHS faults deep in a called function (the webcam_holder class — `p = bezpath_curve(pts,
//! $fn, 2)` with $fn=0). Geometry-statement asserts are CAUGHT (L.5.8 warn-and-continue) and unknown symbols
//! warn, so the driver seam rarely fires; it stamps identically (`Error::at`) when a non-assert fault does.

use fab_lang::{Error, evaluate_geometry, offset_to_line};

/// The 1-based line the error's stamped span points at, or `None` if unstamped.
fn err_line(src: &str) -> Option<u32> {
    let e = evaluate_geometry(src).expect_err("expected an eval error");
    e.span().map(|s| offset_to_line(src, s.start))
}

#[test]
fn hoist_assignment_rhs_fault_points_at_its_line() {
    // `x = bad()` where bad() asserts → fails during the top-level hoist. The error points at line 2 (the
    // assignment), not the program start — exactly the attribution webcam_holder needed.
    assert_eq!(
        err_line("function bad() = assert(false, \"boom\") 1;\nx = bad();\ncube(x);\n"),
        Some(2),
    );
}

#[test]
fn assert_expression_in_assignment_points_at_its_line() {
    // A bare `assert(...)` expression in a top-level assignment on line 1.
    assert_eq!(err_line("x = assert(1 == 2) 5;\ncube(x);\n"), Some(1));
    // Same shape one line down → line 2, proving the stamp tracks the failing assignment, not a constant.
    assert_eq!(
        err_line("y = 3;\nx = assert(1 == 2) 5;\ncube(x);\n"),
        Some(2)
    );
}

#[test]
fn stamping_preserves_the_message() {
    // `Spanned` delegates Display to the inner error — the console text is byte-for-byte what it was.
    let e =
        evaluate_geometry("function bad() = assert(false, \"boom\") 1;\nx = bad();\ncube(x);\n")
            .unwrap_err();
    assert!(
        matches!(e, Error::Spanned { .. }),
        "a fatal fault is stamped"
    );
    assert!(
        e.to_string().contains("boom"),
        "the message still carries the assert text, got: {e}"
    );
}

#[test]
fn a_clean_program_has_no_error() {
    assert!(evaluate_geometry("a = 2;\ncube(a);\n").is_ok());
}
