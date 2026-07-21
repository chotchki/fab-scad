//! Phase Z: the open DOCUMENT is always a PROJECT. A bare `.scad` is a one-file project; a `.scadproj`
//! is an N-file one. [`EditorBuf`](crate::state::EditorBuf) stays the VIEW onto the ACTIVE file — this
//! resource holds the whole in-memory file set + which file renders (`entry`) + which the editor shows
//! (`active`). A file SWITCH flushes the editor's live text back into `files[active]` then hydrates the
//! editor from `files[new]`, so per-file edits survive a switch.
//!
//! Persistence follows file count (chotchki): a lone text file saves as a plain `.scad` (today's path),
//! two-or-more (or any binary asset) saves as a `.scadproj`. Binary assets (png/stl heightmaps) ride
//! along in `assets` — not text-editable, but included in the render pack and re-zipped on save, so a
//! project's `import()`/`surface()` resolve.

#![allow(dead_code)] // Phase Z foundation — the document model + seams; wired into render/open/save/tab in Z.3.2–Z.3.5.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bevy::prelude::*;
use fab_scad::scadproj::{self, FilePack};

/// One editable TEXT file in the project — its project-relative name, live content, and dirty flag.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProjectFile {
    /// Project-relative path, e.g. `"shower_holder.scad"` or `"sub/hook.scad"`.
    pub(crate) name: String,
    /// Live content (the entry file's is config-block-stripped, like `EditorBuf`).
    pub(crate) text: String,
    pub(crate) dirty: bool,
}

/// Where the project came from / where Save writes back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProjectHome {
    /// Pasted / the web demo — no on-disk home yet.
    #[default]
    Fresh,
    /// A single `.scad` on disk (native) — Save rewrites it.
    ScadFile(PathBuf),
    /// A `.scadproj` on disk (native) — Save re-zips it.
    ScadProj(PathBuf),
    /// A web `?model=` name — downloads/save-back name from it.
    WebModel(String),
}

/// The open document — ALWAYS a project. `EditorBuf` is the view onto `files[active]`.
#[derive(Resource, Default)]
pub(crate) struct ProjectDoc {
    pub(crate) files: Vec<ProjectFile>,
    /// Binary (non-text) assets keyed by project-relative path — ride-along, not text-editable.
    pub(crate) assets: BTreeMap<String, Vec<u8>>,
    /// Index into `files` of the file that RENDERS.
    pub(crate) entry: usize,
    /// Index into `files` of the file the editor currently shows.
    pub(crate) active: usize,
    pub(crate) home: ProjectHome,
}

impl ProjectDoc {
    /// A one-file project from a bare source (the common case) — entry == active == the sole file.
    pub(crate) fn single(
        name: impl Into<String>,
        text: impl Into<String>,
        home: ProjectHome,
    ) -> Self {
        ProjectDoc {
            files: vec![ProjectFile {
                name: name.into(),
                text: text.into(),
                dirty: false,
            }],
            assets: BTreeMap::new(),
            entry: 0,
            active: 0,
            home,
        }
    }

