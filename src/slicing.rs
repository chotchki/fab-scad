//! Codegen for the slicing driver (5.2): turn a `[slicing]` spec into a `slice()`/connector
//! `.scad` that `fab slice` renders. Pure string-building — the IO (freeze source, render)
//! lives in `slice_cmd`. This is the GUI ↔ fab contract: the GUI edits the spec, this
//! reproduces the same SCAD headlessly, so preview and `fab slice` are one path.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::geom::{self, V3};
use crate::manifest::{Connector, Slicing};
use crate::openscad::Openscad;

const AXIS: [&str; 3] = ["RIGHT", "BACK", "UP"];

// --- onion orientation gate (#40) -----------------------------------------------------------
// Tunable; the geometric ideal is 45°, refined by a printed coupon (Phase A). See the
// connector-orientation design memory for the derivation.
const SUPPORT_ANGLE: f64 = 45.0; // overhang threshold, degrees from vertical
const CAP_ANG_MIN: f64 = 20.0; // pointiest onion cap we'll print (BOSL2 ang is from vertical)
const CAP_SAFETY: f64 = 0.0; // extra socket margin; 0 keeps the aligned case at today's ang=45

/// The shared onion cap axis + cap angle for one joint, or Infeasible (→ downgrade to a bolt).
#[derive(Debug, Clone, Copy, PartialEq)]
enum OnionAxis {
    Feasible { cap: V3, ang: f64 },
    Infeasible,
}

/// Derive the ONE shared onion cap axis + angle from the two bordering pieces' build-ups `u_lo`
/// (peg piece, below the cut) and `u_up` (socket piece, above). The onion is CUT IN HALF by the cut
/// plane — one half stands proud as the peg, the matching half is carved as the socket — so the CUT
/// axis is irrelevant to printability; only the cap-vs-build angle is. (chotchki: a Y-cut onion with
/// the cap along +Z, sliced in half on the Y plane, is the best possible use of the onion.)
/// - PEG (proud bump, lower piece): the cap follows `u_lo`, so the teardrop narrows going up and
///   prints support-free in ANY orientation. That fixes the cap; the peg never limits feasibility.
/// - SOCKET (cavity, upper piece): the cap is the void's CEILING. Fine when the cap tilts little off
///   +u_up (steepen `ang` to clear it); fine again when it points well AWAY (the cavity opens up — a
///   bowl, no ceiling). The band between is where the ceiling overhangs → downgrade to a bolt.
fn onion_axis(u_lo: V3, u_up: V3) -> OnionAxis {
    let cap = u_lo; // peg-priority: the proud bump follows the lower build, always support-free
    let tilt = geom::angle_deg(cap, u_up); // socket-ceiling tilt off the upper build
    let budget = SUPPORT_ANGLE - CAP_ANG_MIN - CAP_SAFETY; // tilt the steepest printable cap absorbs
    if tilt >= 180.0 - budget {
        return OnionAxis::Feasible { cap, ang: SUPPORT_ANGLE }; // cap points away → bowl, no ceiling
    }
    if tilt > budget {
        return OnionAxis::Infeasible; // ceiling overhangs even at the steepest printable cap
    }
    let ang = (SUPPORT_ANGLE - tilt - CAP_SAFETY).clamp(CAP_ANG_MIN, SUPPORT_ANGLE);
    OnionAxis::Feasible { cap, ang }
}

