//! BOSL2 test corpus runner (K.1, tier 2) — sweep BOSL2's OWN assert-based test suite through scad-rs.
//!
//! BOSL2 ships its tests as `.scadtest` TOML files (`tests/test_*.scadtest`): each `[[test]]` block's
//! `script` is a scad program that `include`s `std.scad` and calls `assert(...)` on BOSL2's expected values.
//! So the tests are SELF-CHECKING — run a script through scad-rs and, if it evaluates without error, every
//! assertion held, which means our evaluator matched BOSL2 bit-for-bit. No oracle needed; the asserts ARE
//! the spec. (Tier 2 of K.1's three; the OpenSCAD suite + `models/` are tiers 1 and 3.)
//!
//! ISOLATION + PARALLELISM: a script can overflow the host stack (BOSL2's `fnliterals` builds deeply
//! recursive partial-application closures — the deferred #141 "Safari cliff"), and a stack overflow is a
//! `SIGABRT` — UNCATCHABLE, no `catch_unwind` survives it. So an in-process sweep would let one bad test
//! abort all 900+. [`run_bosl2_corpus_isolated`] splits the suite into one chunk per CPU and runs each in a
//! `corpus_worker` subprocess that evaluates its range IN-PROCESS (fast — binary + BOSL2 parse paid once per
//! chunk, not per test) and streams results back; a worker that dies buckets the crasher as [`Bucket::Crash`]
//! and restarts just past it, so every chunk finishes. The per-test wall-time it records is what the perf
//! tier (K.1.2) builds on.
//!
//! Failures triage into named [`Bucket`]s: `Assertion` is the load-bearing one — our MATH is wrong (a real
//! correctness bug) — while `Unimplemented`/`Eval`/`Crash` are feature-and-robustness gaps that feed the
//! Phase-I BOSL2 burn-down.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fab_lang::Error;

/// The per-test wall-clock budget. A normal BOSL2 test evaluates in well under a second; a script that runs
/// past this is hung or pathological (a non-terminating recursion the guard doesn't catch, a runaway
/// comprehension) — the worker is killed and the test buckets as [`Bucket::Timeout`]. Generous enough that a
/// legitimately heavy geometry test isn't cut off.
const PER_TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Where a single `[[test]]` landed — `Pass`, or the category of its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    /// Evaluated cleanly — every `assert` in the script held.
    Pass,
    /// An `assert` fired: our computed value diverges from BOSL2's expected one. A CORRECTNESS bug.
    Assertion,
    /// A non-assert evaluation error (unknown function, arity, recursion depth, undef misuse, …).
    Eval,
    /// A deferred/unbuilt construct (`text`/`minkowski`, or an unimplemented feature).
    Unimplemented,
    /// The script failed to parse.
    Parse,
    /// A `use`/`include` couldn't be resolved/read, or an `import`/`surface` had no reader.
    Load,
    /// A geometry node couldn't lower to a solid.
    Lower,
    /// The worker subprocess died (a stack overflow / abort) — an isolation-only outcome.
    Crash,
    /// The test ran past the per-test wall-clock budget (a hang or pathological slowness) and was killed.
    Timeout,
}

impl Bucket {
    /// A short stable label for the report + the histogram.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Bucket::Pass => "pass",
            Bucket::Assertion => "assertion",
            Bucket::Eval => "eval",
            Bucket::Unimplemented => "unimplemented",
            Bucket::Parse => "parse",
            Bucket::Load => "load",
            Bucket::Lower => "lower",
            Bucket::Crash => "crash",
            Bucket::Timeout => "timeout",
        }
    }

    /// Parse a [`label`](Bucket::label) back — the worker↔parent wire format.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Bucket> {
        [
            Bucket::Pass,
            Bucket::Assertion,
            Bucket::Eval,
            Bucket::Unimplemented,
            Bucket::Parse,
            Bucket::Load,
            Bucket::Lower,
            Bucket::Crash,
            Bucket::Timeout,
        ]
        .into_iter()
        .find(|b| b.label() == s)
    }
}

