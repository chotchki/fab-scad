//! L.3 — the `models/` tree harness: sweep chotchki's real parts to (1) PROFILE the scad-rs evaluator and
//! (2) COMPARE each rendered mesh against the OpenSCAD oracle. Two legs, each with the right tool:
//!
//!   • SWEEP (always): every top-level model runs in an isolated `models_worker` SUBPROCESS with a per-model
//!     watchdog. The interpreter is slow enough on heavy BOSL2 geometry that a large fraction of real models
//!     blow the budget — a subprocess is KILLED on timeout (reclaiming the core), where an in-process thread
//!     would leak and thrash. Yields the render distribution (ok / error / timeout), the slowest models, and
//!     the error worklist (unknown modules, unsupported constructs) — the every-run macro benchmark + the
//!     evaluator-gap list. This is the trend line the JIT/intrinsics tier (L.4) has to move.
//!
//!   • DEEP PROFILE (always): the N slowest models that DID render are re-run IN-PROCESS under a tracing layer
//!     that times each builtin (a leaf span → self-time) and module (inclusive) BY NAME. The hot builtins are
//!     the intrinsic worklist — the math/vector ops a JIT or hand-written intrinsic replaces. We profile the
//!     slow COMPLETERS (not the timeouts, whose spans never close) because they exercise the same hot paths.
//!
//!   • COMPARE (opt-in, `MODELS_COMPARE=1`): rendered models vs the oracle (boolean residual). An oracle
//!     render per model is minutes over the tree, so it's off by default; a divergence is DATA, not a failure.
//!
//! `#[ignore]` — a minutes-long sweep needing `libs/BOSL2` + `scad-lib/` on the path (and OpenSCAD for the
//! compare leg). Run on demand:
//!   cargo test -p fab-scad --test models_harness -- --ignored --nocapture models_profile_and_compare
//! Skips a leg cleanly when its dependency is absent (BOSL2 submodule / OpenSCAD binary).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration harness: unwrap/expect ARE the assertions"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fab_scad::openscad::find_bin;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// Per-model watchdog: real models eval in ≪1 s, so 10 s only ever catches a runaway — and the TIMEOUT list
/// is itself a headline finding (the models that most need the JIT tier). Bounds the sweep's wall-time too.
const SWEEP_BUDGET: Duration = Duration::from_secs(10);
/// How many of the slowest COMPLETERS to drill into with the per-builtin profiler.
const PROFILE_TOP_N: usize = 8;
/// Deep-profile budget — its targets already rendered under the (smaller) sweep budget, so this only guards
/// against a machine-load fluke; generous so it never falsely cuts a legit re-run.
const PROFILE_BUDGET: Duration = Duration::from_secs(45);

// ─────────────────────────────────────────────────────────────────────────────────────────────────────────
// The per-name profiling layer (deep-profile leg).
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

/// Per-(span, name) aggregate: instance count + summed wall-time (inclusive of children). A builtin span is
/// a leaf so its time is its own; a module span is inclusive of its subtree (a first-cut "time under this").
type Profile = BTreeMap<String, (u64, Duration)>;

/// The aggregating profile layer: on span exit, add the interval to the `span-name:field-value` bucket — so
/// `builtin:sqrt` / `module:cyl` accumulate PER FUNCTION, not just per span category.
#[derive(Clone, Default)]
struct Profiler {
    profile: Arc<Mutex<Profile>>,
}

/// Reads the `builtin`/`module`/`function` field off a span's attributes → the aggregation key.
struct KeyVisitor {
    span: &'static str,
    key: Option<String>,
}
impl Visit for KeyVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if matches!(field.name(), "builtin" | "module" | "function") {
            self.key = Some(format!("{}:{value}", self.span));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.key.is_none() && matches!(field.name(), "builtin" | "module" | "function") {
            self.key = Some(format!("{}:{value:?}", self.span));
        }
    }
}

/// Per-span timing + resolved key, stashed in the span's registry extensions.
struct Timing {
    key: String,
    last_enter: Option<Instant>,
}

