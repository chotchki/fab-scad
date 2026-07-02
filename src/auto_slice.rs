//! Auto-slice (Phase 13): partition a model too big to print into pieces that each fit the printer
//! bed. The v1 heuristic is a bounding-box partition — for every axis the model overflows the bed on,
//! cut it into `ceil(extent / bed)` EQUAL parts (equal so there are no thin slivers), and cut ONLY
//! the overflowing axes (single-axis when just one dimension is too big — keeps cuts from gridding
//! into the jigsaw-intersection mess chotchki wants to avoid). Pure bbox math, so `fab make` and the
//! GUI compute the same cuts.
//!
//! [`best_fit_rotation`] (v2, Phase 17) closes the rotate-to-fit gap: it spins the model to the
//! fewest bed pieces (trying ±45° about each axis) BEFORE this partitions the resulting bbox — so a
//! diagonal part lines up with an axis instead of over-cutting. `fab make` applies it.
//!
//! Known limits (deliberate, documented rather than hidden):
//!   - **bbox-based**: cuts the whole grid even through empty space — an L-model's empty cells just
//!     drop out downstream (the slicer discards empty pieces).
//!   - **feature-blind placement**: equal division doesn't dodge thin features or spots a connector
//!     can't seat. Cuts land on the even grid; a v2 could nudge them.
//!   - **coarse rotation set**: rotate-to-fit tries a fixed ±45°/axis set, not a continuous search —
//!     enough for the common diagonal case, but not an optimal orientation solver.

/// A planned cut: which axis (0 = X, 1 = Y, 2 = Z) and its position along that axis (model coords).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoCut {
    pub axis: usize,
    pub at: f64,
}

/// Cuts that partition the model bbox `[min, max]` into cells each fitting `bed` = `[x, y, z]` mm.
/// Per axis: if the extent overflows the matching bed dimension, cut into `ceil(extent / bed)` equal
/// slabs (`n − 1` interior planes); otherwise leave it whole. Empty when the model already fits.
pub fn auto_slice(min: [f64; 3], max: [f64; 3], bed: [f64; 3]) -> Vec<AutoCut> {
    let mut cuts = Vec::new();
    for axis in 0..3 {
        let extent = max[axis] - min[axis];
        let bed_dim = bed[axis];
        // Skip a degenerate bed dim, and don't cut an axis that already fits (small epsilon so an
        // exact-fit extent isn't split by float noise).
        if bed_dim <= 0.0 || extent <= bed_dim + 1e-6 {
            continue;
        }
        let n = (extent / bed_dim).ceil() as usize; // pieces along this axis (≥ 2 here)
        let step = extent / n as f64;
        for k in 1..n {
            cuts.push(AutoCut { axis, at: min[axis] + step * k as f64 });
        }
    }
    cuts
}

/// The piece count `auto_slice` produces for `[min, max]` on `bed` — the product of per-axis slab
/// counts. Handy for a "this will make N pieces across M plates" read-out before committing.
pub fn piece_count(min: [f64; 3], max: [f64; 3], bed: [f64; 3]) -> usize {
    (0..3)
        .map(|axis| {
            let extent = max[axis] - min[axis];
            if bed[axis] <= 0.0 || extent <= bed[axis] + 1e-6 {
                1
            } else {
                (extent / bed[axis]).ceil() as usize
            }
        })
        .product()
}

/// The rotation rotate-to-fit chose: the matrix to spin the model by before slicing, plus the
/// resulting bbox + piece count. `rot` is column-major 3×4 for [`crate::kernel::Solid::transform`]
/// (identity when no spin beats leaving it alone).
#[cfg(feature = "kernel")]
#[derive(Debug, Clone, Copy)]
pub struct FitRotation {
    pub rot: [f64; 12],
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub pieces: usize,
}

/// Rotate-to-fit: pick the model orientation that needs the FEWEST bed-fit pieces. A long part lying
/// diagonally in the model frame gets over-cut axis-aligned; spinning it to line up with an axis can
/// shrink its footprint below the bed. We try identity + ±45° about each axis (chotchki's "does 45°
/// reduce cuts" — where the win is), rotating the ACTUAL geometry (not just the bbox, or a compact
/// part's AABB would only ever grow) and scoring each by [`piece_count`]. Rotate ONLY when it strictly
/// reduces pieces (ties keep identity — no needless spin); among spins that tie on pieces, the tighter
/// bbox wins. The pieces come out in the rotated frame and re-orient per-piece for printing anyway, so
/// the model's original orientation doesn't matter downstream.
#[cfg(feature = "kernel")]
pub fn best_fit_rotation(base: &crate::kernel::Solid, bed: [f64; 3]) -> FitRotation {
    let rad = std::f64::consts::FRAC_PI_4; // 45°
    let mut candidates = vec![identity()];
    for &a in &[rad, -rad] {
        candidates.push(rot_x(a));
        candidates.push(rot_y(a));
        candidates.push(rot_z(a));
    }
    let vol = |min: [f64; 3], max: [f64; 3]| (max[0] - min[0]) * (max[1] - min[1]) * (max[2] - min[2]);
    // Seed with identity so a spin must EARN the swap.
    let mut best: Option<FitRotation> = None;
    for rot in candidates {
        let Some((min, max)) = base.transform(&rot).bbox() else { continue };
        let pieces = piece_count(min, max, bed);
        let cand = FitRotation { rot, min, max, pieces };
        let take = match best {
            None => true,
            Some(b) => pieces < b.pieces || (pieces == b.pieces && vol(min, max) + 1e-6 < vol(b.min, b.max)),
        };
        if take {
            best = Some(cand);
        }
    }
    best.expect("at least identity is always a candidate")
}

