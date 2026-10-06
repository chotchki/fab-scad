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

use std::collections::{BTreeMap, BTreeSet};
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

impl ProjectHome {
    /// TG.1: does Save have to write a STRUCTURAL change (add / delete / rename / set-entry) for it to
    /// stick? A container re-zips its file set + manifest, and a Fresh or web document only exists as
    /// what Save writes. A loose folder doesn't: Add and Rename land on disk at once and Delete and
    /// set-entry are session-only (SW.3 doctrine, design Decision 7), so none of them is unsaved there.
    pub(crate) fn persists_structure(&self) -> bool {
        !matches!(self, ProjectHome::ScadFile(_))
    }
}

/// TG.2: a process-unique document identity. Every [`ProjectDoc`] constructor mints one (`Default` is
/// the minting path, and every constructor goes through it), and [`EditorBuf`](crate::state::EditorBuf)
/// records the id whose file it holds. Paths alone can't say whose buffer it is: two `.scadproj`s with
/// the same stem materialize to the same temp dir, and re-opening a file reproduces its own paths, so
/// either used to hand the OLD document's editor text to the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DocId(u64);

impl Default for DocId {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        DocId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// The open document — ALWAYS a project. `EditorBuf` is the view onto `files[active]`.
#[derive(Resource, Default)]
pub(crate) struct ProjectDoc {
    /// TG.2: minted at construction, never reassigned — see [`DocId`].
    id: DocId,
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
    /// The `.scadproj` manifest's title, kept from the archive it opened from (TG.1) — re-zip used to
    /// drop it. `None` for anything that didn't come from a titled archive.
    pub(crate) title: Option<String>,
    /// TG.1: a file was added, deleted or renamed, or the entry moved, since the last save. Counts
    /// toward [`is_dirty`](Self::is_dirty) only where the home persists structure.
    structure_dirty: bool,
    /// TG.1: binary assets imported since the last save — the per-row marker a binary has no
    /// `ProjectFile::dirty` to carry.
    unsaved_assets: BTreeSet<String>,
    /// TG.1: bumped by every mutation, so a save that snapshotted at `rev` can tell whether anything
    /// moved under it before [`mark_saved`](Self::mark_saved) clears the flags.
    rev: u64,
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
            ..Default::default()
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
            ..Default::default()
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

    /// This document's identity (TG.2).
    pub(crate) fn id(&self) -> DocId {
        self.id
    }

    /// Does the editor buffer belong to `files[active]` of THIS document? The flush-before-switch
    /// predicate: when it's false the live buffer is NOT this file's, so flushing it would write one
    /// file's text over another's. TG.2: the owner id must match as well as the path — a same-stem
    /// archive or a re-open reproduces the path exactly, and path equality alone handed the previous
    /// document's buffer to the new one. A rename that forgets to re-point `editor.path` silently turns
    /// this false, and the next switch then discards every unsaved edit. Every switch-time flush goes
    /// through here, and so does every save (TG.3: `save_doc_action`'s flush and
    /// [`DocSnapshot::capture`](crate::save::DocSnapshot::capture)'s splice).
    pub(crate) fn editor_holds(&self, editor: &crate::state::EditorBuf) -> bool {
        editor.owner == Some(self.id)
            && self.active < self.files.len()
            && self.editor_path(self.active) == editor.path
    }

    /// TG.2 (design Risks): the buffer names one of this document's files but another document owns
    /// it — the aliasing `DocId` exists to rule out, i.e. a loader that swapped the document without
    /// [`adopt`](crate::file_ops::adopt). `sync_doc_state` debug-asserts it never holds.
    pub(crate) fn editor_aliases(&self, editor: &crate::state::EditorBuf) -> bool {
        editor.owner.is_some_and(|o| o != self.id)
            && (0..self.files.len()).any(|i| self.editor_path(i) == editor.path)
    }

    /// The `fab:config` the ENTRY carries — what the document last saved (or opened with) for its
    /// render, since a save lands its baked block in the entry's text. TG.1: a fresh parts build stashes
    /// this, never the VIEWED file's block, so the rebuilt plan and the re-taken baseline are the
    /// saved one whichever file is on screen.
    pub(crate) fn entry_config(&self) -> Option<crate::config::FabConfig> {
        self.files
            .get(self.entry)
            .and_then(|f| crate::config::read_config_block(&f.text))
    }

