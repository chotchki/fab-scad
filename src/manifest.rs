//! The project manifest (`project.toml`) — typed, and intentionally MINIMAL.
//!
//! Per the spec: start tiny, add a field only when a real project proves the need
//! (dogfood-driven). Right now a project is a name + a list of render targets; print
//! settings, slicing config, web/showcase metadata, and the build DAG get added as the
//! pilots demand them.
#![allow(dead_code)] // fields/loader wired up by `fab focus` (3.3) and `fab new` (3.5)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::num::Num;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub project: Project,
    #[serde(default)]
    pub part: Vec<Part>,
    /// Slicing spec — cuts + connectors the GUI edits and `fab slice` consumes (Phase 5).
    pub slicing: Option<Slicing>,
    /// Publish metadata for `fab publish` → hotchkiss.io (Phase 15).
    pub publish: Option<Publish>,
}

/// Publish metadata: the project-page body shown on hotchkiss.io/projects/<slug>.
#[derive(Debug, Deserialize)]
pub struct Publish {
    /// Markdown description, rendered above the interactive preview + downloads on the project page.
    #[serde(default)]
    pub description: String,
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
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
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
    /// Per-part slicing (U.3.14). Each block addresses ONE `build_geo_parts` part by `key`; the flat
    /// `cut`/`connector`/`orient` above are the WHOLE-MODEL spec (back-compat + legacy CLI). XOR with
    /// `part` — a spec carrying BOTH is an error (`slice_cmd` bails); the GUI migrates flat→per-part on
    /// its first write, so its output is never a mix.
    #[serde(default, rename = "part")]
    pub parts: Vec<PartSlicing>,
}

/// One `build_geo_parts` part's slicing (U.3.14): the same shape as the flat `Slicing`, but its
/// `cut`/`connector`/`orient` indices are PART-LOCAL — each resolves against this block's own vectors,
/// exactly what `reslice_part_kernel` / `make_planned` already feed `slice_solid`.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PartSlicing {
    pub key: PartKey,
    #[serde(default)]
    pub cut: Vec<Cut>,
    #[serde(default)]
    pub connector: Vec<Connector>,
    #[serde(default)]
    pub orient: Vec<PieceOrient>,
}

/// Binds a `[[slicing.part]]` block to a `build_geo_parts` part. `name` + `nth` is the primary key
/// (survives a reorder); `index` is the authored-order fallback (survives a name going anonymous — a
/// part-count mismatch nulls EVERY provenance name at once). See `backend::resolve_part`.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PartKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub nth: usize, // 0-based ordinal among parts sharing `name`
    pub index: usize, // authored index at save time — the fallback bind
}

/// A manual print-orientation override for one piece (#40).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PieceOrient {
    pub piece: [usize; 3], // slab multi-index [ix, iy, iz]
    /// Connected-component within the slab; 0 = whole slab (back-compat). U.3.14 keys orientation by
    /// (slab, comp) so a manually-oriented component of a presliced blob orients in the sliced output.
    #[serde(default)]
    pub comp: usize,
    pub up: [Num; 3], // build-up direction in model space (unit)
}

/// One slab cut: a plane perpendicular to `axis` at coordinate `at` (model space).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Cut {
    pub axis: String, // "x" | "y" | "z"
    pub at: Num,      // mm, model coords
}

/// A joint across a cut face: `cut` indexes into `cut`, `pos` is the 2D spot on that plane.
#[derive(Debug, Serialize, Deserialize, Clone)]
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

    /// Walk up from `near` to the nearest `project.toml`.
    pub fn find(near: &Path) -> Result<PathBuf> {
        let abs = near
            .canonicalize()
            .with_context(|| format!("resolving {}", near.display()))?;
        let mut dir = abs.parent();
        while let Some(d) = dir {
            let m = d.join("project.toml");
            if m.exists() {
                return Ok(m);
            }
            dir = d.parent();
        }
        anyhow::bail!("no project.toml found above {}", near.display())
    }

    /// Find + load the manifest nearest `near` (the project.toml above it).
    pub fn load_near(near: &Path) -> Result<Manifest> {
        Self::load(&Self::find(near)?)
    }

    pub fn title(&self) -> &str {
        self.project.title.as_deref().unwrap_or(&self.project.name)
    }
}

#[cfg(test)]
mod tests {
    use super::{Manifest, PartKey};

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

    // ── U.3.14 per-part slicing schema ───────────────────────────────────────────────────────────
    #[test]
    fn parses_per_part_slicing() {
        let toml = r#"
[project]
name = "x"
[slicing]
printer = "bambu"
[[slicing.part]]
key = { name = "wall", nth = 0, index = 1 }
cut = [ { axis = "z", at = 40.0 } ]
connector = [ { cut = 0, type = "onion", pos = [10.0, 5.0], size = 12.0 } ]
orient = [ { piece = [0, 0, 1], comp = 2, up = [0.0, 0.0, 1.0] } ]
"#;
        let s = toml::from_str::<Manifest>(toml).unwrap().slicing.unwrap();
        assert_eq!(s.printer.as_deref(), Some("bambu"));
        assert!(s.cut.is_empty()); // no FLAT cut — it's all per-part
        assert_eq!(s.parts.len(), 1);
        let p = &s.parts[0];
        assert_eq!(p.key.name.as_deref(), Some("wall"));
        assert_eq!(p.key.index, 1);
        assert_eq!(p.cut[0].axis, "z");
        assert_eq!(p.connector[0].cut, 0);
        assert_eq!(p.orient[0].piece, [0, 0, 1]);
        assert_eq!(p.orient[0].comp, 2); // component index round-trips
    }

    #[test]
    fn legacy_flat_slicing_parses_with_no_parts() {
        let toml = "[project]\nname = \"x\"\n[slicing]\ncut = [ { axis = \"x\", at = 0.0 } ]\n";
        let s = toml::from_str::<Manifest>(toml).unwrap().slicing.unwrap();
        assert_eq!(s.cut.len(), 1); // flat cut still parses
        assert!(s.parts.is_empty()); // no [[slicing.part]] → whole-model back-compat
    }

    #[test]
    fn orient_comp_defaults_to_zero() {
        let s = toml::from_str::<Manifest>(
            "[project]\nname=\"x\"\n[slicing]\norient=[{piece=[0,0,0],up=[0.0,0.0,1.0]}]\n",
        )
        .unwrap()
        .slicing
        .unwrap();
        assert_eq!(s.orient[0].comp, 0); // omitted comp → whole slab
    }

    #[test]
    fn resolve_part_binds_by_name_then_index() {
        use crate::backend::resolve_part;
        let key = |name: Option<&str>, nth, index| PartKey {
            name: name.map(String::from),
            nth,
            index,
        };
        let names = vec![
            Some("wall".to_string()),
            Some("frame".to_string()),
            Some("wall".to_string()),
        ];
        assert_eq!(resolve_part(&names, &key(Some("wall"), 1, 99)), Some(2)); // 2nd "wall" by nth
        assert_eq!(resolve_part(&names, &key(Some("gone"), 0, 1)), Some(1)); // name miss → index
        assert_eq!(resolve_part(&names, &key(None, 0, 0)), Some(0)); // no name → index
        assert_eq!(resolve_part(&names, &key(Some("gone"), 0, 9)), None); // no name, index past end
        let anon = vec![None, None]; // count-mismatch nulled every name → index-only
        assert_eq!(resolve_part(&anon, &key(Some("wall"), 0, 1)), Some(1));
    }
}
