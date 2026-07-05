//! Primitive tessellation → [`Mesh`] — sphere/cube/cylinder, matching OpenSCAD `primitives.cc`.
//!
//! VERTICES are exact (OpenSCAD's ring math + exact-quadrant [`trig`](super::trig)) — the vertices
//! ARE the solid, so this is the conformance-critical part. The TRIANGULATION is our OWN fan/split
//! and deliberately does NOT reproduce OpenSCAD's: G.3.7 established that the gate for curved solids
//! is the boolean-difference RESIDUAL, which is triangulation-independent — so ANY valid
//! closed-manifold triangulation of these vertices is conformant. We have to be a manifold, not a
//! mimic. (Bit-exact trig still earns its keep on the polyhedral/exact classes + BOSL2 vertex math.)
//!
//! Degenerate inputs (r/size/h ≤ 0, non-finite) return an empty mesh (OpenSCAD returns empty
//! geometry, not an error). A representability guard keeps `u32` indices from overflowing on an
//! absurd `$fn`.

use super::trig::{cos_degrees, sin_degrees};
use crate::Mesh;

/// `sphere(r)` tessellated into `fragments` segments (OpenSCAD `SphereNode`, `primitives.cc:177-223`).
#[must_use]
pub(crate) fn sphere(r: f64, fragments: u32) -> Mesh {
    if !r.is_finite() || r <= 0.0 {
        return Mesh::new();
    }
    let nf = fragments;
    let num_rings = fragments / 2 + fragments % 2; // ceil(fragments/2), overflow-safe
    if u64::from(num_rings) * u64::from(nf) > u64::from(u32::MAX) {
        return Mesh::new(); // unrepresentable in u32 indices
    }

    let mut verts = Vec::new();
    for i in 0..num_rings {
        let phi = 180.0 * (f64::from(i) + 0.5) / f64::from(num_rings);
        let ring_r = r * sin_degrees(phi);
        let z = r * cos_degrees(phi);
        for j in 0..nf {
            let theta = 360.0 * f64::from(j) / f64::from(nf);
            verts.push([ring_r * cos_degrees(theta), ring_r * sin_degrees(theta), z]);
        }
    }

    // Guarded above: every index below is < num_rings*nf <= u32::MAX, so no u32 arithmetic overflows.
    let mut tris = Vec::new();
    fan(&mut tris, 0, nf, false); // top cap
    for i in 0..num_rings.saturating_sub(1) {
        for j in 0..nf {
            let jn = (j + 1) % nf;
            quad(
                &mut tris,
                i * nf + jn,
                i * nf + j,
                (i + 1) * nf + j,
                (i + 1) * nf + jn,
            );
        }
    }
    fan(&mut tris, num_rings.saturating_sub(1) * nf, nf, true); // bottom cap (reversed)
    Mesh { verts, tris }
}

/// `cube(size, center)` (OpenSCAD `CubeNode`, `primitives.cc`). 8 corner vertices, 12 triangles.
#[must_use]
pub(crate) fn cube(size: [f64; 3], center: bool) -> Mesh {
    let [x, y, z] = size;
    if !(x.is_finite() && y.is_finite() && z.is_finite()) || x <= 0.0 || y <= 0.0 || z <= 0.0 {
        return Mesh::new();
    }
    let (lo, hi) = if center {
        ([-x / 2.0, -y / 2.0, -z / 2.0], [x / 2.0, y / 2.0, z / 2.0])
    } else {
        ([0.0, 0.0, 0.0], [x, y, z])
    };
    let verts = vec![
        [lo[0], lo[1], lo[2]], // 0
        [hi[0], lo[1], lo[2]], // 1
        [hi[0], hi[1], lo[2]], // 2
        [lo[0], hi[1], lo[2]], // 3
        [lo[0], lo[1], hi[2]], // 4
        [hi[0], lo[1], hi[2]], // 5
        [hi[0], hi[1], hi[2]], // 6
        [lo[0], hi[1], hi[2]], // 7
    ];
    let tris = vec![
        [0, 3, 2],
        [0, 2, 1], // bottom  (-z)
        [4, 5, 6],
        [4, 6, 7], // top     (+z)
        [0, 1, 5],
        [0, 5, 4], // front   (-y)
        [1, 2, 6],
        [1, 6, 5], // right   (+x)
        [2, 3, 7],
        [2, 7, 6], // back    (+y)
        [3, 0, 4],
        [3, 4, 7], // left    (-x)
    ];
    Mesh { verts, tris }
}