    /// Flush the editor's live text back into `files[active]` before a switch, so a per-file edit
    /// survives moving away and back. Marks the file dirty (and bumps `rev`) only when the text actually
    /// changed.
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
                self.rev += 1;
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

    /// Make file `i` the render ENTRY (the "primary render target"). No-op when out of range or already
    /// the entry — only a real move is a structural change (TG.1).
    pub(crate) fn set_entry(&mut self, i: usize) {
        if i < self.files.len() && i != self.entry {
            self.entry = i;
            self.touch_structure();
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
        self.touch_structure();
        self.files.len() - 1
    }

    /// Import raw bytes under `name` — a UTF-8 text file (.scad + the text formats) becomes an editable
    /// file, anything else a binary asset. Returns the FINAL (de-duplicated) project-relative name so the
    /// caller can materialize it. Marks the import unsaved either way (TG.1): a text file through its own
    /// `dirty`, a binary through `unsaved_assets`, and the file set through `structure_dirty`.
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
                    self.unsaved_assets.insert(uniq.clone());
                }
            }
        } else {
            self.assets.insert(uniq.clone(), bytes);
            self.unsaved_assets.insert(uniq.clone());
        }
        self.touch_structure();
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
        let old = std::mem::replace(&mut self.files[i].name, uniq.clone());
        self.follow_loose_home(&old, &uniq);
        // TG.1: structural, not a text edit — so a loose rename (already on disk) reads saved.
        self.touch_structure();
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
        let entry = self.entry_name().to_string();
        self.follow_loose_home(&removed.name, &entry);
        self.touch_structure();
        Some(removed.name)
    }

    /// TG.7: a loose home names the file the document is called by (title, chip, card, publish stem),
    /// so when that file is renamed — or dropped from the session, where `to` is the new entry — the
    /// home follows it. Same folder only: a `to` with a directory part would re-aim
    /// [`plan`](crate::save::plan)'s write dir at a subfolder, so that one keeps the stale name.
    fn follow_loose_home(&mut self, from: &str, to: &str) {
        if let ProjectHome::ScadFile(p) = &mut self.home
            && p.file_name().is_some_and(|n| n == from)
            && std::path::Path::new(to).components().count() == 1
        {
            p.set_file_name(to);
        }
    }

    /// A structural mutation landed: the file set or entry moved (TG.1).
    fn touch_structure(&mut self) {
        self.structure_dirty = true;
        self.rev += 1;
    }

    /// TG.1: would Save write something it hasn't? Any file with unsaved text, any unsaved asset, or a
    /// structural change where the home persists structure. The live editor buffer and the cut plan
    /// are NOT in here — they live outside the document; [`DocState`](crate::state::DocState) folds
    /// them in.
    pub(crate) fn is_dirty(&self) -> bool {
        self.files.iter().any(|f| f.dirty)
            || !self.unsaved_assets.is_empty()
            || (self.structure_dirty && self.home.persists_structure())
    }

    /// The mutation counter a save snapshots before it writes (TG.1).
    pub(crate) fn rev(&self) -> u64 {
        self.rev
    }

    /// TG.1: a save that captured the document at `at_rev` succeeded — clear every unsaved flag. A no-op
    /// returning `false` when `rev` moved since the capture: a later edit (made while a dialog or an
    /// upload was in flight) didn't reach that write, so nothing may read saved.
    pub(crate) fn mark_saved(&mut self, at_rev: u64) -> bool {
        if self.rev != at_rev {
            return false;
        }
        self.structure_dirty = false;
        self.unsaved_assets.clear();
        for f in &mut self.files {
            f.dirty = false;
        }
        true
    }

    /// TG.3: a PARTIAL save landed (a loose folder writes file by file) — clear just the `files`
    /// indices and `assets` it wrote; everything else stays unsaved. Same stale-`rev` rule as
    /// [`mark_saved`](Self::mark_saved). Structure isn't touched: the only home that saves per file is
    /// a loose folder, which doesn't persist structure.
    pub(crate) fn mark_written(&mut self, at_rev: u64, files: &[usize], assets: &[String]) -> bool {
        if self.rev != at_rev {
            return false;
        }
        for &i in files {
            if let Some(f) = self.files.get_mut(i) {
                f.dirty = false;
            }
        }
        for a in assets {
            self.unsaved_assets.remove(a);
        }
        true
    }

    /// TG.6: `name` (a file or an asset) is on disk byte-for-byte as the document holds it — a loose Add
    /// copies the file into the folder at once — so it carries no unsaved marker. Leaves `rev` and
    /// structure alone: the add that preceded it already moved both, and a loose home ignores structure.
    pub(crate) fn mark_on_disk(&mut self, name: &str) {
        if let Some(f) = self.files.iter_mut().find(|f| f.name == name) {
            f.dirty = false;
        }
        self.unsaved_assets.remove(name);
    }

    /// The row marker for file `i` (TG.1): its stored flag, or, for the active file, live edits in the
    /// buffer that no flush has carried into the document yet. `editor` only counts when it HOLDS
    /// `files[active]` — a buffer that belongs to no file marks no row.
    pub(crate) fn file_unsaved(&self, i: usize, editor: &crate::state::EditorBuf) -> bool {
        self.files.get(i).is_some_and(|f| f.dirty)
            || (i == self.active && editor.dirty && self.editor_holds(editor))
    }

    /// The row marker for binary asset `name` (TG.1).
    pub(crate) fn asset_unsaved(&self, name: &str) -> bool {
        self.unsaved_assets.contains(name)
    }

    /// From a `.scadproj` archive's bytes: text files → editable `files`, binary → `assets`, entry from
    /// the manifest (or the lone `.scad` when manifest-less — `read_scadproj` resolves that).
    pub(crate) fn from_scadproj(bytes: &[u8], home: ProjectHome) -> anyhow::Result<Self> {
        let p = scadproj::read_scadproj(bytes)?;
        let entry_name = p.manifest.entry.clone();
        let title = p.manifest.title.clone();
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
            title,
            ..Default::default()
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

    /// TG.5: the folder this DOCUMENT lives in — a `.scadproj`'s folder, or a loose document's — where
    /// Save As starts. `None` for a document with no file (fresh, or a web name). Not the user's home
    /// ([`file_ops::home_dir`](crate::file_ops::home_dir)), and never `base_dir`, which for a
    /// `.scadproj` is the temp it unpacked to.
    pub(crate) fn home_dir(&self) -> Option<PathBuf> {
        match &self.home {
            ProjectHome::ScadProj(p) | ProjectHome::ScadFile(p) => {
                p.parent().map(|d| d.to_path_buf())
            }
            ProjectHome::Fresh | ProjectHome::WebModel(_) => None,
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

    /// TG.1: a document fresh out of any constructor has nothing to save.
    #[test]
    fn constructors_start_clean() {
        let single = ProjectDoc::single("m.scad", "cube(1);", ProjectHome::Fresh);
        assert!(!single.is_dirty());

        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        files.insert("main.scad".into(), b"cube(1);".to_vec());
        files.insert("logo.png".into(), vec![0x89, b'P', b'N', b'G']);
        let bytes = scadproj::write_scadproj(
            &scadproj::project_from_files(files, Some("main.scad".into()), Some("Brace".into()))
                .unwrap(),
        )
        .unwrap();
        let proj = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        assert!(!proj.is_dirty());
        assert!(!proj.asset_unsaved("logo.png"));
        assert_eq!(
            proj.title.as_deref(),
            Some("Brace"),
            "the manifest title is kept"
        );

        let dir = std::env::temp_dir().join(format!("fab_tg1_clean_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.scad"), "cube(1);").unwrap();
        std::fs::write(dir.join("b.scad"), "sphere(1);").unwrap();
        let loose = ProjectDoc::from_disk(
            dir.clone(),
            &[dir.join("a.scad"), dir.join("b.scad")],
            &dir.join("a.scad"),
        );
        assert!(!loose.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A saved two-file document in `home` (the table's starting point).
    fn saved_doc(home: ProjectHome) -> ProjectDoc {
        let mut d = ProjectDoc::single("main.scad", "cube(1);", home);
        d.add_file("lib.scad", "module m(){}".into());
        assert!(d.mark_saved(d.rev()));
        assert!(!d.is_dirty(), "the fixture starts saved");
        d
    }

    /// TG.1: every mutation Save would write, per home. Text and asset changes are unsaved
    /// everywhere; STRUCTURE (add / delete / rename / set-entry as such) only where the home persists
    /// it — a loose folder already holds an add or rename, and keeps delete and the entry session-only.
    #[test]
    fn every_mutation_marks_the_document_per_home() {
        type Mutation = fn(&mut ProjectDoc);
        // (what, mutation, unsaved in a loose folder?) — every one is unsaved in the other homes.
        let table: [(&str, Mutation, bool); 8] = [
            ("edit the active text", |d| d.flush_active("cube(2);"), true),
            (
                "add a text file",
                |d| {
                    d.add_file("hook.scad", "".into());
                },
                true,
            ),
            (
                "import a text file",
                |d| {
                    d.import("hook.scad", b"// h".to_vec());
                },
                true,
            ),
            (
                "import a binary",
                |d| {
                    d.import("logo.png", vec![0x89, b'P']);
                },
                true,
            ),
            (
                "rename",
                |d| {
                    d.rename_file(1, "util.scad");
                },
                false,
            ),
            (
                "delete",
                |d| {
                    d.remove_file(1);
                },
                false,
            ),
            ("set the entry", |d| d.set_entry(1), false),
            (
                "edit then switch away",
                |d| {
                    d.flush_active("cube(3);");
                    d.set_active(1);
                },
                true,
            ),
        ];
        let homes = [
            (ProjectHome::Fresh, false),
            (ProjectHome::WebModel("Brace.scadproj".into()), false),
            (
                ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")),
                false,
            ),
            (ProjectHome::ScadFile(PathBuf::from("/m/main.scad")), true),
        ];
        for (home, loose) in homes {
            for (what, mutate, unsaved_when_loose) in table {
                let mut d = saved_doc(home.clone());
                let rev = d.rev();
                mutate(&mut d);
                assert!(d.rev() > rev, "{what} on {home:?} didn't bump rev");
                let want = !loose || unsaved_when_loose;
                assert_eq!(d.is_dirty(), want, "{what} on {home:?}");
            }
        }
    }

    /// TG.1: what is NOT a change stays clean — a view switch, re-picking the current entry, an
    /// unchanged flush, a no-op rename.
    #[test]
    fn non_changes_stay_clean() {
        let mut d = saved_doc(ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")));
        let rev = d.rev();
        d.set_active(1);
        d.set_entry(0); // already the entry
        d.flush_active("module m(){}"); // lib.scad's own text
        assert_eq!(d.rename_file(1, "lib.scad"), None);
        assert!(!d.is_dirty());
        assert_eq!(d.rev(), rev, "nothing moved");
    }

    /// TG.1: the row markers. A binary gets one; the active row marks LIVE edits, but only from a
    /// buffer that actually holds it.
    #[test]
    fn row_markers_cover_assets_and_live_edits() {
        let mut d = saved_doc(ProjectHome::Fresh);
        let mut e = crate::state::EditorBuf {
            path: d.editor_path(0),
            owner: Some(d.id()),
            ..Default::default()
        };
        assert!(!d.file_unsaved(0, &e));
        e.dirty = true; // typed, not flushed
        assert!(d.file_unsaved(0, &e), "the active row marks as you type");
        assert!(!d.file_unsaved(1, &e), "and only the active row");
        e.path = PathBuf::from("elsewhere.scad");
        assert!(
            !d.file_unsaved(0, &e),
            "a buffer that holds no file marks no row"
        );

        d.import("logo.png", vec![0x89, b'P']);
        assert!(d.asset_unsaved("logo.png"));
        assert!(d.mark_saved(d.rev()));
        assert!(!d.asset_unsaved("logo.png"));
    }

    /// TG.2: what `sync_doc_state` debug-asserts never happens — another document's buffer at one of
    /// this one's paths (a same-stem archive swapped in without `adopt`). An unowned buffer, one at a
    /// path the document doesn't have, and this document's own buffer on any file are all fine.
    #[test]
    fn editor_aliases_only_another_documents_buffer_at_our_path() {
        let d = saved_doc(ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")));
        let other = ProjectDoc::default().id();
        let at = |owner, path| crate::state::EditorBuf {
            path,
            owner,
            ..Default::default()
        };
        assert!(d.editor_aliases(&at(Some(other), d.editor_path(1))));
        assert!(!d.editor_aliases(&at(Some(d.id()), d.editor_path(1))));
        assert!(!d.editor_aliases(&at(None, d.editor_path(0))));
        assert!(!d.editor_aliases(&at(Some(other), PathBuf::from("/x/else.scad"))));
        // The entry's block is what a fresh build re-applies (TG.1); none here.
        assert!(d.entry_config().is_none());
        let mut c = ProjectDoc::single(
            "main.scad",
            crate::config::with_config_block(
                "cube(1);",
                &[],
                Some(crate::config::PrinterCfg {
                    bed: [200.0, 210.0, 220.0],
                }),
            ),
            ProjectHome::Fresh,
        );
        c.add_file("lib.scad", String::new());
        c.set_active(1); // the viewed file never decides it
        let cfg = c.entry_config().expect("the entry's block");
        assert_eq!(cfg.printer.map(|p| p.bed), Some([200.0, 210.0, 220.0]));
    }

    /// TG.1: `mark_saved` clears what the save captured — and nothing, if the document moved after the
    /// capture (an edit made while a dialog or an upload was in flight never reached that write).
    #[test]
    fn mark_saved_clears_only_at_the_captured_rev() {
        let mut d = saved_doc(ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")));
        d.flush_active("cube(2);");
        d.import("logo.png", vec![1]);
        let captured = d.rev();
        d.add_file("late.scad", "".into()); // lands after the snapshot
        assert!(!d.mark_saved(captured), "a stale rev clears nothing");
        assert!(d.is_dirty());
        assert!(d.files[0].dirty && d.asset_unsaved("logo.png"));

        assert!(d.mark_saved(d.rev()));
        assert!(!d.is_dirty());
        assert!(d.files.iter().all(|f| !f.dirty));
    }

    #[test]
    fn scadproj_save_round_trips() {
        let mut d = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        d.files.push(ProjectFile {
            name: "hook.scad".into(),
            text: "module hook(){}".into(),
            dirty: false,
        });
        d.title = Some("Trash Can Brace".into());
        // TG.3: through the ONE serializer Save uses (a `DocSnapshot` into `rezip_project`).
        let save = |d: &ProjectDoc| {
            let snap = crate::save::DocSnapshot::capture(d, &crate::state::EditorBuf::default());
            let printer = crate::config::PrinterCfg {
                bed: [256.0, 256.0, 256.0],
            };
            crate::jobs::rezip_project(&snap, &[], printer, &BTreeMap::new()).unwrap()
        };
        let back = ProjectDoc::from_scadproj(&save(&d), ProjectHome::Fresh).unwrap();
        assert_eq!(back.files.len(), 2);
        assert_eq!(back.entry_name(), "main.scad");
        // The manifest title rides the round trip (re-zip used to write `None`).
        assert_eq!(back.title.as_deref(), Some("Trash Can Brace"));
        let twice = ProjectDoc::from_scadproj(&save(&back), ProjectHome::Fresh).unwrap();
        assert_eq!(twice.title.as_deref(), Some("Trash Can Brace"));
    }

    /// TG.3: a partial save clears exactly what it wrote — and nothing once the document moved.
    #[test]
    fn mark_written_clears_only_what_landed() {
        let mut d = saved_doc(ProjectHome::ScadFile(PathBuf::from("/m/main.scad")));
        d.flush_active("cube(2);");
        d.set_active(1);
        d.flush_active("module m(){cube(2);}");
        d.import("logo.png", vec![1]);
        let rev = d.rev();
        assert!(d.mark_written(rev, &[0], &[]));
        assert!(!d.files[0].dirty && d.files[1].dirty);
        assert!(d.asset_unsaved("logo.png") && d.is_dirty());
        assert!(d.mark_written(rev, &[1], &["logo.png".into()]));
        assert!(!d.is_dirty());
        d.flush_active("module m(){cube(3);}");
        assert!(
            !d.mark_written(rev, &[1], &[]),
            "a stale rev clears nothing"
        );
        assert!(d.files[1].dirty);
    }
}
