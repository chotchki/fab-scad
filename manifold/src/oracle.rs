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
        mesh.calculate_face_normals();
        mesh.calculate_vert_normals();
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
        mesh.calculate_face_normals();
        mesh.calculate_vert_normals();
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
        type BoxParams = (f64, f64, f64, f64, f64, f64);
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
