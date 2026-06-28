//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Renders/slices at PREVIEW quality: it wraps
//! the source in `$preview = true; include <source>;` so models that gate detail on
//! `$fn = $preview ? low : high` render fast (nail_cure: 2.4s vs 43s at full $fn). Final,
//! full-quality output is `fab`'s job; the GUI just needs a quick, responsive preview.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Result};

use fab_scad::manifest::{Cut, Slicing};
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

/// Slice the source at the given X cuts (preview quality), returning the sliced STL.
/// A pure function of (source, cuts) — the unit a DAG cache would key on later.
pub fn reslice(
    root: Option<&Path>,
    source: &Path,
    cuts_x: &[f64],
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let wrap = preview_wrapper(source, out_dir)?;
    let cut = cuts_x
        .iter()
        .map(|&at| Cut {
            axis: "x".into(),
            at: Num::Float(at),
        })
        .collect();
    let spec = Slicing {
        printer: None,
        cut,
        connector: vec![],
    };
    slicing::slice_part(&oscad, &wrap, &spec, spread, out_dir, TIMEOUT)
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
