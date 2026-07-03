//! Projects, the focus context (`fab focus`), and scaffolding (`fab new`).
//!
//! A project is a directory under `models/<name>/` (the scad-models submodule) holding a
//! `project.toml` manifest, `src/*.scad` (the precious bytes), `out/` (gitignored,
//! regenerated), and `renders/` (kept thumbnails). `fab focus` records an active project
//! under `.fab/focus` so later commands need no name; `fab new` scaffolds the minimal
//! version of that layout and focuses it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Where designs live, relative to the fab-scad root (the scad-models submodule).
pub fn models_dir(root: &Path) -> PathBuf {
    root.join("models")
}

/// The directory for project `name`.
pub fn project_dir(root: &Path, name: &str) -> PathBuf {
    models_dir(root).join(name)
}

/// Per-user state file naming the active project (gitignored — not shared).
fn focus_file(root: &Path) -> PathBuf {
    root.join(".fab").join("focus")
}

/// The currently focused project name, if one is recorded.
pub fn read_focus(root: &Path) -> Option<String> {
    let raw = fs::read_to_string(focus_file(root)).ok()?;
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Record `name` as the active project.
pub fn write_focus(root: &Path, name: &str) -> Result<()> {
    let f = focus_file(root);
    if let Some(parent) = f.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&f, format!("{name}\n")).with_context(|| format!("writing {}", f.display()))
}

/// `fab focus [<project>]` — set the active project, or show it when no name is given.
pub fn focus_cmd(root: &Path, project: Option<String>) -> Result<()> {
    match project {
        Some(name) => {
            validate_name(&name)?;
            let dir = project_dir(root, &name);
            if !dir.is_dir() {
                bail!("no project '{name}' at {}", rel(root, &dir));
            }
            write_focus(root, &name)?;
            let note = if dir.join("project.toml").exists() {
                ""
            } else {
                "  (no project.toml yet — un-migrated)"
            };
            println!("focused {name}{note}");
        }
        None => match read_focus(root) {
            Some(name) if project_dir(root, &name).is_dir() => println!("{name}"),
            Some(name) => println!("{name}  (WARNING: no longer under models/)"),
            None => println!("no project focused — use `fab focus <name>`"),
        },
    }
    Ok(())
}

/// `fab new <name>` — scaffold a minimal project (manifest + starter scad) and focus it.
pub fn new_cmd(root: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    let dir = project_dir(root, name);
    if dir.exists() {
        bail!("{} already exists — refusing to clobber", rel(root, &dir));
    }

    let src = dir.join("src");
    fs::create_dir_all(&src).with_context(|| format!("creating {}", src.display()))?;
    fs::create_dir_all(dir.join("renders"))?;

    let manifest = dir.join("project.toml");
    let scad = src.join(format!("{name}.scad"));
    fs::write(&manifest, manifest_template(name, &title_case(name)))
        .with_context(|| format!("writing {}", manifest.display()))?;
    fs::write(&scad, scad_template(name)).with_context(|| format!("writing {}", scad.display()))?;

    write_focus(root, name)?;

    println!("created project '{name}' (scad-models submodule)");
    println!("  {}", rel(root, &manifest));
    println!("  {}", rel(root, &scad));
    println!("focused {name}");
    println!(
        "next: edit {0}, then `fab render {0} --png`",
        rel(root, &scad)
    );
    Ok(())
}

