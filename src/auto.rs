//! The auto pipeline (Phase 14): a model too big to print → the cut stack + onion connectors that
//! make it printable. Chains [`crate::auto_slice`] (fit the bed) with per-cut cross-section
//! auto-placement (onions on the join faces), applying the corner-clearance + axial-cap rules that
//! keep joints off cut intersections and out of thin slabs. Shared by the GUI (auto-on-open + the
//! Auto button) and the CLI (`fab make`) so both seed IDENTICAL plans — the constants + placement
//! rules live here, not mirrored per front-end.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use crate::cross_section;
use crate::manifest::Connector;
use crate::num::Num;
use crate::openscad::Openscad;

/// Smallest onion worth placing (mm) — below this the slab/wall is too thin for a useful joint.
pub const MIN_ONION: f64 = 2.0;
/// Material left between the onion equator and the nearest edge / slab face.
pub const ONION_WALL: f64 = 1.2;
/// Largest onion the auto-sizer grows to in open material.
pub const ONION_MAX_D: f64 = 16.0;
/// Target span BETWEEN alignment onions — the count scales with the face, so a big joint gets a few
/// spread out and a small one gets a single guide (they align, they don't fill).
pub const ONION_SPACING: f64 = 120.0;
/// Teardrop tip reach = 1/sin(20°): bounds the WORST-case cap the slicer emits (`CAP_ANG_MIN`), so
/// the onion fits at any print orientation decided later.
pub const ONION_TIP: f64 = 2.9238;

/// An auto-generated plan: the cut stack (axis letter + position, in order) and the onion connectors
/// seeded on the cut faces (each connector's `cut` indexes into `cuts`). Empty when the model fits.
pub struct AutoPlan {
    pub cuts: Vec<(char, f64)>,
    pub connectors: Vec<Connector>,
}

/// Onion cap direction (+build = +Z) in a cut's 2D cross-section coords, or `None` for a Z cut (cap
/// points out of the section plane — bounded axially, not in-section).
fn cap_dir(axis: usize) -> Option<[f64; 2]> {
    match axis {
        0 | 1 => Some([0.0, 1.0]), // X / Y cut: +Z is the section's 2nd coord
        _ => None,                 // Z cut
    }
}

fn axis_char(axis: usize) -> char {
    match axis {
        0 => 'x',
        1 => 'y',
        _ => 'z',
    }
}

/// Room bordering cut `i` along its axis on each side `(below, above)`: distance to the nearest
/// same-axis neighbour, or the model bound.
fn axial_room(cuts: &[(usize, f64)], i: usize, min: [f64; 3], max: [f64; 3]) -> (f64, f64) {
    let (ai, at) = cuts[i];
    let (mut below, mut above) = (min[ai], max[ai]);
    for (j, &(aj, aj_at)) in cuts.iter().enumerate() {
        if j == i || aj != ai {
            continue;
        }
        if aj_at <= at && aj_at > below {
            below = aj_at;
        }
        if aj_at >= at && aj_at < above {
            above = aj_at;
        }
    }
    (at - below, above - at)
}

/// Onion-diameter cap from the slab thickness either side of cut `i`: the onion is a sphere reaching
/// d/2 into each piece, except the +Z cap of a Z cut, which reaches the teardrop tip (`ONION_TIP`·r)
/// into the upper slab.
fn axial_cap(cuts: &[(usize, f64)], i: usize, min: [f64; 3], max: [f64; 3]) -> f64 {
    let (below, above) = axial_room(cuts, i, min, max);
    let below_d = 2.0 * (below - ONION_WALL);
    let above_d = if cuts[i].0 == 2 {
        2.0 * (above - ONION_WALL) / ONION_TIP
    } else {
        2.0 * (above - ONION_WALL)
    };
    below_d.min(above_d).max(0.0)
}

