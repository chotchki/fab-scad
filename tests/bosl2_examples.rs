//! L.3.7 — the BOSL2 EXAMPLE corpus. Every `// Example:` doc block in the BOSL2 source (~2500 of them, across
//! 53 files) is a small oracle-comparable geometry snippet exercising one module/function — the
//! whole-library-surface companion to the models harness (chotchki's parts). We EXTRACT each block, wrap it in
//! its includes, and COMPARE the rendered mesh against the OpenSCAD oracle (volume-residual + genus + bbox).
//!
//! `#[ignore]` — a minutes-long sweep needing `libs/BOSL2` + OpenSCAD. Run on demand:
//!   cargo test -p fab-scad --test bosl2_examples -- --ignored --nocapture
//! Skips cleanly when BOSL2 / OpenSCAD is absent, like the corpus + differential.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration harness: unwrap/expect ARE the assertions"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fab_scad::openscad::find_bin;

/// Per-example wall-clock budget — a doc example is tiny, so this only catches a pathological hang. The
/// oracle render inside has its own timeout; this guards the scad-rs side + a slow oracle.
const BUDGET: Duration = Duration::from_secs(20);
/// Concurrent examples (independent oracle subprocesses parallelize freely on a 16-core box).
const CONCURRENCY: usize = 12;

/// One extracted example: where it came from + the code + whether it's 2D (needs an extrude bridge to compare
/// as a solid) and which BOSL2 file to `include` (specialized modules — gears/screws — aren't in std).
struct Example {
    origin: String, // `file.scad:line — title` for the report
    file_stem: String,
    code: String,
    is_2d: bool,
}

/// Parse a `// Example[(FLAGS)]: title` header → (flags, title). `None` if the line isn't an example header.
fn parse_header(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix("// Example")?;
    if let Some(after) = rest.strip_prefix('(') {
        let end = after.find(')')?;
        let title = after[end + 1..].trim_start_matches([':', ' ']).to_string();
        Some((after[..end].to_string(), title))
    } else {
        let title = rest.strip_prefix(':')?.trim().to_string();
        Some((String::new(), title))
    }
}

/// Extract every renderable example block from the top-level BOSL2 `.scad` files (not `tests/`, not
/// `examples/`). A block is the run of `//   <code>` lines after an example header. NORENDER + animated
/// (`Anim`, `Spin`, `FlatSpin` — `$t`-driven) examples are skipped: they're not a single static solid.
fn extract_examples(bosl2: &Path) -> Vec<Example> {
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(bosl2)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "scad"))
        .collect();
    files.sort();
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        // std.scad is just includes; version/constants have no geometry examples worth wrapping.
        if stem == "std" {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = src.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let Some((flags, title)) = parse_header(lines[i]) else {
                i += 1;
                continue;
            };
            // Collect the following `//   code` block (blank `//` lines kept as blank code lines).
            let mut code = String::new();
            let mut j = i + 1;
            while j < lines.len() {
                if let Some(c) = lines[j].strip_prefix("//   ") {
                    code.push_str(c);
                    code.push('\n');
                } else if lines[j].trim_end() == "//" {
                    code.push('\n');
                } else {
                    break;
                }
                j += 1;
            }
            let skip = flags.contains("NORENDER")
                || flags.contains("Anim")
                || flags.contains("Spin") // FlatSpin/Spin animate via $t
                || code.trim().is_empty()
                || code.contains("$t"); // any other $t use → animated
            if !skip {
                out.push(Example {
                    origin: format!("{stem}.scad:{} — {title}", i + 1),
                    file_stem: stem.clone(),
                    code,
                    is_2d: flags.contains("2D"),
                });
            }
            i = j;
        }
    }
    out
}

/// Wrap an example in its includes + (for 2D) a unit extrude so the boolean-residual differential can compare
/// it as a solid. `std` + the origin file covers both the common case and the specialized modules (gears,
/// screws, …) that aren't in std; re-including a std member is harmless.
fn wrap(ex: &Example) -> String {
    let body = if ex.is_2d {
        format!("linear_extrude(1) {{\n{}\n}}", ex.code)
    } else {
        ex.code.clone()
    };
    format!(
        "include <BOSL2/std.scad>\ninclude <BOSL2/{}.scad>\n$fn=32;\n{body}\n",
        ex.file_stem
    )
}

