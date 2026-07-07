//! Single-test repro for the BOSL2 corpus (L.2 burn-down). Where `corpus_worker` streams one-line
//! buckets for the whole sweep, this runs the ONE test you name and prints everything you need to
//! debug a divergence: the bucket, the FULL error (not just its first line), and the ordered
//! `echo`/warning console output (the clue when an `assert` fires — a BOSL2 test usually `echo`s the
//! `got`/`expected` right before the assert).
//!
//! Usage:
//!   cargo run --bin corpus_repro -- <name-substr> [--file <file-substr>] [--script] [--trace] [--all]
//!
//! `<name-substr>` matches the `[[test]]` name (substring, case-sensitive). `--file` further narrows to
//! `.scadtest` files whose basename contains the substring. `--script` dumps the scad source before
//! running it. `--trace` turns on the evaluator's `set -x` binding/assert trace (the `FAB_TRACE` gate) so
//! every `name = value` and assert outcome streams to stderr as it runs — the fastest way to see which
//! value went wrong right before a diverging `assert`. Without `--all`, only the first match runs (the
//! common case: one named test); `--all` runs every match. The BOSL2 dir defaults to
//! `$CARGO_MANIFEST_DIR/libs/BOSL2`; override with `$BOSL2_DIR`.

use std::path::PathBuf;

use fab_scad::corpus::{Bucket, enumerate_tests};

fn main() -> anyhow::Result<()> {
    let mut name_substr: Option<String> = None;
    let mut file_substr: Option<String> = None;
    let mut dump_script = false;
    let mut trace = false;
    let mut all = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--file" => file_substr = args.next(),
            "--script" => dump_script = true,
            "--trace" => trace = true,
            "--all" => all = true,
            other => name_substr = Some(other.to_string()),
        }
    }
    let Some(name_substr) = name_substr else {
        eprintln!(
            "usage: corpus_repro <name-substr> [--file <file-substr>] [--script] [--trace] [--all]"
        );
        std::process::exit(2);
    };

    // Turn on the evaluator's `set -x` trace BEFORE the first eval: the `FAB_TRACE` gate is read once
    // (process-cached `LazyLock`), so setting it here — single-threaded, before any eval or thread spawn —
    // is picked up by every binding/assert. SAFETY: no other thread exists yet at this point in `main`.
    if trace {
        unsafe { std::env::set_var("FAB_TRACE", "1") };
    }

    let bosl2_dir = std::env::var("BOSL2_DIR").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/BOSL2"),
        PathBuf::from,
    );
    let tests_dir = bosl2_dir.join("tests");
    let cases = enumerate_tests(&bosl2_dir)?;

    let matches: Vec<_> = cases
        .iter()
        .filter(|c| c.name.contains(&name_substr))
        .filter(|c| file_substr.as_ref().is_none_or(|f| c.file.contains(f)))
        .collect();

    if matches.is_empty() {
        eprintln!("no test matches name~{name_substr:?} file~{file_substr:?}");
        std::process::exit(1);
    }

    for case in matches.iter().take(if all { usize::MAX } else { 1 }) {
        println!("\n=== {}::{} ===", case.file, case.name);
        if case.expect_success {
            println!("(expect_success = true — a clean eval is the PASS)");
        } else {
            println!("(expect_success = false — an eval ERROR is the PASS; clean eval means the guard is missing)");
        }
        if dump_script {
            println!("--- script ---\n{}\n--- /script ---", case.script);
        }
        match fab_lang::evaluate_geometry_with_base_full(&case.script, &tests_dir, &[]) {
            Ok((_geo, messages)) => {
                println!("RESULT: evaluated cleanly (no error)");
                print_messages(&messages);
            }
            Err(e) => {
                let bucket = classify_display(&e);
                println!("RESULT: {bucket} — {e}");
            }
        }
    }
    Ok(())
}

/// Print the ordered console log so a diverging `assert`'s preceding `echo(got, expected)` is visible.
fn print_messages(messages: &[fab_lang::Message]) {
    if messages.is_empty() {
        println!("(no console output)");
        return;
    }
    println!("--- console ({} lines) ---", messages.len());
    for m in messages {
        println!("  {}", m.render());
    }
}

/// A human label for the error's bucket — same taxonomy as the sweep, so a repro's verdict lines up
/// with the roster. (The FULL error text prints alongside, unlike the sweep's first-line-only detail.)
fn classify_display(e: &fab_lang::Error) -> &'static str {
    use fab_lang::Error;
    match e {
        Error::Parse(_) => Bucket::Parse.label(),
        Error::Eval(m) if m.starts_with("assertion failed") => Bucket::Assertion.label(),
        Error::Eval(_) => Bucket::Eval.label(),
        Error::Load(_) => Bucket::Load.label(),
        Error::Lower(_) => Bucket::Lower.label(),
        Error::Unimplemented(_) | Error::Unknown(_) => Bucket::Unimplemented.label(),
        _ => Bucket::Eval.label(),
    }
}