/// One flattened test: which `.scadtest` file + `[[test]]` name it came from, and its scad `script`.
pub struct TestCase {
    /// The `.scadtest` file (basename).
    pub file: String,
    /// The `[[test]]` `name`.
    pub name: String,
    /// The scad program to run.
    pub script: String,
}

/// One test's result: its origin, [`Bucket`], wall-time (ms), and (on failure) the error's first line.
pub struct TestResult {
    /// The `.scadtest` file (basename).
    pub file: String,
    /// The `[[test]]` `name`.
    pub name: String,
    /// Where it landed.
    pub bucket: Bucket,
    /// scad-rs eval wall-time in milliseconds (0 for a crash).
    pub ms: u128,
    /// The failure's first line (empty on `Pass`).
    pub detail: String,
}

/// One test run's outcome: its [`Bucket`], wall-time (ms), and first-line detail.
type RunOutcome = (Bucket, u128, String);

/// One `[[test]]` block from a `.scadtest` TOML file.
#[derive(serde::Deserialize)]
struct TestBlock {
    name: String,
    script: String,
}

/// A `.scadtest` file: an array of `[[test]]` tables.
#[derive(serde::Deserialize)]
struct ScadTestFile {
    #[serde(default)]
    test: Vec<TestBlock>,
}

/// Enumerate BOSL2's whole `.scadtest` suite under `bosl2_dir` into a DETERMINISTIC flattened list (files
/// sorted, `[[test]]` blocks in declaration order). Parent + worker enumerate identically, so nothing needs
/// to be threaded between them beyond the script text.
///
/// # Errors
/// Fails if `tests/` can't be read or a `.scadtest` file isn't valid TOML.
pub fn enumerate_tests(bosl2_dir: &Path) -> Result<Vec<TestCase>> {
    let tests_dir = bosl2_dir.join("tests");
    let mut files: Vec<_> = std::fs::read_dir(&tests_dir)
        .with_context(|| format!("reading {}", tests_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "scadtest"))
        .collect();
    files.sort();

    let mut cases = Vec::new();
    for path in files {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: ScadTestFile = toml::from_str(&text)
            .with_context(|| format!("parsing {} as .scadtest TOML", path.display()))?;
        for block in parsed.test {
            cases.push(TestCase {
                file: file.clone(),
                name: block.name,
                script: block.script,
            });
        }
    }
    Ok(cases)
}

/// Run one test `script` in-process (base dir = `tests_dir`, so its `include <../std.scad>` resolves) → its
/// bucket + wall-time (ms) + first-line detail. Uses the geometry entry so a script that builds geometry
/// AFTER its asserts still counts as a pass (we only care whether EVAL — hence every assert — succeeded).
///
/// This is the WORKER's single-test body — call it from an isolated subprocess, never the parent sweep, so a
/// stack overflow here can't take the harness down with it.
#[must_use]
pub fn run_script(script: &str, tests_dir: &Path) -> (Bucket, u128, String) {
    let start = Instant::now();
    let result = fab_lang::evaluate_geometry_with_base(script, tests_dir, &[]);
    let ms = start.elapsed().as_millis();
    match result {
        Ok(_) => (Bucket::Pass, ms, String::new()),
        Err(e) => {
            let first = format!("{e}").lines().next().unwrap_or_default().to_string();
            (classify(&e), ms, first)
        }
    }
}

/// Map an [`Error`] to its [`Bucket`] — the `assertion failed` message (from a falsy `assert`) is the one
/// that means our MATH diverged, so it gets its own bucket separate from other eval errors.
fn classify(e: &Error) -> Bucket {
    match e {
        Error::Parse(_) => Bucket::Parse,
        Error::Eval(m) if m.starts_with("assertion failed") => Bucket::Assertion,
        Error::Eval(_) => Bucket::Eval,
        Error::Load(_) => Bucket::Load,
        Error::Lower(_) => Bucket::Lower,
        Error::Unimplemented(_) => Bucket::Unimplemented,
        // `Error` is #[non_exhaustive]; a future variant lands in the catch-all until it earns a bucket.
        _ => Bucket::Eval,
    }
}

