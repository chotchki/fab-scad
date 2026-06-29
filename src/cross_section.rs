//! Cross-section of a sliced model at a cut plane (the per-cut connector editor #43, and the
//! profile #41's auto-placement fits to). It wraps OpenSCAD's `projection(cut = true)` — bring the
//! cut plane onto z=0, project, and read the SVG outline — rather than slicing the mesh in-process:
//! OpenSCAD's CSG already gets holes and disjoint loops right. The loops come back in the cut
//! plane's two NON-AXIS dims, ascending — i.e. connector-pos space — so the GUI can draw the
//! profile and pick connectors on it directly, and #41 can fit to it, with no extra mapping.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

use crate::openscad::Openscad;

/// A closed loop of 2D points in connector-pos coords. One outer outline, plus one loop per hole.
pub type Loop = Vec<[f64; 2]>;

/// Cross-section of `stl` (a rendered mesh) at `axis` (0=X, 1=Y, 2=Z) = `at`, as profile loops in
/// connector-pos coords (the cut plane's two non-axis dims, ascending).
pub fn cross_section(
    oscad: &Openscad,
    stl: &Path,
    axis: usize,
    at: f64,
    out_dir: &Path,
    timeout: Duration,
) -> Result<Vec<Loop>> {
    std::fs::create_dir_all(out_dir)?;
    let scad = out_dir.join("xsection.scad");
    let svg = out_dir.join("xsection.svg");
    std::fs::write(&scad, projection_scad(stl, axis, at)?)?;
    let r = oscad.render(&scad, &svg, timeout)?;
    ensure!(r.ok, "cross-section render failed (axis {axis} at {at})");
    let text = std::fs::read_to_string(&svg).context("read cross-section SVG")?;
    Ok(parse_loops(&text).into_iter().map(|l| map_to_pos(l, axis)).collect())
}

/// The projection driver for one cut: rotate the cut plane onto z=0 (after centring it there with
/// the translate), then `projection(cut = true)`. The chosen rotations leave the projected (u, v)
/// axis-aligned and un-flipped — see `map_to_pos`.
fn projection_scad(stl: &Path, axis: usize, at: f64) -> Result<String> {
    let stl = stl.to_str().context("non-UTF8 STL path")?;
    let xform = match axis {
        0 => format!("rotate([0, 90, 0]) translate([{}, 0, 0])", num(-at)),
        1 => format!("rotate([-90, 0, 0]) translate([0, {}, 0])", num(-at)),
        2 => format!("translate([0, 0, {}])", num(-at)),
        _ => bail!("axis must be 0/1/2, got {axis}"),
    };
    Ok(format!("projection(cut = true) {xform} import(\"{stl}\");\n"))
}

/// Parse the `M x,y L x,y … z` paths of an OpenSCAD 2D SVG into loops of (u, v) points. Projection
/// of a faceted mesh is straight segments only, so M/L/z is the whole grammar; each `<path>` (or
/// each `z`) closes one loop.
fn parse_loops(svg: &str) -> Vec<Vec<[f64; 2]>> {
    let mut loops = Vec::new();
    for d in svg.split("d=\"").skip(1).filter_map(|s| s.split('"').next()) {
        let mut pts: Vec<[f64; 2]> = Vec::new();
        for tok in d.split_whitespace() {
            match tok {
                "M" | "L" | "m" | "l" => {}
                "z" | "Z" => {
                    if !pts.is_empty() {
                        loops.push(std::mem::take(&mut pts));
                    }
                }
                _ => {
                    if let Some((x, y)) = tok.split_once(',') {
                        if let (Ok(x), Ok(y)) = (x.parse(), y.parse()) {
                            pts.push([x, y]);
                        }
                    }
                }
            }
        }
        if !pts.is_empty() {
            loops.push(pts);
        }
    }
    loops
}

/// Auto-size a connector at `point` inside a cross-section: the largest diameter that still keeps a
/// `wall`-thick margin to the nearest profile edge — the outer outline OR any hole — capped at
/// `max_d`. There is NO lower clamp: a point against a thin wall returns a SMALL (down to 0)
/// diameter, never one forced up past the wall. The CALLER decides a usable minimum / whether to
/// place. The GUI feeds this at placement and #41's auto-place fits to the same loops.
pub fn fit_diameter(loops: &[Loop], point: [f64; 2], wall: f64, max_d: f64) -> f64 {
    let nearest = loops
        .iter()
        .flat_map(|lp| (0..lp.len()).map(move |i| (lp, i)))
        .map(|(lp, i)| point_to_segment(point, lp[i], lp[(i + 1) % lp.len()]))
        .fold(f64::INFINITY, f64::min);
    (2.0 * (nearest - wall)).clamp(0.0, max_d)
}

