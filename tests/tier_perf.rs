//! AR.35 — WHAT EACH TIER IS WORTH, on real models, before AR.21 deletes two of them.
//!
//! chotchki's gate on the deletion, and the question it answers is narrow on purpose: the transpiler
//! was bought as a MAINTENANCE win, not a performance one (AR.1), so this is not looking for a speed
//! case. It is checking that removing the hand tiers does not COST anything a user would feel.
//!
//! # Both spans, because one of them is misleading on its own
//!
//! AR.2 measured the hand natives at ~37% of the EVAL tier and ~8% of WALL time on `frame_upper`.
//! Geometry dominates a real render, so a wall-clock-only comparison drowns the tier question in
//! Manifold. This reports EVAL (source → geometry tree, which is the whole span a native can affect)
//! and WALL (…→ mesh, what a user waits for) separately. A tier that halves eval and moves wall by 3%
//! has done exactly that, and saying so is the point.
//!
//! # The legs
//!
//! Four in-process configurations plus the oracle. `interp` is the floor every other leg is measured
//! against; `hand` is what AR.21 deletes (`hand+jit` went out with AR.21.1 — see below); `transpiler`
//! is what replaces it; and
//! `shipped` is the registry the product actually builds today (fab-lang's rows first, then BOSL2 —
//! so its 66 hand rows win the names they share, which is what makes the deletion inert).
//!
//! # Disciplines, inherited from the AO lane because they are what make a number mean anything
//!
//! Config is set EXPLICITLY, never `from_env` — a benchmark whose numbers move with the caller's
//! environment is not a benchmark. Every leg renders the same model from cold. A model that FAILS on
//! any leg is dropped from the aggregate rather than timed, because a fast wrong answer scores
//! nothing, and the drop count is reported. The oracle pays a process fork per model, which is
//! reported rather than silently subtracted.
//!
//! RUN IT IN RELEASE. A debug build does not inline, which is most of what a compiled native buys,
//! so debug numbers understate every tier that is not the interpreter and overstate the interpreter
//! against itself. `cargo nextest run --release -p fab-scad --test tier_perf --run-ignored
//! ignored-only --no-capture`. The committed `perf/baseline.json` wall figures are release too.
//!
//! `--ignored`: it renders the whole models corpus five ways and takes minutes.

// SZ.4 split the transpiled band out of `kernel`, so a lean build has no fab-bosl2 to compare
// against. Comparing tiers is this file's entire job, so it compiles only where the band exists.
#![cfg(feature = "bosl2")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;

/// The per-render step allowance. Generous enough that most of the corpus completes on the
/// INTERPRETER (the slowest leg by construction), bounded enough that one pathological model cannot
/// eat the run.
const BUDGET_STEPS: u64 = 200_000_000;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn libs() -> Vec<PathBuf> {
    vec![manifest().join("libs"), manifest().join("scad-lib")]
}

/// The tier configurations under test.
///
/// The `hand+jit` leg is GONE with AR.21.1 — measured at 38.6 s against `hand`'s 38.2 s, i.e. inside
/// the noise, which is what made deleting the JIT cost nothing. The `hand` leg follows it out at
/// AR.21.2 and this reduces to interpreter-versus-transpiler, which is the comparison that keeps
/// meaning something afterwards.
struct Leg {
    name: &'static str,
    intrinsics: bool,
    /// `None` = fab-lang's own rows (`Registry::builtin()`); `Some(true)` = BOSL2 only;
    /// `Some(false)` = fab-lang's rows THEN BOSL2's, which is what `import::registry()` builds.
    bosl2: Option<bool>,
}

const LEGS: &[Leg] = &[
    // The floor. Everything else is measured against this.
    Leg {
        name: "interp",
        intrinsics: false,
        bosl2: None,
    },
    // What AR.21 deletes.
    Leg {
        name: "hand",
        intrinsics: true,
        bosl2: None,
    },
    // What replaces it.
    Leg {
        name: "transpiler",
        intrinsics: true,
        bosl2: Some(true),
    },
    // What ships today: both, hand first.
    Leg {
        name: "shipped",
        intrinsics: true,
        bosl2: Some(false),
    },
];

