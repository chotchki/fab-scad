//! One-file differential repro (L.3 divergence hunt). Args: `<file.scad> [lib_dir ...]`.
//!
//! Runs ONE `.scad` through both scad-rs and the OpenSCAD oracle and prints the verdict — AGREE, or the
//! divergence detail (genus X vs Y / boolean residual / shape-class mismatch). The debug counterpart to the
//! models harness's one-line-per-model roster: when the sweep flags a model DIVERGE, reduce it to a minimal
//! snippet and run it here to see exactly how the meshes differ. A dev tool, not user-facing.

use fab_scad::differ::{Outcome, diff_files, drivers};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = PathBuf::from(
        args.next()
            .expect("usage: diff_repro <file.scad> [lib_dir ...]"),
    );
    let libs: Vec<PathBuf> = args.map(PathBuf::from).collect();

    // Big stack: deep eval-assembly host recursion (see [`fab_scad::EVAL_STACK`]) — Drop is iterative now (M.1).
    let out = std::thread::Builder::new()
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(move || {
            // Per-engine volume + genus first (which direction the divergence goes — removing too much vs too
            // little), then the verdict.
            let mut lines: Vec<String> = drivers()
                .iter()
                .map(|d| format!("  {:8} {}", d.name(), describe(d.eval_file(&file, &libs))))
                .collect();
            lines.push(match diff_files(&file, &libs) {
                Ok(()) => "AGREE".to_string(),
                Err(why) => format!("DIVERGE: {why}"),
            });
            lines.join("\n")
        })
        .expect("spawn")
        .join()
        .unwrap_or_else(|_| "PANIC".to_string());

    println!("{out}");
}

fn describe(o: Outcome) -> String {
    match o {
        Outcome::Solid(s) => {
            let bb = s.bbox().map_or_else(
                || "bbox=?".to_string(),
                |(lo, hi)| {
                    let (l, h) = (lo.to_array(), hi.to_array());
                    format!(
                        "bbox=[{:.2}x{:.2}x{:.2}]",
                        h[0] - l[0],
                        h[1] - l[1],
                        h[2] - l[2]
                    )
                },
            );
            format!("solid vol={:.1} genus={} {bb}", s.volume(), s.genus())
        }
        Outcome::Empty => "empty".to_string(),
        Outcome::Rejected => "rejected".to_string(),
    }
}
