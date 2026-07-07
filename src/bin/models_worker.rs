//! The models-harness isolation worker (L.3). Args: `<model.scad> [lib_dir ...]`.
//!
//! Evaluates ONE model file — its `include`/`use` graph AND its `import()`/`surface()` meshes (via the
//! fab-scad mesh reader, same as the differential's fab driver) — and prints a single stdout line:
//!   `OK\t<ms>`            — rendered to a geometry tree in `<ms>` milliseconds
//!   `ERR\t<ms>\t<detail>` — the evaluator rejected it (unknown module, assert, parse/load error)
//! A HANG prints nothing: the parent harness times out and kills this process, reclaiming the core — the
//! scad-rs interpreter is slow enough on heavy BOSL2 geometry that ~half the real models blow a 10 s budget,
//! and killing beats leaking a host thread per timeout. A stack overflow aborts ONLY this process.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let model =
        PathBuf::from(args.next().expect("usage: models_worker <model.scad> [lib_dir ...]"));
    let libs: Vec<PathBuf> = args.map(PathBuf::from).collect();

    // Eval on a 1 GiB stack: the evaluator is heap-bounded for RECURSION, but dropping a deeply-nested
    // CSG/value tree still walks the host stack past the default 8 MiB (a real model overflowed it), and a
    // stack overflow is an uncatchable process abort — give it the headroom the parent's watchdog assumes.
    let out = std::thread::Builder::new()
        .name("model-eval".into())
        .stack_size(1 << 30)
        .spawn(move || {
            let start = std::time::Instant::now();
            let res = fab_scad::import::resolve_geometry_file(&model, &libs);
            let ms = start.elapsed().as_millis();
            match res {
                Ok(_) => format!("OK\t{ms}"),
                Err(e) => format!(
                    "ERR\t{ms}\t{}",
                    format!("{e}").lines().next().unwrap_or_default()
                ),
            }
        })
        .expect("spawn eval thread")
        .join()
        .unwrap_or_else(|_| "ERR\t0\tpanicked".to_string());

    println!("{out}");
}
