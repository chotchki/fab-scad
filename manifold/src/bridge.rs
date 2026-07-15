//! The 2D↔3D bridges — Manifold's `Extrude` / `Revolve` (2D→3D) and `Project` / `Slice` (3D→2D), R5/M.5.
//! These are the ops that UNBLOCK M.3.8 (the OpenSCAD `linear_extrude`/`rotate_extrude`/`projection`).
//!
//! The caps reuse the 3D `polygon.rs` triangulator (the same Delaunay-cost ear-clip the boolean leans on);
//! the walls are quads. `CrossSection` is the i_overlay-backed 2D type (area-residual gated); the produced
//! `Mesh` is a normal 3D solid that flows through the byte-exact 3D pipeline (`sort_geometry` etc.).

use crate::boolean::polygon::{PolyVert, triangulate};
use crate::cross_section::CrossSection;
use crate::linalg::{Vec2, Vec3};
use crate::mesh::Mesh;
use crate::mesh_ids::TriId;

impl CrossSection {
    /// Linear extrusion to `height` along +Z (Manifold `Extrude(cs, height)` — the straight-wall case, no
    /// twist / scale / cone). Bottom cap at `z = 0` (wound down), top cap at `z = height` (wound up),
    /// side-wall quads per contour edge. Empty cross-section or `height <= 0` ⇒ empty mesh (Manifold
    /// `Invalid`). The result is a watertight 3D solid.
    pub fn extrude(&self, height: f64) -> Mesh {
        if self.is_empty() || height <= 0.0 {
            return Mesh::default();
        }
        let n_cross = self.num_vert();

        // Verts: bottom (z=0) then top (z=height), each in flat contour order — so top[k] = bottom[k] +
        // n_cross (mirrors C++'s index arithmetic).
        let mut vert_pos: Vec<Vec3> = Vec::with_capacity(2 * n_cross);
        for c in &self.contours {
            for &p in c {
                vert_pos.push(Vec3::new(p.x, p.y, 0.0));
            }
        }
        for c in &self.contours {
            for &p in c {
                vert_pos.push(Vec3::new(p.x, p.y, height));
            }
        }

        // Cap triangulation over the bottom verts (idx 0..n_cross), with the contour scale's
        // epsilon. Guard on verts, not bounds: an EMPTY cross-section's bounds are the
        // all-encompassing rect (the ported C++ quirk), whose size overflows.
        let scale = if self.num_vert() == 0 {
            1.0
        } else {
            let s = self.bounds().size();
            s.x.abs().max(s.y.abs())
        };
        let eps = 1e-12 * scale.max(1.0);
        let mut idx: i32 = 0;
        let polys: Vec<Vec<PolyVert>> = self
            .contours
            .iter()
            .map(|c| {
                c.iter()
                    .map(|&pos| {
                        let v = PolyVert { pos, idx };
                        idx += 1;
                        v
                    })
                    .collect()
            })
            .collect();
        let cap = triangulate(&polys, eps);

        let nc = n_cross as u32;
        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(2 * n_cross + 2 * cap.len());

        // Side walls: per contour edge (prev → this), a quad = 2 tris (C++ winding, faces outward).
        let mut offset: u32 = 0;
        for c in &self.contours {
            let m = c.len() as u32;
            for vert in 0..m {
                let this_bot = offset + vert;
                let last_bot = offset + (vert + m - 1) % m;
                let this_top = this_bot + nc;
                let last_top = last_bot + nc;
                tris.push([this_top, last_top, this_bot]);
                tris.push([last_top, last_bot, this_bot]);
            }
            offset += m;
        }

        // Bottom cap: reversed winding (faces −Z). Top cap: +n_cross, original winding (faces +Z).
        for t in &cap {
            tris.push([t[0] as u32, t[2] as u32, t[1] as u32]);
            tris.push([t[0] as u32 + nc, t[1] as u32 + nc, t[2] as u32 + nc]);
        }

        let mut mesh = Mesh {
            vert_pos,
            num_prop: 0,
            ..Default::default()
        };
        mesh.create_halfedges(&tris);
        mesh.initialize_original();
        mesh.calculate_bbox();
        mesh.set_epsilon(-1.0, false);
        mesh.sort_geometry();
        mesh.set_normals_and_coplanar();
        mesh
    }

    /// Solid of revolution: revolve the cross-section a FULL 360° around the Y-axis, which becomes the
    /// Z-axis of the result (Manifold `Revolve`, the full-revolution case). Only the positive-X part is
    /// used — verts at `x < 0` are dropped, axis crossings interpolated to `x = 0`, and an on-axis vert is
    /// placed ONCE and reused across all slices (so the surface closes cleanly at the axis).
    /// `circular_segments` = segments around. Partial revolves (with front/back caps) are a follow-on.
    pub fn revolve(&self, circular_segments: i32) -> Mesh {
        // Axis-clip: keep the positive-X part of each contour, interpolating the x=0 crossings.
        let mut polygons: Vec<Vec<Vec2>> = Vec::new();
        for poly in &self.contours {
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

        let n_div = circular_segments.max(3) as i64;
        let n_slices = n_div; // full revolution
        let d_phi = 360.0 / n_div as f64;

        let mut vert_pos: Vec<Vec3> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();

        for poly in &polygons {
            let n_pos = poly.iter().filter(|p| p.x > 0.0).count() as i64;
            let n_axis = poly.iter().filter(|p| p.x == 0.0).count() as i64;
            let pn = poly.len();
            for poly_vert in 0..pn {
                let start_i = vert_pos.len() as i64;
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
                        let rad = phi * core::f64::consts::PI / 180.0;
                        vert_pos.push(Vec3::new(
                            curr.x * crate::mathf::cos(rad),
                            curr.x * crate::mathf::sin(rad),
                            curr.y,
                        ));
                    }
                    // Full revolution ⇒ emit for every slice; slice 0 wraps to the last slice.
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
        }

        let mut mesh = Mesh {
            vert_pos,
            num_prop: 0,
            ..Default::default()
        };
        mesh.create_halfedges(&tris);
        mesh.initialize_original();
        mesh.calculate_bbox();
        mesh.set_epsilon(-1.0, false);
        mesh.sort_geometry();
        mesh.set_normals_and_coplanar();
        mesh
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
            .revolve(128);
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
            .revolve(128);
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
}
