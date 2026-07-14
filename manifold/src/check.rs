//! Oracle B — the manifold-invariant checker (a port of Manifold's `test.h` / `intermediateChecks`).
//!
//! REFERENCE-FREE structural gates: unlike the [`crate::oracle`] differential (which needs the C++
//! kernel), these are self-contained, so they SURVIVE the oracle's removal at R.X and stay the
//! permanent correctness net. When `KernelParams::intermediate_checks` is on, the boolean core (R1+)
//! runs [`strictly`] after EVERY internal op — Manifold's trick that catches a corruption at the op
//! that caused it, not three ops later. Off in release.
//!
//! The circularity worry ("is `volume`/`genus` trustworthy enough to assert on?") is broken elsewhere:
//! Gate K.0 (M.0.6) calibrates them against the C++ oracle on identical buffers first. Here we assert
//! only what's topology-intrinsic (parity, finiteness, pairing).
//!
//! COVERAGE (M.0.4) — grows with the crate, gaps LOUD-deferred (never silently pass):
//! - `is_manifold` (via [`crate::mesh::Mesh::is_manifold`]), `finite`, `euler` parity, `genus`: NOW.
//! - self-intersection (`strictly`'s geometric half): needs the collider/BVH — R2.
//! - `related` (property/color provenance survives a boolean): needs booleans + prop tracking — R1+.

use crate::mesh::Mesh;

/// Test/fuzz-time parameters — Manifold's `ManifoldParams`. `intermediate_checks` on ⇒ the boolean
/// core validates the mesh after each internal op (test/fuzz builds); off in release.
#[derive(Clone, Copy, Debug, Default)]
pub struct KernelParams {
    /// Run [`strictly`] after every internal op (Manifold's `intermediateChecks`).
    pub intermediate_checks: bool,
}

/// Every vertex position is finite — no NaN/inf (Manifold's `la::isfinite` vertex gate).
pub fn finite(mesh: &Mesh) -> bool {
    mesh.vert_pos.iter().all(|p| p.is_finite())
}

/// Euler characteristic `χ = V − E + F` (Manifold `NumVert() − NumEdge() + NumTri()`, `NumEdge =
/// halfedge/2`). Only meaningful for a manifold mesh.
pub fn euler_characteristic(mesh: &Mesh) -> i32 {
    mesh.num_vert() as i32 - mesh.num_edge() as i32 + mesh.num_tri() as i32
}

/// Genus, `1 − χ/2` with integer division — Manifold's `Genus()`. This is the single-component
/// formula Manifold itself returns; a multi-component-aware genus needs component counting (R3).
pub fn genus(mesh: &Mesh) -> i32 {
    1 - euler_characteristic(mesh) / 2
}

/// `χ` is even — a closed orientable 2-manifold invariant (`χ = 2·(components − total genus)`), so it's
/// a reference-free consistency gate on the topology. Empty mesh passes vacuously.
pub fn euler_consistent(mesh: &Mesh) -> bool {
    mesh.is_empty() || euler_characteristic(mesh) % 2 == 0
}

/// The composite reference-free gate (Manifold `test.h` `strictly` / the intermediate check, minus the
/// parts this crate can't do yet). Returns the FIRST failing invariant, so a fuzzer trophy names what
/// broke.
///
/// COVERED: manifold topology, finite verts, Euler parity. DEFERRED (never silently pass): the
/// self-intersection half (collider — R2), and `related` provenance (booleans — R1+).
pub fn strictly(mesh: &Mesh) -> Result<(), String> {
    if !mesh.is_manifold() {
        return Err("not manifold (half-edge pairing inconsistent)".to_string());
    }
    if !finite(mesh) {
        return Err("non-finite vertex position".to_string());
    }
    if !euler_consistent(mesh) {
        return Err(format!(
            "odd Euler characteristic χ={} (topology corrupt)",
            euler_characteristic(mesh)
        ));
    }
    Ok(())
}

/// The hook the boolean core (R1+) calls after each internal op. No-op unless
/// `params.intermediate_checks` is set; when set, PANICS with the failing invariant — Manifold's
/// `intermediateChecks`, catching corruption at its source op.
pub fn intermediate_check(mesh: &Mesh, params: KernelParams) {
    if params.intermediate_checks
        && let Err(e) = strictly(mesh)
    {
        panic!("intermediate check failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshGl;

    fn unit_cube() -> Mesh {
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
        Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: verts,
            tri_verts: tris,
        })
    }

    #[test]
    fn cube_invariants() {
        let m = unit_cube();
        assert!(finite(&m));
        // sphere-topology cube: V−E+F = 8−18+12 = 2, genus 0.
        assert_eq!(euler_characteristic(&m), 2);
        assert_eq!(genus(&m), 0);
        assert!(euler_consistent(&m));
        assert!(strictly(&m).is_ok());
    }

    #[test]
    fn empty_mesh_passes() {
        let m = Mesh::default();
        assert!(finite(&m));
        assert!(euler_consistent(&m));
        assert!(strictly(&m).is_ok());
    }

    #[test]
    fn nan_vertex_fails_finite_and_strictly() {
        let mut m = unit_cube();
        m.vert_pos[3].x = f64::NAN;
        assert!(!finite(&m));
        assert_eq!(strictly(&m).unwrap_err(), "non-finite vertex position");
    }

    #[test]
    fn non_manifold_fails_strictly() {
        // single triangle → open, unpaired edges.
        let m = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            tri_verts: vec![0, 1, 2],
        });
        assert!(strictly(&m).is_err());
    }

    #[test]
    fn intermediate_check_gated_by_flag() {
        let bad = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            tri_verts: vec![0, 1, 2],
        });
        // flag off: no-op even on a broken mesh.
        intermediate_check(&bad, KernelParams::default());
    }

    #[test]
    #[should_panic(expected = "intermediate check failed")]
    fn intermediate_check_panics_when_on() {
        let bad = Mesh::from_mesh_gl(&MeshGl {
            num_prop: 3,
            vert_properties: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            tri_verts: vec![0, 1, 2],
        });
        intermediate_check(
            &bad,
            KernelParams {
                intermediate_checks: true,
            },
        );
    }
}