/// Render a path relative to the fab-scad root for tidy output (falls back to absolute).
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// A project name is its URL/dir slug: lowercase ascii, digits, `_`/`-`, starting
/// alphanumeric. Also the path-traversal guard (no `/`, no `..`).
fn validate_name(name: &str) -> Result<()> {
    let valid = matches!(name.bytes().next(), Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !valid {
        bail!(
            "invalid project name '{name}': use lowercase letters, digits, '_' or '-' \
             (e.g. shoe_holder)"
        );
    }
    Ok(())
}

/// `keyboard_tent` / `keyboard-tent` -> `Keyboard Tent`, a friendly default title.
fn title_case(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn manifest_template(name: &str, title: &str) -> String {
    format!(
        "# project.toml — minimal by design; add fields only when a project proves the need.\n\
         [project]\n\
         name = \"{name}\"\n\
         title = \"{title}\"\n\
         \n\
         [[part]]\n\
         src = \"src/{name}.scad\"\n"
    )
}

fn scad_template(name: &str) -> String {
    format!(
        "// {name}\n\
         // Scaffolded by `fab new`. Replace the starter geometry with your model.\n\
         //\n\
         // Libraries resolve via OPENSCADPATH — `fab render` sets it automatically; for the\n\
         // OpenSCAD GUI see docs/openscad-libraries.md. Canonical include form:\n\
         //   include <BOSL2/std.scad>        // pinned third-party (libs/)\n\
         //   include <version_stamp.scad>    // your shared modules (scad-lib/)\n\
         \n\
         include <BOSL2/std.scad>\n\
         \n\
         cuboid([20, 20, 20], anchor = BOTTOM);\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Self-cleaning temp dir (no external crate; std only).
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new() -> TempRoot {
            let p = std::env::temp_dir().join(format!(
                "fab-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            TempRoot(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn name_validation() {
        for ok in ["shoe_holder", "new_desk_v2", "ashtray", "k-tent", "a"] {
            assert!(validate_name(ok).is_ok(), "{ok} should be valid");
        }
        for bad in [
            "",
            "Shoe",
            "../etc",
            "a/b",
            "_leading",
            "-leading",
            "has space",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn title_defaults_are_friendly() {
        assert_eq!(title_case("keyboard_tent"), "Keyboard Tent");
        assert_eq!(title_case("nail-polish-holder"), "Nail Polish Holder");
        assert_eq!(title_case("ashtray"), "Ashtray");
    }

    #[test]
    fn focus_round_trips() {
        let root = TempRoot::new();
        assert_eq!(read_focus(root.path()), None);
        write_focus(root.path(), "shoe_holder").unwrap();
        assert_eq!(read_focus(root.path()).as_deref(), Some("shoe_holder"));
    }

    #[test]
    fn focus_requires_existing_project() {
        let root = TempRoot::new();
        // No such dir -> error, focus untouched.
        assert!(focus_cmd(root.path(), Some("ghost".into())).is_err());
        assert_eq!(read_focus(root.path()), None);
        // Make it exist -> focus sticks.
        fs::create_dir_all(project_dir(root.path(), "ghost")).unwrap();
        focus_cmd(root.path(), Some("ghost".into())).unwrap();
        assert_eq!(read_focus(root.path()).as_deref(), Some("ghost"));
    }

    #[test]
    fn new_scaffolds_parseable_project_and_focuses_it() {
        let root = TempRoot::new();
        new_cmd(root.path(), "widget_box").unwrap();

        let dir = project_dir(root.path(), "widget_box");
        let manifest = dir.join("project.toml");
        assert!(manifest.exists());
        assert!(dir.join("src/widget_box.scad").exists());
        assert!(dir.join("renders").is_dir());

        // The generated manifest must round-trip through the real parser.
        let m = Manifest::load(&manifest).unwrap();
        assert_eq!(m.project.name, "widget_box");
        assert_eq!(m.title(), "Widget Box");
        assert_eq!(m.part.len(), 1);

        // And it focused the new project.
        assert_eq!(read_focus(root.path()).as_deref(), Some("widget_box"));
    }

    #[test]
    fn new_refuses_to_clobber() {
        let root = TempRoot::new();
        new_cmd(root.path(), "dupe").unwrap();
        assert!(new_cmd(root.path(), "dupe").is_err());
    }

    #[test]
    fn new_rejects_bad_names_before_touching_disk() {
        let root = TempRoot::new();
        assert!(new_cmd(root.path(), "../escape").is_err());
        assert!(!models_dir(root.path()).join("..").join("escape").exists());
    }
}
