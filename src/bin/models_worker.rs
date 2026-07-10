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
    let model = PathBuf::from(
        args.next()
            .expect("usage: models_worker <model.scad> [lib_dir ...]"),
    );
    let libs: Vec<PathBuf> = args.map(PathBuf::from).collect();

    // Eval on [`fab_scad::EVAL_STACK`]: as of M.3 geometry eval is HEAP-bounded (the explicit-stack driver —
    // no host recursion), so eval itself would be fine on the default stack; the reserve is now courtesy
    // headroom for the native render path. Still spawned on its own thread so a stack overflow anywhere aborts
    // only this worker and the parent harness reclaims the core.
    let out = std::thread::Builder::new()
        .name("model-eval".into())
        .stack_size(fab_scad::EVAL_STACK)
        .spawn(move || {
            let start = std::time::Instant::now();
            let res = fab_scad::import::resolve_geometry_file(
                &model,
                &libs,
                fab_lang::Config::from_env(),
            );
            let ms = start.elapsed().as_millis();
            match res {
                Ok(geo) => {
                    // A/B fingerprint (FAB_GEO_FINGERPRINT=1): a deterministic hash of the resolved geometry
                    // tree — the Debug form is stable (shortest round-trip f64), so cache-on == cache-off is a
                    // string-hash equality across two runs. Off by default; costs one Debug walk when on.
                    if std::env::var_os("FAB_GEO_FINGERPRINT").is_some() {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        format!("{geo:?}").hash(&mut h);
                        eprintln!("FINGERPRINT\t{:016x}", h.finish());
                    }
                    format!("OK\t{ms}")
                }
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
