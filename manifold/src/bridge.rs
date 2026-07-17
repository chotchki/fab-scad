//! The 2D↔3D bridges — Manifold's `Extrude` / `Revolve` (2D→3D) and `Project` / `Slice` (3D→2D), R5/M.5.
//! These are the ops that UNBLOCK M.3.8 (the OpenSCAD `linear_extrude`/`rotate_extrude`/`projection`).
//!
//! The caps reuse the 3D `polygon.rs` triangulator (the same Delaunay-cost ear-clip the boolean leans on);
//! the walls are quads. `CrossSection` is the i_overlay-backed 2D type (area-residual gated); the produced
//! `Mesh` is a normal 3D solid that flows through the byte-exact 3D pipeline (`sort_geometry` etc.).

use crate::boolean::polygon::{PolyVert, triangulate_with_convex};
use crate::cross_section::CrossSection;
use crate::linalg::{Mat3x4, Vec2, Vec3};
use crate::mathf::{cosd, sind};
use crate::mesh::Mesh;
use crate::mesh_ids::TriId;

/// The shared `Impl` ctor tail every 2D→3D constructor runs (C++ `CreateHalfedges` →
/// `InitializeOriginal` → `CalculateBBox` → `SetEpsilon` → `SortGeometry` → `SetNormalsAndCoplanar`,
/// in that order).
fn finish_solid(vert_pos: Vec<Vec3>, tris: &[[u32; 3]]) -> Mesh {
    let mut mesh = Mesh {
        vert_pos,
        num_prop: 0,
        ..Default::default()
    };
    mesh.create_halfedges(tris);
    mesh.initialize_original();
    mesh.calculate_bbox();
    mesh.set_epsilon(-1.0, false);
    mesh.sort_geometry();
    mesh.set_normals_and_coplanar();
    mesh
}

