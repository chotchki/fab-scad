//! 2D bin-packing of piece footprints onto the fewest bed-sized plates (Phase 12).
//!
//! The endgame — auto-slice a too-big model, then fit the pieces onto as few print plates as
//! possible — is a hard problem (rectangle bin-packing is NP-hard). This is the pragmatic heuristic:
//! **FFDH** (First-Fit Decreasing Height) shelf packing with 90° rotation. Each piece packs by its
//! axis-aligned XY BOUNDING BOX, not its true outline — conservative (an L-bracket wastes its concave
//! corner) but robust: placements provably never overlap. True polygon nesting is a future upgrade if
//! bbox packing leaves plates too empty.

use std::cmp::Ordering;

use anyhow::{ensure, Result};

/// A rectangular footprint to pack: extent in X and Y (mm). The packer may rotate it 90°.
#[derive(Clone, Copy, Debug)]
pub struct Footprint {
    pub w: f64,
    pub h: f64,
}

/// Where a footprint landed: which plate, the min-corner XY within that plate's bed (mm from the
/// bed's front-left corner), and whether it was rotated 90° (so the caller rotates the piece to
/// match). Index-aligned with the `pack` input.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub plate: usize,
    pub x: f64,
    pub y: f64,
    pub rotated: bool,
}

/// Number of distinct plates a placement set spans (0 for an empty set).
pub fn plate_count(ps: &[Placement]) -> usize {
    ps.iter().map(|p| p.plate + 1).max().unwrap_or(0)
}

/// Pack `items` onto the fewest `bed` = `[width_x, depth_y]` (mm) plates, allowing 90° rotation and
/// leaving `gap` mm of spacing between pieces. Returns a `Placement` per item, index-aligned with
/// `items`. Errors if any single item can't fit one bed even alone.
///
/// FFDH: normalize each piece to landscape (w ≥ h), sort tallest-first, drop each into the first
/// shelf (row) it fits across all open plates, opening a new shelf — then a new plate — only when
/// nothing fits.
pub fn pack(items: &[Footprint], bed: [f64; 2], gap: f64) -> Result<Vec<Placement>> {
    let [bw, bh] = bed;
    ensure!(
        bw > 0.0 && bh > 0.0,
        "bed dimensions must be positive, got {bw}×{bh}"
    );

    // Normalize to landscape (w ≥ h), remembering the rotation, and validate bed-fit up front.
    struct Norm {
        idx: usize,
        w: f64,
        h: f64,
        rot: bool,
    }
    let mut norm: Vec<Norm> = Vec::with_capacity(items.len());
    for (idx, f) in items.iter().enumerate() {
        let (w, h, rot) = if f.w >= f.h {
            (f.w, f.h, false)
        } else {
            (f.h, f.w, true)
        };
        ensure!(
            w + gap <= bw + 1e-9 && h + gap <= bh + 1e-9,
            "piece {idx} ({:.1}×{:.1} mm) doesn't fit the {bw:.0}×{bh:.0} bed in either orientation",
            f.w,
            f.h
        );
        norm.push(Norm { idx, w, h, rot });
    }
    // Tallest first — the defining move of FFDH (keeps shelves from being over-tall).
    norm.sort_by(|a, b| b.h.partial_cmp(&a.h).unwrap_or(Ordering::Equal));

    struct Shelf {
        plate: usize,
        y0: f64,
        height: f64,
        x: f64, // next free x on this shelf
    }
    let mut shelves: Vec<Shelf> = Vec::new();
    let mut plate_top: Vec<f64> = Vec::new(); // next free y (new-shelf baseline) per plate
    let mut out = vec![
        Placement {
            plate: 0,
            x: 0.0,
            y: 0.0,
            rotated: false
        };
        items.len()
    ];

    for it in &norm {
        // 1) First existing shelf (any plate) that fits both width and height.
        if let Some(sh) = shelves
            .iter_mut()
            .find(|s| it.h <= s.height + 1e-9 && s.x + it.w + gap <= bw + 1e-9)
        {
            out[it.idx] = Placement {
                plate: sh.plate,
                x: sh.x,
                y: sh.y0,
                rotated: it.rot,
            };
            sh.x += it.w + gap;
            continue;
        }
        // 2) New shelf on the first plate with vertical room.
        if let Some((p, top)) = plate_top
            .iter_mut()
            .enumerate()
            .find(|(_, top)| **top + it.h + gap <= bh + 1e-9)
        {
            let y0 = *top;
            *top = y0 + it.h + gap;
            shelves.push(Shelf {
                plate: p,
                y0,
                height: it.h,
                x: it.w + gap,
            });
            out[it.idx] = Placement {
                plate: p,
                x: 0.0,
                y: y0,
                rotated: it.rot,
            };
            continue;
        }
        // 3) A fresh plate.
        let p = plate_top.len();
        plate_top.push(it.h + gap);
        shelves.push(Shelf {
            plate: p,
            y0: 0.0,
            height: it.h,
            x: it.w + gap,
        });
        out[it.idx] = Placement {
            plate: p,
            x: 0.0,
            y: 0.0,
            rotated: it.rot,
        };
    }
    Ok(out)
}

