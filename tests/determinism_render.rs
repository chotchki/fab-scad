//! S.4 REGRESSION GUARD — the pure-Rust `par` kernel must mesh a complex non-convex model
//! bit-identically run-to-run, same process, same platform.
//!
//! Determinism is by CONSTRUCTION (fab_manifold::par's CommutativeAssociative gate + total-order
//! sorts + SortGeometry canonicalization), and the manifold crate's goldens prove par == serial. This
//! end-to-end byte check is the belt: it catches a future edit that opens a SECOND parallelism door
//! the compile-time gate can't see (e.g. a raw rayon reduce, or a parallelized decompose() feeding a
//! non-total-order sort). Resolve the geo tree ONCE (eval is deterministic), then `build_geo` it N
//! times through the par ManifoldBackend and compare STL hashes.
//!
//! Same-process is deliberate — the global MESH_ID_COUNTER climbs across all N runs, so identical
//! output proves the provenance ids are normalized away (never emitted), not merely reset per process.
//!
//! Skips clean when the `models` submodule isn't checked out (CI without it), so it never fails for
//! absence — only for actual nondeterminism.
#![cfg(all(not(target_arch = "wasm32"), feature = "kernel"))]

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use fab_scad::backend::{ManifoldBackend, build_geo};
use fab_scad::import::resolve_geometry_file;

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// Non-convex, boolean-heavy real models — the S.4 class. garage_door draws from `rands()` (the one
/// impure builtin) and splits multipart; window_light_blocker is 1 body + 88 fully-enclosed pockets
/// (the W.4 components() cavity case). Both exercise the decompose()/components() sort under par.
const MODELS: &[&str] = &[
    "models/garage_door/garage_door.scad",
    "models/window_light_blocker/window_light_blocker.scad",
];

#[test]
fn par_kernel_render_is_bit_identical_run_to_run() {
    let libs = vec![PathBuf::from("libs/BOSL2"), PathBuf::from("libs")];
    const N: usize = 3;
    let mut ran = 0usize;
    let mut nondet: Vec<String> = Vec::new();

    for m in MODELS {
        let model = Path::new(m);
        if !model.exists() {
            eprintln!("SKIP {m}: not present (models submodule not checked out)");
            continue;
        }
        let geo = match resolve_geometry_file(model, &libs, fab_lang::Config::from_env()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP {m}: resolve failed: {e}");
                continue;
            }
        };
        let mut counts: BTreeMap<u64, usize> = BTreeMap::new();
        let mut bytes = 0usize;
        for _ in 0..N {
            let Some(solid) = build_geo(&geo, &ManifoldBackend) else {
                eprintln!("SKIP {m}: empty solid");
                counts.clear();
                break;
            };
            let stl = solid.to_stl_bytes();
            bytes = stl.len();
            *counts.entry(hash_bytes(&stl)).or_default() += 1;
        }
        if counts.is_empty() {
            continue;
        }
        ran += 1;
        let distinct = counts.len();
        eprintln!(
            "{} {m}: {distinct} distinct hash / {N} runs ({bytes} bytes)",
            if distinct == 1 { "OK  " } else { "FAIL" }
        );
        if distinct != 1 {
            nondet.push(format!("{m} ({distinct} distinct over {N})"));
        }
    }

    // No models present ⇒ nothing to prove, pass clean. Any that DID render must be deterministic.
    if ran == 0 {
        eprintln!("no models present — determinism guard is a no-op this run");
    }
    assert!(
        nondet.is_empty(),
        "par kernel is NON-deterministic run-to-run on: {nondet:?} — S.4 has regressed"
    );
}
