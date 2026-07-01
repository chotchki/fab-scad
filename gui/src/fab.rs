//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Renders/slices at PREVIEW quality: it wraps
//! the source in `$preview = true; include <source>;` so models that gate detail on
//! `$fn = $preview ? low : high` render fast (nail_cure: 2.4s vs 43s at full $fn). Final,
//! full-quality output is `fab`'s job; the GUI just needs a quick, responsive preview.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};

use fab_scad::manifest::{Connector, Cut, PieceOrient, Slicing};
use fab_scad::num::Num;
use fab_scad::openscad::Openscad;
use fab_scad::slicing;

use crate::stl::{self, StlMesh};

const TIMEOUT: Duration = Duration::from_secs(120);

/// A `[slicing]` spec carrying only the cuts — the shared base for per-piece rendering and the
/// orientation sweep (connectors/orientation are layered on by the specific caller).
fn cuts_to_spec(cuts: &[(char, f64)]) -> Slicing {
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut { axis: axis.to_string(), at: Num::Float(at) })
        .collect();
    Slicing { printer: None, cut, connector: vec![], orient: vec![] }
}

/// GUI placements → manifest connectors, per kind: an onion carries its auto-sized diameter; a bolt
/// carries its screw size (and lets the slicer default `through`). The slicer consumes both.
fn to_connectors(connectors: &[Conn]) -> Vec<Connector> {
    connectors
        .iter()
        .map(|c| {
            let pos = [Num::Float(c.pos[0]), Num::Float(c.pos[1])];
            match c.kind {
                ConnKind::Onion => Connector {
                    cut: c.cut,
                    kind: "onion".to_string(),
                    screw: None,
                    pos,
                    through: None,
                    size: Some(c.size),
                },
                ConnKind::Bolt => Connector {
                    cut: c.cut,
                    kind: "bolt".to_string(),
                    screw: Some(c.screw.to_string()),
                    pos,
                    through: None, // slicer defaults through-depth (12mm) until we expose it
                    size: None,
                },
            }
        })
        .collect()
}

/// GUI per-piece orientations → manifest `[slicing.orient]` entries.
fn to_orient(orient: &[Orient3]) -> Vec<PieceOrient> {
    orient
        .iter()
        .map(|o| PieceOrient {
            piece: o.piece,
            up: [Num::Float(o.up[0]), Num::Float(o.up[1]), Num::Float(o.up[2])],
        })
        .collect()
}

/// Walk up to the fab-scad root (the dir with `printers.toml` + `scad-lib/`) for OPENSCADPATH.
pub fn find_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("printers.toml").exists() && dir.join("scad-lib").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Render the source whole at PREVIEW quality, returning the STL.
pub fn render_whole(root: Option<&Path>, source: &Path, out_dir: &Path) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let wrap = preview_wrapper(source, out_dir)?;
    let out = out_dir.join(format!("{}.stl", stem_of(source)));
    let r = oscad.render(&wrap, &out, TIMEOUT)?;
    ensure!(r.ok, "render of {} failed", source.display());
    Ok(out)
}

/// The preview STL `render_whole` writes for `source` (reused by the cross-section, no re-render).
pub fn whole_stl(source: &Path, out_dir: &Path) -> PathBuf {
    out_dir.join(format!("{}.stl", stem_of(source)))
}

/// Render ONE piece (slab multi-index) of the already-rendered preview STL per `spec` — a bare spec
/// (no connectors) for auto-orient overhang scoring, the full spec (onions + orientations) for the
/// print-orientation preview's joined piece. Returns the piece STL (empty if no geometry).
pub fn render_piece(
    oscad: &Openscad,
    stl: &Path,
    spec: &Slicing,
    piece: [usize; 3],
    out_dir: &Path,
) -> Result<PathBuf> {
    let name = stl.file_name().and_then(|n| n.to_str()).context("non-UTF8 STL name")?;
    let tag = format!("piece-{}-{}-{}", piece[0], piece[1], piece[2]);
    let scad = out_dir.join(format!("{tag}.scad"));
    let out = out_dir.join(format!("{tag}.stl"));
    std::fs::write(&scad, slicing::piece_driver(spec, name, piece)?)?;
    oscad.render(&scad, &out, TIMEOUT)?; // a piece may be empty (L-shaped gaps) — caller checks
    Ok(out)
}

/// One piece, rendered + auto-oriented for the print-orientation preview: its slab multi-index, mesh
/// (WITH its joints carved — peg/socket), and the least-support build-up (`auto_orient::best_up`).
/// Empty slabs are dropped upstream.
pub struct PiecePrint {
    pub piece: [usize; 3],
    pub mesh: StlMesh,
    pub up: [f32; 3],
}

