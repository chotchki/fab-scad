//! The in-process geometry kernel (Track C) — a typed Rust wrapper over Manifold (`manifold3d`),
//! the same CSG engine OpenSCAD's Manifold backend uses. This is the seam that lets fab do slicing +
//! connector CSG WITHOUT shelling out per piece: a re-slice is an in-process boolean on a cached mesh
//! (~ms), not a process spawn (~hundreds of ms). OpenSCAD stays the SCAD→mesh front-door (see
//! `docs/manifold-kernel-spike.md` for the go/no-go); this owns everything downstream of the base mesh.
//!
//! [`Solid`] is a newtype around a Manifold handle so the rest of fab talks in one strongly-typed
//! shape instead of raw bindings. Import (11.2), STL/3mf export (11.3), the slicer (11.4), and the
//! connectors (11.6) build on it.

use anyhow::{Context, Result, anyhow};
use manifold3d::{CrossSection, Manifold, MeshGL};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::Path;

/// A closed, manifold 3D solid — the unit every kernel op consumes and produces.
///
/// **!Send/!Sync by construction** (the `PhantomData<*const ()>`). The upstream binding declares
/// `unsafe impl Send/Sync for Manifold`, but it isn't airtight: `clone` SHARES the underlying
/// `CsgNode`, and `CsgLeafNode::GetImpl()` bakes a pending transform via an UNLOCKED mutation of a
/// `mutable` member — so a transform-pending leaf shared across threads (via clone) and evaluated
/// concurrently is a data race (UB). Rather than depend on that, we forbid moving a `Solid` across
/// threads at the type level: thread boundaries must carry inert mesh data (STL bytes / vertex
/// buffers) and rebuild the `Solid` on the far side. See `docs/manifold-thread-safety.md`.
///
/// The !Send guarantee is locked in — this must NOT compile:
/// ```compile_fail
/// # use fab_scad::kernel::Solid;
/// fn assert_send<T: Send>(_: T) {}
/// assert_send(Solid::cube(1.0, 1.0, 1.0, true)); // Solid is !Send by construction
/// ```
#[derive(Clone)]
pub struct Solid(Manifold, PhantomData<*const ()>);

impl Solid {
    /// The single construction point — keeps the !Send marker consistent everywhere.
    fn wrap(m: Manifold) -> Self {
        Solid(m, PhantomData)
    }

    /// Wrap a raw Manifold (import/slicer internals build these). Used by 11.2 import / 11.4 slicer.
    #[allow(dead_code)]
    pub(crate) fn from_manifold(m: Manifold) -> Self {
        Solid::wrap(m)
    }

