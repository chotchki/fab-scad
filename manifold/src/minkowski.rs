//! Minkowski sum — a port of Manifold's `minkowski.cpp` (PR #666's tiered hull+union). The Minkowski
//! sum `A ⊕ B = { a + b : a ∈ A, b ∈ B }` dilates `A` by `B` (the OpenSCAD `minkowski()` primitive).
//!
//! The key trick that makes this tractable without a general convex-decomposition algorithm: the
//! mesh's OWN triangulation is the decomposition — each triangle face is already a (degenerate)
//! convex piece, so the sum is built as a union of per-face hulls. Three tiers, dispatched on
//! [`Mesh::is_convex`]:
//!
//! - **Tier 0** convex×convex: one hull of every vertex-sum. Fast, exact.
//! - **Tier 1** nonconvex×convex: sweep the convex operand along each face of the other (hull of the
//!   3 face verts each summed with all convex verts), union the per-face hulls.
//! - **Tier 2** nonconvex×nonconvex: per (face, face) pair, hull the 9 vertex-sums, skip coplanar
//!   pairs, union. `O(triA · triB)` — the slow, rare tier.
//!
//! The final result unions all the per-face hulls with the ORIGINAL `A` (which fills the core), then
//! C++ resets provenance via `AsOriginal` — skipped here, invisible to the volume gate.
//!
//! Parallelism mirrors C++'s COARSE grain (BU.4.4): per-face hulls go through the seam with C++'s
//! exact threshold 100 (`autoPolicy(numIter, 100)` — each item is a whole quickhull), and the hull
//! unions run as a fixed-shape pairwise reduction tree. Two deviations from the C++, neither
//! affecting the computed SOLID (the gate is volume-residual, algorithm-independent — Minkowski
//! triangulation is never byte-identical):
//!
//! - **Fixed-shape union tree.** C++ `BatchBoolean(items, Add)` unions through a size-ordered
//!   priority queue whose pairing can be timing-dependent; we union adjacent pairs in fixed rounds
//!   (tree shape = pure function of the input count), so serial/par/wasm execute the IDENTICAL
//!   boolean sequence. Union is associative — same solid, different (still deterministic)
//!   triangulation than either C++ or the pre-BU.4.4 left-fold.
//! - **Inset/difference deferred.** Only `minkowski_sum` (dilate) ships here; `MinkowskiDifference`
//!   (erode, `inset = true`) is a later box — no stub, no caller yet.
//!
//! All three tiers are gated vs C++ `minkowski_sum` (`m3_7_minkowski_vs_cpp`). Tier 1/2 were briefly
//! blocked by a boolean coplanar-merge infinite loop the swept-face-hull unions triggered — that was
//! a port bug in the ear-clip (`ring`'s dropped re-anchor), fixed in M.3.9.

use crate::boolean::OpType;
use crate::boolean::boolean_result::boolean;
use crate::mesh::Mesh;
use crate::mesh_ids::{HalfedgeId, TriId};
use crate::par;
use crate::status::Error;

/// Union a set of meshes (C++ `BatchBoolean(items, Add)`). `None` if empty. FIXED-SHAPE pairwise
/// reduction tree: each round unions adjacent pairs `(0,1), (2,3), …` — an odd tail rides into the
/// next round unchanged — until one mesh remains. The tree shape is a pure function of the input
/// count (NOT C++'s BatchBoolean priority queue, whose pairing follows intermediate result sizes),
/// so every lane executes the identical boolean sequence. Rounds go through
/// [`par::map_collect_min_len`] with threshold 2: each item is a whole boolean, so the seam's
/// uniform 10k threshold would never fire.
///
/// HISTORY: a pairwise tree was tried pre-M.3.9 and some pairings hung in the boolean's
/// coplanar-merge loop — that was the ear-clip re-anchor port bug, fixed at M.3.9. Re-verified at
/// BU.4.4: tier-1/2 lib tests + the t1 harness + a nonconvex⊕nonconvex case all complete.
fn union_all(meshes: Vec<Mesh>) -> Option<Mesh> {
    let mut level = meshes;
    while level.len() > 1 {
        let pairs: Vec<(&Mesh, &Mesh)> = level.chunks_exact(2).map(|c| (&c[0], &c[1])).collect();
        let mut next = par::map_collect_min_len(&pairs, 2, |&(x, y)| boolean(x, y, OpType::Add));
        drop(pairs);
        if level.len() % 2 == 1 {
            next.push(level.pop().expect("odd level is non-empty"));
        }
        level = next;
    }
    level.pop()
}