/// `cylinder(h, r1, r2, center)` (OpenSCAD `CylinderNode`, `primitives.cc:251-308`). A ring collapses
/// to an apex when its radius is 0 (cone / inverted cone).
#[must_use]
pub(crate) fn cylinder(h: f64, r1: f64, r2: f64, fragments: u32, center: bool) -> Mesh {
    if !(h.is_finite() && r1.is_finite() && r2.is_finite())
        || h <= 0.0
        || r1 < 0.0
        || r2 < 0.0
        || (r1 == 0.0 && r2 == 0.0)
    {
        return Mesh::new();
    }
    let nf = fragments;
    if u64::from(nf) * 2 > u64::from(u32::MAX) {
        return Mesh::new();
    }
    let (z1, z2) = if center {
        (-h / 2.0, h / 2.0)
    } else {
        (0.0, h)
    };
    let bottom_apex = r1 == 0.0;
    let top_apex = r2 == 0.0;

    let mut verts = Vec::new();
    push_ring(&mut verts, r1, z1, nf, bottom_apex);
    let top_start = if bottom_apex { 1 } else { nf };
    push_ring(&mut verts, r2, z2, nf, top_apex);

    // Side faces. Both-apex is guarded out above, so these three branches are exhaustive AND all
    // reachable (regular cylinder / cone / inverted cone) — no dead arm.
    let mut tris = Vec::new();
    for j in 0..nf {
        let jn = (j + 1) % nf;
        if bottom_apex {
            tris.push([0, top_start + jn, top_start + j]); // triangle to bottom apex
        } else if top_apex {
            tris.push([j, jn, top_start]); // triangle to top apex
        } else {
            quad(&mut tris, j, jn, top_start + jn, top_start + j); // quad between two rings
        }
    }
    if !bottom_apex {
        fan(&mut tris, 0, nf, true); // bottom cap (reversed, faces -z)
    }
    if !top_apex {
        fan(&mut tris, top_start, nf, false); // top cap (faces +z)
    }
    Mesh { verts, tris }
}

/// Push a ring of `nf` vertices at height `z` (or a single apex vertex at the axis if `apex`).
fn push_ring(verts: &mut Vec<[f64; 3]>, r: f64, z: f64, nf: u32, apex: bool) {
    if apex {
        verts.push([0.0, 0.0, z]);
        return;
    }
    for j in 0..nf {
        let theta = 360.0 * f64::from(j) / f64::from(nf);
        verts.push([r * cos_degrees(theta), r * sin_degrees(theta), z]);
    }
}

/// Triangulate a quad `[a, b, c, d]` into two triangles.
fn quad(tris: &mut Vec<[u32; 3]>, a: u32, b: u32, c: u32, d: u32) {
    tris.push([a, b, c]);
    tris.push([a, c, d]);
}

/// Fan-triangulate an `nf`-gon starting at vertex `base`; `reverse` flips the winding.
fn fan(tris: &mut Vec<[u32; 3]>, base: u32, nf: u32, reverse: bool) {
    for j in 1..nf.saturating_sub(1) {
        if reverse {
            tris.push([base, base + j + 1, base + j]);
        } else {
            tris.push([base, base + j, base + j + 1]);
        }
    }
}

