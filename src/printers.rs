//! `printers.toml` (bed profiles) and the cut planner (4.3).
//!
//! Cardinal rule from the spec: **fewer cuts > clever cuts.** So before splitting a part,
//! try to make it fit the bed whole — first by a 90° orientation (which dimension stands
//! up), then by rotating the footprint diagonally to use the bed's diagonal. Only when it
//! still won't fit do we cut, and then on the orientation that yields the fewest pieces.
//!
//! This is pure planning over a bounding box; the actual cutting is `scad-lib/slicer.scad`,
//! fed the cut positions this produces. Wiring a real model's bbox in (render → measure)
//! is the Phase 6 job; for now `fab plan --size XxYxZ` drives it directly.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::num::Num;

#[derive(Debug, Deserialize)]
struct PrintersFile {
    #[serde(default)]
    printer: Vec<PrinterRaw>,
}
/// Bambu preset identifiers for a printer, so the exported multi-plate `.3mf`'s `project_settings.config`
/// names EXISTING BambuStudio presets — else BambuStudio prompts to import "customized filament/printer
/// presets". Optional per printer (a `[printer.bambu]` sub-table in printers.toml); the exact strings
/// come from a real "Save Project" export. When absent, the writer emits a minimal config (which loads
/// the plates but with that import prompt).
#[derive(Debug, Clone, Deserialize)]
pub struct BambuPreset {
    pub model: String,      // printer_model, e.g. "Bambu Lab H2D"
    pub printer_id: String, // printer_settings_id
    pub process_id: String, // print_settings_id
    #[serde(default)]
    pub filaments: Vec<String>, // filament_settings_id (one per used slot)
    #[serde(default)]
    pub nozzles: Vec<String>, // nozzle_diameter
    #[serde(default)]
    pub bed_type: Option<String>, // curr_bed_type
}

#[derive(Debug, Deserialize)]
struct PrinterRaw {
    name: String,
    bed: [Num; 3],
    /// The REAL printer plate size (the Bambu `printable_area`), when it's LARGER than the usable
    /// `bed` — e.g. the H2D's plate is 350 wide but only 325 is reachable by extruder 1. Drives the
    /// multi-plate `.3mf` grid + config so BambuStudio's plate size matches ours (no "custom bed"
    /// prompt); packing still fits pieces within `bed`. Optional; defaults to `bed`.
    #[serde(default)]
    plate: Option<[Num; 3]>,
    #[serde(default)]
    bambu: Option<BambuPreset>,
    #[serde(default)]
    default: bool,
}

/// A printer's usable build volume.
#[derive(Debug, Clone)]
pub struct Printer {
    pub name: String,
    pub bed: [f64; 3], // x, y, z in mm — the USABLE area (pieces are packed to fit within this)
    /// The real plate size for the Bambu export grid/`printable_area` (≥ `bed`; = `bed` if unset).
    pub plate: [f64; 3],
    /// Bambu preset ids for a prompt-free `.3mf` import (None → minimal config + the import prompt).
    pub bambu: Option<BambuPreset>,
    pub is_default: bool,
}

pub fn load(path: &Path) -> Result<Vec<Printer>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let f: PrintersFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(f.printer
        .into_iter()
        .map(|p| {
            let bed = [p.bed[0].f(), p.bed[1].f(), p.bed[2].f()];
            let plate = p
                .plate
                .map(|a| [a[0].f(), a[1].f(), a[2].f()])
                .unwrap_or(bed);
            Printer {
                name: p.name,
                bed,
                plate,
                bambu: p.bambu,
                is_default: p.default,
            }
        })
        .collect())
}

/// Pick a printer by name, else the one flagged `default`, else the first.
pub fn select<'a>(printers: &'a [Printer], name: Option<&str>) -> Result<&'a Printer> {
    match name {
        Some(n) => printers
            .iter()
            .find(|p| p.name == n)
            .with_context(|| format!("no printer '{n}' in printers.toml")),
        None => printers
            .iter()
            .find(|p| p.is_default)
            .or_else(|| printers.first())
            .context("no printers defined in printers.toml (it's still the example stub)"),
    }
}

