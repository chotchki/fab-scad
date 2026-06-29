//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Renders/slices at PREVIEW quality: it wraps
//! the source in `$preview = true; include <source>;` so models that gate detail on
//! `$fn = $preview ? low : high` render fast (nail_cure: 2.4s vs 43s at full $fn). Final,
//! full-quality output is `fab`'s job; the GUI just needs a quick, responsive preview.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};

use fab_scad::manifest::{Connector, Cut, Slicing};
use fab_scad::num::Num;
use fab_scad::openscad::Openscad;
use fab_scad::slicing;

const TIMEOUT: Duration = Duration::from_secs(120);

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

/// Render ONE bare piece (slab multi-index) of the already-rendered preview STL — for auto-orient
/// overhang scoring + the print-orientation preview. Returns the piece STL (empty if no geometry).
pub fn render_piece(
    root: Option<&Path>,
    stl: &Path,
    cuts: &[(char, f64)],
    piece: [usize; 3],
    out_dir: &Path,
) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut { axis: axis.to_string(), at: Num::Float(at) })
        .collect();
    let spec = Slicing { printer: None, cut, connector: vec![], orient: vec![] };
    let name = stl.file_name().and_then(|n| n.to_str()).context("non-UTF8 STL name")?;
    let tag = format!("piece-{}-{}-{}", piece[0], piece[1], piece[2]);
    let scad = out_dir.join(format!("{tag}.scad"));
    let out = out_dir.join(format!("{tag}.stl"));
    std::fs::write(&scad, slicing::piece_driver(&spec, name, piece)?)?;
    oscad.render(&scad, &out, TIMEOUT)?; // a piece may be empty (L-shaped gaps) — caller checks
    Ok(out)
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
    let connector = connectors
        .iter()
        .map(|c| Connector {
            cut: c.cut,
            kind: "onion".to_string(),
            screw: None,
            pos: [Num::Float(c.pos[0]), Num::Float(c.pos[1])],
            through: None,
            size: Some(c.size),
        })
        .collect();
    let spec = Slicing {
        printer: None,
        cut,
        connector,
        orient: vec![], // per-piece orientation overrides arrive in Phase E (GUI ORIENT stage)
    };
    slicing::slice_part(&oscad, &wrap, &spec, spread, out_dir, TIMEOUT)
}

/// A connector to place, resolved for slicing: `cut` is the index into the cuts slice passed
/// alongside, `pos` the two coords in the cut plane's non-axis dims, `size` the onion diameter
/// (auto-sized from the cross-section). Onion is the GUI's connector kind now (#39).
#[derive(Clone, Copy)]
pub struct Conn {
    pub cut: usize,
    pub pos: [f64; 2],
    pub size: f64,
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
