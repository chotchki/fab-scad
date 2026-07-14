//! Oracle A — the C++ Manifold differential harness.
//!
//! A [`KernelDriver`] trait with two backends — [`RustKernel`] (this crate) and [`CppKernel`]
//! (`manifold3d`, the linked C++ kernel fab-scad already ships) — runs the SAME op through both and
//! compares. R0 compares the SCALAR properties an identity ingest can produce: volume + surface area
//! (rel), bbox (rel). The triangulation-INDEPENDENT boolean-residual metric (G.3.7,
//! `vol((A−B) ∪ (B−A)) / vol(A) < 1e-5`) is the R1 tool — it needs Rust booleans, which don't exist
//! yet, so it's parked until M.1. Backstop metrics (genus, component count) join as this crate grows.
//!
//! This is a SCAFFOLD: it gates R0..R.X, then goes away at R.X when we freeze goldens and drop
//! `manifold3d`. Native-only (needs the C++ toolchain) — the whole module is behind the `oracle`
//! feature + a non-wasm cfg (see `lib.rs`).

use crate::linalg::{Box3, Vec3};
use crate::mesh::{Mesh, MeshGl};

/// One geometry kernel, reduced to the surface the differential needs. Stateless → associated
/// functions, so generic code says `K::volume(&s)`.
pub trait KernelDriver {
    /// The kernel's opaque solid handle (our [`Mesh`], or a C++ `manifold3d::Manifold`).
    type Solid;
    /// A label for divergence reports.
    fn name() -> &'static str;
    /// Ingest a flat mesh buffer. `Err` when the kernel rejects it (e.g. non-manifold) — the
    /// differential treats "both reject" as agreement.
    fn ingest(mesh: &MeshGl) -> Result<Self::Solid, String>;
    /// Signed volume.
    fn volume(s: &Self::Solid) -> f64;
    /// Surface area.
    fn surface_area(s: &Self::Solid) -> f64;
    /// Axis-aligned bounding box.
    fn bbox(s: &Self::Solid) -> Box3;
}

/// The Rust kernel under test — this crate's [`Mesh`].
pub struct RustKernel;

impl KernelDriver for RustKernel {
    type Solid = Mesh;
    fn name() -> &'static str {
        "rust(fab-manifold)"
    }
    fn ingest(mesh: &MeshGl) -> Result<Mesh, String> {
        let m = Mesh::from_mesh_gl(mesh);
        if !m.is_manifold() {
            return Err("rust: not manifold".to_string());
        }
        Ok(m)
    }
    fn volume(s: &Mesh) -> f64 {
        s.volume()
    }
    fn surface_area(s: &Mesh) -> f64 {
        s.surface_area()
    }
    fn bbox(s: &Mesh) -> Box3 {
        s.b_box
    }
}

/// The C++ reference kernel — `manifold3d` (the linked Manifold v3.5.1).
pub struct CppKernel;

impl KernelDriver for CppKernel {
    type Solid = manifold3d::Manifold;
    fn name() -> &'static str {
        "cpp(manifold3d)"
    }
    fn ingest(mesh: &MeshGl) -> Result<manifold3d::Manifold, String> {
        // MeshGL64 indices are u64; ours are u32.
        let tris: Vec<u64> = mesh.tri_verts.iter().map(|&i| i as u64).collect();
        manifold3d::Manifold::from_mesh_f64(&mesh.vert_properties, mesh.num_prop, &tris)
            .map_err(|e| format!("cpp: {e:?}"))
    }
    fn volume(s: &manifold3d::Manifold) -> f64 {
        s.volume()
    }
    fn surface_area(s: &manifold3d::Manifold) -> f64 {
        s.surface_area()
    }
    fn bbox(s: &manifold3d::Manifold) -> Box3 {
        let bb = s.bounding_box().expect("finite bounding box");
        Box3 {
            min: Vec3::from(bb.min()),
            max: Vec3::from(bb.max()),
        }
    }
}

/// Export a C++ `manifold3d::Manifold` to our flat [`MeshGl`] — the bridge that lets the C++ kernel
/// GENERATE diverse test geometry (sphere, cylinder, boolean results) that both engines then re-ingest.
pub fn cpp_to_mesh_gl(m: &manifold3d::Manifold) -> MeshGl {
    let (vert_properties, num_prop, tri_u64) = m.to_mesh_f64();
    MeshGl {
        num_prop,
        vert_properties,
        tri_verts: tri_u64.iter().map(|&i| i as u32).collect(),
    }
}

/// One metric that disagreed between the two kernels beyond tolerance.
#[derive(Debug, Clone)]
pub struct Divergence {
    /// Which metric (`volume`, `surface_area`, `bbox.min.x`, …).
    pub metric: String,
    /// The Rust kernel's value.
    pub rust: f64,
    /// The C++ kernel's value.
    pub cpp: f64,
    /// Absolute difference.
    pub abs: f64,
    /// Relative difference (`abs / max(|cpp|, 1e-12)`).
    pub rel: f64,
}

struct Props {
    volume: f64,
    area: f64,
    bbox: Box3,
}

