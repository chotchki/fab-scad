//! `fab gen-diff` (AJ.8) — the ORACLE differential over generated programs: for each seed, run
//! the SAME fab-gen program through OUR evaluator+renderer AND the local OpenSCAD binary, then
//! compare the echo output line-for-line and the wall time.
//!
//! This is R.2's "values/echo first" made concrete on the fattened AJ grammar. The oracle runs
//! with `--enable=textmetrics --enable=object-function` so the experimental features we ship
//! always-on are live on both sides (a first probe run downgrades to no flags if the local build
//! rejects them). TIMING: the oracle pays process startup per program, so an EMPTY-program
//! baseline is measured first and subtracted; ours measures the same span (parse → eval → mesh
//! lower). Small generated programs mostly measure fixed overheads — the aggregate MEDIANS are
//! the signal, per-seed ratios are noise.
//!
//! Divergences report the first differing echo line with the seed (replay: `fab-gen` is
//! seed-deterministic), and classify oracle-side failures (timeout / render error) separately —
//! an oracle that refuses a program is a finding about the PROGRAM, not a value divergence.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::oracle;

/// The `--enable` flags handed to the oracle so both sides speak the same (experimental) surface.
const ORACLE_FLAGS: &[&str] = &["textmetrics", "object-function"];

/// One seed's outcome.
enum Outcome {
    /// Echo streams match; timing captured (ours, oracle-minus-baseline). `export_failed` marks
    /// a run whose EVAL agreed but whose oracle EXPORT refused the result (e.g. a 2D top level →
    /// "not a 3D object") — agreement, with an asterisk, counted separately.
    Match {
        ours_ms: f64,
        oracle_ms: f64,
        export_failed: bool,
    },
    /// First differing echo line.
    Diverge {
        line: usize,
        ours: String,
        oracle: String,
    },
    /// The oracle produced NOTHING comparable (timeout / spawn failure) — counted, not compared.
    OracleFailed(String),
    /// OUR side errored (a generated program must never do that — a generator/evaluator bug).
    OursFailed(String),
}

/// Run the differential over seeds `0..seeds`.
///
/// # Errors
/// Only on harness-level failures (no OpenSCAD binary at all); per-seed failures are outcomes.
pub fn run(seeds: u32, timeout_secs: u64, md: bool) -> Result<()> {
    let timeout = Duration::from_secs(timeout_secs);

    // Probe: can this oracle take our flags? (An old build rejecting --enable=object-function
    // downgrades the run to flagless — the object/metrics arms will then diverge, visibly.)
    let flags: &[&str] = if oracle::run_with_flags("cube(1);", timeout, ORACLE_FLAGS).is_ok() {
        ORACLE_FLAGS
    } else {
        &[]
    };

    // Capability probe: a pre-July-2026 oracle predates multi-letter swizzles (v.wy) — its
    // divergences on that family are VERSION SKEW against the master goldens we implement, not
    // findings. Detected once and labeled in the report.
    let skew_swizzles = matches!(
        oracle::run_with_flags("echo(([1, 2, 3, 4]).wy); cube(1);", timeout, flags),
        Ok(r) if r.echo.iter().any(|l| l.contains("undef"))
    );

    // Startup baseline: the cheapest possible render, thrice, take the minimum.
    let mut baseline = Duration::MAX;
    for _ in 0..3 {
        let r = oracle::run_with_flags("cube(1);", timeout, flags)
            .context("oracle baseline run (is OpenSCAD installed?)")?;
        baseline = baseline.min(r.duration);
    }

    let mut matches = 0u32;
    let mut export_fails = 0u32;
    let mut ours_times = Vec::new();
    let mut oracle_times = Vec::new();
    let mut diverges: Vec<(u32, usize, String, String)> = Vec::new();
    let mut skew: Vec<u32> = Vec::new();
    let mut oracle_fails: Vec<(u32, String)> = Vec::new();
    let mut ours_fails: Vec<(u32, String)> = Vec::new();

    for seed in 0..seeds {
        let src = fab_gen::generate(seed);
        match diff_one(&src, timeout, flags) {
            Outcome::Match {
                ours_ms,
                oracle_ms,
                export_failed,
            } => {
                matches += 1;
                export_fails += u32::from(export_failed);
                ours_times.push(ours_ms);
                oracle_times.push(oracle_ms);
            }
            Outcome::Diverge { line, ours, oracle } => {
                // Version-skew classification: a pre-July-2026 oracle lacks multi-letter swizzles,
                // so a divergence in a program that USES one is skew, not a finding.
                let multi_swizzle = [".wy", ".rgba", ".xyz", ".xyxy", ".xr"]
                    .iter()
                    .any(|m| src.contains(m));
                if skew_swizzles && multi_swizzle {
                    skew.push(seed);
                } else {
                    diverges.push((seed, line, ours, oracle));
                }
            }
            Outcome::OracleFailed(why) => oracle_fails.push((seed, why)),
            Outcome::OursFailed(why) => ours_fails.push((seed, why)),
        }
    }

    let med = |xs: &mut Vec<f64>| -> f64 {
        if xs.is_empty() {
            return 0.0;
        }
        xs.sort_by(f64::total_cmp);
        xs[xs.len() / 2]
    };
    let ours_med = med(&mut ours_times);
    let oracle_med = med(&mut oracle_times);
    let ratio = if ours_med > 0.0 {
        oracle_med / ours_med
    } else {
        0.0
    };

    let oracle_version = crate::openscad::Openscad::discover(None)
        .ok()
        .and_then(|o| o.tool_version())
        .unwrap_or_else(|| "unknown".to_string());
    let h = if md { "### " } else { "" };
    println!("{h}gen-diff — {seeds} seed(s), oracle: {oracle_version}, flags: {flags:?}");
    if skew_swizzles {
        println!(
            "note: this oracle predates multi-letter swizzles — swizzle-family divergences are \
             VERSION SKEW vs the master goldens, not findings"
        );
    }
    println!();
    println!(
        "{}{matches}/{seeds} echo-match ({export_fails} oracle-export-failed with agreeing eval). {} diverged, {} version-skew (multi-swizzle), {} oracle-failed, {} ours-failed.",
        if md { "**" } else { "" },
        diverges.len(),
        skew.len(),
        oracle_fails.len(),
        ours_fails.len(),
    );
    let base_ms = baseline.as_secs_f64() * 1e3;
    println!(
        "timing (medians, RAW): ours {ours_med:.1} ms, oracle {oracle_med:.1} ms (incl. ~{base_ms:.0} ms process startup; adjusted ≈ {:.1} ms) → raw oracle/ours {ratio:.2}x{}",
        (oracle_med - base_ms).max(0.0),
        if md { "**" } else { "" }
    );
    if !skew.is_empty() {
        println!("  skew seeds (oracle predates multi-swizzles): {skew:?}");
    }
    for (seed, line, ours, oracle) in &diverges {
        println!("  seed {seed}: echo line {line} — ours `{ours}` vs oracle `{oracle}`");
    }
    for (seed, why) in &oracle_fails {
        println!("  seed {seed}: oracle failed — {why}");
    }
    for (seed, why) in &ours_fails {
        println!("  seed {seed}: OURS failed — {why}");
    }
    Ok(())
}

