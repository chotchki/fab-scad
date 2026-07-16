//! The pure onion-JOINT feasibility + slab math (W.3.4) — no `Solid`, so it lives under `geometry`
//! and compiles on the wasm fab-gui app (which flags joint downgrades live, in-process). Extracted
//! from `slicing` so that module stays honestly `kernel`-gated for its Solid-coupled codegen while
//! this predicate + the shared slab helpers ride the seam. The kernel slicer imports the same
//! helpers, so the downgrade flag the GUI shows and the joint the slice carves run through ONE path
//! and never disagree.

use anyhow::{Context, Result};
use fab_lang::VecExt;

use crate::manifest::{Connector, Slicing};
use fab_lang::Vec3;

// --- onion orientation gate (#40) -----------------------------------------------------------
// Tunable; the geometric ideal is 45°, refined by a printed coupon (Phase A). See the
// connector-orientation design memory for the derivation.
const SUPPORT_ANGLE: f64 = 45.0; // overhang threshold, degrees from vertical
const CAP_ANG_MIN: f64 = 20.0; // pointiest onion cap we'll print (BOSL2 ang is from vertical)
const CAP_SAFETY: f64 = 0.0; // extra socket margin; 0 keeps the aligned case at today's ang=45

/// The shared onion cap axis + cap angle for one joint, or Infeasible (→ downgrade to a bolt).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OnionAxis {
    Feasible { cap: Vec3, ang: f64 },
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
pub(crate) fn onion_axis(u_lo: Vec3, u_up: Vec3) -> OnionAxis {
    let cap = u_lo; // peg-priority: the proud bump follows the lower build, always support-free
    let tilt = cap.angle_deg(u_up); // socket-ceiling tilt off the upper build
    let budget = SUPPORT_ANGLE - CAP_ANG_MIN - CAP_SAFETY; // tilt the steepest printable cap absorbs
    if tilt >= 180.0 - budget {
        return OnionAxis::Feasible {
            cap,
            ang: SUPPORT_ANGLE,
        }; // cap points away → bowl, no ceiling
    }
    if tilt > budget {
        return OnionAxis::Infeasible; // ceiling overhangs even at the steepest printable cap
    }
    let ang = (SUPPORT_ANGLE - tilt - CAP_SAFETY).clamp(CAP_ANG_MIN, SUPPORT_ANGLE);
    OnionAxis::Feasible { cap, ang }
}

/// Cut positions grouped by axis, each ascending — the shared prep for the driver, per-piece
/// codegen, and the feasibility query (`slice()` and the slab math both need sorted cuts).
pub(crate) fn axes_sorted(s: &Slicing) -> Result<[Vec<f64>; 3]> {
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
            Ok(matches!(
                onion_resolution(s, &by_axis, c)?,
                OnionAxis::Feasible { .. }
            ))
        })
        .collect()
}

/// Slab index of `coord` among `sorted_cuts` (cuts strictly below it). For a cut's own position
/// this is the LOWER piece's index on that axis; the upper piece is +1.
pub(crate) fn slab_index(sorted_cuts: &[f64], coord: f64) -> usize {
    sorted_cuts.iter().filter(|&&x| x < coord - 1e-6).count()
}

/// A piece's build-up, keyed by (slab, connected-COMPONENT) — U.3.14 Phase G. A component-specific
/// override wins (a presliced blob's comp `k` orients on its own); else the slab-level orient (comp 0)
/// applies to every component of that slab; else +Z (as-modeled). The onion/bolt carve queries comp 0
/// (a cut slab is one component), the 3mf co-pack queries each real component.
pub(crate) fn piece_up(s: &Slicing, mi: [usize; 3], comp: usize) -> Vec3 {
    let at = |c: usize| s.orient.iter().find(|p| p.piece == mi && p.comp == c);
    at(comp)
        .or_else(|| at(0))
        .map(|p| Vec3::new(p.up[0].f(), p.up[1].f(), p.up[2].f()).normalize_or_self())
        .unwrap_or(Vec3::new(0.0, 0.0, 1.0))
}

/// Resolve one onion connector to its cap axis/angle (or Infeasible) from its two bordering
/// pieces' orientations. `by_axis` holds the sorted enabled cuts per axis (for slab lookup).
pub(crate) fn onion_resolution(
    s: &Slicing,
    by_axis: &[Vec<f64>; 3],
    c: &Connector,
) -> Result<OnionAxis> {
    let cut = s.cut.get(c.cut).with_context(|| {
        format!(
            "connector references cut {}, but there are {} cut(s)",
            c.cut,
            s.cut.len()
        )
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
    // The onion joins two CUT slabs — each a single component — so gate on comp 0.
    Ok(onion_axis(piece_up(s, lo, 0), piece_up(s, up, 0)))
}