/// Run the whole BOSL2 `.scadtest` suite with crash-resilient, PARALLEL subprocess isolation. The test range
/// is split into one chunk per CPU; each chunk runs in its own `worker_exe` (the `corpus_worker` bin), which
/// evaluates its range IN-PROCESS (fast — binary + enumeration cost paid once per chunk) and streams
/// `idx\tbucket\tms\tdetail` per test. A stack overflow aborts only that worker; its chunk driver buckets the
/// crasher as [`Bucket::Crash`] and restarts just past it, so every chunk finishes. Results merge by index.
/// `bosl2_dir` must hold `std.scad` + `tests/`.
///
/// # Errors
/// Fails only if the suite can't be enumerated — per-worker spawn/abort failures are absorbed as crash
/// buckets, never propagated (a broken corpus is data, not a harness error).
pub fn run_bosl2_corpus_isolated(bosl2_dir: &Path, worker_exe: &Path) -> Result<Vec<TestResult>> {
    let cases = enumerate_tests(bosl2_dir)?;
    let n = cases.len();
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(n.max(1));
    let chunk = n.div_ceil(workers.max(1));

    // One thread per chunk; each drives its own crash-resilient worker over `[lo, hi)`. `scope` lets the
    // threads borrow `worker_exe`/`bosl2_dir` without cloning.
    let chunks: Vec<Vec<(usize, RunOutcome)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let lo = w * chunk;
                let hi = ((w + 1) * chunk).min(n);
                s.spawn(move || run_range(worker_exe, bosl2_dir, lo, hi))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    let mut slots: Vec<Option<RunOutcome>> = (0..n).map(|_| None).collect();
    for chunk_results in chunks {
        for (idx, result) in chunk_results {
            if idx < n {
                slots[idx] = Some(result);
            }
        }
    }
    Ok(cases
        .into_iter()
        .zip(slots)
        .map(|(case, slot)| {
            let (bucket, ms, detail) = slot.unwrap_or((Bucket::Crash, 0, "not run".to_string()));
            TestResult {
                file: case.file,
                name: case.name,
                bucket,
                ms,
                detail,
            }
        })
        .collect())
}

/// Drive one chunk `[lo, hi)` through a crash-AND-timeout-resilient worker: spawn
/// `worker_exe <bosl2_dir> <start> <hi>`, collect its streamed results with a per-test watchdog, and on abort
/// OR a stalled test restart just past the offender (bucketed [`Bucket::Crash`] / [`Bucket::Timeout`]) — until
/// the whole range is covered. Never fails: a spawn error marks the remaining range as crashes.
///
/// The watchdog: a reader thread forwards each worker line over a channel; the driver `recv_timeout`s on it.
/// A line arriving resets the clock; [`PER_TEST_TIMEOUT`] of silence means the test after the last emitted one
/// is hung — kill the worker and restart past it.
fn run_range(
    worker_exe: &Path,
    bosl2_dir: &Path,
    lo: usize,
    hi: usize,
) -> Vec<(usize, RunOutcome)> {
    let mut out = Vec::new();
    let mut next = lo;
    while next < hi {
        let spawned = Command::new(worker_exe)
            .arg(bosl2_dir)
            .arg(next.to_string())
            .arg(hi.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            out.extend((next..hi).map(|i| (i, (Bucket::Crash, 0, "worker spawn failed".to_string()))));
            break;
        };

        // Forward the worker's lines over a channel so the driver can time out on silence.
        let (tx, rx) = mpsc::channel();
        let reader = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
        });

        let mut highest: Option<usize> = None;
        let mut timed_out = false;
        loop {
            match rx.recv_timeout(PER_TEST_TIMEOUT) {
                Ok(line) => {
                    if let Some((idx, result)) = parse_stream_line(&line)
                        && (next..hi).contains(&idx)
                    {
                        out.push((idx, result));
                        highest = Some(idx);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // worker finished or aborted
            }
        }
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        if let Some(r) = reader {
            let _ = r.join();
        }
        if ok && !timed_out {
            break; // worker ran the whole remaining range cleanly
        }
        // The test after the last emitted one is the offender (or `next` if none emitted). A timeout kill
        // shows up as a non-success exit too, so `timed_out` picks the bucket.
        let offender = highest.map_or(next, |h| h + 1);
        if offender < hi {
            let outcome = if timed_out {
                (Bucket::Timeout, PER_TEST_TIMEOUT.as_millis(), "exceeded per-test budget".to_string())
            } else {
                (Bucket::Crash, 0, "worker aborted (stack overflow?)".to_string())
            };
            out.push((offender, outcome));
        }
        next = offender + 1;
    }
    out
}