/// `Manifold::Extrude` (constructors.cpp:215), the GENERAL form: `n_divisions` intermediate slices, a
/// total `twist_degrees` spin, and a linearly-interpolated `scale_top` — `(0, 0)` makes a CONE (rings
/// collapse to one apex vertex per contour, duplicated for genus). Empty input or `height <= 0` ⇒ empty
/// mesh (Manifold `Invalid`). Operates on raw polygons exactly like the C++ (no i_overlay round-trip);
/// winding carries intent. Per-slice transform is `diag(scale) · R(phi)` with `sind`/`cosd` degree trig
/// and la's entry-then-apply product order, so verts are bit-identical to C++.
///
/// DEVIATION (panic-safety): a negative `n_divisions` is clamped to 0 — the C++ would divide by zero
/// (UB-adjacent inf verts); no caller passes one.
pub fn extrude_polygons(
    cross_section: &[Vec<Vec2>],
    height: f64,
    n_divisions: i32,
    twist_degrees: f64,
    mut scale_top: Vec2,
) -> Mesh {
    if cross_section.is_empty() || height <= 0.0 {
        return Mesh::default();
    }

    scale_top.x = scale_top.x.max(0.0);
    scale_top.y = scale_top.y.max(0.0);

    let nd = i64::from(n_divisions.max(0)) + 1; // C++ ++nDivisions
    let is_cone = scale_top.x == 0.0 && scale_top.y == 0.0;

    // Bottom verts (z = 0) in flat contour order, doubling as the indexed cap polygons.
    let mut vert_pos: Vec<Vec3> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut n_cross: i64 = 0;
    let mut idx: i32 = 0;
    let mut polygons_indexed: Vec<Vec<PolyVert>> = Vec::with_capacity(cross_section.len());
    for poly in cross_section {
        n_cross += poly.len() as i64;
        let mut simple = Vec::with_capacity(poly.len());
        for &pv in poly {
            vert_pos.push(Vec3::new(pv.x, pv.y, 0.0));
            simple.push(PolyVert { pos: pv, idx });
            idx += 1;
        }
        polygons_indexed.push(simple);
    }

    for i in 1..=nd {
        let alpha = i as f64 / nd as f64;
        let phi = alpha * twist_degrees;
        // la::lerp(vec2(1.0), scaleTop, alpha) — the a·(1−t) + b·t form, kept verbatim for bit-parity.
        let scale = Vec2::new(
            (1.0 - alpha) + scale_top.x * alpha,
            (1.0 - alpha) + scale_top.y * alpha,
        );
        // transform = diag(scale) · R(phi): four entries FIRST, then column-apply — la's product order,
        // NOT scale-after-rotate (different rounding). The `0.0 *` terms are la's full 2-term dot over
        // the diagonal matrix; they look dead but FIX THE SIGN OF ZERO (`1·(−0.0) + 0.0·c = +0.0`,
        // where the shortcut single product keeps `−0.0`) — one cylinder quadrant vert caught it.
        let (s, c) = (sind(phi), cosd(phi));
        let (t00, t10) = (scale.x * c + 0.0 * s, 0.0 * c + scale.y * s);
        let (t01, t11) = (scale.x * -s + 0.0 * c, 0.0 * -s + scale.y * c);
        let mut j: i64 = 0;
        let mut idx: i64 = 0;
        #[allow(
            clippy::explicit_counter_loop,
            reason = "C++ port parity: the reference walks polys with an explicit running counter; \
                      reshaping to enumerate() obscures the line-for-line correspondence"
        )]
        for poly in cross_section {
            let pn = poly.len() as i64;
            for vert in 0..pn {
                let offset = idx + n_cross * i;
                let this_vert = vert + offset;
                let last_vert = (if vert == 0 { pn } else { vert }) - 1 + offset;
                if i == nd && is_cone {
                    tris.push([
                        (n_cross * i + j) as u32,
                        (last_vert - n_cross) as u32,
                        (this_vert - n_cross) as u32,
                    ]);
                } else {
                    let p = poly[vert as usize];
                    vert_pos.push(Vec3::new(
                        t00 * p.x + t01 * p.y,
                        t10 * p.x + t11 * p.y,
                        height * alpha,
                    ));
                    tris.push([
                        this_vert as u32,
                        last_vert as u32,
                        (this_vert - n_cross) as u32,
                    ]);
                    tris.push([
                        last_vert as u32,
                        (last_vert - n_cross) as u32,
                        (this_vert - n_cross) as u32,
                    ]);
                }
            }
            j += 1;
            idx += pn;
        }
    }
    if is_cone {
        // One apex per contour — the duplicate keeps the genus right (C++ comment: "for Genus").
        for _ in cross_section {
            vert_pos.push(Vec3::new(0.0, 0.0, height));
        }
    }

    // Caps: C++ TriangulateIdx with its defaults — epsilon -1 (the ear-clip self-computes from its
    // bbox) and allowConvex true (convex caps take the alternating fast clip).
    let cap = triangulate_with_convex(&polygons_indexed, -1.0, true);
    for t in &cap {
        tris.push([t[0] as u32, t[2] as u32, t[1] as u32]);
        if !is_cone {
            tris.push([
                (i64::from(t[0]) + n_cross * nd) as u32,
                (i64::from(t[1]) + n_cross * nd) as u32,
                (i64::from(t[2]) + n_cross * nd) as u32,
            ]);
        }
    }

    finish_solid(vert_pos, &tris)
}