impl Mesh {
    /// The Minkowski sum `self ⊕ other` (dilate `self` by `other`). Handles all convexity
    /// combinations (Tier 0/1/2). Positions-only through the boolean pipeline.
    ///
    /// Empty is the identity: `A ⊕ ∅ = A`, `∅ ⊕ B = B` (the empty-*annihilator* some callers expect is
    /// a scad-layer concern, above the kernel).
    pub fn minkowski_sum(&self, other: &Mesh) -> Result<Mesh, Error> {
        let mut a = self;
        let mut b = other;
        let mut a_convex = a.is_convex();
        let mut b_convex = b.is_convex();

        // Put the convex operand second (so the tier dispatch below only ever branches on b_convex).
        if a_convex && !b_convex {
            std::mem::swap(&mut a, &mut b);
            std::mem::swap(&mut a_convex, &mut b_convex);
        }

        if b.is_empty() {
            return Ok(a.clone());
        }
        if a.is_empty() {
            return Ok(b.clone());
        }

        // The original A is the base of the final union (it fills the core; the per-face hulls form
        // the swept boundary shell).
        let mut composed_hulls: Vec<Mesh> = vec![a.clone()];

        if a_convex && b_convex {
            // Tier 0 — convex×convex: one hull of { a + b } over all vertex pairs.
            let mut simple_hull = Vec::with_capacity(a.num_vert() * b.num_vert());
            for &av in &a.vert_pos {
                for &bv in &b.vert_pos {
                    simple_hull.push(bv + av);
                }
            }
            composed_hulls.push(Mesh::hull_of_points(&simple_hull)?);
        } else if b_convex {
            // Tier 1 — nonconvex×convex: sweep convex B along each A triangle face. Each face's
            // hull is a pure function of (face verts, B), order preserved through the seam;
            // threshold 100 is C++'s `autoPolicy(numIter, 100)` — a whole quickhull per item.
            let num_tri = a.num_tri();
            let faces: Vec<usize> = (0..num_tri).collect();
            let hulls = par::map_collect_min_len(&faces, 100, |&tri| {
                let mut simple_hull = Vec::with_capacity(3 * b.num_vert());
                for i in 0..3 {
                    let vertex = a.pos(a.start(HalfedgeId::from_usize(tri * 3 + i)));
                    for &bv in &b.vert_pos {
                        simple_hull.push(bv + vertex);
                    }
                }
                Mesh::hull_of_points(&simple_hull)
            })
            .into_iter()
            .collect::<Result<Vec<Mesh>, Error>>()?;
            if let Some(u) = union_all(hulls) {
                composed_hulls.push(u);
            }
        } else {
            // Tier 2 — nonconvex×nonconvex: per (A-face, B-face) pair, hull the 9 vertex-sums.
            // A faces stay sequential (as in C++); each A face's B sweep goes through the seam
            // with C++'s threshold 100 (`autoPolicy(numTriB, 100)`). `None` = coplanar-skipped
            // or empty hull; order preserved, filtered after the seam.
            let num_tri_a = a.num_tri();
            let num_tri_b = b.num_tri();
            let b_faces: Vec<usize> = (0..num_tri_b).collect();
            let mut accumulated: Vec<Mesh> = Vec::new();
            for a_face in 0..num_tri_a {
                let a1 = a.pos(a.start(HalfedgeId::from_usize(a_face * 3)));
                let a2 = a.pos(a.start(HalfedgeId::from_usize(a_face * 3 + 1)));
                let a3 = a.pos(a.start(HalfedgeId::from_usize(a_face * 3 + 2)));
                let n_a = a.tri_normal(TriId::from_usize(a_face));
                let hulls = par::map_collect_min_len(&b_faces, 100, |&b_face| {
                    let n_b = b.tri_normal(TriId::from_usize(b_face));
                    // Skip coplanar face pairs — their 9-point hull is degenerate.
                    let coplanar =
                        (n_a.dot(n_b) - 1.0).abs() < 1e-12 || (n_a.dot(-n_b) - 1.0).abs() < 1e-12;
                    if coplanar {
                        return Ok(None);
                    }
                    let b1 = b.pos(b.start(HalfedgeId::from_usize(b_face * 3)));
                    let b2 = b.pos(b.start(HalfedgeId::from_usize(b_face * 3 + 1)));
                    let b3 = b.pos(b.start(HalfedgeId::from_usize(b_face * 3 + 2)));
                    let pts = [
                        a1 + b1,
                        a1 + b2,
                        a1 + b3,
                        a2 + b1,
                        a2 + b2,
                        a2 + b3,
                        a3 + b1,
                        a3 + b2,
                        a3 + b3,
                    ];
                    let h = Mesh::hull_of_points(&pts)?;
                    Ok(if h.is_empty() { None } else { Some(h) })
                });
                let mut face_hulls: Vec<Mesh> = Vec::with_capacity(hulls.len());
                for h in hulls {
                    if let Some(h) = h? {
                        face_hulls.push(h);
                    }
                }
                if let Some(u) = union_all(face_hulls) {
                    accumulated.push(u);
                }
            }
            if let Some(u) = union_all(accumulated) {
                composed_hulls.push(u);
            }
        }

        // `composed_hulls` always holds at least the base mesh, so the fold never returns None.
        Ok(union_all(composed_hulls).expect("composed_hulls holds at least the base mesh"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linalg::{Mat3x4, Vec3};

    /// BU.4.4 timing harness — the perf-tracked "t1" case (concave = cube(2,2,2) minus the [1,2]³
    /// corner octant, dilated by a centered 0.25-cube). Ignored: run explicitly with
    /// `cargo test --release -p fab-manifold --lib -- --ignored t1_timing` (± `--features par`).
    /// Prints median-of-5 ms plus the result's num_tri and volume BITS — the cross-lane
    /// (serial/par/pre-change) identity check.
    #[test]
    #[ignore = "timing harness — run with --release -- --ignored"]
    fn t1_timing_and_result_fingerprint() {
        let concave = boolean(
            &Mesh::cube(Vec3::new(2.0, 2.0, 2.0), false).unwrap(),
            &Mesh::cube(Vec3::new(1.0, 1.0, 1.0), false)
                .unwrap()
                .transform(Mat3x4::translate(Vec3::new(1.0, 1.0, 1.0)))
                .unwrap(),
            OpType::Subtract,
        );
        let small = Mesh::cube(Vec3::new(0.25, 0.25, 0.25), true).unwrap();

        let mut times = Vec::new();
        let mut result = None;
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let r = concave.minkowski_sum(&small).unwrap();
            times.push(t0.elapsed().as_secs_f64() * 1e3);
            result = Some(r);
        }
        times.sort_by(f64::total_cmp);
        let r = result.unwrap();
        eprintln!(
            "t1 median {:.3} ms (runs {:?}); num_tri={} volume={} volume_bits=0x{:016x}",
            times[2],
            times,
            r.num_tri(),
            r.volume(),
            r.volume().to_bits()
        );
    }

    #[test]
    fn cube_minkowski_cube_is_a_bigger_box() {
        // A = [0,10]³ (convex), B = [-1,1]³ (convex, contains origin). A ⊕ B = [-1,11]³, volume 12³.
        let a = Mesh::cube(Vec3::splat(10.0), false).unwrap();
        let b = Mesh::cube(Vec3::splat(2.0), true).unwrap();
        assert!(a.is_convex(), "cube A should be convex");
        assert!(b.is_convex(), "cube B should be convex");

        let sum = a.minkowski_sum(&b).unwrap();
        assert!(sum.is_manifold(), "minkowski result must be manifold");
        // 12³ = 1728, exact for the box⊕box case.
        assert!(
            (sum.volume() - 1728.0).abs() < 1e-6,
            "cube(10) ⊕ cube(2,centered) volume {} != 1728",
            sum.volume()
        );
        let bb = sum.bounding_box();
        assert!((bb.min.x - -1.0).abs() < 1e-9 && (bb.max.x - 11.0).abs() < 1e-9);
    }

    #[test]
    fn is_convex_detects_cube_and_notch() {
        let cube = Mesh::cube(Vec3::splat(4.0), false).unwrap();
        assert!(cube.is_convex(), "a cube is convex");
        // A corner-notched cube ([0,6]³ minus the [0,3]³ octant) is concave.
        let big = Mesh::cube(Vec3::splat(6.0), false).unwrap();
        let notch = Mesh::cube(Vec3::splat(3.0), false).unwrap();
        let concave = boolean(&big, &notch, OpType::Subtract);
        assert!(concave.is_manifold());
        assert!(!concave.is_convex(), "a corner-notched cube is non-convex");
    }

    #[test]
    fn tier1_nonconvex_convex_dilates() {
        // Tier 1: dilate a concave solid ([0,6]³ minus the [0,3]³ octant) by a small cube. Exercises
        // the swept-face-hull union path end-to-end (the C++ differential lives in the oracle suite).
        let big = Mesh::cube(Vec3::splat(6.0), false).unwrap();
        let notch = Mesh::cube(Vec3::splat(3.0), false).unwrap();
        let concave = boolean(&big, &notch, OpType::Subtract);
        assert!(!concave.is_convex());
        let concave_vol = concave.volume();

        let tool = Mesh::cube(Vec3::splat(1.0), true).unwrap(); // [-0.5,0.5]³
        let dilated = concave.minkowski_sum(&tool).unwrap();
        assert!(dilated.is_manifold(), "tier 1 result must be manifold");
        // Dilation only grows the solid.
        assert!(
            dilated.volume() > concave_vol,
            "dilated volume {} should exceed the original {concave_vol}",
            dilated.volume()
        );
    }

    #[test]
    fn tier2_nonconvex_nonconvex_dilates() {
        // Tier 2: BOTH operands concave (corner-notched cubes) — the (A-face × B-face) hull sweep
        // plus the union tree. Doubles as the BU.4.4 hang re-test: pre-M.3.9 some pairwise-tree
        // pairings looped forever in the boolean coplanar merge; this must terminate.
        let concave_a = boolean(
            &Mesh::cube(Vec3::splat(4.0), false).unwrap(),
            &Mesh::cube(Vec3::splat(2.0), false).unwrap(),
            OpType::Subtract,
        );
        let concave_b = boolean(
            &Mesh::cube(Vec3::splat(1.0), false).unwrap(),
            &Mesh::cube(Vec3::splat(0.5), false).unwrap(),
            OpType::Subtract,
        );
        assert!(!concave_a.is_convex() && !concave_b.is_convex());
        let a_vol = concave_a.volume();

        let dilated = concave_a.minkowski_sum(&concave_b).unwrap();
        assert!(dilated.is_manifold(), "tier 2 result must be manifold");
        assert!(
            dilated.volume() > a_vol,
            "dilated volume {} should exceed the original {a_vol}",
            dilated.volume()
        );
        // A ⊕ B's bounding box is exactly box(A) + box(B): [0,4]³ + [0,1]³ = [0,5]³.
        let bb = dilated.bounding_box();
        assert!(bb.min.x.abs() < 1e-9 && (bb.max.x - 5.0).abs() < 1e-9);
    }

    /// Cross-lane identity for the tier-2 case (the union tree + B-sweep under par): prints
    /// num_tri + volume bits — run under serial and `--features par`, the lines must match.
    #[test]
    #[ignore = "fingerprint probe — run with -- --ignored on both lanes and compare"]
    fn tier2_result_fingerprint() {
        let concave_a = boolean(
            &Mesh::cube(Vec3::splat(4.0), false).unwrap(),
            &Mesh::cube(Vec3::splat(2.0), false).unwrap(),
            OpType::Subtract,
        );
        let concave_b = boolean(
            &Mesh::cube(Vec3::splat(1.0), false).unwrap(),
            &Mesh::cube(Vec3::splat(0.5), false).unwrap(),
            OpType::Subtract,
        );
        let r = concave_a.minkowski_sum(&concave_b).unwrap();
        eprintln!(
            "tier2 num_tri={} volume={} volume_bits=0x{:016x}",
            r.num_tri(),
            r.volume(),
            r.volume().to_bits()
        );
    }

    #[test]
    fn minkowski_with_empty_is_identity() {
        let a = Mesh::cube(Vec3::splat(3.0), false).unwrap();
        let empty = Mesh {
            num_prop: 0,
            ..Default::default()
        };
        // A ⊕ ∅ = A.
        let r1 = a.minkowski_sum(&empty).unwrap();
        assert!(
            (r1.volume() - 27.0).abs() < 1e-9,
            "A ⊕ ∅ should be A (vol 27)"
        );
        // ∅ ⊕ A = A.
        let r2 = empty.minkowski_sum(&a).unwrap();
        assert!(
            (r2.volume() - 27.0).abs() < 1e-9,
            "∅ ⊕ A should be A (vol 27)"
        );
    }
}
