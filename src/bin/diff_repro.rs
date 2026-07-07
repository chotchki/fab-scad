//! One-file differential repro (L.3 divergence hunt). Args: `<file.scad> [lib_dir ...]`.
//!
//! Runs ONE `.scad` through both scad-rs and the OpenSCAD oracle and prints the verdict — AGREE, or the
//! divergence detail (genus X vs Y / boolean residual / shape-class mismatch). The debug counterpart to the
//! models harness's one-line-per-model roster: when the sweep flags a model DIVERGE, reduce it to a minimal
//! snippet and run it here to see exactly how the meshes differ. A dev tool, not user-facing.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = PathBuf::from(args.next().expect("usage: diff_repro <file.scad> [lib_dir ...]"));
    let libs: Vec<PathBuf> = args.map(PathBuf::from).collect();

    // Big stack (deep-tree Drop overflows the default 8 MiB) — same reason the models harness evals on 1 GiB.
    let verdict = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || match fab_scad::differ::diff_files(&file, &libs) {
            Ok(()) => "AGREE".to_string(),
            Err(why) => format!("DIVERGE: {why}"),
        })
        .expect("spawn")
        .join()
        .unwrap_or_else(|_| "PANIC".to_string());

    println!("{verdict}");
}
