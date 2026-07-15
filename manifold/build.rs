//! One job: emit the `par_live` cfg alias — "the rayon path is compiled here" — so the seven
//! dispatch sites in `par.rs` don't each repeat a three-clause cfg. The condition (M.6.1):
//!
//!   `par` feature AND (not wasm32, OR wasm32-unknown-unknown)
//!
//! Native gets rayon's OS-thread pool; browser wasm (`wasm32-unknown-unknown`) gets rayon over
//! wasm-bindgen-rayon's Web-Worker + SharedArrayBuffer pool (nightly `+atomics` build, app calls
//! `init_thread_pool` first); `wasm32-wasip1` — the wasmtime differential-test lane — stays serial
//! ALWAYS (no thread pool there, and the native==wasm golden wants the fixed serial path anyway).

fn main() {
    println!("cargo::rustc-check-cfg=cfg(par_live)");
    let par = std::env::var("CARGO_FEATURE_PAR").is_ok();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if par && (arch != "wasm32" || os == "unknown") {
        println!("cargo::rustc-cfg=par_live");
    }
}
