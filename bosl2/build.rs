//! AR.26.3 — transpile BOSL2 at BUILD time, into `OUT_DIR`.
//!
//! The whole point of the phase in one file: the generated Rust never enters version control, so
//! there is nothing to keep current and no regen gate to run. A BOSL2 bump changes the submodule
//! pointer and the next build re-transpiles; the diff worth reviewing is the SUBMODULE's, and the
//! regression that actually matters — functions falling out of coverage — is caught by the ratchet,
//! which is a test rather than an artifact.
//!
//! AR.37 moved the 200 lines that used to live here into [`fab_lib::build`], because MCAD proved
//! they were generic: a library crate is now the four fields below.

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let root = manifest
        .parent()
        .expect("the crate has a parent directory")
        .join("libs/BOSL2");
    fab_lib::build::transpile(&fab_lib::build::Library {
        name: "fab-bosl2",
        root: &root,
        // BOSL2's own umbrella include — present in every checkout, absent in none.
        sentinel: "std.scad",
        out_file: "bosl2.rs",
    });
}
