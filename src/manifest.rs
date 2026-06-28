//! The project manifest (`project.toml`) — typed, and intentionally MINIMAL.
//!
//! Per the spec: start tiny, add a field only when a real project proves the need
//! (dogfood-driven). Right now a project is a name + a list of render targets; print
//! settings, slicing config, web/showcase metadata, and the build DAG get added as the
//! pilots demand them.
#![allow(dead_code)] // fields/loader wired up by `fab focus` (3.3) and `fab new` (3.5)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub part: Vec<Part>,
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
