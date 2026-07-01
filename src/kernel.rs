//! The in-process geometry kernel (Track C) — a typed Rust wrapper over Manifold (`manifold3d`),
//! the same CSG engine OpenSCAD's Manifold backend uses. This is the seam that lets fab do slicing +
//! connector CSG WITHOUT shelling out per piece: a re-slice is an in-process boolean on a cached mesh
//! (~ms), not a process spawn (~hundreds of ms). OpenSCAD stays the SCAD→mesh front-door (see
//! `docs/manifold-kernel-spike.md` for the go/no-go); this owns everything downstream of the base mesh.
//!
//! [`Solid`] is a newtype around a Manifold handle so the rest of fab talks in one strongly-typed
//! shape instead of raw bindings. Import (11.2), STL/3mf export (11.3), the slicer (11.4), and the
//! connectors (11.6) build on it.

use anyhow::{anyhow, Context, Result};
use manifold3d::{Manifold, MeshGL};
use std::collections::HashMap;
use std::path::Path;

/// A closed, manifold 3D solid — the unit every kernel op consumes and produces.
#[derive(Clone)]
pub struct Solid(Manifold);

impl Solid {
    /// Wrap a raw Manifold (import/slicer internals build these). Used by 11.2 import / 11.4 slicer.
    #[allow(dead_code)]
    pub(crate) fn from_manifold(m: Manifold) -> Self {
        Solid(m)
    }

