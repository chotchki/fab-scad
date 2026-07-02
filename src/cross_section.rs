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

/// Distance from `point` to the profile boundary along unit `dir` — the nearest positive-t crossing
/// of the ray over every loop segment, or +inf if it never exits. Used to bound the onion's cap,
/// which reaches past the sphere only in the build direction.
pub fn dist_along(loops: &[Loop], point: [f64; 2], dir: [f64; 2]) -> f64 {
    let [px, py] = point;
    let [dx, dy] = dir;
    let mut best = f64::INFINITY;
    for lp in loops {
        let n = lp.len();
        for i in 0..n {
            let (a, b) = (lp[i], lp[(i + 1) % n]);
            let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
            let denom = dx * ey - dy * ex; // cross(dir, edge); ~0 = parallel
            if denom.abs() < 1e-12 {
                continue;
            }
            let (rx, ry) = (a[0] - px, a[1] - py);
            let t = (rx * ey - ry * ex) / denom; // ray param (distance, dir is unit)
            let s = (rx * dy - ry * dx) / denom; // segment param
            if t > 1e-9 && (0.0..=1.0).contains(&s) {
                best = best.min(t);
            }
        }
    }
    best
}

/// Largest onion diameter at `point` that fits with a `wall` margin. Like `fit_diameter`, but the
/// onion is a TEARDROP: its tip reaches `tip_factor`·r past the sphere in the cap direction `cap_dir`
/// (unit, cross-section coords), so bound the tip against the edge in that direction too. `None`
/// cap_dir (the cut is perpendicular to the build, cap out of plane) falls back to the sphere.
pub fn fit_onion(
    loops: &[Loop],
    point: [f64; 2],
    wall: f64,
    max_d: f64,
    cap_dir: Option<[f64; 2]>,
    tip_factor: f64,
) -> f64 {
    let sphere = fit_diameter(loops, point, wall, max_d);
    match cap_dir {
        Some(dir) if tip_factor > 0.0 => {
            let clear = dist_along(loops, point, dir);
            let cap = (2.0 * (clear - wall) / tip_factor).max(0.0);
            sphere.min(cap)
        }
        _ => sphere,
    }
}