/// Column-major 3×4 rotation matrices for `Solid::transform` (columns = images of e_x, e_y, e_z, then
/// a zero translation).
#[cfg(feature = "kernel")]
fn identity() -> [f64; 12] {
    [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.]
}
#[cfg(feature = "kernel")]
fn rot_x(a: f64) -> [f64; 12] {
    let (s, c) = a.sin_cos();
    [1., 0., 0., 0., c, s, 0., -s, c, 0., 0., 0.]
}
#[cfg(feature = "kernel")]
fn rot_y(a: f64) -> [f64; 12] {
    let (s, c) = a.sin_cos();
    [c, 0., -s, 0., 1., 0., s, 0., c, 0., 0., 0.]
}
#[cfg(feature = "kernel")]
fn rot_z(a: f64) -> [f64; 12] {
    let (s, c) = a.sin_cos();
    [c, s, 0., -s, c, 0., 0., 0., 1., 0., 0., 0.]
}

#[cfg(test)]
mod tests {
    use super::*;

    const BED: [f64; 3] = [256.0, 256.0, 256.0];

    #[test]
    fn model_within_bed_makes_no_cuts() {
        assert!(auto_slice([0.0; 3], [200.0, 200.0, 200.0], BED).is_empty());
        assert_eq!(piece_count([0.0; 3], [200.0, 200.0, 200.0], BED), 1);
    }

    #[test]
    fn one_overflowing_axis_one_cut() {
        // 500 on X, bed 256 → 2 equal pieces → a single cut at the midpoint. Y/Z untouched.
        let cuts = auto_slice([0.0; 3], [500.0, 100.0, 50.0], BED);
        assert_eq!(cuts, vec![AutoCut { axis: 0, at: 250.0 }]);
        assert_eq!(piece_count([0.0; 3], [500.0, 100.0, 50.0], BED), 2);
    }

    #[test]
    fn equal_division_leaves_no_sliver() {
        // 600 on X, bed 256 → ceil(600/256)=3 equal 200mm pieces → cuts at 200 and 400 (NOT 256+256+88).
        let cuts = auto_slice([0.0; 3], [600.0, 100.0, 50.0], BED);
        assert_eq!(cuts, vec![AutoCut { axis: 0, at: 200.0 }, AutoCut { axis: 0, at: 400.0 }]);
    }

    #[test]
    fn two_overflowing_axes_grid() {
        // 400×300 footprint on a 256 bed → 1 cut on X (2 pieces), 1 on Y (2 pieces) → a 2×2 grid.
        let cuts = auto_slice([0.0; 3], [400.0, 300.0, 50.0], BED);
        assert_eq!(cuts.iter().filter(|c| c.axis == 0).count(), 1);
        assert_eq!(cuts.iter().filter(|c| c.axis == 1).count(), 1);
        assert_eq!(cuts.iter().filter(|c| c.axis == 2).count(), 0, "Z fits, no Z cut");
        assert_eq!(piece_count([0.0; 3], [400.0, 300.0, 50.0], BED), 4);
    }

    #[test]
    fn cuts_offset_by_model_min() {
        // A model not at the origin: cuts are placed in model coords (min + step·k), not from zero.
        let cuts = auto_slice([-100.0, 0.0, 0.0], [400.0, 100.0, 50.0], BED);
        // extent 500 on X → 2 pieces → cut at min + 250 = 150.
        assert_eq!(cuts, vec![AutoCut { axis: 0, at: 150.0 }]);
    }

    #[test]
    fn exact_bed_fit_is_not_cut() {
        assert!(auto_slice([0.0; 3], [256.0, 256.0, 256.0], BED).is_empty());
    }

    #[test]
    #[cfg(feature = "kernel")]
    fn rotate_to_fit_spins_a_diagonal_bar_into_fewer_pieces() {
        use crate::kernel::Solid;
        // A 400×20×20 bar lying at 45° in XY: its footprint bloats to ~297×297 → 2×2 = 4 pieces
        // axis-aligned. rotate-to-fit should spin it back to the 400×20×20 orientation → 2 pieces.
        let bar = Solid::cube(400.0, 20.0, 20.0, true).transform(&rot_z(std::f64::consts::FRAC_PI_4));
        let (min, max) = bar.bbox().unwrap();
        assert_eq!(piece_count(min, max, BED), 4, "diagonal bar over-cuts axis-aligned");
        let fit = best_fit_rotation(&bar, BED);
        assert_eq!(fit.pieces, 2, "rotate-to-fit spins it back to 2 pieces");
        assert!(fit.rot != identity(), "it chose a non-identity spin");
    }

    #[test]
    #[cfg(feature = "kernel")]
    fn rotate_to_fit_leaves_a_fitting_part_alone() {
        use crate::kernel::Solid;
        let cube = Solid::cube(200.0, 200.0, 200.0, true);
        let fit = best_fit_rotation(&cube, BED);
        assert_eq!(fit.pieces, 1);
        assert_eq!(fit.rot, identity(), "no needless spin when it already fits");
    }
}
