//! The auto pipeline (Phase 14): a model too big to print → the cut stack + onion connectors that
//! make it printable. Chains [`crate::auto_slice`] (fit the bed) with per-cut cross-section
//! auto-placement (onions on the join faces), applying the corner-clearance + axial-cap rules that
//! keep joints off cut intersections and out of thin slabs. Shared by the GUI (auto-on-open + the
//! Auto button) and the CLI (`fab make`) so both seed IDENTICAL plans — the constants + placement
//! rules live here, not mirrored per front-end.

#[cfg(all(feature = "kernel", feature = "native"))]
use std::path::Path;
#[cfg(all(feature = "kernel", feature = "native"))]
use std::time::Duration;

#[cfg(feature = "kernel")]
use anyhow::Result;

#[cfg(feature = "kernel")]
use crate::cross_section;
use crate::manifest::Connector;
#[cfg(feature = "kernel")]
use crate::num::Num;
#[cfg(all(feature = "kernel", feature = "native"))]
use crate::openscad::Openscad;

/// Smallest onion worth placing (mm) — below this the slab/wall is too thin for a useful joint.
pub const MIN_ONION: f64 = 2.0;
/// Material left between the onion equator and the nearest edge / slab face.
pub const ONION_WALL: f64 = 1.2;
/// Largest onion the auto-sizer grows to in open material.
pub const ONION_MAX_D: f64 = 16.0;
/// Max gap between alignment onions (mm) — the placement guarantees every stretch of a join face is
/// within this of an onion, so no long span is left unpinned to sag. Tighter → more onions along a
/// long joint; this is the alignment interval, not a fill pitch.
pub const ONION_SPACING: f64 = 80.0;
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
/// points out of the section plane — bounded axially, not in-section). Pub + ungated: the web
/// editor sizes manual onions with the same rule auto-place uses, kernel or not.
pub fn cap_dir(axis: usize) -> Option<[f64; 2]> {
    match axis {
        0 | 1 => Some([0.0, 1.0]), // X / Y cut: +Z is the section's 2nd coord
        _ => None,                 // Z cut
    }
}

#[cfg(feature = "kernel")]
fn axis_char(axis: usize) -> char {
    match axis {
        0 => 'x',
        1 => 'y',
        _ => 'z',
    }
}

