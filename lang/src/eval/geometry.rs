//! Primitive tessellation → [`Mesh`] (3D) / contours (2D) — sphere/cube/cylinder + square/circle/polygon,
//! matching OpenSCAD `primitives.cc`.
//!
//! VERTICES are exact (OpenSCAD's ring math + exact-quadrant [`trig`](super::trig)) — the vertices
//! ARE the solid, so this is the conformance-critical part. The TRIANGULATION is our OWN fan/split
//! and deliberately does NOT reproduce OpenSCAD's: G.3.7 established that the gate for curved solids
//! is the boolean-difference RESIDUAL, which is triangulation-independent — so ANY valid
//! closed-manifold triangulation of these vertices is conformant. We have to be a manifold, not a
//! mimic. (Bit-exact trig still earns its keep on the polyhedral/exact classes + BOSL2 vertex math.)
//!
//! The 2D primitives produce CONTOURS (rings of [`Vec2`]) — the [`Shape2D::Polygon`] leaf data (J.3.2),
//! the 2D analogue of a [`Mesh`]. `circle` reuses the SAME exact-quadrant ring math as `cylinder`/`sphere`
//! (OpenSCAD's `generate_circle` is `cos_degrees`/`sin_degrees` too), so 2D and 3D share `$fn` parity.
//!
//! Degenerate inputs (r/size/h ≤ 0, non-finite) return empty geometry — an empty mesh / no contours
//! (OpenSCAD returns empty geometry, not an error). A representability guard keeps `u32` indices from
//! overflowing on an absurd `$fn`.

use super::geo2d::Contour;
use super::trig::{cos_degrees, sin_degrees};
use crate::Mesh;
use crate::geom::{Tri, Vec2, Vec3};

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
            verts.push(Vec3::new(
                ring_r * cos_degrees(theta),
                ring_r * sin_degrees(theta),
                z,
            ));
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
        Vec3::new(lo[0], lo[1], lo[2]), // 0
        Vec3::new(hi[0], lo[1], lo[2]), // 1
        Vec3::new(hi[0], hi[1], lo[2]), // 2
        Vec3::new(lo[0], hi[1], lo[2]), // 3
        Vec3::new(lo[0], lo[1], hi[2]), // 4
        Vec3::new(hi[0], lo[1], hi[2]), // 5
        Vec3::new(hi[0], hi[1], hi[2]), // 6
        Vec3::new(lo[0], hi[1], hi[2]), // 7
    ];
    let tris = vec![
        Tri::new(0, 3, 2),
        Tri::new(0, 2, 1), // bottom  (-z)
        Tri::new(4, 5, 6),
        Tri::new(4, 6, 7), // top     (+z)
        Tri::new(0, 1, 5),
        Tri::new(0, 5, 4), // front   (-y)
        Tri::new(1, 2, 6),
        Tri::new(1, 6, 5), // right   (+x)
        Tri::new(2, 3, 7),
        Tri::new(2, 7, 6), // back    (+y)
        Tri::new(3, 0, 4),
        Tri::new(3, 4, 7), // left    (-x)
    ];
    Mesh { verts, tris }
}

