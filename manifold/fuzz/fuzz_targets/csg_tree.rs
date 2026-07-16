//! M.1.5 — the structure-aware CSG-tree fuzzer: up to 100 continuously-transformed unit cubes,
//! fold-unioned, `strictly` (manifold + finite + Euler parity) asserted after EVERY op. Continuous
//! transforms per the GATE-B design note — exact coplanarity is measure-zero, so hits here are real
//! robustness findings, not R2-class grid artifacts.
#![no_main]

use arbitrary::Arbitrary;
use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::check::{KernelParams, intermediate_check};
use fab_manifold::linalg::{Mat3x4, Vec3};
use fab_manifold::mesh::Mesh;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Xform {
    t: [f32; 3],
    r: [f32; 3],
    s: f32,
}

#[derive(Arbitrary, Debug)]
struct Tree {
    cubes: Vec<Xform>,
}

/// Fold an arbitrary finite f32 into `[lo, hi)` — uniformity doesn't matter for fuzz, finiteness
/// and determinism do.
fn clamp(v: f32, lo: f64, hi: f64) -> f64 {
    if v.is_finite() {
        lo + (hi - lo) * (f64::from(v).abs() % 1.0)
    } else {
        lo
    }
}

fuzz_target!(|tree: Tree| {
    let mut acc: Option<Mesh> = None;
    for x in tree.cubes.iter().take(100) {
        let rot = Mat3x4::rotate(
            clamp(x.r[0], 0.0, 360.0),
            clamp(x.r[1], 0.0, 360.0),
            clamp(x.r[2], 0.0, 360.0),
        );
        let scale = Mat3x4::scale(Vec3::splat(clamp(x.s, 0.25, 4.0)));
        let trans = Mat3x4::translate(Vec3::new(
            clamp(x.t[0], -2.0, 2.0),
            clamp(x.t[1], -2.0, 2.0),
            clamp(x.t[2], -2.0, 2.0),
        ));
        let cube = Mesh::cube(Vec3::new(1.0, 1.0, 1.0), true).expect("unit cube");
        let c = cube
            .transform(rot)
            .and_then(|m| m.transform(scale))
            .and_then(|m| m.transform(trans))
            .expect("clamped finite transforms");

        acc = Some(match acc {
            None => c,
            Some(prev) => {
                let mut u = boolean(&prev, &c, OpType::Add);
                intermediate_check(
                    &u,
                    KernelParams {
                        intermediate_checks: true,
                    },
                );
                u.set_epsilon(-1.0, false);
                u.initialize_original();
                u.set_normals_and_coplanar();
                u
            }
        });
    }
});