/// Fraction of total plate area the packed footprints occupy (0..1), for a quality read-out. A low
/// number on many plates says the bbox packer left room a smarter nester could reclaim.
pub fn fill_ratio(items: &[Footprint], placements: &[Placement], bed: [f64; 2]) -> f64 {
    let plates = plate_count(placements);
    if plates == 0 {
        return 0.0;
    }
    let used: f64 = items.iter().map(|f| f.w * f.h).sum();
    used / (plates as f64 * bed[0] * bed[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(w: f64, h: f64) -> Footprint {
        Footprint { w, h }
    }

    // Placed rectangle (post-rotation) for overlap checks.
    fn rect(items: &[Footprint], p: &Placement, i: usize) -> (f64, f64, f64, f64) {
        let (w, h) = if p.rotated {
            (items[i].h, items[i].w)
        } else {
            (items[i].w, items[i].h)
        };
        (p.x, p.y, p.x + w, p.y + h)
    }

    fn overlaps(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
        a.0 < b.2 - 1e-6 && b.0 < a.2 - 1e-6 && a.1 < b.3 - 1e-6 && b.1 < a.3 - 1e-6
    }

    // No two pieces on the same plate overlap, and every piece sits within the bed.
    fn assert_valid(items: &[Footprint], ps: &[Placement], bed: [f64; 2]) {
        for i in 0..items.len() {
            let (x0, y0, x1, y1) = rect(items, &ps[i], i);
            assert!(
                x0 >= -1e-6 && y0 >= -1e-6 && x1 <= bed[0] + 1e-6 && y1 <= bed[1] + 1e-6,
                "piece {i} off-bed"
            );
            for j in (i + 1)..items.len() {
                if ps[i].plate == ps[j].plate {
                    assert!(
                        !overlaps(rect(items, &ps[i], i), rect(items, &ps[j], j)),
                        "pieces {i},{j} overlap"
                    );
                }
            }
        }
    }

    #[test]
    fn single_small_piece_one_plate() {
        let items = [fp(50.0, 40.0)];
        let ps = pack(&items, [256.0, 256.0], 2.0).unwrap();
        assert_eq!(plate_count(&ps), 1);
        assert_valid(&items, &ps, [256.0, 256.0]);
    }

    #[test]
    fn oversized_piece_errors() {
        let items = [fp(300.0, 40.0)];
        assert!(
            pack(&items, [256.0, 256.0], 2.0).is_err(),
            "300mm > 256mm bed in both orientations"
        );
    }

    #[test]
    fn tall_piece_rotates_to_landscape() {
        // 40 wide × 200 tall on a 256 bed: fits either way, but normalized to landscape → rotated.
        let items = [fp(40.0, 200.0)];
        let ps = pack(&items, [256.0, 256.0], 2.0).unwrap();
        assert!(
            ps[0].rotated,
            "portrait piece should be rotated to landscape"
        );
        assert_valid(&items, &ps, [256.0, 256.0]);
    }

    #[test]
    fn small_pieces_share_a_plate() {
        let items = [fp(60.0, 60.0), fp(60.0, 60.0), fp(60.0, 60.0)];
        let ps = pack(&items, [256.0, 256.0], 2.0).unwrap();
        assert_eq!(plate_count(&ps), 1, "three 60mm squares fit one 256 bed");
        assert_valid(&items, &ps, [256.0, 256.0]);
    }

    #[test]
    fn big_pieces_spill_to_more_plates() {
        // Four ~200×200 slabs can't co-exist on a 256 bed (only one fits per plate) → 4 plates.
        let items = [
            fp(200.0, 200.0),
            fp(200.0, 200.0),
            fp(200.0, 200.0),
            fp(200.0, 200.0),
        ];
        let ps = pack(&items, [256.0, 256.0], 2.0).unwrap();
        assert_eq!(plate_count(&ps), 4);
        assert_valid(&items, &ps, [256.0, 256.0]);
    }

    #[test]
    fn many_mixed_pieces_never_overlap() {
        let items: Vec<Footprint> = (0..20)
            .map(|i| fp(30.0 + (i % 5) as f64 * 20.0, 40.0 + (i % 3) as f64 * 30.0))
            .collect();
        let ps = pack(&items, [256.0, 256.0], 3.0).unwrap();
        assert_valid(&items, &ps, [256.0, 256.0]);
        assert!(fill_ratio(&items, &ps, [256.0, 256.0]) > 0.0);
    }
}
