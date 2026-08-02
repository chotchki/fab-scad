//! AR.37 — point the transpiler at an ARBITRARY library directory and report what it covers.
//!
//!   cargo run -p fab-lib --example probe_library -- <dir> [<dir>...]
//!
//! The generality question in one command. Every number the transpiler quotes came from BOSL2,
//! which it was built against for a whole phase — so "99.5% of BOSL2" says nothing about whether
//! the emitter's subset is a general OpenSCAD subset or a BOSL2-shaped one. This aims the same
//! `Library::read` -> `function_band` -> `generate_standalone_modules` pipeline the real build
//! script runs at any directory of `.scad` files.
//!
//! READ THE NUMBER CORRECTLY: this reports what the emitter EMITTED, not what compiles. Those are
//! different claims and the gap between them is where the bugs live — MCAD emitted 39 of 39
//! functions and then failed to build with 20 errors, every one of them the single thing BOSL2
//! could never have shown (a leading digit is a legal OpenSCAD identifier and an illegal Rust
//! one). A coverage number is a lie until the band compiles; this tool gets the first half, a
//! crate like `fab-bosl2` gets the second.
//!
//! Declines are grouped by leading cause so a library's shape reads as a histogram rather than a
//! wall — one construct blocking forty functions means something very different from forty
//! separate one-offs.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    if dirs.is_empty() {
        eprintln!("usage: probe_library <dir> [<dir>...]  (each a directory of .scad files)");
        std::process::exit(2);
    }
    for arg in dirs {
        probe(&arg);
    }
}

fn probe(arg: &str) {
    let root = PathBuf::from(arg);
    let name = root
        .file_name()
        .map_or_else(|| arg.to_string(), |n| n.to_string_lossy().into());

    let lib = match fab_lib::library::Library::read(&root) {
        Ok(l) => l,
        Err(e) => {
            println!("{name}: does not read — {e}");
            return;
        }
    };
    // An unparsed file is a SILENT coverage hole — it shrinks every number below while nothing
    // fails — so it prints before them rather than after.
    for (file, why) in &lib.unparsed {
        println!("{name}: {file} does not parse ({why}) — its declarations are absent");
    }

    match fab_lib::emit::function_band(&lib) {
        Err(e) => println!("{name}: the function band does not compute — {e}"),
        Ok(band) => {
            let n = lib.functions.len();
            println!(
                "{name}: {}/{n} functions ({}), {} collisions, {} fixpoint rounds",
                band.subjects.len(),
                pct(band.subjects.len(), n),
                lib.collisions.len(),
                band.rounds
            );
            let mut why: BTreeMap<String, usize> = BTreeMap::new();
            for (_, reason) in &band.declined {
                let head: String = reason
                    .split(':')
                    .next()
                    .unwrap_or(reason)
                    .chars()
                    .take(58)
                    .collect();
                *why.entry(head).or_default() += 1;
            }
            let mut rows: Vec<_> = why.into_iter().collect();
            rows.sort_by_key(|(reason, count)| (std::cmp::Reverse(*count), reason.clone()));
            for (reason, count) in rows.iter().take(10) {
                println!("    declined {count:4}  {reason}");
            }
        }
    }

    match fab_lib::emit::generate_standalone_modules(&root) {
        Err(e) => println!("    modules: the band does not emit — {e}"),
        Ok(files) => {
            // The spine (`mod.rs`) declares the others and defines nothing, so it counts in
            // neither total — including it would inflate the file count and contribute no natives.
            let emitted: usize = files
                .iter()
                .filter(|(n, _)| n != "mod.rs")
                .map(|(_, text)| text.matches("pub(super) fn ").count())
                .sum();
            let m = lib.modules.len();
            println!(
                "    modules: {emitted}/{m} emitted ({}) across {} files",
                pct(emitted, m),
                files.len().saturating_sub(1)
            );
        }
    }
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "n/a".into();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a library declaring more than 2^53 names is not the failure mode here"
    )]
    let p = part as f64 * 100.0 / whole as f64;
    format!("{p:.1}%")
}
