//! Native binary entry point. The app itself lives in the `fab_gui` library (`lib.rs`) so the SAME
//! code links into both this desktop bin and the wasm `cdylib` (W.3 — one codebase, desktop + web).

fn main() {
    fab_gui::native_entry();
}
