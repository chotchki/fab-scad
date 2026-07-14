//! Regression guard for the window_light_blocker cavity-pull-out bug (W.4).
//!
//! The whole model is one huge sheet (913 × 1370 mm) riddled with 88 fully-enclosed magnet pockets.
//! `Solid::components()` used to hand-roll a union-find over the exported mesh; it over-segmented on
//! the coincident-but-distinct verts Manifold leaves along boolean seams, rebuilt OPEN shells, and
//! SILENTLY dropped every NotManifold fragment — so the whole model came back as ZERO components. In
//! the GUI that collapsed to a single un-decomposable plate with the magnet pockets extracted. Native
//! `Decompose()` folds every void back into its body → ONE connected piece, pockets carved.
//!
//! Needs `libs/BOSL2` (a git submodule); skips (does not fail) when it isn't checked out.

use fab_scad::backend::{ManifoldBackend, build_geo};
use std::path::PathBuf;

#[test]
fn whole_model_is_one_connected_body_with_pockets_intact() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if !manifest.join("libs/BOSL2/std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let model = manifest.join("models/window_light_blocker/window_light_blocker.scad");
    let libs = vec![manifest.join("libs"), manifest.join("scad-lib")];
    let geo = fab_scad::import::resolve_geometry_file(&model, &libs, fab_lang::Config::from_env())
        .expect("render the whole model");
    let solid = build_geo(&geo, &ManifoldBackend).expect("non-empty geometry");
    assert!(
        solid.is_manifold(),
        "the rendered whole must be a valid 2-manifold"
    );

    let comps = solid.components();
    // The bug returned 0; the whole sheet is one connected body (the pockets are voids, not pieces).
    assert_eq!(
        comps.len(),
        1,
        "one connected body, not zero (or 88 pulled-out pockets)"
    );
    comps[0]
        .check()
        .expect("the component is a valid 2-manifold");
    // The piece keeps every pocket carved — same volume as the whole (voids intact, not filled in).
    assert!(
        (comps[0].volume() - solid.volume()).abs() < 1.0,
        "pockets stay carved: comp {} vs whole {}",
        comps[0].volume(),
        solid.volume()
    );
}