impl<S> Layer<S> for Profiler
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let span_name = span.name();
        let mut visitor = KeyVisitor {
            span: span_name,
            key: None,
        };
        attrs.record(&mut visitor);
        let key = visitor.key.unwrap_or_else(|| span_name.to_string());
        span.extensions_mut().insert(Timing {
            key,
            last_enter: None,
        });
    }
    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(t) = span.extensions_mut().get_mut::<Timing>()
        {
            t.last_enter = Some(Instant::now());
        }
    }
    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        let Some(t) = ext.get_mut::<Timing>() else {
            return;
        };
        let Some(start) = t.last_enter.take() else {
            return;
        };
        let (dur, key) = (start.elapsed(), t.key.clone());
        drop(ext);
        let mut p = self.profile.lock().unwrap();
        let e = p.entry(key).or_insert((0, Duration::ZERO));
        e.0 += 1;
        e.1 += dur;
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────────────
// Model discovery.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The TOP-LEVEL models under `models/`, deterministically sorted. A `.scad` that some other file
/// `include`s/`use`s is a LIBRARY or DATA file (measurements.scad, monitor.scad, the 712k-element
/// `height_map_*`), NOT a model: rendering it standalone is meaningless and drags the pathological data blobs
/// in. So the set is every non-`out/` `.scad` whose basename is NOT an include/use target anywhere, minus
/// `height_map_*` by name (a top-level `3d_shape` still `include`s one — the watchdog bounds that).
fn model_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_scad(&manifest().join("models"), &mut out);
    // Drop build outputs (`out/`) and abandoned experiments (`unused/`) — both unambiguous dead-code markers,
    // so they don't count against the correctness baseline. Other dead models (`_test`/`_slice`/`second_approach`)
    // are a judgment call left to a model-cleanup pass.
    out.retain(|p| {
        !p.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("out" | "unused")))
    });
    let included = included_basenames(&out);
    out.retain(|p| {
        let base = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        !included.contains(base) && !base.starts_with("height_map_")
    });
    out.sort();
    out
}

fn collect_scad(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_scad(&path, out);
        } else if path.extension().is_some_and(|e| e == "scad") {
            out.push(path);
        }
    }
}

/// Basenames any file in `files` pulls via `include <…>` / `use <…>` (path stripped to the leaf).
fn included_basenames(files: &[PathBuf]) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    for f in files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        for line in src.lines() {
            let t = line.trim_start();
            if !(t.starts_with("include") || t.starts_with("use")) {
                continue;
            }
            if let Some(lt) = t.find('<')
                && let Some(rel) = t[lt + 1..].find('>').map(|gt| &t[lt + 1..lt + 1 + gt])
            {
                set.insert(rel.rsplit('/').next().unwrap_or(rel).to_string());
            }
        }
    }
    set
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────────────
// The isolated sweep.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One model's render outcome under the isolated sweep.
enum Outcome {
    /// Rendered to a geometry tree in this many ms.
    Rendered(u128),
    /// The evaluator rejected it (unknown module, assert, parse/load error) — the first error line.
    Failed(String),
    /// Blew the watchdog budget — killed. The intrinsics tier's prime targets.
    Timeout,
}

/// Run ONE model in a `models_worker` subprocess, killing it if it blows `budget`. A reader thread drains the
/// worker's single stdout line; `recv_timeout` races it against the watchdog. Killing the child (not leaking a
/// thread) is the whole point — the interpreter is slow enough that a big fraction of the tree times out.
fn run_worker(worker: &str, model: &Path, libs: &[PathBuf], budget: Duration) -> Outcome {
    let mut cmd = Command::new(worker);
    cmd.arg(model);
    for l in libs {
        cmd.arg(l);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("spawn worker: {e}")),
    };
    let stdout = child.stdout.take().expect("worker stdout piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    match rx.recv_timeout(budget) {
        Ok(line) => {
            let _ = child.wait();
            parse_worker_line(&line)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Outcome::Timeout
        }
    }
}

/// Boolean-residual-compare ONE rendered model against the oracle, on a [`fab_scad::EVAL_STACK`] thread with a
/// `budget`. The big stack matters: `diff_files` re-runs scad-rs IN-PROCESS, and deep eval-assembly host
/// recursion overflows the main test thread's default (an early version SIGABRT'd the whole run here) — the
/// worker + profile legs eval on the same reserve. The oracle render inside is bounded by its own timeout; this
/// `budget` guards the scad-rs side + a slow oracle. A leaked thread on timeout dies at process exit.
fn compare_one(model: PathBuf, libs: Vec<PathBuf>, budget: Duration) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("compare".into())
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(move || {
            let _ = tx.send(fab_scad::differ::diff_files(&model, &libs));
        })
        .expect("spawn compare thread");
    match rx.recv_timeout(budget) {
        Ok(r) => r,
        Err(_) => Err(format!("compare exceeded {}s", budget.as_secs())),
    }
}