/// Auto-place onion connectors across a cross-section. Onions are ALIGNMENT guides, so this seeds a
/// FEW spread across the face rather than a dense fill: `spacing` is the target span BETWEEN onions,
/// so the count scales with the face (a big joint gets more, a small one gets one), placed at
/// cell-CENTRES (so none land on the boundary). Each candidate must sit in SOLID material (inside the
/// outline, outside any hole) with room for a `min_d` onion; it gets the largest teardrop-fitting
/// diameter (`fit_onion` with `cap_dir` + `tip_factor`, capped at `max_d`). The GUI + `auto::plan`
/// both cap each by the slab's axial room. Returns (point, diameter) in connector-pos coords.
pub fn auto_place(
    loops: &[Loop],
    wall: f64,
    max_d: f64,
    spacing: f64,
    min_d: f64,
    cap_dir: Option<[f64; 2]>,
    tip_factor: f64,
) -> Vec<([f64; 2], f64)> {
    if loops.is_empty() || spacing <= 0.0 {
        return Vec::new();
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for lp in loops {
        for p in lp {
            lo[0] = lo[0].min(p[0]);
            lo[1] = lo[1].min(p[1]);
            hi[0] = hi[0].max(p[0]);
            hi[1] = hi[1].max(p[1]);
        }
    }
    // Dense candidate grid at a FINE pitch, so we reliably find solid material even in a cut face
    // full of walls + holes (a sparse grid would miss it — the 0-connector trap). Keep each point in
    // material with room for a `min_d` onion.
    let pitch = (spacing / 6.0).max(min_d.max(1.0));
    let n = [((hi[0] - lo[0]) / pitch).floor() as i64, ((hi[1] - lo[1]) / pitch).floor() as i64];
    let start = [
        (lo[0] + hi[0]) / 2.0 - n[0] as f64 * pitch / 2.0,
        (lo[1] + hi[1]) / 2.0 - n[1] as f64 * pitch / 2.0,
    ];
    let mut cand: Vec<([f64; 2], f64)> = Vec::new();
    for i in 0..=n[0] {
        for j in 0..=n[1] {
            let p = [start[0] + i as f64 * pitch, start[1] + j as f64 * pitch];
            if !point_in_material(loops, p) {
                continue;
            }
            let d = fit_onion(loops, p, wall, max_d, cap_dir, tip_factor);
            if d >= min_d {
                cand.push((p, d));
            }
        }
    }
    // Thin to alignment guides: biggest onions first (the best guides, in the most open material),
    // keeping a candidate only when it clears every kept one by `spacing`. Poisson-disk-style — a few
    // well-separated onions instead of a fill.
    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<([f64; 2], f64)> = Vec::new();
    for (p, d) in cand {
        if out.iter().all(|(q, _)| (p[0] - q[0]).hypot(p[1] - q[1]) >= spacing) {
            out.push((p, d));
        }
    }
    out
}

/// True if `point` is in solid material: inside the outer outline and OUTSIDE any hole. Even-odd
/// ray-crossing over all loop segments (inside the outline = odd crossings; inside a hole flips it
/// back to even = not material), so it's correct for a profile with holes regardless of winding.
fn point_in_material(loops: &[Loop], point: [f64; 2]) -> bool {
    let [px, py] = point;
    let mut inside = false;
    for lp in loops {
        let n = lp.len();
        for i in 0..n {
            let (a, b) = (lp[i], lp[(i + 1) % n]);
            if (a[1] > py) != (b[1] > py) {
                let x = a[0] + (py - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if x > px {
                    inside = !inside;
                }
            }
        }
    }
    inside
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

/// Map a loop's parsed SVG (u, v) to connector-pos coords (the cut's two non-axis dims, ascending).
/// OpenSCAD's SVG export NEGATES Y (SVG's y-axis points down), so the parsed `v` is the negated
/// projected coordinate — undo that here, or the whole profile comes back upside-down and auto-place
/// scatters connectors below the model. Per `projection_scad`: X → (u,v)=(z,-y) so pos=(y,z)=(-v,u);
/// Y → (u,v)=(x,-z) so pos=(x,z)=(u,-v); Z → (u,v)=(x,-y) so pos=(x,y)=(u,-v).
fn map_to_pos(loop_uv: Vec<[f64; 2]>, axis: usize) -> Vec<[f64; 2]> {
    loop_uv.into_iter().map(|[u, v]| if axis == 0 { [-v, u] } else { [u, -v] }).collect()
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
    fn x_axis_maps_uv_to_pos_yz() {
        // X cut: SVG (u,v) = (z, -y) after OpenSCAD's Y-flip; connector pos = (y, z) = (-v, u).
        let mapped = map_to_pos(vec![[3.0, 7.0]], 0);
        assert_eq!(mapped, vec![[-7.0, 3.0]]);
    }

    #[test]
    fn yz_axes_unflip_v() {
        // Y/Z: SVG (u,v) = (first, -second) after the Y-flip; pos = (first, second) = (u, -v).
        assert_eq!(map_to_pos(vec![[3.0, 7.0]], 1), vec![[3.0, -7.0]]);
        assert_eq!(map_to_pos(vec![[3.0, 7.0]], 2), vec![[3.0, -7.0]]);
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
    fn point_in_material_respects_holes() {
        let sq = vec![[-10.0, -10.0], [10.0, -10.0], [10.0, 10.0], [-10.0, 10.0]];
        let hole = vec![[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0]];
        let loops = vec![sq.clone(), hole];
        assert!(point_in_material(&loops, [7.0, 0.0])); // in the wall -> material
        assert!(!point_in_material(&loops, [0.0, 0.0])); // in the hole -> not material
        assert!(!point_in_material(&loops, [20.0, 0.0])); // outside everything
    }

    #[test]
    fn auto_place_fills_material_and_skips_holes() {
        // 40x40 square, hole in the middle. Grid at spacing 10 should place several onions in the
        // material ring and NONE in the central hole.
        let sq = vec![[-20.0, -20.0], [20.0, -20.0], [20.0, 20.0], [-20.0, 20.0]];
        let hole = vec![[-6.0, -6.0], [6.0, -6.0], [6.0, 6.0], [-6.0, 6.0]];
        let placed = auto_place(&[sq, hole], 1.0, 16.0, 10.0, 3.0, None, 1.0);
        assert!(!placed.is_empty(), "should place some connectors");
        // every placement sits in material (not in the hole) and is at least min_d
        for (p, d) in &placed {
            assert!(*d >= 3.0);
            assert!(!(p[0].abs() < 6.0 && p[1].abs() < 6.0), "placed inside the hole: {p:?}");
        }
    }

    #[test]
    fn auto_place_empty_section_is_empty() {
        assert!(auto_place(&[], 1.0, 16.0, 10.0, 3.0, None, 1.0).is_empty());
    }

    #[test]
    fn auto_place_is_a_sparse_spread_not_a_fill() {
        // A 400×250 solid face. At a 120mm alignment span the onions are a handful, spread across the
        // interior — NOT the dozens a fine fill grid would drop.
        let face = vec![vec![[0.0, 0.0], [400.0, 0.0], [400.0, 250.0], [0.0, 250.0]]];
        let pts = auto_place(&face, 1.2, 16.0, 120.0, 3.0, None, 2.9238);
        // A handful of guides thinned to ≥120mm apart — not the dozens a fill would give.
        assert!((3..=10).contains(&pts.len()), "a handful of alignment guides, got {}", pts.len());
        // Every kept pair clears the 120mm alignment span, and they spread across the long axis.
        for a in 0..pts.len() {
            for b in (a + 1)..pts.len() {
                let (p, q) = (pts[a].0, pts[b].0);
                assert!((p[0] - q[0]).hypot(p[1] - q[1]) >= 120.0 - 1e-6, "too close");
            }
        }
        let xs: Vec<f64> = pts.iter().map(|(p, _)| p[0]).collect();
        let (lo, hi) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
        assert!(hi - lo > 200.0, "spread across the face, span {}", hi - lo);
    }

    #[test]
    fn fit_onion_bounds_the_teardrop_tip_toward_the_cap() {
        // 40x40 square; a point 10 below the top edge. The sphere would fit d = 2*(10-1) = 18, but a
        // teardrop with tip 1.5x its radius toward +Y can only be d/2 = (10-1)/1.5 -> d = 12.
        let sq = vec![vec![[-20.0, -20.0], [20.0, -20.0], [20.0, 20.0], [-20.0, 20.0]]];
        let p = [0.0, 10.0]; // 10 from the +Y edge
        assert!((fit_diameter(&sq, p, 1.0, 40.0) - 18.0).abs() < 1e-9);
        let capped = fit_onion(&sq, p, 1.0, 40.0, Some([0.0, 1.0]), 1.5);
        assert!((capped - 12.0).abs() < 1e-6, "cap-bounded d {capped}");
        // no cap direction -> the plain sphere fit
        assert!((fit_onion(&sq, p, 1.0, 40.0, None, 1.5) - 18.0).abs() < 1e-9);
    }

    #[test]
    fn dist_along_hits_the_facing_edge() {
        let sq = vec![vec![[-20.0, -20.0], [20.0, -20.0], [20.0, 20.0], [-20.0, 20.0]]];
        assert!((dist_along(&sq, [0.0, 5.0], [0.0, 1.0]) - 15.0).abs() < 1e-9); // up to +Y edge
        assert!((dist_along(&sq, [0.0, 5.0], [0.0, -1.0]) - 25.0).abs() < 1e-9); // down to -Y edge
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