/// `Manifold::Revolve` (constructors.cpp:304): revolve raw polygons `revolve_degrees` around the Y-axis,
/// which becomes the Z-axis of the result. Only the positive-X part is used — verts at `x < 0` are
/// dropped, axis crossings interpolated to `x = 0`, and an on-axis vert is placed ONCE and reused across
/// all slices. A partial revolve (`revolve_degrees < 360`; above 360 clamps down like the C++) gets one
/// extra slice ring plus triangulated front/back caps over the clipped profile.
///
/// DEVIATION: `circular_segments < 3` clamps to 3 — the C++ falls back to its `Quality` module scaled by
/// arc fraction, which is unported (every fab caller resolves `$fn` upstream and passes explicit counts).
pub fn revolve_polygons(
    cross_section: &[Vec<Vec2>],
    circular_segments: i32,
    mut revolve_degrees: f64,
) -> Mesh {
    // Axis-clip: keep the positive-X part of each contour, interpolating the x=0 crossings.
    let mut polygons: Vec<Vec<Vec2>> = Vec::new();
    for poly in cross_section {
        let n = poly.len();
        let mut i = 0;
        while i < n && poly[i].x < 0.0 {
            i += 1;
        }
        if i == n {
            continue;
        }
        let mut out = Vec::new();
        let start = i;
        loop {
            if poly[i].x >= 0.0 {
                out.push(poly[i]);
            }
            let next = if i + 1 == n { 0 } else { i + 1 };
            if (poly[next].x < 0.0) != (poly[i].x < 0.0) {
                let y = poly[next].y
                    - poly[next].x * (poly[i].y - poly[next].y) / (poly[i].x - poly[next].x);
                out.push(Vec2::new(0.0, y));
            }
            i = next;
            if i == start {
                break;
            }
        }
        if !out.is_empty() {
            polygons.push(out);
        }
    }
    if polygons.is_empty() {
        return Mesh::default();
    }

    if revolve_degrees > 360.0 {
        revolve_degrees = 360.0;
    }
    let is_full = revolve_degrees == 360.0;

    let n_div = i64::from(circular_segments.max(3));
    // First and last slice are distinguished if not a full revolution.
    let n_slices = if is_full { n_div } else { n_div + 1 };
    let d_phi = revolve_degrees / n_div as f64;

    let mut vert_pos: Vec<Vec3> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut start_poses: Vec<i64> = Vec::new();
    let mut end_poses: Vec<i64> = Vec::new();

    for poly in &polygons {
        let n_pos = poly.iter().filter(|p| p.x > 0.0).count() as i64;
        let n_axis = poly.iter().filter(|p| p.x == 0.0).count() as i64;
        let pn = poly.len();
        for poly_vert in 0..pn {
            let start_i = vert_pos.len() as i64;
            if !is_full {
                start_poses.push(start_i);
            }
            let curr = poly[poly_vert];
            let prev = poly[if poly_vert == 0 {
                pn - 1
            } else {
                poly_vert - 1
            }];
            // Where the PREVIOUS polyVert's ring starts (wrapping to the last vert when poly_vert==0).
            let prev_start = start_i
                + if poly_vert == 0 {
                    n_axis + n_slices * n_pos
                } else {
                    0
                }
                + if prev.x == 0.0 { -1 } else { -n_slices };
            for slice in 0..n_slices {
                let phi = slice as f64 * d_phi;
                if slice == 0 || curr.x > 0.0 {
                    vert_pos.push(Vec3::new(curr.x * cosd(phi), curr.x * sind(phi), curr.y));
                }
                // Full revolution ⇒ emit for every slice (slice 0 wraps to the last); partial ⇒ slice 0
                // only places verts.
                if is_full || slice > 0 {
                    let last = if slice == 0 { n_div } else { slice } - 1;
                    if curr.x > 0.0 {
                        let third = if prev.x == 0.0 {
                            prev_start
                        } else {
                            prev_start + last
                        };
                        tris.push([
                            (start_i + slice) as u32,
                            (start_i + last) as u32,
                            third as u32,
                        ]);
                    }
                    if prev.x > 0.0 {
                        let third = if curr.x == 0.0 {
                            start_i
                        } else {
                            start_i + slice
                        };
                        tris.push([
                            (prev_start + last) as u32,
                            (prev_start + slice) as u32,
                            third as u32,
                        ]);
                    }
                }
            }
            if !is_full {
                end_poses.push(vert_pos.len() as i64 - 1);
            }
        }
    }

    // Front and back caps close a partial revolve: triangulate the CLIPPED profile (flat running
    // indices into start/end_poses), front as-is at phi=0, back reversed at phi=revolve_degrees.
    // Epsilon -1 = the fresh C++ Impl's default epsilon_ at this point in the ctor.
    if !is_full {
        let mut fidx: i32 = 0;
        let polys_indexed: Vec<Vec<PolyVert>> = polygons
            .iter()
            .map(|c| {
                c.iter()
                    .map(|&pos| {
                        let v = PolyVert { pos, idx: fidx };
                        fidx += 1;
                        v
                    })
                    .collect()
            })
            .collect();
        let front = triangulate_with_convex(&polys_indexed, -1.0, true);
        for t in &front {
            tris.push([
                start_poses[t[0] as usize] as u32,
                start_poses[t[1] as usize] as u32,
                start_poses[t[2] as usize] as u32,
            ]);
        }
        for t in &front {
            tris.push([
                end_poses[t[2] as usize] as u32,
                end_poses[t[1] as usize] as u32,
                end_poses[t[0] as usize] as u32,
            ]);
        }
    }

    finish_solid(vert_pos, &tris)
}

impl CrossSection {
    /// Linear extrusion to `height` along +Z — the straight-wall case of [`extrude_polygons`] (C++
    /// `Manifold::Extrude` default args: no divisions, no twist, scale 1). The result is a watertight 3D
    /// solid; empty cross-section or `height <= 0` ⇒ empty mesh.
    pub fn extrude(&self, height: f64) -> Mesh {
        extrude_polygons(&self.contours, height, 0, 0.0, Vec2::new(1.0, 1.0))
    }

