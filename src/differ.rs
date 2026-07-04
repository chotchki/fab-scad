//! The differential metric (G.3.7) — scad-rs's mesh vs the OpenSCAD oracle, across tiers of
//! increasing strictness, to find the STRICTEST tier that holds per model class.
//!
//! Tiers (loose → strict):
//!   1. quantized vertex-MULTISET — do the vertex SETS agree on a grid? Order-independent (the
//!      oracle reindexes through Manifold), and the grid absorbs the export's ~1e-6 quantization.
//!   2. bulk metrics — volume + surface area (relative tolerance), genus (exact). Size + topology,
//!      triangulation-independent.
//!   3. boolean residual — `vol((A−B) ∪ (B−A)) / vol(A)`, the actual solid symmetric difference:
//!      ~0 means "the SAME solid" no matter how each was triangulated. The strongest tier.
//!
//! Both engines' meshes become a `kernel::Solid` (Manifold) — this IS the downstream
//! `Mesh → Solid::from_indexed` hand-off the G.3.5 architecture deferred to here.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::kernel::Solid;
use crate::oracle;

/// One source compared across every tier.
#[derive(Debug, Clone)]
pub struct Comparison {
    pub scad_verts: usize,
    pub oracle_verts: usize,
    pub scad_tris: usize,
    pub oracle_tris: usize,
    /// Smallest grid `eps` (from [`EPS_LADDER`]) at which the vertex multisets match; None if none did.
    pub vertex_match_eps: Option<f64>,
    pub volume_rel_err: f64,
    pub area_rel_err: f64,
    pub genus_scad: i32,
    pub genus_oracle: i32,
    /// `vol((A−B) ∪ (B−A)) / vol(A)` — the symmetric-difference ratio (0 = identical solids).
    pub boolean_residual: f64,
}

/// Grid epsilons probed for the vertex-multiset tier (strict → loose). The smallest that matches is
/// the tier's achieved strictness.
pub const EPS_LADDER: [f64; 6] = [1e-9, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3];

/// Compare scad-rs and the oracle on `source`. `timeout` bounds the oracle render.
///
/// # Errors
/// Fails if either engine errors, or a mesh can't be realized as a manifold `Solid`.
pub fn compare(source: &str, timeout: Duration) -> Result<Comparison> {
    // scad-rs engine → Solid.
    let mesh = fab_lang::evaluate(source).context("scad-rs evaluate")?;
    let scad_solid =
        Solid::from_indexed(&mesh.verts, &mesh.tris).context("scad-rs mesh → Solid")?;

    // oracle engine → Solid.
    let run = oracle::run(source, timeout).context("oracle run")?;
    let oracle_tris = run.mesh.tris();
    let oracle_solid =
        Solid::from_indexed(&run.mesh.verts, &oracle_tris).context("oracle mesh → Solid")?;

    // Tier 1: the smallest grid at which the vertex multisets agree.
    let vertex_match_eps = EPS_LADDER
        .iter()
        .copied()
        .find(|&eps| vertex_multiset_matches(&mesh.verts, &run.mesh.verts, eps));

    // Tier 2: bulk metrics.
    let volume_rel_err = rel_err(scad_solid.volume(), oracle_solid.volume());
    let area_rel_err = rel_err(scad_solid.surface_area(), oracle_solid.surface_area());

    // Tier 3: boolean residual — the symmetric difference's volume, normalized.
    let sym_diff = scad_solid
        .difference(&oracle_solid)
        .union(&oracle_solid.difference(&scad_solid));
    let ref_vol = scad_solid.volume().abs().max(f64::MIN_POSITIVE);
    let boolean_residual = sym_diff.volume() / ref_vol;

    Ok(Comparison {
        scad_verts: mesh.vert_count(),
        oracle_verts: run.mesh.vert_count(),
        scad_tris: mesh.tri_count(),
        oracle_tris: oracle_tris.len(),
        vertex_match_eps,
        volume_rel_err,
        area_rel_err,
        genus_scad: scad_solid.genus(),
        genus_oracle: oracle_solid.genus(),
        boolean_residual,
    })
}