fn props<K: KernelDriver>(mesh: &MeshGl) -> Result<Props, String> {
    let s = K::ingest(mesh)?;
    Ok(Props {
        volume: K::volume(&s),
        area: K::surface_area(&s),
        bbox: K::bbox(&s),
    })
}

/// Run the identical buffer through both kernels and report every scalar metric that diverges by more
/// than `rel_tol` (relative). `Err` only if the two kernels DISAGREE on validity (one ingests, the
/// other rejects) — that itself is a divergence worth surfacing loudly.
pub fn differential(mesh: &MeshGl, rel_tol: f64) -> Result<Vec<Divergence>, String> {
    let r = props::<RustKernel>(mesh);
    let c = props::<CppKernel>(mesh);
    let (r, c) = match (r, c) {
        (Ok(r), Ok(c)) => (r, c),
        (Err(_), Err(_)) => return Ok(Vec::new()), // both reject → agree
        (Ok(_), Err(e)) => return Err(format!("rust accepted, {e}")),
        (Err(e), Ok(_)) => return Err(format!("cpp accepted, {e}")),
    };

    let mut divs = Vec::new();
    let mut check = |metric: &str, rust: f64, cpp: f64| {
        let abs = (rust - cpp).abs();
        let rel = abs / cpp.abs().max(1e-12);
        if rel > rel_tol {
            divs.push(Divergence {
                metric: metric.to_string(),
                rust,
                cpp,
                abs,
                rel,
            });
        }
    };
    check("volume", r.volume, c.volume);
    check("surface_area", r.area, c.area);
    check("bbox.min.x", r.bbox.min.x, c.bbox.min.x);
    check("bbox.min.y", r.bbox.min.y, c.bbox.min.y);
    check("bbox.min.z", r.bbox.min.z, c.bbox.min.z);
    check("bbox.max.x", r.bbox.max.x, c.bbox.max.x);
    check("bbox.max.y", r.bbox.max.y, c.bbox.max.y);
    check("bbox.max.z", r.bbox.max.z, c.bbox.max.z);
    Ok(divs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The M.0.3 identity-op green light: a hand-built unit cube, bit-EXACT volume/area vs C++ (small
    /// integer coords ⇒ no FP cancellation ⇒ literally equal), bbox exact.
    #[test]
    fn cube_identity_matches_cpp_bit_exact() {
        #[rustfmt::skip]
        let verts = vec![
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7, 0,1,5, 0,5,4,
            2,3,7, 2,7,6, 0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mesh = MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        };

        let rs = RustKernel::ingest(&mesh).unwrap();
        let cs = CppKernel::ingest(&mesh).unwrap();
        // Identical buffers + integer coords ⇒ the Kahan sums agree to the bit.
        assert_eq!(
            RustKernel::volume(&rs).to_bits(),
            CppKernel::volume(&cs).to_bits()
        );
        assert_eq!(
            RustKernel::surface_area(&rs).to_bits(),
            CppKernel::surface_area(&cs).to_bits()
        );
        assert!(differential(&mesh, 0.0).unwrap().is_empty());
    }

    /// The real payoff: use the C++ kernel to GENERATE non-trivial geometry (curved, thousands of
    /// tris), then diff both engines on the identical exported buffer. If mathf/linalg/volume are
    /// faithful, they agree to a tight relative tolerance. This is the K.0 thesis in miniature.
    #[test]
    fn generated_solids_match_cpp() {
        let cases: Vec<(&str, manifold3d::Manifold)> = vec![
            ("sphere", manifold3d::Manifold::sphere(10.0, 64)),
            (
                "cylinder",
                manifold3d::Manifold::cylinder(20.0, 7.0, 7.0, 48, true),
            ),
            ("cube", manifold3d::Manifold::cube(3.0, 5.0, 7.0, false)),
            // a boolean result — irregular tri distribution, the interesting stress case
            (
                "sphere − cube",
                manifold3d::Manifold::sphere(10.0, 48)
                    .difference(&manifold3d::Manifold::cube(12.0, 12.0, 12.0, true)),
            ),
        ];
        for (name, solid) in &cases {
            let mesh = cpp_to_mesh_gl(solid);
            // sanity: the exported buffer really is manifold on our side
            assert!(
                RustKernel::ingest(&mesh).is_ok(),
                "{name}: rust rejected the C++ mesh as non-manifold"
            );
            let divs = differential(&mesh, 1e-9).unwrap();
            assert!(divs.is_empty(), "{name}: divergences vs C++: {divs:#?}");
        }
    }

    /// Volume/area track the C++ kernel across a scale sweep (exercises the Kahan sum at magnitudes
    /// where naive summation would drift).
    #[test]
    fn volume_tracks_cpp_across_scales() {
        for &r in &[0.5, 5.0, 50.0, 500.0] {
            let sphere = manifold3d::Manifold::sphere(r, 64);
            let mesh = cpp_to_mesh_gl(&sphere);
            let divs = differential(&mesh, 1e-9).unwrap();
            assert!(divs.is_empty(), "r={r}: {divs:#?}");
        }
    }
}
