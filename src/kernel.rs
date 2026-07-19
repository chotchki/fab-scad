//! The in-process geometry kernel (Track C) — a typed wrapper over `fab-manifold`, OUR pure-Rust
//! port of the Manifold CSG engine (Phase M; flipped off the `manifold3d` C++ bindings in M.7.3).
//! This is the seam that lets fab do slicing + connector CSG WITHOUT shelling out per piece: a
//! re-slice is an in-process boolean on a cached mesh (~ms), not a process spawn (~hundreds of ms).
//! OpenSCAD stays the SCAD→mesh front-door (see `docs/manifold-kernel-spike.md` for the go/no-go);
//! this owns everything downstream of the base mesh.
//!
//! [`Solid`] is a newtype around a [`Mesh`] so the rest of fab talks in one strongly-typed shape
//! instead of raw kernel types. Import (11.2), STL/3mf export (11.3), the slicer (11.4), and the
//! connectors (11.6) build on it.
//!
//! Fallibility at this seam: fab-manifold surfaces invalid input as typed `Err`s (M.5.4.5), where
//! the C++ silently produced an INVALID (empty, status-carrying) manifold. This wrapper keeps its
//! historical infallible signatures by mapping those `Err`s to the SAME observable the C++ gave:
//! an empty solid/section (plus a `tracing::warn!`) — no caller sees a new failure mode.

use anyhow::{Context, Result, anyhow};
use fab_lang::{Affine, Affine2, ExtrudeKind, Join2D, Rgba, Tri, Vec3};
use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::cross_section::{CrossSection, FillRule, JoinType};
use fab_manifold::linalg::{Mat2x3, Mat3x4, Vec2, rotate_xyz_degrees};
use fab_manifold::mesh::{Mesh, MeshGl};
use fab_manifold::{bridge, check};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::Path;

/// A fab-lang [`Affine`] (row-major 3×4) as the kernel's column-basis [`Mat3x4`].
fn to_mat3x4(m: &Affine) -> Mat3x4 {
    let c = m.to_column_major();
    Mat3x4 {
        x: Vec3::new(c[0], c[1], c[2]),
        y: Vec3::new(c[3], c[4], c[5]),
        z: Vec3::new(c[6], c[7], c[8]),
        w: Vec3::new(c[9], c[10], c[11]),
    }
}

/// A fab-lang [`Affine2`] (row-major 2×3) as the kernel's column-basis [`Mat2x3`].
fn to_mat2x3(m: &Affine2) -> Mat2x3 {
    let c = m.to_column_major();
    Mat2x3 {
        x: Vec2::new(c[0], c[1]),
        y: Vec2::new(c[2], c[3]),
        w: Vec2::new(c[4], c[5]),
    }
}

/// Collapse a kernel `Err` to the empty mesh the C++ backend's INVALID manifold used to surface as,
/// loudly — the M.5.4.5 seam contract (see the module doc).
fn mesh_or_empty(r: std::result::Result<Mesh, fab_manifold::status::Error>, op: &str) -> Mesh {
    r.unwrap_or_else(|e| {
        tracing::warn!(
            ?e,
            op,
            "kernel op rejected its input; yielding the empty solid"
        );
        Mesh::default()
    })
}

/// Raw `[x, y]` contours as the kernel's typed [`Vec2`] rings.
fn to_vec2_contours(contours: &[Vec<[f64; 2]>]) -> Vec<Vec<Vec2>> {
    contours
        .iter()
        .map(|c| c.iter().map(|&[x, y]| Vec2::new(x, y)).collect())
        .collect()
}

/// [`mesh_or_empty`]'s 2D twin.
fn section_or_empty(
    r: std::result::Result<CrossSection, fab_manifold::status::Error>,
    op: &str,
) -> CrossSection {
    r.unwrap_or_else(|e| {
        tracing::warn!(
            ?e,
            op,
            "2D op rejected its input; yielding the empty region"
        );
        CrossSection::new()
    })
}

/// A closed, manifold 3D solid — the unit every kernel op consumes and produces.
///
/// **!Send/!Sync by construction.** `Solid` wraps `Rc<Mesh>`, which is already `!Send`; the
/// `PhantomData<*const ()>` locks that in independently of the field, so a later `Rc`→`Arc` switch
/// (SPEC OPEN #3) can't silently make it `Send`. Deliberate: thread boundaries carry inert mesh DATA
/// (STL bytes / vertex buffers) and rebuild the `Solid` on the far side. (The old rationale — a C++
/// `Manifold` whose `clone` shared a `CsgNode` with an unlocked pending-transform mutation — is gone;
/// the kernel is pure Rust since M.7.4, and `Mesh` is a plain deep value type.)
///
/// The !Send guarantee is locked in — this must NOT compile:
/// ```compile_fail
/// # use fab_scad::kernel::Solid;
/// fn assert_send<T: Send>(_: T) {}
/// assert_send(Solid::cube(1.0, 1.0, 1.0, true)); // Solid is !Send by construction
/// ```
#[derive(Clone)]
pub struct Solid(std::rc::Rc<Mesh>, PhantomData<*const ()>);

impl Solid {
    /// The single construction point — keeps the !Send marker consistent everywhere. `Rc` restores
    /// the cheap `clone()` the C++ backend's shared handle gave (a `Mesh` is a deep value type, and
    /// the slicer/components paths clone Solids freely); `Solid` is immutable and already `!Send`,
    /// so a plain refcount is exactly right.
    fn wrap(m: Mesh) -> Self {
        Solid(std::rc::Rc::new(m), PhantomData)
    }

    /// Wrap a raw kernel mesh (import/slicer internals build these). Used by 11.2 import / 11.4 slicer.
    #[allow(dead_code)]
    pub(crate) fn from_manifold(m: Mesh) -> Self {
        Solid::wrap(m)
    }