fn registry_for(leg: &Leg) -> Registry {
    match leg.bosl2 {
        None => Registry::new().with(fab_lang::surface::Natives.rows()),
        Some(true) => Registry::new().with(fab_bosl2::Bosl2.rows()),
        Some(false) => Registry::new()
            .with(fab_lang::surface::Natives.rows())
            .with(fab_bosl2::Bosl2.rows()),
    }
}

/// EVAL only: source → geometry tree. The whole span a native tier can affect, and the one where a
/// difference is legible.
fn time_eval(model: &Path, leg: &Leg, registry: &Registry) -> Option<Duration> {
    let config = fab_lang::Config {
        intrinsics: leg.intrinsics,
        // A DETERMINISTIC bound (eval STEPS, not wall time), so the same model fails at the same
        // point on every machine and every leg gets the identical allowance. Without it the
        // `interp` leg simply does not finish on the heavy BOSL2 models — which the models harness
        // already knows and solves with a subprocess watchdog.
        eval_budget: Some(BUDGET_STEPS),
        ..fab_lang::Config::default()
    };
    let t = Instant::now();
    let out = fab_scad::import::resolve_geometry_file_with(model, &libs(), registry, config);
    out.ok().map(|_| t.elapsed())
}

/// One model's committed WALL-clock row: `(model, fab_ms, oracle_ms)`, either side `None` when it
/// did not render inside that run's budget.
type WallRow = (String, Option<u64>, Option<u64>);

