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

        // Cap triangulation over the bottom verts (idx 0..n_cross), with the contour scale's epsilon.
        let scale = self
            .bounds()
            .map(|(min, max)| (max.x - min.x).abs().max((max.y - min.y).abs()))
            .unwrap_or(1.0);
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

        let mut mesh = Mesh { vert_pos, num_prop: 0, ..Default::default() };
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
    /// and drop out.
    pub fn project(&self) -> CrossSection {
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
            polys.push(if a2 < 0.0 { vec![p[0], p[2], p[1]] } else { p.to_vec() });
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
        let cs = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)]);
        let solid = cs.extrude(3.0);
        assert!(solid.is_manifold(), "extruded box must be a watertight manifold");
        assert_eq!(crate::check::genus(&solid), 0, "a box is genus 0");
        assert!((solid.volume() - 12.0).abs() < 1e-9, "extrude volume {} != 12", solid.volume());
    }

    #[test]
    fn extrude_holed_is_a_tube() {
        // A 10×10 square with a 2×2 hole, extruded to height 1 → a tube: volume (100−4)·1 = 96, genus 1.
        let outer = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)]);
        let inner = CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]);
        let ring = outer.difference(&inner);
        assert_eq!(ring.num_contour(), 2, "ring = outer + hole");
        let tube = ring.extrude(1.0);
        assert!(tube.is_manifold(), "holed extrude must be a watertight manifold");
        assert!((tube.volume() - 96.0).abs() < 1e-9, "tube volume {} != 96", tube.volume());
        assert_eq!(crate::check::genus(&tube), 1, "a tube is genus 1");
    }

    #[test]
    fn project_box_is_its_footprint() {
        // A 2×2×3 box projected onto XY → its 2×2 base square, area 4. Vertical walls project to lines
        // (zero area) and drop; the caps give the footprint.
        let box3 = CrossSection::from_polygons(&[square(0.0, 0.0, 2.0)]).extrude(3.0);
        let shadow = box3.project();
        assert!((shadow.area() - 4.0).abs() < 1e-9, "box footprint area {} != 4", shadow.area());
    }

    #[test]
    fn project_tube_keeps_hole() {
        // A tube projected → a ring (the hole survives in the silhouette), area 96.
        let ring = CrossSection::from_polygons(&[square(0.0, 0.0, 10.0)])
            .difference(&CrossSection::from_polygons(&[square(4.0, 4.0, 2.0)]));
        let shadow = ring.extrude(1.0).project();
        assert!((shadow.area() - 96.0).abs() < 1e-9, "tube footprint area {} != 96", shadow.area());
    }

    #[test]
    fn extrude_degenerate_is_empty() {
        assert!(CrossSection::new().extrude(1.0).is_empty(), "empty cross-section ⇒ empty");
        let sq = CrossSection::from_polygons(&[square(0.0, 0.0, 1.0)]);
        assert!(sq.extrude(0.0).is_empty(), "height 0 ⇒ empty");
    }
}