    /// From a `.scadproj` archive's bytes: text files → editable `files`, binary → `assets`, entry from
    /// the manifest (or the lone `.scad` when manifest-less — `read_scadproj` resolves that).
    pub(crate) fn from_scadproj(bytes: &[u8], home: ProjectHome) -> anyhow::Result<Self> {
        let p = scadproj::read_scadproj(bytes)?;
        let entry_name = p.manifest.entry.clone();
        let mut files = Vec::new();
        let mut assets = BTreeMap::new();
        for (name, body) in p.files {
            if is_text_file(&name) {
                match String::from_utf8(body) {
                    Ok(text) => files.push(ProjectFile {
                        name,
                        text,
                        dirty: false,
                    }),
                    // A "text" file that isn't UTF-8 rides as an opaque asset rather than corrupting.
                    Err(e) => {
                        assets.insert(name, e.into_bytes());
                    }
                }
            } else {
                assets.insert(name, body);
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        let entry = files.iter().position(|f| f.name == entry_name).unwrap_or(0);
        Ok(ProjectDoc {
            files,
            assets,
            entry,
            active: entry,
            home,
        })
    }

    /// More than one file (or any asset) ⇒ it saves as a `.scadproj`, not a bare `.scad`.
    pub(crate) fn is_multifile(&self) -> bool {
        self.files.len() + self.assets.len() > 1
    }

    /// The entry file's project-relative name.
    pub(crate) fn entry_name(&self) -> &str {
        self.files
            .get(self.entry)
            .map(|f| f.name.as_str())
            .unwrap_or("model.scad")
    }

    /// Assemble the render inputs from the CURRENT project state: `(entry bytes, pack)` where the pack is
    /// every OTHER file + all assets, keyed by project-relative path — exactly what the kernel's
    /// `Source::Bytes` resolver consumes. `active_text` is the editor's LIVE text for `files[active]`
    /// (which may be ahead of the stored `files[active].text` before a flush), so a preview reflects an
    /// unsaved edit to ANY file. The caller merges the library closure (BOSL2 …) into the pack.
    pub(crate) fn render_pack(&self, active_text: &str) -> (Vec<u8>, FilePack) {
        let text_at = |i: usize| -> String {
            if i == self.active {
                active_text.to_string()
            } else {
                self.files
                    .get(i)
                    .map(|f| f.text.clone())
                    .unwrap_or_default()
            }
        };
        let main = text_at(self.entry).into_bytes();
        let mut libs: FilePack = Vec::new();
        for i in 0..self.files.len() {
            if i == self.entry {
                continue;
            }
            libs.push((self.files[i].name.clone(), text_at(i).into_bytes()));
        }
        for (name, body) in &self.assets {
            libs.push((name.clone(), body.clone()));
        }
        (main, libs)
    }

    /// Serialize the whole project to `.scadproj` bytes (the ≥2-file save path). `active_text` splices the
    /// editor's live text for `files[active]` so an unsaved edit is captured.
    pub(crate) fn to_scadproj_bytes(&self, active_text: &str) -> anyhow::Result<Vec<u8>> {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (i, f) in self.files.iter().enumerate() {
            let text = if i == self.active {
                active_text.to_string()
            } else {
                f.text.clone()
            };
            files.insert(f.name.clone(), text.into_bytes());
        }
        for (name, body) in &self.assets {
            files.insert(name.clone(), body.clone());
        }
        let project =
            scadproj::project_from_files(files, Some(self.entry_name().to_string()), None)?;
        scadproj::write_scadproj(&project)
    }
}

/// Is this project-relative name a TEXT file the editor can show? `.scad` + the text asset formats; a
/// binary asset (png/binary-stl/3mf) is not. A conservative allowlist — an unknown extension is treated
/// as a binary asset (ride-along, not editable) so we never garble it in a `String`.
fn is_text_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".scad", ".svg", ".json", ".txt", ".md", ".toml", ".csv"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_is_a_one_file_project() {
        let d = ProjectDoc::single("m.scad", "cube(1);", ProjectHome::Fresh);
        assert_eq!(d.files.len(), 1);
        assert!(!d.is_multifile());
        assert_eq!(d.entry_name(), "m.scad");
        // The render pack for a one-file project is (the text, EMPTY libs) — the caller adds the lib
        // closure, so this is byte-identical to today's single-file web render.
        let (main, libs) = d.render_pack("cube(2);"); // live edit ahead of stored text
        assert_eq!(main, b"cube(2);");
        assert!(libs.is_empty());
    }

    #[test]
    fn scadproj_round_trips_into_files_and_assets() {
        // Build a project (2 .scad + 1 binary asset) → .scadproj bytes → ProjectDoc.
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        files.insert("main.scad".into(), b"include <hook.scad>\nhook();".to_vec());
        files.insert("hook.scad".into(), b"module hook(){cube(1);}".to_vec());
        files.insert(
            "heightmap.png".into(),
            vec![0x89, b'P', b'N', b'G', 0x00, 0xFF],
        );
        let bytes = scadproj::write_scadproj(
            &scadproj::project_from_files(files, Some("main.scad".into()), None).unwrap(),
        )
        .unwrap();

        let d = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        assert!(d.is_multifile());
        // .scad files are editable; the png rode into assets.
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.assets.len(), 1);
        assert!(d.assets.contains_key("heightmap.png"));
        assert_eq!(d.entry_name(), "main.scad");

        // The render pack: entry is main, the pack carries hook.scad (relpath) + the png (bytes intact).
        let active_text = &d.files[d.active].text.clone();
        let (main, libs) = d.render_pack(active_text);
        assert_eq!(main, b"include <hook.scad>\nhook();");
        assert!(libs.iter().any(|(k, _)| k == "hook.scad"));
        assert!(
            libs.iter().any(
                |(k, v)| k == "heightmap.png" && v == &vec![0x89, b'P', b'N', b'G', 0x00, 0xFF]
            )
        );
    }

    #[test]
    fn render_pack_reflects_a_live_edit_to_a_non_entry_file() {
        let mut d = ProjectDoc::single(
            "main.scad",
            "include <hook.scad>\nhook();",
            ProjectHome::Fresh,
        );
        d.files.push(ProjectFile {
            name: "hook.scad".into(),
            text: "module hook(){cube(1);}".into(),
            dirty: false,
        });
        // Editing hook.scad (make it active) and previewing must still render the ENTRY with the new hook.
        d.active = 1;
        let (main, libs) = d.render_pack("module hook(){cube(99);}");
        assert_eq!(main, b"include <hook.scad>\nhook();"); // the entry, unchanged
        let hook = libs.iter().find(|(k, _)| k == "hook.scad").unwrap();
        assert_eq!(hook.1, b"module hook(){cube(99);}"); // the LIVE edit, not the stored text
    }

    #[test]
    fn scadproj_save_round_trips() {
        let mut d = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        d.files.push(ProjectFile {
            name: "hook.scad".into(),
            text: "module hook(){}".into(),
            dirty: false,
        });
        let bytes = d.to_scadproj_bytes(&d.files[0].text.clone()).unwrap();
        let back = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        assert_eq!(back.files.len(), 2);
        assert_eq!(back.entry_name(), "main.scad");
    }
}