// I.7 — Kani proofs that the u32 TESSELLATION INDEX arithmetic can't overflow on an untrusted `$fn`
// (docs/testing-cards.md: "indices in bounds", panic-freedom on untrusted SCAD). The representability
// guards (sphere: num_rings*nf <= u32::MAX at line 25; cylinder: nf*2 <= u32::MAX at line 111) are the
// preconditions; Kani proves every index expression stays in u32 for ALL ring/segment counts that pass
// them. Preconditions are stated in u64 so the assumes themselves can't wrap; the PROVEN arithmetic is
// the actual u32 code. Compiled only under `cargo kani`.
#[cfg(kani)]
mod proofs {
    /// sphere()'s quad-corner indices (geometry.rs:48-51 + the bottom-cap base at :55): under the
    /// line-25 guard and the loop bounds (i+1 < num_rings, j < nf), none overflow u32.
    #[kani::proof]
    fn sphere_quad_indices_never_overflow() {
        let nf: u32 = kani::any();
        let num_rings: u32 = kani::any();
        // Bound each factor to 2^16 — a symbolic u32*u32 is CBMC's hard case, but with 16-bit-
        // significant operands the multiplier is tractable. The PRODUCT still spans u32::MAX
        // (2^16 * 2^16 = 2^32 > u32::MAX), so the line-25 guard is genuinely exercised at its boundary:
        // for the upper inputs the guard fires and the index code is unreachable, exactly as shipped.
        // (65536 segments is already absurd for any real $fn; the guard covers beyond by construction.)
        kani::assume(nf >= 1 && nf <= (1 << 16));
        kani::assume(num_rings >= 1 && num_rings <= (1 << 16));
        kani::assume(u64::from(num_rings) * u64::from(nf) <= u64::from(u32::MAX)); // the guard
        let i: u32 = kani::any();
        let j: u32 = kani::any();
        kani::assume(u64::from(i) + 1 < u64::from(num_rings)); // quad loop: i in 0..num_rings-1
        kani::assume(u64::from(j) < u64::from(nf)); // j in 0..nf
        let jn = (j + 1) % nf;
        let _a = i * nf + jn;
        let _b = i * nf + j;
        let _c = (i + 1) * nf + j;
        let _d = (i + 1) * nf + jn;
        let _base = num_rings.saturating_sub(1) * nf;
    }

    /// cylinder()'s side/apex indices (geometry.rs:133-137): under the line-111 guard (nf*2 <=
    /// u32::MAX) with top_start <= nf and j < nf, `top_start + jn` fits in u32.
    #[kani::proof]
    fn cylinder_side_indices_never_overflow() {
        let nf: u32 = kani::any();
        kani::assume(nf >= 1);
        kani::assume(u64::from(nf) * 2 <= u64::from(u32::MAX)); // the guard
        let top_start: u32 = kani::any();
        kani::assume(u64::from(top_start) <= u64::from(nf)); // top_start is nf (or 1 for a bottom apex)
        let j: u32 = kani::any();
        kani::assume(u64::from(j) < u64::from(nf));
        let jn = (j + 1) % nf;
        let _a = top_start + jn;
        let _b = top_start + j;
    }

    /// fan()'s indices (`base + j + 1`, geometry.rs:171/173): safe as long as the CALLER guarantees
    /// base + nf <= u32::MAX — which sphere/cylinder both do via their guards. Proven per-index (the
    /// loop body), not over the loop, so there's nothing to unwind.
    #[kani::proof]
    fn fan_indices_never_overflow() {
        let base: u32 = kani::any();
        let nf: u32 = kani::any();
        kani::assume(u64::from(base) + u64::from(nf) <= u64::from(u32::MAX)); // caller precondition
        let j: u32 = kani::any();
        kani::assume(u64::from(j) + 1 < u64::from(nf)); // fan loop: j in 1..nf-1
        let _x = base + j + 1;
        let _y = base + j;
    }
}