    /// The general extrude — divisions/twist/scale-top, see [`extrude_polygons`].
    pub fn extrude_with_options(
        &self,
        height: f64,
        n_divisions: i32,
        twist_degrees: f64,
        scale_top: Vec2,
    ) -> Mesh {
        extrude_polygons(
            &self.contours,
            height,
            n_divisions,
            twist_degrees,
            scale_top,
        )
    }

    /// Solid of revolution around the Y-axis (which becomes +Z), full or partial-angle — see
    /// [`revolve_polygons`].
    pub fn revolve(&self, circular_segments: i32, revolve_degrees: f64) -> Mesh {
        revolve_polygons(&self.contours, circular_segments, revolve_degrees)
    }
}

impl Mesh {
    /// `Manifold::Cylinder` (constructors.cpp:128), verbatim: a `cosd`/`sind` circle at `radius_low`
    /// extruded to `height` with top scale `radius_high / radius_low` — so cones (`radius_high == 0`) are
    /// the extrude's cone path, and an apex-at-BOTTOM cone (`radius_low == 0`) is the flipped one, built
    /// centered and mirrored through z (the winding flip rides [`Mesh::transform`]'s negative-determinant
    /// path). A negative `radius_high` means "same as low" (scale 1). `center` puts z ∈ ±height/2 instead
    /// of [0, height]. Degenerate params (`height <= 0`, `radius_low < 0`, both radii 0) ⇒ empty mesh.
    ///
    /// DEVIATIONS: `circular_segments < 3` clamps to 3 (C++ Quality fallback unported, same as
    /// [`revolve_polygons`]); non-finite params ⇒ empty mesh for panic-safety (the C++ happily builds a
    /// NaN mesh).
    #[allow(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "the guards are deliberately NaN-true: `!(x > 0.0)` rejects NaN where `x <= 0.0` \
                  would admit it — the negated form IS the intent, not a readability accident"
    )]
    pub fn cylinder(
        height: f64,
        radius_low: f64,
        radius_high: f64,
        circular_segments: i32,
        center: bool,
    ) -> Mesh {
        if !(height > 0.0) || !(radius_low >= 0.0) || !height.is_finite() || !radius_low.is_finite()
        {
            return Mesh::default();
        }
        if radius_low == 0.0 {
            if radius_high <= 0.0 || !radius_high.is_finite() {
                return Mesh::default();
            }
            // Cone with apex at bottom: build the centered apex-at-top version and mirror it.
            let cone = Mesh::cylinder(height, radius_high, 0.0, circular_segments, true);
            let mut cone = cone
                .transform(Mat3x4::scale(Vec3::new(1.0, 1.0, -1.0)))
                .expect("finite mirror of a finite cone");
            if !center {
                cone = cone
                    .transform(Mat3x4::translate(Vec3::new(0.0, 0.0, height / 2.0)))
                    .expect("finite translate");
            }
            cone.initialize_original(); // AsOriginal
            return cone;
        }
        if !radius_high.is_finite() {
            return Mesh::default();
        }
        let scale = if radius_high >= 0.0 {
            radius_high / radius_low
        } else {
            1.0
        };
        let n = circular_segments.max(3);
        let d_phi = 360.0 / f64::from(n);
        let circle: Vec<Vec2> = (0..n)
            .map(|i| {
                Vec2::new(
                    radius_low * cosd(d_phi * f64::from(i)),
                    radius_low * sind(d_phi * f64::from(i)),
                )
            })
            .collect();
        let cyl = extrude_polygons(&[circle], height, 0, 0.0, Vec2::new(scale, scale));
        if center {
            let mut c = cyl
                .transform(Mat3x4::translate(Vec3::new(0.0, 0.0, -height / 2.0)))
                .expect("finite translate");
            c.initialize_original(); // AsOriginal
            c
        } else {
            cyl
        }
    }

    /// A sphere of `radius` with `circular_segments` around the equator.
    ///
    /// DEVIATION (deliberate, M.7.3): C++ `Manifold::Sphere` is a warped subdivided OCTAHEDRON, which
    /// would drag in the whole `Subdivide` module; ours is a UV sphere — a revolved semicircle with
    /// `max(2, ⌈segments/2⌉)` latitude bands. Its only consumers are fab-scad's own connector solids
    /// (onion, bolt_clearance), gated by tolerance/property tests; OpenSCAD `sphere()` never routes here
    /// (fab-lang tessellates it itself). The profile endpoints land EXACTLY on the revolve axis
    /// (`cosd(±90) == 0`), so the poles close via the on-axis vertex-reuse path.
    #[allow(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "deliberately NaN-true guard, as in `cylinder` above"
    )]
    pub fn sphere(radius: f64, circular_segments: i32) -> Mesh {
        if !(radius > 0.0) || !radius.is_finite() {
            return Mesh::default();
        }
        let segments = circular_segments.max(3);
        let lat = i64::from((segments + 1) / 2).max(2);
        // Semicircle from the south pole (0, -r) through (+r, 0) to the north pole (0, r), wound CCW;
        // the implied closing edge runs down the axis.
        let profile: Vec<Vec2> = (0..=lat)
            .map(|k| {
                let theta = -90.0 + 180.0 * k as f64 / lat as f64;
                Vec2::new(radius * cosd(theta), radius * sind(theta))
            })
            .collect();
        revolve_polygons(&[profile], segments, 360.0)
    }
}

