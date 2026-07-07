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
/// something that passed now fails — a regression the suite catches. Baseline 2026-07-06: 873/901 pass
/// (96.9%) — 8 assertion, 11 unimplemented, 8 timeout, 0 crash. Evaluator fixes cleared 166 (`search` +25,
/// letrec +12, `rands` +27, function-value `str()` +58, range-indexing `r[0..2]` +12, island-global
/// bootstrapping +5 [L.2.8a: a top-level constant's fn call sees the constants hoisted so far → the
/// modular_hose `turtle([arc...])` cluster loads], empty-statement `$children` +5 [L.2.8b: a lone `;` is
/// not a child → the screw()/attachable `$children==2` family], seedless-`rands` advance +2 [L.2.8c: one
/// per-eval stream so consecutive `rands()` differ → plane_intersection's random line is non-degenerate],
/// unary-minus-on-matrix +4 [L.2.8d: `-[[…]]` negates element-wise not undef → rot_inverse/rot_resample],
/// C-style-for sequential binding +7 [L.2.8e: `for(…;…;x=…,y=x…)` — a later update sees the earlier one →
/// skin(method="distance")'s `_dp_distance_row` DP], `each if`/`each for` splices +4 [L.2.8f:
/// `each if(c) list` splices the list not `[[list]]` → nurbs_curve's `each if(…) lerpn(…)` sampling], nested-fn `str()` bare +2 [L.2.8g: `str()` renders nested function literals bare like OpenSCAD, not the canonical `(function (x) …)` → fnliterals f_1arg/f_2arg/f_3arg], let-in-vector transparency +3 [L.2.8h: `[let(x) [a,b]]` contributes one element (splices only if the body does) → trapezoid corner paths]);
/// the `expect_success=false` scorer fix corrected 21 more.
///
/// 2026-07-07: 873→877 (97.3%) — 4 assertion, 13 unimplemented, 7 timeout. Builtin named-args are POSITIONAL
/// +1 [L.2.4: an OpenSCAD builtin has no declared param names, so it reads every arg by source position and
/// ignores the name — the split-off-named map dropped them, defaulting `search`'s `index_col_num` to 0 →
/// test_in_list], bool ordering + range structural-equality land together as test_compare_vals /
/// test_typeof +2 [L.2.6: `false<true` coerces 0/1, and a range is SELF-equal even with a NaN step so
/// `is_nan([0:NAN:INF])` is false → "invalid"], is_num(NaN)=false +1 [L.2.8n: NaN routes to is_nan/typeof
/// "nan", never "number" → test_f_is_num], duplicate-param two-phase binding +0 net [modules/functions bind
/// ALL defaults then args, so BOSL2's `rounding_edge_mask`/`fillet` (param `r` listed twice) no longer see
/// `r=undef` — they now clear the `all_nonnegative` assert and block one step later on a module-body-local
/// `make_path`, an OPEN nested-definition gap (L.2.8m)]. NB f_acos's `(r/π)*180` rad2deg was tried + reverted
/// (regressed test_glued_circles's arc discretization; needs correctly-rounded acos — L.2.8i). Floored below
/// 877 for timeout jitter.
const PASS_FLOOR: usize = 875;

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

    // Full failure roster (file::name\tbucket\tdetail) to a stable path — for slicing the generic
    // `got==expected` long tail by the test NAME (which usually names the diverging function).
    let roster: String = results
        .iter()
        .filter(|r| r.bucket != Bucket::Pass)
        .map(|r| format!("{}\t{}\t{}\t{}\n", r.file, r.name, r.bucket.label(), r.detail))
        .collect();
    let roster_path = std::env::temp_dir().join("bosl2_fails.tsv");
    if std::fs::write(&roster_path, roster).is_ok() {
        eprintln!("\nfull failure roster → {}", roster_path.display());
    }

    assert!(
        pass >= PASS_FLOOR,
        "BOSL2 corpus regressed: {pass} pass < floor {PASS_FLOOR} (raise the floor when fixes land, \
         investigate when it drops)"
    );
}