/// The committed perf baseline's model list, with its recorded fab/oracle WALL times.
///
/// Reading the artifact rather than re-walking `models/` keeps this corpus identical to the one
/// `perf/baseline.json` reports on, so an eval number here and a wall number there are about the
/// same program. The oracle times come from that file too: they are real measurements, taken on the
/// same machine class, and re-forking OpenSCAD 120 times to restate them would buy nothing.
fn baseline_models() -> Option<(Vec<PathBuf>, Vec<WallRow>)> {
    let raw = std::fs::read_to_string(manifest().join("perf/baseline.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut models = Vec::new();
    let mut wall = Vec::new();
    for row in json.get("rows")?.as_array()? {
        let name = row.get("model")?.as_str()?.to_string();
        let path = manifest().join(&name);
        if !path.exists() {
            continue; // models submodule not checked out, or the corpus moved
        }
        let ms = |k: &str| row.get(k).and_then(serde_json::Value::as_u64);
        wall.push((name, ms("fab_ms"), ms("oracle_ms")));
        models.push(path);
    }
    (!models.is_empty()).then_some((models, wall))
}

/// A median, because a mean over a corpus with one 8-second model reports that model.
fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// THE REPORT. Not an assertion about speed — AR.1 settled that speed was never the bar — but a
/// number chotchki asked to see before an irreversible delete. The only thing asserted is that the
/// transpiler does not make things WORSE than the tiers it replaces, which is the actual risk.
#[test]
#[ignore = "perf: renders the models corpus once per tier"]
fn tier_performance_on_real_models() {
    // ON A RESERVED STACK, like every model-rendering site in the tree — generated natives recurse
    // on the host stack and a real model builds a ladder deeper than a test thread's default holds
    // (AR.26.4.4). Learned here the same way it was learned there: by SIGABRT.
    std::thread::Builder::new()
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(sweep)
        .expect("spawn")
        .join()
        .expect("the sweep must not overflow");
}

fn sweep() {
    if !manifest().join("libs/BOSL2/std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    // THE CORPUS IS THE COMMITTED PERF BASELINE's, deliberately: it is already the curated
    // top-level set, it is in git, and using it means these eval numbers sit beside `perf/`'s
    // wall-clock fab-vs-oracle rows for the same models rather than beside a set nobody can compare.
    let Some((models, oracle)) = baseline_models() else {
        eprintln!("skipping: no perf/baseline.json (or no models checked out)");
        return;
    };
    // Registries built ONCE — indexing parses and fingerprints every reference, and paying that per
    // model would make this measure registry construction (the `build_count` trap, one level up).
    let registries: Vec<Registry> = LEGS.iter().map(registry_for).collect();

    let mut rows: Vec<(String, Vec<Option<Duration>>)> = Vec::new();
    for m in &models {
        let times: Vec<Option<Duration>> = LEGS
            .iter()
            .zip(&registries)
            .map(|(leg, reg)| time_eval(m, leg, reg))
            .collect();
        rows.push((
            m.strip_prefix(manifest())
                .unwrap_or(m)
                .display()
                .to_string(),
            times,
        ));
    }

    // A model that failed on ANY leg is not comparable — dropped, and counted.
    let complete: Vec<&(String, Vec<Option<Duration>>)> = rows
        .iter()
        .filter(|(_, t)| t.iter().all(Option::is_some))
        .collect();
    let dropped = rows.len() - complete.len();

    println!("\n=== AR.35 tier EVAL time, {} models ===", complete.len());
    println!("(dropped {dropped} that did not complete on at least one leg)");

    // WHICH LEG COULDN'T FINISH is a finding in its own right, and reporting only the all-legs
    // aggregate would bury it: the models dropped are exactly the ones where the compiled tiers help
    // most, so excluding them biases the ratio AGAINST the natives. Say so with numbers.
    for (i, leg) in LEGS.iter().enumerate() {
        let done = rows.iter().filter(|(_, t)| t[i].is_some()).count();
        println!(
            "  {:<11} completed {done}/{} within the budget",
            leg.name,
            rows.len()
        );
    }
    let mut totals: Vec<u128> = vec![0; LEGS.len()];
    for (_, times) in &complete {
        for (i, t) in times.iter().enumerate() {
            totals[i] += t.expect("complete").as_micros();
        }
    }
    let base_total = totals[0].max(1);
    for (i, leg) in LEGS.iter().enumerate() {
        // Per-model speedup vs `interp`, MEDIAN — a total is dominated by the heaviest model.
        let ratios: Vec<f64> = complete
            .iter()
            .map(|(_, t)| {
                let b = t[0].expect("complete").as_secs_f64().max(1e-9);
                b / t[i].expect("complete").as_secs_f64().max(1e-9)
            })
            .collect();
        #[allow(
            clippy::cast_precision_loss,
            reason = "microsecond totals; f64 is exact well past any corpus this size"
        )]
        let total_ratio = base_total as f64 / totals[i].max(1) as f64;
        println!(
            "  {:<11} total {:>8.1} ms   vs interp: total {:.2}x, median {:.2}x",
            leg.name,
            totals[i] as f64 / 1000.0,
            total_ratio,
            median(ratios),
        );
    }

    // THE WALL-CLOCK CONTEXT, from the committed baseline. Reported beside the eval numbers rather
    // than folded into them, because the two answer different questions and mixing them is how
    // "natives are 37% faster" becomes a claim about what a user waits for.
    let (mut fab_total, mut orc_total, mut both) = (0u64, 0u64, 0usize);
    for (_, f, o) in &oracle {
        if let (Some(f), Some(o)) = (f, o) {
            fab_total += f;
            orc_total += o;
            both += 1;
        }
    }
    if both > 0 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "millisecond totals over ~100 models"
        )]
        let ratio = orc_total as f64 / fab_total.max(1) as f64;
        println!(
            "\n=== WALL clock, full render (perf/baseline.json, {both} models) ===\n  \
             fab {fab_total} ms   openscad {orc_total} ms   ({ratio:.2}x)"
        );
        println!(
            "  NOTE: wall is eval + kernel. Geometry dominates it, so a tier's eval win is a small \
             fraction of this — which is the AR.1 finding restated, not a disappointment."
        );
    }

    // THE ONLY ASSERTION, and it is about the deletion rather than about speed: the transpiler must
    // not be slower than the hand tiers it replaces. A regression here is the one result that would
    // make AR.21 cost a user something.
    let idx = |n: &str| LEGS.iter().position(|l| l.name == n).expect("leg");
    let (hand, transpiler) = (totals[idx("hand")], totals[idx("transpiler")]);
    #[allow(clippy::cast_precision_loss, reason = "microsecond totals; see above")]
    let ratio = transpiler as f64 / hand.max(1) as f64;
    println!("\n  transpiler / hand = {ratio:.2}x  (< 1 is faster)");
    assert!(
        ratio < 1.25,
        "the transpiler is {ratio:.2}x the hand tiers' eval time — AR.21 would cost real time, \
         which is a reason to keep them rather than a number to accept"
    );
}
