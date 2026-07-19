//! Fuzz the GEOMETRY-LOWERING seam (Y.7): ANY input bytes → parse → eval → the dimension-tagged `Geo`
//! → lower through the REAL Manifold kernel → mesh/contours, and NEVER panic, hang, overflow, or trip
//! ASan. This is the eval→kernel integration nothing else fuzzes:
//!   - lang/fuzz `eval` stops at fab-lang's no-backend `mesh_of` (leaf tessellation only, no CSG);
//!   - manifold/fuzz `csg_tree`/`polygon` start from constructed kernel inputs, never from SCAD source.
//! Here the whole `build_geo`/`build_2d` dispatch runs — Union/Difference/Intersection/Hull/Minkowski/
//! Extrude/Resize/Color/Transform + 2D Polygon/booleans/Offset/Projection and the X.4 `Shape2D::Hull`→
//! `CrossSection::hull_of` path — over real booleans, so the kernel's 13 `get_unchecked` unsafe blocks
//! (boolean3/collider) get ASan on fuzzer geometry, and the empty-CSG algebra + GeoCache memo get walked.
//!
//! Hermetic + bounded: fuzzer source has no `include`, so no filesystem. `FAB_EVAL_BUDGET` caps eval like
//! the `eval` target (a runaway comprehension can't hang before it reaches the kernel); pair with a
//! libFuzzer `-timeout`/`-rss_limit_mb` in CI for the rare heavy Solid (a valid small program can still
//! ask for a huge `$fn`). Seed from the `eval`/`parse` corpora — the programs those campaigns found that
//! actually build geometry.

#![no_main]

use std::sync::Once;

use fab_lang::Geo;
use fab_scad::backend::{GeometryBackend, ManifoldBackend, build_2d, build_geo};
use libfuzzer_sys::fuzz_target;

static INIT: Once = Once::new();

fuzz_target!(|data: &[u8]| {
    // One-time: bound eval so a `[for(i=[0:9e9]) …]` can't hang before lowering (mirrors the eval target).
    INIT.call_once(|| {
        if std::env::var_os("FAB_EVAL_BUDGET").is_none() {
            // edition 2024: set once at startup, before any eval — no env race.
            unsafe { std::env::set_var("FAB_EVAL_BUDGET", "2000000") };
        }
    });
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // A parse/eval error is a typed Err, not a crash — only lower what evaluates to geometry.
    let Ok(geo) = fab_lang::evaluate_geometry(src) else {
        return;
    };
    match &geo {
        // 3D: GeoNode → Solid through the real kernel booleans, then extract the mesh (drives to_mesh's
        // indexed-vertex path too).
        Geo::D3(_) => {
            let solid = build_geo(&geo, &ManifoldBackend);
            let _ = ManifoldBackend.to_mesh(&solid);
        }
        // 2D: Shape2D → CrossSection (incl. the X.4 hull_2d), then extract contours.
        Geo::D2(shape) => {
            let region = build_2d(shape, &ManifoldBackend);
            let _ = ManifoldBackend.to_polygons(&region);
        }
    }
});