#[test]
#[ignore = "minutes-long BOSL2 example sweep; run explicitly with --ignored"]
fn bosl2_examples_vs_oracle() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bosl2 = manifest.join("libs/BOSL2");
    if !bosl2.join("std.scad").exists() {
        eprintln!("note: libs/BOSL2 not checked out — example corpus skipped");
        return;
    }
    if find_bin().is_none() {
        eprintln!(
            "note: OpenSCAD not found — example corpus skipped (it's an oracle differential)"
        );
        return;
    }
    let libs = vec![manifest.join("libs")];
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bosl2_examples");
    std::fs::create_dir_all(&tmp).unwrap();

    let examples = extract_examples(&bosl2);
    eprintln!(
        "=== extracted {} BOSL2 examples ({} 2D) — comparing vs the oracle, {CONCURRENCY}-wide ===",
        examples.len(),
        examples.iter().filter(|e| e.is_2d).count()
    );

    // AGREE / DIVERGE(reason) / TIMEOUT — per example, filled by the pool.
    let slots: Vec<Mutex<Option<Result<(), String>>>> =
        examples.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = examples.len();
    std::thread::scope(|s| {
        for _ in 0..CONCURRENCY {
            std::thread::Builder::new()
                .stack_size(fab_scad::EVAL_STACK) // deep eval-assembly host recursion, like the models harness
                .spawn_scoped(s, || {
                    loop {
                        let idx = next.fetch_add(1, Ordering::Relaxed);
                        if idx >= examples.len() {
                            break;
                        }
                        let ex = &examples[idx];
                        let path = tmp.join(format!("{idx}.scad"));
                        std::fs::write(&path, wrap(ex)).unwrap();
                        let outcome = compare_with_timeout(&path, &libs, BUDGET);
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Err(why) = &outcome {
                            eprintln!(
                                "  [{n:>4}/{total}] DIVERGE {} :: {}",
                                ex.origin,
                                first_line(why)
                            );
                        }
                        *slots[idx].lock().unwrap() = Some(outcome);
                    }
                })
                .unwrap();
        }
    });

    let results: Vec<(&Example, Result<(), String>)> = examples
        .iter()
        .zip(slots)
        .map(|(ex, slot)| (ex, slot.into_inner().unwrap().expect("every slot filled")))
        .collect();

    let agree = results.iter().filter(|(_, r)| r.is_ok()).count();
    let diverge = total - agree;
    eprintln!(
        "\n=== BOSL2 examples: {agree}/{total} agree with the oracle ({diverge} diverge) ==="
    );

    // Bucket divergences by their normalized first line (numbers stripped) so a systemic cause clusters.
    let mut by_reason: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (ex, r) in &results {
        if let Err(why) = r {
            by_reason
                .entry(normalize(first_line(why)))
                .or_default()
                .push(&ex.origin);
        }
    }
    eprintln!("\n=== divergence buckets (most common first) ===");
    let mut buckets: Vec<(&String, &Vec<&str>)> = by_reason.iter().collect();
    buckets.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (reason, exs) in buckets.iter().take(30) {
        eprintln!("  [{}×] {reason}", exs.len());
        for e in exs.iter().take(4) {
            eprintln!("        {e}");
        }
        if exs.len() > 4 {
            eprintln!("        … +{} more", exs.len() - 4);
        }
    }

    assert!(
        agree > 0,
        "no BOSL2 examples agreed — the harness is broken"
    );
}

/// Compare on a fresh big-stack thread with a wall-clock budget; a leaked thread on timeout dies at process
/// exit. `diff_files` runs scad-rs in-process + the oracle as a subprocess.
fn compare_with_timeout(path: &Path, libs: &[PathBuf], budget: Duration) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (p, l) = (path.to_path_buf(), libs.to_vec());
    std::thread::Builder::new()
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(move || {
            let _ = tx.send(fab_scad::differ::diff_files(&p, &l));
        })
        .unwrap();
    match rx.recv_timeout(budget) {
        Ok(r) => r,
        Err(_) => Err(format!("TIMEOUT (>{}s)", budget.as_secs())),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default()
}

/// Strip volatile bits (numbers, paths) so divergence reasons cluster: `genus 3 vs 5 (vol …)` → `genus N vs N`.
fn normalize(reason: &str) -> String {
    // Drop the trailing "(vol … bbox …)" detail and collapse digit runs to N.
    let head = reason.split(" (vol ").next().unwrap_or(reason);
    let head = head.rsplit(':').next().unwrap_or(head).trim(); // drop the leading "path: driver vs driver:"
    let mut out = String::new();
    let mut prev_digit = false;
    for c in head.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' {
            if !prev_digit {
                out.push('N');
            }
            prev_digit = true;
        } else {
            out.push(c);
            prev_digit = false;
        }
    }
    out
}