/// Parse a worker's `idx\tbucket\tms\tdetail` stream line.
fn parse_stream_line(line: &str) -> Option<(usize, RunOutcome)> {
    let mut parts = line.trim_end().splitn(4, '\t');
    let idx: usize = parts.next()?.parse().ok()?;
    let bucket = Bucket::from_label(parts.next()?)?;
    let ms: u128 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let detail = parts.next().unwrap_or("").to_string();
    Some((idx, (bucket, ms, detail)))
}

/// The `corpus_worker` bin's body: enumerate the suite, run tests `[start, end)` IN-PROCESS, and stream
/// `idx\tbucket\tms\tdetail` (flushed) per test. A stack overflow aborts this process; the parent restarts it
/// past the crasher. Kept here so the bin is a one-liner and the logic stays testable.
///
/// # Errors
/// Fails if the suite can't be enumerated.
pub fn worker_main(bosl2_dir: &Path, start: usize, end: usize) -> Result<()> {
    let tests_dir = bosl2_dir.join("tests");
    let cases = enumerate_tests(bosl2_dir)?;
    let end = end.min(cases.len());
    let mut out = std::io::stdout();
    for (idx, case) in cases.iter().enumerate().take(end).skip(start) {
        let (bucket, ms, detail) = run_script(&case.script, &tests_dir);
        // Tabs/newlines in the detail would corrupt the single-line wire format — flatten them.
        let detail = detail.replace(['\t', '\n', '\r'], " ");
        writeln!(out, "{idx}\t{}\t{ms}\t{detail}", bucket.label()).ok();
        out.flush().ok(); // per-test flush: the last line the parent sees pinpoints a crasher
    }
    Ok(())
}

/// Tally results by bucket, in `Bucket` order — the divergence histogram for the report.
#[must_use]
pub fn histogram(results: &[TestResult]) -> std::collections::BTreeMap<Bucket, usize> {
    let mut h = std::collections::BTreeMap::new();
    for r in results {
        *h.entry(r.bucket).or_insert(0) += 1;
    }
    h
}

/// Cluster the failures by (bucket, error first-line) and count each — the burn-down worklist. Many tests
/// share one root cause (a missing builtin, one broken helper's assert), so the biggest clusters are the
/// highest-leverage fixes: knock one out and the pass count jumps. Sorted by count descending, then label.
#[must_use]
pub fn signatures(results: &[TestResult]) -> Vec<(Bucket, String, usize)> {
    let mut map: std::collections::BTreeMap<(Bucket, String), usize> = std::collections::BTreeMap::new();
    for r in results.iter().filter(|r| r.bucket != Bucket::Pass) {
        *map.entry((r.bucket, r.detail.clone())).or_insert(0) += 1;
    }
    let mut v: Vec<(Bucket, String, usize)> = map.into_iter().map(|((b, d), n)| (b, d, n)).collect();
    v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    v
}

/// Guard against a malformed worker path being silently treated as "everything crashed".
///
/// # Errors
/// Fails if `worker_exe` doesn't exist — a misconfigured harness, not a corpus result.
pub fn check_worker(worker_exe: &Path) -> Result<()> {
    if !worker_exe.exists() {
        bail!("corpus worker binary not found at {}", worker_exe.display());
    }
    Ok(())
}