    /// Borrow the underlying handle (for ops the wrapper doesn't surface yet). Used by 11.3 export.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Manifold {
        &self.0
    }

    /// An axis-aligned box. `center` puts the centroid at the origin (else the min corner).
    pub fn cube(x: f64, y: f64, z: f64, center: bool) -> Self {
        Solid::wrap(Manifold::cube(x, y, z, center))
    }

    /// A UV sphere of `radius` with `segments` around the equator.
    pub fn sphere(radius: f64, segments: i32) -> Self {
        Solid::wrap(Manifold::sphere(radius, segments))
    }

    /// A cone/cylinder along +Z: `r_low` at the base, `r_high` at the top (0 ⇒ a point). `center`
    /// puts the mid-height at the origin; otherwise the base is at z=0 spanning `[0, height]`.
    pub fn cylinder(height: f64, r_low: f64, r_high: f64, segments: i32, center: bool) -> Self {
        Solid::wrap(Manifold::cylinder(height, r_low, r_high, segments, center))
    }

    /// A teardrop PRISM along +Z (spanning `[0, length]`): a circle of radius `r` with a pointed cap
    /// toward +Y — the convex hull of the circle plus an apex at `(0, r·√2)`, where the two 45°
    /// tangents to the circle meet. That's a self-supporting hole: the printed ceiling never exceeds
    /// 45°. Traced as a 2D `CrossSection` (hull of the circle points + apex) and extruded. The peak is
    /// +Y in this local frame; the caller rotates it toward the print build-up.
    pub fn teardrop_prism(r: f64, length: f64, segments: i32) -> Self {
        let n = segments.max(3);
        let mut pts: Vec<[f64; 2]> = (0..n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                [r * a.cos(), r * a.sin()]
            })
            .collect();
        pts.push([0.0, r * std::f64::consts::SQRT_2]); // apex where the 45° tangents meet
        Solid::wrap(CrossSection::hull_simple_polygon(&pts).extrude(length))
    }

    // --- connector solids (11.6) -----------------------------------------------------------------

    /// A BOSL2-style onion (`onion(r, ang)` = `rotate_extrude(teardrop2d)`): a sphere with a tangent
    /// conical cap so it prints support-free cap-up. Centered at the origin (sphere equator on z=0,
    /// which sits on the cut plane), cap toward +Z. `ang` is the cap half-angle FROM VERTICAL in
    /// degrees (BOSL2's convention) — smaller = pointier; the tip reaches `r/sin(ang)`. Built as
    /// sphere ∪ cone because the cone of slope −cot(ang) is exactly tangent to the sphere at latitude
    /// z = r·sin(ang), so the union is smooth — identical to revolving the teardrop profile.
    pub fn onion(d: f64, ang_deg: f64, segments: i32) -> Self {
        let r = d / 2.0;
        let ang = ang_deg.to_radians();
        let (s, c) = (ang.sin(), ang.cos());
        let z_tangent = r * s; // where the cap leaves the sphere
        let cone_h = r * c * c / s; // tip (r/sin) minus z_tangent
        let base_r = r * c; // sphere radius at that latitude
        let cone =
            Solid::cylinder(cone_h, base_r, 0.0, segments, false).translate(0.0, 0.0, z_tangent);
        Solid::sphere(r, segments).union(&cone)
    }

    /// The bolt-joint NEGATIVE (the fallback when an onion can't print support-free): a through
    /// clearance shaft + head counterbore on the +Z (access) piece, and a heat-set insert pocket on
    /// the −Z piece. Centered on the cut plane at the origin, axis +Z; diff it from BOTH pieces.
    #[allow(clippy::too_many_arguments)] // a dimension list, not a design smell — mirrors _insert_spec
    pub fn bolt_clearance(
        clearance_d: f64,
        through: f64,
        counterbore_d: f64,
        counterbore_h: f64,
        insert_d: f64,
        insert_depth: f64,
        segments: i32,
        teardrop: bool,
    ) -> Self {
        // When the hole runs horizontal on the bed, a TEARDROP shaft + counterbore self-support (peak
        // toward +Y here; the slicer rotates it to the build-up). The insert pocket stays round — a
        // short blind hole that seats a cylindrical heat-set insert.
        let shaft = if teardrop {
            Solid::teardrop_prism(clearance_d / 2.0, through, segments)
        } else {
            Solid::cylinder(
                through,
                clearance_d / 2.0,
                clearance_d / 2.0,
                segments,
                false,
            )
        };
        let cbore = if teardrop {
            Solid::teardrop_prism(counterbore_d / 2.0, counterbore_h, segments)
        } else {
            Solid::cylinder(
                counterbore_h,
                counterbore_d / 2.0,
                counterbore_d / 2.0,
                segments,
                false,
            )
        }
        .translate(0.0, 0.0, through - counterbore_h);
        let pocket = Solid::cylinder(
            insert_depth,
            insert_d / 2.0,
            insert_d / 2.0,
            segments,
            false,
        )
        .translate(0.0, 0.0, -insert_depth);
        Solid::batch_union(&[shaft, cbore, pocket])
    }

    /// Cross-section at `axis` (0=X, 1=Y, 2=Z) = `at`, as profile loops in connector-pos coords (the
    /// cut's two non-axis dims, ascending) — the IN-PROCESS twin of the OpenSCAD `projection(cut=true)`
    /// path, no shell-out. Rotate the cut plane onto z with a PROPER rotation that also lands the two
    /// non-axis dims on (x, y) in order, then slice: the slice's (x, y) IS pos — X→(y,z), Y→(x,z),
    /// Z→(x,y), no SVG y-negation to undo. One outer loop + one per hole; empty if the plane misses.
    pub fn cross_section(&self, axis: usize, at: f64) -> Vec<Vec<[f64; 2]>> {
        // Column-major 3×4 (columns = images of e_x, e_y, e_z; last = translation). Each is a proper
        // rotation (det +1) so loop winding is preserved.
        let (rot, h) = match axis {
            // (x,y,z) → (y, z, x): x-normal → +z; slice at `at`, giving (y, z).
            0 => (
                self.transform(&[0., 0., 1., 1., 0., 0., 0., 1., 0., 0., 0., 0.]),
                at,
            ),
            // (x,y,z) → (x, z, −y): y-normal → −z; slice at −`at`, giving (x, z).
            1 => (
                self.transform(&[1., 0., 0., 0., 0., -1., 0., 1., 0., 0., 0., 0.]),
                -at,
            ),
            // z-normal already +z; slice at `at`, giving (x, y).
            2 => (self.clone(), at),
            _ => return Vec::new(),
        };
        rot.0.slice_at_z(h)
    }

    // --- import (11.2) ---------------------------------------------------------------------------

    /// Load an STL file (binary or ASCII) as a Solid — the front-door for a mesh OpenSCAD rendered.
    pub fn from_stl_file(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading STL {}", path.display()))?;
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
        Ok(Solid::wrap(m))
    }

    /// Build from an ALREADY-indexed mesh (3mf objects arrive this way — no weld needed; the
    /// file's own topology is authoritative). Fails like `from_stl_bytes` if it isn't manifold.
    pub fn from_indexed(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> Result<Self> {
        if verts.is_empty() || tris.is_empty() {
            return Err(anyhow!("indexed mesh is empty"));
        }
        let flat: Vec<f32> = verts.iter().flatten().map(|&c| c as f32).collect();
        let idx: Vec<u32> = tris.iter().flatten().copied().collect();
        let mesh = MeshGL::new(&flat, 3, &idx).map_err(|e| anyhow!("building mesh: {e:?}"))?;
        let m = Manifold::from_meshgl(&mesh)
            .map_err(|e| anyhow!("mesh is not a valid manifold: {e:?}"))?;
        Ok(Solid::wrap(m))
    }

    // --- export (11.3) ---------------------------------------------------------------------------

    /// Serialize to binary STL bytes (per-face normals computed from the winding).
    pub fn to_stl_bytes(&self) -> Vec<u8> {
        let (v, stride, idx) = self.0.to_mesh_f64();
        let p = |i: u64| {
            let b = i as usize * stride;
            [v[b] as f32, v[b + 1] as f32, v[b + 2] as f32]
        };
        let ntri = (idx.len() / 3) as u32;
        let mut out = Vec::with_capacity(84 + 50 * ntri as usize);
        out.extend_from_slice(&[0u8; 80]); // header
        out.extend_from_slice(&ntri.to_le_bytes());
        for t in idx.chunks_exact(3) {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if l > 0.0 {
                n = [n[0] / l, n[1] / l, n[2] / l];
            }
            for comp in n {
                out.extend_from_slice(&comp.to_le_bytes());
            }
            for vert in [a, b, c] {
                for comp in vert {
                    out.extend_from_slice(&comp.to_le_bytes());
                }
            }
            out.extend_from_slice(&[0u8; 2]); // attribute byte count
        }
        out
    }

    /// Indexed mesh: deduped vertices + 0-based triangle indices (for exporters that want indexed
    /// geometry, e.g. the Bambu writer).
    pub fn to_indexed(&self) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (v, stride, idx) = self.0.to_mesh_f64();
        let verts = (0..v.len() / stride)
            .map(|i| [v[i * stride], v[i * stride + 1], v[i * stride + 2]])
            .collect();
        let tris = idx
            .chunks_exact(3)
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect();
        (verts, tris)
    }

    /// Triangles as coordinate triples — for orientation math (`auto_orient::best_up`).
    pub fn tris(&self) -> Vec<[[f64; 3]; 3]> {
        let (verts, tris) = self.to_indexed();
        tris.iter()
            .map(|t| {
                [
                    verts[t[0] as usize],
                    verts[t[1] as usize],
                    verts[t[2] as usize],
                ]
            })
            .collect()
    }

    /// Write this solid as a binary STL.
    pub fn write_stl(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_stl_bytes())
            .with_context(|| format!("writing STL {}", path.display()))
    }

    /// Write `pieces` as SEPARATE objects on ONE 3mf plate — the multipart export (replaces the
    /// OpenSCAD lazy-union trick; separation is first-class here). Object/item ids are 1-based.
    pub fn write_3mf(path: &Path, pieces: &[Solid]) -> Result<()> {
        use threemf::model::{Build, Item, Model, Object, Resources};
        let object = pieces
            .iter()
            .enumerate()
            .map(|(i, p)| Object {
                id: i + 1,
                mesh: Some(to_3mf_mesh(p)),
                ..Default::default()
            })
            .collect();
        let item = (0..pieces.len())
            .map(|i| Item {
                objectid: i + 1,
                transform: None,
                partnumber: None,
            })
            .collect();
        let model = Model {
            xmlns: "http://schemas.microsoft.com/3dmanufacturing/core/2015/02".into(),
            xmlns_m: None,
            metadata: vec![],
            resources: Resources {
                object,
                basematerials: None,
            },
            build: Build { item },
            unit: Default::default(),
        };
        let f = std::fs::File::create(path)
            .with_context(|| format!("creating 3mf {}", path.display()))?;
        threemf::write(f, model).map_err(|e| anyhow!("writing 3mf: {e:?}"))
    }

    // --- booleans --------------------------------------------------------------------------------

    pub fn union(&self, other: &Solid) -> Solid {
        Solid::wrap(self.0.union(&other.0))
    }
    pub fn difference(&self, other: &Solid) -> Solid {
        Solid::wrap(self.0.difference(&other.0))
    }
    pub fn intersection(&self, other: &Solid) -> Solid {
        Solid::wrap(self.0.intersection(&other.0))
    }

    /// Union many solids at once (cheaper + more robust than folding `union`). Empty ⇒ empty solid.
    pub fn batch_union(solids: &[Solid]) -> Solid {
        let hs: Vec<Manifold> = solids.iter().map(|s| s.0.clone()).collect();
        Solid::wrap(Manifold::batch_union(&hs))
    }

    // --- transforms ------------------------------------------------------------------------------

    pub fn translate(&self, x: f64, y: f64, z: f64) -> Solid {
        Solid::wrap(self.0.translate(x, y, z))
    }
    /// Rotate by Euler angles in DEGREES (X then Y then Z).
    pub fn rotate(&self, x_deg: f64, y_deg: f64, z_deg: f64) -> Solid {
        Solid::wrap(self.0.rotate(x_deg, y_deg, z_deg))
    }
    /// Apply a 3×4 affine (column-major 12-float, as Manifold expects).
    pub fn transform(&self, m: &[f64; 12]) -> Solid {
        Solid::wrap(self.0.transform(m))
    }

    /// Rotate so local +Z maps onto unit `axis` — used to point a connector's cap along its
    /// derived build axis before translating it onto the cut. Rodrigues' rotation between vectors;
    /// the antipodal (+Z→−Z) case flips about X. A zero/degenerate axis leaves it unrotated.
    pub fn align_z_to(&self, axis: [f64; 3]) -> Solid {
        let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if n < 1e-12 {
            return self.clone();
        }
        let u = [axis[0] / n, axis[1] / n, axis[2] / n];
        let d = u[2]; // cos angle between +Z and u
        let r = if d > 1.0 - 1e-9 {
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]
        } else if d < -1.0 + 1e-9 {
            [[1., 0., 0.], [0., -1., 0.], [0., 0., -1.]] // 180° about X
        } else {
            let v = [-u[1], u[0], 0.0]; // z × u
            let k = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
            let mut k2 = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    k2[i][j] = (0..3).map(|m| k[i][m] * k[m][j]).sum();
                }
            }
            let f = 1.0 / (1.0 + d);
            let mut r = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    r[i][j] = if i == j { 1.0 } else { 0.0 } + k[i][j] + k2[i][j] * f;
                }
            }
            r
        };
        // Manifold wants a column-major 3×4: columns 0..2 are R's columns, column 3 the translation.
        let m = [
            r[0][0], r[1][0], r[2][0], //
            r[0][1], r[1][1], r[2][1], //
            r[0][2], r[1][2], r[2][2], //
            0.0, 0.0, 0.0,
        ];
        self.transform(&m)
    }

    // --- slab slicer (11.4) ----------------------------------------------------------------------

    /// Extract ONE piece: this base clipped to the cell that slab multi-index `piece` selects, given
    /// `cuts` (ascending cut coordinates per axis, empty for an uncut axis). Each axis clips to
    /// [cut[i-1], cut[i]] via split_by_plane — the linear O(N) slice, no 2^N blowup. An uncut axis
    /// spans the whole model (its index must be 0). Returns an empty solid when the cell holds no
    /// material (an L-shaped model leaves some cells bare).
    pub fn slab_piece(&self, cuts: &[Vec<f64>; 3], piece: [usize; 3]) -> Solid {
        let mut s = self.clone();
        for a in 0..3 {
            let ac = &cuts[a];
            if ac.is_empty() {
                continue; // uncut axis — whole span; piece[a] is 0
            }
            let i = piece[a];
            let mut normal = [0.0; 3];
            normal[a] = 1.0;
            if i > 0 {
                s = s.split_by_plane(normal, ac[i - 1]).0; // keep the > lower-cut half
            }
            if i < ac.len() {
                s = s.split_by_plane(normal, ac[i]).1; // keep the < upper-cut half
            }
        }
        s
    }

    /// Slice the base into every non-empty piece, paired with its slab multi-index (ix outer → iz
    /// inner, matching `slicing::piece_indices`). Empty cells are dropped. This is the whole slice.
    pub fn slab_pieces(&self, cuts: &[Vec<f64>; 3]) -> Vec<([usize; 3], Solid)> {
        let counts = [cuts[0].len() + 1, cuts[1].len() + 1, cuts[2].len() + 1];
        let mut out = Vec::new();
        for ix in 0..counts[0] {
            for iy in 0..counts[1] {
                for iz in 0..counts[2] {
                    let piece = [ix, iy, iz];
                    let s = self.slab_piece(cuts, piece);
                    if !s.is_empty() {
                        out.push((piece, s));
                    }
                }
            }
        }
        out
    }

    // --- half-space cuts (the slicer primitives, 11.4) -------------------------------------------

    /// Split by the plane `normal·p = offset` into `(positive, negative)` — the positive half is the
    /// `normal·p > offset` side. Both halves are independent solids; this is the slicer primitive
    /// (11.4), preferred over `trim_by_plane` because both sides come back clean.
    pub fn split_by_plane(&self, normal: [f64; 3], offset: f64) -> (Solid, Solid) {
        let (pos, neg) = self.0.split_by_plane(normal, offset);
        (Solid::wrap(pos), Solid::wrap(neg))
    }
    /// Keep only the `normal·p > offset` half (drops the rest). NOTE upstream #1516: trimmed halves
    /// may not re-union cleanly (coincident faces) — use `split_by_plane` when you need both sides.
    pub fn trim_by_plane(&self, normal: [f64; 3], offset: f64) -> Solid {
        Solid::wrap(self.0.trim_by_plane(normal, offset))
    }

    // --- queries ---------------------------------------------------------------------------------

    /// Err if the solid isn't a valid 2-manifold — the gate a slice/connector result must pass.
    pub fn check(&self) -> Result<()> {
        self.0
            .status()
            .map_err(|e| anyhow!("non-manifold solid: {e:?}"))
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
                        f32::from_le_bytes([
                            bytes[o + k],
                            bytes[o + k + 1],
                            bytes[o + k + 2],
                            bytes[o + k + 3],
                        ])
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

/// A Solid's mesh as a threemf Mesh (indexed verts + triangles) for the 3mf writer.
fn to_3mf_mesh(s: &Solid) -> threemf::model::Mesh {
    let (v, stride, idx) = s.0.to_mesh_f64();
    let vertex = (0..v.len() / stride)
        .map(|i| threemf::model::Vertex {
            x: v[i * stride],
            y: v[i * stride + 1],
            z: v[i * stride + 2],
        })
        .collect();
    let triangle = idx
        .chunks_exact(3)
        .map(|t| threemf::model::Triangle {
            v1: t[0] as usize,
            v2: t[1] as usize,
            v3: t[2] as usize,
        })
        .collect();
    threemf::model::Mesh {
        vertices: threemf::model::Vertices { vertex },
        triangles: threemf::model::Triangles { triangle },
    }
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
    fn teardrop_prism_is_a_pointed_manifold() {
        let (r, len) = (5.0, 10.0);
        let t = Solid::teardrop_prism(r, len, 48);
        assert!(t.is_manifold(), "teardrop prism should be a valid manifold");
        let (min, max) = t.bbox().unwrap();
        // Circle in x (±r); y from −r (circle bottom) up to the apex r·√2 ≈ 7.07 (the point);
        // extruded z ∈ [0, len].
        assert!(
            (min[0] + r).abs() < 0.1 && (max[0] - r).abs() < 0.1,
            "x spans ±r: {min:?}..{max:?}"
        );
        assert!((min[1] + r).abs() < 0.1, "y bottom ≈ −r, got {}", min[1]);
        assert!(
            (max[1] - r * std::f64::consts::SQRT_2).abs() < 0.2,
            "y peak ≈ r·√2, got {}",
            max[1]
        );
        assert!(
            min[2].abs() < 1e-6 && (max[2] - len).abs() < 1e-6,
            "z ∈ [0,len]"
        );
    }

    #[test]
    fn cross_section_returns_pos_coords_per_axis() {
        // A box with distinct extents so a swapped/negated axis can't hide: x∈[0,10], y∈[0,20], z∈[0,30].
        let b = Solid::cube(10.0, 20.0, 30.0, false);
        let bbox2 = |loops: Vec<Vec<[f64; 2]>>| {
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for lp in &loops {
                for p in lp {
                    for k in 0..2 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            (lo, hi)
        };
        let approx = |a: f64, b: f64| (a - b).abs() < 0.05;
        // Z cut → pos (x, y) = [0,10]×[0,20].
        let (lo, hi) = bbox2(b.cross_section(2, 15.0));
        assert!(
            approx(lo[0], 0.0) && approx(hi[0], 10.0) && approx(lo[1], 0.0) && approx(hi[1], 20.0),
            "Z: {lo:?}..{hi:?}"
        );
        // X cut → pos (y, z) = [0,20]×[0,30].
        let (lo, hi) = bbox2(b.cross_section(0, 5.0));
        assert!(
            approx(lo[0], 0.0) && approx(hi[0], 20.0) && approx(lo[1], 0.0) && approx(hi[1], 30.0),
            "X: {lo:?}..{hi:?}"
        );
        // Y cut → pos (x, z) = [0,10]×[0,30].
        let (lo, hi) = bbox2(b.cross_section(1, 10.0));
        assert!(
            approx(lo[0], 0.0) && approx(hi[0], 10.0) && approx(lo[1], 0.0) && approx(hi[1], 30.0),
            "Y: {lo:?}..{hi:?}"
        );
        // A plane that misses the solid → no loops.
        assert!(b.cross_section(2, 99.0).is_empty(), "miss → empty");
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
        let err = Solid::from_stl_bytes(&binary_stl(&TETRA[..1]))
            .err()
            .expect("should reject");
        assert!(
            format!("{err:#}").contains("not a valid manifold"),
            "got: {err:#}"
        );
    }

    #[test]
    fn stl_export_roundtrips() {
        let c = Solid::cube(10.0, 20.0, 30.0, true);
        let back = Solid::from_stl_bytes(&c.to_stl_bytes()).unwrap();
        assert_eq!((back.num_vert(), back.num_tri()), (8, 12));
        let (min, max) = back.bbox().unwrap();
        for k in 0..3 {
            assert!((min[k] - c.bbox().unwrap().0[k]).abs() < 1e-4);
            assert!((max[k] - c.bbox().unwrap().1[k]).abs() < 1e-4);
        }
    }

    #[test]
    fn writes_two_pieces_as_separate_3mf_objects() {
        let a = Solid::cube(10.0, 10.0, 10.0, true);
        let b = Solid::cube(10.0, 10.0, 10.0, true).translate(30.0, 0.0, 0.0);
        let path = std::env::temp_dir().join(format!("kernel_3mf_{}.3mf", std::process::id()));
        Solid::write_3mf(&path, &[a, b]).unwrap();
        let models = threemf::read(std::fs::File::open(&path).unwrap()).unwrap();
        let objects: usize = models.iter().map(|m| m.resources.object.len()).sum();
        assert_eq!(
            objects, 2,
            "two pieces should be two separate objects on the plate"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn slices_one_axis_into_partitioned_slabs() {
        // 60mm cube on X, cut at -10 and +10 -> 3 slabs: [-30,-10], [-10,10], [10,30].
        let c = Solid::cube(60.0, 60.0, 60.0, true);
        let cuts = [vec![-10.0, 10.0], vec![], vec![]];
        let pieces = c.slab_pieces(&cuts);
        assert_eq!(pieces.len(), 3);
        let want = [(-30.0, -10.0), (-10.0, 10.0), (10.0, 30.0)];
        for ((idx, s), (lo, hi)) in pieces.iter().zip(want) {
            s.check().unwrap();
            let (min, max) = s.bbox().unwrap();
            assert!(
                (min[0] - lo).abs() < 1e-4 && (max[0] - hi).abs() < 1e-4,
                "piece {idx:?}: {min:?}..{max:?}"
            );
        }
    }

    #[test]
    fn slices_two_axes_into_cells() {
        // Cut on X@0 and Y@0 -> 4 quadrant cells, each a valid solid, X/Y each half-width.
        let c = Solid::cube(40.0, 40.0, 40.0, true);
        let cuts = [vec![0.0], vec![0.0], vec![]];
        let pieces = c.slab_pieces(&cuts);
        assert_eq!(pieces.len(), 4);
        for (idx, s) in &pieces {
            s.check().unwrap();
            let (min, max) = s.bbox().unwrap();
            assert!(
                (max[0] - min[0] - 20.0).abs() < 1e-4,
                "cell {idx:?} X width"
            );
            assert!(
                (max[1] - min[1] - 20.0).abs() < 1e-4,
                "cell {idx:?} Y width"
            );
            assert!((max[2] - min[2] - 40.0).abs() < 1e-4, "cell {idx:?} Z full");
        }
        // The middle piece of a single-axis onion floater bug can't recur here — each cell is built
        // by clipping the SAME base, so nothing from a neighbour cell leaks in.
    }

    #[test]
    fn onion_is_a_support_free_teardrop() {
        let (d, ang) = (10.0, 45.0_f64);
        let o = Solid::onion(d, ang, 64);
        o.check().unwrap();
        let (min, max) = o.bbox().unwrap();
        let r = d / 2.0;
        let tip = r / ang.to_radians().sin();
        // Widest at the equator (radius r), pointed tip at r/sin(ang), rounded bottom at -r.
        assert!((max[2] - tip).abs() < 0.05, "tip {} want {tip}", max[2]);
        assert!((min[2] + r).abs() < 0.05, "bottom {}", min[2]);
        assert!(
            (max[0] - r).abs() < 0.06 && (min[0] + r).abs() < 0.06,
            "equator radius {max:?}"
        );
    }

    #[test]
    fn socket_swallows_the_peg() {
        let (d, ang, slop) = (10.0, 45.0, 0.2);
        let peg = Solid::onion(d, ang, 64);
        let socket = Solid::onion(d + 2.0 * slop, ang, 64); // grow the whole onion by slop
        // The peg drops fully into the socket — self-consistent, no BOSL2 dependency.
        assert!(
            peg.difference(&socket).is_empty(),
            "peg should fit inside the slop-grown socket"
        );
        assert!(
            socket.bbox().unwrap().1[2] > peg.bbox().unwrap().1[2],
            "socket is larger"
        );
    }

    #[test]
    fn align_z_to_points_the_cap() {
        // A cone tips toward +Z; align it to +X and the tip should move to the +X extreme.
        let cone = Solid::cylinder(10.0, 4.0, 0.0, 32, false); // apex at z=10
        let along_x = cone.align_z_to([1.0, 0.0, 0.0]);
        let (min, max) = along_x.bbox().unwrap();
        assert!(
            (max[0] - 10.0).abs() < 0.05,
            "tip should reach +X=10, got {}",
            max[0]
        );
        assert!(
            max[2] < 4.1 && min[2] > -4.1,
            "no longer tall on Z: {min:?}..{max:?}"
        );
    }

    #[test]
    fn bolt_clearance_spans_both_pieces() {
        let b = Solid::bolt_clearance(3.4, 12.0, 6.0, 3.0, 5.0, 6.0, 48, false);
        b.check().unwrap();
        let (min, max) = b.bbox().unwrap();
        assert!(
            (min[2] + 6.0).abs() < 1e-6,
            "insert pocket depth {}",
            min[2]
        ); // -insert_depth
        assert!((max[2] - 12.0).abs() < 1e-6, "through length {}", max[2]); // +through
        // Plain (cylinder) shaft: symmetric in Y, no peak.
        assert!(
            (max[1] - 3.0).abs() < 0.1 && (min[1] + 3.0).abs() < 0.1,
            "round shaft ±r in Y"
        );
    }

    #[test]
    fn teardrop_bolt_has_a_peak_toward_plus_y() {
        // The counterbore (d=6 → r=3) is the widest teardrop, so the +Y peak reaches ~3·√2 ≈ 4.24,
        // well past the round radius — that pointed ceiling is what self-supports a horizontal hole.
        let b = Solid::bolt_clearance(3.4, 12.0, 6.0, 3.0, 5.0, 6.0, 48, true);
        b.check().unwrap();
        let (min, max) = b.bbox().unwrap();
        assert!(
            max[1] > 3.0 * std::f64::consts::SQRT_2 - 0.3,
            "teardrop peak in +Y, got {}",
            max[1]
        );
        assert!(
            (min[1] + 3.0).abs() < 0.2,
            "round on the −Y side, got {}",
            min[1]
        );
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
