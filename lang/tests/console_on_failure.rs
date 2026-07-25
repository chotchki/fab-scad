//! AP.2/AP.3 — a FAILED evaluation still hands back everything the program printed.
//!
//! The console is an output of evaluation, not a reward for succeeding. Before `Failure` existed it
//! lived in a `Ctx` side buffer that only escaped on the success path, so the instant a `?` fired every
//! echo and warning went with it. That is not a cosmetic loss: it made "this program printed nothing"
//! and "this program failed" the same observation to every caller, which is how `differ` ended up
//! comparing empty vectors and passing vacuously (AN.18), and how `corpus_repro` printed a bucket with
//! no trail to it.
//!
//! These tests exist because that regression is INVISIBLE. Nothing goes red if the console starts
//! getting dropped again — values still agree, errors still propagate, the suite stays green. Only an
//! explicit assertion that a failure carries its console can catch it.
//!
//! The faults here are all HOIST-time (`x = assert(…) …` in a top-level assignment) for a reason worth
//! knowing: a failed `assert` in STATEMENT position is deliberately NOT fatal (L.5.8 — upstream prints
//! the error but still exports the geometry accumulated before it, so `geo_stack` converts that one
//! error into a warning and stops). The hoist has no such catch, so it is where a genuine fatal lives.
//! `an_assert_statement_is_not_fatal_at_all` pins that difference rather than leaving it folklore.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use fab_lang::{Message, evaluate_geometry_full};

/// The rendered console of a run that is expected to FAIL.
fn console_of_failure(src: &str) -> Vec<String> {
    let failure = evaluate_geometry_full(src).expect_err("expected this program to fail");
    failure.console().iter().map(Message::render).collect()
}

#[test]
fn a_fatal_fault_keeps_the_echoes_that_preceded_it() {
    // Both echoes run during the hoist (the `echo(…) value` EXPRESSION form), then the third binding
    // faults. Everything printed before the fault survives, with the fault last.
    let console = console_of_failure(
        "a = echo(\"first\") 1;\nb = echo(\"second\") 2;\nx = assert(1 == 2) 5;\ncube(x);\n",
    );
    assert_eq!(
        console.len(),
        3,
        "two echoes plus the terminal error: {console:?}"
    );
    assert_eq!(console[0], "ECHO: \"first\"");
    assert_eq!(console[1], "ECHO: \"second\"");
    assert!(
        console[2].starts_with("ERROR: "),
        "the fault is the LAST console line: {console:?}"
    );
}

#[test]
fn a_failure_keeps_warnings_too_not_just_echoes() {
    // The warning half matters independently: `differ`'s warning channel is the one AN.13 built and
    // AN.18 found blind, and it reads exactly this path.
    let console = console_of_failure("a = let(q = 1, q = 2) q;\nx = assert(1 == 2) 5;\ncube(x);\n");
    assert!(
        console
            .iter()
            .any(|l| l.starts_with("WARNING: ")
                && l.contains("Ignoring duplicate variable assignment")),
        "the duplicate-binding warning survives the later fault: {console:?}"
    );
    assert!(console.last().is_some_and(|l| l.starts_with("ERROR: ")));
}

#[test]
fn the_error_is_the_terminal_line_and_appears_exactly_once() {
    // `Failure` holds ONE `Error`, so this is true by construction — asserted anyway, because
    // `console()` synthesizing the line is the part that could break it.
    let console = console_of_failure("a = echo(\"x\") 1;\nx = assert(1 == 2) 5;\ncube(x);\n");
    let errors = console.iter().filter(|l| l.starts_with("ERROR: ")).count();
    assert_eq!(errors, 1, "exactly one terminal error: {console:?}");
    assert!(console.last().is_some_and(|l| l.starts_with("ERROR: ")));
}

#[test]
fn the_error_is_not_stored_twice() {
    // `Failure::messages` holds ONLY what eval emitted; `console()` derives the ERROR line from the
    // `error` field. Two stored copies could drift, so the split is asserted rather than assumed.
    let failure = evaluate_geometry_full("a = echo(\"x\") 1;\nx = assert(1 == 2) 5;\ncube(x);\n")
        .expect_err("expected failure");
    assert!(
        !failure
            .messages
            .iter()
            .any(|m| matches!(m, Message::Error(_))),
        "the raw buffer carries no Error variant: {:?}",
        failure.messages
    );
    assert_eq!(failure.console().len(), failure.messages.len() + 1);
}

#[test]
fn an_assert_statement_is_not_fatal_at_all() {
    // L.5.8, and the reason the tests above use hoist faults. A STATEMENT-position assert is caught in
    // `geo_stack` and evaluation STOPS while KEEPING the geometry accumulated before it — matching
    // upstream, which for `cube(10); assert(false); cube(5);` exports the cube(10), drops the cube(5),
    // and exits 0. So this is `Ok`, not a `Failure` — but it is still reported as an ERROR (AP.7),
    // because a warning understates a render that halted.
    let (_, messages) = evaluate_geometry_full("cube(10);\nassert(false);\ncube(5);\n")
        .expect("a statement-position assert is not fatal (L.5.8)");
    let rendered: Vec<String> = messages.iter().map(Message::render).collect();
    assert!(
        rendered.iter().any(|l| l.starts_with("ERROR: ")),
        "a failed assert reports at ERROR level even though it is not fatal: {rendered:?}"
    );
}

#[test]
fn a_clean_run_is_unaffected() {
    // The success path must be byte-identical to before — this whole change is about the OTHER arm.
    let (_, messages) = evaluate_geometry_full("echo(\"ok\");\ncube(1);").expect("evaluates");
    let rendered: Vec<String> = messages.iter().map(Message::render).collect();
    assert_eq!(rendered, ["ECHO: \"ok\""]);
}