/// Cut positions grouped by axis, each ascending — the shared prep for the driver, per-piece
/// codegen, and the feasibility query (`slice()` and the slab math both need sorted cuts).
fn axes_sorted(s: &Slicing) -> Result<[Vec<f64>; 3]> {
    let mut by_axis: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for c in &s.cut {
        by_axis[c.axis_index()?].push(c.at());
    }
    for v in by_axis.iter_mut() {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    Ok(by_axis)
}

/// Per-connector onion feasibility under the spec's orientations, index-aligned with `s.connector`:
/// `true` = the onion prints support-free for both bordering pieces, `false` = its orientation gate
/// failed and `driver_scad` downgrades it to a bolt. Non-onion connectors are `true` (nothing to
/// downgrade). The GUI's joint-downgrade flag runs through THIS — same gate the slice applies, so
/// the flag the user sees and the joint the slice carves never disagree.
pub fn onion_feasibility(s: &Slicing) -> Result<Vec<bool>> {
    let by_axis = axes_sorted(s)?;
    s.connector
        .iter()
        .map(|c| {
            if c.kind != "onion" {
                return Ok(true);
            }
            Ok(matches!(onion_resolution(s, &by_axis, c)?, OnionAxis::Feasible { .. }))
        })
        .collect()
}

/// Slab index of `coord` among `sorted_cuts` (cuts strictly below it). For a cut's own position
/// this is the LOWER piece's index on that axis; the upper piece is +1.
fn slab_index(sorted_cuts: &[f64], coord: f64) -> usize {
    sorted_cuts.iter().filter(|&&x| x < coord - 1e-6).count()
}

/// A piece's build-up: a manual override from the spec, else +Z (auto-orient fills this in #42/D).
fn piece_up(s: &Slicing, mi: [usize; 3]) -> V3 {
    s.orient
        .iter()
        .find(|p| p.piece == mi)
        .map(|p| geom::normalize([p.up[0].f(), p.up[1].f(), p.up[2].f()]))
        .unwrap_or([0.0, 0.0, 1.0])
}

/// Resolve one onion connector to its cap axis/angle (or Infeasible) from its two bordering
/// pieces' orientations. `by_axis` holds the sorted enabled cuts per axis (for slab lookup).
fn onion_resolution(s: &Slicing, by_axis: &[Vec<f64>; 3], c: &Connector) -> Result<OnionAxis> {
    let cut = s.cut.get(c.cut).with_context(|| {
        format!("connector references cut {}, but there are {} cut(s)", c.cut, s.cut.len())
    })?;
    let axis = cut.axis_index()?;
    let others: Vec<usize> = (0..3).filter(|&x| x != axis).collect();
    let k = slab_index(&by_axis[axis], cut.at()); // lower piece's index on the cut axis
    let mut lo = [0usize; 3];
    lo[axis] = k;
    lo[others[0]] = slab_index(&by_axis[others[0]], c.pos[0].f());
    lo[others[1]] = slab_index(&by_axis[others[1]], c.pos[1].f());
    let mut up = lo;
    up[axis] = k + 1;
    Ok(onion_axis(piece_up(s, lo), piece_up(s, up)))
}

/// Freeze `source` to a mesh, generate the slicer driver from `spec`, render the pieces.
/// Returns the sliced STL path. The shared slice flow — `fab slice` and the GUI both call it.
pub fn slice_part(
    oscad: &Openscad,
    source: &Path,
    spec: &Slicing,
    spread: f64,
    out_dir: &Path,
    timeout: Duration,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // Freeze the source to a mesh (slicing the frozen STL stays linear — no 2^N).
    let source_stl = out_dir.join(format!("{stem}.stl"));
    let f = oscad.render(source, &source_stl, timeout)?;
    if !f.ok {
        bail!("source render failed: {}", source.display());
    }

    // Generate the driver from the spec (imports the frozen mesh by name) and render it.
    let driver = driver_scad(spec, &format!("{stem}.stl"), spread)?;
    let driver_path = out_dir.join(format!("{stem}-sliced.scad"));
    std::fs::write(&driver_path, driver)
        .with_context(|| format!("writing {}", driver_path.display()))?;
    let sliced = out_dir.join(format!("{stem}-sliced.stl"));
    let r = oscad.render(&driver_path, &sliced, timeout)?;
    if !r.ok {
        bail!("slice render failed");
    }
    Ok(sliced)
}

/// Format a coordinate without a trailing `.0` for whole numbers (tidy generated SCAD).
fn n(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Generate the driver: nested `slice()` per axis around a `diff()` of the imported source
/// minus the connectors. `source` is the import path, relative to the driver file.
pub fn driver_scad(s: &Slicing, source: &str, spread: f64) -> Result<String> {
    let by_axis = axes_sorted(s)?; // slice() requires ascending cuts

    let mut slices = String::new();
    for (ax, cuts) in by_axis.iter().enumerate() {
        if !cuts.is_empty() {
            let list = cuts.iter().map(|&x| n(x)).collect::<Vec<_>>().join(", ");
            // Onion joints ride slice()'s per-piece `connectors`; bolt/pin stay in the diff below.
            let onions = onion_param(s, ax, &by_axis)?;
            slices += &format!(
                "slice([{list}], axis = {}, spread = {}, connectors = {onions})\n",
                AXIS[ax],
                n(spread)
            );
        }
    }
    if slices.is_empty() {
        bail!("[slicing] has no cuts");
    }

    // `force_tag()` pulls the raw `import()` mesh into BOSL2's tag system — without it `diff()`
    // doesn't see the import as keep geometry and the connectors don't carve (BOSL2's own
    // attachable primitives are tagged automatically; `import()` is not).
    let mut body = String::from("tag_scope() diff() {\n");
    body += &format!("    force_tag() import(\"{source}\");\n");
    for c in &s.connector {
        // bolt/pin go here as-is; a feasible onion rides slice()'s connectors param (skip it here);
        // an INFEASIBLE onion downgrades to a bolt in the diff (connector_line handles "onion").
        let feasible_onion = c.kind == "onion"
            && matches!(onion_resolution(s, &by_axis, c)?, OnionAxis::Feasible { .. });
        if !feasible_onion {
            body += &connector_line(s, c)?;
        }
    }
    body += "}\n";

    Ok(format!(
        "// generated by `fab slice` from project.toml [slicing] — edits go in the spec, not here.\n\
         include <slicer.scad>\n\
         include <connectors.scad>\n\n\
         {slices}{body}"
    ))
}

/// Every piece's slab multi-index: the cartesian product of `0..(cuts_on_axis + 1)` per axis (an
/// axis with no cuts contributes only index 0). Iteration is x-outer → z-inner, and each axis runs
/// in ascending-cut order — the SAME order `piece_driver` sorts by — so a returned `[ix, iy, iz]`
/// selects exactly the slab that `piece_driver`/`slice(only=)` would carve. Feeds the per-piece
/// render sweep (auto-orient #42, print-orientation preview).
pub fn piece_indices(s: &Slicing) -> Result<Vec<[usize; 3]>> {
    let mut slabs = [1usize; 3]; // an uncut axis is one slab
    for c in &s.cut {
        slabs[c.axis_index()?] += 1;
    }
    let mut out = Vec::with_capacity(slabs[0] * slabs[1] * slabs[2]);
    for ix in 0..slabs[0] {
        for iy in 0..slabs[1] {
            for iz in 0..slabs[2] {
                out.push([ix, iy, iz]);
            }
        }
    }
    Ok(out)
}

/// Codegen for ONE piece (no spread): nested `slice(only=)` per axis around the imported source,
/// each carrying its axis's FEASIBLE onions so the piece shows its real joints — the peg unioned in
/// when this piece is below a connector's cut, the socket diffed out when above (the slicer decides
/// per piece). `s` with no connectors gives the bare piece (auto-orient overhang scoring #42); `s`
/// with onions gives the print-orientation preview's joined piece. `piece` is the slab multi-index;
/// an axis with no cuts must be index 0.
pub fn piece_driver(s: &Slicing, source: &str, piece: [usize; 3]) -> Result<String> {
    let by_axis = axes_sorted(s)?;
    let mut slices = String::new();
    for (ax, cuts) in by_axis.iter().enumerate() {
        if cuts.is_empty() {
            if piece[ax] != 0 {
                bail!("piece index {} on axis {ax} but that axis has no cuts", piece[ax]);
            }
            continue;
        }
        let list = cuts.iter().map(|&x| n(x)).collect::<Vec<_>>().join(", ");
        let onions = onion_param(s, ax, &by_axis)?;
        slices += &format!(
            "slice([{list}], axis = {}, only = {}, connectors = {onions})\n",
            AXIS[ax], piece[ax]
        );
    }
    if slices.is_empty() {
        bail!("[slicing] has no cuts");
    }
    Ok(format!(
        "// generated by `fab` for a single piece (orientation / print preview render).\n\
         include <slicer.scad>\n\n\
         {slices}import(\"{source}\");\n"
    ))
}

/// The FEASIBLE onion connectors on `axis` as a SCAD list `[[cut_pos, a, b, d, ox, oy, oz, ang],
/// ...]` for `slice()`'s `connectors` param — applied per piece (peg into the lower, socket out of
/// the upper). `(ox,oy,oz)` is the cap axis + `ang` its cap angle, DERIVED per joint from the two
/// bordering pieces' print orientations (`onion_axis`). Infeasible onions are omitted here and
/// downgraded to a bolt in the diff body. `by_axis` = sorted enabled cuts per axis, for piece lookup.
fn onion_param(s: &Slicing, axis: usize, by_axis: &[Vec<f64>; 3]) -> Result<String> {
    let mut items = Vec::new();
    for c in s.connector.iter().filter(|c| c.kind == "onion") {
        let cut = s.cut.get(c.cut).with_context(|| {
            format!("connector references cut {}, but there are {} cut(s)", c.cut, s.cut.len())
        })?;
        if cut.axis_index()? != axis {
            continue;
        }
        if let OnionAxis::Feasible { cap, ang } = onion_resolution(s, by_axis, c)? {
            items.push(format!(
                "[{}, {}, {}, {}, {}, {}, {}, {}]",
                n(cut.at()),
                n(c.pos[0].f()),
                n(c.pos[1].f()),
                n(c.size.unwrap_or(10.0)),
                n(cap[0]),
                n(cap[1]),
                n(cap[2]),
                n(ang)
            ));
        }
    }
    Ok(format!("[{}]", items.join(", ")))
}

/// One `tag("remove") <connector>` line, positioned on its cut plane and oriented along the
/// cut axis (so it slices into both pieces correctly).
fn connector_line(s: &Slicing, c: &Connector) -> Result<String> {
    let cut = s.cut.get(c.cut).with_context(|| {
        format!("connector references cut {}, but there are {} cut(s)", c.cut, s.cut.len())
    })?;
    let ai = cut.axis_index()?;
    // Point = `at` along the cut axis, `pos` in the two perpendicular dims.
    let mut p = [0.0_f64; 3];
    p[ai] = cut.at();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    p[others[0]] = c.pos[0].f();
    p[others[1]] = c.pos[1].f();

    let conn = match c.kind.as_str() {
        // An onion that can't print support-free for both pieces downgrades to a bolt here
        // (its halves orient independently) — chotchki's pick for the infeasible case.
        "bolt" | "onion" => format!(
            "bolt_joint(\"{}\", through = {}, orient = {})",
            c.screw.as_deref().unwrap_or("M3"),
            n(c.through.unwrap_or(12.0)),
            AXIS[ai]
        ),
        other => bail!("connector type must be 'bolt' or 'onion', got '{other}'"),
    };
    Ok(format!(
        "    translate([{}, {}, {}]) tag(\"remove\") {conn};\n",
        n(p[0]),
        n(p[1]),
        n(p[2])
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn spec(toml: &str) -> Slicing {
        let m: Manifest = ::toml::from_str(toml).unwrap();
        m.slicing.unwrap()
    }

    #[test]
    fn cuts_group_by_axis_and_sort() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        assert!(d.contains("slice([-10, 25], axis = RIGHT, spread = 0, connectors = [])"), "{d}");
        // force_tag() is load-bearing: without it diff() won't carve connectors from the import.
        assert!(d.contains("force_tag() import(\"t.stl\")"), "{d}");
        assert!(d.contains("tag_scope() diff()"));
    }

    #[test]
    fn bolt_connector_positioned_and_oriented() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"bolt\"\nscrew=\"M4\"\npos=[5,-3]\nthrough=15\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        // cut on Z -> point is (pos.x, pos.y, at), oriented UP
        assert!(
            d.contains("translate([5, -3, 0]) tag(\"remove\") bolt_joint(\"M4\", through = 15, orient = UP)"),
            "{d}"
        );
    }

    fn deg(d: f64) -> f64 {
        d.to_radians()
    }

    #[test]
    fn onion_axis_aligned_case_matches_today() {
        // both pieces build +Z: cap = +Z, ang = 45 (identical to pre-orientation output).
        match onion_axis([0.0, 0.0, 1.0], [0.0, 0.0, 1.0]) {
            OnionAxis::Feasible { cap, ang } => {
                assert!((cap[2] - 1.0).abs() < 1e-9 && cap[0].abs() < 1e-9);
                assert!((ang - 45.0).abs() < 1e-9);
            }
            _ => panic!("aligned case must be feasible"),
        }
    }

    #[test]
    fn onion_infeasible_when_the_two_pieces_build_90_apart() {
        // peg piece builds +X, socket piece builds +Z: no single cap serves both — the socket
        // ceiling sits at a 90° overhang. The CUT axis is irrelevant; only the build mismatch is.
        assert_eq!(onion_axis([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]), OnionAxis::Infeasible);
    }

    #[test]
    fn onion_socket_steepens_cap_for_a_tilted_upper_piece() {
        // upper piece tilted 20° from the lower's +Z. cap stays +Z (the peg), ang shrinks to clear it.
        let u_up = [deg(20.0).sin(), 0.0, deg(20.0).cos()];
        match onion_axis([0.0, 0.0, 1.0], u_up) {
            OnionAxis::Feasible { ang, .. } => assert!((ang - 25.0).abs() < 0.5, "ang {ang}"),
            _ => panic!("20° upper tilt should be feasible with a steeper cap"),
        }
        // 30° upper tilt exceeds the cap budget (45-CAP_ANG_MIN=25) -> infeasible.
        let steep = [deg(30.0).sin(), 0.0, deg(30.0).cos()];
        assert_eq!(onion_axis([0.0, 0.0, 1.0], steep), OnionAxis::Infeasible);
    }

    #[test]
    fn onion_socket_bowl_up_is_always_feasible() {
        // upper piece builds opposite the lower (-Z vs +Z): the socket opens upward, no ceiling.
        match onion_axis([0.0, 0.0, 1.0], [0.0, 0.0, -1.0]) {
            OnionAxis::Feasible { ang, .. } => assert!((ang - 45.0).abs() < 1e-9),
            _ => panic!("bowl-up socket must be feasible"),
        }
    }

    #[test]
    fn onion_on_x_cut_default_up_is_feasible() {
        // X cut, both pieces default +Z: the onion is sliced in half on the cut plane, cap +Z, and
        // prints support-free — the cut axis doesn't matter. It rides slice()'s connectors, no bolt.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = driver_scad(&s, "t.stl", 30.0).unwrap();
        assert!(d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"), "onion rides the slice: {d}");
        assert!(!d.contains("bolt_joint("), "feasible onion is NOT downgraded: {d}");
    }

    #[test]
    fn orientation_override_on_the_lower_piece_can_force_a_downgrade() {
        // Z cut; override the LOWER piece [0,0,0] to build +X while the upper stays +Z -> the two
        // pieces build 90° apart, no shared cap -> downgrade. Exercises the override -> piece_up ->
        // slab-lookup -> gate path.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=12\n\
             [[slicing.orient]]\npiece=[0,0,0]\nup=[1,0,0]\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        assert!(d.contains("connectors = []"), "override forces infeasible: {d}");
        assert!(d.contains("bolt_joint("), "downgraded to bolt: {d}");
    }

    #[test]
    fn onion_rides_the_slice_param_not_the_diff() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = driver_scad(&s, "t.stl", 30.0).unwrap();
        // Z cut -> others (x,y); onion enters slice()'s connectors param as [at,a,b,d,ox,oy,oz,ang],
        // cap axis = the cut axis (+Z) at Phase B.
        assert!(d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"), "{d}");
        // ...and is NOT emitted as a pre-slice remove in the diff body.
        assert!(!d.contains("onion_"), "{d}");
    }

    #[test]
    fn piece_driver_isolates_one_slab_per_axis() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n",
        );
        let d = piece_driver(&s, "m.stl", [1, 0, 1]).unwrap();
        // bare spec -> per-axis slice(only=) with no onions
        assert!(d.contains("slice([-10, 25], axis = RIGHT, only = 1, connectors = [])"), "{d}");
        assert!(d.contains("slice([0], axis = UP, only = 1, connectors = [])"), "{d}");
        assert!(d.contains("import(\"m.stl\")"), "{d}");
        // an axis with no cuts must be index 0
        assert!(piece_driver(&s, "m.stl", [0, 1, 0]).is_err());
    }

    #[test]
    fn piece_driver_carves_a_feasible_onion_into_the_piece() {
        // Z cut + onion, both pieces default +Z -> feasible. The piece slice carries the onion so
        // the slicer unions the peg (lower) / diffs the socket (upper); the preview shows the joint.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = piece_driver(&s, "m.stl", [0, 0, 0]).unwrap();
        assert!(d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"), "{d}");
    }

    #[test]
    fn onion_feasibility_flags_the_downgrade() {
        // Both pieces default +Z, so an onion is feasible on ANY cut (Z and X alike) — sliced in
        // half on the cut plane, cap +Z, support-free -> [true, true], index-aligned with the list.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=10\n\
             [[slicing.connector]]\ncut=1\ntype=\"onion\"\npos=[0,0]\nsize=10\n",
        );
        assert_eq!(onion_feasibility(&s).unwrap(), vec![true, true]);
        // The downgrade now comes from a build MISMATCH: an override that tilts the lower piece 90°
        // off the upper leaves no shared cap.
        let tilted = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=10\n\
             [[slicing.orient]]\npiece=[0,0,0]\nup=[1,0,0]\n",
        );
        assert_eq!(onion_feasibility(&tilted).unwrap(), vec![false]);
    }

    #[test]
    fn piece_indices_are_the_axis_slab_product() {
        // 2 X cuts (3 X slabs) + 1 Z cut (2 Z slabs), no Y cuts (1 Y slab) -> 3*1*2 = 6 pieces.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n",
        );
        let pieces = piece_indices(&s).unwrap();
        assert_eq!(pieces.len(), 6);
        assert!(pieces.contains(&[0, 0, 0]) && pieces.contains(&[2, 0, 1]));
        // every Y index is 0 (no Y cuts), and no index exceeds its axis's slab count
        assert!(pieces.iter().all(|p| p[1] == 0 && p[0] < 3 && p[2] < 2));
        // an uncut model is a single piece
        let none = spec("[project]\nname=\"t\"\n[slicing]\n");
        assert_eq!(piece_indices(&none).unwrap(), vec![[0, 0, 0]]);
    }

    #[test]
    fn spread_threads_through() {
        let s = spec("[project]\nname=\"t\"\n[slicing]\n[[slicing.cut]]\naxis=\"y\"\nat=0\n");
        let d = driver_scad(&s, "t.stl", 40.0).unwrap();
        assert!(d.contains("axis = BACK, spread = 40"), "{d}");
    }

    #[test]
    fn no_cuts_errors() {
        let s = spec("[project]\nname=\"t\"\n[slicing]\n");
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }

    #[test]
    fn bad_connector_cut_index_errors() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=5\ntype=\"bolt\"\n",
        );
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }

    #[test]
    fn retired_pin_connector_type_errors() {
        // pin/dowel was retired (the onion replaced the glued peg); bolt + onion remain.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"pin\"\n",
        );
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }
}
