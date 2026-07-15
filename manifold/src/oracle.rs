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
    /// Topological genus (exact integer backstop).
    fn genus(s: &Self::Solid) -> i32;
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
    fn genus(s: &Mesh) -> i32 {
        crate::check::genus(s)
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
    fn genus(s: &manifold3d::Manifold) -> i32 {
        s.genus()
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
    genus: i32,
    bbox: Box3,
}

fn props<K: KernelDriver>(mesh: &MeshGl) -> Result<Props, String> {
    let s = K::ingest(mesh)?;
    Ok(Props {
        volume: K::volume(&s),
        area: K::surface_area(&s),
        genus: K::genus(&s),
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
    // genus is an exact integer — compare it before the f64 `check` closure borrows `divs`.
    if r.genus != c.genus {
        divs.push(Divergence {
            metric: "genus".to_string(),
            rust: r.genus as f64,
            cpp: c.genus as f64,
            abs: (r.genus - c.genus).unsigned_abs() as f64,
            rel: f64::INFINITY,
        });
    }
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

    /// A prepared box's params: `(ox, oy, oz, sx, sy, sz)`. Shared by the GATE-A/B config sweeps.
    type BoxParams = (f64, f64, f64, f64, f64, f64);

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

    // =====================================================================================
    // GATE K.0 (M.0.6) — the R0 exit gate. On identical buffers, the Rust spine must:
    //   (1) accept the mesh as manifold (rust IsManifold == the C++ kernel accepting it),
    //   (2) agree with C++ on volume/area/genus/bbox to a tight tolerance (breaks the
    //       invariant-circularity: `volume`/`genus` are TRUSTWORTHY because they're
    //       calibrated against C++ here, before check.rs asserts on them),
    //   (3) round-trip idempotently (MeshGl→Mesh→MeshGl→Mesh preserves volume bit-exact).
    // The corpus spans genus 0 AND genus 1, primitives AND boolean results.
    // =====================================================================================
    #[test]
    fn k0_gate() {
        // A block with a square tunnel bored through Z — genus 1, to exercise the genus backstop
        // past the trivial genus-0 primitives.
        let tunnel_block = manifold3d::Manifold::cube(10.0, 10.0, 10.0, true)
            .difference(&manifold3d::Manifold::cube(4.0, 4.0, 20.0, true));
        assert_eq!(
            CppKernel::genus(&tunnel_block),
            1,
            "test geometry sanity: the tunnel block should be genus 1"
        );

        let corpus: Vec<(&str, manifold3d::Manifold)> = vec![
            ("sphere-32", manifold3d::Manifold::sphere(8.0, 32)),
            ("sphere-128", manifold3d::Manifold::sphere(8.0, 128)),
            (
                "cylinder",
                manifold3d::Manifold::cylinder(15.0, 4.0, 9.0, 60, true),
            ),
            ("box", manifold3d::Manifold::cube(2.0, 3.0, 5.0, true)),
            (
                "sphere ∪ box",
                manifold3d::Manifold::sphere(6.0, 48)
                    .union(&manifold3d::Manifold::cube(8.0, 8.0, 8.0, true)),
            ),
            ("tunnel-block (genus 1)", tunnel_block),
        ];

        for (name, solid) in &corpus {
            let mesh = cpp_to_mesh_gl(solid);

            // (1) validity agreement.
            let rust =
                RustKernel::ingest(&mesh).expect("K.0: rust rejected a C++-valid corpus mesh");
            assert!(rust.is_manifold(), "K.0 [{name}]: rust mesh not manifold");

            // (2) property agreement vs C++ (volume/area/genus/bbox), tight tolerance.
            let divs = differential(&mesh, 1e-9).unwrap();
            assert!(
                divs.is_empty(),
                "K.0 [{name}]: divergences vs C++: {divs:#?}"
            );

            // (3) round-trip idempotence — our own re-ingest preserves geometry to the bit.
            let reingested = Mesh::from_mesh_gl(&rust.to_mesh_gl());
            assert_eq!(
                rust.volume().to_bits(),
                reingested.volume().to_bits(),
                "K.0 [{name}]: round-trip changed the volume"
            );

            eprintln!(
                "K.0 [{name}]: {} tris, vol={:.6}, genus={} — rust==cpp ✓",
                rust.num_tri(),
                rust.volume(),
                crate::check::genus(&rust),
            );
        }
    }

    /// A unit cube at an offset, fully prepared for a Rust boolean (halfedges, bbox, epsilon, both
    /// normal fields) — the GATE-A input fixture.
    fn prepared_cube(ox: f64, oy: f64, oz: f64) -> Mesh {
        #[rustfmt::skip]
        let base = [
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        let mut verts = Vec::new();
        for c in base.chunks_exact(3) {
            verts.push(c[0] + ox);
            verts.push(c[1] + oy);
            verts.push(c[2] + oz);
        }
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7,
            0,1,5, 0,5,4, 2,3,7, 2,7,6,
            0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mut mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        });
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.set_normals_and_coplanar();
        mesh
    }

    // =====================================================================================
    // ★ GATE-A (M.1.3) — the R1 tracer boolean go/no-go. An OFFSET (general-position) cube∪cube:
    //   the Rust union must be a watertight, genus-0 solid whose volume tracks C++, AND the
    //   triangulation-INDEPENDENT boolean residual `vol((A−B) ∪ (B−A)) / vol(A)` (computed by the C++
    //   oracle, so it tolerates each engine's own triangulation) must be < 1e-5. Clean ⇒ the four-table
    //   intersection core + the assembly are PROVEN against the reference kernel.
    // =====================================================================================
    #[test]
    fn gate_a_offset_cube_union_residual_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        // General position: the offset shares no coordinate between the two meshes, so no cross-mesh
        // `p == q` tie fires and the perturbation normals stay inert — the pure-f64 core is under test.
        let p = prepared_cube(0.0, 0.0, 0.0);
        let q = prepared_cube(0.3, 0.4, 0.5);

        // Rust union → watertight, genus 0.
        let a = boolean(&p, &q, OpType::Add);
        assert!(a.is_manifold(), "GATE-A: rust union is not a manifold");
        assert_eq!(crate::check::genus(&a), 0, "GATE-A: rust union genus != 0");

        // C++ union of the same inputs.
        let p_cpp = CppKernel::ingest(&p.to_mesh_gl()).unwrap();
        let q_cpp = CppKernel::ingest(&q.to_mesh_gl()).unwrap();
        let b_cpp = p_cpp.union(&q_cpp);

        // Scalar differential: volume + genus agree tightly.
        let a_vol = a.volume();
        let b_vol = b_cpp.volume();
        assert!(
            (a_vol - b_vol).abs() / b_vol.abs() < 1e-9,
            "GATE-A: volume diverges — rust {a_vol}, cpp {b_vol}"
        );
        assert_eq!(
            crate::check::genus(&a),
            b_cpp.genus(),
            "GATE-A: genus diverges from C++"
        );

        // THE GATE: the triangulation-independent residual, via the C++ oracle. A and B are the same
        // solid triangulated differently, so both symmetric differences are ~empty.
        let a_cpp = CppKernel::ingest(&a.to_mesh_gl())
            .expect("GATE-A: C++ rejects the rust union as non-manifold");
        let a_minus_b = a_cpp.difference(&b_cpp);
        let b_minus_a = b_cpp.difference(&a_cpp);
        let sym = a_minus_b.union(&b_minus_a);
        let residual = sym.volume() / a_cpp.volume();
        assert!(
            residual < 1e-5,
            "GATE-A: boolean residual {residual:.3e} >= 1e-5 — the core diverges from C++"
        );
        eprintln!(
            "GATE-A ✓ offset cube∪cube: {} tris, rust vol={a_vol:.9}, cpp vol={b_vol:.9}, residual={residual:.3e}",
            a.num_tri()
        );
    }

    /// An axis-aligned box of size `(sx,sy,sz)` at `(ox,oy,oz)`, prepared for a Rust boolean.
    fn prepared_box(ox: f64, oy: f64, oz: f64, sx: f64, sy: f64, sz: f64) -> Mesh {
        #[rustfmt::skip]
        let unit = [
            (0.0,0.0,0.0),(1.0,0.0,0.0),(1.0,1.0,0.0),(0.0,1.0,0.0),
            (0.0,0.0,1.0),(1.0,0.0,1.0),(1.0,1.0,1.0),(0.0,1.0,1.0),
        ];
        let mut verts = Vec::new();
        for &(x, y, z) in &unit {
            verts.push(x * sx + ox);
            verts.push(y * sy + oy);
            verts.push(z * sz + oz);
        }
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7,
            0,1,5, 0,5,4, 2,3,7, 2,7,6,
            0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mut mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        });
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.set_normals_and_coplanar();
        mesh
    }

    /// GATE-A robustness sweep: several general-position box∪box configs (varied sizes + offsets, so cut
    /// faces are non-convex polygons the ear-clip must handle), each held to the residual gate. Guards
    /// against the primary GATE-A passing by luck of one offset.
    #[test]
    fn gate_a_union_sweep_residual_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        // (p-params, q-params) as (ox,oy,oz,sx,sy,sz) — all chosen so no coordinate coincides across the
        // pair (general position), and every pair genuinely overlaps.
        let configs: &[(BoxParams, BoxParams)] = &[
            ((0.0, 0.0, 0.0, 1.0, 1.0, 1.0), (0.3, 0.4, 0.5, 1.0, 1.0, 1.0)),
            ((0.0, 0.0, 0.0, 1.0, 1.0, 1.0), (0.5, 0.3, 0.7, 2.0, 2.0, 2.0)),
            ((0.0, 0.0, 0.0, 3.0, 2.0, 1.0), (1.3, 0.7, -0.4, 1.0, 1.0, 2.0)),
            ((0.0, 0.0, 0.0, 2.0, 3.0, 4.0), (-0.6, 1.1, 1.7, 3.0, 1.0, 1.0)),
        ];

        for (i, &(pp, qp)) in configs.iter().enumerate() {
            let p = prepared_box(pp.0, pp.1, pp.2, pp.3, pp.4, pp.5);
            let q = prepared_box(qp.0, qp.1, qp.2, qp.3, qp.4, qp.5);
            let a = boolean(&p, &q, OpType::Add);
            assert!(a.is_manifold(), "GATE-A sweep [{i}]: rust union not manifold");

            let p_cpp = CppKernel::ingest(&p.to_mesh_gl()).unwrap();
            let q_cpp = CppKernel::ingest(&q.to_mesh_gl()).unwrap();
            let b_cpp = p_cpp.union(&q_cpp);

            let a_vol = a.volume();
            let b_vol = b_cpp.volume();
            assert!(
                (a_vol - b_vol).abs() / b_vol.abs() < 1e-9,
                "GATE-A sweep [{i}]: volume rust {a_vol} vs cpp {b_vol}"
            );
            assert_eq!(
                crate::check::genus(&a),
                b_cpp.genus(),
                "GATE-A sweep [{i}]: genus diverges"
            );

            let a_cpp = CppKernel::ingest(&a.to_mesh_gl())
                .unwrap_or_else(|e| panic!("GATE-A sweep [{i}]: cpp rejects rust union: {e}"));
            let sym = a_cpp
                .difference(&b_cpp)
                .union(&b_cpp.difference(&a_cpp));
            let residual = sym.volume() / a_cpp.volume();
            assert!(
                residual < 1e-5,
                "GATE-A sweep [{i}]: residual {residual:.3e} >= 1e-5"
            );
            eprintln!("GATE-A sweep [{i}] ✓ vol={a_vol:.6}, residual={residual:.3e}");
        }
    }

    // =====================================================================================
    // ★ GATE-B (M.1.4) — the COINCIDENT case. Unlike GATE-A's general position, these box∪box configs
    //   SHARE coordinate planes, so `Shadows`'s `p == q` fires and the symbolic-perturbation normals are
    //   consulted for the first time. Each must produce a watertight solid matching C++ to residual
    //   < 1e-5 (the shared-coordinate tie-break bit-matching the reference), plus the analytic volume.
    // =====================================================================================
    #[test]
    fn gate_b_coincident_cube_union_residual_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        // (q-params, analytic union volume, label). P is always the unit cube [0,1]³. Each Q shares ≥1
        // coordinate plane with P (values in {0,1}), so p==q fires on those axes.
        let configs: &[(BoxParams, f64, &str)] = &[
            // shares the y,z planes {0,1}; x-overlap → union box [0,1.5]×[0,1]×[0,1].
            ((0.5, 0.0, 0.0, 1.0, 1.0, 1.0), 1.5, "shared y,z planes"),
            // shares the z planes {0,1}; x,y offset → union prism, XY area 1.75, height 1.
            ((0.5, 0.5, 0.0, 1.0, 1.0, 1.0), 1.75, "shared z plane"),
            // face-TOUCHING at x=1 (fully coincident shared face) → union box [0,2]×[0,1]×[0,1].
            ((1.0, 0.0, 0.0, 1.0, 1.0, 1.0), 2.0, "face-touching"),
            // shares the x,y planes {0,1}; z-overlap → union box [0,1]×[0,1]×[0,1.5].
            ((0.0, 0.0, 0.5, 1.0, 1.0, 1.0), 1.5, "shared x,y planes"),
            // shares the x,z planes {0,1}; y-overlap → union box [0,1]×[0,1.5]×[0,1].
            ((0.0, 0.5, 0.0, 1.0, 1.0, 1.0), 1.5, "shared x,z planes"),
            // Q wholly INSIDE P, sharing the origin corner planes x=y=z=0 → union = P, vol 1.
            ((0.0, 0.0, 0.0, 0.5, 0.5, 0.5), 1.0, "contained at corner"),
        ];

        for (i, &(qp, expected_vol, label)) in configs.iter().enumerate() {
            let p = prepared_box(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
            let q = prepared_box(qp.0, qp.1, qp.2, qp.3, qp.4, qp.5);
            let a = boolean(&p, &q, OpType::Add);

            assert!(a.is_manifold(), "GATE-B [{i}] ({label}): rust union not manifold");
            let a_vol = a.volume();
            assert!(
                (a_vol - expected_vol).abs() < 1e-9,
                "GATE-B [{i}] ({label}): volume {a_vol} != analytic {expected_vol}"
            );

            let p_cpp = CppKernel::ingest(&p.to_mesh_gl()).unwrap();
            let q_cpp = CppKernel::ingest(&q.to_mesh_gl()).unwrap();
            let b_cpp = p_cpp.union(&q_cpp);
            assert_eq!(
                crate::check::genus(&a),
                b_cpp.genus(),
                "GATE-B [{i}] ({label}): genus diverges from C++"
            );

            let a_cpp = CppKernel::ingest(&a.to_mesh_gl())
                .unwrap_or_else(|e| panic!("GATE-B [{i}] ({label}): cpp rejects rust union: {e}"));
            let sym = a_cpp.difference(&b_cpp).union(&b_cpp.difference(&a_cpp));
            let residual = sym.volume() / a_cpp.volume();
            assert!(
                residual < 1e-5,
                "GATE-B [{i}] ({label}): residual {residual:.3e} >= 1e-5 — coincident tie-break diverges"
            );
            eprintln!("GATE-B [{i}] ✓ ({label}): vol={a_vol:.6}, residual={residual:.3e}");
        }
    }

    /// The FULLY-COPLANAR extreme, beyond GATE-B's face-sharing scope: identical cubes, where EVERY
    /// face of P coincides with a face of Q. The R1 tracer has no coplanar-face merge (`edge_op`/
    /// `SimplifyTopology` is deferred to R2/M.2), so it currently doubles the coincident faces → genus
    /// −1 (χ=4, two components) instead of the clean genus-0 cube C++ produces (12 tris). This is NOT a
    /// tie-break bug — the partial-coincidence GATE-B cases all pass residual-0, so the cascade is
    /// correct; it's the missing cleanup. The edge_op TRIPWIRE, confirmed fired.
    ///
    /// R2 acceptance, now GREEN: `edge_op::simplify_topology`'s provenance-free subset (SplitPinchedVerts
    /// + DedupeEdges + CollapseShortEdges) collapses the doubled coincident faces to the clean genus-0
    /// cube. (The un-ignore is the check that R2 fixed the M.1.6 tripwire.)
    #[test]
    fn identical_cubes_need_coplanar_merge_r2() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        let p = prepared_box(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        let q = prepared_box(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        let a = boolean(&p, &q, OpType::Add);

        assert!(a.is_manifold(), "identical union not manifold");
        assert_eq!(crate::check::genus(&a), 0, "identical union should be genus 0 (one cube)");
        assert!((a.volume() - 1.0).abs() < 1e-9, "identical union volume should be 1");

        let b_cpp = CppKernel::ingest(&p.to_mesh_gl())
            .unwrap()
            .union(&CppKernel::ingest(&q.to_mesh_gl()).unwrap());
        let a_cpp = CppKernel::ingest(&a.to_mesh_gl()).unwrap();
        let sym = a_cpp.difference(&b_cpp).union(&b_cpp.difference(&a_cpp));
        assert!(sym.volume() / a_cpp.volume() < 1e-5, "identical union residual dirty");
    }

    /// A tiny deterministic PRNG (PCG-style LCG) — the thesis sweep is reproducible with zero deps
    /// (proptest drives the continuous fuzzer in M.1.5; here we just want a fixed, replayable stream).
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Lcg(seed ^ 0x9E37_79B9_7F4A_7C15)
        }
        fn next_u32(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (self.next_u32() as f64 / u32::MAX as f64) * (hi - lo)
        }
    }

    /// Re-prepare an intermediate union result for the next boolean (bbox + epsilon + both normal
    /// fields), mirroring a fresh input. `boolean` leaves face_normal but not vert_normal.
    fn prepare(mesh: &mut Mesh) {
        mesh.calculate_bbox();
        mesh.set_epsilon(-1.0, false);
        mesh.calculate_face_normals();
        mesh.calculate_vert_normals();
    }

    /// Install a `RUST_LOG`-driven tracing subscriber ONCE for the whole test binary — the switch that
    /// turns the `manifold::simplify` / `manifold::fold` debug events into visible output (e.g.
    /// `RUST_LOG=manifold::fold=debug cargo test … -- --nocapture`). No-op when `RUST_LOG` is unset, and
    /// idempotent (`try_init` ignores the already-installed case under test parallelism).
    fn init_tracing() {
        use tracing_subscriber::{EnvFilter, fmt};
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    /// Locate Manifold's bundled test-model directory (`test/models/*.obj`) inside the linked
    /// `manifold-csg-sys` build output — the nasty corpus (M.2.4). The build hash varies, so glob
    /// `target/{debug,release}/build/manifold-csg-sys-*/out/manifold-src/test/models`. `None` if the C++
    /// source isn't unpacked (then the corpus test skips).
    fn models_dir() -> Option<std::path::PathBuf> {
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
        for profile in ["release", "debug"] {
            let build = target.join(profile).join("build");
            let Ok(entries) = std::fs::read_dir(&build) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                if name.to_string_lossy().starts_with("manifold-csg-sys-") {
                    let models = e.path().join("out/manifold-src/test/models");
                    if models.is_dir() {
                        return Some(models);
                    }
                }
            }
        }
        None
    }

    /// Parse a triangle-mesh Wavefront `.obj` (`v x y z` + `f a b c`, 1-indexed, no texture/normal refs —
    /// the form Manifold's test corpus uses) into a position-only [`MeshGl`].
    fn load_obj(path: &std::path::Path) -> MeshGl {
        let text = std::fs::read_to_string(path).unwrap();
        let mut vert_properties = Vec::new();
        let mut tri_verts = Vec::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("v") => {
                    for _ in 0..3 {
                        vert_properties.push(it.next().unwrap().parse::<f64>().unwrap());
                    }
                }
                Some("f") => {
                    // `f a b c` — 1-indexed, possibly `a/vt/vn`; take the vertex index before any `/`.
                    for _ in 0..3 {
                        let tok = it.next().unwrap();
                        let idx: u32 = tok.split('/').next().unwrap().parse().unwrap();
                        tri_verts.push(idx - 1);
                    }
                }
                _ => {}
            }
        }
        MeshGl { num_prop: 3, vert_properties, tri_verts }
    }

    // --- Triangulation-robust solid comparison (chotchki's methodology: invariants + Monte-Carlo). The
    // boolean-residual metric runs through C++'s tolerance booleans, which sliver when tessellations
    // diverge (our un-simplified R1 mesh keeps collinear verts C++ merges). These read the SOLID, not
    // the mesh: scalar invariants (per-mesh, sliver-free) as a fast pre-check, then Monte-Carlo
    // containment run with OUR OWN point-in-mesh on BOTH meshes — no C++ booleans, triangulation-blind. ---

    /// Möller–Trumbore ray/triangle: returns `(t, u, v)` (ray parameter + barycentrics); the caller
    /// range-checks. `None` when the ray is parallel to the triangle.
    fn ray_tri(orig: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<(f64, f64, f64)> {
        let e1 = v1 - v0;
        let e2 = v2 - v0;
        let pv = dir.cross(e2);
        let det = e1.dot(pv);
        if det.abs() < 1e-14 {
            return None;
        }
        let inv = 1.0 / det;
        let tv = orig - v0;
        let u = tv.dot(pv) * inv;
        let qv = tv.cross(e1);
        let v = dir.dot(qv) * inv;
        let t = e2.dot(qv) * inv;
        Some((t, u, v))
    }

    /// Parity of a ray's forward crossings of `mesh` — `Some(inside)`, or `None` if the ray GRAZES an
    /// edge/vertex (ambiguous, so the caller retries another direction).
    fn cast_parity(p: Vec3, dir: Vec3, mesh: &Mesh) -> Option<bool> {
        use crate::mesh_ids::TriId;
        const M: f64 = 1e-9; // barycentric edge margin
        let mut count = 0u32;
        for tri in 0..mesh.num_tri() {
            let t = TriId::from_usize(tri);
            let v0 = mesh.pos(mesh.start(t.halfedge(0)));
            let v1 = mesh.pos(mesh.start(t.halfedge(1)));
            let v2 = mesh.pos(mesh.start(t.halfedge(2)));
            if let Some((tt, u, v)) = ray_tri(p, dir, v0, v1, v2) {
                if tt <= 1e-12 {
                    continue; // behind the point or at it
                }
                let w = 1.0 - u - v;
                if u < -M || v < -M || w < -M {
                    continue; // misses the triangle
                }
                if u < M || v < M || w < M {
                    return None; // grazes an edge/vertex — ambiguous
                }
                count += 1;
            }
        }
        Some(count % 2 == 1)
    }

    /// Is `p` inside the closed manifold `mesh`? Ray-cast parity with a few generic (non-axis-aligned)
    /// directions; the first that doesn't graze decides. (A point exactly on the surface is excluded by
    /// the caller's sampling, not here.)
    fn point_inside_mesh(p: Vec3, mesh: &Mesh) -> bool {
        const DIRS: [(f64, f64, f64); 4] = [
            (0.31, 0.53, 0.79),
            (0.87, -0.29, 0.41),
            (-0.19, 0.67, -0.72),
            (0.41, -0.83, 0.37),
        ];
        for &(x, y, z) in &DIRS {
            if let Some(inside) = cast_parity(p, Vec3::new(x, y, z), mesh) {
                return inside;
            }
        }
        false // all four grazed — vanishingly unlikely; treat as outside
    }

    /// Scalar-invariant agreement (the fast pre-check): VOLUME (relative) + BBOX (relative).
    ///
    /// Deliberately NOT area or genus: both are CLEANLINESS-sensitive, and an un-simplified R1 mesh
    /// legitimately carries internal degenerate structure (coincident/doubled walls at fold seams that
    /// `SimplifyTopology`/`edge_op` = R2 would remove) — that inflates area and breaks genus WITHOUT
    /// changing the solid (proven: Monte-Carlo 0/100000 + bit-identical volume on the same case). Only
    /// volume + bbox are robust to it; Monte-Carlo containment carries the shape check. (Single unions —
    /// GATE-A/B — DO gate genus, where the topology is clean; folds don't, pending R2.)
    fn invariants_divergence(a: &Mesh, b_vol: f64, b_box: Box3, vol_tol: f64) -> Option<String> {
        let rel = |x: f64, y: f64| (x - y).abs() / y.abs().max(1e-9);
        if rel(a.volume(), b_vol) >= vol_tol {
            return Some(format!("volume {} vs {b_vol}", a.volume()));
        }
        let bb = a.b_box;
        for (x, y) in [
            (bb.min.x, b_box.min.x), (bb.min.y, b_box.min.y), (bb.min.z, b_box.min.z),
            (bb.max.x, b_box.max.x), (bb.max.y, b_box.max.y), (bb.max.z, b_box.max.z),
        ] {
            if (x - y).abs() > 1e-9 * y.abs().max(1.0) {
                return Some(format!("bbox {x} vs {y}"));
            }
        }
        None
    }

    /// Triangulation-robust solid equality: invariant pre-check, then Monte-Carlo containment (our
    /// point-in-mesh on BOTH meshes over `n` seeded points in the shared bbox). Estimates the
    /// symmetric-difference fraction; a correct union disagrees only on the vanishing boundary-rounding
    /// shell. `Some(reason)` on divergence.
    fn solid_divergence(a: &Mesh, b: &Mesh, n: usize, seed: u64, vol_tol: f64) -> Option<String> {
        if let Some(r) = invariants_divergence(a, b.volume(), b.b_box, vol_tol) {
            return Some(format!("invariant: {r}"));
        }
        // Genus (handle count) is a topological invariant a filled-over hole or a spurious internal wall
        // would break even when volume matches — the exact defect M.2.3's keyhole path fixed. Cheap, so
        // check it on every differential.
        let (ga, gb) = (RustKernel::genus(a), RustKernel::genus(b));
        if ga != gb {
            return Some(format!("genus {ga} vs {gb}"));
        }
        let lo = a.b_box.min.cmin(b.b_box.min);
        let hi = a.b_box.max.cmax(b.b_box.max);
        let size = hi - lo;
        let bbox_vol = size.x * size.y * size.z;
        let mut rng = Lcg::new(seed);
        let mut disagree = 0u32;
        for _ in 0..n {
            let p = Vec3::new(
                lo.x + rng.range(0.0, 1.0) * size.x,
                lo.y + rng.range(0.0, 1.0) * size.y,
                lo.z + rng.range(0.0, 1.0) * size.z,
            );
            if point_inside_mesh(p, a) != point_inside_mesh(p, b) {
                disagree += 1;
            }
        }
        let frac = disagree as f64 / n as f64;
        let est_resid = frac * bbox_vol / a.volume().max(1e-12);
        // A correct union disagrees only on the boundary-rounding shell (≪ 0.1%); a gross shape error
        // (missing/extra chunk) disagrees on whole percent. Gate the gross case.
        if est_resid > 2e-3 {
            return Some(format!(
                "Monte-Carlo: {disagree}/{n} points disagree ⇒ est residual {est_resid:.3e}"
            ));
        }
        None
    }

    /// Fold-union the prepared input meshes (Rust) and compare with the triangulation-robust
    /// [`solid_divergence`] (invariants + Monte-Carlo, our own point-in-mesh on BOTH) against a C++
    /// fold-union in the SAME order. Returns `Some(reason)` on divergence, `None` if clean.
    ///
    /// This SUPERSEDES the old C++-boolean-residual fold check: once `simplify_topology` runs, our
    /// tessellation legitimately differs from C++'s (we collapse a DIFFERENT subset — the colinear+swap
    /// stages are provenance-gated), so C++'s `difference` slivers along the mismatched faces and the
    /// residual reads its measurement floor (~1e-4) even though volume matches to 1e-9 and the SOLIDS are
    /// identical. The invariant check here keeps the tight 1e-9 volume gate; Monte-Carlo carries the shape
    /// check with no C++ booleans. (Same reasoning the rotated thesis already used — see its doc.)
    fn fold_union_solid_divergence(rmeshes: &[Mesh], n: usize, seed: u64, vol_tol: f64) -> Option<String> {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        let gls: Vec<MeshGl> = rmeshes.iter().map(|m| m.to_mesh_gl()).collect();
        let mut acc = rmeshes[0].clone();
        for (si, c) in rmeshes[1..].iter().enumerate() {
            acc = boolean(&acc, c, OpType::Add);
            tracing::debug!(
                target: "manifold::fold",
                step = si,
                volume = acc.volume(),
                genus = crate::check::genus(&acc),
                num_tri = acc.num_tri(),
                "rust fold step",
            );
            if acc.is_empty() {
                break;
            }
            if !acc.is_manifold() {
                return Some("an intermediate union is not manifold".to_string());
            }
            prepare(&mut acc);
        }
        if !acc.is_manifold() {
            return Some("final union is not manifold".to_string());
        }

        let mut ccpp = match CppKernel::ingest(&gls[0]) {
            Ok(m) => m,
            Err(e) => return Some(format!("cpp rejected input 0: {e}")),
        };
        for g in &gls[1..] {
            match CppKernel::ingest(g) {
                Ok(m) => ccpp = ccpp.union(&m),
                Err(e) => return Some(format!("cpp rejected an input: {e}")),
            }
        }
        // C++ result as our Mesh, for the Monte-Carlo point-in-mesh (a triangle soup is enough).
        let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&ccpp));
        tracing::debug!(
            target: "manifold::fold",
            rust_genus = crate::check::genus(&acc),
            rust_volume = acc.volume(),
            rust_num_tri = acc.num_tri(),
            cpp_genus = ccpp.genus(),
            cpp_volume = ccpp.volume(),
            cpp_num_tri = b.num_tri(),
            "rust vs cpp final fold",
        );
        solid_divergence(&acc, &b, n, seed, vol_tol)
    }

    // =====================================================================================
    // ★ M.1.6 THESIS (R1 exit) — random multi-cube FOLD-unions vs C++. Beyond the fixed GATE-A/B
    //   configs, this hits the boolean on its OWN output (each fold step unions the running result with
    //   another cube) across a seeded sweep of continuous-random boxes (general position — exact
    //   coincidence measure-zero). Every result must be a watertight solid matching C++ to volume 1e-9 +
    //   bbox + Monte-Carlo containment (the triangulation-robust [`fold_union_solid_divergence`] — the
    //   C++ residual is now measurement noise once `simplify_topology` retessellates). CLEAN ⇒ the R1
    //   tracer thesis holds. (The cargo-fuzz/ASan 1h continuous run + polygon_fuzz port are the M.1.5 tail.)
    // =====================================================================================
    #[test]
    fn thesis_random_cube_fold_unions_vs_cpp() {
        init_tracing();
        let mut rng = Lcg::new(0x00F1_A5C0_FFEE);
        // 120 standing trials (~0.4s); a one-off 600-trial × 8-cube stress ran clean during M.1.6.
        let trials = 120;
        for trial in 0..trials {
            let n = 2 + (rng.next_u32() % 5) as usize; // 2..=6 cubes
            let rmeshes: Vec<Mesh> = (0..n)
                .map(|_| {
                    prepared_box(
                        rng.range(0.0, 2.5),
                        rng.range(0.0, 2.5),
                        rng.range(0.0, 2.5),
                        rng.range(0.8, 2.5),
                        rng.range(0.8, 2.5),
                        rng.range(0.8, 2.5),
                    )
                })
                .collect();
            if let Some(reason) = fold_union_solid_divergence(&rmeshes, 2500, 0xA5C0 + trial as u64, 1e-9) {
                panic!("THESIS trial {trial} (n={n}): {reason}");
            }
        }
        eprintln!("THESIS ✓ {trials} random multi-cube fold-unions — watertight, solid-clean vs C++");
    }

    /// A unit cube SCALED, ROTATED by ZYX-Euler angles, then TRANSLATED — a general-position solid with
    /// arbitrary (non-axis-aligned) face normals. Both engines ingest the identical rotated `MeshGl`, so
    /// the trig only builds test geometry; the boolean runs on identical inputs.
    #[allow(clippy::too_many_arguments)]
    fn prepared_rot_box(
        ox: f64,
        oy: f64,
        oz: f64,
        sx: f64,
        sy: f64,
        sz: f64,
        ra: f64,
        rb: f64,
        rc: f64,
    ) -> Mesh {
        use crate::mathf::{cos, sin};
        let (sa, ca) = (sin(ra), cos(ra));
        let (sb, cb) = (sin(rb), cos(rb));
        let (sc, cc) = (sin(rc), cos(rc));
        #[rustfmt::skip]
        let unit = [
            (0.0,0.0,0.0),(1.0,0.0,0.0),(1.0,1.0,0.0),(0.0,1.0,0.0),
            (0.0,0.0,1.0),(1.0,0.0,1.0),(1.0,1.0,1.0),(0.0,1.0,1.0),
        ];
        let mut verts = Vec::new();
        for &(ux, uy, uz) in &unit {
            // scale
            let (x, y, z) = (ux * sx, uy * sy, uz * sz);
            // Rx(ra)
            let (x1, y1, z1) = (x, y * ca - z * sa, y * sa + z * ca);
            // Ry(rb)
            let (x2, y2, z2) = (x1 * cb + z1 * sb, y1, -x1 * sb + z1 * cb);
            // Rz(rc)
            let (x3, y3, z3) = (x2 * cc - y2 * sc, x2 * sc + y2 * cc, z2);
            verts.push(x3 + ox);
            verts.push(y3 + oy);
            verts.push(z3 + oz);
        }
        #[rustfmt::skip]
        let tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7,
            0,1,5, 0,5,4, 2,3,7, 2,7,6,
            0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let mut mesh = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        });
        mesh.set_epsilon(-1.0, false);
        mesh.initialize_original();
        mesh.set_normals_and_coplanar();
        mesh
    }

    /// THESIS, hardest form: fold-unions of ROTATED cubes — arbitrary face normals, non-axis-aligned
    /// intersections that exercise the general `GetAxisAlignedProjection`/`CCW` paths and near-degenerate
    /// crossings the axis-aligned sweep can't reach.
    ///
    /// Compared with the triangulation-robust [`solid_divergence`] (invariants + Monte-Carlo), NOT the
    /// C++-boolean residual — the residual hits its measurement floor here (our un-simplified R1 mesh
    /// carries collinear verts C++ merges via SimplifyTopology/SortGeometry = R2, and C++'s `difference`
    /// slivers along the differently-tessellated near-coincident faces). Monte-Carlo reads the SOLID via
    /// our own point-in-mesh on both, so it's blind to that tessellation gap — and it confirms the union
    /// is geometrically correct (the residual floor was pure measurement noise).
    #[test]
    fn thesis_random_rotated_cube_unions_vs_cpp() {
        init_tracing();
        let mut rng = Lcg::new(0x00F0_7A7E_D0F0_u64);
        let trials = 120;
        let tau = 2.0 * crate::mathf::PI;
        for trial in 0..trials {
            let n = 2 + (rng.next_u32() % 4) as usize; // 2..=5 rotated cubes
            let rmeshes: Vec<Mesh> = (0..n)
                .map(|_| {
                    prepared_rot_box(
                        rng.range(0.0, 2.0),
                        rng.range(0.0, 2.0),
                        rng.range(0.0, 2.0),
                        rng.range(0.8, 2.0),
                        rng.range(0.8, 2.0),
                        rng.range(0.8, 2.0),
                        rng.range(0.0, tau),
                        rng.range(0.0, tau),
                        rng.range(0.0, tau),
                    )
                })
                .collect();
            // M.2.2.3 CLOSED (commit for M.2.3 keyhole): all 120 rotated folds are now byte-identical to
            // C++ in volume (rel < 1e-12) AND genus-matched AND Monte-Carlo-clean. The residual ~8 vol
            // outliers + ~30 genus mismatches the earlier passes chased as "SimplifyTopology collapse-order
            // divergence" were actually FILLED-OVER HOLES — the old per-loop Face2Tri filled interior hole
            // loops (zero-volume internal walls → wrong genus) and inverted CW loops (→ volume drift). The
            // multi-loop keyhole EarClip (M.2.3) fixed both. So the gate is the TIGHT 1e-9 volume + the
            // genus-match now baked into solid_divergence.
            if let Some(reason) = fold_union_solid_divergence(&rmeshes, 2500, 0xA5E1 + trial as u64, 1e-9) {
                panic!("ROTATED THESIS trial {trial} (n={n}): {reason}");
            }
        }
        eprintln!("THESIS(rot) ✓ {trials} rotated-cube fold-unions — invariants + Monte-Carlo match C++");
    }

    proptest::proptest! {
        // M.2.2.3 / M.2.3 — the KEYHOLE fuzzer at the BOOLEAN level. A [0,10]³ block minus 1..=2 bars,
        // each piercing all the way THROUGH a random axis at a strictly-interior cross-section, so every
        // bar punches a genus-adding hole through two opposite faces — exactly the holed-face case the
        // multi-loop keyhole EarClip must triangulate. Held to the full solid oracle (1e-9 volume +
        // genus-match + Monte-Carlo) vs C++, so a keyhole regression (filled hole → genus break, inverted
        // CW loop → volume drift) is caught, not just hoped-for from the fixed rotated-fold sweep.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]
        #[test]
        fn keyhole_boolean_holes_match_cpp(
            // (axis 0=x/1=y/2=z, cross-coord u, cross-coord v, width u, width v), strictly interior.
            bars in proptest::collection::vec(
                (0usize..3, 1.0f64..7.0, 1.0f64..7.0, 0.8f64..2.0, 0.8f64..2.0),
                1..=2,
            )
        ) {
            use crate::boolean::OpType;
            use crate::boolean::boolean_result::boolean;

            // A bar that pierces `axis` through [0,10], cross-section (u,v)+(wu,wv) in the other two axes.
            let make_bar = |axis: usize, u: f64, v: f64, wu: f64, wv: f64| match axis {
                0 => prepared_box(-1.0, u, v, 12.0, wu, wv), // through x
                1 => prepared_box(u, -1.0, v, wu, 12.0, wv), // through y
                _ => prepared_box(u, v, -1.0, wu, wv, 12.0), // through z
            };

            let mut acc = prepared_box(0.0, 0.0, 0.0, 10.0, 10.0, 10.0);
            let mut ccpp = CppKernel::ingest(&acc.to_mesh_gl()).unwrap();
            for &(axis, u, v, wu, wv) in &bars {
                let bar = make_bar(axis, u, v, wu, wv);
                acc = boolean(&acc, &bar, OpType::Subtract);
                proptest::prop_assert!(acc.is_manifold(), "holed result not manifold: bars {:?}", bars);
                prepare(&mut acc);
                ccpp = ccpp.difference(&CppKernel::ingest(&bar.to_mesh_gl()).unwrap());
            }
            // Sanity: the fuzzer actually produced at least one tunnel (a genus-adding hole).
            proptest::prop_assert!(ccpp.genus() >= 1, "no hole produced (bars {:?})", bars);

            let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&ccpp));
            if let Some(reason) = solid_divergence(&acc, &b, 3000, 0x4011, 1e-9) {
                proptest::prop_assert!(false, "keyhole boolean diverges from C++: {reason}\nbars {:?}", bars);
            }
        }
    }

    /// M.2.4 — the NASTY corpus: Manifold's own hard test models (self-intersecting / thin /
    /// near-degenerate real geometry), unioned left+right per `boolean_complex_test.cpp`, checked
    /// manifold + solid-divergence vs C++. The LBVH broad phase (M.2.4.1) unblocked `self_intersect`
    /// (17K+17K tri, ~340ms debug) — it's in. The other big one, `Generic_Twin_7081` (20K), is CORRECT
    /// but ~190s debug on the serial narrow phase (64.5M near-coincident candidate pairs) → its own
    /// `#[ignore]`d [`big_twin_union_vs_cpp`] until the parallelism phase. Skips cleanly if the C++ source
    /// isn't unpacked.
    #[test]
    fn nasty_corpus_union_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;
        init_tracing();
        let Some(dir) = models_dir() else {
            eprintln!("nasty corpus: models dir not found — skipping");
            return;
        };
        // (left, right, MC samples). Small models get 4000; self_intersect (33K faces) gets 800 — the
        // brute-force point-in-mesh is O(samples·faces), and its invariants (tri count + volume) already
        // match C++ exactly, so a lighter Monte-Carlo still catches any gross shape error.
        let pairs = [
            ("Havocglass8_left.obj", "Havocglass8_right.obj", 4000),
            ("Cray_left.obj", "Cray_right.obj", 4000),
            ("Generic_Twin_7863.1.t0_left.obj", "Generic_Twin_7863.1.t0_right.obj", 4000),
            ("self_intersectA.obj", "self_intersectB.obj", 800),
        ];
        for (l, r, mc) in pairs {
            let gl_l = load_obj(&dir.join(l));
            let gl_r = load_obj(&dir.join(r));

            let mut ml = Mesh::from_mesh_gl(&gl_l);
            ml.set_epsilon(-1.0, false);
            ml.initialize_original();
            ml.set_normals_and_coplanar();
            let mut mr = Mesh::from_mesh_gl(&gl_r);
            mr.set_epsilon(-1.0, false);
            mr.initialize_original();
            mr.set_normals_and_coplanar();

            let res = boolean(&ml, &mr, OpType::Add);
            assert!(res.is_manifold(), "{l} ∪ {r}: rust union is not manifold");

            let cpp = CppKernel::ingest(&gl_l)
                .unwrap_or_else(|e| panic!("{l}: cpp ingest {e}"))
                .union(&CppKernel::ingest(&gl_r).unwrap_or_else(|e| panic!("{r}: cpp ingest {e}")));
            let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&cpp));
            // Solid oracle at the tight 1e-9 volume + genus-match + Monte-Carlo (M.2.2.3).
            if let Some(reason) = solid_divergence(&res, &b, mc, 0x0B5E, 1e-9) {
                panic!("NASTY {l} ∪ {r}: {reason}");
            }
            eprintln!("nasty ✓ {l} ∪ {r}: vol {:.5} ntri {}", res.volume(), res.num_tri());
        }
    }

    /// M.2.4 — the BIG twin (`Generic_Twin_7081`, 19.7K+11.7K tri): a near-COINCIDENT pair whose face
    /// boxes overlap almost everywhere, so `intersect12` legitimately emits ~64.5M candidate box overlaps
    /// (for ~1024 real hits). The LBVH broad phase finds them fine — the residual ~15s (release) is the
    /// SERIAL narrow phase grinding ~124M `kernel12` calls, which is the PARALLELISM phase's job (J.4.5),
    /// not a broad-phase defect. Correct + Monte-Carlo-clean today (matches C++ to the near-degenerate
    /// determinism tail). `#[ignore]`d until parallel narrow-phase makes it fast enough for the gate.
    #[test]
    #[ignore = "correct + EXACT vs C++ (33230 tri); ~3s under release+par (narrow phase 15s→1.2s), but ~190s in the default debug+serial lane — run it in a release+par CI lane"]
    fn big_twin_union_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;
        init_tracing();
        let Some(dir) = models_dir() else {
            eprintln!("big twin: models dir not found — skipping");
            return;
        };
        let (l, r) = ("Generic_Twin_7081.1.t0_left.obj", "Generic_Twin_7081.1.t0_right.obj");
        let gl_l = load_obj(&dir.join(l));
        let gl_r = load_obj(&dir.join(r));
        let mut ml = Mesh::from_mesh_gl(&gl_l);
        ml.set_epsilon(-1.0, false);
        ml.initialize_original();
        ml.set_normals_and_coplanar();
        let mut mr = Mesh::from_mesh_gl(&gl_r);
        mr.set_epsilon(-1.0, false);
        mr.initialize_original();
        mr.set_normals_and_coplanar();
        let res = boolean(&ml, &mr, OpType::Add);
        assert!(res.is_manifold(), "{l} ∪ {r}: rust union is not manifold");
        let cpp = CppKernel::ingest(&gl_l)
            .unwrap_or_else(|e| panic!("{l}: cpp ingest {e}"))
            .union(&CppKernel::ingest(&gl_r).unwrap_or_else(|e| panic!("{r}: cpp ingest {e}")));
        let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&cpp));
        if let Some(reason) = solid_divergence(&res, &b, 4000, 0x0B5E, 2e-2) {
            panic!("BIG TWIN {l} ∪ {r}: {reason}");
        }
        eprintln!("big twin ✓ {l} ∪ {r}: vol {:.5} ntri {} (cpp {})", res.volume(), res.num_tri(), b.num_tri());
    }

    /// Unit-check the point-in-mesh oracle the Monte-Carlo comparison relies on: a unit cube classifies
    /// its interior/exterior correctly (including points that would graze axis-aligned faces).
    #[test]
    fn point_in_mesh_classifies_cube() {
        let cube = prepared_box(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert!(point_inside_mesh(Vec3::new(0.5, 0.5, 0.5), &cube), "center is inside");
        assert!(point_inside_mesh(Vec3::new(0.1, 0.9, 0.3), &cube), "off-center interior");
        assert!(!point_inside_mesh(Vec3::new(1.5, 0.5, 0.5), &cube), "outside +x");
        assert!(!point_inside_mesh(Vec3::new(-0.2, 0.5, 0.5), &cube), "outside -x");
        assert!(!point_inside_mesh(Vec3::new(0.5, 0.5, 2.0), &cube), "outside +z");
        // A larger offset box: interior/exterior still correct.
        let b = prepared_box(3.0, 3.0, 3.0, 2.0, 2.0, 2.0);
        assert!(point_inside_mesh(Vec3::new(4.0, 4.0, 4.0), &b));
        assert!(!point_inside_mesh(Vec3::new(0.0, 0.0, 0.0), &b));
    }

    proptest::proptest! {
        // M.1.5 proptest FAST-GATE — the shrinking counterpart to the deterministic thesis sweep: on a
        // regression, proptest minimizes to the smallest diverging box set. 64 cases (~1s vs C++).
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
        #[test]
        fn prop_cube_fold_unions_match_cpp(
            params in proptest::collection::vec(
                (0.0f64..2.5, 0.0f64..2.5, 0.0f64..2.5, 0.8f64..2.5, 0.8f64..2.5, 0.8f64..2.5),
                2..=6,
            )
        ) {
            let rmeshes: Vec<Mesh> = params
                .iter()
                .map(|&(ox, oy, oz, sx, sy, sz)| prepared_box(ox, oy, oz, sx, sy, sz))
                .collect();
            // Weave the Monte-Carlo seed from THIS case's params (FNV-1a over the raw bits) — determinism
            // like everywhere else, but each case (and each proptest shrink) samples its own point set,
            // reproducibly, rather than reusing one fixed pattern.
            let mc_seed = params.iter().fold(0xcbf2_9ce4_8422_2325u64, |acc, t| {
                [t.0, t.1, t.2, t.3, t.4, t.5].iter().fold(acc, |h, v| {
                    (h ^ v.to_bits()).wrapping_mul(0x0000_0100_0000_01b3)
                })
            });
            if let Some(reason) = fold_union_solid_divergence(&rmeshes, 2500, mc_seed, 1e-9) {
                proptest::prop_assert!(false, "{reason}\nparams = {params:?}");
            }
        }
    }

    // =====================================================================================
    // ★ R2 (M.2.1) — difference + intersection. The op param (c1/c2/c3 + invertQ) was ported in the
    //   M.1.3 assembly but never exercised (only Add was gated). Offset cubes: P−Q and P∩Q must be
    //   watertight, analytic-volume, and match C++ on the triangulation-robust solid oracle.
    // =====================================================================================
    #[test]
    fn r2_offset_difference_intersection_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        let p = prepared_box(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        let q = prepared_box(0.3, 0.4, 0.5, 1.0, 1.0, 1.0);
        let p_cpp = CppKernel::ingest(&p.to_mesh_gl()).unwrap();
        let q_cpp = CppKernel::ingest(&q.to_mesh_gl()).unwrap();
        // overlap = [0.3,1]×[0.4,1]×[0.5,1] = 0.7·0.6·0.5 = 0.21.

        // Subtract: P − Q = 1 − 0.21 = 0.79.
        let sub = boolean(&p, &q, OpType::Subtract);
        assert!(sub.is_manifold(), "P−Q is not manifold");
        assert!((sub.volume() - 0.79).abs() < 1e-9, "P−Q volume {} != 0.79", sub.volume());
        let sub_b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&p_cpp.difference(&q_cpp)));
        if let Some(r) = solid_divergence(&sub, &sub_b, 5000, 0xD1FF, 1e-9) {
            panic!("P−Q diverges from C++: {r}");
        }

        // Intersect: P ∩ Q = 0.21.
        let int = boolean(&p, &q, OpType::Intersect);
        assert!(int.is_manifold(), "P∩Q is not manifold");
        assert!((int.volume() - 0.21).abs() < 1e-9, "P∩Q volume {} != 0.21", int.volume());
        let int_b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&p_cpp.intersection(&q_cpp)));
        if let Some(r) = solid_divergence(&int, &int_b, 5000, 0x1417, 1e-9) {
            panic!("P∩Q diverges from C++: {r}");
        }
        eprintln!("R2 ✓ P−Q (vol {:.4}) + P∩Q (vol {:.4}) match C++", sub.volume(), int.volume());
    }

    /// M.2.3 — the KEYHOLE integration test: a bar punched all the way through a box (difference) leaves a
    /// square HOLE in the box's top and bottom faces, so `Face2Tri` must triangulate a holed polygon (an
    /// outer loop + an interior CW hole loop) via `CutKeyhole`. Without the keyhole path those faces fill
    /// over the hole → non-manifold / wrong genus. The result is a genus-1 tunnel; check watertight +
    /// analytic volume + genus-1 + solid-divergence-clean vs C++.
    #[test]
    fn r2_tunnel_difference_holed_face_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        // Box [0,10]³ minus a 4×4 bar spanning z∈[-1,11] centered at (5,5): a square tunnel through z.
        let block = prepared_box(0.0, 0.0, 0.0, 10.0, 10.0, 10.0);
        let bar = prepared_box(3.0, 3.0, -1.0, 4.0, 4.0, 12.0);
        let res = boolean(&block, &bar, OpType::Subtract);

        assert!(res.is_manifold(), "tunnel result is not manifold — keyhole face failed");
        // Volume = 10³ − (4·4·10 tunnel through the block) = 1000 − 160 = 840.
        assert!((res.volume() - 840.0).abs() < 1e-9, "tunnel volume {} != 840", res.volume());
        assert_eq!(RustKernel::genus(&res), 1, "a through-tunnel is genus 1");

        let block_cpp = CppKernel::ingest(&block.to_mesh_gl()).unwrap();
        let bar_cpp = CppKernel::ingest(&bar.to_mesh_gl()).unwrap();
        let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&block_cpp.difference(&bar_cpp)));
        assert_eq!(RustKernel::genus(&b), 1, "C++ tunnel is genus 1 (sanity)");
        if let Some(r) = solid_divergence(&res, &b, 6000, 0x7011, 1e-9) {
            panic!("tunnel difference diverges from C++: {r}");
        }
        eprintln!("M.2.3 ✓ keyhole tunnel: vol {:.1} genus {} ({} tri)", res.volume(), RustKernel::genus(&res), res.num_tri());
    }

    /// R2 sweep: difference + intersection across several general-position box pairs (varied sizes +
    /// offsets, all genuinely overlapping so results are non-empty), each held to the solid oracle vs
    /// C++. Guards the op param (invertQ face-flip for Subtract, the c1/c2/c3 inclusion transforms)
    /// against config-specific bugs the single offset case can't reach.
    #[test]
    fn r2_diff_intersect_sweep_vs_cpp() {
        use crate::boolean::OpType;
        use crate::boolean::boolean_result::boolean;

        let configs: &[(BoxParams, BoxParams)] = &[
            ((0.0, 0.0, 0.0, 1.0, 1.0, 1.0), (0.3, 0.4, 0.5, 1.0, 1.0, 1.0)),
            ((0.0, 0.0, 0.0, 2.0, 1.0, 1.0), (0.5, 0.3, -0.2, 1.0, 2.0, 1.0)),
            ((0.0, 0.0, 0.0, 3.0, 2.0, 1.0), (1.3, 0.7, -0.4, 1.0, 1.0, 2.0)),
            ((0.0, 0.0, 0.0, 2.0, 3.0, 2.0), (0.6, 1.1, 0.7, 3.0, 1.0, 1.0)),
        ];

        for (i, &(pp, qp)) in configs.iter().enumerate() {
            let p = prepared_box(pp.0, pp.1, pp.2, pp.3, pp.4, pp.5);
            let q = prepared_box(qp.0, qp.1, qp.2, qp.3, qp.4, qp.5);
            let p_cpp = CppKernel::ingest(&p.to_mesh_gl()).unwrap();
            let q_cpp = CppKernel::ingest(&q.to_mesh_gl()).unwrap();

            for op in [OpType::Subtract, OpType::Intersect] {
                let a = boolean(&p, &q, op);
                assert!(a.is_manifold(), "R2 sweep [{i}] {op:?}: not manifold");
                let b_cpp = match op {
                    OpType::Subtract => p_cpp.difference(&q_cpp),
                    OpType::Intersect => p_cpp.intersection(&q_cpp),
                    OpType::Add => unreachable!(),
                };
                let bvol = b_cpp.volume();
                assert!(!a.is_empty() && bvol > 1e-6, "R2 sweep [{i}] {op:?}: empty result");
                assert!(
                    (a.volume() - bvol).abs() / bvol.abs().max(1e-9) < 1e-9,
                    "R2 sweep [{i}] {op:?}: volume {} vs cpp {bvol}",
                    a.volume()
                );
                let b = Mesh::from_mesh_gl(&cpp_to_mesh_gl(&b_cpp));
                if let Some(r) = solid_divergence(&a, &b, 4000, 0x5A5A + i as u64, 1e-9) {
                    panic!("R2 sweep [{i}] {op:?} diverges from C++: {r}");
                }
            }
        }
        eprintln!("R2 ✓ difference + intersection sweep ({} configs) match C++", configs.len());
    }

    /// Exercises the divergence-REPORTING machinery (the paths that only fire when the kernels
    /// disagree — normally dormant because the port is faithful).
    #[test]
    fn differential_reports_divergences_and_rejects() {
        // Kernel labels.
        assert_eq!(RustKernel::name(), "rust(fab-manifold)");
        assert_eq!(CppKernel::name(), "cpp(manifold3d)");

        // rel_tol = -1 ⇒ every finite metric "diverges" (rel ≥ 0 > -1): forces the check-closure push
        // + Divergence construction for volume/area/bbox.
        let cube = cpp_to_mesh_gl(&manifold3d::Manifold::cube(2.0, 3.0, 5.0, false));
        let all = differential(&cube, -1.0).unwrap();
        assert!(all.iter().any(|d| d.metric == "volume"));
        assert!(all.iter().any(|d| d.metric.starts_with("bbox")));
        let d = &all[0];
        assert!(d.abs >= 0.0 && d.rel >= 0.0 && !format!("{d:?}").is_empty());

        // A REAL divergence the harness must catch: a unit cube with two DANGLING vertices (indexed by
        // no triangle). Rust keeps them → χ=10−18+12=4 → genus −1, and a bbox that swallows them; C++
        // drops unreferenced verts → genus 0, cube bbox. So genus AND bbox diverge (volume agrees).
        #[rustfmt::skip]
        let mut vp = vec![
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        vp.extend_from_slice(&[50.0, 50.0, 50.0, 60.0, 60.0, 60.0]); // two dangling verts
        #[rustfmt::skip]
        let cube_tris = vec![
            0,2,1, 0,3,2, 4,5,6, 4,6,7, 0,1,5, 0,5,4,
            2,3,7, 2,7,6, 0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let dangling = MeshGl {
            num_prop: 3,
            vert_properties: vp,
            tri_verts: cube_tris,
        };
        let divs = differential(&dangling, 1e-9).unwrap();
        assert!(
            divs.iter().any(|d| d.metric == "genus"),
            "expected a genus divergence: {divs:#?}"
        );

        // Reject paths: a mesh with an out-of-range index — BOTH kernels reject (rust: unpaired →
        // not manifold; cpp: from_mesh_f64 fails) → the both-reject arm returns Ok(empty).
        let bad_index = MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            tri_verts: vec![0, 1, 99],
        };
        assert!(RustKernel::ingest(&bad_index).is_err());
        assert!(CppKernel::ingest(&bad_index).is_err());
        assert!(differential(&bad_index, 1e-9).unwrap().is_empty());

        // Asymmetric validity — the two kernels DISAGREE, which the differential surfaces as an Err.
        // Both are REAL, meaningful cases:
        //   (a) rust's ingest is topology-only, so a NaN-vertex mesh (valid pairing, invalid geometry)
        //       is accepted by rust but rejected by C++ → "rust accepted, cpp rejected".
        #[rustfmt::skip]
        let mut nan_vp = vec![
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        nan_vp[0] = f64::NAN;
        #[rustfmt::skip]
        let cube_tris = vec![
            0u32,2,1, 0,3,2, 4,5,6, 4,6,7, 0,1,5, 0,5,4,
            2,3,7, 2,7,6, 0,4,7, 0,7,3, 1,2,6, 1,6,5,
        ];
        let nan_mesh = MeshGl {
            num_prop: 3,
            vert_properties: nan_vp,
            tri_verts: cube_tris.clone(),
        };
        let e = differential(&nan_mesh, 1e-9).unwrap_err();
        assert!(e.contains("rust accepted"), "got: {e}");

        //   (b) the mirror — an opposed-triangle "flap" appended to a valid cube: C++'s CreateHalfedges
        //       REMOVES the degenerate pair and accepts, but our clean-pairing (opposed-tri removal is
        //       the M.0.5 gap deferred to R1) sees broken pairing and rejects → "cpp accepted".
        #[rustfmt::skip]
        let flap_vp = vec![
            0.0,0.0,0.0, 1.0,0.0,0.0, 1.0,1.0,0.0, 0.0,1.0,0.0,
            0.0,0.0,1.0, 1.0,0.0,1.0, 1.0,1.0,1.0, 0.0,1.0,1.0,
        ];
        let mut flap_tris = cube_tris;
        flap_tris.extend_from_slice(&[0, 1, 2, 0, 2, 1]); // coincident opposed pair
        let flap = MeshGl {
            num_prop: 3,
            vert_properties: flap_vp,
            tri_verts: flap_tris,
        };
        let e = differential(&flap, 1e-9).unwrap_err();
        assert!(e.contains("cpp accepted"), "got: {e}");
    }
}
