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

use std::collections::BTreeMap;
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
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Registry;

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
        let mut visitor = KeyVisitor { span: span_name, key: None };
        attrs.record(&mut visitor);
        let key = visitor.key.unwrap_or_else(|| span_name.to_string());
        span.extensions_mut().insert(Timing { key, last_enter: None });
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
        let Some(t) = ext.get_mut::<Timing>() else { return };
        let Some(start) = t.last_enter.take() else { return };
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
    out.retain(|p| !p.components().any(|c| matches!(c.as_os_str().to_str(), Some("out" | "unused"))));
    let included = included_basenames(&out);
    out.retain(|p| {
        let base = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        !included.contains(base) && !base.starts_with("height_map_")
    });
    out.sort();
    out
}

fn collect_scad(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
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
        let Ok(src) = std::fs::read_to_string(f) else { continue };
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

/// Boolean-residual-compare ONE rendered model against the oracle, on a 1 GiB thread with a `budget`. The
/// big stack matters: `diff_files` re-runs scad-rs IN-PROCESS, and the main test thread's default 8 MiB
/// overflows dropping a deep tree (an early version SIGABRT'd the whole run here) — the worker + profile legs
/// already eval on 1 GiB for the same reason. The oracle render inside is bounded by its own timeout; this
/// `budget` guards the scad-rs side + a slow oracle. A leaked thread on timeout dies at process exit.
fn compare_one(model: PathBuf, libs: Vec<PathBuf>, budget: Duration) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("compare".into())
        .stack_size(1 << 30)
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
// The harness.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────────

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
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= files.len() {
                    break;
                }
                let path = &files[i];
                let rel = path.strip_prefix(&manifest).unwrap_or(path).display().to_string();
                let outcome = run_worker(worker, path, &libs, SWEEP_BUDGET);
                let tag = match &outcome {
                    Outcome::Rendered(ms) => format!("{ms} ms"),
                    Outcome::Timeout => "TIMEOUT".to_string(),
                    Outcome::Failed(d) => format!("ERR: {d}"),
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!("  [{n:>3}/{total}] {rel} → {tag}");
                *slots[i].lock().unwrap() = Some(outcome);
            });
        }
    });
    let results: Vec<(String, Outcome)> = files
        .iter()
        .zip(slots)
        .map(|(path, slot)| {
            let rel = path.strip_prefix(&manifest).unwrap_or(path).display().to_string();
            (rel, slot.into_inner().unwrap().expect("every slot filled"))
        })
        .collect();

    let rendered = results.iter().filter(|(_, o)| matches!(o, Outcome::Rendered(_))).count();
    let timed_out = results.iter().filter(|(_, o)| matches!(o, Outcome::Timeout)).count();
    let failed = results.iter().filter(|(_, o)| matches!(o, Outcome::Failed(_))).count();
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
        .filter_map(|(rel, o)| if let Outcome::Rendered(ms) = o { Some((rel, *ms)) } else { None })
        .collect();
    completers.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
    eprintln!("\n=== slowest RENDERED models (deep-profile targets) ===");
    for (rel, ms) in completers.iter().take(12) {
        eprintln!("  {ms:>7} ms  {rel}");
    }
    eprintln!("\n=== TIMEOUT models (>{}s — the JIT/intrinsics prime targets) ===", SWEEP_BUDGET.as_secs());
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
    let targets: Vec<PathBuf> = completers.iter().take(PROFILE_TOP_N).map(|(rel, _)| manifest.join(rel)).collect();
    let profiler = Profiler::default();
    let profile = profiler.profile.clone();
    // GLOBAL subscriber: each target profiles on its own big-stack thread; a global subscriber is the one every
    // thread's spans reach. Set ONCE, before any span we want to count fires (the sweep's spans were in child
    // processes — invisible here — and the compare leg runs strictly after we snapshot the profile below).
    tracing::subscriber::set_global_default(Registry::default().with(profiler))
        .expect("set global tracing subscriber");
    eprintln!("\n=== deep-profiling the {} slowest completers (per-builtin) ===", targets.len());
    for target in &targets {
        let rel = target.strip_prefix(&manifest).unwrap_or(target).display().to_string();
        eprintln!("  profiling {rel}");
        let (tx, rx) = mpsc::channel();
        let t = target.clone();
        let libs_t = libs.clone();
        thread::Builder::new()
            .name("profile-eval".into())
            .stack_size(1 << 30)
            .spawn(move || {
                let _ = fab_scad::import::resolve_geometry_file(&t, &libs_t);
                let _ = tx.send(());
            })
            .expect("spawn profile thread");
        let _ = rx.recv_timeout(PROFILE_BUDGET); // completers rendered under the sweep budget; leak on the off chance
    }

    let profile = profile.lock().unwrap();
    let mut ranked: Vec<(&String, u64, Duration)> = profile.iter().map(|(k, &(n, d))| (k, n, d)).collect();
    ranked.sort_by_key(|&(_, _, d)| std::cmp::Reverse(d));
    // BUILTINS are LEAF spans → SELF-time: the real intrinsic worklist (the math/vector ops a JIT replaces).
    eprintln!("\n=== hot BUILTINS (leaf self-time — the intrinsic worklist) — total ms × calls ===");
    for (key, n, d) in ranked.iter().filter(|(k, ..)| k.starts_with("builtin:")).take(25) {
        eprintln!("  {:<26} {:>8} ms  ×{n}", key.trim_start_matches("builtin:"), d.as_millis());
    }
    // MODULES are INCLUSIVE of their subtree — cost CONCENTRATION, not self-time.
    eprintln!("\n=== hot MODULES (inclusive subtree time — cost concentration) — total ms × calls ===");
    for (key, n, d) in ranked.iter().filter(|(k, ..)| k.starts_with("module:")).take(15) {
        eprintln!("  {:<26} {:>8} ms  ×{n}", key.trim_start_matches("module:"), d.as_millis());
    }
    drop(profile);

    // ── COMPARE (opt-in) ───────────────────────────────────────────────────────────────────────────────────
    if std::env::var_os("MODELS_COMPARE").is_none() {
        eprintln!("\n(compare leg skipped — set MODELS_COMPARE=1 to run rendered models vs the oracle)");
    } else if let Some(bin) = find_bin() {
        eprintln!("\n=== compare vs oracle ({}) — boolean residual per rendered model ===", bin.display());
        let (mut compared, mut diverged) = (0, 0);
        for (rel, o) in &results {
            if !matches!(o, Outcome::Rendered(_)) {
                continue;
            }
            match compare_one(manifest.join(rel), libs.clone(), Duration::from_secs(60)) {
                Ok(()) => compared += 1,
                Err(why) => {
                    diverged += 1;
                    eprintln!("  DIVERGE {rel}: {}", why.lines().next().unwrap_or_default());
                }
            }
        }
        eprintln!("compared {compared} models; {diverged} diverged (residual over gate or oracle error)");
    } else {
        eprintln!("\n(compare leg requested but OpenSCAD binary not found — skipped)");
    }

    // The gate: SOME models must render (a total wipe-out is a real regression). Not a pass ratchet yet — the
    // distribution + profile + divergence report ARE the deliverable, and the numbers move as the tiers land.
    assert!(rendered > 0, "no models rendered — the sweep is broken");
}