    /// Borrow the underlying handle (for ops the wrapper doesn't surface yet). Used by 11.3 export.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Manifold {
        &self.0
    }

    /// An axis-aligned box. `center` puts the centroid at the origin (else the min corner).
    pub fn cube(x: f64, y: f64, z: f64, center: bool) -> Self {
        Solid(Manifold::cube(x, y, z, center))
    }

    /// A UV sphere of `radius` with `segments` around the equator.
    pub fn sphere(radius: f64, segments: i32) -> Self {
        Solid(Manifold::sphere(radius, segments))
    }

    // --- import (11.2) ---------------------------------------------------------------------------

    /// Load an STL file (binary or ASCII) as a Solid — the front-door for a mesh OpenSCAD rendered.
    pub fn from_stl_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading STL {}", path.display()))?;
        Self::from_stl_bytes(&bytes).with_context(|| format!("importing STL {}", path.display()))
    }

    /// Load an STL from bytes: parse the triangle soup, weld coincident verts by exact bits (OpenSCAD
    /// emits bit-identical shared verts), and build a manifold Solid. Errors if the welded mesh still
    /// isn't a valid 2-manifold — the guarantee every downstream boolean relies on.
    pub fn from_stl_bytes(bytes: &[u8]) -> Result<Self> {
        let soup = read_stl_soup(bytes)?;
        if soup.is_empty() {
            return Err(anyhow!("STL has no triangles"));
        }
        // Exact-bits weld: coincident verts collapse to one index, giving Manifold the shared
        // topology it needs (raw per-triangle soup reads as open edges everywhere).
        let mut map: HashMap<[u32; 3], u32> = HashMap::new();
        let mut verts: Vec<f32> = Vec::new();
        let mut idx: Vec<u32> = Vec::with_capacity(soup.len());
        for p in &soup {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let id = *map.entry(key).or_insert_with(|| {
                verts.extend_from_slice(p);
                (verts.len() / 3 - 1) as u32
            });
            idx.push(id);
        }
        let mesh = MeshGL::new(&verts, 3, &idx).map_err(|e| anyhow!("building mesh: {e:?}"))?;
        let m = Manifold::from_meshgl(&mesh)
            .map_err(|e| anyhow!("STL is not a valid manifold after weld: {e:?}"))?;
        Ok(Solid(m))
    }

    // --- booleans --------------------------------------------------------------------------------

    pub fn union(&self, other: &Solid) -> Solid {
        Solid(self.0.union(&other.0))
    }
    pub fn difference(&self, other: &Solid) -> Solid {
        Solid(self.0.difference(&other.0))
    }
    pub fn intersection(&self, other: &Solid) -> Solid {
        Solid(self.0.intersection(&other.0))
    }

    /// Union many solids at once (cheaper + more robust than folding `union`). Empty ⇒ empty solid.
    pub fn batch_union(solids: &[Solid]) -> Solid {
        let hs: Vec<Manifold> = solids.iter().map(|s| s.0.clone()).collect();
        Solid(Manifold::batch_union(&hs))
    }

    // --- transforms ------------------------------------------------------------------------------

    pub fn translate(&self, x: f64, y: f64, z: f64) -> Solid {
        Solid(self.0.translate(x, y, z))
    }
    /// Rotate by Euler angles in DEGREES (X then Y then Z).
    pub fn rotate(&self, x_deg: f64, y_deg: f64, z_deg: f64) -> Solid {
        Solid(self.0.rotate(x_deg, y_deg, z_deg))
    }
    /// Apply a 3×4 affine (column-major 12-float, as Manifold expects).
    pub fn transform(&self, m: &[f64; 12]) -> Solid {
        Solid(self.0.transform(m))
    }

    // --- half-space cuts (the slicer primitives, 11.4) -------------------------------------------

    /// Split by the plane `normal·p = offset` into `(positive, negative)` — the positive half is the
    /// `normal·p > offset` side. Both halves are independent solids; this is the slicer primitive
    /// (11.4), preferred over `trim_by_plane` because both sides come back clean.
    pub fn split_by_plane(&self, normal: [f64; 3], offset: f64) -> (Solid, Solid) {
        let (pos, neg) = self.0.split_by_plane(normal, offset);
        (Solid(pos), Solid(neg))
    }
    /// Keep only the `normal·p > offset` half (drops the rest). NOTE upstream #1516: trimmed halves
    /// may not re-union cleanly (coincident faces) — use `split_by_plane` when you need both sides.
    pub fn trim_by_plane(&self, normal: [f64; 3], offset: f64) -> Solid {
        Solid(self.0.trim_by_plane(normal, offset))
    }

    // --- queries ---------------------------------------------------------------------------------

    /// Err if the solid isn't a valid 2-manifold — the gate a slice/connector result must pass.
    pub fn check(&self) -> Result<()> {
        self.0.status().map_err(|e| anyhow!("non-manifold solid: {e:?}"))
    }
    pub fn is_manifold(&self) -> bool {
        self.0.status().is_ok()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn num_tri(&self) -> usize {
        self.0.num_tri()
    }
    pub fn num_vert(&self) -> usize {
        self.0.num_vert()
    }

    /// `(min, max)` corners, or None when empty.
    pub fn bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        self.0.bounding_box().map(|b| (b.min(), b.max()))
    }
}

