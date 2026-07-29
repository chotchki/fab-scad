//! AS.3 — the builtin-dispatch hot-path measurement. `apply` + `run_builtin` are the interpreter's
//! hottest seam (`is_num`/`is_undef`/`len` run into the millions on BOSL2), and the one-declaration
//! refactor must compile to the SAME dispatch shape — a match on the name, not a per-call hash. This
//! bench is the acceptance instrument: run it on both sides of the change, in RELEASE
//! (`cargo test -p fab-lang --release --test builtin_dispatch_bench -- --ignored --nocapture`).
//! `#[ignore]`d because a debug-mode timing is noise, not signal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test harness: unwrap/expect ARE the assertions; timing uses the I.6-sanctioned Instant"
)]

use std::time::Instant;

use fab_lang::{Scope, eval_program, parse};

#[test]
#[ignore = "timing measurement — run --release with --nocapture and read the numbers"]
fn builtin_dispatch_hot_path() {
    // One comprehension pass = 200k iterations x 5 builtin calls (sin, abs, min, is_num, len) = 1M
    // dispatches per eval, dominated by run_builtin -> apply rather than geometry or parsing.
    let program = parse(
        "xs = [for (i = [0 : 199999]) abs(sin(i)) + min(i, 3) + (is_num(i) ? len([i, i]) : 0)]; \
         echo(len(xs));",
    )
    .expect("parses");
    let runs = 5;
    let mut best = None::<std::time::Duration>;
    for run in 0..runs {
        let start = Instant::now();
        eval_program(&program, &Scope::new()).expect("evaluates");
        let elapsed = start.elapsed();
        println!("run {run}: {elapsed:?}");
        best = Some(best.map_or(elapsed, |b| b.min(elapsed)));
    }
    println!("AS.3 builtin dispatch, best of {runs}: {:?}", best.unwrap());
}