/// One axis's cut plan: `count` planes at these centered coordinates (feed to slice()).
#[derive(Debug, PartialEq)]
pub struct Cut {
    pub axis: char, // 'X' | 'Y' | 'Z' on the bed
    pub count: usize,
    pub positions: Vec<f64>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Fits whole in this orientation (`up` = which part axis stands vertical).
    FitsAsIs { up: usize },
    /// Fits whole if the footprint is rotated `degrees` in XY (diagonal placement).
    FitsRotated { up: usize, degrees: f64 },
    /// Won't fit; cut on the orientation giving the fewest pieces.
    NeedsCuts {
        oriented: [f64; 3], // part dims along bed x,y,z for the chosen orientation
        cuts: Vec<Cut>,
        pieces: usize,
    },
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub size: [f64; 3],
    pub bed: [f64; 3],
    pub outcome: Outcome,
}

/// Plan how to get a `size`-bounded part onto a `bed`: orient, rotate, or (last) cut.
pub fn plan(size: [f64; 3], bed: [f64; 3]) -> Plan {
    let [bx, by, bz] = bed;

    // 1. Whole fit by a 90° orientation (cheapest — no cuts, no rotation). Prefer the
    //    flattest fitting orientation (smallest height) — lower + more stable to print.
    let mut whole: Option<usize> = None;
    for up in 0..3 {
        let f = others(size, up);
        if size[up] <= bz && fits(f[0], f[1], bx, by) && whole.is_none_or(|u| size[up] < size[u]) {
            whole = Some(up);
        }
    }
    if let Some(up) = whole {
        return Plan {
            size,
            bed,
            outcome: Outcome::FitsAsIs { up },
        };
    }

    // 2. Whole fit by rotating the footprint diagonally — least rotation wins.
    let mut rot: Option<(usize, f64)> = None;
    for up in 0..3 {
        if size[up] > bz {
            continue;
        }
        let f = others(size, up);
        if let Some(deg) = min_rotation(f[0], f[1], bx, by)
            && rot.is_none_or(|(_, d)| deg < d)
        {
            rot = Some((up, deg));
        }
    }
    if let Some((up, degrees)) = rot {
        return Plan {
            size,
            bed,
            outcome: Outcome::FitsRotated { up, degrees },
        };
    }

    // 3. Cut. Pick the orientation + footprint assignment with the fewest pieces.
    let mut best: Option<(usize, [f64; 3], Vec<Cut>)> = None;
    for up in 0..3 {
        let h = size[up];
        let f = others(size, up);
        for &(dx, dy) in &[(f[0], f[1]), (f[1], f[0])] {
            let (px, py, pz) = (
                pieces_along(dx, bx),
                pieces_along(dy, by),
                pieces_along(h, bz),
            );
            let pieces = px * py * pz;
            let mut cuts = Vec::new();
            if px > 1 {
                cuts.push(Cut {
                    axis: 'X',
                    count: px - 1,
                    positions: even_cuts(dx, px),
                });
            }
            if py > 1 {
                cuts.push(Cut {
                    axis: 'Y',
                    count: py - 1,
                    positions: even_cuts(dy, py),
                });
            }
            if pz > 1 {
                cuts.push(Cut {
                    axis: 'Z',
                    count: pz - 1,
                    positions: even_cuts(h, pz),
                });
            }
            // Fewest pieces wins; tie-break to the flattest orientation (smallest height).
            if best
                .as_ref()
                .is_none_or(|(bp, bo, _)| (pieces, h) < (*bp, bo[2]))
            {
                best = Some((pieces, [dx, dy, h], cuts));
            }
        }
    }
    let (pieces, oriented, cuts) = best.expect("3 orientations always produce a candidate");
    Plan {
        size,
        bed,
        outcome: Outcome::NeedsCuts {
            oriented,
            cuts,
            pieces,
        },
    }
}

/// The two dimensions perpendicular to the `up` axis (the footprint).
fn others(s: [f64; 3], up: usize) -> [f64; 2] {
    match up {
        0 => [s[1], s[2]],
        1 => [s[0], s[2]],
        _ => [s[0], s[1]],
    }
}

/// Footprint fits the bed either way round.
fn fits(f0: f64, f1: f64, bx: f64, by: f64) -> bool {
    (f0 <= bx && f1 <= by) || (f1 <= bx && f0 <= by)
}