/// `polyhedron(points, faces)` → an indexed mesh (OpenSCAD `PolyhedronNode`, `primitives.cc`). Each face
/// (a vertex-index loop) FAN-triangulates from its first vertex: `[i0,i1,…,in]` → `(i0,i1,i2)`,
/// `(i0,i2,i3)`, … — OpenSCAD's exact triangulation. Winding is the PRODUCER's; OpenSCAD trusts the
/// caller's clockwise-from-outside order and the harness canonicalizes for comparison, so we preserve
/// index order. A face shorter than 3, or a triangle referencing an out-of-range vertex, is DROPPED here
/// (no panic) — the exact OpenSCAD out-of-bounds ERROR + degenerate-face WARNING are the validation layer
/// (J.2.6.2). `points` becomes the vertex table verbatim (unreferenced points are kept, as OpenSCAD does).
#[must_use]
pub(crate) fn polyhedron(points: Vec<Vec3>, faces: &[Vec<u32>]) -> Mesh {
    let n = u32::try_from(points.len()).unwrap_or(u32::MAX);
    let mut tris = Vec::new();
    for face in faces {
        // fan: (face[0], face[k], face[k+1]) for k in 1..len-1
        for pair in face.windows(2).skip(1) {
            let (a, b, c) = (face[0], pair[0], pair[1]);
            if a < n && b < n && c < n {
                tris.push(Tri::new(a, b, c));
            }
        }
    }
    Mesh {
        verts: points,
        tris,
    }
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
            tris.push(Tri::new(0, top_start + jn, top_start + j)); // triangle to bottom apex
        } else if top_apex {
            tris.push(Tri::new(j, jn, top_start)); // triangle to top apex
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

// ─────────────────────────────── 2D primitives (J.3.2) → contours ───────────────────────────────
// The tessellation half of J.3.2 (parity-critical, esp. circle's `$fn`): pure `args → contours`
// builders, unit-tested against exact geometry. WIRING them into the evaluator — recognizing
// square/circle/polygon as 2D module calls and threading the dimension-tagged `Geo` result through the
// geometry pass — is the J.3.2 eval-wire follow-up (it changes `evaluate_geometry`'s return type, which
// ripples into fab-scad, so it lands with review). The `dead_code` allow drops with that wiring.

/// `square([x, y], center)` → its single contour (OpenSCAD `SquareNode`). CCW winding
/// `[0,0] → [x,0] → [x,y] → [0,y]`; `center` puts the centroid at the origin. Degenerate
/// (non-positive / non-finite) → no contours (an empty region).
#[must_use]
#[allow(
    dead_code,
    reason = "J.3.2 2D tessellation; the eval-wire follow-up (Geo-tagged geometry pass) calls it"
)]
pub(crate) fn square(x: f64, y: f64, center: bool) -> Vec<Contour> {
    if !(x.is_finite() && y.is_finite()) || x <= 0.0 || y <= 0.0 {
        return Vec::new();
    }
    let (x0, y0, x1, y1) = if center {
        (-x / 2.0, -y / 2.0, x / 2.0, y / 2.0)
    } else {
        (0.0, 0.0, x, y)
    };
    vec![vec![
        Vec2::new(x0, y0),
        Vec2::new(x1, y0),
        Vec2::new(x1, y1),
        Vec2::new(x0, y1),
    ]]
}

/// `circle(r)` tessellated into `fragments` segments (OpenSCAD `CircleNode` / `generate_circle`) — a
/// regular `fragments`-gon, CCW, using the SAME exact-quadrant ring math as `cylinder`/`sphere` (so 2D
/// and 3D share `$fn` parity to the bit). `fragments` is `$fn`-resolved by [`fragments`](super::fragments)
/// (always ≥ 3). Degenerate radius (non-positive / non-finite) → no contours.
#[must_use]
#[allow(
    dead_code,
    reason = "J.3.2 2D tessellation; the eval-wire follow-up (Geo-tagged geometry pass) calls it"
)]
pub(crate) fn circle(r: f64, fragments: u32) -> Vec<Contour> {
    if !r.is_finite() || r <= 0.0 {
        return Vec::new();
    }
    let contour = (0..fragments)
        .map(|i| {
            let theta = 360.0 * f64::from(i) / f64::from(fragments);
            Vec2::new(r * cos_degrees(theta), r * sin_degrees(theta))
        })
        .collect();
    vec![contour]
}

/// `polygon(points, paths)` → its contours (OpenSCAD `PolygonNode`). With `paths`, each path is a ring
/// of indices into `points`; without, the single contour is all `points` in order. An out-of-range index
/// is DROPPED and a contour of fewer than 3 valid points is discarded (the exact out-of-range ERROR is
/// the validation layer, a later J.3 task, mirroring [`polyhedron`]). No usable contour → empty.
#[must_use]
#[allow(
    dead_code,
    reason = "J.3.2 2D tessellation; the eval-wire follow-up (Geo-tagged geometry pass) calls it"
)]
pub(crate) fn polygon(points: &[Vec2], paths: Option<&[Vec<u32>]>) -> Vec<Contour> {
    match paths {
        // No paths → the whole point list is one contour (needs ≥ 3 to bound any area).
        None => {
            if points.len() < 3 {
                Vec::new()
            } else {
                vec![points.to_vec()]
            }
        }
        // Each path selects its contour's points by index; bad indices drop, short contours discard.
        Some(paths) => paths
            .iter()
            .filter_map(|path| {
                let contour: Contour = path
                    .iter()
                    .filter_map(|&i| points.get(i as usize).copied())
                    .collect();
                (contour.len() >= 3).then_some(contour)
            })
            .collect(),
    }
}

