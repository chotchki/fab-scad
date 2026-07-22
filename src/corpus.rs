//! BOSL2 test corpus runner (K.1, tier 2) — sweep BOSL2's OWN assert-based test suite through scad-rs.
//!
//! BOSL2 ships its tests as `.scadtest` TOML files (`tests/test_*.scadtest`): each `[[test]]` block's
//! `script` is a scad program that `include`s `std.scad` and calls `assert(...)` on BOSL2's expected values.
//! So the tests are SELF-CHECKING — run a script through scad-rs and, if it evaluates without error, every
//! assertion held, which means our evaluator matched BOSL2 bit-for-bit. No oracle needed; the asserts ARE
//! the spec. (Tier 2 of K.1's three; the OpenSCAD suite + `models/` are tiers 1 and 3.)
//!
//! ISOLATION + PARALLELISM: a script could historically overflow the host stack (#141's "Safari
//! cliff" — RESOLVED by Phase AB 2026-07-22: the real seams were the Echo/Assert body re-entries +
//! comprehension nesting, both task-framed now; closures/calls were always framed), and a stack
//! overflow is a `SIGABRT` — UNCATCHABLE, no `catch_unwind` survives it, so the isolation stays as
//! the belt for whatever the NEXT cliff turns out to be. So an in-process sweep would let one bad test
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
use fab_lang::{Error, Message};

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
    /// Evaluated cleanly but produced NULL geometry (SU.3, examples lane only) — "renders" nothing,
    /// which for an upstream example that used to produce a model is a failure, not a pass.
    Empty,
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
            Bucket::Empty => "empty",
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
            Bucket::Empty,
            Bucket::Crash,
            Bucket::Timeout,
        ]
        .into_iter()
        .find(|b| b.label() == s)
    }
}

/// Which upstream corpus a sweep runs (SU.3). `Tests` = the `.scadtest` assertion suite (the VALUES
/// bar — a clean eval means every assert held). `Examples` = `examples/*.scad` (the RENDER-CLEAN bar —
/// eval with no error and non-null geometry; mesh-level differential stays R.2's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// `tests/*.scadtest` — upstream's own assertions.
    Tests,
    /// `examples/*.scad` — upstream's showcase models.
    Examples,
    /// An arbitrary `.scad` file list from a MANIFEST (SU.4: the openscad corpus lane — the nightly
    /// tree-diffs upstream's `testdata/`+`examples/` and sweeps only the new/changed files). The sweep's
    /// "root" argument is the manifest path, one absolute `.scad` path per line; each file evals with
    /// its own parent as base (relative includes resolve in the sparse checkout). Eval-clean is the
    /// whole bar — no null-geometry check (their corpus is full of 2D/echo-only files) and no verdict
    /// inversion; there's no expectation spec to hold it to, only "does our evaluator survive it".
    Files,
}

impl Lane {
    /// Stable label — the worker argv + report key.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Lane::Tests => "tests",
            Lane::Examples => "examples",
            Lane::Files => "files",
        }
    }

    /// Parse a [`label`](Lane::label) back (the worker argv format).
    #[must_use]
    pub fn from_label(s: &str) -> Option<Lane> {
        [Lane::Tests, Lane::Examples, Lane::Files]
            .into_iter()
            .find(|l| l.label() == s)
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
    /// `false` for a `.scadtest` block marked `expect_success = false` — a test that DELIBERATELY feeds bad
    /// input to prove a function REJECTS it (`test_*_errors`). For these, an eval error is the PASS and a
    /// clean eval is the failure — the inverse of a normal test. Default `true`.
    pub expect_success: bool,
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
    /// BOSL2's `expect_success` flag (default `true`) — see [`TestCase::expect_success`].
    #[serde(default = "yes")]
    expect_success: bool,
}

/// serde default for [`TestBlock::expect_success`] — an absent flag means a normal (expect-success) test.
fn yes() -> bool {
    true
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
                expect_success: block.expect_success,
            });
        }
    }
    Ok(cases)
}

/// Enumerate `examples/*.scad` under `bosl2_dir` into the same flattened [`TestCase`] list shape as the
/// test suite (files sorted; one case per file, `name` = "render"). The script is the file's content;
/// its `include <BOSL2/std.scad>` resolves via the library path the runner passes ([`run_example`]).
///
/// # Errors
/// Fails if `examples/` can't be read.
pub fn enumerate_examples(bosl2_dir: &Path) -> Result<Vec<TestCase>> {
    let dir = bosl2_dir.join("examples");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "scad"))
        .collect();
    files.sort();

    let mut cases = Vec::new();
    for path in files {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let script = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        cases.push(TestCase {
            file,
            name: "render".to_string(),
            script,
            expect_success: true,
        });
    }
    Ok(cases)
}