/// Parse an STL (binary or ASCII) into a flat triangle soup (3 verts/triangle, dup'd at shared
/// edges). Binary is trusted only when the size matches the exact `84 + 50n` layout — the same guard
/// the smoke oracle uses so an ASCII file that happens to be ≥84 bytes doesn't read as binary.
fn read_stl_soup(bytes: &[u8]) -> Result<Vec<[f32; 3]>> {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if bytes.len() == 84 + 50 * n {
            let mut out = Vec::with_capacity(n * 3);
            for t in 0..n {
                let base = 84 + t * 50 + 12; // skip the 12-byte face normal
                for v in 0..3 {
                    let o = base + v * 12;
                    let f = |k: usize| {
                        f32::from_le_bytes([bytes[o + k], bytes[o + k + 1], bytes[o + k + 2], bytes[o + k + 3]])
                    };
                    out.push([f(0), f(4), f(8)]);
                }
            }
            return Ok(out);
        }
    }
    // ASCII: every `vertex x y z`, in file order (three make a triangle).
    let text = std::str::from_utf8(bytes).context("STL is neither valid binary nor UTF-8 ASCII")?;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("vertex ") {
            let mut it = rest.split_whitespace().map(str::parse::<f32>);
            match (it.next(), it.next(), it.next()) {
                (Some(Ok(x)), Some(Ok(y)), Some(Ok(z))) => out.push([x, y, z]),
                _ => return Err(anyhow!("malformed ASCII STL vertex: {line:?}")),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A unit tetrahedron (4 verts, 4 faces) as triangle soup — welds 12 soup verts down to 4.
    const TETRA: [[[f32; 3]; 3]; 4] = [
        [[0., 0., 0.], [0., 1., 0.], [1., 0., 0.]],
        [[0., 0., 0.], [1., 0., 0.], [0., 0., 1.]],
        [[0., 0., 0.], [0., 0., 1.], [0., 1., 0.]],
        [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
    ];

    fn binary_stl(tris: &[[[f32; 3]; 3]]) -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for t in tris {
            b.extend_from_slice(&[0u8; 12]); // normal (ignored on read)
            for v in t {
                for c in v {
                    b.extend_from_slice(&c.to_le_bytes());
                }
            }
            b.extend_from_slice(&[0u8; 2]); // attr byte count
        }
        b
    }

    #[test]
    fn welds_a_binary_soup_into_a_manifold() {
        let s = Solid::from_stl_bytes(&binary_stl(&TETRA)).unwrap();
        assert_eq!(s.num_vert(), 4, "12 soup verts should weld to 4 corners");
        assert_eq!(s.num_tri(), 4);
        s.check().unwrap();
    }

    #[test]
    fn parses_ascii_stl_equivalently() {
        let mut ascii = String::from("solid t\n");
        for t in &TETRA {
            ascii.push_str("facet normal 0 0 0\n outer loop\n");
            for v in t {
                ascii.push_str(&format!("  vertex {} {} {}\n", v[0], v[1], v[2]));
            }
            ascii.push_str(" endloop\n endfacet\n");
        }
        ascii.push_str("endsolid t\n");
        let s = Solid::from_stl_bytes(ascii.as_bytes()).unwrap();
        assert_eq!((s.num_vert(), s.num_tri()), (4, 4));
        s.check().unwrap();
    }

    #[test]
    fn rejects_a_non_manifold_open_mesh() {
        // One lone triangle — three open edges, not a closed solid.
        let err = Solid::from_stl_bytes(&binary_stl(&TETRA[..1])).err().expect("should reject");
        assert!(format!("{err:#}").contains("not a valid manifold"), "got: {err:#}");
    }

    #[test]
    fn cube_union_is_a_valid_solid() {
        let a = Solid::cube(40.0, 40.0, 40.0, true);
        let b = Solid::cube(30.0, 30.0, 30.0, true).translate(15.0, 0.0, 0.0);
        a.check().unwrap();
        let u = a.union(&b);
        u.check().unwrap();
        assert!(u.num_tri() > 0 && !u.is_empty());
        // Union spans from A's low face (-20) to B's high face (+15+15 = +30) on X.
        let (min, max) = u.bbox().unwrap();
        assert!((min[0] - -20.0).abs() < 1e-6, "min x {}", min[0]);
        assert!((max[0] - 30.0).abs() < 1e-6, "max x {}", max[0]);
    }

    #[test]
    fn split_halves_partition_the_solid() {
        let c = Solid::cube(20.0, 20.0, 20.0, true);
        let (pos, neg) = c.split_by_plane([1.0, 0.0, 0.0], 0.0); // (x>0, x<0)
        pos.check().unwrap();
        neg.check().unwrap();
        // Each half is 10mm thick on X; the positive half is [0, 10], the negative [-10, 0].
        assert!((pos.bbox().unwrap().1[0] - 10.0).abs() < 1e-6);
        assert!((pos.bbox().unwrap().0[0] - 0.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().0[0] - -10.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().1[0] - 0.0).abs() < 1e-6);
    }
}