impl Mesh {
    /// Project the mesh onto the XY plane (Manifold `Project`) — the 2D silhouette / footprint. Every
    /// triangle projects to a 2D triangle (oriented CCW); the whole batch feeds ONE i_overlay Positive-fill
    /// pass, so overlapping projections union into the outline (a downward-facing tri and its upward
    /// partner cover the same 2D region ⇒ they merge). Degenerate (edge-on) triangles project to zero area
    /// and drop out. Errs (`NonFiniteVertex`) on a mesh carrying non-finite positions — `from_mesh_gl`'s
    /// ingest is topology-only, so such meshes exist; the 2D boundary rejects them (M.5.4.5).
    pub fn project(&self) -> Result<CrossSection, crate::status::Error> {
        let mut polys: Vec<Vec<Vec2>> = Vec::with_capacity(self.num_tri());
        for tri in 0..self.num_tri() {
            let t = TriId::from_usize(tri);
            let p: [Vec2; 3] = [0, 1, 2].map(|i| {
                let v = self.pos(self.start(t.halfedge(i)));
                Vec2::new(v.x, v.y)
            });
            // Signed area; skip edge-on triangles; orient CCW so Positive fill accumulates coverage.
            let a2 = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[2].x - p[0].x) * (p[1].y - p[0].y);
            if a2.abs() < 1e-12 {
                continue;
            }
            polys.push(if a2 < 0.0 {
                vec![p[0], p[2], p[1]]
            } else {
                p.to_vec()
            });
        }
        CrossSection::from_polygons(&polys)
    }

    /// The 2D cross-section of the mesh at the plane `z = height` (Manifold `Slice`). Marching-triangles
    /// contour trace: for each triangle straddling the plane (`min_z ≤ height < max_z`), walk the
    /// below→above edge crossings across paired triangles until the contour closes. A `BTreeSet` of
    /// crossing triangles makes the contour order (and the trace) deterministic (native==wasm, run-to-run).
    pub fn slice_at_z(&self, height: f64) -> Result<CrossSection, crate::status::Error> {
        use crate::mesh_ids::HalfedgeId;
        let z = |he: usize| self.pos(self.start(HalfedgeId::from_usize(he))).z;

        let mut crossing = std::collections::BTreeSet::new();
        for tri in 0..self.num_tri() {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for j in 0..3 {
                let zj = z(3 * tri + j);
                lo = lo.min(zj);
                hi = hi.max(zj);
            }
            if lo <= height && hi > height {
                crossing.insert(tri);
            }
        }

        let mut polys: Vec<Vec<Vec2>> = Vec::new();
        while let Some(&start_tri) = crossing.iter().next() {
            let mut poly: Vec<Vec2> = Vec::new();
            // Entry corner: the vert above the plane whose next vert is at/below it.
            let mut k = 0;
            for j in 0..3 {
                if z(3 * start_tri + j) > height && z(3 * start_tri + (j + 1) % 3) <= height {
                    k = (j + 1) % 3;
                    break;
                }
            }
            let mut tri = start_tri;
            loop {
                crossing.remove(&tri);
                // Advance k to the below→above ("up") edge, then record its crossing point.
                if z(3 * tri + (k + 1) % 3) <= height {
                    k = (k + 1) % 3;
                }
                let up = HalfedgeId::from_usize(3 * tri + k);
                let below = self.pos(self.start(up));
                let above = self.pos(self.end(up));
                let a = (height - below.z) / (above.z - below.z);
                let cross = below + (above - below) * a;
                poly.push(Vec2::new(cross.x, cross.y));
                // Cross into the paired triangle.
                let pair = self.pair(up).u();
                tri = pair / 3;
                k = (pair % 3 + 1) % 3;
                if tri == start_tri {
                    break;
                }
            }
            polys.push(poly);
        }
        CrossSection::from_polygons(&polys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linalg::Vec2;

    fn square(x: f64, y: f64, s: f64) -> Vec<Vec2> {
        vec![
            Vec2::new(x, y),
            Vec2::new(x + s, y),
            Vec2::new(x + s, y + s),
            Vec2::new(x, y + s),
        ]
    }

    #[test]
    fn extrude_square_is_a_box() {
        // A 2×2 square extruded to height 3 → a 2×2×3 box, volume 12, genus 0, watertight.
        let cs = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)]).unwrap();
        let solid = cs.extrude(3.0);
        assert!(
            solid.is_manifold(),
            "extruded box must be a watertight manifold"
        );
        assert_eq!(crate::check::genus(&solid), 0, "a box is genus 0");
        assert!(
            (solid.volume() - 12.0).abs() < 1e-9,
            "extrude volume {} != 12",
            solid.volume()
        );
    }

    #[test]
    fn extrude_holed_is_a_tube() {
        // A 10×10 square with a 2×2 hole, extruded to height 1 → a tube: volume (100−4)·1 = 96, genus 1.
        let outer = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)]).unwrap();
        let inner = CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]).unwrap();
        let ring = outer.difference(&inner);
        assert_eq!(ring.num_contour(), 2, "ring = outer + hole");
        let tube = ring.extrude(1.0);
        assert!(
            tube.is_manifold(),
            "holed extrude must be a watertight manifold"
        );
        assert!(
            (tube.volume() - 96.0).abs() < 1e-9,
            "tube volume {} != 96",
            tube.volume()
        );
        assert_eq!(crate::check::genus(&tube), 1, "a tube is genus 1");
    }

    #[test]
    fn revolve_square_is_a_cylinder() {
        // Revolve the unit square [0,1]×[0,1] (touching the Y-axis at x=0) → a solid cylinder radius 1,
        // height 1. Exercises the on-axis vertex reuse. Volume ≈ π (inscribed N-gon for N segments).
        let cyl = CrossSection::from_polygons(&[square(0.0, 0.0, 1.0)])
            .unwrap()
            .revolve(128, 360.0);
        assert!(
            cyl.is_manifold(),
            "revolved cylinder must be a watertight manifold"
        );
        assert_eq!(crate::check::genus(&cyl), 0, "a solid cylinder is genus 0");
        assert!(
            (cyl.volume() - core::f64::consts::PI).abs() < 1e-2,
            "cylinder volume {} vs ~π",
            cyl.volume()
        );
    }

    #[test]
    fn revolve_offset_square_is_a_torus_tube() {
        // Revolve a square at x∈[1,2] (off the axis) → an annular cylinder (tube), inner r=1, outer r=2,
        // height 1 → genus 1. Volume ≈ π(2²−1²)·1 = 3π.
        let ring = CrossSection::from_polygons(&[square(1.0, 0.0, 1.0)])
            .unwrap()
            .revolve(128, 360.0);
        assert!(ring.is_manifold(), "off-axis revolve must be manifold");
        assert_eq!(
            crate::check::genus(&ring),
            1,
            "an annular cylinder is genus 1"
        );
        assert!(
            (ring.volume() - 3.0 * core::f64::consts::PI).abs() < 3e-2,
            "tube volume {} vs ~3π",
            ring.volume()
        );
    }

    #[test]
    fn project_box_is_its_footprint() {
        // A 2×2×3 box projected onto XY → its 2×2 base square, area 4. Vertical walls project to lines
        // (zero area) and drop; the caps give the footprint.
        let box3 = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)])
            .unwrap()
            .extrude(3.0);
        let shadow = box3.project().unwrap();
        assert!(
            (shadow.area() - 4.0).abs() < 1e-9,
            "box footprint area {} != 4",
            shadow.area()
        );
    }

    #[test]
    fn project_tube_keeps_hole() {
        // A tube projected → a ring (the hole survives in the silhouette), area 96.
        let ring = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)])
            .unwrap()
            .difference(&CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]).unwrap());
        let shadow = ring.extrude(1.0).project().unwrap();
        assert!(
            (shadow.area() - 96.0).abs() < 1e-9,
            "tube footprint area {} != 96",
            shadow.area()
        );
    }

    #[test]
    fn slice_box_and_tube() {
        // A 2×2×3 box sliced at z=1.5 → a 2×2 square, area 4.
        let box3 = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)])
            .unwrap()
            .extrude(3.0);
        assert!(
            (box3.slice_at_z(1.5).unwrap().area() - 4.0).abs() < 1e-9,
            "box slice != 4"
        );
        // A tube sliced mid-height → a ring (hole survives), area 96.
        let tube = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)])
            .unwrap()
            .difference(&CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]).unwrap())
            .extrude(2.0);
        let cut = tube.slice_at_z(1.0).unwrap();
        assert!(
            (cut.area() - 96.0).abs() < 1e-9,
            "tube slice area {} != 96",
            cut.area()
        );
        assert_eq!(cut.num_contour(), 2, "ring slice = outer + hole");
    }

    #[test]
    fn extrude_degenerate_is_empty() {
        assert!(
            CrossSection::new().extrude(1.0).is_empty(),
            "empty cross-section ⇒ empty"
        );
        let sq = CrossSection::from_polygons(&[square(0.0, 0.0, 1.0)]).unwrap();
        assert!(sq.extrude(0.0).is_empty(), "height 0 ⇒ empty");
    }

    #[test]
    fn extrude_twisted_keeps_volume_and_topology() {
        // A twisted prism's slices are rotated copies (area preserved), but the triangulated walls
        // between 11.25°-apart slices bulge OUTWARD like an antiprism, so the polyhedron measures
        // ~5% over base·height: 12.6338 for a 2×2 square, 90° over 8 divisions — the exact figure is
        // C++-byte-confirmed by `m7_3_extrude_options_vs_cpp`.
        let cs = CrossSection::from_polygons(&[square(-1.0, -1.0, 2.0)]).unwrap();
        let m = cs.extrude_with_options(3.0, 8, 90.0, Vec2::new(1.0, 1.0));
        assert!(m.is_manifold(), "twisted extrude must be watertight");
        assert_eq!(crate::check::genus(&m), 0);
        assert!(
            (m.volume() - 12.6338).abs() < 1e-3,
            "twisted antiprism volume {} !≈ 12.6338",
            m.volume()
        );
    }

    #[test]
    fn extrude_scaled_is_a_frustum() {
        // Linear taper 1 → 0.5 over height 3: a square frustum. V = h/3·(A0 + A1 + √(A0·A1))
        // with A0 = 4, A1 = 1 → 3/3·(4 + 1 + 2) = 7.
        let cs = CrossSection::from_polygons(&[square(-1.0, -1.0, 2.0)]).unwrap();
        let m = cs.extrude_with_options(3.0, 4, 0.0, Vec2::new(0.5, 0.5));
        assert!(m.is_manifold());
        assert!(
            (m.volume() - 7.0).abs() < 1e-9,
            "frustum volume {} != 7",
            m.volume()
        );
    }

    #[test]
    fn extrude_scale_zero_is_a_cone() {
        // scaleTop (0,0) → pyramid over the square base: V = A·h/3 = 4·3/3 = 4. The apex path
        // dedups verts (one apex per contour), so the solid is genus 0 and watertight.
        let cs = CrossSection::from_polygons(&[square(-1.0, -1.0, 2.0)]).unwrap();
        let m = cs.extrude_with_options(3.0, 4, 0.0, Vec2::new(0.0, 0.0));
        assert!(m.is_manifold(), "cone extrude must be watertight");
        assert_eq!(crate::check::genus(&m), 0);
        assert!(
            (m.volume() - 4.0).abs() < 1e-9,
            "pyramid volume {} != 4",
            m.volume()
        );
    }

    #[test]
    fn revolve_partial_is_a_wedge() {
        // 90° revolve of the unit square touching the axis → a quarter cylinder: V = π/4 (of the
        // inscribed N-gon's quarter), genus 0, watertight — the front/back caps close it.
        let quarter = CrossSection::from_polygons(&[square(0.0, 0.0, 1.0)])
            .unwrap()
            .revolve(128, 90.0);
        assert!(quarter.is_manifold(), "partial revolve must be watertight");
        assert_eq!(crate::check::genus(&quarter), 0);
        // NOTE the segment count spans the ARC, not a full turn (C++ semantics): 128 segments over
        // 90° tessellates 4× finer than a full revolve's quarter, so compare analytic, not full/4.
        assert!(
            (quarter.volume() - core::f64::consts::PI / 4.0).abs() < 1e-3,
            "quarter-cylinder volume {} vs ~π/4",
            quarter.volume()
        );
    }

    #[test]
    fn revolve_partial_off_axis_keeps_caps() {
        // 180° revolve of an off-axis square → half an annular tube, genus 0 (the caps close what
        // the full revolution's genus-1 hole would be).
        let half = CrossSection::from_polygons(&[square(1.0, 0.0, 1.0)])
            .unwrap()
            .revolve(64, 180.0);
        assert!(half.is_manifold());
        assert_eq!(crate::check::genus(&half), 0);
        assert!(
            (half.volume() - 1.5 * core::f64::consts::PI).abs() < 4e-2,
            "half-tube volume {} vs ~3π/2",
            half.volume()
        );
    }

    #[test]
    fn cylinder_cone_and_centering() {
        // Straight cylinder r=2 h=3: V ≈ π·4·3. Inscribed 64-gon slightly less.
        let cyl = Mesh::cylinder(3.0, 2.0, -1.0, 64, false);
        assert!(cyl.is_manifold());
        assert!(
            (cyl.volume() - 12.0 * core::f64::consts::PI).abs() < 0.2,
            "cylinder volume {} vs ~12π",
            cyl.volume()
        );
        assert!(
            cyl.b_box.min.z.abs() < 1e-12 && (cyl.b_box.max.z - 3.0).abs() < 1e-12,
            "uncentered cylinder spans [0, h]"
        );
        let centered = Mesh::cylinder(3.0, 2.0, 2.0, 64, true);
        assert!(
            (centered.b_box.min.z + 1.5).abs() < 1e-12
                && (centered.b_box.max.z - 1.5).abs() < 1e-12,
            "centered cylinder spans ±h/2"
        );
        // Apex-at-top cone (r_high = 0) and apex-at-BOTTOM cone (r_low = 0, the mirrored path):
        // same volume, mirrored bbox behavior.
        let cone_up = Mesh::cylinder(3.0, 2.0, 0.0, 64, false);
        let cone_down = Mesh::cylinder(3.0, 0.0, 2.0, 64, false);
        assert!(cone_up.is_manifold() && cone_down.is_manifold());
        assert!(
            (cone_up.volume() - cone_down.volume()).abs() < 1e-9,
            "mirrored cones must match: up {} down {}",
            cone_up.volume(),
            cone_down.volume()
        );
        assert!(
            (cone_up.volume() - 4.0 * core::f64::consts::PI).abs() < 0.1,
            "cone volume {} vs ~4π",
            cone_up.volume()
        );
        assert!(
            cone_down.b_box.min.z.abs() < 1e-12 && (cone_down.b_box.max.z - 3.0).abs() < 1e-12,
            "uncentered apex-at-bottom cone still spans [0, h]"
        );
        // Degenerates: zero/negative height, negative r_low, both radii zero.
        assert!(Mesh::cylinder(0.0, 1.0, 1.0, 8, false).is_empty());
        assert!(Mesh::cylinder(1.0, -1.0, 1.0, 8, false).is_empty());
        assert!(Mesh::cylinder(1.0, 0.0, 0.0, 8, false).is_empty());
    }

    #[test]
    fn sphere_is_round_watertight_and_converges() {
        let r = 2.0;
        let analytic = 4.0 / 3.0 * core::f64::consts::PI * r * r * r;
        let coarse = Mesh::sphere(r, 16);
        let fine = Mesh::sphere(r, 128);
        for (label, s) in [("coarse", &coarse), ("fine", &fine)] {
            assert!(s.is_manifold(), "{label} sphere must be watertight");
            assert_eq!(crate::check::genus(s), 0, "{label} sphere is genus 0");
            assert!(
                s.volume() < analytic,
                "{label}: inscribed tessellation stays under the analytic volume"
            );
        }
        let coarse_err = analytic - coarse.volume();
        let fine_err = analytic - fine.volume();
        assert!(
            fine_err < coarse_err / 10.0,
            "volume must converge to analytic: coarse err {coarse_err}, fine err {fine_err}"
        );
        // Equator vert count honors `segments`: r·(cos, sin) ring at z=0 with `segments` verts.
        let eq = coarse.vert_pos.iter().filter(|p| p.z.abs() < 1e-12).count();
        assert_eq!(eq, 16, "16 verts around the equator");
        assert!(Mesh::sphere(0.0, 16).is_empty(), "r 0 ⇒ empty");
        assert!(Mesh::sphere(f64::NAN, 16).is_empty(), "NaN r ⇒ empty");
    }
}