/// Enumerate a [`Lane::Files`] MANIFEST: one `.scad` path per line (blank lines + `#` comments
/// skipped), each becoming a case whose `file` is the FULL path (the runner needs it for base-dir
/// resolution) — sorted for parent/worker index agreement.
///
/// # Errors
/// Fails if the manifest or any listed file can't be read — a sweep against half a corpus would
/// under-report, so it's strict.
pub fn enumerate_files(manifest: &Path) -> Result<Vec<TestCase>> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading manifest {}", manifest.display()))?;
    let mut paths: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    paths.sort_unstable();
    let mut cases = Vec::new();
    for p in paths {
        // Decode via the fs-seam doctrine (AA.5): UTF-8 with a Latin-1 fallback — the same read a
        // real `fab render` would do, so the sweep's verdicts match production (nbsp-latin1-test.scad
        // is Latin-1 on purpose, and upstream's lexer is byte-lenient). Deterministic both
        // parent- and worker-side.
        let bytes = std::fs::read(p).with_context(|| format!("reading listed file {p}"))?;
        cases.push(TestCase {
            file: p.to_string(),
            name: "eval".to_string(),
            script: fab_lang::decode_scad_source(bytes),
            expect_success: true,
        });
    }
    Ok(cases)
}

/// Run one MANIFEST-listed file (SU.4): eval with the file's own parent as base so its relative
/// includes resolve. Eval-clean is the bar — see [`Lane::Files`] — EXCEPT a missing-library warning,
/// which buckets [`Bucket::Load`]: the tolerant loader would otherwise eval an EMPTY program and
/// report a vacuous pass (a file whose includes never loaded proved nothing).
#[must_use]
pub fn run_file(script: &str, path: &Path) -> (Bucket, u128, String) {
    let base = path.parent().unwrap_or(Path::new("."));
    let start = Instant::now();
    let result = fab_lang::evaluate_geometry_with_base_full(script, base, &[]);
    let ms = start.elapsed().as_millis();
    match result {
        Ok((_, messages)) => {
            let verdict = messages.iter().find_map(|m| match m {
                Message::Warning(w) if w.starts_with("Can't open library") => {
                    Some((Bucket::Load, w))
                }
                Message::Warning(w) if w.starts_with("assertion failed") => {
                    Some((Bucket::Assertion, w))
                }
                _ => None,
            });
            match verdict {
                Some((bucket, w)) => (bucket, ms, w.lines().next().unwrap_or_default().to_string()),
                None => (Bucket::Pass, ms, String::new()),
            }
        }
        Err(e) => {
            let first = format!("{e}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            (classify(&e), ms, first)
        }
    }
}

/// Enumerate `lane`'s corpus — the ONE enumeration parent + worker both call, so their indices agree.
/// For [`Lane::Files`], `bosl2_dir` is the MANIFEST path, not a BOSL2 tree.
///
/// # Errors
/// As [`enumerate_tests`] / [`enumerate_examples`] / [`enumerate_files`].
pub fn enumerate_lane(bosl2_dir: &Path, lane: Lane) -> Result<Vec<TestCase>> {
    match lane {
        Lane::Tests => enumerate_tests(bosl2_dir),
        Lane::Examples => enumerate_examples(bosl2_dir),
        Lane::Files => enumerate_files(bosl2_dir),
    }
}

/// Run one EXAMPLE in-process (SU.3, the render-clean bar): base = `examples/`, library path =
/// `bosl2_dir`'s PARENT so `include <BOSL2/std.scad>` resolves. Verdicts beyond [`run_script`]'s:
/// a missing-library warning buckets [`Bucket::Load`] (the loader tolerates it with an empty program,
/// which would otherwise "pass" VACUOUSLY — an example that never loaded BOSL2 proved nothing), and a
/// clean eval whose geometry is NULL buckets [`Bucket::Empty`] (an upstream showcase model that
/// produces nothing is a failure even without an error).
#[must_use]
pub fn run_example(script: &str, bosl2_dir: &Path) -> (Bucket, u128, String) {
    let start = Instant::now();
    let examples_dir = bosl2_dir.join("examples");
    let lib_root = bosl2_dir.parent().unwrap_or(Path::new(".")).to_path_buf();
    let result = fab_lang::evaluate_geometry_with_base_full(script, &examples_dir, &[lib_root]);
    let ms = start.elapsed().as_millis();
    match result {
        Ok((geo, messages)) => {
            let verdict = messages.iter().find_map(|m| match m {
                Message::Warning(w) if w.starts_with("Can't open library") => {
                    Some((Bucket::Load, w))
                }
                Message::Warning(w) if w.starts_with("assertion failed") => {
                    Some((Bucket::Assertion, w))
                }
                _ => None,
            });
            match verdict {
                Some((bucket, w)) => (bucket, ms, w.lines().next().unwrap_or_default().to_string()),
                None if geo.is_null() => (
                    Bucket::Empty,
                    ms,
                    "evaluated cleanly but produced NULL geometry".to_string(),
                ),
                None => (Bucket::Pass, ms, String::new()),
            }
        }
        Err(e) => {
            let first = format!("{e}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            (classify(&e), ms, first)
        }
    }
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
    // `_full` for the MESSAGES: since L.5.8 a failed `assert` HALTS-and-exports partial geometry (a WARNING,
    // not a hard error) to match OpenSCAD's STL export — but BOSL2's scadtest suite treats a fired assert as a
    // FAILURE (its `expect_success=false` negatives PROVE validation rejects bad input). So an assert warning
    // still buckets as `Assertion` here, the verdict the corpus ran on before the driver's export change.
    let result = fab_lang::evaluate_geometry_with_base_full(script, tests_dir, &[]);
    let ms = start.elapsed().as_millis();
    match result {
        Ok((_, messages)) => {
            match messages.iter().find_map(|m| match m {
                Message::Warning(w) if w.starts_with("assertion failed") => Some(w),
                _ => None,
            }) {
                Some(w) => (
                    Bucket::Assertion,
                    ms,
                    w.lines().next().unwrap_or_default().to_string(),
                ),
                None => (Bucket::Pass, ms, String::new()),
            }
        }
        Err(e) => {
            let first = format!("{e}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            (classify(&e), ms, first)
        }
    }
}

/// Map an [`Error`] to its [`Bucket`] — the `assertion failed` message (from a falsy `assert`) is the one
/// that means our MATH diverged, so it gets its own bucket separate from other eval errors.
fn classify(e: &Error) -> Bucket {
    // Peel any W.3.37 `Spanned` wrapper so the classification reads the ROOT fault, not the catch-all.
    match e.root() {
        Error::Parse(_) => Bucket::Parse,
        // A failed `assert` — its own bucket (it means our MATH diverged). Now a distinct Error::Assert
        // variant (L.5.8); the legacy Eval-message check stays for any pre-variant assert string.
        Error::Assert(_) => Bucket::Assertion,
        Error::Eval(m) if m.starts_with("assertion failed") => Bucket::Assertion,
        Error::Eval(_) => Bucket::Eval,
        Error::Load(_) => Bucket::Load,
        Error::Lower(_) => Bucket::Lower,
        Error::Unimplemented(_) => Bucket::Unimplemented,
        // A missing builtin / unknown module — a feature gap, same bucket as a deferred construct, but its
        // message NAMES the symbol so the signature histogram clusters per-symbol (L.2.1).
        Error::Unknown(_) => Bucket::Unimplemented,
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
    run_corpus_isolated(bosl2_dir, worker_exe, Lane::Tests)
}

/// [`run_bosl2_corpus_isolated`] generalized over the [`Lane`] (SU.3): the same crash-and-timeout-
/// resilient parallel sweep, over either upstream corpus. The examples lane REQUIRES `bosl2_dir` to be
/// named `BOSL2` — examples `include <BOSL2/std.scad>`, which resolves against the checkout's PARENT,
/// so any other basename would vacuous-fail every example (stage a candidate under a `BOSL2` symlink).
///
/// # Errors
/// As [`run_bosl2_corpus_isolated`], plus the examples-lane basename check.
pub fn run_corpus_isolated(
    bosl2_dir: &Path,
    worker_exe: &Path,
    lane: Lane,
) -> Result<Vec<TestResult>> {
    if lane == Lane::Examples && bosl2_dir.file_name().is_none_or(|n| n != "BOSL2") {
        bail!(
            "examples lane needs the checkout AT a directory named BOSL2 (include <BOSL2/…> resolves \
             against its parent); got {}",
            bosl2_dir.display()
        );
    }
    let cases = enumerate_lane(bosl2_dir, lane)?;
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
                s.spawn(move || run_range(worker_exe, bosl2_dir, lo, hi, lane))
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
            // `expect_success = false` INVERTS the verdict: the test feeds bad input to prove rejection, so
            // ANY failure (error/crash/timeout) is the expected PASS, and a clean eval is the real failure.
            let (bucket, detail) = match (case.expect_success, bucket) {
                (true, b) => (b, detail),
                (false, Bucket::Pass) => (
                    Bucket::Assertion,
                    "expected a failure (expect_success=false), but evaluated cleanly".to_string(),
                ),
                (false, _) => (Bucket::Pass, String::new()),
            };
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
    lane: Lane,
) -> Vec<(usize, RunOutcome)> {
    let mut out = Vec::new();
    let mut next = lo;
    while next < hi {
        let spawned = Command::new(worker_exe)
            .arg(bosl2_dir)
            .arg(next.to_string())
            .arg(hi.to_string())
            .arg(lane.label())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            out.extend(
                (next..hi).map(|i| (i, (Bucket::Crash, 0, "worker spawn failed".to_string()))),
            );
            break;
        };

        // Forward the worker's lines over a channel so the driver can time out on silence.
        let (tx, rx) = mpsc::channel();
        let reader = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
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
                (
                    Bucket::Timeout,
                    PER_TEST_TIMEOUT.as_millis(),
                    "exceeded per-test budget".to_string(),
                )
            } else {
                (
                    Bucket::Crash,
                    0,
                    "worker aborted (stack overflow?)".to_string(),
                )
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
pub fn worker_main(bosl2_dir: &Path, start: usize, end: usize, lane: Lane) -> Result<()> {
    let tests_dir = bosl2_dir.join("tests");
    let cases = enumerate_lane(bosl2_dir, lane)?;
    let end = end.min(cases.len());
    let mut out = std::io::stdout();
    for (idx, case) in cases.iter().enumerate().take(end).skip(start) {
        let (bucket, ms, detail) = match lane {
            Lane::Tests => run_script(&case.script, &tests_dir),
            Lane::Examples => run_example(&case.script, bosl2_dir),
            Lane::Files => run_file(&case.script, Path::new(&case.file)),
        };
        // Tabs/newlines in the detail would corrupt the single-line wire format — flatten them.
        let detail = detail.replace(['\t', '\n', '\r'], " ");
        writeln!(out, "{idx}\t{}\t{ms}\t{detail}", bucket.label()).ok();
        out.flush().ok(); // per-test flush: the last line the parent sees pinpoints a crasher
    }
    Ok(())
}

/// SU.3: pairwise verdicts between a COMMITTED-pin sweep and a CANDIDATE-pin sweep of the same lane,
/// keyed `(file, name)`. The load-bearing sets: `regressions` (passed committed, fails candidate — the
/// new upstream version BROKE us) and `new_failing` (a test/example that only exists upstream now, and
/// we fail it — a fresh gap). `fixed` + `still_failing` complete the picture; `removed` names what
/// upstream deleted.
#[derive(Debug, Default)]
pub struct CorpusDiff {
    /// Passed on committed, fails on candidate: `(file, name, candidate bucket, detail)`.
    pub regressions: Vec<(String, String, Bucket, String)>,
    /// Exists only on candidate and fails there: `(file, name, bucket, detail)`.
    pub new_failing: Vec<(String, String, Bucket, String)>,
    /// Failed on committed, passes on candidate.
    pub fixed: Vec<(String, String)>,
    /// Fails on both — the pre-existing gap, not this bump's fault.
    pub still_failing: usize,
    /// Exists only on committed (upstream removed it).
    pub removed: Vec<(String, String)>,
    /// Pass counts as `(committed passed, committed total, candidate passed, candidate total)`.
    pub counts: (usize, usize, usize, usize),
}

/// Diff two sweeps of the same [`Lane`] (SU.3) — committed pin vs candidate. Pure; order-insensitive.
#[must_use]
pub fn diff_results(committed: &[TestResult], candidate: &[TestResult]) -> CorpusDiff {
    let key = |r: &TestResult| (r.file.clone(), r.name.clone());
    let committed_map: std::collections::BTreeMap<_, _> =
        committed.iter().map(|r| (key(r), r)).collect();
    let candidate_map: std::collections::BTreeMap<_, _> =
        candidate.iter().map(|r| (key(r), r)).collect();

    let mut d = CorpusDiff {
        counts: (
            committed
                .iter()
                .filter(|r| r.bucket == Bucket::Pass)
                .count(),
            committed.len(),
            candidate
                .iter()
                .filter(|r| r.bucket == Bucket::Pass)
                .count(),
            candidate.len(),
        ),
        ..CorpusDiff::default()
    };
    for (k, cand) in &candidate_map {
        match committed_map.get(k) {
            Some(comm) => match (comm.bucket == Bucket::Pass, cand.bucket == Bucket::Pass) {
                (true, false) => d.regressions.push((
                    cand.file.clone(),
                    cand.name.clone(),
                    cand.bucket,
                    cand.detail.clone(),
                )),
                (false, true) => d.fixed.push((cand.file.clone(), cand.name.clone())),
                (false, false) => d.still_failing += 1,
                (true, true) => {}
            },
            None if cand.bucket != Bucket::Pass => d.new_failing.push((
                cand.file.clone(),
                cand.name.clone(),
                cand.bucket,
                cand.detail.clone(),
            )),
            None => {}
        }
    }
    for k in committed_map.keys() {
        if !candidate_map.contains_key(k) {
            d.removed.push((k.0.clone(), k.1.clone()));
        }
    }
    d
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
    let mut map: std::collections::BTreeMap<(Bucket, String), usize> =
        std::collections::BTreeMap::new();
    for r in results.iter().filter(|r| r.bucket != Bucket::Pass) {
        *map.entry((r.bucket, r.detail.clone())).or_insert(0) += 1;
    }
    let mut v: Vec<(Bucket, String, usize)> =
        map.into_iter().map(|((b, d), n)| (b, d, n)).collect();
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

#[cfg(test)]
mod tests {
    use super::{Bucket, Lane, TestResult, diff_results};

    fn r(file: &str, name: &str, bucket: Bucket) -> TestResult {
        TestResult {
            file: file.to_string(),
            name: name.to_string(),
            bucket,
            ms: 1,
            detail: if bucket == Bucket::Pass {
                String::new()
            } else {
                "boom".to_string()
            },
        }
    }

    /// SU.3: every pairwise verdict lands in its set — regression (pass→fail), fixed (fail→pass),
    /// still-failing (fail→fail), new-failing (candidate-only fail), removed (committed-only). A
    /// candidate-only PASS (a new upstream test we handle) is silently fine.
    #[test]
    fn diff_classifies_every_shape() {
        let committed = vec![
            r("a", "t1", Bucket::Pass),      // → fails on candidate = regression
            r("a", "t2", Bucket::Eval),      // → passes on candidate = fixed
            r("a", "t3", Bucket::Assertion), // → still failing
            r("a", "t4", Bucket::Pass),      // absent on candidate = removed
            r("a", "t5", Bucket::Pass),      // stays passing
        ];
        let candidate = vec![
            r("a", "t1", Bucket::Assertion),
            r("a", "t2", Bucket::Pass),
            r("a", "t3", Bucket::Timeout),
            r("a", "t5", Bucket::Pass),
            r("a", "t6", Bucket::Eval), // new upstream test, we fail it
            r("a", "t7", Bucket::Pass), // new upstream test, we pass it
        ];
        let d = diff_results(&committed, &candidate);
        assert_eq!(
            d.regressions,
            vec![(
                "a".to_string(),
                "t1".to_string(),
                Bucket::Assertion,
                "boom".to_string()
            )]
        );
        assert_eq!(d.fixed, vec![("a".to_string(), "t2".to_string())]);
        assert_eq!(d.still_failing, 1);
        assert_eq!(
            d.new_failing,
            vec![(
                "a".to_string(),
                "t6".to_string(),
                Bucket::Eval,
                "boom".to_string()
            )]
        );
        assert_eq!(d.removed, vec![("a".to_string(), "t4".to_string())]);
        assert_eq!(d.counts, (3, 5, 3, 6));
    }

    /// The worker argv lane labels round-trip (the parent↔worker wire format).
    #[test]
    fn lane_labels_round_trip() {
        for lane in [Lane::Tests, Lane::Examples] {
            assert_eq!(Lane::from_label(lane.label()), Some(lane));
        }
        assert_eq!(Lane::from_label("nope"), None);
    }
}
