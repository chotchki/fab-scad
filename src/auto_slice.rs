//! Auto-slice (Phase 13): partition a model too big to print into pieces that each fit the printer
//! bed. The v1 heuristic is a bounding-box partition — for every axis the model overflows the bed on,
//! cut it into `ceil(extent / bed)` EQUAL parts (equal so there are no thin slivers), and cut ONLY
//! the overflowing axes (single-axis when just one dimension is too big — keeps cuts from gridding
//! into the jigsaw-intersection mess chotchki wants to avoid). Pure bbox math, so `fab make` and the
//! GUI compute the same cuts.
//!
//! Known v1 limits (deliberate, documented rather than hidden):
//!   - **bbox-based**: cuts the whole grid even through empty space — an L-model's empty cells just
//!     drop out downstream (the slicer discards empty pieces).
//!   - **no rotate-to-fit**: each cell is cut to fit AXIS-ALIGNED against the matching bed dim, so a
//!     piece that would fit the bed if spun still gets cut. Conservative, occasionally over-cuts.
//!   - **feature-blind placement**: equal division doesn't dodge thin features or spots a connector
//!     can't seat. Cuts land on the even grid; a v2 could nudge them.

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
}