fn pieces_along(d: f64, bed: f64) -> usize {
    (d / bed).ceil().max(1.0) as usize
}

/// Centered cut coordinates that split a `d`-long axis into `count` equal pieces.
fn even_cuts(d: f64, count: usize) -> Vec<f64> {
    (1..count)
        .map(|i| d * (i as f64) / (count as f64) - d / 2.0)
        .collect()
}

/// Smallest in-plane rotation (deg) whose rotated footprint AABB fits the bed, if any.
fn min_rotation(f0: f64, f1: f64, bx: f64, by: f64) -> Option<f64> {
    let mut t: f64 = 0.0;
    while t <= 90.0 {
        let r = t.to_radians();
        let ax = f0 * r.cos() + f1 * r.sin();
        let ay = f0 * r.sin() + f1 * r.cos();
        if fits(ax, ay, bx, by) {
            return Some(t);
        }
        t += 0.25;
    }
    None
}

/// 0/1/2 -> 'X'/'Y'/'Z', and the matching BOSL2 slice() axis constant.
pub fn axis_name(i: usize) -> char {
    ['X', 'Y', 'Z'][i]
}
pub fn slice_axis(c: char) -> &'static str {
    match c {
        'X' => "RIGHT",
        'Y' => "BACK",
        _ => "UP",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_int_and_float_beds_and_selects_default() {
        let toml = r#"
            [[printer]]
            name = "H2D"
            bed = [325, 320, 320]
            default = true

            [[printer]]
            name = "H2C"
            bed = [300.0, 320, 320]
        "#;
        let f: PrintersFile = ::toml::from_str(toml).unwrap();
        let printers: Vec<Printer> = f
            .printer
            .into_iter()
            .map(|p| {
                let bed = [p.bed[0].f(), p.bed[1].f(), p.bed[2].f()];
                Printer {
                    name: p.name,
                    bed,
                    plate: p.plate.map(|a| [a[0].f(), a[1].f(), a[2].f()]).unwrap_or(bed),
                    bambu: p.bambu,
                    is_default: p.default,
                }
            })
            .collect();
        assert_eq!(printers.len(), 2);
        assert_eq!(select(&printers, None).unwrap().name, "H2D");
        assert_eq!(
            select(&printers, Some("H2C")).unwrap().bed,
            [300.0, 320.0, 320.0]
        );
        assert!(select(&printers, Some("nope")).is_err());
    }

    #[test]
    fn small_part_fits_as_is() {
        let p = plan([100.0, 80.0, 60.0], [325.0, 320.0, 320.0]);
        assert!(matches!(p.outcome, Outcome::FitsAsIs { .. }));
    }

    #[test]
    fn long_part_needs_one_cut_on_its_long_axis() {
        // 400 > 325 and can't be rotated into the bed -> one cut, two pieces.
        let p = plan([400.0, 200.0, 150.0], [325.0, 320.0, 320.0]);
        match p.outcome {
            Outcome::NeedsCuts { pieces, cuts, .. } => {
                assert_eq!(pieces, 2);
                assert_eq!(cuts.len(), 1);
                assert_eq!(cuts[0].axis, 'X'); // laid flat, cut the long axis (not stood up)
                assert_eq!(cuts[0].count, 1);
                assert_eq!(cuts[0].positions, vec![0.0]); // centered single cut
            }
            other => panic!("expected NeedsCuts, got {other:?}"),
        }
    }

    #[test]
    fn long_thin_part_fits_diagonally() {
        // 380 > 300 axis-aligned, but a 380x20 footprint fits a 300x300 bed rotated ~45°.
        let p = plan([380.0, 20.0, 100.0], [300.0, 300.0, 300.0]);
        match p.outcome {
            Outcome::FitsRotated { degrees, .. } => assert!(degrees > 0.0 && degrees < 90.0),
            other => panic!("expected FitsRotated, got {other:?}"),
        }
    }

    #[test]
    fn even_cuts_are_centered() {
        assert_eq!(even_cuts(400.0, 2), vec![0.0]);
        assert_eq!(even_cuts(300.0, 3), vec![-50.0, 50.0]);
        assert_eq!(even_cuts(100.0, 1), Vec::<f64>::new());
    }
}
