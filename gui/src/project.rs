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
    /// Native only: the on-disk root the render reads from — the REAL folder for a loose `.scad`
    /// (no copy, `include`s + save resolve in place) or a temp materialization dir for a `.scadproj`.
    /// `None` on the web (in-memory, rendered via [`render_pack`](Self::render_pack) + `Source::Bytes`).
    pub(crate) base_dir: Option<PathBuf>,
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
            base_dir: None,
        }
    }

    /// A native project from files ALREADY on disk under `base_dir` (a loose `.scad` + its folder
    /// siblings, or a `.scadproj` freshly materialized to a temp dir). `paths` are absolute; each
    /// file's project-relative name is its path minus `base_dir`. `entry` is the path that renders.
    /// `base_dir` roots the render's `import()`/`surface()` and its library fs fallback, and for a
    /// loose project it IS the user's folder — so a model's assets and its Save both resolve in
    /// place, no second copy. (Pure path logic; only `native_paths` is native-gated. The `--script`
    /// harness compiles on both targets, so this must too.)
    pub(crate) fn from_disk(base_dir: PathBuf, paths: &[PathBuf], entry: &std::path::Path) -> Self {
        let rel = |p: &std::path::Path| -> String {
            p.strip_prefix(&base_dir)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        };
        let files: Vec<ProjectFile> = paths
            .iter()
            .map(|p| ProjectFile {
                name: rel(p),
                // Load the content now (SW.3): the doc IS the render source, so the bytes must be
                // present, not lazy. A raw entry keeps its fab:config block (a comment the render
                // ignores; the editor strips it for display, save re-bakes it).
                text: std::fs::read_to_string(p).unwrap_or_default(),
                dirty: false,
            })
            .collect();
        let entry = paths.iter().position(|p| p == entry).unwrap_or(0);
        let home = ProjectHome::ScadFile(
            base_dir.join(&files[entry.min(files.len().saturating_sub(1))].name),
        );
        ProjectDoc {
            files,
            assets: BTreeMap::new(),
            entry,
            active: entry,
            home,
            base_dir: Some(base_dir),
        }
    }

    /// Native render paths — `base_dir.join(name)` per file, in `files` order (so a `FileList` derived
    /// from this aligns index-for-index with `active`/`entry`). Empty when `base_dir` is unset (web).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn native_paths(&self) -> Vec<PathBuf> {
        let Some(base) = self.base_dir.as_ref() else {
            return Vec::new();
        };
        self.files.iter().map(|f| base.join(&f.name)).collect()
    }

    /// The [`EditorBuf`](crate::state::EditorBuf) path for file `i`: the real on-disk path on native,
    /// the bare project-relative NAME on the web. The browser has no filesystem, so there `editor.path`
    /// isn't a location at all — it's an identity TOKEN answering "does the live buffer belong to
    /// `files[active]`?". One place decides, so the two can't drift.
    pub(crate) fn editor_path(&self, i: usize) -> PathBuf {
        let name = self.files.get(i).map(|f| f.name.as_str()).unwrap_or("");
        match self.base_dir.as_ref() {
            Some(base) => base.join(name),
            None => PathBuf::from(name),
        }
    }

    /// Does `path` still name `files[active]`? The flush-before-switch predicate: when it's false the
    /// live buffer is NOT this file's, so flushing it would write one file's text over another's.
    /// A rename that forgets to re-point `editor.path` silently turns this false, and the next switch
    /// then discards every unsaved edit — which is why both callers go through here.
    pub(crate) fn editor_holds(&self, path: &std::path::Path) -> bool {
        self.active < self.files.len() && self.editor_path(self.active) == path
    }

    /// Flush the editor's live text back into `files[active]` before a switch, so a per-file edit
    /// survives moving away and back. Marks the file dirty when the text actually changed.
    ///
    /// The incoming text is the editor buffer, which is `fab:config`-STRIPPED — so the stored file's
    /// block is carried across rather than flushed away ([`config::reattach_config_block`]). Two things
    /// fall out: the doc keeps the persisted slicing plan (the shadow file used to hold the only other
    /// copy), and merely OPENING a model with a block and clicking another tab no longer marks it dirty.
    pub(crate) fn flush_active(&mut self, text: &str) {
        if let Some(f) = self.files.get_mut(self.active) {
            let merged = crate::config::reattach_config_block(&f.text, text);
            if f.text != merged {
                f.dirty = true;
            }
            f.text = merged;
        }
    }

    /// Make file `i` the active (editor-shown) one. No-op when out of range.
    pub(crate) fn set_active(&mut self, i: usize) {
        if i < self.files.len() {
            self.active = i;
        }
    }

    /// Make file `i` the render ENTRY (the "primary render target"). No-op when out of range.
    pub(crate) fn set_entry(&mut self, i: usize) {
        if i < self.files.len() {
            self.entry = i;
        }
    }

    /// A project-relative name not already taken by a file OR an asset — `stem.scad`, else `stem-1.scad`,
    /// `stem-2.scad`, … So an added/renamed file never silently overwrites a sibling.
    pub(crate) fn unique_name(&self, want: &str) -> String {
        let taken = |n: &str| self.files.iter().any(|f| f.name == n) || self.assets.contains_key(n);
        if !taken(want) {
            return want.to_string();
        }
        let (stem, ext) = match want.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (want.to_string(), String::new()),
        };
        (1..)
            .map(|n| format!("{stem}-{n}{ext}"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| want.to_string())
    }

    /// Add a TEXT file to the project (name de-duplicated), returning its index. Marked dirty — it's a
    /// change the next save persists. It reaches disk on that Save, not before: a `.scad` renders
    /// straight out of the doc via [`hybrid_pack`](Self::hybrid_pack).
    pub(crate) fn add_file(&mut self, name: &str, text: String) -> usize {
        let name = self.unique_name(name);
        self.files.push(ProjectFile {
            name,
            text,
            dirty: true,
        });
        self.files.len() - 1
    }

    /// Import raw bytes under `name` — a UTF-8 text file (.scad + the text formats) becomes an editable
    /// file, anything else a binary asset. Returns the FINAL (de-duplicated) project-relative name so the
    /// caller can materialize it. Marks a text import dirty (a change to persist on save).
    pub(crate) fn import(&mut self, name: &str, bytes: Vec<u8>) -> String {
        let uniq = self.unique_name(name);
        if is_text_file(&uniq) {
            match String::from_utf8(bytes) {
                Ok(text) => {
                    self.files.push(ProjectFile {
                        name: uniq.clone(),
                        text,
                        dirty: true,
                    });
                }
                // A "text" name that isn't UTF-8 rides as an opaque asset rather than corrupting.
                Err(e) => {
                    self.assets.insert(uniq.clone(), e.into_bytes());
                }
            }
        } else {
            self.assets.insert(uniq.clone(), bytes);
        }
        uniq
    }

    /// Rename file `i` to `new_name` (de-duplicated, keeping the entry/active indices — same slot, new
    /// name), returning the OLD name so the caller can move its on-disk/temp copy. `None` when out of
    /// range, blank, or unchanged. NOTE: this does NOT rewrite `include`/`use` refs in sibling files — a
    /// rename can break a reference, exactly as it would in a folder; the user updates the reference.
    pub(crate) fn rename_file(&mut self, i: usize, new_name: &str) -> Option<String> {
        let new_name = new_name.trim();
        if new_name.is_empty() || i >= self.files.len() || self.files[i].name == new_name {
            return None;
        }
        let uniq = self.unique_name(new_name);
        let old = std::mem::replace(&mut self.files[i].name, uniq);
        self.files[i].dirty = true;
        Some(old)
    }

    /// Remove file `i`, returning its name (for the caller to delete its on-disk/temp copy). Refuses to
    /// drop the LAST file (a project needs an entry) and fixes up the `entry`/`active` indices — a delete
    /// of the entry re-homes it to file 0. `None` when out of range or it's the sole file.
    pub(crate) fn remove_file(&mut self, i: usize) -> Option<String> {
        if i >= self.files.len() || self.files.len() <= 1 {
            return None;
        }
        let removed = self.files.remove(i);
        let fix = |idx: &mut usize| {
            if *idx > i {
                *idx -= 1;
            } else if *idx == i {
                *idx = 0; // the removed slot's referent is gone — fall back to the first file
            }
        };
        fix(&mut self.entry);
        fix(&mut self.active);
        Some(removed.name)
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
            base_dir: None,
        })
    }

    /// More than one file (or any asset) ⇒ it saves as a `.scadproj`, not a bare `.scad`.
    pub(crate) fn is_multifile(&self) -> bool {
        self.files.len() + self.assets.len() > 1
    }

    /// The DOCUMENT's name stem (Z.3.9) — what the whole project is called when it leaves as ONE file:
    /// a web Save download, a save-back / publish upload, the Publish dialog's pre-filled title. This is
    /// deliberately NOT a file's name. `editor.path` (what those callers used to read) is the ACTIVE
    /// file, so a multi-file project published while viewing `lib.scad` uploaded `lib.scadproj`; and for
    /// a `.scadproj` the entry keeps its own inside-the-archive name (`main.scad`), which is not the
    /// identity the site knows the item by. `None` for a [`ProjectHome::Fresh`] document — it has no name
    /// of its own yet, so the caller supplies the fallback (the publish title's slug, or `model`).
    pub(crate) fn doc_stem(&self) -> Option<String> {
        let stem_of = |p: &std::path::Path| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        match &self.home {
            // The `?model=` name, now the item's real title (Z.3.9) rather than its `media_ref` hash.
            ProjectHome::WebModel(n) => stem_of(std::path::Path::new(n)),
            ProjectHome::ScadProj(p) | ProjectHome::ScadFile(p) => stem_of(p),
            ProjectHome::Fresh => None,
        }
    }

    /// The entry file's project-relative name.
    pub(crate) fn entry_name(&self) -> &str {
        self.files
            .get(self.entry)
            .map(|f| f.name.as_str())
            .unwrap_or("model.scad")
    }

    /// The NATIVE render pack (SW.3): `(every file as (project-relative name, text), the entry's name)`
    /// — `Source::Pack`'s shape, and the hybrid loader's overlay. `active_text` is the editor's LIVE
    /// text for `files[active]` (which may be ahead of the stored `files[active].text` before a flush),
    /// so a preview reflects an unsaved edit to ANY file, not just the entry.
    ///
    /// Assets stay OUT deliberately: natively they already sit on disk — at the real project dir for a
    /// loose open, at the materialized temp for a container — which is exactly where `read_import`
    /// looks, so copying their bytes into every render request would buy nothing. That is the whole
    /// difference from [`render_pack`](Self::render_pack), whose browser has no disk to lean on.
    pub(crate) fn hybrid_pack(&self, active_text: &str) -> (Vec<(String, String)>, String) {
        let files = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let text = if i == self.active {
                    active_text.to_string()
                } else {
                    f.text.clone()
                };
                (f.name.clone(), text)
            })
            .collect();
        let entry = self
            .files
            .get(self.entry)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        (files, entry)
    }

    /// The WEB render inputs from the CURRENT project state: `(entry bytes, pack)` where the pack is
    /// every OTHER file + all assets, keyed by project-relative path — exactly what the kernel's
    /// `Source::Bytes` resolver consumes. `active_text` splices the live buffer as in
    /// [`hybrid_pack`](Self::hybrid_pack). The caller merges the library closure (BOSL2 …) into the pack.
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

    /// The editor buffer is `fab:config`-stripped, so a naive flush would delete the persisted slicing
    /// plan from the document — and since SW.3 the document is the only copy of it until Save. Two
    /// consequences pinned here: the block survives a flush, and an untouched file doesn't go dirty
    /// just because the block isn't in the buffer.
    #[test]
    fn flush_keeps_the_config_block_the_editor_never_shows() {
        const BLOCK: &str = "// fab:config v2 {\"printer\":null,\"parts\":[]}";
        let raw = format!("cube(1);\n\n{BLOCK}\n");
        let mut d = ProjectDoc::single("main.scad", raw, ProjectHome::Fresh);

        d.flush_active("cube(1);\n"); // what the editor holds: stripped
        assert!(
            d.files[0].text.contains(BLOCK),
            "the block rode through the flush"
        );
        assert!(!d.files[0].dirty, "an unchanged file is not dirtied by it");

        d.flush_active("cube(2);\n"); // a real edit
        assert!(d.files[0].text.starts_with("cube(2);"));
        assert!(d.files[0].text.contains(BLOCK), "still carried");
        assert!(d.files[0].dirty);

        // A block the user typed themselves wins — this never resurrects a deleted one.
        d.flush_active("cube(3);\n// fab:config v2 {\"printer\":null,\"parts\":[\"mine\"]}\n");
        assert!(!d.files[0].text.contains(BLOCK));
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

    /// `hybrid_pack`'s native twin of the test above, pinning the same entry-vs-active split — the one
    /// the old disk mirror used to mask, since the render read a file rather than these indices. Swap
    /// `active` for `entry` in the splice and this is what catches it.
    #[test]
    fn hybrid_pack_splices_the_live_edit_at_active_and_names_the_entry() {
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
        d.assets.insert("logo.svg".into(), vec![1, 2, 3]);
        d.active = 1; // editing the sibling; the ENTRY is still main.scad

        let (files, entry) = d.hybrid_pack("module hook(){cube(99);}");
        assert_eq!(
            entry, "main.scad",
            "the pack renders the ENTRY, not the view"
        );
        assert_eq!(
            files,
            vec![
                (
                    "main.scad".to_string(),
                    "include <hook.scad>\nhook();".to_string()
                ),
                (
                    "hook.scad".to_string(),
                    "module hook(){cube(99);}".to_string()
                ),
            ],
            "the live text lands on the ACTIVE file and nowhere else"
        );
        assert!(
            !files.iter().any(|(n, _)| n == "logo.svg"),
            "assets stay on disk — no byte-copies ride a native render"
        );
    }

    #[test]
    fn unique_name_dedups_against_files_and_assets() {
        let mut d = ProjectDoc::single("main.scad", "", ProjectHome::Fresh);
        d.assets.insert("logo.svg".into(), vec![1]);
        assert_eq!(d.unique_name("hook.scad"), "hook.scad"); // free
        assert_eq!(d.unique_name("main.scad"), "main-1.scad"); // file taken
        assert_eq!(d.unique_name("logo.svg"), "logo-1.svg"); // asset taken
    }

    #[test]
    fn add_new_and_delete_keep_entry_active_consistent() {
        let mut d = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        // add two files; entry/active stay on main (index 0)
        let a = d.add_file("a.scad", "//a".into());
        let b = d.add_file("b.scad", "//b".into());
        assert_eq!((a, b), (1, 2));
        assert!(d.files[a].dirty && d.files[b].dirty);
        assert_eq!(d.entry, 0);
        // make `b` the entry + active, then delete `a` (a lower index) — both shift down by one.
        d.set_entry(b);
        d.set_active(b);
        assert_eq!(d.remove_file(a).as_deref(), Some("a.scad"));
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.entry_name(), "b.scad"); // entry followed the shift
        assert_eq!(d.files[d.active].name, "b.scad"); // active too
    }

    #[test]
    fn delete_refuses_the_only_file_and_rehomes_a_deleted_entry() {
        let mut d = ProjectDoc::single("only.scad", "", ProjectHome::Fresh);
        assert_eq!(d.remove_file(0), None); // a project needs an entry
        d.add_file("lib.scad", "".into());
        d.set_entry(1);
        d.set_active(1);
        // delete the ENTRY itself → entry + active fall back to file 0.
        assert_eq!(d.remove_file(1).as_deref(), Some("lib.scad"));
        assert_eq!(d.entry, 0);
        assert_eq!(d.active, 0);
    }

    /// Z.3.9: the document's name is the HOME's, never the active/entry file's — a `.scadproj` keeps its
    /// own `main.scad` inside, and the archive is still called what the site knows it by.
    #[test]
    fn doc_stem_names_the_document_not_the_file() {
        let mut d = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        // Fresh has no name of its own — the caller owns the fallback.
        assert_eq!(d.doc_stem(), None);

        d.home = ProjectHome::WebModel("Shower Holder.scadproj".into());
        assert_eq!(d.doc_stem().as_deref(), Some("Shower Holder"));
        // A web `.scad` deep-link, and the degraded (extension-less basename) fallback path.
        d.home = ProjectHome::WebModel("Shower Holder.scad".into());
        assert_eq!(d.doc_stem().as_deref(), Some("Shower Holder"));
        d.home = ProjectHome::WebModel("019f81dd2c3b72839333a1b5ec961d64".into());
        assert_eq!(
            d.doc_stem().as_deref(),
            Some("019f81dd2c3b72839333a1b5ec961d64")
        );

        // Native homes read their path, not the entry — even when the two disagree.
        d.home = ProjectHome::ScadProj(PathBuf::from("/models/Trash Can Brace.scadproj"));
        assert_eq!(d.doc_stem().as_deref(), Some("Trash Can Brace"));
        d.home = ProjectHome::ScadFile(PathBuf::from("/models/hook.scad"));
        assert_eq!(d.doc_stem().as_deref(), Some("hook"));

        // A home with no file component is no name at all.
        d.home = ProjectHome::WebModel(String::new());
        assert_eq!(d.doc_stem(), None);
    }

    #[test]
    fn rename_dedups_keeps_indices_and_returns_the_old_name() {
        let mut d = ProjectDoc::single("main.scad", "", ProjectHome::Fresh);
        d.add_file("hook.scad", "".into());
        d.set_entry(1);
        d.set_active(1);
        // rename the entry file; the entry/active indices stay on slot 1, now "part.scad".
        assert_eq!(d.rename_file(1, "part.scad").as_deref(), Some("hook.scad"));
        assert_eq!(d.entry_name(), "part.scad");
        assert_eq!(d.entry, 1);
        // a blank / unchanged rename is a no-op; a collision de-dups.
        assert_eq!(d.rename_file(1, "   "), None);
        assert_eq!(d.rename_file(1, "part.scad"), None);
        assert_eq!(d.rename_file(1, "main.scad").as_deref(), Some("part.scad")); // collides → main-1
        assert_eq!(d.files[1].name, "main-1.scad");
    }

    #[test]
    fn import_routes_text_to_files_and_binary_to_assets() {
        let mut d = ProjectDoc::single("main.scad", "", ProjectHome::Fresh);
        assert_eq!(
            d.import("hook.scad", b"module hook(){}".to_vec()),
            "hook.scad"
        );
        assert!(d.files.iter().any(|f| f.name == "hook.scad"));
        // a PNG (binary) → assets, not a garbled String.
        assert_eq!(d.import("map.png", vec![0x89, b'P', b'N', b'G']), "map.png");
        assert!(d.assets.contains_key("map.png"));
        // a name collision de-dups.
        assert_eq!(d.import("hook.scad", b"// two".to_vec()), "hook-1.scad");
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
