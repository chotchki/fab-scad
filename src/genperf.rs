//! AO.4-AO.7 — the HEAVY generated-program perf lane: fab vs the OpenSCAD binary on programs big
//! enough that the number means something.
//!
//! `gen-diff` (AJ.8) already times both engines, but on `Profile::CHEAP` programs whose whole point is
//! being cheap — so the oracle's wall time is essentially its process fork, the startup-adjusted median
//! saturates near zero, and the ratio is noise. This lane runs `Profile::heavy(dial)` instead and
//! reports what a real render costs.
//!
//! Five disciplines, each of which the number is worthless without:
//!
//! - **AO.4, the startup confound.** The oracle pays a process fork per program. Report BOTH the raw
//!   wall time and the fork-adjusted figure, plus what fraction of the oracle's time the fork actually
//!   was — a lane whose adjusted median is dominated by its own correction is not measuring rendering.
//! - **AO.5, cold renders.** `Config` is set EXPLICITLY here, never `from_env`: a benchmark whose
//!   numbers move with the caller's environment is not a benchmark. Cross-seed contamination (the
//!   W.3.17 cache-warm trap) is structurally impossible on this path — each seed builds its own `Ctx`
//!   and passes no persistent `GeoCache` — but the WITHIN-program CSG memo stays on, because that is
//!   what fab actually ships and the oracle has its own intra-render caches too.
//! - **AO.6, correctness gates timing.** A seed that DISAGREES with the oracle is not timed. A fast
//!   wrong answer scores nothing.
//! - **AO.10, the work floor.** A seed that agreed but rendered ~nothing is not timed either, and the
//!   rejected fraction is REPORTED. Both engines produce nothing equally fast, so agreement alone
//!   cannot tell you the corpus is still doing work — a corpus decaying toward trivial programs would
//!   otherwise read as a performance win.
//! - **AO.3, blowup guards.** A per-seed oracle timeout AND a `Config::eval_budget` on our side, so one
//!   pathological seed cannot eat the nightly from either end.
//!
//! SPAN PARITY: ours is timed over eval → lower → build → write the mesh to disk, because that is the
//! span the oracle's `-o out.off` covers. Dropping our export would hand fab a free win worth real
//! milliseconds on a large mesh.
//!
//! Budgeted in WALL CLOCK, not seeds (chotchki): heavy seeds vary enormously in cost, so a seed count
//! buys an unpredictable runtime — the wrong contract for something on a nightly schedule.

use std::time::{Duration, Instant};

use anyhow::Result;

/// The work floor (AO.10): a render under this many triangles was not a meaningful timing sample.
const MIN_TRIS: usize = 32;

/// What one seed contributed.
enum Seed {
    /// Agreed with the oracle AND did real work — the only kind that gets timed.
    Timed {
        ours_ms: f64,
        oracle_ms: f64,
        tris: usize,
    },
    /// Agreed, but rendered under the work floor (AO.10).
    NoWork,
    /// Echo streams differ — a correctness failure, excluded from timing (AO.6). Carries the seed and
    /// the first differing line so the row can NAME what broke: a divergence found here is worth more
    /// than the timing that found it, and a bare count is not actionable.
    Disagreed {
        seed: u32,
        line: usize,
        ours: String,
        oracle: String,
    },
    /// Our side could not evaluate it (including an `eval_budget` trip).
    OursFailed,
    /// The oracle could not run it (timeout, spawn failure).
    OracleFailed,
}