    /// Borrow the underlying mesh (for ops the wrapper doesn't surface yet). Used by 11.3 export.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Mesh {
        &self.0
    }

    /// An axis-aligned box. `center` puts the centroid at the origin (else the min corner).
    pub fn cube(x: f64, y: f64, z: f64, center: bool) -> Self {
        Solid::wrap(mesh_or_empty(
            Mesh::cube(Vec3::new(x, y, z), center),
            "cube",
        ))
    }

    /// A UV sphere of `radius` with `segments` around the equator.
    pub fn sphere(radius: f64, segments: i32) -> Self {
        Solid::wrap(Mesh::sphere(radius, segments))
    }

    /// A cone/cylinder along +Z: `r_low` at the base, `r_high` at the top (0 ⇒ a point). `center`
    /// puts the mid-height at the origin; otherwise the base is at z=0 spanning `[0, height]`.
    pub fn cylinder(height: f64, r_low: f64, r_high: f64, segments: i32, center: bool) -> Self {
        Solid::wrap(Mesh::cylinder(height, r_low, r_high, segments, center))
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
        let pts: Vec<Vec2> = pts.iter().map(|&[x, y]| Vec2::new(x, y)).collect();
        let hull = section_or_empty(CrossSection::hull_of_points(&pts), "teardrop hull");
        Solid::wrap(hull.extrude(length))
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
        let cone = Solid::cylinder(cone_h, base_r, 0.0, segments, false)
            .translate(Vec3::new(0.0, 0.0, z_tangent));
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
        .translate(Vec3::new(0.0, 0.0, through - counterbore_h));
        let pocket = Solid::cylinder(
            insert_depth,
            insert_d / 2.0,
            insert_d / 2.0,
            segments,
            false,
        )
        .translate(Vec3::new(0.0, 0.0, -insert_depth));
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
                self.transform(&Affine::from_column_major([
                    0., 0., 1., 1., 0., 0., 0., 1., 0., 0., 0., 0.,
                ])),
                at,
            ),
            // (x,y,z) → (x, z, −y): y-normal → −z; slice at −`at`, giving (x, z).
            1 => (
                self.transform(&Affine::from_column_major([
                    1., 0., 0., 0., 0., -1., 0., 1., 0., 0., 0., 0.,
                ])),
                -at,
            ),
            // z-normal already +z; slice at `at`, giving (x, y).
            2 => (self.clone(), at),
            _ => return Vec::new(),
        };
        section_or_empty(rot.0.slice_at_z(h), "cross_section slice").to_polygons()
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
        let gl = MeshGl {
            num_prop: 3,
            vert_properties: verts.iter().map(|&v| f64::from(v)).collect(),
            tri_verts: idx,
            ..Default::default()
        };
        let m = Mesh::from_mesh_gl(&gl)
            .map_err(|e| anyhow!("STL is not a valid manifold after weld: {e:?}"))?;
        Ok(Solid::wrap(m))
    }

    /// Build from an ALREADY-indexed mesh (polyhedron/VNF leaves, 3mf objects). Fails like
    /// [`Self::from_stl_bytes`] if it isn't manifold.
    ///
    /// WELDS exact-bit-coincident vertices to one index first. OpenSCAD's `polyhedron()` welds, and a
    /// REVOLVED VNF — `rotate_sweep` / a chamfered or rounded `cyl` / `teardrop`, anything that closes a
    /// 360° loop — DUPLICATES its seam ring (section N == section 0 as DISTINCT indices, bit-for-bit equal).
    /// Manifold reads that as an OPEN seam (non-manifold) → the whole leaf drops to empty, which was the
    /// dominant `models/`-sweep divergence (L.3.4): chamfered cylinders and teardrops rendered NOTHING.
    /// Welding by EXACT f64 bits (not a tolerance) is the manifold-correct move — two bit-identical positions
    /// ARE one point — and it's a no-op everywhere it must be: a 3mf's shared topology has no exact dups, and
    /// a re-imported boolean-RESULT mesh's NEAR-coincident seam verts differ in the low bits so they stay
    /// distinct (J.2.7.1 preserved — that's why the whole path stays f64, never an f32 downcast). A tri that
    /// collapses to <3 distinct verts under the weld is degenerate (zero area) and dropped.
    pub fn from_indexed(verts: &[Vec3], tris: &[Tri]) -> Result<Self> {
        if verts.is_empty() || tris.is_empty() {
            return Err(anyhow!("indexed mesh is empty"));
        }
        let mut map: HashMap<[u64; 3], u32> = HashMap::new();
        let mut flat: Vec<f64> = Vec::with_capacity(verts.len() * 3);
        let mut remap: Vec<u32> = Vec::with_capacity(verts.len());
        for v in verts {
            let a = v.to_array();
            let key = [a[0].to_bits(), a[1].to_bits(), a[2].to_bits()];
            let id = *map.entry(key).or_insert_with(|| {
                flat.extend_from_slice(&a);
                u32::try_from(flat.len() / 3 - 1).expect("vertex count fits u32")
            });
            remap.push(id);
        }
        let idx: Vec<u32> = tris
            .iter()
            .filter_map(|t| {
                let [a, b, c] = t.indices().map(|i| remap[i as usize]);
                // Drop tris the weld collapsed to a degenerate (a repeated vertex → zero area).
                (a != b && b != c && a != c).then_some([a, b, c])
            })
            .flatten()
            .collect();
        if idx.is_empty() {
            return Err(anyhow!(
                "mesh is degenerate after weld (no non-degenerate triangles)"
            ));
        }
        let gl = MeshGl {
            num_prop: 3,
            vert_properties: flat,
            tri_verts: idx,
            ..Default::default()
        };
        let m =
            Mesh::from_mesh_gl(&gl).map_err(|e| anyhow!("mesh is not a valid manifold: {e:?}"))?;
        Ok(Solid::wrap(m))
    }

    // --- export (11.3) ---------------------------------------------------------------------------

    /// Serialize to binary STL bytes (per-face normals computed from the winding).
    pub fn to_stl_bytes(&self) -> Vec<u8> {
        let gl = self.0.to_mesh_gl();
        let (v, stride, idx) = (gl.vert_properties, gl.num_prop, gl.tri_verts);
        let p = |i: u32| {
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

    /// Serialize to a standard 3MF (core + `<basematerials>` color) — the whole model as ONE object at
    /// the origin, for the web save-back's mesh variant (W.5). Carries the per-vertex color the kernel
    /// holds ([`vertex_colors`](Self::vertex_colors), which survives every boolean) as a distinct-color
    /// material table; an uncolored solid emits a plain mesh. In-memory, the 3MF twin of
    /// [`to_stl_bytes`](Self::to_stl_bytes). See [`crate::threemf_out`] for the format + the seam caveat.
    pub fn to_3mf_bytes(&self) -> Vec<u8> {
        let (verts, tris) = self.to_indexed();
        let v: Vec<[f64; 3]> = verts.iter().map(|p| p.to_array()).collect();
        let t: Vec<[u32; 3]> = tris.iter().map(|tri| tri.indices()).collect();
        let colors = self.vertex_colors().map(|cs| {
            cs.iter()
                .map(|c| [c.r, c.g, c.b, c.a])
                .collect::<Vec<[f64; 4]>>()
        });
        crate::threemf_out::to_3mf_bytes(&v, &t, colors.as_deref())
    }

    /// Indexed mesh: deduped vertices + 0-based triangle indices (for exporters that want indexed
    /// geometry, e.g. the Bambu writer).
    pub fn to_indexed(&self) -> (Vec<Vec3>, Vec<Tri>) {
        let gl = self.0.to_mesh_gl();
        let (v, stride, idx) = (gl.vert_properties, gl.num_prop, gl.tri_verts);
        let verts = (0..v.len() / stride)
            .map(|i| Vec3::new(v[i * stride], v[i * stride + 1], v[i * stride + 2]))
            .collect();
        let tris = idx
            .chunks_exact(3)
            .map(|t| Tri::new(t[0], t[1], t[2]))
            .collect();
        (verts, tris)
    }

    // --- color (J.2.9) — RGBA as 4 EXTRA Manifold vertex properties (numProp 4 → MeshGL stride 7).
    // Manifold carries properties through every boolean, so a colored subtree keeps its color when
    // union/difference/intersection'd (seam verts linear-interpolate, which is exact for a uniform color).

    /// Set EVERY vertex's color to `rgba` — the `color()` module's overwrite (outermost wins in
    /// OpenSCAD, so re-coloring replaces any inner color). Cheap: one `SetProperties` pass.
    pub fn with_color(&self, rgba: Rgba) -> Solid {
        Solid::wrap(self.0.set_properties(4, move |new, _pos, _old| {
            new.copy_from_slice(&rgba.to_array());
        }))
    }

    /// Per-vertex colors, index-aligned with [`to_indexed`](Self::to_indexed)'s verts — or `None` when
    /// the solid is UNCOLORED (no color property: MeshGL stride 3, not 7).
    pub fn vertex_colors(&self) -> Option<Vec<Rgba>> {
        let gl = self.0.to_mesh_gl();
        let (v, stride) = (gl.vert_properties, gl.num_prop);
        if stride < 7 {
            return None; // xyz only — never colored
        }
        Some(
            (0..v.len() / stride)
                .map(|i| {
                    let p = i * stride + 3;
                    Rgba::new(v[p], v[p + 1], v[p + 2], v[p + 3])
                })
                .collect(),
        )
    }

    /// Triangles as coordinate triples — for orientation math (`auto_orient::best_up`).
    pub fn tris(&self) -> Vec<[Vec3; 3]> {
        let (verts, tris) = self.to_indexed();
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.indices();
                [verts[a as usize], verts[b as usize], verts[c as usize]]
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
        Solid::wrap(boolean(&self.0, &other.0, OpType::Add))
    }
    pub fn difference(&self, other: &Solid) -> Solid {
        Solid::wrap(boolean(&self.0, &other.0, OpType::Subtract))
    }
    pub fn intersection(&self, other: &Solid) -> Solid {
        Solid::wrap(boolean(&self.0, &other.0, OpType::Intersect))
    }

    /// Minkowski sum `self ⊕ other` (`minkowski()`, J.4.4) — Manifold's NATIVE `minkowski_sum` (elalish/
    /// manifold PR #666: union of per-face convex hulls, with a convex⊕convex + convex⊕non-convex fast
    /// path). Every point of `self` translated by every point of `other`; the dominant use is rounding /
    /// offsetting a shape by a convex probe (sphere/box). Validated by VOLUME-RESIDUAL (`differ.rs`), not
    /// bit-exact — a mesh Minkowski sum is topologically unlike CGAL's Nef result but shape-identical.
    pub fn minkowski_sum(&self, other: &Solid) -> Solid {
        Solid::wrap(mesh_or_empty(self.0.minkowski_sum(&other.0), "minkowski"))
    }

    /// Union many solids at once — the C++ `BatchBoolean(Add)` strategy: a min-heap by vert count
    /// (insertion-order tie-break) always unions the two SMALLEST next, giving a balanced
    /// O(total·log n) merge tree. The left fold this replaced was "a perf trick, not a semantic" —
    /// semantically true, complexity-fatal: a 101-layer union re-triangulated a 2.3M-tri
    /// accumulator per op (the M.7.3.2 outlet runaway). Same solid out, deterministic order.
    /// Empty ⇒ empty solid.
    pub fn batch_union(solids: &[Solid]) -> Solid {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        use std::rc::Rc;
        if solids.is_empty() {
            return Solid::wrap(Mesh::default());
        }
        if solids.len() == 1 {
            return solids[0].clone();
        }
        let mut slab: Vec<Option<Rc<Mesh>>> = Vec::with_capacity(2 * solids.len());
        let mut heap: BinaryHeap<Reverse<(usize, u64, usize)>> =
            BinaryHeap::with_capacity(solids.len());
        for (i, s) in solids.iter().enumerate() {
            heap.push(Reverse((s.0.num_vert(), i as u64, i)));
            slab.push(Some(s.0.clone()));
        }
        let mut serial = solids.len() as u64;
        loop {
            let Reverse((_, _, ia)) = heap.pop().expect("heap has >= 2 entries");
            let Reverse((_, _, ib)) = heap.pop().expect("heap has >= 2 entries");
            let a = slab[ia].take().expect("heap indexes live slab entries");
            let b = slab[ib].take().expect("heap indexes live slab entries");
            let u = boolean(&a, &b, OpType::Add);
            if heap.is_empty() {
                return Solid::wrap(u);
            }
            heap.push(Reverse((u.num_vert(), serial, slab.len())));
            serial += 1;
            slab.push(Some(Rc::new(u)));
        }
    }

    /// The convex hull of many solids COMBINED (`hull()`, J.4.1) — one quickhull over the union of
    /// their vertices (Manifold `Hull(points)`). `hull()` of one solid is its own convex hull.
    /// Empty ⇒ empty solid.
    pub fn batch_hull(solids: &[Solid]) -> Solid {
        let pts: Vec<Vec3> = solids.iter().flat_map(|s| s.0.vert_pos.clone()).collect();
        Solid::wrap(mesh_or_empty(Mesh::hull_of_points(&pts), "hull"))
    }

    // --- transforms ------------------------------------------------------------------------------

    pub fn translate(&self, v: Vec3) -> Solid {
        Solid::wrap(mesh_or_empty(
            self.0.transform(Mat3x4::translate(v)),
            "translate",
        ))
    }
    /// Rotate by Euler angles in DEGREES (X then Y then Z).
    pub fn rotate(&self, x_deg: f64, y_deg: f64, z_deg: f64) -> Solid {
        Solid::wrap(mesh_or_empty(
            self.0.transform(rotate_xyz_degrees(x_deg, y_deg, z_deg)),
            "rotate",
        ))
    }
    /// Apply a 3×4 [`Affine`] (row-major; transposed to the kernel's column basis at the boundary).
    pub fn transform(&self, m: &Affine) -> Solid {
        Solid::wrap(mesh_or_empty(self.0.transform(to_mat3x4(m)), "transform"))
    }

    /// Rotate so local +Z maps onto unit `axis` — used to point a connector's cap along its
    /// derived build axis before translating it onto the cut. Rodrigues' rotation between vectors;
    /// the antipodal (+Z→−Z) case flips about X. A zero/degenerate axis leaves it unrotated.
    pub fn align_z_to(&self, axis: Vec3) -> Solid {
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
        // A column-major 3×4: columns 0..2 are R's columns, column 3 the translation. Wrapped as an
        // Affine via from_column_major so no transpose is redone (transform re-transposes to Manifold).
        let m = [
            r[0][0], r[1][0], r[2][0], //
            r[0][1], r[1][1], r[2][1], //
            r[0][2], r[1][2], r[2][2], //
            0.0, 0.0, 0.0,
        ];
        self.transform(&Affine::from_column_major(m))
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
            let normal = Vec3::from_array(normal);
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

    /// Split into connected components — one Solid per maximal connected solid BODY, with its enclosed
    /// voids (magnet pockets, cable channels) kept CARVED IN. A presliced part (many disjoint slabs
    /// unioned into one manifold) splits into its individual slabs; a single connected solid — even one
    /// riddled with internal cavities — comes back as ONE piece. The reason (T.2a): the print pipeline
    /// orients each piece with `auto_orient::best_up` and packs it, but scoring a whole presliced blob
    /// picks ONE meaningless build-up (the uniform-45° dogfood bug), so the blob has to break into
    /// printable pieces FIRST.
    ///
    /// Backed by Manifold's native `Decompose()` (manifold-csg ≥ 0.3.3). Decompose splits into
    /// topologically disconnected manifolds — but a solid-with-void surfaces as an OUTER body (positive
    /// volume) PLUS each void as a separate INVERTED shell (negative volume), so we re-fold every void
    /// into the body that geometrically encloses it. Every returned solid is a real, checked 2-manifold.
    /// The old path was a hand-rolled union-find over the exported mesh; it over-segmented on the
    /// coincident-but-distinct verts Manifold leaves along boolean seams, rebuilt OPEN shells, and
    /// SILENTLY dropped every NotManifold fragment — window_light_blocker (1 body + 88 magnet pockets)
    /// came back as ZERO components, so the whole model collapsed to one un-decomposable plate with the
    /// pockets extracted (W.4). Components come back sorted by bbox-min corner then triangle count — a
    /// GEOMETRIC key stable across Manifold's internal mesh reordering (S.4), so a per-component
    /// orientation override stays pinned to the same slab across a re-render.
    pub fn components(&self) -> Vec<Solid> {
        if self.is_empty() {
            return Vec::new();
        }
        // A real body has positive signed volume; a void surfaces as an inverted (negative) shell.
        let (bodies, cavities): (Vec<Solid>, Vec<Solid>) = self
            .0
            .decompose()
            .into_iter()
            .map(Solid::wrap)
            .partition(|s| s.volume() >= 0.0);
        // (decompose is fab-manifold's native port of Manifold Decompose — same body/void surfacing.)
        tracing::debug!(
            bodies = bodies.len(),
            cavities = cavities.len(),
            "components: native decompose"
        );
        // One body (with any number of voids inside it) IS the whole solid — hand it back untouched,
        // voids intact, num_tri preserved (no round-trip). This is the overwhelmingly common case: a
        // single part, and every cut CELL of a sheet.
        if bodies.len() <= 1 {
            return vec![self.clone()];
        }
        // Many disjoint bodies. With NO voids there's no nesting (disjoint manifolds can't overlap), so
        // the decomposed bodies ARE the pieces. Otherwise carve: each piece = the original restricted to
        // that body's region, minus any SMALLER body nested inside it (an island floating in this body's
        // void). Difference with a disjoint solid is a safe no-op, so selecting nests by bbox is fine.
        const EPS: f64 = 1e-6;
        let boxes: Vec<([f64; 3], [f64; 3])> = bodies
            .iter()
            .map(|s| {
                s.bbox()
                    .map_or(([0.0; 3], [0.0; 3]), |(a, b)| (a.to_array(), b.to_array()))
            })
            .collect();
        let bbox_vol =
            |b: &([f64; 3], [f64; 3])| (b.1[0] - b.0[0]) * (b.1[1] - b.0[1]) * (b.1[2] - b.0[2]);
        let inside = |inner: &([f64; 3], [f64; 3]), outer: &([f64; 3], [f64; 3])| {
            (0..3).all(|d| outer.0[d] <= inner.0[d] + EPS && outer.1[d] + EPS >= inner.1[d])
        };
        let mut solids: Vec<Solid> = if cavities.is_empty() {
            bodies
        } else {
            bodies
                .iter()
                .enumerate()
                .map(|(i, body)| {
                    let mut piece = self.intersection(body);
                    for (j, other) in bodies.iter().enumerate() {
                        if i != j
                            && inside(&boxes[j], &boxes[i])
                            && bbox_vol(&boxes[j]) < bbox_vol(&boxes[i])
                        {
                            piece = piece.difference(other);
                        }
                    }
                    piece
                })
                .collect()
        };
        // LOUD on a non-manifold result — never silently drop a piece again (chotchki's rule); an empty
        // carve (a body fully consumed by a nested subtraction) is expected, so quietly drop only those.
        solids.retain(|s| {
            if s.is_empty() {
                return false;
            }
            if !s.is_manifold() {
                tracing::warn!(
                    vol = s.volume(),
                    tris = s.num_tri(),
                    "components() produced a NON-MANIFOLD piece — keeping it, but this is a bug"
                );
            }
            true
        });
        // Geometric, S.4-stable order — a SELF-CONTAINED key, not a lean on decompose()'s input order.
        // bbox corners (position + extent), then topology counts, then volume. `total_cmp` (not
        // `partial_cmp().unwrap_or(Equal)`) makes each f64 comparison itself a total order — no NaN/±0
        // collapse to Equal. Two DISTINCT pieces tie here only if congruent at the same bbox (identical
        // both corners, both counts, AND volume) — geometrically impossible for disjoint solids —
        // so a future parallel/unstable decompose() can't reintroduce S.4 through this sort on any real
        // input; the residual exact tie falls back to decompose order (serial + deterministic today).
        solids.sort_by(|a, b| {
            let corners = |s: &Solid| {
                s.bbox()
                    .map_or(([f64::INFINITY; 3], [f64::INFINITY; 3]), |(m, x)| {
                        (m.to_array(), x.to_array())
                    })
            };
            let cmp3 = |x: &[f64; 3], y: &[f64; 3]| {
                x.iter()
                    .zip(y)
                    .map(|(p, q)| p.total_cmp(q))
                    .find(|o| o.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            };
            let (amin, amax) = corners(a);
            let (bmin, bmax) = corners(b);
            cmp3(&amin, &bmin)
                .then_with(|| cmp3(&amax, &bmax))
                .then_with(|| a.num_tri().cmp(&b.num_tri()))
                .then_with(|| a.num_vert().cmp(&b.num_vert()))
                .then_with(|| a.volume().total_cmp(&b.volume()))
        });
        solids
    }

    // --- half-space cuts (the slicer primitives, 11.4) -------------------------------------------

    /// Split by the plane `normal·p = offset` into `(positive, negative)` — the positive half is the
    /// `normal·p > offset` side. Both halves are independent solids; this is the slicer primitive
    /// (11.4), preferred over `trim_by_plane` because both sides come back clean.
    pub fn split_by_plane(&self, normal: Vec3, offset: f64) -> (Solid, Solid) {
        let (pos, neg) = self.0.split_by_plane(normal, offset);
        (Solid::wrap(pos), Solid::wrap(neg))
    }
    /// Keep only the `normal·p > offset` half (drops the rest). NOTE upstream #1516: trimmed halves
    /// may not re-union cleanly (coincident faces) — use `split_by_plane` when you need both sides.
    pub fn trim_by_plane(&self, normal: Vec3, offset: f64) -> Solid {
        Solid::wrap(self.0.trim_by_plane(normal, offset))
    }

    // --- queries ---------------------------------------------------------------------------------

    /// Err if the solid isn't a valid 2-manifold — the gate a slice/connector result must pass.
    pub fn check(&self) -> Result<()> {
        check::strictly(&self.0).map_err(|e| anyhow!("non-manifold solid: {e}"))
    }
    pub fn is_manifold(&self) -> bool {
        self.0.is_manifold()
    }
    /// This solid under FRESH mesh-instance IDs (P.2 serve semantics — see
    /// `fab_manifold::mesh::Mesh::as_fresh_instance`): geometry identical, provenance IDs re-minted
    /// so a served cache copy relates to its siblings exactly like a fresh render would.
    #[must_use]
    pub fn as_fresh_instance(&self) -> Solid {
        Solid::wrap(self.0.as_fresh_instance())
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
    pub fn bbox(&self) -> Option<(Vec3, Vec3)> {
        if self.0.is_empty() {
            return None;
        }
        let b = self.0.bounding_box();
        Some((b.min, b.max))
    }

    /// Enclosed volume — a topology-invariant scalar the differential harness compares (G.3.7).
    pub fn volume(&self) -> f64 {
        self.0.volume()
    }

    /// Total surface area — the differential harness's second bulk metric.
    pub fn surface_area(&self) -> f64 {
        self.0.surface_area()
    }

    /// Genus (handle count). Euler characteristic of the closed surface is `2 - 2·genus`.
    pub fn genus(&self) -> i32 {
        check::genus(&self.0)
    }

    /// `projection()` — flatten this solid to a 2D [`Section`] (the 3D→2D bridge, J.3.6). `cut = true`
    /// takes the cross-section at `z = 0` (Manifold `slice`); `cut = false` is the shadow — the whole
    /// solid projected onto the XY plane (Manifold `project`).
    pub fn project_2d(&self, cut: bool) -> Section {
        if cut {
            Section::wrap(section_or_empty(self.0.slice_at_z(0.0), "projection cut"))
        } else {
            Section::wrap(section_or_empty(self.0.project(), "projection shadow"))
        }
    }
}

/// Resample each contour of a 2D profile for a TWISTED [`extrude`](Section::extrude), matching OpenSCAD:
/// split each edge into `round(edge_len / perimeter · facets)` even segments (min 1), corners preserved,
/// so the swept helical walls line up. `facets` is `$fn`; when unset (< 3) OpenSCAD fragments by `$fs`
/// instead, which we approximate with its default fragment length of 2.0 (≈ `perimeter / 2` segments).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a segment count is a small non-negative rounded value; max(1.0) floors it before the cast"
)]
fn resample_for_twist(polygons: &[Vec<[f64; 2]>], facets: u32) -> Vec<Vec<[f64; 2]>> {
    let dist = |a: [f64; 2], b: [f64; 2]| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    polygons
        .iter()
        .map(|contour| {
            let n = contour.len();
            let perimeter: f64 = (0..n).map(|i| dist(contour[i], contour[(i + 1) % n])).sum();
            if n < 3 || perimeter <= 0.0 {
                return contour.clone(); // not a fillable ring — leave it be
            }
            // $fn sets the perimeter fragment count; without it, OpenSCAD's default $fs = 2.0 length.
            let frags = if facets >= 3 {
                f64::from(facets)
            } else {
                (perimeter / 2.0).round().max(3.0)
            };
            let mut out = Vec::new();
            for i in 0..n {
                let (a, b) = (contour[i], contour[(i + 1) % n]);
                let segs = (dist(a, b) / perimeter * frags).round().max(1.0) as u32;
                for k in 0..segs {
                    let t = f64::from(k) / f64::from(segs);
                    out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                }
            }
            out
        })
        .collect()
}

/// A 2D region — the unit the 2D subsystem (J.3) consumes and produces, a typed newtype over Manifold's
/// `CrossSection` (which bundles Clipper2, the same 2D lib OpenSCAD 2021+ uses). The 2D analogue of
/// [`Solid`]: the evaluator's [`Shape2D`](fab_lang::Shape2D) tree lowers to one of these, extrusion
/// bridges it to a `Solid`, and `projection` bridges a `Solid` back to one.
///
/// Unlike [`Solid`] (whose `Rc<Mesh>` keeps it thread-local), a `Section` is a plain `Send + Sync`
/// value type: `CrossSection` is deep-owned `Vec` contour data with no shared handle. It used to be
/// `!Send` to mirror the C++ `CrossSection`'s clone-shares-state hazard — gone since the kernel went
/// pure Rust (M.7.4).
#[derive(Clone)]
pub struct Section(CrossSection);

impl Section {
    /// The single construction point.
    fn wrap(c: CrossSection) -> Self {
        Section(c)
    }

    /// The empty region (no area) — the 2D identity for union, absorbing for intersection.
    pub fn empty() -> Self {
        Section::wrap(CrossSection::new())
    }

    /// A region from closed contours, resolved by Manifold's default `Positive` fill rule (positively-
    /// wound contours fill, negatively-wound are holes). Used for contours WE produce that are already
    /// correctly wound — a `project` shadow, a twist resample — where the winding carries the intent.
    pub fn from_polygons(contours: &[Vec<[f64; 2]>]) -> Self {
        Section::wrap(section_or_empty(
            CrossSection::from_polygons(&to_vec2_contours(contours)),
            "from_polygons",
        ))
    }

    /// A region from a `polygon()` primitive's raw contours (J.3.2) — the EVEN-ODD fill rule, matching
    /// OpenSCAD: `polygon()` fills a contour by NESTING depth, not winding, so a lone clockwise loop (a
    /// BOSL2 `star()`/`hexagon()` path, which winds CW) still fills, and a contour nested inside another
    /// is a hole regardless of its winding. The default `Positive` rule would drop the CW loop to empty.
    pub fn polygon(contours: &[Vec<[f64; 2]>]) -> Self {
        Section::wrap(section_or_empty(
            CrossSection::from_polygons_with(&to_vec2_contours(contours), FillRule::EvenOdd),
            "polygon",
        ))
    }

    pub fn union(&self, other: &Section) -> Section {
        Section::wrap(self.0.union(&other.0))
    }
    pub fn difference(&self, other: &Section) -> Section {
        Section::wrap(self.0.difference(&other.0))
    }
    pub fn intersection(&self, other: &Section) -> Section {
        Section::wrap(self.0.intersection(&other.0))
    }

    /// The convex hull of many regions COMBINED (`hull()` over 2D children, X.4) — pools every section's
    /// contour points and runs Manifold's `CrossSection::hull_of` (an Andrew monotone-chain, deterministic
    /// via its internal `total_cmp` sort). An empty region contributes no points; all-empty → empty. The
    /// 2D twin of [`Solid::batch_hull`].
    pub fn hull_of(sections: &[Section]) -> Section {
        let cs: Vec<CrossSection> = sections.iter().map(|s| s.0.clone()).collect();
        Section::wrap(CrossSection::hull_of(&cs))
    }

    /// `offset()` — inflate the outline by `delta` (negative shrinks), finishing convex corners per
    /// `join`. `segments` is the Round join's facet count (`$fn`-resolved upstream; ignored otherwise);
    /// the miter limit is Clipper2's default of 2.0.
    pub fn offset(&self, delta: f64, join: Join2D, segments: i32) -> Section {
        let jt = match join {
            Join2D::Round => JoinType::Round,
            Join2D::Miter => JoinType::Miter,
            // OpenSCAD's `offset(chamfer = true)` squares the corner off — its source maps chamfer →
            // Clipper2 `jtSquare`, NOT `jtBevel` (verified by area vs 2026.06.12: square → 78.2548).
            Join2D::Bevel => JoinType::Square,
        };
        Section::wrap(section_or_empty(
            self.0.offset(delta, jt, 2.0, segments),
            "offset",
        ))
    }

    /// Apply a 2×3 [`Affine2`] (row-major; transposed to the kernel's column basis at the boundary).
    pub fn transform(&self, m: &Affine2) -> Section {
        Section::wrap(section_or_empty(
            self.0.transform(to_mat2x3(m)),
            "2d transform",
        ))
    }

    /// Sweep this region into a 3D [`Solid`] — the 2D→3D bridge (`linear_extrude` / `rotate_extrude`,
    /// J.3.4 / J.3.5). Linear uses Manifold's twist/scale/slice extrude (then centers on `z = 0` when
    /// asked); rotate revolves it about +Z.
    pub fn extrude(&self, kind: &ExtrudeKind) -> Solid {
        match *kind {
            ExtrudeKind::Linear {
                height,
                twist,
                scale,
                slices,
                facets,
                center,
            } => {
                // TWIST: Manifold spins the OPPOSITE way from OpenSCAD (so negate), and OpenSCAD resamples
                // the profile perimeter to `facets` ($fn) points before sweeping — each edge into
                // `round(len / perimeter · facets)` even segments — so the helical walls line up. The raw
                // profile only matches an un-twisted sweep. J.3.4.1 (measured 22.8% residual → 6.9% after
                // the negate → ~1-2% at typical $fn after the resample; a small per-slice tessellation-phase
                // remainder is an accepted, documented divergence).
                //
                // The resampled contours go to the extrude as RAW polygons (the C++ `Manifold::Extrude`
                // signature) — a CrossSection round-trip would let i_overlay's normalization DROP the
                // resample's deliberately-collinear edge points, collapsing the profile back to its
                // corners and gutting the helical walls (measured: the twist-90 square lost 6.5% volume).
                let m = if twist == 0.0 {
                    self.0.extrude_with_options(
                        height,
                        slices as i32,
                        0.0,
                        Vec2::new(scale[0], scale[1]),
                    )
                } else {
                    let resampled = resample_for_twist(&self.to_polygons(), facets);
                    bridge::extrude_polygons(
                        &to_vec2_contours(&resampled),
                        height,
                        slices as i32,
                        -twist,
                        Vec2::new(scale[0], scale[1]),
                    )
                };
                let s = Solid::wrap(m);
                if center {
                    s.translate(Vec3::new(0.0, 0.0, -height / 2.0))
                } else {
                    s
                }
            }
            ExtrudeKind::Rotate { angle, segments } => {
                Solid::wrap(self.0.revolve(segments as i32, angle))
            }
        }
    }

    /// Whether the region is empty (no area).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Total enclosed area — the 2D differential's bulk metric (J.3.7).
    pub fn area(&self) -> f64 {
        self.0.area()
    }

    /// The region's contours as closed point rings — the extract path + the inert data a `Section`
    /// crosses a thread boundary as.
    pub fn to_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        self.0.to_polygons()
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
    let gl = s.0.to_mesh_gl();
    let (v, stride, idx) = (gl.vert_properties, gl.num_prop, gl.tri_verts);
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
    fn color_sets_every_vertex_and_survives_booleans() {
        let red = Rgba::opaque(1.0, 0.0, 0.0);
        let blue = Rgba::opaque(0.0, 0.0, 1.0);
        let cube = Solid::cube(10.0, 10.0, 10.0, false);

        // Uncolored solids carry no color property (MeshGL stride 3, not 7).
        assert!(cube.vertex_colors().is_none());
        // with_color sets EVERY vertex.
        let red_cube = cube.with_color(red);
        let cols = red_cube.vertex_colors().expect("colored → Some");
        assert!(!cols.is_empty() && cols.iter().all(|&c| c == red));

        // Outer color() over a boolean is UNIFORM: color the RESULT → all red (OpenSCAD outer-wins).
        let hole = Solid::cube(6.0, 6.0, 6.0, false).translate(Vec3::new(5.0, 5.0, 5.0));
        let uniform = cube.difference(&hole).with_color(red);
        assert!(uniform.vertex_colors().unwrap().iter().all(|&c| c == red));

        // Distinct colors SURVIVE a union — Manifold carries per-vertex props through the boolean.
        let blue_cube = Solid::cube(6.0, 6.0, 6.0, false)
            .translate(Vec3::new(20.0, 0.0, 0.0))
            .with_color(blue);
        let both = red_cube.union(&blue_cube).vertex_colors().unwrap();
        assert!(both.contains(&red) && both.contains(&blue));

        // Re-coloring OVERWRITES — color("red") color("blue") cube → the outer red would win.
        assert!(
            red_cube
                .with_color(blue)
                .vertex_colors()
                .unwrap()
                .iter()
                .all(|&c| c == blue)
        );
    }

    #[test]
    fn to_3mf_bytes_carries_the_real_mesh_and_distinct_colors() {
        use std::io::Read;
        let read_model = |bytes: &[u8]| -> String {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
            let mut f = zip.by_name("3D/3dmodel.model").unwrap();
            let mut s = String::new();
            f.read_to_string(&mut s).unwrap();
            s
        };

        // Two DISJOINT colored cubes (translated apart → no boolean seam, so colors stay exactly
        // red/blue — a clean two-material check on a real Manifold mesh).
        let red = Rgba::opaque(1.0, 0.0, 0.0);
        let blue = Rgba::opaque(0.0, 0.0, 1.0);
        let red_cube = Solid::cube(10.0, 10.0, 10.0, false).with_color(red);
        let blue_cube = Solid::cube(6.0, 6.0, 6.0, false)
            .translate(Vec3::new(20.0, 0.0, 0.0))
            .with_color(blue);
        let both = red_cube.union(&blue_cube);

        let (verts, tris) = both.to_indexed();
        let model = read_model(&both.to_3mf_bytes());
        assert_eq!(
            model.matches("<vertex ").count(),
            verts.len(),
            "every kernel vertex emitted"
        );
        assert_eq!(
            model.matches("<triangle ").count(),
            tris.len(),
            "every kernel triangle emitted"
        );
        assert_eq!(
            model.matches("<base ").count(),
            2,
            "red + blue survive as two materials"
        );
        assert!(
            model.contains("displaycolor=\"#FF0000FF\"")
                && model.contains("displaycolor=\"#0000FFFF\"")
        );

        // An uncolored solid → a plain mesh, no materials resource.
        let plain = Solid::cube(5.0, 5.0, 5.0, false).to_3mf_bytes();
        assert!(!read_model(&plain).contains("basematerials"));
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
        let b = Solid::cube(10.0, 10.0, 10.0, true).translate(Vec3::new(30.0, 0.0, 0.0));
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
    fn components_split_a_disjoint_union_deterministically() {
        // Two cubes 100mm apart, unioned into one manifold (the presliced-blob shape). components()
        // must break them back into two valid solids, ordered by bbox-min X (the +100 cube last).
        let a = Solid::cube(10.0, 10.0, 10.0, true);
        let b = Solid::cube(10.0, 10.0, 10.0, true).translate(Vec3::new(100.0, 0.0, 0.0));
        let blob = a.union(&b);
        let comps = blob.components();
        assert_eq!(comps.len(), 2, "disjoint union splits into two");
        for c in &comps {
            c.check().unwrap();
            let (min, max) = c.bbox().unwrap();
            assert!((max[0] - min[0] - 10.0).abs() < 1e-4, "each is a 10mm cube");
        }
        // Geometric ordering: the origin cube (min.x ≈ -5) before the +100 cube (min.x ≈ 95).
        assert!(comps[0].bbox().unwrap().0[0] < comps[1].bbox().unwrap().0[0]);

        // A single connected solid is one component (and comes back intact — the fast path).
        let one = Solid::cube(10.0, 10.0, 10.0, true).components();
        assert_eq!(one.len(), 1);
        assert_eq!(
            one[0].num_tri(),
            Solid::cube(10.0, 10.0, 10.0, true).num_tri()
        );

        // Empty in, empty out.
        assert!(
            Solid::cube(1.0, 1.0, 1.0, true)
                .difference(&Solid::cube(2.0, 2.0, 2.0, true))
                .components()
                .is_empty()
        );
    }

    #[test]
    fn components_keeps_an_enclosed_cavity_with_its_host() {
        // A fully-enclosed void (a captured magnet pocket) is a separate MESH SHELL sharing no
        // vertices with the outer shell — the raw union-find would split it into a phantom
        // inverted-normal solid AND erase the pocket from the host (window_light_blocker dogfood).
        // components() must fold the cavity back into the host: ONE piece, cavity intact.
        let hollow = Solid::cube(20.0, 20.0, 20.0, true).difference(&Solid::sphere(6.0, 32));
        let comps = hollow.components();
        assert_eq!(
            comps.len(),
            1,
            "solid-with-void is ONE component, not outer+cavity"
        );
        comps[0].check().unwrap();
        let v = comps[0].volume();
        // cube 8000 − sphere(r6) ≈ 8000 − 885 ≈ 7115; NOT the solid cube (8000, pocket erased).
        assert!(
            (7000.0..7200.0).contains(&v),
            "cavity survives, got vol {v}"
        );

        // A disjoint piece FLOATING inside a void is a real separate piece (nested-shell parity): a
        // hollow shell with a smaller solid ball rattling inside → 2 components (shell + ball).
        let ball = Solid::sphere(2.0, 24);
        let nested = Solid::cube(20.0, 20.0, 20.0, true)
            .difference(&Solid::sphere(6.0, 32))
            .union(&ball);
        assert_eq!(
            nested.components().len(),
            2,
            "a solid island inside the void stays its own piece; only the cavity merges"
        );
    }

    #[test]
    fn components_folds_a_grid_of_enclosed_pockets_into_one_body() {
        // The window_light_blocker failure class (W.4): a plate riddled with fully-enclosed pockets
        // (magnet voids that never reach a face). The old union-find over-segmented on boolean-seam
        // verts and dropped every rebuilt-open shell → ZERO components. Native decompose folds each
        // void back into the body → ONE piece, pockets carved.
        let mut plate = Solid::cube(60.0, 60.0, 6.0, true);
        for gx in -2..=2 {
            for gy in -2..=2 {
                // r1.6 ball centred in the 6mm-thick plate (spans z=-1.6..1.6 ⊂ -3..3) → fully enclosed.
                let pocket = Solid::sphere(1.6, 24).translate(Vec3::new(
                    f64::from(gx) * 10.0,
                    f64::from(gy) * 10.0,
                    0.0,
                ));
                plate = plate.difference(&pocket);
            }
        }
        let comps = plate.components();
        assert_eq!(comps.len(), 1, "plate + 25 enclosed pockets is ONE body");
        comps[0].check().unwrap();
        // The single piece IS the whole solid, pockets carved (volume well below the solid 21600).
        assert!((comps[0].volume() - plate.volume()).abs() < 1.0);
        assert!(plate.volume() < 21_600.0 - 300.0, "the pockets stay hollow");
    }

    #[test]
    fn components_carves_two_disjoint_bodies_that_each_have_a_pocket() {
        // The multi-body-WITH-cavities carve path (new in W.4): two disjoint plates, each with its own
        // enclosed pocket, unioned into one manifold. Must split into two pieces, each keeping its own
        // void — not fold both voids into one body, not drop a piece.
        let one = Solid::cube(20.0, 20.0, 8.0, true).difference(&Solid::sphere(2.0, 24));
        let two = one.translate(Vec3::new(100.0, 0.0, 0.0));
        let blob = one.union(&two);
        let comps = blob.components();
        assert_eq!(comps.len(), 2, "two disjoint hollow plates → two pieces");
        for c in &comps {
            c.check().unwrap();
            // each keeps its pocket: a solid 20×20×8 cube is 3200; the void drops it below that.
            assert!(
                c.volume() < 3200.0 - 20.0,
                "the pocket survives in each piece"
            );
            assert!(
                c.volume() > 3000.0,
                "only ONE pocket per piece (not both folded in)"
            );
        }
        assert!(
            comps[0].bbox().unwrap().0[0] < comps[1].bbox().unwrap().0[0],
            "sorted by bbox-min X"
        );
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
        let along_x = cone.align_z_to(Vec3::new(1.0, 0.0, 0.0));
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
        let b = Solid::cube(30.0, 30.0, 30.0, true).translate(Vec3::new(15.0, 0.0, 0.0));
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
        let (pos, neg) = c.split_by_plane(Vec3::new(1.0, 0.0, 0.0), 0.0); // (x>0, x<0)
        pos.check().unwrap();
        neg.check().unwrap();
        // Each half is 10mm thick on X; the positive half is [0, 10], the negative [-10, 0].
        assert!((pos.bbox().unwrap().1[0] - 10.0).abs() < 1e-6);
        assert!((pos.bbox().unwrap().0[0] - 0.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().0[0] - -10.0).abs() < 1e-6);
        assert!((neg.bbox().unwrap().1[0] - 0.0).abs() < 1e-6);
    }
}
