//! M.1.5 — `polygon_fuzz` for the ear clip (the one non-verbatim triangulator component): build a
//! simple star polygon BY CONSTRUCTION from arbitrary bytes (sorted distinct angles × bounded
//! radii ⇒ no self-intersection), triangulate, and hold the simple-polygon law: exactly n−2
//! triangles, every index a real corner.
#![no_main]

use arbitrary::Arbitrary;
use fab_manifold::boolean::polygon::{PolyVert, triangulate};
use fab_manifold::linalg::Vec2;
use fab_manifold::mathf;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Star {
    spokes: Vec<(u16, u16)>,
}

fuzz_target!(|star: Star| {
    // Distinct sorted angles (min gap via u16 dedup), radius in [0.1, 8].
    let mut spokes: Vec<(u16, u16)> = star.spokes;
    spokes.sort_by_key(|s| s.0);
    spokes.dedup_by_key(|s| s.0);
    if spokes.len() < 3 || spokes.len() > 512 {
        return;
    }
    let poly: Vec<PolyVert> = spokes
        .iter()
        .enumerate()
        .map(|(i, &(a, r))| {
            let theta = f64::from(a) / f64::from(u16::MAX) * core::f64::consts::TAU;
            let radius = 0.1 + 7.9 * f64::from(r) / f64::from(u16::MAX);
            PolyVert {
                pos: Vec2::new(radius * mathf::cos(theta), radius * mathf::sin(theta)),
                idx: i as i32,
            }
        })
        .collect();
    let n = poly.len();
    let tris = triangulate(core::slice::from_ref(&poly), 1e-9);
    assert_eq!(tris.len(), n - 2, "simple {n}-gon must give n-2 triangles");
    for t in &tris {
        for &i in t {
            assert!((i as usize) < n, "triangle index {i} out of range {n}");
        }
    }
});