/// `OK\t<ms>` / `ERR\t<ms>\t<detail>`; an empty line = the worker died without printing (stack overflow).
fn parse_worker_line(line: &str) -> Outcome {
    let mut f = line.trim_end().split('\t');
    match f.next() {
        Some("OK") => Outcome::Rendered(f.next().and_then(|s| s.parse().ok()).unwrap_or(0)),
        Some("ERR") => {
            let _ms = f.next();
            Outcome::Failed(f.next().unwrap_or("(no detail)").to_string())
        }
        _ => Outcome::Failed("crashed (no output — likely stack overflow)".to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────────────
// BU.6 — the PERSISTENT perf artifact: every sweep writes a run JSON, diffs against the committed
// baseline, and can freeze itself AS the new baseline. The trend line lives in git, not in scrollback.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One model's timing row. `ms` is `Some` only when that side RENDERED inside the budget; `kind`
/// keeps the terminal state either way (solid/empty/rejected/TIMEOUT) so status TRANSITIONS diff too.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PerfRow {
    model: String,
    fab_ms: Option<u64>,
    fab_kind: String,
    oracle_ms: Option<u64>,
    oracle_kind: String,
}

/// A whole sweep, as written to `perf/runs/run-<epoch>.json` (local, gitignored) and
/// `perf/baseline.json` (committed — the anchor every later run reports against).
#[derive(serde::Serialize, serde::Deserialize)]
struct PerfRun {
    schema: u32,
    captured_epoch_s: u64,
    budget_s: u64,
    rows: Vec<PerfRow>,
}

/// The common-set aggregates: (both-rendered count, fab total ms, oracle total ms, median o/f ratio).
fn perf_aggregates(rows: &[PerfRow]) -> (usize, u64, u64, f64) {
    let mut ratios: Vec<f64> = Vec::new();
    let (mut fab_total, mut orc_total, mut both) = (0u64, 0u64, 0usize);
    for r in rows {
        if let (Some(f), Some(o)) = (r.fab_ms, r.oracle_ms) {
            both += 1;
            fab_total += f;
            orc_total += o;
            ratios.push(o as f64 / (f as f64).max(1.0));
        }
    }
    ratios.sort_by(f64::total_cmp);
    let median = if ratios.is_empty() {
        f64::NAN
    } else {
        ratios[ratios.len() / 2]
    };
    (both, fab_total, orc_total, median)
}

/// The delta report. Status TRANSITIONS always print; a fab-side timing move prints past the noise
/// gate (|Δ| ≥ 100ms AND ≥ 20%). Per-model ORACLE timing moves stay out on purpose — OpenSCAD is
/// run-to-run nondeterministic under TBB, so only its aggregate is worth reading.
fn print_perf_delta(base: &PerfRun, run: &PerfRun) {
    let by_model: BTreeMap<&str, &PerfRow> =
        base.rows.iter().map(|r| (r.model.as_str(), r)).collect();
    eprintln!(
        "\n=== delta vs baseline (epoch {}) ===",
        base.captured_epoch_s
    );
    let mut moves = 0usize;
    for now in &run.rows {
        let Some(then) = by_model.get(now.model.as_str()) else {
            eprintln!("  NEW  {}", now.model);
            moves += 1;
            continue;
        };
        if then.fab_kind != now.fab_kind || then.oracle_kind != now.oracle_kind {
            eprintln!(
                "  {}: fab {} → {} | oracle {} → {}",
                now.model, then.fab_kind, now.fab_kind, then.oracle_kind, now.oracle_kind
            );
            moves += 1;
            continue;
        }
        if let (Some(a), Some(b)) = (then.fab_ms, now.fab_ms) {
            let d = b as i64 - a as i64;
            if d.unsigned_abs() >= 100 && d.unsigned_abs() as f64 >= 0.2 * a as f64 {
                eprintln!(
                    "  {}: fab {a}ms → {b}ms ({:+}%)",
                    now.model,
                    100 * d / (a.max(1) as i64)
                );
                moves += 1;
            }
        }
    }
    let now_names: BTreeSet<&str> = run.rows.iter().map(|r| r.model.as_str()).collect();
    for r in &base.rows {
        if !now_names.contains(r.model.as_str()) {
            eprintln!("  GONE {}", r.model);
            moves += 1;
        }
    }
    if moves == 0 {
        eprintln!("  (no per-model moves past the noise gate)");
    }
    let (b_both, b_fab, b_orc, b_med) = perf_aggregates(&base.rows);
    let (n_both, n_fab, n_orc, n_med) = perf_aggregates(&run.rows);
    eprintln!(
        "  aggregate: both {b_both}→{n_both} | fab wall {:.1}s→{:.1}s | oracle wall {:.1}s→{:.1}s | median o/f {b_med:.2}×→{n_med:.2}×",
        b_fab as f64 / 1e3,
        n_fab as f64 / 1e3,
        b_orc as f64 / 1e3,
        n_orc as f64 / 1e3,
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────────────
// The harness.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

/// K.1.2 / BU.6 — full-PIPELINE wall time, fab-scad vs OpenSCAD, over the real `models/` tree. Each
/// side runs its complete file→Solid path via the differ's own `Driver::eval_file` (ours: resolve +
/// evaluate + kernel build; oracle: spawn OpenSCAD, render, re-import) — the same code the
/// correctness differential trusts, now timed. Serial on purpose (no contention noise); one timed
/// pass per side per model (this is a shape-of-the-landscape sweep, not a microbenchmark). A side
/// that errors or blows the budget is listed but excluded from ratios; an in-process timeout leaks
/// its eval thread until exit (the compare_one precedent) — acceptable for a perf lane.
///
/// PERSISTENT since BU.6: every run writes `perf/runs/run-<epoch>.json` (gitignored) and prints a
/// delta report against the committed `perf/baseline.json` (status transitions + fab-side moves past
/// the 100ms/20% noise gate). `FAB_PERF_WRITE_BASELINE=1` freezes the run as the new baseline —
/// commit that only on a QUIET machine and a deliberate anchor point (post-perf-phase, not mid-fix).
///
///   cargo test --release -p fab-scad --test models_harness -- --ignored --nocapture models_perf
#[test]
#[ignore = "minutes-long timed models/ sweep; run explicitly with --ignored (use --release)"]
fn models_perf_vs_openscad() {
    use fab_scad::differ::{Outcome, drivers};

    const BUDGET: Duration = Duration::from_secs(30);

    let manifest = manifest();
    if !manifest.join("libs/BOSL2/std.scad").exists() {
        eprintln!("note: libs/BOSL2 not checked out — perf sweep skipped");
        return;
    }
    let drivers = drivers();
    if drivers.len() < 2 {
        eprintln!("note: OpenSCAD not found — perf sweep needs both engines");
        return;
    }
    let libs: Vec<PathBuf> = vec![manifest.join("libs"), manifest.join("scad-lib")];
    let files = model_files();
    eprintln!(
        "=== K.1.2 perf: {} models × {} drivers, serial, {}s budget/side ===",
        files.len(),
        drivers.len(),
        BUDGET.as_secs()
    );

    let mut rows: Vec<PerfRow> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&manifest)
            .unwrap_or(path)
            .display()
            .to_string();
        let mut cells: Vec<(Option<u64>, &'static str)> = Vec::new();
        for d in &drivers {
            let model = path.clone();
            let libs = libs.clone();
            let name: String = d.name().to_string();
            let (tx, rx) = mpsc::channel();
            let start = Instant::now();
            thread::Builder::new()
                .name("perf-eval".to_string())
                .stack_size(fab_scad::EVAL_STACK)
                .spawn(move || {
                    // `Solid` is !Send — ship only the outcome KIND; the wall time is measured on
                    // the receiving side.
                    let kind = match fab_scad::differ::drivers()
                        .into_iter()
                        .find(|dd| dd.name() == name.as_str())
                        .expect("driver present")
                        .eval_file(&model, &libs)
                    {
                        Outcome::Solid(_) => "solid",
                        Outcome::Empty => "empty",
                        Outcome::Rejected => "rejected",
                    };
                    let _ = tx.send(kind);
                })
                .expect("spawn perf thread");
            let cell = match rx.recv_timeout(BUDGET) {
                Ok("rejected") => (None, "rejected"),
                Ok(k) => (Some(start.elapsed().as_millis() as u64), k),
                Err(_) => (None, "TIMEOUT"),
            };
            cells.push(cell);
        }
        let show = |c: &(Option<u64>, &'static str)| match c.0 {
            Some(ms) => format!("{ms}ms"),
            None => c.1.to_string(),
        };
        eprintln!(
            "  {rel}: fab {} | oracle {}",
            show(&cells[0]),
            show(&cells[1])
        );
        rows.push(PerfRow {
            model: rel,
            fab_ms: cells[0].0,
            fab_kind: cells[0].1.to_string(),
            oracle_ms: cells[1].0,
            oracle_kind: cells[1].1.to_string(),
        });
    }

    // Aggregate over models BOTH sides rendered.
    let (both, fab_total, orc_total, median) = perf_aggregates(&rows);
    let fab_only_fail = rows
        .iter()
        .filter(|r| r.fab_ms.is_none() && r.oracle_ms.is_some())
        .count();
    let orc_only_fail = rows
        .iter()
        .filter(|r| r.fab_ms.is_some() && r.oracle_ms.is_none())
        .count();
    eprintln!("\n=== K.1.2 summary ===");
    eprintln!("both rendered: {both}/{} models", rows.len());
    eprintln!("fab-scad failed/timed-out where OpenSCAD rendered: {fab_only_fail}");
    eprintln!("OpenSCAD failed/timed-out where fab-scad rendered: {orc_only_fail}");
    if both > 0 {
        eprintln!(
            "wall totals on the common set: fab-scad {:.1}s vs OpenSCAD {:.1}s (ratio {:.2}× — >1 means fab-scad faster)",
            fab_total as f64 / 1e3,
            orc_total as f64 / 1e3,
            orc_total as f64 / fab_total as f64
        );
        eprintln!("median per-model oracle/fab ratio: {median:.2}×");
    }

    // ── BU.6: persist the run, diff against the committed baseline, optionally freeze ────────────
    let run = PerfRun {
        schema: 1,
        captured_epoch_s: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        budget_s: BUDGET.as_secs(),
        rows,
    };
    let perf_dir = manifest.join("perf");
    let runs_dir = perf_dir.join("runs");
    std::fs::create_dir_all(&runs_dir).unwrap();
    let run_path = runs_dir.join(format!("run-{}.json", run.captured_epoch_s));
    std::fs::write(&run_path, serde_json::to_string_pretty(&run).unwrap()).unwrap();
    eprintln!("\nrun written: {}", run_path.display());

    let baseline_path = perf_dir.join("baseline.json");
    match std::fs::read_to_string(&baseline_path) {
        Ok(text) => {
            let base: serde_json::Result<PerfRun> = serde_json::from_str(&text);
            match base {
                Ok(base) => print_perf_delta(&base, &run),
                Err(e) => eprintln!("perf/baseline.json unreadable ({e}) — refreeze it"),
            }
        }
        Err(_) => {
            eprintln!("no perf/baseline.json — rerun with FAB_PERF_WRITE_BASELINE=1 to freeze one")
        }
    }
    if std::env::var_os("FAB_PERF_WRITE_BASELINE").is_some_and(|v| v == "1") {
        std::fs::write(&baseline_path, serde_json::to_string_pretty(&run).unwrap()).unwrap();
        eprintln!("baseline frozen: {}", baseline_path.display());
    }
}

#[test]
#[ignore = "minutes-long models/ sweep; run explicitly with --ignored"]
fn models_profile_and_compare() {
    let manifest = manifest();
    if !manifest.join("libs/BOSL2/std.scad").exists() {
        eprintln!("note: libs/BOSL2 not checked out — models harness skipped");
        return;
    }
    // Library search path: BOSL2 (`include <BOSL2/std.scad>`) + scad-lib (`include <connectors.scad>`).
    // Same-dir includes resolve against each file's own parent (the worker's / oracle's base_dir).
    let libs: Vec<PathBuf> = vec![manifest.join("libs"), manifest.join("scad-lib")];
    let worker = env!("CARGO_BIN_EXE_models_worker");
    let files = model_files();

    // ── SWEEP (isolated subprocess per model, bounded parallel pool) ─────────────────────────────────────────
    // Each model is its own kill-safe subprocess, so the sweep parallelizes freely (no shared eval state) —
    // 12-wide on a 16-core box turns ~10 min of serial timeout-waiting into ~1-2 min. Each worker pulls the
    // next index off an atomic counter; one `eprintln!` per model keeps the interleaved progress lines whole.
    const CONCURRENCY: usize = 12;
    eprintln!(
        "=== sweeping {} top-level models ({CONCURRENCY}-wide, {}s budget each) ===",
        files.len(),
        SWEEP_BUDGET.as_secs()
    );
    let slots: Vec<Mutex<Option<Outcome>>> = files.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = files.len();
    thread::scope(|s| {
        for _ in 0..CONCURRENCY {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() {
                        break;
                    }
                    let path = &files[i];
                    let rel = path
                        .strip_prefix(&manifest)
                        .unwrap_or(path)
                        .display()
                        .to_string();
                    let outcome = run_worker(worker, path, &libs, SWEEP_BUDGET);
                    let tag = match &outcome {
                        Outcome::Rendered(ms) => format!("{ms} ms"),
                        Outcome::Timeout => "TIMEOUT".to_string(),
                        Outcome::Failed(d) => format!("ERR: {d}"),
                    };
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    eprintln!("  [{n:>3}/{total}] {rel} → {tag}");
                    *slots[i].lock().unwrap() = Some(outcome);
                }
            });
        }
    });
    let results: Vec<(String, Outcome)> = files
        .iter()
        .zip(slots)
        .map(|(path, slot)| {
            let rel = path
                .strip_prefix(&manifest)
                .unwrap_or(path)
                .display()
                .to_string();
            (rel, slot.into_inner().unwrap().expect("every slot filled"))
        })
        .collect();

    let rendered = results
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Rendered(_)))
        .count();
    let timed_out = results
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Timeout))
        .count();
    let failed = results
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Failed(_)))
        .count();
    eprintln!(
        "\n=== render distribution: {rendered} ok / {timed_out} timeout / {failed} error  (of {}) ===",
        results.len()
    );

    // Timing histogram of the models that rendered — the shape of "how slow is the interpreter".
    let mut buckets: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, o) in &results {
        if let Outcome::Rendered(ms) = o {
            let b = match ms {
                0..=99 => "<100ms",
                100..=499 => "100-500ms",
                500..=999 => "0.5-1s",
                1000..=2999 => "1-3s",
                _ => "3-10s",
            };
            *buckets.entry(b).or_default() += 1;
        }
    }
    eprintln!("--- rendered-model timing histogram ---");
    for b in ["<100ms", "100-500ms", "0.5-1s", "1-3s", "3-10s"] {
        eprintln!("  {b:<12} {}", buckets.get(b).copied().unwrap_or(0));
    }

    // Slowest completers (the deep-profile targets) + the timeout roster (the intrinsics targets we can't
    // even measure per-builtin yet).
    let mut completers: Vec<(&String, u128)> = results
        .iter()
        .filter_map(|(rel, o)| {
            if let Outcome::Rendered(ms) = o {
                Some((rel, *ms))
            } else {
                None
            }
        })
        .collect();
    completers.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
    eprintln!("\n=== slowest RENDERED models (deep-profile targets) ===");
    for (rel, ms) in completers.iter().take(12) {
        eprintln!("  {ms:>7} ms  {rel}");
    }
    eprintln!(
        "\n=== TIMEOUT models (>{}s — the JIT/intrinsics prime targets) ===",
        SWEEP_BUDGET.as_secs()
    );
    for (rel, o) in &results {
        if matches!(o, Outcome::Timeout) {
            eprintln!("  {rel}");
        }
    }

    // Error worklist — clustered by first line so recurring evaluator gaps (unknown modules, unsupported
    // constructs) surface as the "what's left" list.
    let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (rel, o) in &results {
        if let Outcome::Failed(d) = o {
            by_reason.entry(d.as_str()).or_default().push(rel);
        }
    }
    if !by_reason.is_empty() {
        eprintln!("\n=== error worklist (evaluator gaps) — {failed} models ===");
        for (reason, models) in &by_reason {
            eprintln!("  [{}×] {reason}", models.len());
            for m in models {
                eprintln!("        {m}");
            }
        }
    }

    // ── DEEP PROFILE (in-process, per-builtin) ─────────────────────────────────────────────────────────────
    // Re-run the slowest COMPLETERS under the tracing layer to see WHICH builtins/modules the time goes to.
    let targets: Vec<PathBuf> = completers
        .iter()
        .take(PROFILE_TOP_N)
        .map(|(rel, _)| manifest.join(rel))
        .collect();
    let profiler = Profiler::default();
    let profile = profiler.profile.clone();
    // GLOBAL subscriber: each target profiles on its own big-stack thread; a global subscriber is the one every
    // thread's spans reach. Set ONCE, before any span we want to count fires (the sweep's spans were in child
    // processes — invisible here — and the compare leg runs strictly after we snapshot the profile below).
    tracing::subscriber::set_global_default(Registry::default().with(profiler))
        .expect("set global tracing subscriber");
    eprintln!(
        "\n=== deep-profiling the {} slowest completers (per-builtin) ===",
        targets.len()
    );
    for target in &targets {
        let rel = target
            .strip_prefix(&manifest)
            .unwrap_or(target)
            .display()
            .to_string();
        eprintln!("  profiling {rel}");
        let (tx, rx) = mpsc::channel();
        let t = target.clone();
        let libs_t = libs.clone();
        thread::Builder::new()
            .name("profile-eval".into())
            .stack_size(fab_scad::EVAL_STACK)
            .spawn(move || {
                let _ = fab_scad::import::resolve_geometry_file(
                    &t,
                    &libs_t,
                    fab_lang::Config::from_env(),
                );
                let _ = tx.send(());
            })
            .expect("spawn profile thread");
        let _ = rx.recv_timeout(PROFILE_BUDGET); // completers rendered under the sweep budget; leak on the off chance
    }

    let profile = profile.lock().unwrap();
    let mut ranked: Vec<(&String, u64, Duration)> =
        profile.iter().map(|(k, &(n, d))| (k, n, d)).collect();
    ranked.sort_by_key(|&(_, _, d)| std::cmp::Reverse(d));
    // BUILTINS are LEAF spans → SELF-time: the real intrinsic worklist (the math/vector ops a JIT replaces).
    eprintln!(
        "\n=== hot BUILTINS (leaf self-time — the intrinsic worklist) — total ms × calls ==="
    );
    for (key, n, d) in ranked
        .iter()
        .filter(|(k, ..)| k.starts_with("builtin:"))
        .take(25)
    {
        eprintln!(
            "  {:<26} {:>8} ms  ×{n}",
            key.trim_start_matches("builtin:"),
            d.as_millis()
        );
    }
    // MODULES are INCLUSIVE of their subtree — cost CONCENTRATION, not self-time.
    eprintln!(
        "\n=== hot MODULES (inclusive subtree time — cost concentration) — total ms × calls ==="
    );
    for (key, n, d) in ranked
        .iter()
        .filter(|(k, ..)| k.starts_with("module:"))
        .take(15)
    {
        eprintln!(
            "  {:<26} {:>8} ms  ×{n}",
            key.trim_start_matches("module:"),
            d.as_millis()
        );
    }
    drop(profile);

    // ── COMPARE (opt-in) ───────────────────────────────────────────────────────────────────────────────────
    if std::env::var_os("MODELS_COMPARE").is_none() {
        eprintln!(
            "\n(compare leg skipped — set MODELS_COMPARE=1 to run rendered models vs the oracle)"
        );
    } else if let Some(bin) = find_bin() {
        eprintln!(
            "\n=== compare vs oracle ({}) — boolean residual per rendered model ===",
            bin.display()
        );
        let (mut compared, mut diverged) = (0, 0);
        for (rel, o) in &results {
            if !matches!(o, Outcome::Rendered(_)) {
                continue;
            }
            match compare_one(manifest.join(rel), libs.clone(), Duration::from_secs(60)) {
                Ok(()) => compared += 1,
                Err(why) => {
                    diverged += 1;
                    eprintln!(
                        "  DIVERGE {rel}: {}",
                        why.lines().next().unwrap_or_default()
                    );
                }
            }
        }
        eprintln!(
            "compared {compared} models; {diverged} diverged (residual over gate or oracle error)"
        );
    } else {
        eprintln!("\n(compare leg requested but OpenSCAD binary not found — skipped)");
    }

    // The gate: SOME models must render (a total wipe-out is a real regression). Not a pass ratchet yet — the
    // distribution + profile + divergence report ARE the deliverable, and the numbers move as the tiers land.
    assert!(rendered > 0, "no models rendered — the sweep is broken");
}

