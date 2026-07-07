//! BOSL2 test corpus (K.1, tier 2) — sweep BOSL2's own assert-based `.scadtest` suite through scad-rs.
//! Each `[[test]]` script includes `std.scad` + asserts BOSL2's expected values, so a script that evaluates
//! without error means our evaluator matched BOSL2 (the asserts ARE the spec — no oracle). Each test runs in
//! an ISOLATED worker subprocess so a stack overflow buckets as a crash instead of aborting the sweep.
//!
//! `#[ignore]` — it's a ~minutes-long full sweep (900+ subprocess-isolated tests), so it runs on demand /
//! in CI, not on every `cargo test`: `cargo test -p fab-scad --test bosl2_corpus -- --ignored --nocapture`.
//! It prints the divergence report (pass count + failure buckets + samples) and RATCHETS on the pass count,
//! so a regression fails while a fix bumps the baseline. Skips cleanly when `libs/BOSL2` isn't checked out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_scad::corpus::{Bucket, check_worker, histogram, run_bosl2_corpus_isolated, signatures};

/// The pinned pass-count floor (the ratchet). Raise it as fixes land in the L.2 burn-down; a DROP means
/// something that passed now fails — a regression the suite catches. Baseline 2026-07-06: 723/901 pass
/// (80.2%) — 128 assertion, 44 unimplemented, 6 timeout, 0 crash. Two foundational L.2 fixes cleared 37:
/// `search` list-match-miss (686→711, string/list layer) + letrec self-recursive function literals
/// (711→723, strip_left/fnliterals). Floored below 723 to tolerate timeout-timing jitter, not regressions.
const PASS_FLOOR: usize = 717;

#[test]
#[ignore = "minutes-long full BOSL2 sweep; run explicitly with --ignored"]
fn bosl2_corpus_ratchet_and_report() {
    let bosl2 = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/BOSL2");
    if !bosl2.join("std.scad").exists() {
        eprintln!("note: libs/BOSL2 submodule not checked out — BOSL2 corpus skipped");
        return;
    }
    let worker = Path::new(env!("CARGO_BIN_EXE_corpus_worker"));
    check_worker(worker).expect("corpus worker built");

    let results = run_bosl2_corpus_isolated(&bosl2, worker).expect("corpus runs");
    let hist = histogram(&results);
    let total = results.len();
    let pass = hist.get(&Bucket::Pass).copied().unwrap_or(0);

    // The report: overall + the failure histogram + a few samples per non-pass bucket (the triage).
    let pct = 100.0 * pass as f64 / total as f64;
    eprintln!("\n=== BOSL2 corpus: {pass}/{total} pass ({pct:.1}%) ===");
    for (bucket, n) in &hist {
        eprintln!("  {:<14} {n}", bucket.label());
    }
    for bucket in [
        Bucket::Assertion,
        Bucket::Unimplemented,
        Bucket::Eval,
        Bucket::Crash,
        Bucket::Timeout,
        Bucket::Load,
        Bucket::Lower,
        Bucket::Parse,
    ] {
        let samples: Vec<_> = results.iter().filter(|r| r.bucket == bucket).take(6).collect();
        if !samples.is_empty() {
            eprintln!("--- {} (first {} of {}) ---", bucket.label(), samples.len(), hist.get(&bucket).copied().unwrap_or(0));
            for r in samples {
                eprintln!("  {}::{}: {}", r.file, r.name, r.detail);
            }
        }
    }

    // The burn-down worklist: the biggest failure clusters (same root cause), highest-leverage first.
    eprintln!("\n=== top failure signatures (count × bucket: first-line) ===");
    for (bucket, detail, count) in signatures(&results).into_iter().take(40) {
        eprintln!("  {count:>4} × {:<13} {}", bucket.label(), detail);
    }

    assert!(
        pass >= PASS_FLOOR,
        "BOSL2 corpus regressed: {pass} pass < floor {PASS_FLOOR} (raise the floor when fixes land, \
         investigate when it drops)"
    );
}
