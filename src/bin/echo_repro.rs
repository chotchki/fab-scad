//! Echo dump (L.3.4 divergence hunt). Args: `<file.scad> [lib_dir ...]`.
//!
//! Evaluates a `.scad` through scad-rs and prints its `echo`/warning console output — the counterpart to
//! `diff_repro` for VALUE-level debugging. When a shape renders empty (a VNF that came out with no faces),
//! reconstruct its construction with `echo()` on the intermediates and run it here vs `OpenSCAD -o /dev/null`
//! to see WHICH value first diverges. No mesh reader (pure-geometry snippets only). A dev tool.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = PathBuf::from(
        args.next()
            .expect("usage: echo_repro <file.scad> [lib_dir ...]"),
    );
    let libs: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let source = std::fs::read_to_string(&file).expect("read source");
    let base = file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    // Big stack: deep eval-assembly host recursion (see [`fab_scad::EVAL_STACK`]) — same as the other repro tools.
    let out = std::thread::Builder::new()
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(
            move || match fab_lang::evaluate_geometry_with_base_full(&source, &base, &libs) {
                Ok((_, messages)) => messages
                    .iter()
                    .map(fab_lang::Message::render)
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("EVAL ERROR: {e}"),
            },
        )
        .expect("spawn")
        .join()
        .unwrap_or_else(|_| "PANIC".to_string());

    println!("{out}");
}