/// Room bordering cut `i` along its axis on each side `(below, above)`: distance to the nearest
/// same-axis neighbour, or the model bound.
#[cfg(feature = "kernel")]
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
#[cfg(feature = "kernel")]
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
/// axial-capped by slab thickness). Cross-sections are IN-PROCESS ([`crate::kernel::Solid::cross_section`]),
/// no OpenSCAD spawn. `base` is the whole-model solid; `min`/`max` its bbox; `bed` the printer build
/// volume `[x, y, z]`. A model that already fits → an empty plan.
#[cfg(feature = "kernel")]
pub fn plan(
    base: &crate::kernel::Solid,
    min: [f64; 3],
    max: [f64; 3],
    bed: [f64; 3],
) -> Result<AutoPlan> {
    let cuts: Vec<(usize, f64)> = crate::auto_slice::auto_slice(min, max, bed)
        .into_iter()
        .map(|c| (c.axis, c.at))
        .collect();

    let mut connectors = Vec::new();
    for (i, &(ai, at)) in cuts.iter().enumerate() {
        let loops = base.cross_section(ai, at); // in-process, no OpenSCAD spawn (17.2)
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

    let cuts = cuts
        .into_iter()
        .map(|(ai, at)| (axis_char(ai), at))
        .collect();
    Ok(AutoPlan { cuts, connectors })
}

/// Make a printable Bambu multi-plate project from a model in ONE shot (14.3) — the headless twin of
/// the GUI's auto-open. Renders the base (front-door), auto-slices it to fit `bed`, auto-places
/// onions ([`plan`]), orients each piece least-support ([`crate::auto_orient::best_up`]), packs onto
/// the fewest plates, and writes the project to `out_3mf`. Reuses the EXACT lib code the GUI drives,
/// so CLI and GUI produce the same result. Returns the export summary (plates, pieces, fill).
#[cfg(all(feature = "kernel", feature = "native"))]
pub fn make(
    oscad: &Openscad,
    source: &Path,
    bed: [f64; 3],
    out_3mf: &Path,
    out_dir: &Path,
    timeout: Duration,
    gap: f64,
) -> Result<crate::bambu::ExportSummary> {
    use crate::kernel::Solid;
    use anyhow::{Context, bail};

    std::fs::create_dir_all(out_dir)?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // Front-door: render the base model to a mesh once.
    let base_stl = out_dir.join(format!("{stem}.stl"));
    if !oscad.render(source, &base_stl, timeout)?.ok {
        bail!("source render failed: {}", source.display());
    }
    let base = Solid::from_stl_file(&base_stl)?;
    let out = std::fs::File::create(out_3mf)
        .with_context(|| format!("creating {}", out_3mf.display()))?;
    make_solid(base, bed, out, gap)
}

/// The kernel-only core of [`make`]: everything after the front-door render — rotate-to-fit,
/// auto-plan, per-piece orientation, pack, Bambu emit — from a `Solid` straight into any
/// `Write + Seek` sink. The browser build's whole export IS this call (bytes in, 3mf out,
/// no filesystem); native `make` wraps it with the OpenSCAD render + a `File`.
#[cfg(feature = "kernel")]
pub fn make_solid<W: std::io::Write + std::io::Seek>(
    base: crate::kernel::Solid,
    bed: [f64; 3],
    out: W,
    gap: f64,
) -> Result<crate::bambu::ExportSummary> {
    use anyhow::Context;

    base.bbox().context("model has no geometry")?;

    // Rotate-to-fit: spin the model to the fewest bed pieces before cutting — a part lying diagonally
    // fits in fewer. The pieces come out in the rotated frame and re-orient per-piece for printing, so
    // the assembled object is identical, just cut less. Identity when no spin helps.
    let fit = crate::auto_slice::best_fit_rotation(&base, bed);
    let base = base.transform(&fit.rot);
    let (min, max) = (fit.min, fit.max);

    // Auto-plan cuts + onions (in-process cross-sections, no per-cut OpenSCAD spawn).
    let planned = plan(&base, min, max, bed)?;
    make_planned(base, &planned.cuts, planned.connectors, bed, out, gap)
}

/// The pack/export tail of [`make_solid`] with the plan SUPPLIED — the web editor's export runs
/// this so user-edited connectors survive; `make_solid` feeds it the auto-plan. `base` must
/// already be in the plan's frame.
#[cfg(feature = "kernel")]
pub fn make_planned<W: std::io::Write + std::io::Seek>(
    base: crate::kernel::Solid,
    cuts: &[(char, f64)],
    connectors: Vec<Connector>,
    bed: [f64; 3],
    out: W,
    gap: f64,
) -> Result<crate::bambu::ExportSummary> {
    use crate::bambu::{self, PieceToPlace};
    use crate::manifest::{Cut, PieceOrient, Slicing};
    use anyhow::bail;

    let make_cut = || -> Vec<Cut> {
        cuts.iter()
            .map(|&(ax, at)| Cut {
                axis: ax.to_string(),
                at: Num::Float(at),
            })
            .collect()
    };

    // Bare slice → least-support orientation per piece.
    let bare = Slicing {
        printer: None,
        cut: make_cut(),
        connector: vec![],
        orient: vec![],
    };
    let mut ups: Vec<([usize; 3], [f64; 3])> = Vec::new();
    for (piece, solid) in crate::slicing::slice_solid(&bare, &base)? {
        // best_up now speaks Vec3 (mesh tris are [Vec3;3]); keep the [usize;3]→[f64;3] up map as arrays.
        let up = crate::auto_orient::best_up(&solid.tris(), &[]);
        ups.push((piece, up.to_array()));
    }

    // Carved slice, gated by those orientations.
    let orient = ups
        .iter()
        .map(|&(piece, up)| PieceOrient {
            piece,
            up: [Num::Float(up[0]), Num::Float(up[1]), Num::Float(up[2])],
        })
        .collect();
    let spec = Slicing {
        printer: None,
        cut: make_cut(),
        connector: connectors,
        orient,
    };
    let pieces = crate::slicing::slice_solid(&spec, &base)?;
    if pieces.is_empty() {
        bail!("slice produced no pieces");
    }

    // Orient (best_up) + pack + export.
    let to_place: Vec<PieceToPlace> = pieces
        .iter()
        .map(|(piece, solid)| {
            let up = ups
                .iter()
                .find(|(p, _)| p == piece)
                .map(|(_, u)| *u)
                .unwrap_or([0.0, 0.0, 1.0]);
            let (v, t) = solid.to_indexed();
            let verts = v.iter().map(|p| p.to_array()).collect();
            let tris = t.iter().map(|f| f.indices()).collect();
            PieceToPlace {
                mesh: bambu::Mesh { verts, tris },
                up,
            }
        })
        .collect();
    bambu::export_plates_to(out, to_place, [bed[0], bed[1]], gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "kernel")]
    fn cap_direction_and_axis_char() {
        assert_eq!(cap_dir(0), Some([0.0, 1.0]));
        assert_eq!(cap_dir(1), Some([0.0, 1.0]));
        assert_eq!(cap_dir(2), None);
        assert_eq!([axis_char(0), axis_char(1), axis_char(2)], ['x', 'y', 'z']);
    }

    #[test]
    #[cfg(feature = "kernel")]
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
    #[cfg(feature = "kernel")]
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
    #[cfg(all(feature = "kernel", feature = "native"))]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn make_produces_a_multi_plate_bambu_project() {
        let tmp = std::env::temp_dir().join(format!("auto_make_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let scad = tmp.join("bigbox.scad");
        std::fs::write(&scad, "cube([700,120,60]);").unwrap(); // 700mm > a 256 bed → must be cut
        let oscad = Openscad::discover(None).unwrap();
        let out = tmp.join("bigbox-plates.3mf");

        let sum = make(
            &oscad,
            &scad,
            [256.0, 256.0, 256.0],
            &out,
            &tmp,
            Duration::from_secs(60),
            5.0,
        )
        .unwrap();
        assert_eq!(sum.pieces, 3, "700mm on a 256 bed → 3 pieces");
        assert!(out.exists(), "wrote the project");

        // It's a valid Bambu project: the gate + one plate per piece it couldn't co-locate.
        let f = std::fs::File::open(&out).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        use std::io::Read;
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("name=\"Application\">BambuStudio-"));
        assert_eq!(model.matches("<item ").count(), 3);
        assert!(sum.plates >= 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(feature = "kernel")]
    fn make_solid_exports_plates_in_memory() {
        // The browser export path end to end: Solid in, Bambu 3mf bytes out, no filesystem.
        // 700mm on a 256 bed → 3 pieces, valid multi-piece project in the Cursor.
        let base = crate::kernel::Solid::cube(700.0, 120.0, 60.0, false);
        let mut buf = std::io::Cursor::new(Vec::new());
        let sum = make_solid(base, [256.0; 3], &mut buf, 5.0).unwrap();
        assert_eq!(sum.pieces, 3, "700mm on a 256 bed → 3 pieces");
        let bytes = buf.into_inner();
        assert!(!bytes.is_empty());
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        use std::io::Read;
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("name=\"Application\">BambuStudio-"));
        assert_eq!(model.matches("<item ").count(), 3);
    }

    #[test]
    #[cfg(feature = "kernel")]
    fn plan_slices_and_connects_a_big_box() {
        // Now in-process — no OpenSCAD needed. 600mm on X, 256 bed → X overflows (only X) → cuts on
        // X, onions on the cut faces. Kernel cube (min corner at origin) matches the min/max.
        let base = crate::kernel::Solid::cube(600.0, 100.0, 50.0, false);
        let p = plan(&base, [0.0; 3], [600.0, 100.0, 50.0], [256.0; 3]).unwrap();
        assert!(!p.cuts.is_empty(), "600mm on a 256 bed must be cut");
        assert!(
            p.cuts.iter().all(|&(ax, _)| ax == 'x'),
            "only X overflows: {:?}",
            p.cuts
        );
        assert!(!p.connectors.is_empty(), "onions seeded on the cut faces");
        assert!(
            p.connectors
                .iter()
                .all(|c| c.kind == "onion" && c.size.is_some())
        );
    }
}