/// Run the heavy lane across `dials`, splitting `budget` evenly between them.
///
/// One dial gives a number; SEVERAL give AO.7's scaling curve — where we win or lose as programs grow,
/// which is the part a single median cannot tell you.
///
/// # Errors
/// Only a failure to reach the oracle binary at all — an individual seed's failure is data, not an error.
pub fn run(
    dials: &[u32],
    budget: Duration,
    per_seed_timeout: Duration,
    eval_budget: Option<u64>,
    md: bool,
) -> Result<()> {
    // The SAME language surface on both legs, negotiated once (see `gendiff::negotiate_flags`).
    let flags = crate::gendiff::negotiate_flags(per_seed_timeout);
    let startup_ms = oracle_startup_ms(per_seed_timeout, flags)?;
    let per_dial = budget / u32::try_from(dials.len().max(1)).unwrap_or(1);

    let mut rows = Vec::new();
    for &dial in dials {
        let profile = fab_gen::Profile::heavy(dial);
        let began = Instant::now();
        let mut results = Vec::new();
        let mut seed = 0u32;
        while began.elapsed() < per_dial {
            results.push(one_seed(
                seed,
                profile,
                per_seed_timeout,
                eval_budget,
                flags,
            ));
            seed += 1;
        }
        rows.push(summarize(dial, &results, startup_ms, began.elapsed(), seed));
    }
    render(&rows, startup_ms, md);
    Ok(())
}

/// The oracle's per-program process cost, measured once — the AO.4 correction term.
///
/// A do-nothing program isolates the fork + startup: whatever it takes to render nothing is what every
/// other measurement carries on top of its real work. Median of 5 so one scheduling hiccup can't set it.
fn oracle_startup_ms(timeout: Duration, flags: &[&str]) -> Result<f64> {
    let mut samples: Vec<f64> = Vec::new();
    for _ in 0..5 {
        let r = crate::gendiff::oracle_report("cube(1);\n", timeout, flags)?;
        samples.push(r.duration.as_secs_f64() * 1e3);
    }
    Ok(median(&mut samples))
}

/// A divergence line with its seed stripped — the part that identifies the BUG rather than the input
/// that happened to find it. Used to collapse "29 seeds disagreed" into "N distinct disagreements".
fn shape(line: &str) -> &str {
    line.split_once(", ").map_or(line, |(_, rest)| rest)
}

/// Median of a non-empty slice (every caller checks).
fn median(xs: &mut [f64]) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// Evaluate + render one seed on both engines.
fn one_seed(
    seed: u32,
    profile: fab_gen::Profile,
    timeout: Duration,
    eval_budget: Option<u64>,
    flags: &[&str],
) -> Seed {
    use crate::backend::{ManifoldBackend, build_geo_cold};

    let src = fab_gen::generate_with(seed, profile);

    // AO.5 — an EXPLICIT config, not `from_env`. `csg_cache` matches what `from_env` ships (on) so the
    // measurement reflects the real renderer; the accelerators `from_env` leaves off stay off. The
    // eval budget is AO.3's half of the blowup guard: our side's answer to the oracle's timeout.
    let config = fab_lang::Config {
        csg_cache: true,
        eval_budget,
        ..fab_lang::Config::default()
    };

    let tmp = std::env::temp_dir();
    let stl = tmp.join(format!("fab-genperf-{}-{seed}.stl", std::process::id()));

    let start = Instant::now();
    let Ok((tree, messages)) =
        crate::import::resolve_geometry_with_base_full(&src, &tmp, &[], config)
    else {
        return Seed::OursFailed;
    };
    let solid = build_geo_cold(&tree, &ManifoldBackend);
    // The export leg — the oracle's `-o` pays for one, so ours does too (see SPAN PARITY above).
    if let Some(s) = solid.as_ref() {
        let _ = s.write_stl(&stl);
    }
    let ours_ms = start.elapsed().as_secs_f64() * 1e3;
    let _ = std::fs::remove_file(&stl);
    let tris = solid.as_ref().map_or(0, crate::kernel::Solid::num_tri);

    let Ok(report) = crate::gendiff::oracle_report(&src, timeout, flags) else {
        return Seed::OracleFailed;
    };
    // A CRASHED oracle answered nothing — scoring it as a disagreement would bury the real
    // divergences under upstream's own aborts (AO.4 found one: `search("a", [[1,2]])`).
    if report.timed_out || report.crashed {
        return Seed::OracleFailed;
    }

    // AO.6 — correctness first. A divergence means the two timings describe different computations.
    if let Some((line, ours, oracle)) =
        crate::gendiff::first_echo_divergence(&messages, &report.echo)
    {
        return Seed::Disagreed {
            seed,
            line,
            ours,
            oracle,
        };
    }

    // AO.10 — and did it actually do anything?
    if tris < MIN_TRIS {
        return Seed::NoWork;
    }
    Seed::Timed {
        ours_ms,
        oracle_ms: report.duration.as_secs_f64() * 1e3,
        tris,
    }
}

