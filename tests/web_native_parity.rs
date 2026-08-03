//! SZ.1 — THE WEB AND NATIVE PATHS EVALUATE THE SAME PROGRAM AGAINST THE SAME LIBRARIES.
//!
//! The browser renders through ONE door and one only: the geom worker takes `Source::Bytes` (`main`
//! plus the in-memory `{path: text}` pack the app fetched), which lands in
//! `fab_lang::resolve_geometry_from_sources_full`. Native renders take the file- and string-rooted
//! doors instead. Those are different functions, and this asserts they agree about which libraries
//! exist.
//!
//! WHY A TEST RATHER THAN A CODE REVIEW. The divergence is invisible everywhere a user or a normal
//! test would look: the GEOMETRY is identical either way, because a transpiled native is
//! bit-identical to interpreting its reference by construction. Only the speed differs, and speed is
//! not what a differential asserts. So the compiled tier can silently stop being reachable while
//! every mesh comparison in the tree still passes — which is exactly what happened, twice. AR.35
//! found three string-rooted doors sitting on fab-lang's own rows, fixed them, and missed this one
//! because it is the only door with no `_with_registry` sibling.
//!
//! It drives the REAL worker request (`handle_with_store` with `Source::Bytes`), not a test-only
//! shim, because a shim is a fourth door and the bug under test is that doors disagree.
//!
//! `wired_count()` is the observable — how many natives the last evaluation actually ARMED. It is a
//! process-global last-value, so each test evaluates and reads immediately. Compared as a FLOOR
//! against fab-lang's own row count rather than an exact number: the question is "did the library
//! band arm at all", and pinning a count would turn every legitimate coverage change into a failure
//! here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

#[cfg(feature = "libraries")]
use fab_lang::surface::LibrarySurface;
use fab_scad::geomsg::{Quality, Request, Response, Source};
use fab_scad::geomsvc::{SolidStore, handle_with_store};

/// A program that calls into BOSL2, small enough that a failure prints readably. `vector_angle` is a
/// real BOSL2 function with a real transpiled native, so an armed band dispatches it.
const BOSL2_PROGRAM: &str =
    "include <BOSL2/std.scad>\necho(vector_angle([1,0,0],[0,1,0]));\ncube(1);\n";

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn libs_dir() -> PathBuf {
    repo().join("libs")
}

fn have_bosl2() -> bool {
    libs_dir().join("BOSL2/std.scad").exists()
}

/// fab-lang's OWN function-row count — the floor a path must CLEAR to prove it received more than
/// the built-in registry. Read from the surface rather than hardcoded, so deleting a hand row
/// (AR.21.2 deleted three) cannot silently weaken the assertion into a tautology.
#[cfg(feature = "libraries")]
fn builtin_rows() -> usize {
    fab_lang::surface::Natives.rows().functions.len()
}

/// The pack the WEB app hands the worker: every BOSL2 file keyed `BOSL2/<name>`, exactly as
/// `src/bin/pack_libs.rs` writes it and `lib_fetch.rs` closes over it.
fn web_pack() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(libs_dir().join("BOSL2")).expect("libs/BOSL2 is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "scad") {
            let name = path
                .file_name()
                .expect("a file")
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&path).expect("BOSL2 file reads");
            out.push((format!("BOSL2/{name}"), bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Render `BOSL2_PROGRAM` the way the BROWSER does, through the real worker envelope.
fn render_web() -> Response {
    let mut store = SolidStore::new(0);
    handle_with_store(
        &mut store,
        Request::RenderWhole {
            source: Source::Bytes {
                main: BOSL2_PROGRAM.as_bytes().to_vec(),
                libs: web_pack(),
            },
            root: None,
            preview: false,
            quality: Quality::Draft,
        },
    )
}

/// Render it the way NATIVE does.
fn render_native() {
    let _ = fab_scad::import::resolve_geometry_with_base_full(
        BOSL2_PROGRAM,
        repo(),
        &[libs_dir()],
        fab_lang::Config::default(),
    )
    .expect("the native path renders a BOSL2 program");
}

/// THE CONTROL. If the native path ever stops arming the band, the web assertion below is measuring
/// nothing and would pass for the wrong reason.
///
/// Band-only: SZ.4 made a LEAN build (no `libraries` feature) a shipping configuration, and there
/// the correct answer is that nothing arms. The parity claim that survives both builds is
/// `both_paths_arm_the_same_number_of_natives`, which is the one that actually caught the bug.
#[test]
#[cfg(feature = "libraries")]
fn the_native_path_arms_the_transpiled_band() {
    if !have_bosl2() {
        return;
    }
    render_native();
    let wired = fab_lang::wired_count();
    assert!(
        wired > builtin_rows(),
        "native armed {wired} natives, no more than fab-lang's own {} rows — the product registry \
         is not reaching the evaluator at all",
        builtin_rows()
    );
}

/// THE BUG. `resolve_geometry_from_sources_full` hardcodes `Registry::builtin()`, so the browser
/// gets fab-lang's rows and none of BOSL2's 1322. Band-only, as above.
#[test]
#[cfg(feature = "libraries")]
fn the_web_path_arms_the_transpiled_band() {
    if !have_bosl2() {
        return;
    }
    let response = render_web();
    if let Response::Failed { error, line } = &response {
        panic!("the web path failed to render a BOSL2 program: {error} (line {line:?})");
    }
    let wired = fab_lang::wired_count();
    assert!(
        wired > builtin_rows(),
        "the WEB path armed {wired} natives, no more than fab-lang's own {} rows — the browser is \
         evaluating BOSL2 through the INTERPRETER while native takes the compiled tier. AR.1's \
         stated benefit is one tier everywhere INCLUDING wasm, and this is where it is lost.",
        builtin_rows()
    );
}

/// The two agree on the SIZE of the set, not merely that both are non-trivial — a floor alone would
/// pass if web armed 100 and native armed 1400.
#[test]
fn both_paths_arm_the_same_number_of_natives() {
    if !have_bosl2() {
        return;
    }
    render_native();
    let native = fab_lang::wired_count();

    let response = render_web();
    if let Response::Failed { error, line } = &response {
        panic!("the web path failed to render: {error} (line {line:?})");
    }
    let web = fab_lang::wired_count();

    assert_eq!(
        native, web,
        "native armed {native} natives and the web path armed {web} for the SAME program — the two \
         platforms are running different amounts of compiled code"
    );
}
