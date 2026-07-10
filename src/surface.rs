//! `surface()` heightmap → a solid [`fab_lang::Mesh`] (M.5.2, DAT-only). The impure reader fab-lang's
//! `surface()` File need resolves through — a sibling of the STL/3MF import readers.
//!
//! Tessellation is `surface.cc` bug-for-bug, confirmed against the oracle (2026.06.12): the TOP is the
//! heightmap — a vertex per grid point at its height, plus a CENTER vertex per cell at the 4-corner average,
//! each cell fanning into 4 triangles (the diamond pattern); the solid is closed by a flat BASE at
//! `z = min(heights) − 1` and vertical WALLS around the boundary. (The base is our own grid-mirror
//! triangulation, not the oracle's boundary-ring fan — the differential is boolean-residual + genus, so the
//! SAME solid passes regardless of how the flat base is cut.) `center` is applied EVAL-side in fab-lang (the
//! reader is path-only); `invert` + PNG are the deferred follow-up (backlog #159) — invert is a no-op for
//! DAT, so DAT-only needs neither.

use anyhow::{Context, Result, bail};
use fab_lang::{Mesh, Tri, Vec3};
use std::path::Path;

/// Read a DAT heightmap → the `surface()` solid. See the module docs for the tessellation.
///
/// # Errors
/// Fails if the file can't be read or has no numeric data rows.
pub fn dat_mesh(path: &Path) -> Result<Mesh> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let grid = parse_dat(&text)?;
    Ok(tessellate(&grid))
}

/// Parse a DAT: each non-comment line is a row of whitespace-separated z-values (row = y, col = x, no
/// y-flip — verified against the oracle). Blank lines and `#`/`!` comment lines are skipped; a ragged row
/// pads with 0 to the widest row. Errors on no data.
fn parse_dat(text: &str) -> Result<Vec<Vec<f64>>> {
    let mut rows: Vec<Vec<f64>> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
            continue;
        }
        let row: Vec<f64> = t
            .split_whitespace()
            .filter_map(|tok| tok.parse().ok())
            .collect();
        if !row.is_empty() {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        bail!("surface DAT has no numeric data rows");
    }
    let w = rows.iter().map(Vec::len).max().unwrap_or(0);
    for r in &mut rows {
        r.resize(w, 0.0);
    }
    Ok(rows)
}