/// One dial's verdict — a point on AO.7's scaling curve.
struct Row {
    dial: u32,
    seeds: u32,
    timed: usize,
    ours_ms: f64,
    oracle_ms: f64,
    /// The oracle's median MINUS the measured fork cost (AO.4).
    adjusted_ms: f64,
    /// What fraction of the oracle's raw number the fork was. Over ~5% and the row is timing startup.
    startup_share: f64,
    avg_tris: usize,
    secs: f64,
    no_work: usize,
    disagreed: usize,
    ours_failed: usize,
    oracle_failed: usize,
    /// One representative per DISTINCT disagreement (see [`shape`]).
    divergences: Vec<String>,
}

impl Row {
    /// Adjusted oracle over ours: above 1.0 we are faster.
    fn ratio(&self) -> f64 {
        self.adjusted_ms / self.ours_ms.max(0.001)
    }

    fn rejected(&self) -> usize {
        self.no_work + self.disagreed + self.ours_failed + self.oracle_failed
    }
}

/// Reduce one dial's seeds to a [`Row`].
fn summarize(dial: u32, results: &[Seed], startup_ms: f64, elapsed: Duration, seeds: u32) -> Row {
    let mut ours: Vec<f64> = Vec::new();
    let mut oracle: Vec<f64> = Vec::new();
    let (mut no_work, mut ours_failed, mut oracle_failed) = (0, 0, 0);
    let mut tri_total = 0usize;
    let mut divergences: Vec<String> = results
        .iter()
        .filter_map(|r| match r {
            Seed::Disagreed {
                seed,
                line,
                ours,
                oracle,
            } => Some(format!(
                "seed {seed}, echo line {line}: ours {ours:?} vs oracle {oracle:?}"
            )),
            _ => None,
        })
        .collect();
    // DEDUPE by the divergence itself, not the seed — 29 seeds tripping one bug is one bug, and the
    // raw count reads like 29 problems. What matters is how many DISTINCT ways we disagree.
    let disagreed = divergences.len();
    divergences.sort();
    divergences.dedup_by(|a, b| shape(a) == shape(b));
    for r in results {
        match *r {
            Seed::Timed {
                ours_ms,
                oracle_ms,
                tris,
            } => {
                ours.push(ours_ms);
                oracle.push(oracle_ms);
                tri_total += tris;
            }
            Seed::NoWork => no_work += 1,
            Seed::OursFailed => ours_failed += 1,
            Seed::OracleFailed => oracle_failed += 1,
            Seed::Disagreed { .. } => {}
        }
    }
    let timed = ours.len();
    // A dial with no timeable seeds still gets a row — zeroes and a full reject breakdown say more
    // than a missing line, which reads as "not run".
    let (ours_med, oracle_med) = if timed == 0 {
        (0.0, 0.0)
    } else {
        (median(&mut ours), median(&mut oracle))
    };
    Row {
        dial,
        seeds,
        timed,
        ours_ms: ours_med,
        oracle_ms: oracle_med,
        adjusted_ms: (oracle_med - startup_ms).max(0.0),
        startup_share: if oracle_med > 0.0 {
            startup_ms / oracle_med * 100.0
        } else {
            0.0
        },
        avg_tris: tri_total.checked_div(timed).unwrap_or(0),
        secs: elapsed.as_secs_f64(),
        no_work,
        disagreed,
        ours_failed,
        oracle_failed,
        divergences,
    }
}

