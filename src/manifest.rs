//! The project manifest (`project.toml`) — typed, and intentionally MINIMAL.
//!
//! Per the spec: start tiny, add a field only when a real project proves the need
//! (dogfood-driven). Right now a project is a name + a list of render targets; print
//! settings, slicing config, web/showcase metadata, and the build DAG get added as the
//! pilots demand them.
#![allow(dead_code)] // fields/loader wired up by `fab focus` (3.3) and `fab new` (3.5)

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::num::Num;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub part: Vec<Part>,
    /// Slicing spec — cuts + connectors the GUI edits and `fab slice` consumes (Phase 5).
    pub slicing: Option<Slicing>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    /// URL/dir slug — the stable identity.
    pub name: String,
    /// Human display name (defaults to `name` when absent).
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Part {
    /// `.scad` entry file for this render target.
    pub src: PathBuf,
    /// Optional label (defaults to the src stem).
    pub name: Option<String>,
    /// Optional explicit output path.
    pub out: Option<PathBuf>,
}

/// The slicing spec: how to split a part into printable pieces. Edited by the GUI (5.1),
/// consumed by `fab slice` (5.2), applied via `slicer.scad` / `connectors.scad`.
#[derive(Debug, Deserialize)]
pub struct Slicing {
    /// Printer whose bed the pieces target (defaults to printers.toml's default).
    pub printer: Option<String>,
    #[serde(default)]
    pub cut: Vec<Cut>,
    #[serde(default)]
    pub connector: Vec<Connector>,
    /// Manual per-piece print-orientation overrides (sparse). A piece is the slab multi-index
    /// `[ix,iy,iz]` from the sorted enabled cuts per axis; `up` is its build direction (the model
    /// direction that points +Z on the bed). Un-listed pieces derive +Z (or the auto-pick). The
    /// onion's cap axis is DERIVED from these per joint, never stored — see connector-orientation.
    #[serde(default)]
    pub orient: Vec<PieceOrient>,
}

/// A manual print-orientation override for one piece (#40).
#[derive(Debug, Deserialize)]
pub struct PieceOrient {
    pub piece: [usize; 3], // slab multi-index [ix, iy, iz]
    pub up: [Num; 3],      // build-up direction in model space (unit)
}

/// One slab cut: a plane perpendicular to `axis` at coordinate `at` (model space).
#[derive(Debug, Deserialize)]
pub struct Cut {
    pub axis: String, // "x" | "y" | "z"
    pub at: Num,      // mm, model coords
}

/// A joint across a cut face: `cut` indexes into `cut`, `pos` is the 2D spot on that plane.
#[derive(Debug, Deserialize)]
pub struct Connector {
    pub cut: usize,
    #[serde(rename = "type")]
    pub kind: String, // "bolt" (heat-set + bolt) | "onion" (support-free; replaced pin/dowel)
    pub screw: Option<String>,
    #[serde(default)]
    pub pos: [Num; 2],
    pub through: Option<f64>,
    pub size: Option<f64>, // onion joint diameter (auto-sized from the cross-section)
}

impl Cut {
    /// 0 = X, 1 = Y, 2 = Z.
    pub fn axis_index(&self) -> Result<usize> {
        match self.axis.to_lowercase().as_str() {
            "x" => Ok(0),
            "y" => Ok(1),
            "z" => Ok(2),
            other => bail!("slicing cut: axis must be x/y/z, got '{other}'"),
        }
    }
    pub fn at(&self) -> f64 {
        self.at.f()
    }
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing manifest {}", path.display()))
    }

    pub fn title(&self) -> &str {
        self.project.title.as_deref().unwrap_or(&self.project.name)
    }
}

#[cfg(test)]
mod tests {
    use super::Manifest;

    #[test]
    fn parses_a_project_with_parts() {
        let toml = r#"
            [project]
            name = "keyboard_tent"
            title = "Keyboard Tent"

            [[part]]
            src = "src/keyboard_tent.scad"

            [[part]]
            name = "refine"
            src = "src/keyboard_tent_refine.scad"
        "#;
        let m: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(m.project.name, "keyboard_tent");
        assert_eq!(m.title(), "Keyboard Tent");
        assert_eq!(m.part.len(), 2);
    }

    #[test]
    fn name_only_is_valid_and_title_defaults() {
        let m: Manifest = toml::from_str("[project]\nname = \"dowels\"\n").unwrap();
        assert!(m.part.is_empty());
        assert_eq!(m.title(), "dowels");
    }
}
