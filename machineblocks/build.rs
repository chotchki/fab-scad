//! AR.37 — transpile machineblocks at BUILD time, into `OUT_DIR`. Third library, same four fields:
//! once [`fab_lib::build`] existed, adding one costs a path and a sentinel.

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    // The library proper, NOT the repo root: machineblocks ships 529 .scad files, and the ~500 under
    // examples/ and templates/ are generated part variants rather than library code. `lib/` is the
    // 16 files that declare anything.
    let root = manifest
        .parent()
        .expect("the crate has a parent directory")
        .join("libs/machineblocks/lib");
    fab_lib::build::transpile(&fab_lib::build::Library {
        name: "fab-machineblocks",
        root: &root,
        // No umbrella include here either — its own files `use <block.scad>` directly, which makes
        // block.scad the one every checkout has and the one a rename would be loudest about.
        sentinel: "block.scad",
        out_file: "machineblocks.rs",
    });
}
