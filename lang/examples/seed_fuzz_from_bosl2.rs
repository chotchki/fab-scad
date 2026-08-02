//! Seed the `eval` fuzz corpus from BOSL2: parse every `libs/BOSL2/*.scad` file and slice each top-level
//! `function` def (by its AST span) into a one-def corpus file. Run from the repo root:
//!   cargo run -p fab-lang --example `seed_fuzz_from_bosl2`
//!
//! Why per-DEF, not per-file: a whole BOSL2 file leads with its include-guard assignment and drags in its
//! own `include<>` graph, so a per-def slice is dense, self-contained mutation fodder where a per-file one
//! is mostly preamble. The nightly `fuzz.yml` runs this (with the BOSL2 submodule checked out) before
//! fuzzing, so the campaign always starts from the ~1300 real BOSL2 numeric/list bodies rather than from
//! raw bytes. It used to seed `jit_diff` too; that target died with the JIT at AR.21.1.
//! Idempotent: re-running overwrites the `bosl2_*` seeds and leaves libFuzzer's own corpus units alone.

use std::fs;
use std::path::Path;

use fab_lang::{StmtKind, parse};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bosl2 = Path::new("libs/BOSL2");
    let out_eval = Path::new("lang/fuzz/corpus/eval");
    fs::create_dir_all(out_eval)?;

    let (mut files, mut defs) = (0u32, 0u32);
    for entry in
        fs::read_dir(bosl2).map_err(|e| format!("read libs/BOSL2 (submodule checked out?): {e}"))?
    {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "scad") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(prog) = parse(&src) else { continue };
        files += 1;
        let stem = path
            .file_stem()
            .ok_or("a .scad path always has a file stem")?
            .to_string_lossy();
        for (i, stmt) in prog.stmts.iter().enumerate() {
            if matches!(stmt.kind, StmtKind::FunctionDef { .. }) {
                let snippet = &src[stmt.span.clone()];
                let name = format!("bosl2_{stem}_{i}");
                fs::write(out_eval.join(&name), snippet)?;
                defs += 1;
            }
        }
    }
    eprintln!("seeded {defs} function defs from {files} BOSL2 files → corpus/eval");
    Ok(())
}