/// Diff one program: ours (eval + mesh lower, timed) vs the oracle (timed, baseline-adjusted).
fn diff_one(src: &str, timeout: Duration, flags: &[&str]) -> Outcome {
    use crate::backend::{ManifoldBackend, build_geo};

    // OURS — the same span the oracle's render covers: parse → eval → lower to a mesh.
    let tmp = std::env::temp_dir();
    let start = Instant::now();
    let evaluated = crate::import::resolve_geometry_with_base_full(
        src,
        &tmp,
        &[],
        fab_lang::Config::from_env(),
    );
    let (tree, messages) = match evaluated {
        Ok(pair) => pair,
        Err(e) => return Outcome::OursFailed(format!("{e}")),
    };
    let _solid = build_geo(&tree, &ManifoldBackend); // None (empty) is fine — timing parity is the point
    let ours_ms = start.elapsed().as_secs_f64() * 1e3;

    // ORACLE — the raw Report, so a failed EXPORT (2D top level, empty result) still hands us
    // the eval's echo for comparison (AK.2: agreement was invisible through run_with_flags's
    // throw-away-on-failure). Only a timeout / spawn failure is uncomparable.
    let report = match oracle_report(src, timeout, flags) {
        Ok(r) => r,
        Err(e) => {
            let first = format!("{e}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            return Outcome::OracleFailed(first);
        }
    };
    if report.timed_out {
        return Outcome::OracleFailed("timeout".to_string());
    }
    let oracle_ms = report.duration.as_secs_f64() * 1e3;

    // Echo comparison — LINE streams on both sides (a raw multi-line echo splits into lines, which
    // is exactly how the oracle's console emits it).
    let ours_echo: Vec<String> = messages
        .iter()
        .filter_map(|m| match m {
            fab_lang::Message::Echo(s) => Some(s.clone()),
            fab_lang::Message::Warning(_) => None,
        })
        .flat_map(|s| s.lines().map(String::from).collect::<Vec<_>>())
        .collect();
    let oracle_echo: Vec<String> = report
        .echo
        .iter()
        .map(|l| l.strip_prefix("ECHO: ").unwrap_or(l).to_string())
        .collect();

    let n = ours_echo.len().max(oracle_echo.len());
    for i in 0..n {
        let a = ours_echo.get(i).map_or("<none>", |s| s.trim_end());
        let b = oracle_echo.get(i).map_or("<none>", |s| s.trim_end());
        if a != b {
            return Outcome::Diverge {
                line: i + 1,
                ours: a.chars().take(120).collect(),
                oracle: b.chars().take(120).collect(),
            };
        }
    }
    Outcome::Match {
        ours_ms,
        oracle_ms,
        export_failed: !report.ok,
    }
}

/// Run the oracle on `src` and return the RAW [`crate::openscad::Report`] — echo + timing survive
/// an export failure, unlike [`oracle::run_with_flags`]'s all-or-nothing result.
fn oracle_report(src: &str, timeout: Duration, flags: &[&str]) -> Result<crate::openscad::Report> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let osc = crate::openscad::Openscad::discover(None)?;
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir();
    let stem = format!("fab-gendiff-{}-{seq}", std::process::id());
    let scad = dir.join(format!("{stem}.scad"));
    let off = dir.join(format!("{stem}.off"));
    std::fs::write(&scad, src).with_context(|| format!("writing {}", scad.display()))?;
    let report = osc.render_with_flags(&scad, &off, timeout, flags);
    let _ = std::fs::remove_file(&scad);
    let _ = std::fs::remove_file(&off);
    report
}
