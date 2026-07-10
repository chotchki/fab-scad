//! Seed the `eval` + `jit_diff` fuzz corpora from BOSL2: parse every `libs/BOSL2/*.scad` file and slice
//! each top-level `function` def (by its AST span) into a one-def corpus file. Run from the repo root:
//!   cargo run -p fab-lang --example seed_fuzz_from_bosl2
//!
//! Why per-DEF, not per-file: `jit_diff` requires `stmts.first()` to BE a function def, and a whole BOSL2
//! file leads with its include-guard assignment — so a per-def seed is what actually reaches the JIT. It's
//! also dense, real mutation fodder for `eval`. The nightly `fuzz.yml` runs this (with the BOSL2 submodule
//! checked out) before fuzzing, so the campaign always starts from the ~1300 real BOSL2 numeric/list bodies
//! rather than from raw bytes — on the first local run it took `jit_diff` coverage from ~1k to ~13k.
//! Idempotent: re-running overwrites the `bosl2_*` seeds and leaves libFuzzer's own corpus units alone.

use std::fs;
use std::path::Path;

use fab_lang::{StmtKind, parse};

fn main() {
    let bosl2 = Path::new("libs/BOSL2");
    let out_eval = Path::new("lang/fuzz/corpus/eval");
    let out_jit = Path::new("lang/fuzz/corpus/jit_diff");
    fs::create_dir_all(out_eval).expect("create corpus/eval");
    fs::create_dir_all(out_jit).expect("create corpus/jit_diff");

    let (mut files, mut defs) = (0u32, 0u32);
    for entry in fs::read_dir(bosl2).expect("read libs/BOSL2 (submodule checked out?)") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "scad") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else { continue };
        let Ok(prog) = parse(&src) else { continue };
        files += 1;
        let stem = path.file_stem().expect("file stem").to_string_lossy();
        for (i, stmt) in prog.stmts.iter().enumerate() {
            if matches!(stmt.kind, StmtKind::FunctionDef { .. }) {
                let snippet = &src[stmt.span.clone()];
                let name = format!("bosl2_{stem}_{i}");
                fs::write(out_jit.join(&name), snippet).expect("write jit_diff seed");
                fs::write(out_eval.join(&name), snippet).expect("write eval seed");
                defs += 1;
            }
        }
    }
    eprintln!("seeded {defs} function defs from {files} BOSL2 files → corpus/eval + corpus/jit_diff");
}
