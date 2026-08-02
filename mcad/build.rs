//! AR.37 — transpile MCAD at BUILD time, into `OUT_DIR`. Same shape as fab-bosl2, which is the
//! point: proving the transpiler generalizes means a second library costs a path and a sentinel,
//! not a second build script.

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let root = manifest
        .parent()
        .expect("the crate has a parent directory")
        .join("libs/MCAD");
    fab_lib::build::transpile(&fab_lib::build::Library {
        name: "fab-mcad",
        root: &root,
        // MCAD has no umbrella include (a user includes the one file they want), so the sentinel
        // is just a file every checkout has. `constants.scad` is the oldest and least likely to
        // be renamed out from under us; the ratchet catches it if that ever stops being true.
        sentinel: "constants.scad",
        out_file: "mcad.rs",
    });
}