/// Push a ring of `nf` vertices at height `z` (or a single apex vertex at the axis if `apex`).
fn push_ring(verts: &mut Vec<Vec3>, r: f64, z: f64, nf: u32, apex: bool) {
    if apex {
        verts.push(Vec3::new(0.0, 0.0, z));
        return;
    }
    for j in 0..nf {
        let theta = 360.0 * f64::from(j) / f64::from(nf);
        verts.push(Vec3::new(r * cos_degrees(theta), r * sin_degrees(theta), z));
    }
}

/// Triangulate a quad `[a, b, c, d]` into two triangles.
fn quad(tris: &mut Vec<Tri>, a: u32, b: u32, c: u32, d: u32) {
    tris.push(Tri::new(a, b, c));
    tris.push(Tri::new(a, c, d));
}

/// Fan-triangulate an `nf`-gon starting at vertex `base`; `reverse` flips the winding.
fn fan(tris: &mut Vec<Tri>, base: u32, nf: u32, reverse: bool) {
    for j in 1..nf.saturating_sub(1) {
        if reverse {
            tris.push(Tri::new(base, base + j + 1, base + j));
        } else {
            tris.push(Tri::new(base, base + j, base + j + 1));
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "2D primitive vertices are EXACT — literal square corners + exact-quadrant circle points"
)]
mod tests {
    use super::{circle, polygon, square};
    use crate::geom::Vec2;

    /// A 2D point.
    fn p(x: f64, y: f64) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn square_corners_and_centering() {
        // Resting on the origin: CCW from [0,0].
        assert_eq!(
            square(2.0, 3.0, false),
            vec![vec![p(0.0, 0.0), p(2.0, 0.0), p(2.0, 3.0), p(0.0, 3.0)]]
        );
        // Centered: centroid at the origin.
        assert_eq!(
            square(2.0, 2.0, true),
            vec![vec![p(-1.0, -1.0), p(1.0, -1.0), p(1.0, 1.0), p(-1.0, 1.0)]]
        );
        // Degenerate → no contours.
        assert!(square(0.0, 5.0, false).is_empty());
        assert!(square(5.0, -1.0, false).is_empty());
        assert!(square(f64::NAN, 5.0, false).is_empty());
    }

    #[test]
    fn circle_is_a_regular_polygon_with_exact_quadrants() {
        // $fn = 4 → a diamond at the axes; exact-quadrant trig makes these EXACT.
        assert_eq!(
            circle(1.0, 4),
            vec![vec![p(1.0, 0.0), p(0.0, 1.0), p(-1.0, 0.0), p(0.0, -1.0)]]
        );
        // A finer circle: `fragments` points, first on +x, none degenerate.
        let c = circle(5.0, 32);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].len(), 32);
        assert_eq!(c[0][0], p(5.0, 0.0));
        // Degenerate radius → no contours.
        assert!(circle(0.0, 16).is_empty());
        assert!(circle(-2.0, 16).is_empty());
        assert!(circle(f64::INFINITY, 16).is_empty());
    }

    #[test]
    fn polygon_from_points_and_paths() {
        let pts = [p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(0.0, 4.0)];
        // No paths → all points, one contour.
        assert_eq!(polygon(&pts, None), vec![pts.to_vec()]);
        // Fewer than 3 points → no contour.
        assert!(polygon(&pts[..2], None).is_empty());
        // Paths select + reorder points; two contours (e.g. an outer + a hole).
        let inner = [p(1.0, 1.0), p(2.0, 1.0), p(2.0, 2.0)];
        let all: Vec<Vec2> = pts.iter().chain(inner.iter()).copied().collect();
        let paths = vec![vec![0, 1, 2, 3], vec![4, 5, 6]];
        assert_eq!(
            polygon(&all, Some(&paths)),
            vec![pts.to_vec(), inner.to_vec()]
        );
        // An out-of-range index is dropped; a path left with < 3 points is discarded entirely.
        let paths_bad = vec![vec![0, 1, 99], vec![0, 1, 2]];
        assert_eq!(polygon(&pts, Some(&paths_bad)), vec![pts[..3].to_vec()]);
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