/// The divergence half of a row — capped, because a nightly summary that dumps hundreds of lines gets
/// scrolled past, and the DISTINCT count is the number that matters anyway.
fn report_divergences(r: &Row) {
    const SHOWN: usize = 5;
    if r.divergences.is_empty() {
        return;
    }
    println!(
        "  {} DISTINCT disagreements across {} seeds — each is a correctness bug worth more than the \
         timing that found it:",
        r.divergences.len(),
        r.disagreed
    );
    for d in r.divergences.iter().take(SHOWN) {
        println!("    {d}");
    }
    if r.divergences.len() > SHOWN {
        println!(
            "    ... and {} more (re-run at a single dial to see them all)",
            r.divergences.len() - SHOWN
        );
    }
}

/// Print the curve.
fn render(rows: &[Row], startup_ms: f64, md: bool) {
    if md {
        println!(
            "| dial | timed | ours ms | oracle raw ms | oracle adj ms | fork % | ratio | avg tris | rejected |"
        );
        println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        for r in rows {
            println!(
                "| {} | {} | {:.1} | {:.1} | {:.1} | {:.0}% | {:.2}x | {} | {}/{} |",
                r.dial,
                r.timed,
                r.ours_ms,
                r.oracle_ms,
                r.adjusted_ms,
                r.startup_share,
                r.ratio(),
                r.avg_tris,
                r.rejected(),
                r.seeds
            );
        }
        println!();
        println!(
            "Oracle process startup measured at {startup_ms:.1} ms — the adjustment subtracted from \
             every raw oracle time. `ratio` is adjusted-oracle over ours: above 1.00x we are faster."
        );
        for r in rows {
            if r.divergences.is_empty() {
                continue;
            }
            println!();
            println!(
                "**dial {}: {} distinct disagreements** across {} seeds — each is a correctness bug \
                 worth more than the row above it.",
                r.dial,
                r.divergences.len(),
                r.disagreed
            );
            for d in &r.divergences {
                println!("- `{d}`");
            }
        }
        return;
    }

    println!(
        "oracle process startup: {startup_ms:.1} ms (subtracted from every raw oracle time — AO.4)"
    );
    for r in rows {
        if r.timed == 0 {
            println!(
                "dial {}: NO TIMEABLE SEEDS in {:.0}s ({} tried: {} under the work floor, {} \
                 disagreed, {} we failed, {} the oracle failed) — the lane measured nothing, which \
                 is a result, not an error",
                r.dial, r.secs, r.seeds, r.no_work, r.disagreed, r.ours_failed, r.oracle_failed
            );
            continue;
        }
        println!(
            "dial {}: {} timed seeds in {:.0}s — ours {:.1} ms, oracle {:.1} ms raw / {:.1} ms \
             adjusted (the fork is {:.0}% of the oracle's number), ratio {:.2}x, avg {} tris",
            r.dial,
            r.timed,
            r.secs,
            r.ours_ms,
            r.oracle_ms,
            r.adjusted_ms,
            r.startup_share,
            r.ratio(),
            r.avg_tris
        );
        println!(
            "  rejected {}/{}: {} under the {MIN_TRIS}-tri work floor, {} disagreed with the oracle, \
             {} we could not evaluate, {} the oracle could not render",
            r.rejected(),
            r.seeds,
            r.no_work,
            r.disagreed,
            r.ours_failed,
            r.oracle_failed
        );
        if r.startup_share > 5.0 {
            println!(
                "  NOTE: the fork is over 5% of the oracle's time — dial UP until it isn't, or this \
                 row is dominated by process startup (AO.4)"
            );
        }
        report_divergences(r);
    }
}
