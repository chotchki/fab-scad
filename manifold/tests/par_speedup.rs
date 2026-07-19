//! W.6.2 — the threading ROI measurement (does `par` actually pay on real geometry?).
//!
//! The wasm geom worker ships THREADED now (release-web.yml), and the standing caution from the JIT
//! work is "measure before believing parallelism helps" — per-call speedups routinely fail to survive
//! contact with a real, geometry-dominated model. So this times the one thing threading touches: the
//! Manifold boolean on meshes big enough to cross the 10k seam threshold (Generic_Twin ~19k tris, the
//! self-intersect pair ~17k). Serial-vs-par is a COMPILE-TIME split, not a runtime toggle, so you run
//! this twice and diff the two numbers:
//!
//!   cargo test -p fab-manifold --test par_speedup --release -- --ignored --nocapture           # serial
//!   cargo test -p fab-manifold --test par_speedup --release --features par -- --ignored --nocapture  # par
//!
//! `#[ignore]` keeps it out of the correctness gate — it's a stopwatch, not an assertion. It prints
//! `par_live=<bool>` + the rayon thread count so the two runs are unambiguous, and reports MIN over the
//! sample (the least-noisy signal — the fastest run is the one least perturbed by the OS).
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::mesh::{Mesh, MeshGl};

fn models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("goldens")
        .join("models")
}

/// Minimal OBJ reader (v + tri f), mirrors the one in m7_golden_mode.rs — kept local so this bench
/// doesn't reach into another test file's private helpers.
fn load_obj(name: &str) -> Mesh {
    let path = models_dir().join(format!("{name}.obj"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut verts: Vec<f64> = Vec::new();
    let mut tris: Vec<u32> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                for _ in 0..3 {
                    verts.push(it.next().expect("v x y z").parse().expect("f64"));
                }
            }
            Some("f") => {
                for _ in 0..3 {
                    let tok = it.next().expect("f a b c");
                    let idx: u32 = tok.split('/').next().unwrap().parse().expect("index");
                    tris.push(idx - 1);
                }
            }
            _ => {}
        }
    }
    let mgl = MeshGl {
        num_prop: 3,
        vert_properties: verts,
        tri_verts: tris,
        ..Default::default()
    };
    Mesh::from_mesh_gl(&mgl).unwrap()
}

/// Time `boolean(a, b, op)` over `iters` runs (after one warmup) and return the MIN duration.
fn time_boolean(a: &Mesh, b: &Mesh, op: OpType, iters: u32) -> Duration {
    let _ = boolean(a, b, op); // warm the allocator / caches
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let out = boolean(a, b, op);
        let e = t.elapsed();
        std::hint::black_box(&out); // don't let the result get optimized away
        best = best.min(e);
    }
    best
}

#[test]
#[ignore = "timing, not correctness — run explicitly with --release -- --ignored --nocapture"]
fn par_speedup_heavy_booleans() {
    // Under par_live rayon's default pool sizes to available_parallelism; serial is a single thread.
    let threads = if cfg!(par_live) {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
        1
    };

    println!("\n=== par_speedup: par_live={} threads={threads} ===", cfg!(par_live));

    // Heavy pairs that cross the 10k seam threshold — the only regime where threading can pay.
    let cases: &[(&str, &str, &str, OpType)] = &[
        (
            "Generic_Twin_7081 ∪",
            "Generic_Twin_7081.1.t0_left",
            "Generic_Twin_7081.1.t0_right",
            OpType::Add,
        ),
        (
            "Generic_Twin_7081 −",
            "Generic_Twin_7081.1.t0_left",
            "Generic_Twin_7081.1.t0_right",
            OpType::Subtract,
        ),
        (
            "self_intersect ∩",
            "self_intersectA",
            "self_intersectB",
            OpType::Intersect,
        ),
    ];

    for (label, an, bn, op) in cases {
        let a = load_obj(an);
        let b = load_obj(bn);
        let best = time_boolean(&a, &b, *op, 12);
        println!("{label:<22} min {:>8.2} ms/op", best.as_secs_f64() * 1e3);
    }
    println!();
}