/// Tessellate a `H×W` height grid into the `surface()` solid (see module docs). A grid under 2×2 has no
/// cell → an empty mesh (a degenerate heightmap renders nothing, matching the oracle).
#[allow(
    clippy::cast_precision_loss,
    reason = "grid indices are small; x/y as f64 coordinates is exact for any realistic heightmap"
)]
fn tessellate(grid: &[Vec<f64>]) -> Mesh {
    let h = grid.len();
    let w = grid.first().map_or(0, Vec::len);
    if w < 2 || h < 2 {
        return Mesh::new();
    }
    let base_z = grid.iter().flatten().copied().fold(f64::INFINITY, f64::min) - 1.0;

    let mut verts: Vec<Vec3> = Vec::with_capacity(2 * w * h + (w - 1) * (h - 1));
    // Block 0: top grid verts (at their heights), index = y*w + x.
    for (y, row) in grid.iter().enumerate() {
        for (x, &z) in row.iter().enumerate() {
            verts.push(Vec3::new(x as f64, y as f64, z));
        }
    }
    // Block 1: base grid verts (flat at base_z), index = w*h + y*w + x.
    for y in 0..h {
        for x in 0..w {
            verts.push(Vec3::new(x as f64, y as f64, base_z));
        }
    }
    // Block 2: cell centers (4-corner average), index = 2*w*h + y*(w-1) + x.
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let z = (grid[y][x] + grid[y][x + 1] + grid[y + 1][x] + grid[y + 1][x + 1]) / 4.0;
            verts.push(Vec3::new(x as f64 + 0.5, y as f64 + 0.5, z));
        }
    }
    // Index helpers (casts are bounded by the grid size).
    let idx = |i: usize| u32::try_from(i).unwrap_or(u32::MAX);
    let top = |x: usize, y: usize| idx(y * w + x);
    let bot = |x: usize, y: usize| idx(w * h + y * w + x);
    let ctr = |x: usize, y: usize| idx(2 * w * h + y * (w - 1) + x);

    let mut tris = Vec::new();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            // TOP: fan from the cell center, CCW corners → up (+z) normals.
            let (a, b, c, d, m) = (
                top(x, y),
                top(x + 1, y),
                top(x + 1, y + 1),
                top(x, y + 1),
                ctr(x, y),
            );
            tris.push(Tri::new(a, b, m));
            tris.push(Tri::new(b, c, m));
            tris.push(Tri::new(c, d, m));
            tris.push(Tri::new(d, a, m));
            // BASE: the same cell flat at base_z, reversed → down (−z) normals.
            let (a2, b2, c2, d2) = (bot(x, y), bot(x + 1, y), bot(x + 1, y + 1), bot(x, y + 1));
            tris.push(Tri::new(a2, c2, b2));
            tris.push(Tri::new(a2, d2, c2));
        }
    }

    // WALLS: the boundary as a CCW loop (interior on the left, viewed from +z); each edge p→q gets two
    // outward-facing triangles connecting the top edge to the base edge.
    let mut boundary: Vec<(usize, usize)> = Vec::new();
    boundary.extend((0..w).map(|x| (x, 0))); // front (+x)
    boundary.extend((1..h).map(|y| (w - 1, y))); // right (+y)
    boundary.extend((0..w - 1).rev().map(|x| (x, h - 1))); // back (−x)
    boundary.extend((1..h - 1).rev().map(|y| (0, y))); // left (−y)
    for i in 0..boundary.len() {
        let (px, py) = boundary[i];
        let (qx, qy) = boundary[(i + 1) % boundary.len()];
        let (tp, tq, bp, bq) = (top(px, py), top(qx, qy), bot(px, py), bot(qx, qy));
        // (top_p, base_p, top_q) + (top_q, base_p, base_q) — outward for a CCW boundary.
        tris.push(Tri::new(tp, bp, tq));
        tris.push(Tri::new(tq, bp, bq));
    }

    Mesh { verts, tris }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: unwrap/expect ARE the assertions"
)]
mod tests {
    use super::{parse_dat, tessellate};

    #[test]
    fn parses_a_grid_with_comments_and_ragged_rows() {
        let g = parse_dat("# header\n1 2 3\n\n4 5\n! bang comment\n").unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(g[1], vec![4.0, 5.0, 0.0], "short row padded with 0");
    }

    #[test]
    fn tessellates_a_3x3_to_the_oracle_structure() {
        // The probe grid: a center bump. Top = 9 grid verts + 4 cell centers; the solid closes with a
        // grid-mirror base + walls. Vertex counts match the oracle's TOP (grid + centers); the base is ours.
        let grid = vec![
            vec![0.0, 0.0, 0.0],
            vec![0.0, 9.0, 0.0],
            vec![0.0, 0.0, 0.0],
        ];
        let m = tessellate(&grid);
        // top grid (9) + base grid (9) + centers (4) = 22 verts.
        assert_eq!(m.vert_count(), 22);
        // top 4/cell (16) + base 2/cell (8) + walls 2/edge over 8 boundary edges (16) = 40 tris.
        assert_eq!(m.tri_count(), 40);
        // the center bump lifts one grid vert to z = 9, the base sits at min − 1 = −1.
        let zs: Vec<f64> = m.verts.iter().map(|v| v.z).collect();
        assert!(
            zs.iter().any(|&z| (z - 9.0).abs() < 1e-9),
            "the bump survives"
        );
        assert!(
            zs.iter().any(|&z| (z + 1.0).abs() < 1e-9),
            "base at min − 1"
        );
    }

    #[test]
    fn a_degenerate_grid_is_empty() {
        assert_eq!(
            tessellate(&[vec![1.0, 2.0, 3.0]]).tri_count(),
            0,
            "1 row → no cells"
        );
    }
}
