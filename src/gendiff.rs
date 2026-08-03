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
/// An oracle that predates a flag just WARNS "unknown feature" and runs (probed on 2026.07.20) —
/// so listing newer flags is safe on old oracles; the swizzle skew-probe below detects what the
/// binary actually speaks. `vector-swizzle` + `import-function` gate surfaces we ship always-on.
const ORACLE_FLAGS: &[&str] = &[
    "textmetrics",
    "object-function",
    "vector-swizzle",
    "import-function",
];

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
    /// BOTH sides refused the program. On the BUILTIN surface this is unreachable by design; on a
    /// LIBRARY surface it is the common case and not a finding — BOSL2 asserts its own preconditions
    /// (`is_path`, `is_consistent`, `is_description`), and a generator feeding arbitrary values will
    /// trip them constantly. Upstream raises there too, so the tiers AGREE about refusing.
    ///
    /// Compared on PRESENCE, not on text: our fault wording is deliberately not upstream's
    /// (`check_assert` settled that matching it word-for-word is a non-goal), so an error CHANNEL
    /// could only ever compare whether both refused. Counting these as `OursFailed` — which is what
    /// the lane did before AR.36 — leaves them uncompared and reads as a generator bug.
    BothRefused,
}

/// What `--enable` flags this oracle actually accepts.
///
/// Probe: can it take ours? An old build rejecting `--enable=object-function` downgrades the run to
/// flagless — the object/metrics arms will then diverge, VISIBLY, rather than silently.
///
/// Shared with the AO.4 perf lane deliberately. Its first cut hardcoded `&[]` and duly reported 25
/// "correctness divergences" that were one missing flag: upstream answers `undef` for every
/// `object()` call unless the experimental feature is enabled, and the generator emits `object()`
/// constantly. A differential harness that doesn't negotiate the SAME language surface on both legs
/// isn't measuring the implementation, it's measuring the flags.
pub(crate) fn negotiate_flags(timeout: Duration) -> &'static [&'static str] {
    if oracle::run_with_flags("cube(1);", timeout, ORACLE_FLAGS).is_ok() {
        ORACLE_FLAGS
    } else {
        &[]
    }
}

/// Run the differential over seeds `0..seeds`.
///
/// # Errors
/// Only on harness-level failures (no OpenSCAD binary at all); per-seed failures are outcomes.
pub fn run(seeds: u32, timeout_secs: u64, md: bool) -> Result<()> {
    run_surface(seeds, timeout_secs, md, Surface::Builtins)
}

/// Which SURFACE the generator draws its calls from (AR.36, generalized at AR.37).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Surface {
    /// OpenSCAD's own builtins — `sin`, `len`, `concat`. Reaches the dispatch machinery and never
    /// a single transpiled native.
    #[default]
    Builtins,
    /// BOSL2's 1329 declared callables.
    Bosl2,
    /// MCAD's 39. A different SHAPE of library (AR.37) and one the emitter was never built against.
    Mcad,
}

impl Surface {
    /// The submodule this surface needs, relative to the repo root, and the library's own name.
    fn asset(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Builtins => None,
            Self::Bosl2 => Some(("libs/BOSL2/std.scad", "BOSL2")),
            Self::Mcad => Some(("libs/MCAD/constants.scad", "MCAD")),
        }
    }
}