/// Relative error `|a−b| / max(|a|,|b|)`.
fn rel_err(a: f64, b: f64) -> f64 {
    let denom = a.abs().max(b.abs()).max(f64::MIN_POSITIVE);
    (a - b).abs() / denom
}

/// Do the two vertex sets agree as MULTISETS when snapped to a grid of `eps`?
///
/// Caveat: a vertex sitting exactly on a grid boundary can round either way, so two near-equal
/// points straddling a cell edge quantize apart. Harmless for well-separated tessellation vertices
/// at a sane `eps`; a boundary-tolerant snap is the fix if a pathological corpus needs it.
#[must_use]
pub fn vertex_multiset_matches(a: &[[f64; 3]], b: &[[f64; 3]], eps: f64) -> bool {
    a.len() == b.len() && quantized_multiset(a, eps) == quantized_multiset(b, eps)
}

fn quantized_multiset(verts: &[[f64; 3]], eps: f64) -> BTreeMap<[i64; 3], u32> {
    let mut m = BTreeMap::new();
    for v in verts {
        *m.entry(quantize(*v, eps)).or_insert(0) += 1;
    }
    m
}

/// Snap a vertex to an integer grid of `eps` units so near-equal floats collapse to one key.
fn quantize(v: [f64; 3], eps: f64) -> [i64; 3] {
    [
        (v[0] / eps).round() as i64,
        (v[1] / eps).round() as i64,
        (v[2] / eps).round() as i64,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openscad::find_bin;

    #[test]
    fn multiset_is_order_independent_and_eps_tolerant() {
        let a = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let b = [[1.0, 0.0, 0.0], [0.0, 0.0, 1e-9]]; // reordered + 1e-9 jitter
        assert!(vertex_multiset_matches(&a, &b, 1e-6)); // coarse grid: jitter absorbed
        assert!(!vertex_multiset_matches(&a, &b, 1e-12)); // fine grid: jitter separates
        assert!(!vertex_multiset_matches(
            &a,
            &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
            1e-6
        )); // genuinely different set
        assert!(!vertex_multiset_matches(&a, &a[..1], 1e-6)); // length mismatch
    }

    fn skip_if_no_oracle() -> bool {
        if find_bin().is_none() {
            eprintln!("skipping: OpenSCAD not found");
            return true;
        }
        false
    }

    /// The tracer bullet's payoff: run the sphere resolution sweep, print the tier matrix, and gate
    /// on the strongest tier (boolean residual ~0, genus exact).
    #[test]
    fn sphere_resolution_matrix() {
        if skip_if_no_oracle() {
            return;
        }
        eprintln!(
            "\n$fn | scad_v oracle_v | scad_t oracle_t | vtx_eps | vol_err  area_err | genus | bool_resid"
        );
        eprintln!("{}", "-".repeat(92));
        for fn_ in [8, 16, 32, 64, 128, 256] {
            let src = format!("sphere(10, $fn={fn_});");
            let c = compare(&src, Duration::from_secs(60)).unwrap();
            eprintln!(
                "{fn_:>3} | {:>6} {:>8} | {:>6} {:>8} | {:>7} | {:.1e}  {:.1e} | {}/{}   | {:.2e}",
                c.scad_verts,
                c.oracle_verts,
                c.scad_tris,
                c.oracle_tris,
                c.vertex_match_eps
                    .map_or_else(|| "none".to_string(), |e| format!("{e:.0e}")),
                c.volume_rel_err,
                c.area_rel_err,
                c.genus_scad,
                c.genus_oracle,
                c.boolean_residual,
            );
            // The gate: both are closed genus-0 solids and their symmetric difference is negligible.
            assert_eq!(c.genus_scad, 0, "$fn={fn_}: scad-rs not genus 0");
            assert_eq!(c.genus_oracle, 0, "$fn={fn_}: oracle not genus 0");
            assert!(
                c.boolean_residual < 1e-3,
                "$fn={fn_}: boolean residual {:.2e} exceeds 1e-3",
                c.boolean_residual
            );
        }
        eprintln!();
    }
}