/// Shortest distance from `p` to segment `a`–`b`.
fn point_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
    ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
}

/// Map a loop's projected (u, v) to connector-pos coords (the cut's two non-axis dims, ascending).
/// From the `projection_scad` transforms: X → (u,v)=(z,y) so pos=(y,z)=(v,u); Y → (u,v)=(x,z)=pos;
/// Z → (u,v)=(x,y)=pos. Only X swaps.
fn map_to_pos(loop_uv: Vec<[f64; 2]>, axis: usize) -> Vec<[f64; 2]> {
    loop_uv.into_iter().map(|[u, v]| if axis == 0 { [v, u] } else { [u, v] }).collect()
}

/// SCAD number: trim a trailing `.0` so `translate` reads cleanly; never scientific notation.
fn num(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string(); // also folds -0.0
    }
    let s = format!("{x:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r#"<svg viewBox="0 0 10 5">
        <path d="
        M 0,0 L 10,0 L 10,5
         L 0,5 z
        " stroke="black"/>
        </svg>"#;

    #[test]
    fn parses_a_single_loop() {
        let loops = parse_loops(SVG);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0], vec![[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]]);
    }

    #[test]
    fn parses_outer_plus_hole() {
        let svg = r#"<path d="M 0,0 L 4,0 L 4,4 z"/><path d="M 1,1 L 2,1 L 2,2 z"/>"#;
        let loops = parse_loops(svg);
        assert_eq!(loops.len(), 2);
        assert_eq!(loops[1], vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0]]);
    }

    #[test]
    fn x_axis_swaps_uv_to_pos_yz() {
        // X cut: SVG (u,v) = (z, y); connector pos must be (y, z) = (v, u).
        let mapped = map_to_pos(vec![[3.0, 7.0]], 0);
        assert_eq!(mapped, vec![[7.0, 3.0]]);
    }

    #[test]
    fn yz_axes_pass_uv_through() {
        assert_eq!(map_to_pos(vec![[3.0, 7.0]], 1), vec![[3.0, 7.0]]);
        assert_eq!(map_to_pos(vec![[3.0, 7.0]], 2), vec![[3.0, 7.0]]);
    }

    #[test]
    fn fit_diameter_sizes_to_nearest_edge() {
        // a 40x40 square outline centred at origin
        let sq = vec![vec![[-20.0, -20.0], [20.0, -20.0], [20.0, 20.0], [-20.0, 20.0]]];
        // mid-material: 5mm to the right edge, wall 1 -> d = 2*(5-1) = 8
        assert!((fit_diameter(&sq, [15.0, 0.0], 1.0, 16.0) - 8.0).abs() < 1e-9);
        // open centre: 20mm to any edge -> 2*(20-1)=38, clamped to max 16
        assert_eq!(fit_diameter(&sq, [0.0, 0.0], 1.0, 16.0), 16.0);
        // hard against a wall -> shrinks toward 0, never forced up past the wall
        assert_eq!(fit_diameter(&sq, [19.5, 0.0], 1.0, 16.0), 0.0);
        // a thin wall returns a small (sub-min) diameter rather than an oversized one
        assert!((fit_diameter(&sq, [17.5, 0.0], 1.0, 16.0) - 3.0).abs() < 1e-9); // 2*(2.5-1)
        // a hole pulls the size down too
        let with_hole = vec![sq[0].clone(), vec![[4.0, -2.0], [8.0, -2.0], [8.0, 2.0], [4.0, 2.0]]];
        assert!(fit_diameter(&with_hole, [0.0, 0.0], 1.0, 16.0) < 16.0);
    }

    #[test]
    fn projection_scad_per_axis() {
        let p = Path::new("m.stl");
        assert!(projection_scad(p, 0, 5.0).unwrap().contains("rotate([0, 90, 0]) translate([-5, 0, 0])"));
        assert!(projection_scad(p, 1, -3.0).unwrap().contains("rotate([-90, 0, 0]) translate([0, 3, 0])"));
        assert!(projection_scad(p, 2, 0.0).unwrap().contains("translate([0, 0, 0])"));
        assert!(projection_scad(p, 3, 0.0).is_err());
    }
}