/// [`run`] over a chosen SURFACE (AR.36).
///
/// A LIBRARY surface generates against that library's declared callables instead of the builtins,
/// which is the only lane that checks the TRANSPILER against ground truth. Every other differential
/// in the tree compares our compiled tier against our own interpreter — so if both are wrong the
/// same way, they agree. This one asks OpenSCAD.
///
/// It is also a different KIND of program: `names_bind` is true on every library decl and false on
/// every builtin, so the whole named-argument family is unreachable from the builtin lane.
///
/// # Errors
/// As [`run`], plus a missing submodule when a library surface is asked for.
pub fn run_surface(seeds: u32, timeout_secs: u64, md: bool, surface: Surface) -> Result<()> {
    let timeout = Duration::from_secs(timeout_secs);
    // The fab-scad root: the oracle needs it for `OPENSCADPATH`, we need it for `library_paths`.
    let root = surface
        .asset()
        .map(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    if let (Some(r), Some((asset, name))) = (&root, surface.asset()) {
        anyhow::ensure!(
            r.join(asset).exists(),
            "the {name} surface needs {asset} checked out"
        );
    }
    let libs: Vec<std::path::PathBuf> = root
        .as_ref()
        .map(|r| vec![r.join("libs"), r.join("scad-lib")])
        .unwrap_or_default();
    // SZ.4 split the transpiled band out of `kernel`, so a LEAN build has no library crates to
    // generate against. That is a REFUSAL, not a compile error: the lane is meaningful only against
    // a band, and silently falling back to the builtin surface would report a green 200/200 for a
    // run that never called a library function — the exact shape of vacuous pass this phase keeps
    // finding.
    let surface: Option<fab_gen::NativeSurface> = match surface {
        Surface::Builtins => None,
        #[cfg(feature = "bosl2")]
        Surface::Bosl2 => Some(fab_gen::NativeSurface::from_library(&fab_bosl2::Bosl2)),
        #[cfg(feature = "mcad")]
        Surface::Mcad => Some(fab_gen::NativeSurface::from_library(&fab_mcad::Mcad)),
        #[cfg(not(all(feature = "bosl2", feature = "mcad")))]
        other => anyhow::bail!(
            "this build carries no transpiled band, so the {other:?} surface has nothing to \
             generate against — rebuild with the `libraries` feature"
        ),
    };

    let flags = negotiate_flags(timeout);

    // Capability probe: an oracle without `vector-swizzle` (the flag or the feature — pre-July-2026
    // builds lack both) yields undef on v.wy — its divergences on that family are VERSION SKEW
    // against the master goldens we implement, not findings. Detected once, labeled in the report;
    // on an oracle that speaks the flag this comes back false and the family compares for real.
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
    let mut both_refused = 0u32;
    let mut export_fails = 0u32;
    let mut ours_times = Vec::new();
    let mut oracle_times = Vec::new();
    let mut diverges: Vec<(u32, usize, String, String)> = Vec::new();
    let mut skew: Vec<u32> = Vec::new();
    let mut oracle_fails: Vec<(u32, String)> = Vec::new();
    let mut ours_fails: Vec<(u32, String)> = Vec::new();

    for seed in 0..seeds {
        let src = match &surface {
            Some(s) => fab_gen::generate_against(seed, fab_gen::Profile::AB, s),
            None => fab_gen::generate(seed),
        };
        match diff_one(&src, timeout, flags, &libs, root.as_deref()) {
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
            // Both refused: agreement about the program being invalid, which on a library surface
            // is the majority of what a random generator produces.
            Outcome::BothRefused => both_refused += 1,
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
        "{}{matches}/{seeds} echo-match ({export_fails} oracle-export-failed with agreeing eval), \
         {both_refused} both-refused. {} diverged, {} version-skew (multi-swizzle), {} \
         oracle-failed, {} ours-failed.",
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
fn diff_one(
    src: &str,
    timeout: Duration,
    flags: &[&str],
    libs: &[std::path::PathBuf],
    root: Option<&std::path::Path>,
) -> Outcome {
    use crate::backend::{ManifoldBackend, build_geo};

    // OURS — the same span the oracle's render covers: parse → eval → lower to a mesh.
    let tmp = std::env::temp_dir();
    let start = Instant::now();
    let evaluated = crate::import::resolve_geometry_with_base_full(
        src,
        &tmp,
        libs,
        fab_lang::Config::from_env(),
    );
    let (tree, messages) = match evaluated {
        Ok(pair) => pair,
        Err(e) => {
            // ASK THE ORACLE ANYWAY. Returning here without doing so is what left four of sixty
            // BOSL2-surface seeds uncompared: a generated call into a library that asserts its own
            // preconditions raises on BOTH engines, and refusing to look means never learning
            // whether upstream agreed.
            return match oracle_report(src, timeout, flags, root) {
                Ok(r)
                    if !r.ok
                        || r.warnings
                            .iter()
                            .any(|l| l.trim_start().starts_with("ERROR:")) =>
                {
                    Outcome::BothRefused
                }
                Ok(_) => Outcome::OursFailed(format!("{e}")),
                // The oracle produced nothing comparable, so we cannot say who was right.
                Err(_) => Outcome::OursFailed(format!("{e}")),
            };
        }
    };
    let _solid = build_geo(&tree, &ManifoldBackend); // None (empty) is fine — timing parity is the point
    let ours_ms = start.elapsed().as_secs_f64() * 1e3;

    // ORACLE — the raw Report, so a failed EXPORT (2D top level, empty result) still hands us
    // the eval's echo for comparison (AK.2: agreement was invisible through run_with_flags's
    // throw-away-on-failure). Only a timeout / spawn failure is uncomparable.
    let report = match oracle_report(src, timeout, flags, root) {
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

    if let Some((line, ours, oracle)) = first_echo_divergence(&messages, &report.echo) {
        return Outcome::Diverge { line, ours, oracle };
    }
    Outcome::Match {
        ours_ms,
        oracle_ms,
        export_failed: !report.ok,
    }
}

/// The first differing ECHO line between our console and the oracle's, or `None` when the streams
/// match. Returns `(1-based line, ours, oracle)` with each side clipped for reporting.
///
/// ONE comparison shared by `gen-diff` and the AO.4 perf lane, deliberately: the perf lane's first cut
/// rolled its own and reported a 10% disagreement rate that was entirely its own missing multi-line
/// split. Two implementations of "do these agree" is two chances to be subtly wrong about it.
pub(crate) fn first_echo_divergence(
    ours: &[fab_lang::Message],
    oracle: &[String],
) -> Option<(usize, String, String)> {
    // LINE streams on both sides — a raw multi-line echo splits into lines, which is exactly how the
    // oracle's console emits it.
    let mine: Vec<String> = ours
        .iter()
        .filter_map(fab_lang::Message::echo)
        .flat_map(|s| s.lines().map(String::from).collect::<Vec<_>>())
        .collect();
    let theirs: Vec<String> = oracle
        .iter()
        .map(|l| l.strip_prefix("ECHO: ").unwrap_or(l).to_string())
        .collect();

    let n = mine.len().max(theirs.len());
    (0..n).find_map(|i| {
        let a = mine.get(i).map_or("<none>", |s| s.trim_end());
        let b = theirs.get(i).map_or("<none>", |s| s.trim_end());
        (a != b).then(|| {
            (
                i + 1,
                a.chars().take(120).collect(),
                b.chars().take(120).collect(),
            )
        })
    })
}

/// Run the oracle on `src` and return the RAW [`crate::openscad::Report`] — echo + timing survive
/// an export failure, unlike [`oracle::run_with_flags`]'s all-or-nothing result.
pub(crate) fn oracle_report(
    src: &str,
    timeout: Duration,
    flags: &[&str],
    root: Option<&std::path::Path>,
) -> Result<crate::openscad::Report> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    // AR.36 — with a ROOT, `OPENSCADPATH` carries `libs/`, which is the only way a generated
    // program that `include`s BOSL2 resolves on the oracle side. Without it every such program
    // fails identically on both engines, and a differential reads that as agreement.
    let osc = crate::openscad::Openscad::discover(root)?;
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