/// Render every piece of `source` at `cuts` + `connectors`, auto-pick each piece's print orientation
/// (least overhang), then re-render each piece WITH its feasible onions carved so the preview shows
/// the real printable geometry — joints and all. Two passes: bare → `best_up` (orientation gates the
/// onions), then carve with those orientations. Serial; pieces are cheap STL intersections off the
/// frozen whole mesh. Seeds the orientations `reslice` threads back in.
pub fn print_layout(
    root: Option<&Path>,
    source: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    out_dir: &Path,
) -> Result<Vec<PiecePrint>> {
    let oscad = Openscad::discover(root)?;
    // The pieces slice from the frozen whole mesh; render it once if a prior whole render didn't.
    let whole = whole_stl(source, out_dir);
    if !whole.exists() {
        render_whole(root, source, out_dir)?;
    }
    let bare = cuts_to_spec(cuts);

    // Pass 1: bare render of each non-empty piece -> least-support orientation. (Axis-aligned cuts
    // only today, so the cut-face normals are already in `best_up`'s base set — pass none.)
    let mut ups: Vec<([usize; 3], [f64; 3])> = Vec::new();
    for piece in slicing::piece_indices(&bare)? {
        let mesh = stl::load_stl(&render_piece(&oscad, &whole, &bare, piece, out_dir)?)?;
        if mesh.positions.is_empty() {
            continue; // an empty slab (L-shaped gap) — nothing to print
        }
        ups.push((piece, fab_scad::auto_orient::best_up(&to_tris(&mesh), &[])));
    }

    // Pass 2: carve each piece with the onions, gated by the orientations just picked, so the
    // preview's joints match what the slice would produce.
    let spec = Slicing {
        printer: None,
        cut: bare.cut,
        connector: to_connectors(connectors),
        orient: ups
            .iter()
            .map(|&(piece, up)| PieceOrient {
                piece,
                up: [Num::Float(up[0]), Num::Float(up[1]), Num::Float(up[2])],
            })
            .collect(),
    };
    let mut out = Vec::new();
    for (piece, up) in ups {
        let mesh = stl::load_stl(&render_piece(&oscad, &whole, &spec, piece, out_dir)?)?;
        if mesh.positions.is_empty() {
            continue;
        }
        out.push(PiecePrint { piece, mesh, up: [up[0] as f32, up[1] as f32, up[2] as f32] });
    }
    Ok(out)
}

/// `StlMesh` positions (flat, 3 verts per tri) → `[[f64; 3]; 3]` triangles for the orientation math.
fn to_tris(m: &StlMesh) -> Vec<[[f64; 3]; 3]> {
    m.positions
        .chunks_exact(3)
        .map(|t| std::array::from_fn(|i| [t[i][0] as f64, t[i][1] as f64, t[i][2] as f64]))
        .collect()
}

/// The cut's 2D cross-section profile (loops in connector-pos coords), from the already-rendered
/// preview STL — for the per-cut connector editor.
pub fn cross_section(
    root: Option<&Path>,
    stl: &Path,
    axis: usize,
    at: f64,
    out_dir: &Path,
) -> Result<Vec<Vec<[f64; 2]>>> {
    let oscad = Openscad::discover(root)?;
    fab_scad::cross_section::cross_section(&oscad, stl, axis, at, out_dir, TIMEOUT)
}

/// Slice the source at the given cuts — each `(axis, at)` with axis in `'x' | 'y' | 'z'` (preview
/// quality), returning the sliced STL. A pure function of (source, cuts) — the DAG-cache unit.
pub fn reslice(
    root: Option<&Path>,
    source: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let wrap = preview_wrapper(source, out_dir)?;
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut {
            axis: axis.to_string(),
            at: Num::Float(at),
        })
        .collect();
    // Per-piece print orientations (auto-picked, seeded by the print-orientation preview). They
    // GATE the onions — a piece oriented off its cut axis downgrades that joint to a bolt. Empty =
    // every piece defaults to +Z (`slicing::piece_up`), which is the pre-orientation behaviour.
    let spec = Slicing {
        printer: None,
        cut,
        connector: to_connectors(connectors),
        orient: to_orient(orient),
    };
    slicing::slice_part(&oscad, &wrap, &spec, spread, out_dir, TIMEOUT)
}

/// Per-connector onion feasibility under the current cuts + orientations, index-aligned with
/// `connectors`: `true` = prints support-free, `false` = downgrades to a bolt. Pure (no render),
/// so the GUI can flag joints live as cuts/orientations change. Same gate `reslice` carves with.
pub fn conn_feasibility(
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
) -> Result<Vec<bool>> {
    let mut spec = cuts_to_spec(cuts);
    spec.connector = to_connectors(connectors);
    spec.orient = to_orient(orient);
    slicing::onion_feasibility(&spec)
}

/// The two connector kinds the GUI places (both consumed by the slicer): Onion = support-free
/// peg/socket, auto-sized from the cross-section; Bolt = heat-set pocket + machine screw across the
/// cut. An onion that can't print support-free downgrades to a bolt in the slice regardless.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnKind {
    Onion,
    Bolt,
}

/// A connector to place, resolved for slicing: `cut` is the index into the cuts slice passed
/// alongside, `pos` the two coords in the cut plane's non-axis dims. `size` is the onion diameter
/// (auto-sized from the cross-section, ignored for a bolt); `screw` the bolt size ("M3"/"M4"/"M5",
/// ignored for an onion). `kind` picks which.
#[derive(Clone, Copy)]
pub struct Conn {
    pub cut: usize,
    pub pos: [f64; 2],
    pub size: f64,
    pub kind: ConnKind,
    pub screw: &'static str,
}

/// A per-piece print orientation, resolved for slicing: the slab multi-index and its build-up
/// direction (model space, unit). Threaded into `reslice` as `[slicing.orient]` so the slice
/// honours the auto-picked / manual print orientation (and gates the onions accordingly).
#[derive(Clone, Copy)]
pub struct Orient3 {
    pub piece: [usize; 3],
    pub up: [f64; 3],
}

/// Write a `$preview = true; include <source>;` wrapper so the source's
/// `$fn = $preview ? low : high` resolves to the low (fast) path. Returns the wrapper path.
fn preview_wrapper(source: &Path, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let abs = source.canonicalize()?;
    let wrap = out_dir.join(format!("{}-preview.scad", stem_of(source)));
    std::fs::write(&wrap, format!("$preview = true;\ninclude <{}>;\n", abs.display()))?;
    Ok(wrap)
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into())
}
