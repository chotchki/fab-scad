//! SZ.4.3 — THE LEAN BUILD DROPS THE BAND AND KEEPS THE ANSWERS.
//!
//! The web worker ships in two variants. The FULL one carries the transpiled band (BOSL2's 1322
//! functions + MCAD's 39); the LEAN one carries the kernel and the evaluator and nothing else, and
//! the app picks between them from the model's own `include` lines. Measured, brotli: 5.4 MB
//! against 1.3 MB, because the band is 76% of that download.
//!
//! The reason a variant is SAFE is the whole premise of the compiled tier: a transpiled native is
//! bit-identical to interpreting its reference by construction, so dropping it changes speed and
//! nothing else. This asserts that premise where it is now load-bearing in a new way — two shipped
//! artifacts have to agree about geometry, and the only thing standing between them is a claim.
//!
//! WHY A LEAN BUILD MUST STILL RESOLVE BOSL2. Dropping the band drops the ROWS, not the SOURCE. A
//! lean worker still renders `include <BOSL2/std.scad>` — every call interprets. That distinction
//! is why `import::libraries()` declares source unconditionally: the first cut cfg-gated each entry
//! on its transpiled crate, which would have shipped a lean worker that could not resolve BOSL2 at
//! all, turning a speed difference into a missing part (and a missing import costs a PART, not an
//! error, so it would have been silent).
//!
//! Run the lean half with `--no-default-features --features kernel`; both halves run in CI.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn libs_dir() -> PathBuf {
    repo().join("libs")
}

fn have_bosl2() -> bool {
    libs_dir().join("BOSL2/std.scad").exists()
}

/// A program whose answer depends on real BOSL2 maths, so an interpreted run and a compiled run
/// have something to disagree about.
const PROGRAM: &str = "include <BOSL2/std.scad>\n\
     echo(vector_angle([1,0,0],[0,1,0]));\n\
     echo(unit([3,4,0]));\n\
     echo(is_path([[0,0],[1,0],[1,1]]));\n\
     cube(1);\n";

fn console() -> Vec<String> {
    let (_geo, msgs) = fab_scad::import::resolve_geometry_with_base_full(
        PROGRAM,
        repo(),
        &[libs_dir()],
        fab_lang::Config::default(),
    )
    .expect("renders");
    msgs.iter().map(fab_lang::Message::render).collect()
}

/// A LEAN build renders BOSL2 — the source is still there, every call just interprets. If this ever
/// fails, the source pack got tied back to the transpiled crates and a lean worker ships unable to
/// resolve the library it is supposed to interpret.
#[test]
fn a_lean_build_still_resolves_and_renders_bosl2() {
    if !have_bosl2() {
        return;
    }
    let out = console();
    assert!(
        out.iter().any(|m| m.contains("90")),
        "vector_angle of two perpendicular axes is 90 — the library did not resolve at all: {out:?}"
    );
}

/// BOSL2's source is declared regardless of which crates this build carries. The lean variant is
/// exactly the build where a cfg-gated source list would have silently come up short.
#[test]
fn bosl2_source_is_declared_even_without_its_transpiled_crate() {
    let declared: Vec<&str> = fab_scad::import::libraries()
        .iter()
        .map(|l| l.name)
        .collect();
    for want in ["BOSL2", "MCAD", "machineblocks"] {
        assert!(
            declared.contains(&want),
            "`{want}` source must be declared in every build — a lean worker interprets it and \
             therefore needs the .scad more, not less. Declared: {declared:?}"
        );
    }
}

/// THE BAND IS PRESENT ONLY WHEN THE FEATURE IS. Reads the registry rather than the feature flag, so
/// it catches a build whose feature is on and whose crate declared nothing (the empty-submodule
/// shape that shipped in every signed desktop release before SZ.1).
#[test]
fn the_band_tracks_its_feature() {
    if !have_bosl2() {
        return;
    }
    // Renders first so `wired_count()` reflects THIS program.
    let _ = console();
    let wired = fab_lang::wired_count();
    let base = fab_lang::surface::LibrarySurface::rows(&fab_lang::surface::Natives)
        .functions
        .len();

    if cfg!(feature = "libraries") {
        assert!(
            wired > base,
            "the `libraries` feature is ON but only {wired} natives armed, no more than fab-lang's \
             own {base} rows — the band is not reaching dispatch"
        );
    } else {
        assert!(
            wired <= base,
            "the `libraries` feature is OFF but {wired} natives armed against fab-lang's own \
             {base} rows — the lean build is carrying a band it is supposed to have dropped, so \
             the 4.1 MB it exists to save is still in the binary"
        );
    }
}