/// Auto-plan a print for a model too big for the bed: [`crate::auto_slice`] the bbox, then seed
/// onions on each cut's cross-section (auto-placed, corner-cleared against perpendicular cuts,
/// axial-capped by slab thickness). `base_stl` is the rendered whole model; `min`/`max` its bbox;
/// `bed` the printer build volume `[x, y, z]`. A model that already fits → an empty plan.
pub fn plan(
    oscad: &Openscad,
    base_stl: &Path,
    min: [f64; 3],
    max: [f64; 3],
    bed: [f64; 3],
    out_dir: &Path,
    timeout: Duration,
) -> Result<AutoPlan> {
    let cuts: Vec<(usize, f64)> =
        crate::auto_slice::auto_slice(min, max, bed).into_iter().map(|c| (c.axis, c.at)).collect();

    let mut connectors = Vec::new();
    for (i, &(ai, at)) in cuts.iter().enumerate() {
        let loops = cross_section::cross_section(oscad, base_stl, ai, at, out_dir, timeout)?;
        let placements = cross_section::auto_place(
            &loops,
            ONION_WALL,
            ONION_MAX_D,
            ONION_SPACING,
            MIN_ONION,
            cap_dir(ai),
            ONION_TIP,
        );
        let cap = axial_cap(&cuts, i, min, max);

        // Perpendicular enabled cuts as lines in THIS section's 2D coords — drop onions whose
        // footprint would straddle the intersection (the messy jigsaw corner).
        let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
        let perp: Vec<(usize, f64)> = cuts
            .iter()
            .enumerate()
            .filter(|&(j, &(aj, _))| j != i && aj != ai)
            .filter_map(|(_, &(aj, aj_at))| {
                if aj == others[0] {
                    Some((0, aj_at))
                } else if aj == others[1] {
                    Some((1, aj_at))
                } else {
                    None
                }
            })
            .collect();

        for (p, d) in placements {
            let clearance = d / 2.0 + ONION_WALL;
            if perp.iter().any(|&(c, pat)| (p[c] - pat).abs() < clearance) {
                continue;
            }
            let size = d.min(cap);
            if size >= MIN_ONION {
                connectors.push(Connector {
                    cut: i,
                    kind: "onion".to_string(),
                    screw: None,
                    pos: [Num::Float(p[0]), Num::Float(p[1])],
                    through: None,
                    size: Some(size),
                });
            }
        }
    }

    let cuts = cuts.into_iter().map(|(ai, at)| (axis_char(ai), at)).collect();
    Ok(AutoPlan { cuts, connectors })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_direction_and_axis_char() {
        assert_eq!(cap_dir(0), Some([0.0, 1.0]));
        assert_eq!(cap_dir(1), Some([0.0, 1.0]));
        assert_eq!(cap_dir(2), None);
        assert_eq!([axis_char(0), axis_char(1), axis_char(2)], ['x', 'y', 'z']);
    }

    #[test]
    fn axial_room_finds_nearest_same_axis_neighbours() {
        // Three X cuts in a model spanning X ∈ [0, 500]. The middle cut's room is bounded by its
        // neighbours; the edge cuts by the model bound.
        let cuts = [(0usize, 100.0), (0, 300.0), (1, 250.0)]; // the Y cut is a different axis
        let (min, max) = ([0.0; 3], [500.0, 500.0, 500.0]);
        assert_eq!(axial_room(&cuts, 0, min, max), (100.0, 200.0)); // [0..100..300]
        assert_eq!(axial_room(&cuts, 1, min, max), (200.0, 200.0)); // [100..300..500]
        // The Y cut ignores the X cuts entirely.
        assert_eq!(axial_room(&cuts, 2, min, max), (250.0, 250.0));
    }

    #[test]
    fn axial_cap_reserves_the_wall_and_z_tip() {
        let cuts = [(0usize, 250.0)]; // one X cut, slab 250 each side
        let (min, max) = ([0.0; 3], [500.0, 500.0, 500.0]);
        // X cut: sphere both sides → 2·(250 − 1.2) = 497.6.
        assert!((axial_cap(&cuts, 0, min, max) - 497.6).abs() < 1e-6);
        // Z cut: the +Z (above) side reserves the teardrop tip, so it's the tighter cap.
        let zc = [(2usize, 250.0)];
        let expect = (2.0 * (250.0 - ONION_WALL) / ONION_TIP).min(2.0 * (250.0 - ONION_WALL));
        assert!((axial_cap(&zc, 0, min, max) - expect).abs() < 1e-6);
    }

    #[test]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn plan_slices_and_connects_a_big_box() {
        let tmp = std::env::temp_dir().join(format!("auto_plan_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let scad = tmp.join("box.scad");
        std::fs::write(&scad, "cube([600,100,50]);").unwrap();
        let stl = tmp.join("box.stl");
        let oscad = Openscad::discover(None).unwrap();
        oscad.render(&scad, &stl, Duration::from_secs(60)).unwrap();

        // 600mm on X, 256 bed → X overflows (only X) → cuts on X, onions on the cut faces.
        let p = plan(&oscad, &stl, [0.0; 3], [600.0, 100.0, 50.0], [256.0; 3], &tmp, Duration::from_secs(60))
            .unwrap();
        assert!(!p.cuts.is_empty(), "600mm on a 256 bed must be cut");
        assert!(p.cuts.iter().all(|&(ax, _)| ax == 'x'), "only X overflows: {:?}", p.cuts);
        assert!(!p.connectors.is_empty(), "onions seeded on the cut faces");
        assert!(p.connectors.iter().all(|c| c.kind == "onion" && c.size.is_some()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
