//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Render a source to a mesh; slice it.

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

/// Render a source `.scad` to an STL (the whole model, for display).
pub fn render_whole(root: Option<&Path>, source: &Path, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let oscad = Openscad::discover(root)?;
    let stem = stem_of(source);
    let out = out_dir.join(format!("{stem}.stl"));
    let r = oscad.render(source, &out, TIMEOUT)?;
    ensure!(r.ok, "render of {} failed", source.display());
    Ok(out)
}

/// Slice the source at one X cut and return the sliced STL (pieces fanned out by `spread`).
pub fn reslice(
    root: Option<&Path>,
    source: &Path,
    cut_x: f64,
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let spec = Slicing {
        printer: None,
        cut: vec![Cut {
            axis: "x".into(),
            at: Num::Float(cut_x),
        }],
        connector: vec![],
    };
    slicing::slice_part(&oscad, source, &spec, spread, out_dir, TIMEOUT)
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into())
}