/// The default targets for [`models_profile_targets`]: the eval-bound tail BU.7 named — models whose wall is
/// ≥85% EVALUATOR (the kernel + caches already did their part), i.e. exactly what the intrinsics/JIT tier
/// has to move. window_air_cover is the headline: 36s of its 38s wall is eval.
const PROFILE_TARGET_DEFAULTS: &[&str] = &[
    "models/window_air_cover/window_air_cover.scad",
    "models/shoe_holder/shoe_holder.scad",
    "models/webcam_holder/webcam_holder.scad",
    "models/pill_holder/pill_holder.scad",
];

/// O.4 — deep-profile SPECIFIC models by name, however slow they are. The `models_profile_and_compare` leg
/// only drills into the top completers of a 10s sweep, so a 38s eval-bound model (the intrinsics tier's whole
/// point) is structurally invisible to it. This leg takes its targets from `FAB_PROFILE_TARGETS`
/// (comma-separated, manifest-relative) or defaults to the BU.7 eval-bound tail, runs each IN-PROCESS under
/// the tracing layer (per-builtin self-time, per-module inclusive), and prints PER-MODEL tables — plus, when
/// `FAB_PROFILE_FNS=1` is set, the fnprofile report (per-user-fn SELF time — the worklist) prints from inside
/// each eval. Wall times here include probe overhead — attribution shares are the signal, not the totals; the
/// perf harness owns the honest walls. Run:
///   FAB_PROFILE_FNS=1 cargo test --release -p fab-scad --test models_harness -- --ignored --nocapture models_profile_targets
#[test]
#[ignore = "targeted deep-profile; run explicitly with --ignored (see doc comment)"]
fn models_profile_targets() {
    let manifest = manifest();
    if !manifest.join("libs/BOSL2/std.scad").exists() {
        eprintln!("note: libs/BOSL2 not checked out — targeted profile skipped");
        return;
    }
    let libs: Vec<PathBuf> = vec![manifest.join("libs"), manifest.join("scad-lib")];
    let targets: Vec<PathBuf> = match std::env::var("FAB_PROFILE_TARGETS") {
        Ok(list) => list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|rel| manifest.join(rel))
            .collect(),
        Err(_) => PROFILE_TARGET_DEFAULTS
            .iter()
            .map(|rel| manifest.join(rel))
            .collect(),
    };
    // Generous per-model budget: these targets are CHOSEN for being slow, and the probes multiply the wall.
    // On expiry the eval thread is leaked (same tradeoff as the deep-profile leg) and the partial attribution
    // still prints — partial shares of a hung model are exactly the data we came for.
    let budget = Duration::from_secs(
        std::env::var("FAB_PROFILE_BUDGET_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    );

    let profiler = Profiler::default();
    let profile = profiler.profile.clone();
    tracing::subscriber::set_global_default(Registry::default().with(profiler))
        .expect("set global tracing subscriber");
    if std::env::var_os("FAB_PROFILE_FNS").is_none() {
        eprintln!("(FAB_PROFILE_FNS not set — per-user-fn self-time tables won't print)");
    }

    for target in &targets {
        let rel = target
            .strip_prefix(&manifest)
            .unwrap_or(target)
            .display()
            .to_string();
        eprintln!(
            "\n=== deep-profiling {rel} (budget {}s) ===",
            budget.as_secs()
        );
        let t0 = Instant::now();
        let (tx, rx) = mpsc::channel();
        let t = target.clone();
        let libs_t = libs.clone();
        thread::Builder::new()
            .name("profile-eval".into())
            .stack_size(fab_scad::EVAL_STACK)
            .spawn(move || {
                let res = fab_scad::import::resolve_geometry_file(
                    &t,
                    &libs_t,
                    fab_lang::Config::from_env(),
                );
                let _ = tx.send(res.map(|_| ()).map_err(|e| e.to_string()));
            })
            .expect("spawn profile thread");
        match rx.recv_timeout(budget) {
            Ok(Ok(())) => eprintln!(
                "  rendered in {:.2}s (probe overhead included)",
                t0.elapsed().as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "  ERR after {:.2}s: {}",
                t0.elapsed().as_secs_f64(),
                e.lines().next().unwrap_or_default()
            ),
            Err(_) => eprintln!(
                "  TIMED OUT at {}s — thread leaked, attribution below is PARTIAL",
                budget.as_secs()
            ),
        }

        // Snapshot + clear so each model's tables are its own (the subscriber is global and reused).
        let snap: Profile = std::mem::take(&mut *profile.lock().unwrap());
        let mut ranked: Vec<(&String, u64, Duration)> =
            snap.iter().map(|(k, &(n, d))| (k, n, d)).collect();
        ranked.sort_by_key(|&(_, _, d)| std::cmp::Reverse(d));
        eprintln!("  --- hot BUILTINS (leaf self-time) — total ms × calls ---");
        for (key, n, d) in ranked
            .iter()
            .filter(|(k, ..)| k.starts_with("builtin:"))
            .take(20)
        {
            eprintln!(
                "  {:<26} {:>8} ms  ×{n}",
                key.trim_start_matches("builtin:"),
                d.as_millis()
            );
        }
        eprintln!("  --- hot MODULES (inclusive subtree time) — total ms × calls ---");
        for (key, n, d) in ranked
            .iter()
            .filter(|(k, ..)| k.starts_with("module:"))
            .take(15)
        {
            eprintln!(
                "  {:<26} {:>8} ms  ×{n}",
                key.trim_start_matches("module:"),
                d.as_millis()
            );
        }
    }
}
