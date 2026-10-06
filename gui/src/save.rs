//! TG.3: the ONE save path. Save, Cmd/Ctrl+S and (via TG.4/TG.5) the site save and Save As all route
//! through here, so where Save goes, what it serializes, and what it reports can't drift per platform
//! again — the old `save_buffer` was three branches inside `panel_ui`, each with its own idea of all
//! three.
//!
//! - [`plan`] is the single table of "where does Save go" for a document (also the hover text and the
//!   Project card's Save rule, TG.4/TG.7).
//! - [`DocSnapshot`] is the ONLY input the serializers ([`rezip_project`](crate::jobs::rezip_project),
//!   `project_source_variant`) accept, and it always carries the live editor text, so an unflushed
//!   edit can't be left out of a save by a caller forgetting to flush.
//! - [`atomic_write`] replaces a file only once its successor is complete on disk.
//! - [`save_doc_action`] handles [`PanelCmd::Save`] and posts every outcome to [`Status`].
//!
//! Like `file_ops`, the rules are cfg-free and unit-tested on native; the system's wasm arm is a thin
//! shim over them, since wasm has no test harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::prelude::*;

use crate::config;
use crate::project::{ProjectDoc, ProjectHome};
use crate::state::{
    DocState, EditorBuf, PanelCmd, Part, Parts, Platform, SaveTarget, SceneCfg, Status,
};

/// Where Save goes for the open document (design Decision 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SavePlan {
    /// A `.scadproj` on disk: rewrite that archive, every file and asset.
    Rezip(PathBuf),
    /// A loose folder: the entry (config baked) and every other unsaved file, back into it.
    WriteLoose(PathBuf),
    /// A desktop document with no file yet: Save asks where, as Save As does.
    NeedsSaveAs,
    /// The web: download the document under this file name (`.scadproj` for more than one file).
    Download(String),
    /// A hotchkiss.io item with a save-back target: Save updates the item on the site.
    Site,
}

/// The save table: platform, then home. File count only picks the download's extension — a native
/// home writes where it lives whatever its size.
pub(crate) fn plan(doc: &ProjectDoc, platform: Platform, has_site_target: bool) -> SavePlan {
    match platform {
        Platform::Web if has_site_target => SavePlan::Site,
        Platform::Web => SavePlan::Download(download_name(doc)),
        Platform::Desktop => match &doc.home {
            ProjectHome::ScadProj(p) => SavePlan::Rezip(p.clone()),
            ProjectHome::ScadFile(p) => {
                SavePlan::WriteLoose(p.parent().unwrap_or(p.as_path()).to_path_buf())
            }
            // A web name has no folder to write into on a desktop; neither does a fresh document.
            ProjectHome::Fresh | ProjectHome::WebModel(_) => SavePlan::NeedsSaveAs,
        },
    }
}

/// What Save itself does: [`plan`], except a hotchkiss.io item downloads like any web document
/// (spec: "On the web: Save downloads the document"). The site update stays its own deliberate
/// button — a keystroke must not replace the published item's files.
pub(crate) fn save_route(doc: &ProjectDoc, platform: Platform, has_site_target: bool) -> SavePlan {
    match plan(doc, platform, has_site_target) {
        SavePlan::Site => SavePlan::Download(download_name(doc)),
        p => p,
    }
}

/// The web download's file name: the document's own stem (Z.3.9), else the entry's (a fresh demo is
/// `demo.scad`), else `model`; `.scadproj` for a multi-file document, `.scad` for one file.
pub(crate) fn download_name(doc: &ProjectDoc) -> String {
    let stem = doc
        .doc_stem()
        .or_else(|| entry_stem(doc))
        .unwrap_or_else(|| "model".into());
    let ext = if doc.is_multifile() {
        fab_scad::scadproj::PROJECT_EXT
    } else {
        "scad"
    };
    format!("{stem}.{ext}")
}

/// What the status line calls the document: its home's file name, the web item's name, or `untitled`.
pub(crate) fn doc_name(doc: &ProjectDoc) -> String {
    match &doc.home {
        ProjectHome::ScadProj(p) | ProjectHome::ScadFile(p) => p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
        ProjectHome::WebModel(n) => n.clone(),
        ProjectHome::Fresh => "untitled".into(),
    }
}

/// One text file as Save will write it.
#[derive(Clone, Debug)]
pub(crate) struct SnapFile {
    pub(crate) name: String,
    pub(crate) text: String,
    /// Holds text Save hasn't written: the file's own flag, or a live edit that differs from it.
    pub(crate) unsaved: bool,
}

/// The document exactly as Save writes it, captured at [`rev`](Self::rev). Fields are private and
/// [`capture`](Self::capture) is the only constructor, so every serializer sees the live editor text
/// — the web site-save shipped the active file without its unflushed edit because splicing it was a
/// convention each caller had to remember.
#[derive(Clone, Debug)]
pub(crate) struct DocSnapshot {
    files: Vec<SnapFile>,
    assets: BTreeMap<String, Vec<u8>>,
    unsaved_assets: Vec<String>,
    entry: usize,
    title: Option<String>,
    rev: u64,
}

impl DocSnapshot {
    /// Snapshot `doc` with `editor`'s live text spliced over `files[active]` — when, and only when, the
    /// editor HOLDS that file ([`ProjectDoc::editor_holds`]); a buffer from another document or a
    /// path load is never written into this one. The splice carries the stored `fab:config` block
    /// across exactly as a flush would.
    pub(crate) fn capture(doc: &ProjectDoc, editor: &EditorBuf) -> Self {
        let live = doc.editor_holds(editor).then_some(editor.text.as_str());
        let files = doc
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| match live {
                Some(text) if i == doc.active => {
                    let merged = config::reattach_config_block(&f.text, text);
                    SnapFile {
                        name: f.name.clone(),
                        unsaved: f.dirty || merged != f.text,
                        text: merged,
                    }
                }
                _ => SnapFile {
                    name: f.name.clone(),
                    text: f.text.clone(),
                    unsaved: f.dirty,
                },
            })
            .collect();
        DocSnapshot {
            files,
            assets: doc.assets.clone(),
            unsaved_assets: doc
                .assets
                .keys()
                .filter(|n| doc.asset_unsaved(n))
                .cloned()
                .collect(),
            entry: doc.entry,
            title: doc.title.clone(),
            rev: doc.rev(),
        }
    }

    pub(crate) fn files(&self) -> &[SnapFile] {
        &self.files
    }

    pub(crate) fn assets(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.assets
    }

    pub(crate) fn entry(&self) -> usize {
        self.entry
    }

    /// The entry's project-relative name (`model.scad` for an empty document, as `ProjectDoc` says).
    pub(crate) fn entry_name(&self) -> &str {
        self.files
            .get(self.entry)
            .map(|f| f.name.as_str())
            .unwrap_or("model.scad")
    }

    /// The `.scadproj` manifest title the document opened with.
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The document revision these bytes are; success calls `mark_saved(rev)` with it.
    pub(crate) fn rev(&self) -> u64 {
        self.rev
    }

    /// More than one file, or any asset: it leaves as a `.scadproj`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))] // the web's source variant asks
    pub(crate) fn is_multifile(&self) -> bool {
        self.files.len() + self.assets.len() > 1
    }

    /// The entry's text with the live cut plan + printer baked in — what every save writes for it.
    /// `None` for a document with no files.
    pub(crate) fn entry_baked(
        &self,
        parts: &[Part],
        printer: config::PrinterCfg,
    ) -> Option<String> {
        self.files
            .get(self.entry)
            .map(|f| config::with_config_block(&f.text, parts, Some(printer)))
    }
}

/// Write `bytes` to `path` atomically: a hidden sibling temp file, synced, then renamed over the
/// target, so an interrupted write leaves the previous file intact (a sibling keeps the rename on one
/// filesystem). A symlink is followed to the file it names. The original's permissions carry over, a
/// read-only original is refused rather than silently replaced, and the temp is removed on any failure.
/// A writable file in a read-only folder fails too: the sibling temp can't be created there.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the browser saves by download, never to a path
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Through a symlink, write its target as `fs::write` did: a rename over the link would swap the
    // link for a regular file and leave the real file stale behind a "saved".
    let resolved;
    let path = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            resolved = std::fs::canonicalize(path)?;
            resolved.as_path()
        }
        _ => path,
    };
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name to save to")
    })?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let perms = match std::fs::metadata(path) {
        Ok(m) if m.permissions().readonly() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the file is read-only",
            ));
        }
        Ok(m) => Some(m.permissions()),
        Err(_) => None, // a new file: the temp's defaults are the right ones
    };
    let tmp = dir.join(format!(
        ".{}.fab-save-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let res = (|| {
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?; // durable BEFORE the rename publishes it
        }
        if let Some(p) = perms {
            std::fs::set_permissions(&tmp, p)?;
        }
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// What a loose save landed: indices into `files` and asset names written, and what failed with why.
#[derive(Debug, Default)]
pub(crate) struct LooseWrite {
    pub(crate) files: Vec<usize>,
    pub(crate) assets: Vec<String>,
    pub(crate) failed: Vec<(String, String)>,
}

/// Write a loose document back into `dir` (SW.3: the folder IS the home): the entry always, as
/// `entry_text` (its config baked in), every other file with unsaved text, and every unsaved asset.
/// Each write is independent and atomic, so one failure never costs the others.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn write_loose(snap: &DocSnapshot, dir: &Path, entry_text: &str) -> LooseWrite {
    let mut out = LooseWrite::default();
    let write = |rel: &str, bytes: &[u8]| -> std::io::Result<()> {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write(&dest, bytes)
    };
    for (i, f) in snap.files.iter().enumerate() {
        let text = if i == snap.entry {
            entry_text
        } else if f.unsaved {
            &f.text
        } else {
            continue;
        };
        match write(&f.name, text.as_bytes()) {
            Ok(()) => out.files.push(i),
            Err(e) => out.failed.push((f.name.clone(), e.to_string())),
        }
    }
    for name in &snap.unsaved_assets {
        let Some(body) = snap.assets.get(name) else {
            continue;
        };
        match write(name, body) {
            Ok(()) => out.assets.push(name.clone()),
            Err(e) => out.failed.push((name.clone(), e.to_string())),
        }
    }
    out
}

/// Land a save on the document: the entry now holds what Save wrote for it (so a container's
/// re-materialized temp and the next flush carry the new `fab:config`), then the written files clear.
/// `live_entry` is the editor's text when it shows the entry: the entry then lands as the NEXT flush of
/// that buffer will store it, so viewing another file right after a save can't re-dirty it.
/// `None` = everything in `snap` landed; `Some(written)` = only those files and assets did.
/// Returns whether the document's flags cleared; `false` when `rev` moved since the capture.
fn land(
    doc: &mut ProjectDoc,
    snap: &DocSnapshot,
    entry_baked: Option<&str>,
    live_entry: Option<&str>,
    written: Option<&LooseWrite>,
) -> bool {
    if doc.rev() != snap.rev() {
        return false; // something landed after the capture; it wasn't in these bytes
    }
    let entry_landed = written.is_none_or(|w| w.files.contains(&snap.entry));
    if entry_landed && let (Some(text), Some(f)) = (entry_baked, doc.files.get_mut(snap.entry)) {
        f.text = match live_entry {
            Some(live) => config::reattach_config_block(text, live),
            None => text.to_string(),
        };
    }
    match written {
        None => doc.mark_saved(snap.rev()),
        Some(w) => doc.mark_written(snap.rev(), &w.files, &w.assets),
    }
}

/// [`land`] plus the state outside the document: the config baseline moves to what the entry's write
/// baked (only if the entry landed), and the buffer reads clean once its file is written — not before:
/// a loose save whose ACTIVE file failed keeps the buffer unsaved too.
fn landed(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    doc: &mut DocState,
    snap: &DocSnapshot,
    entry_baked: Option<&str>,
    fp: &config::ConfigFp,
    written: Option<&LooseWrite>,
) -> bool {
    let live = (project.editor_holds(editor) && project.active == snap.entry())
        .then(|| editor.text.clone());
    let cleared = land(project, snap, entry_baked, live.as_deref(), written);
    if written.is_none_or(|w| w.files.contains(&snap.entry)) {
        doc.rebaseline(fp.clone());
    }
    // The save flushed the buffer first, so its file's own flag now says whether the buffer landed.
    if cleared && project.files.get(project.active).is_none_or(|f| !f.dirty) {
        editor.dirty = false;
    }
    cleared
}

/// A save that completes on a LATER frame — the bytes it writes, captured at a `rev`, held until the
/// write answers so success can land them: a hotchkiss.io site upload (TG.4) or a Save As whose dialog
/// is up (TG.5). The user can keep typing meanwhile, and those edits were never in the bytes.
pub(crate) struct DeferredSave {
    /// The document the bytes came from. TG.5: rfd's Windows/xdg dialogs aren't modal, so Open... can
    /// swap the document while a Save As dialog is up; landing then must not touch the new one.
    doc: crate::project::DocId,
    snap: DocSnapshot,
    entry_baked: Option<String>,
    fp: config::ConfigFp,
}

impl DeferredSave {
    /// Flush the live buffer, then capture: the document itself records the edit, so the landing's
    /// `rev` check sees anything typed after this point. The snapshot splices it regardless.
    pub(crate) fn begin(
        project: &mut ProjectDoc,
        editor: &EditorBuf,
        parts: &[Part],
        bed: [f32; 3],
    ) -> Self {
        if project.editor_holds(editor) {
            project.flush_active(&editor.text);
        }
        let snap = DocSnapshot::capture(project, editor);
        let entry_baked = snap.entry_baked(parts, Self::printer(bed));
        DeferredSave {
            doc: project.id(),
            snap,
            entry_baked,
            fp: config::config_fp(parts, bed),
        }
    }

    /// Were these bytes captured from `project` — the same document, not just the same paths?
    pub(crate) fn is_for(&self, project: &ProjectDoc) -> bool {
        self.doc == project.id()
    }

    /// The printer block every save bakes for `bed`.
    pub(crate) fn printer(bed: [f32; 3]) -> config::PrinterCfg {
        config::PrinterCfg {
            bed: bed.map(f64::from),
        }
    }

    /// What the write carries.
    pub(crate) fn snapshot(&self) -> &DocSnapshot {
        &self.snap
    }

    /// The entry as the write bakes it (cut plan + printer); a one-file Save As writes exactly this.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the web uploads through the snapshot
    pub(crate) fn entry_baked(&self) -> Option<&str> {
        self.entry_baked.as_deref()
    }

    /// The write succeeded: land it, as a synchronous save would. An edit still only in the buffer is
    /// flushed FIRST, so one typed mid-write moves `rev` and the landing clears nothing — `editor.dirty`
    /// alone can't tell a pre-capture edit (in the bytes) from a later one (not). A config change made
    /// mid-write stays unsaved against the baseline this moves to. Returns whether the document cleared.
    /// A different document open by now ([`Self::is_for`]) is left alone: these bytes were never its.
    pub(crate) fn land(
        &self,
        project: &mut ProjectDoc,
        editor: &mut EditorBuf,
        doc: &mut DocState,
    ) -> bool {
        if !self.is_for(project) {
            return false;
        }
        if project.editor_holds(editor) {
            project.flush_active(&editor.text);
        }
        landed(
            project,
            editor,
            doc,
            &self.snap,
            self.entry_baked.as_deref(),
            &self.fp,
            None,
        )
    }
}

/// TG.5: the file Save As writes — a `.scad` for one text file with no assets, a `.scadproj` otherwise
/// (the spec's rule). A loose folder's assets count although SW.3 keeps them out of `doc.assets`
/// ([`has_disk_assets`]), so a plain-file Save As into another folder can't strand an `import()`ed
/// sibling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveAsKind {
    Scad,
    ScadProj,
}

impl SaveAsKind {
    /// `disk_assets`: [`has_disk_assets`] for `doc` — a parameter so the rule stays a pure table.
    pub(crate) fn of(doc: &ProjectDoc, disk_assets: bool) -> Self {
        if doc.is_multifile() || disk_assets {
            SaveAsKind::ScadProj
        } else {
            SaveAsKind::Scad
        }
    }

    /// The extension the dialog offers and the written file carries.
    pub(crate) fn ext(self) -> &'static str {
        match self {
            SaveAsKind::Scad => "scad",
            SaveAsKind::ScadProj => fab_scad::scadproj::PROJECT_EXT,
        }
    }

    /// The dialog's filter label.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
    pub(crate) fn filter_name(self) -> &'static str {
        match self {
            SaveAsKind::Scad => "OpenSCAD model",
            SaveAsKind::ScadProj => "OpenSCAD project",
        }
    }
}

/// What the Save As dialog starts with (TG.5).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveAsDefaults {
    pub(crate) kind: SaveAsKind,
    /// The suggested file name: the document's own name ([`ProjectDoc::doc_stem`]), else the entry's
    /// stem (a fresh session's `untitled`), with [`SaveAsKind::ext`].
    pub(crate) name: String,
    /// The document's own folder ([`ProjectDoc::home_dir`]), else the user's home. Never
    /// `scene.source`: for a `.scadproj` that is the temp it unpacked to, and a Save As landing there
    /// is lost on the next same-stem open.
    pub(crate) dir: Option<PathBuf>,
}

/// Does `doc`'s loose folder hold an asset its model could `import()` (one not already in
/// `doc.assets`)? Always `false` on the web, which has no folder.
pub(crate) fn has_disk_assets(doc: &ProjectDoc) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::jobs::has_loose_sibling_assets(doc)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = doc;
        false
    }
}

/// The Save As dialog's defaults for `doc`; `disk_assets` as [`SaveAsKind::of`] takes it, `user_home`
/// the fallback folder.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
pub(crate) fn save_as_defaults(
    doc: &ProjectDoc,
    disk_assets: bool,
    user_home: Option<&Path>,
) -> SaveAsDefaults {
    let kind = SaveAsKind::of(doc, disk_assets);
    let stem = doc
        .doc_stem()
        .or_else(|| entry_stem(doc))
        .unwrap_or_else(|| "untitled".into());
    SaveAsDefaults {
        kind,
        name: format!("{stem}.{}", kind.ext()),
        dir: doc.home_dir().or_else(|| user_home.map(Path::to_path_buf)),
    }
}

/// The entry file's stem, when it has one.
fn entry_stem(doc: &ProjectDoc) -> Option<String> {
    Path::new(doc.entry_name())
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The path Save As writes for the dialog's pick: `chosen` when it already ends in `kind`'s extension
/// (any case), else `chosen` with it APPENDED (`brace.v2` -> `brace.v2.scadproj`; replacing would eat
/// the `.v2`). A platform dialog whose filter didn't enforce the extension can't get zip bytes written
/// under a `.scad` name, or a `.scad` the next Open won't list.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
pub(crate) fn save_as_target(chosen: &Path, kind: SaveAsKind) -> PathBuf {
    let has = chosen
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(kind.ext()));
    if has {
        return chosen.to_path_buf();
    }
    let name = chosen
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into());
    chosen.with_file_name(format!("{name}.{}", kind.ext()))
}

/// TG.5: after a Save As to `target` landed, the document IS that file — later Saves write there and
/// the original is never touched again:
/// - `.scad`: home `ScadFile(target)`, `base_dir` its folder, and the lone file takes the new name, so
///   it is now a loose document in that folder;
/// - `.scadproj`: a loose folder's on-disk sibling assets become the document's own (the archive
///   already carries them), home `ScadProj(target)`, and the render re-roots to a fresh temp
///   materialization, exactly as an opened `.scadproj` does, so preview never writes the user's folder.
///
/// The editor keeps the buffer (same [`DocId`](crate::project::DocId), path re-pointed), and `source`
/// follows the entry, so the next view switch doesn't read as a render-target change and wipe the cut
/// plan. Nothing here moves `rev`: the re-home is what was just saved, not a new change. A failed temp
/// materialization is returned for the status; the document is re-homed regardless (the archive is
/// written), its render root stays where it was.
///
/// Every file whose editor path moved lands in `moved` as `(old, new)`, for `panel_ui` to re-key its
/// customizer defaults the way a rename does (`RenameUi::renamed`): the DocId survives, so the map
/// isn't dropped, and a missed key re-captures "as-loaded" values from an already-customized buffer.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn rehome(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    source: &mut Option<PathBuf>,
    target: &Path,
    kind: SaveAsKind,
    tmp: &Path,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    let held = project.editor_holds(editor);
    let before: Vec<PathBuf> = (0..project.files.len())
        .map(|i| project.editor_path(i))
        .collect();
    let mut res = Ok(());
    match kind {
        SaveAsKind::Scad => {
            let dir = target.parent().unwrap_or(Path::new(".")).to_path_buf();
            let name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled.scad".into());
            let entry = project.entry;
            if let Some(f) = project.files.get_mut(entry) {
                f.name = name;
            }
            project.home = ProjectHome::ScadFile(target.to_path_buf());
            project.base_dir = Some(dir);
        }
        SaveAsKind::ScadProj => {
            // Before `home` changes: a loose home is what names the folder to sweep.
            let absorbed = crate::jobs::loose_sibling_assets(project);
            project.assets.extend(absorbed);
            project.home = ProjectHome::ScadProj(target.to_path_buf());
            let stem = target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into());
            let dir = tmp.join("scadproj").join(stem);
            let _ = std::fs::remove_dir_all(&dir);
            match crate::jobs::materialize_all(project, &dir) {
                Ok(()) => project.base_dir = Some(dir),
                Err(e) => res = Err(format!("preparing {}: {e:#}", dir.display())),
            }
        }
    }
    if held {
        editor.path = project.editor_path(project.active);
    }
    if project.base_dir.is_some() {
        *source = Some(project.editor_path(project.entry));
    }
    for (i, old) in before.into_iter().enumerate() {
        let new = project.editor_path(i);
        if new != old {
            moved.push((old, new));
        }
    }
    res
}

/// The Save As status: `saved <new> -> <folder>`, then that the original was left as it was — a
/// `.scadproj` by name, a loose folder by place — unless there was none (a fresh session) or the pick
/// WAS the original. `cleared` false (an edit landed while the dialog was up) says those stay unsaved.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
pub(crate) fn save_as_line(
    target: &Path,
    original: &ProjectHome,
    cleared: bool,
    home: Option<&Path>,
) -> String {
    use crate::file_ops::tilde;
    let file_name = |p: &Path| {
        p.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    let dir = target.parent().unwrap_or(target);
    let mut line = saved_line(&file_name(target), &tilde(dir, home));
    let left = match original {
        ProjectHome::ScadProj(p) if p != target => Some(file_name(p)),
        ProjectHome::ScadFile(p) if p != target => {
            Some(tilde(p.parent().unwrap_or(p.as_path()), home))
        }
        _ => None,
    };
    if let Some(left) = left {
        line.push_str(&format!(" — {left} left unchanged"));
    }
    if !cleared {
        line.push_str(" — edits made since are still unsaved");
    }
    line
}

/// Why Save As must not run, if it mustn't (TG.5): with no file in the document it would write an empty
/// `.scad` and report it saved, and an edited buffer the document doesn't hold is text the bytes would
/// leave out while the landing marks the document clean. Either way: refuse, keep everything unsaved.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
pub(crate) fn save_as_refusal(project: &ProjectDoc, editor: &EditorBuf) -> Option<&'static str> {
    if project.files.is_empty() {
        Some("no document is open")
    } else if editor.dirty && !project.editor_holds(editor) {
        Some("the editor's text belongs to no open document")
    } else {
        None
    }
}

/// The Save As status when `current` replaced the saved document while the dialog was up:
/// `saved <new> -> <folder> — <current> was opened meanwhile and stays where it was`.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // Save As is desktop-only
pub(crate) fn save_as_elsewhere_line(
    target: &Path,
    current: &ProjectDoc,
    home: Option<&Path>,
) -> String {
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| target.display().to_string());
    let dir = target.parent().unwrap_or(target);
    format!(
        "{} — {} was opened meanwhile and stays where it was",
        saved_line(&name, &crate::file_ops::tilde(dir, home)),
        doc_name(current)
    )
}

/// TG.5: a Save As in flight — the dialog + write task, and the [`DeferredSave`] its bytes came from
/// (captured with their `rev` when the dialog opened, so an edit made while it is up stays unsaved).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Default)]
pub(crate) struct SaveAsJob(Option<SaveAsInFlight>);

#[cfg(not(target_arch = "wasm32"))]
struct SaveAsInFlight {
    task: bevy::tasks::Task<Result<PathBuf, String>>,
    save: DeferredSave,
    kind: SaveAsKind,
}

/// The status a cancelled Save As dialog posts — also the task's "no pick" error, so the landing can
/// tell it from a failed write.
#[cfg(not(target_arch = "wasm32"))]
const SAVE_AS_CANCELLED: &str = "save as cancelled";

/// `PanelCmd::SaveAs` (TG.5) — the Project tab's Save As..., Cmd/Ctrl+Shift+S, and Save on a document
/// with no file ([`SavePlan::NeedsSaveAs`]). Every desktop home: it replaced Z.3.7's
/// `save_as_project_action`, which only packed a loose multi-file folder into a `.scadproj` and opened
/// its dialog in `scene.source` (a container's temp). The bytes and their `rev` are captured NOW
/// ([`DeferredSave::begin`]); the dialog and the atomic write run off-thread (rfd can't block Bevy's
/// loop), and [`poll_save_as`] lands and re-homes.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn save_as_action(
    mut cmds: MessageReader<PanelCmd>,
    mut project: ResMut<ProjectDoc>,
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    mut job: ResMut<SaveAsJob>,
    mut status: ResMut<Status>,
) {
    if cmds.read().filter(|c| **c == PanelCmd::SaveAs).count() == 0 {
        return;
    }
    if job.0.is_some() {
        status.0 = "already saving…".into();
        return;
    }
    if let Some(why) = save_as_refusal(&project, &editor) {
        status.0 = failed_line(why);
        warn!("{}", status.0);
        return;
    }
    let save = DeferredSave::begin(&mut project, &editor, &parts.0, scene.bed);
    let defaults = save_as_defaults(
        &project,
        has_disk_assets(&project),
        crate::file_ops::home_dir().as_deref(),
    );
    let kind = defaults.kind;
    let bytes = match save_as_bytes(&project, &save, &parts.0, scene.bed, kind) {
        Ok(b) => b,
        Err(e) => {
            status.0 = failed_line(&format!("{e:#}"));
            return;
        }
    };
    let task = bevy::tasks::AsyncComputeTaskPool::get().spawn(async move {
        let mut dlg = rfd::AsyncFileDialog::new()
            .add_filter(kind.filter_name(), &[kind.ext()])
            .set_file_name(&defaults.name);
        if let Some(d) = defaults.dir {
            dlg = dlg.set_directory(d);
        }
        let Some(handle) = dlg.save_file().await else {
            return Err(SAVE_AS_CANCELLED.to_string());
        };
        let target = save_as_target(handle.path(), kind);
        atomic_write(&target, &bytes).map_err(|e| format!("{}: {e}", target.display()))?;
        Ok(target)
    });
    job.0 = Some(SaveAsInFlight { task, save, kind });
    status.0 = format!("save as .{}: choose where…", kind.ext());
}

/// Land a Save As (TG.5): mark saved exactly what the dialog-time bytes held, then [`rehome`] the
/// document onto the written file and say where it went and that the original is unchanged. A
/// cancel or a failed write leaves the document as it was, still unsaved.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn poll_save_as(
    mut job: ResMut<SaveAsJob>,
    mut project: ResMut<ProjectDoc>,
    mut editor: ResMut<EditorBuf>,
    mut scene: ResMut<SceneCfg>,
    mut doc: ResMut<DocState>,
    mut status: ResMut<Status>,
    mut rename: ResMut<crate::state::RenameUi>,
) {
    let Some(flight) = job.0.as_mut() else {
        return;
    };
    let Some(result) = bevy::tasks::block_on(bevy::tasks::futures_lite::future::poll_once(
        &mut flight.task,
    )) else {
        return;
    };
    let Some(flight) = job.0.take() else {
        return;
    };
    let target = match result {
        Ok(t) => t,
        Err(e) if e == SAVE_AS_CANCELLED => {
            status.0 = e;
            return;
        }
        Err(e) => {
            status.0 = failed_line(&e);
            error!("{}", status.0);
            return;
        }
    };
    status.0 = land_save_as(
        &mut project,
        &mut editor,
        &mut doc,
        &mut scene,
        &flight.save,
        flight.kind,
        &target,
        &mut rename.renamed,
    );
    info!("{}", status.0);
}

/// The bytes a Save As of `kind` writes, from the dialog-time [`DeferredSave`]: the baked entry for a
/// `.scad`, else the archive — a loose folder's on-disk assets swept in, since a `.scadproj` is
/// self-contained. Shared with the harness's `saveas` verb (TG.8), which skips only the dialog.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_as_bytes(
    project: &ProjectDoc,
    save: &DeferredSave,
    parts: &[Part],
    bed: [f32; 3],
    kind: SaveAsKind,
) -> anyhow::Result<Vec<u8>> {
    match kind {
        // No entry = no document: never an empty file reported saved ([`save_as_refusal`]).
        SaveAsKind::Scad => save
            .entry_baked()
            .map(|t| t.as_bytes().to_vec())
            .ok_or_else(|| anyhow::anyhow!("no document is open")),
        SaveAsKind::ScadProj => crate::jobs::rezip_project(
            save.snapshot(),
            parts,
            DeferredSave::printer(bed),
            &crate::jobs::loose_sibling_assets(project),
        ),
    }
}

/// A Save As's bytes are on disk at `target`: land them ([`DeferredSave::land`]), [`rehome`] the
/// document there (file moves into `moved`), and return the status line. A loose `.scad` now lives in
/// a real folder, so the workspace root (BOSL2, scad-lib) is re-found by walking up from it, as a loose
/// view switch does. A different document opened while the dialog was up is left exactly as it is:
/// the file holds the old document's bytes, and re-homing the new one there would aim its next Save
/// at them.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)] // the landing's whole world, as `poll_save_as` hands it over
pub(crate) fn land_save_as(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    doc: &mut DocState,
    scene: &mut SceneCfg,
    save: &DeferredSave,
    kind: SaveAsKind,
    target: &Path,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> String {
    if !save.is_for(project) {
        return save_as_elsewhere_line(target, project, crate::file_ops::home_dir().as_deref());
    }
    let original = project.home.clone();
    let cleared = save.land(project, editor, doc);
    if let Err(e) = rehome(
        project,
        editor,
        &mut scene.source,
        target,
        kind,
        &scene.tmp,
        moved,
    ) {
        warn!("save as: {e}");
    }
    if kind == SaveAsKind::Scad
        && let Some(r) = target
            .parent()
            .and_then(|d| std::fs::canonicalize(d).ok())
            .as_deref()
            .and_then(crate::fab::find_root_from)
    {
        scene.root = Some(r);
    }
    save_as_line(
        target,
        &original,
        cleared,
        crate::file_ops::home_dir().as_deref(),
    )
}

/// What Save does for `plan`, in one line: the Save hover (TG.4) and the Project card's Save rule
/// (TG.7). `home` folds the user's home folder to `~`. ASCII + the known-safe set only (gui/CLAUDE.md).
pub(crate) fn plan_line(plan: &SavePlan, home: Option<&Path>) -> String {
    use crate::file_ops::tilde;
    match plan {
        SavePlan::Rezip(p) => format!("rewrite {}", tilde(p, home)),
        SavePlan::WriteLoose(dir) => format!("write the changes into {}", tilde(dir, home)),
        SavePlan::NeedsSaveAs => "choose where to save it".into(),
        SavePlan::Download(name) => format!("download {name}"),
        SavePlan::Site => "update on hotchkiss.io".into(),
    }
}

/// `names` for a status line: up to three, then a count.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // only a loose save lists files
fn list_names(names: &[String]) -> String {
    match names.len() {
        0 => "nothing".into(),
        1..=3 => names.join(", "),
        n => format!("{} + {} more", names[..3].join(", "), n - 3),
    }
}

/// `saved <what> -> <where>` (ASCII `->`: a raw arrow is tofu, gui/CLAUDE.md).
pub(crate) fn saved_line(what: &str, place: &str) -> String {
    format!("saved {what} -> {place}")
}

/// `save failed: <why> — still unsaved`.
pub(crate) fn failed_line(why: &str) -> String {
    format!("save failed: {why} — still unsaved")
}

/// `nothing to save — <name> is up to date`.
pub(crate) fn nothing_line(doc: &ProjectDoc) -> String {
    format!("nothing to save — {} is up to date", doc_name(doc))
}

/// `PanelCmd::Save` — the Save button and Cmd/Ctrl+S both write it, so `save_buffer` left `panel_ui`
/// (at Bevy's 16-param cap) and the save logic stopped being shaped by it. A clean document answers
/// "nothing to save"; otherwise [`save_route`] routes it: a container re-zips atomically and
/// re-materializes its temp, a loose folder writes per file and clears only what landed, the web
/// downloads (a hotchkiss.io item too), and Save As (TG.5) takes a desktop document with no file.
/// Every outcome lands in [`Status`]; a failure leaves the document unsaved.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn save_doc_action(
    mut cmds: MessageMutator<PanelCmd>,
    mut project: ResMut<ProjectDoc>,
    mut editor: ResMut<EditorBuf>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    platform: Res<Platform>,
    save_target: Res<SaveTarget>,
    mut doc: ResMut<DocState>,
    mut status: ResMut<Status>,
) {
    // Drain, don't `any`: a short-circuit leaves a second Save unread, and next frame it would
    // overwrite this save's status with "nothing to save".
    if cmds.read().filter(|c| **c == PanelCmd::Save).count() == 0 {
        return;
    }
    let fp = config::config_fp(&parts.0, scene.bed);
    // Derived fresh, not `doc.dirty`: `sync_doc_state` may not have run since this frame's edit.
    if !doc.derive(&project, editor.dirty, &fp) {
        status.0 = nothing_line(&project);
        return;
    }
    let plan = save_route(&project, *platform, save_target.0.is_some());
    // TG.5: no file yet (a fresh desktop session) — Save asks where, as Save As does (the macOS rule).
    if plan == SavePlan::NeedsSaveAs {
        cmds.write(PanelCmd::SaveAs);
        return;
    }
    // Synchronous from here, so flush first: the document then records the live edit itself, and the
    // landing's `mark_saved(rev)` covers it. The snapshot splices it regardless.
    if project.editor_holds(&editor) {
        project.flush_active(&editor.text);
    }
    let snap = DocSnapshot::capture(&project, &editor);
    let printer = config::PrinterCfg {
        bed: scene.bed.map(f64::from),
    };
    let entry_baked = snap.entry_baked(&parts.0, printer);
    let mut saved = |written: Option<&LooseWrite>| {
        landed(
            &mut project,
            &mut editor,
            &mut doc,
            &snap,
            entry_baked.as_deref(),
            &fp,
            written,
        )
    };
    match plan {
        #[cfg(not(target_arch = "wasm32"))]
        SavePlan::Rezip(path) => {
            let res = crate::jobs::rezip_project(&snap, &parts.0, printer, &BTreeMap::new())
                .and_then(|bytes| atomic_write(&path, &bytes).map_err(anyhow::Error::from));
            match res {
                Ok(()) => {
                    saved(None);
                    // The temp is what `import()` reads; make it what was just saved (closes the
                    // add-time-only materialize's edit-lag KNOWN LIMIT for containers).
                    // Only a temp under `scene.tmp`: a promote whose re-root failed leaves `base_dir`
                    // on the user's REAL loose folder, which a container save must never write into.
                    if let Some(base) = project
                        .base_dir
                        .clone()
                        .filter(|b| b.starts_with(&scene.tmp))
                        && let Err(e) = crate::jobs::materialize_all(&project, &base)
                    {
                        warn!("save: refreshing {} failed: {e:#}", base.display());
                    }
                    let dir = path.parent().unwrap_or(path.as_path());
                    status.0 = saved_line(
                        &doc_name(&project),
                        &crate::file_ops::tilde(dir, crate::file_ops::home_dir().as_deref()),
                    );
                }
                Err(e) => status.0 = failed_line(&format!("{}: {e:#}", doc_name(&project))),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        SavePlan::WriteLoose(dir) => {
            let entry_text = entry_baked.clone().unwrap_or_default();
            let w = write_loose(&snap, &dir, &entry_text);
            saved(Some(&w));
            let mut names: Vec<String> = w
                .files
                .iter()
                .filter_map(|&i| snap.files().get(i).map(|f| f.name.clone()))
                .collect();
            names.extend(w.assets.iter().cloned());
            let place = crate::file_ops::tilde(&dir, crate::file_ops::home_dir().as_deref());
            status.0 = if w.failed.is_empty() {
                saved_line(&list_names(&names), &place)
            } else {
                let why = w
                    .failed
                    .iter()
                    .map(|(n, e)| format!("{n}: {e}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                let mut line = failed_line(&why);
                if !names.is_empty() {
                    line.push_str(&format!(" (wrote {})", list_names(&names)));
                }
                line
            };
        }
        #[cfg(target_arch = "wasm32")]
        SavePlan::Download(name) => {
            let stem = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(s, _)| s)
                .to_string();
            match crate::jobs::project_source_variant(&snap, &parts.0, printer, &stem) {
                Ok((file, mime, bytes)) => {
                    if crate::web_host::download_bytes(&file, mime, &bytes) {
                        saved(None);
                        status.0 = saved_line(&file, "your downloads");
                    } else {
                        status.0 = failed_line("the browser refused the download");
                    }
                }
                Err(e) => status.0 = failed_line(&format!("{e:#}")),
            }
        }
        // A plan the platform can't reach (a path home on the web, a download on a desktop): say so
        // rather than drop it.
        other => status.0 = failed_line(&format!("no save route for {other:?} here")),
    }
    if status.0.starts_with("save failed") {
        error!("{}", status.0);
    } else {
        info!("{}", status.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectFile;

    fn two_files(home: ProjectHome) -> ProjectDoc {
        let mut d = ProjectDoc::single("main.scad", "include <lib.scad>\nm();", home);
        d.files.push(ProjectFile {
            name: "lib.scad".into(),
            text: "module m(){cube(1);}".into(),
            dirty: false,
        });
        d
    }

    /// A buffer that holds `files[active]` of `d`, the way `doc_into_editor` leaves it.
    fn holding(d: &ProjectDoc, text: &str) -> EditorBuf {
        EditorBuf {
            text: text.into(),
            path: d.editor_path(d.active),
            owner: Some(d.id()),
            dirty: true,
            ..Default::default()
        }
    }

    const BED: [f32; 3] = [256.0, 256.0, 256.0];

    /// A headless app running just [`save_doc_action`] on a desktop, over `d` + `e`, with `tmp` as
    /// `scene.tmp` and one auto part.
    fn save_app(d: ProjectDoc, e: EditorBuf, tmp: &Path, doc: DocState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<PanelCmd>()
            .insert_resource(d)
            .insert_resource(e)
            .insert_resource(Parts(vec![Part::default()]))
            .insert_resource(SceneCfg {
                source: None,
                stl: None,
                bed: BED,
                plate: [256.0, 256.0],
                root: None,
                tmp: tmp.to_path_buf(),
                reslice_on_start: false,
                cut_pct: 50.0,
            })
            .insert_resource(Platform::Desktop)
            .init_resource::<SaveTarget>()
            .insert_resource(doc)
            .insert_resource(Status(String::new()))
            .add_systems(Update, save_doc_action);
        app
    }

    /// A fresh scratch dir per test (tests run in parallel; nextest in separate processes).
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fab_tg3_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The save table: every home × single/multi × platform (× site target on the web). A desktop
    /// home writes where it lives whatever its size; the web downloads, naming the format by size;
    /// a site item updates the site.
    #[test]
    fn plan_routes_every_home_size_and_platform() {
        let proj = PathBuf::from("/m/brace.scadproj");
        let loose = PathBuf::from("/m/brace/main.scad");
        let homes = [
            ProjectHome::Fresh,
            ProjectHome::ScadFile(loose.clone()),
            ProjectHome::ScadProj(proj.clone()),
            ProjectHome::WebModel("Shower Holder.scadproj".into()),
        ];
        for home in homes {
            for multi in [false, true] {
                let d = if multi {
                    two_files(home.clone())
                } else {
                    ProjectDoc::single("main.scad", "cube(1);", home.clone())
                };
                let desktop = plan(&d, Platform::Desktop, false);
                let want = match &home {
                    ProjectHome::ScadProj(p) => SavePlan::Rezip(p.clone()),
                    ProjectHome::ScadFile(_) => SavePlan::WriteLoose(PathBuf::from("/m/brace")),
                    _ => SavePlan::NeedsSaveAs,
                };
                assert_eq!(desktop, want, "desktop {home:?} multi={multi}");
                // A site target means nothing on a desktop (it never has one, but the table says so).
                assert_eq!(plan(&d, Platform::Desktop, true), want);

                let ext = if multi { "scadproj" } else { "scad" };
                let stem = match &home {
                    ProjectHome::Fresh => "main",
                    ProjectHome::ScadFile(_) => "main",
                    ProjectHome::ScadProj(_) => "brace",
                    ProjectHome::WebModel(_) => "Shower Holder",
                };
                assert_eq!(
                    plan(&d, Platform::Web, false),
                    SavePlan::Download(format!("{stem}.{ext}")),
                    "web {home:?} multi={multi}"
                );
                assert_eq!(plan(&d, Platform::Web, true), SavePlan::Site);
                // ...but Save itself downloads there too: the site update is its own button.
                assert_eq!(
                    save_route(&d, Platform::Web, true),
                    plan(&d, Platform::Web, false)
                );
                assert_eq!(save_route(&d, Platform::Desktop, true), want);
            }
        }
        // An asset alone makes a one-file document leave as an archive.
        let mut d = ProjectDoc::single("demo.scad", "", ProjectHome::Fresh);
        d.assets.insert("logo.png".into(), vec![1]);
        assert_eq!(
            plan(&d, Platform::Web, false),
            SavePlan::Download("demo.scadproj".into())
        );
    }

    /// The bug class this type exists for: an edit still in the editor, never flushed, rides every
    /// serialization — and only into the file the editor actually holds.
    #[test]
    fn a_snapshot_carries_an_unflushed_edit() {
        let mut d = two_files(ProjectHome::WebModel("Brace.scadproj".into()));
        d.set_active(1);
        let e = holding(&d, "module m(){cube(9);}");
        let snap = DocSnapshot::capture(&d, &e);
        assert_eq!(snap.files()[1].text, "module m(){cube(9);}");
        assert!(snap.files()[1].unsaved, "the live edit is unsaved");
        assert!(!snap.files()[0].unsaved);
        assert_eq!(
            d.files[1].text, "module m(){cube(1);}",
            "capture never mutates"
        );

        let parts = [Part::default()];
        let printer = config::PrinterCfg {
            bed: [256.0, 256.0, 256.0],
        };
        let bytes = crate::jobs::rezip_project(&snap, &parts, printer, &BTreeMap::new()).unwrap();
        let back = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        let lib = back.files.iter().find(|f| f.name == "lib.scad").unwrap();
        assert_eq!(lib.text, "module m(){cube(9);}", "the archive has the edit");

        // A buffer from ANOTHER document (same path, different owner) is never spliced in.
        let stranger = EditorBuf {
            owner: None,
            ..holding(&d, "// not yours")
        };
        let snap = DocSnapshot::capture(&d, &stranger);
        assert_eq!(snap.files()[1].text, "module m(){cube(1);}");
    }

    /// An `atomic_write` that can't complete leaves the original exactly as it was, and no temp
    /// behind. A read-only DIRECTORY blocks the temp itself — the failure a plain `fs::write` over the
    /// target would not have survived intact had it failed mid-write.
    #[cfg(unix)]
    #[test]
    fn atomic_write_failure_keeps_the_original() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("atomic_ro");
        let target = dir.join("brace.scadproj");
        std::fs::write(&target, b"the old archive").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores directory modes; then there's no failure to provoke.
        let root = std::fs::write(dir.join(".probe"), b"").is_ok();
        let res = atomic_write(&target, b"the new archive");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !root {
            assert!(res.is_err(), "a read-only folder can't take the save");
            assert_eq!(std::fs::read(&target).unwrap(), b"the old archive");
            let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
            assert_eq!(left.len(), 1, "no temp left behind: {left:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The success path: replaced in place, the original's mode kept, no temp left; a read-only
    /// original is refused, not silently replaced.
    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("atomic_ok");
        let target = dir.join("main.scad");
        std::fs::write(&target, b"cube(1);").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&target, b"cube(2);").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"cube(2);");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp left");

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(atomic_write(&target, b"cube(3);").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"cube(2);");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A loose save where one file can't be written: the files that landed clear, the one that didn't
    /// stays unsaved, and so does the document.
    #[test]
    fn a_loose_partial_failure_leaves_the_failed_file_dirty() {
        let dir = scratch("loose_partial");
        std::fs::write(dir.join("main.scad"), "include <lib.scad>\nm();").unwrap();
        std::fs::write(dir.join("lib.scad"), "module m(){cube(1);}").unwrap();
        std::fs::write(dir.join("util.scad"), "// u").unwrap();
        let mut d = ProjectDoc::from_disk(
            dir.clone(),
            &[
                dir.join("lib.scad"),
                dir.join("main.scad"),
                dir.join("util.scad"),
            ],
            &dir.join("main.scad"),
        );
        let (lib, util) = (0, 2);
        d.set_active(lib);
        d.flush_active("module m(){cube(2);}");
        d.set_active(util);
        d.flush_active("// u2");
        // util.scad can't take the write (read-only on every platform).
        let mut ro = std::fs::metadata(dir.join("util.scad"))
            .unwrap()
            .permissions();
        ro.set_readonly(true);
        std::fs::set_permissions(dir.join("util.scad"), ro.clone()).unwrap();

        let e = EditorBuf::default(); // holds nothing: everything is already in the document
        let snap = DocSnapshot::capture(&d, &e);
        let baked = snap.files()[snap.entry()].text.clone();
        let w = write_loose(&snap, &dir, &baked);
        assert!(land(&mut d, &snap, Some(&baked), None, Some(&w)));

        assert_eq!(
            std::fs::read_to_string(dir.join("lib.scad")).unwrap(),
            "module m(){cube(2);}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("util.scad")).unwrap(),
            "// u"
        );
        assert!(!d.files[lib].dirty, "the file that landed is saved");
        assert!(d.files[util].dirty, "the file that failed stays unsaved");
        assert!(d.is_dirty());
        assert_eq!(w.failed.len(), 1);
        assert_eq!(w.failed[0].0, "util.scad");

        #[allow(clippy::permissions_set_readonly_false)] // undoing our own set, for the cleanup
        ro.set_readonly(false);
        std::fs::set_permissions(dir.join("util.scad"), ro).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The manifest title survives a save — re-zip used to pass `None` and drop it.
    #[test]
    fn rezip_keeps_the_manifest_title() {
        let mut files = BTreeMap::new();
        files.insert("main.scad".to_string(), b"cube(1);".to_vec());
        files.insert("lib.scad".to_string(), b"module m(){}".to_vec());
        let bytes = fab_scad::scadproj::write_scadproj(
            &fab_scad::scadproj::project_from_files(
                files,
                Some("main.scad".into()),
                Some("Trash Can Brace".into()),
            )
            .unwrap(),
        )
        .unwrap();
        let d = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        let snap = DocSnapshot::capture(&d, &EditorBuf::default());
        let printer = config::PrinterCfg {
            bed: [256.0, 256.0, 256.0],
        };
        let again = crate::jobs::rezip_project(&snap, &[], printer, &BTreeMap::new()).unwrap();
        let back = fab_scad::scadproj::read_scadproj(&again).unwrap();
        assert_eq!(back.manifest.title.as_deref(), Some("Trash Can Brace"));
        assert_eq!(back.manifest.entry, "main.scad");
    }

    /// A landing on a document that moved after the capture clears nothing.
    #[test]
    fn a_landing_after_a_later_edit_clears_nothing() {
        let mut d = two_files(ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")));
        d.flush_active("cube(2);");
        let snap = DocSnapshot::capture(&d, &EditorBuf::default());
        d.add_file("late.scad", String::new());
        assert!(!land(&mut d, &snap, None, None, None));
        assert!(d.is_dirty());
    }

    /// Every line Save posts reads as the spec has it, in glyphs egui's fonts carry.
    #[test]
    fn status_lines_name_the_document() {
        let d = ProjectDoc::single(
            "main.scad",
            "",
            ProjectHome::ScadProj(PathBuf::from("/m/bracket.scadproj")),
        );
        assert_eq!(
            nothing_line(&d),
            "nothing to save — bracket.scadproj is up to date"
        );
        assert_eq!(
            saved_line("bracket.scadproj", "~/models"),
            "saved bracket.scadproj -> ~/models"
        );
        assert_eq!(
            failed_line("disk full"),
            "save failed: disk full — still unsaved"
        );
        let fresh = ProjectDoc::single("x.scad", "", ProjectHome::Fresh);
        assert_eq!(doc_name(&fresh), "untitled");
        let names: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        assert_eq!(list_names(&names), "a, b, c + 2 more");
        for line in [nothing_line(&d), failed_line("x"), saved_line("a", "b")] {
            assert!(
                line.chars().all(|c| c.is_ascii() || c == '—'),
                "tofu risk in {line:?}"
            );
        }
    }

    /// The system end to end, headless: Save on an opened `.scadproj` with an added file and an
    /// unflushed edit rewrites the archive (title kept), re-materializes the temp, reads clean and says
    /// where it wrote; a second Save answers "nothing to save"; a fresh desktop document is handed to
    /// Save As.
    #[test]
    fn save_doc_action_rezips_reports_and_answers_a_clean_save() {
        let dir = scratch("system");
        let tmp = dir.join("temp");
        let home = dir.join("brace.scadproj");
        let mut files = BTreeMap::new();
        files.insert(
            "main.scad".to_string(),
            b"include <lib.scad>\nm();".to_vec(),
        );
        files.insert("lib.scad".to_string(), b"module m(){cube(1);}".to_vec());
        let bytes = fab_scad::scadproj::write_scadproj(
            &fab_scad::scadproj::project_from_files(
                files,
                Some("main.scad".into()),
                Some("Brace".into()),
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::write(&home, bytes).unwrap();
        let mut d = ProjectDoc::from_scadproj(
            &std::fs::read(&home).unwrap(),
            ProjectHome::ScadProj(home.clone()),
        )
        .unwrap();
        d.base_dir = Some(tmp.clone());
        d.import("hook.scad", b"module hook(){}".to_vec());
        let mut e = EditorBuf::default();
        crate::state::doc_into_editor(&mut e, &d, d.entry);
        e.text = "include <lib.scad>\nm(); // edited".into();
        e.dirty = true;

        let mut app = save_app(d, e, &tmp, DocState::default());
        app.world_mut().write_message(PanelCmd::Save);
        app.update();

        let status = app.world().resource::<Status>().0.clone();
        assert!(status.starts_with("saved brace.scadproj -> "), "{status}");
        let saved = fab_scad::scadproj::read_scadproj(&std::fs::read(&home).unwrap()).unwrap();
        assert_eq!(saved.manifest.title.as_deref(), Some("Brace"));
        let main = saved.files.iter().find(|(n, _)| *n == "main.scad").unwrap();
        assert!(
            String::from_utf8_lossy(main.1).contains("// edited"),
            "the live edit"
        );
        assert!(
            saved.files.iter().any(|(n, _)| n == "hook.scad"),
            "the added file"
        );
        assert!(tmp.join("hook.scad").exists(), "the temp mirrors the save");
        let doc = app.world().resource::<ProjectDoc>();
        let editor = app.world().resource::<EditorBuf>();
        assert!(!doc.is_dirty() && !editor.dirty);

        app.world_mut().write_message(PanelCmd::Save);
        app.update();
        assert_eq!(
            app.world().resource::<Status>().0,
            "nothing to save — brace.scadproj is up to date"
        );

        // A fresh desktop document has nowhere to write: Save becomes Save As.
        let fresh = ProjectDoc::single("untitled.scad", "cube(1);", ProjectHome::Fresh);
        let owned = EditorBuf {
            text: "cube(2);".into(),
            path: fresh.editor_path(0),
            owner: Some(fresh.id()),
            dirty: true,
            ..Default::default()
        };
        app.insert_resource(fresh).insert_resource(owned);
        app.world_mut().write_message(PanelCmd::Save);
        app.update();
        let forwarded = app
            .world()
            .resource::<Messages<PanelCmd>>()
            .iter_current_update_messages()
            .any(|c| *c == PanelCmd::SaveAs);
        assert!(forwarded, "Save on a fresh document opens Save As");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.3 (spec: a failed write): a container save that can't write leaves everything unsaved. The
    /// status says why, the document, the buffer and the added asset's marker stay, and the archive on
    /// disk is byte-for-byte the old one. `atomic_write` refuses a read-only archive itself, so this
    /// holds as root too.
    #[test]
    fn save_doc_action_rezip_failure_keeps_everything_unsaved() {
        let dir = scratch("rezip_ro");
        let tmp = dir.join("temp");
        let home = dir.join("brace.scadproj");
        let files = [
            (
                "main.scad".to_string(),
                b"include <lib.scad>\nm();".to_vec(),
            ),
            ("lib.scad".to_string(), b"module m(){cube(1);}".to_vec()),
        ]
        .into_iter()
        .collect();
        let bytes = fab_scad::scadproj::write_scadproj(
            &fab_scad::scadproj::project_from_files(files, Some("main.scad".into()), None).unwrap(),
        )
        .unwrap();
        std::fs::write(&home, &bytes).unwrap();
        let mut d = ProjectDoc::from_scadproj(&bytes, ProjectHome::ScadProj(home.clone())).unwrap();
        d.base_dir = Some(tmp.clone());
        d.import("logo.png", vec![0x89, b'P']);
        let mut e = EditorBuf::default();
        crate::state::doc_into_editor(&mut e, &d, d.entry);
        e.text = "include <lib.scad>\nm(); // edited".into();
        e.dirty = true;
        let mut ro = std::fs::metadata(&home).unwrap().permissions();
        ro.set_readonly(true);
        std::fs::set_permissions(&home, ro.clone()).unwrap();

        let mut app = save_app(d, e, &tmp, DocState::default());
        app.world_mut().write_message(PanelCmd::Save);
        app.update();

        assert_eq!(
            app.world().resource::<Status>().0,
            "save failed: brace.scadproj: the file is read-only — still unsaved"
        );
        assert_eq!(
            std::fs::read(&home).unwrap(),
            bytes,
            "the old archive stands"
        );
        let doc = app.world().resource::<ProjectDoc>();
        assert!(doc.is_dirty());
        assert!(
            doc.asset_unsaved("logo.png"),
            "the asset row keeps its marker"
        );
        assert!(
            doc.files[doc.entry].dirty,
            "the flushed edit is still unsaved"
        );
        assert!(app.world().resource::<EditorBuf>().dirty);
        #[allow(clippy::permissions_set_readonly_false)]
        // test cleanup of a file it made read-only
        ro.set_readonly(false);
        std::fs::set_permissions(&home, ro).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The view a file switch does (`file_ops::switch`): flush the buffer if it holds its file, then
    /// hydrate file `i`.
    fn view(d: &mut ProjectDoc, e: &mut EditorBuf, i: usize) {
        if d.editor_holds(e) {
            d.flush_active(&e.text);
        }
        d.set_active(i);
        crate::state::doc_into_editor(e, d, i);
    }

    /// The review's repro: a buffer ending in blank lines, saved, then viewing another file and back
    /// (and away again) must leave the document saved. The landing used to store the baked text while
    /// the next flush rebuilt it from the untrimmed buffer, and the hydrate trimmed it again.
    #[test]
    fn a_saved_entry_stays_saved_across_file_views() {
        let printer = config::PrinterCfg {
            bed: [256.0, 256.0, 256.0],
        };
        for buf in [
            "include <lib.scad>\nm();\n\n\n",
            "include <lib.scad>\nm();\n  \n",
            "include <lib.scad>\nm();",
        ] {
            let mut d = two_files(ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")));
            let mut e = EditorBuf::default();
            crate::state::doc_into_editor(&mut e, &d, 0);
            e.text = buf.into();
            // save_doc_action's order: flush, capture, bake, land with the live entry.
            d.flush_active(&e.text);
            let snap = DocSnapshot::capture(&d, &e);
            let baked = snap.entry_baked(&[Part::default()], printer);
            assert!(baked.as_deref().is_some_and(|b| b.contains("fab:config")));
            assert!(land(&mut d, &snap, baked.as_deref(), Some(&e.text), None));
            assert!(!d.is_dirty(), "{buf:?}: saved");
            for i in [1, 0, 1, 0] {
                view(&mut d, &mut e, i);
                assert!(
                    !d.is_dirty(),
                    "{buf:?}: viewing file {i} re-dirtied the save"
                );
            }
        }
    }

    /// The loose route end to end, with the ENTRY unwritable: the status names the failure and what
    /// did land, the landed file clears, the entry and the buffer stay unsaved, and the cut-plan
    /// baseline doesn't move to a config no file holds.
    #[test]
    fn save_doc_action_loose_reports_a_failed_entry_and_keeps_it_unsaved() {
        let dir = scratch("loose_system");
        std::fs::write(dir.join("main.scad"), "include <lib.scad>\nm();\n").unwrap();
        std::fs::write(dir.join("lib.scad"), "module m(){cube(1);}\n").unwrap();
        let mut d = ProjectDoc::from_disk(
            dir.clone(),
            &[dir.join("lib.scad"), dir.join("main.scad")],
            &dir.join("main.scad"),
        );
        let lib = d.files.iter().position(|f| f.name == "lib.scad").unwrap();
        let main = d.entry;
        d.set_active(lib);
        d.flush_active("module m(){cube(2);}\n");
        let mut e = EditorBuf::default();
        d.set_active(main);
        crate::state::doc_into_editor(&mut e, &d, main);
        e.text = "include <lib.scad>\nm(); // edited\n".into();
        e.dirty = true;
        let mut ro = std::fs::metadata(dir.join("main.scad"))
            .unwrap()
            .permissions();
        ro.set_readonly(true);
        std::fs::set_permissions(dir.join("main.scad"), ro.clone()).unwrap();
        // A saved plan the live one differs from (another bed), so config reads unsaved.
        let mut docs = DocState::default();
        docs.rebaseline(config::config_fp(&[Part::default()], [200.0, 200.0, 200.0]));

        let mut app = save_app(d, e, &dir, docs);
        app.world_mut().write_message(PanelCmd::Save);
        app.update();

        let status = app.world().resource::<Status>().0.clone();
        assert!(
            status.starts_with("save failed: main.scad: the file is read-only — still unsaved"),
            "{status}"
        );
        assert!(status.ends_with("(wrote lib.scad)"), "{status}");
        assert_eq!(
            std::fs::read_to_string(dir.join("lib.scad")).unwrap(),
            "module m(){cube(2);}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("main.scad")).unwrap(),
            "include <lib.scad>\nm();\n"
        );
        let doc = app.world().resource::<ProjectDoc>();
        assert!(!doc.files[lib].dirty, "lib.scad landed");
        assert!(doc.files[main].dirty, "the entry did not");
        assert!(
            app.world().resource::<EditorBuf>().dirty,
            "nor did the buffer"
        );
        let live = config::config_fp(&[Part::default()], BED);
        assert!(
            app.world().resource::<DocState>().config_dirty(&live),
            "no file holds the new plan: the baseline stays"
        );

        #[allow(clippy::permissions_set_readonly_false)] // undoing our own set, for the cleanup
        ro.set_readonly(false);
        std::fs::set_permissions(dir.join("main.scad"), ro).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A container whose `base_dir` isn't a temp under `scene.tmp` (a promote whose re-root failed
    /// leaves it on the user's loose folder): Save writes the archive and never the folder.
    #[test]
    fn a_container_save_never_rematerializes_outside_the_temp() {
        let dir = scratch("outside_tmp");
        let real = dir.join("loose");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("main.scad"), "cube(1);\n").unwrap();
        let home = dir.join("brace.scadproj");
        let mut d = ProjectDoc::single(
            "main.scad",
            "cube(1);\n",
            ProjectHome::ScadProj(home.clone()),
        );
        d.base_dir = Some(real.clone());
        let mut e = EditorBuf::default();
        crate::state::doc_into_editor(&mut e, &d, 0);
        e.text = "cube(2);\n".into();
        e.dirty = true;

        let mut app = save_app(d, e, &dir.join("temp"), DocState::default());
        app.world_mut().write_message(PanelCmd::Save);
        app.update();
        let status = app.world().resource::<Status>().0.clone();
        assert!(status.starts_with("saved brace.scadproj -> "), "{status}");
        assert!(home.exists());
        assert_eq!(
            std::fs::read_to_string(real.join("main.scad")).unwrap(),
            "cube(1);\n",
            "the real folder is untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saving through a symlink writes the file it names and keeps the link a link.
    #[cfg(unix)]
    #[test]
    fn atomic_write_follows_a_symlink() {
        let dir = scratch("atomic_link");
        std::fs::create_dir_all(dir.join("sync")).unwrap();
        let real = dir.join("sync").join("brace.scadproj");
        std::fs::write(&real, b"old").unwrap();
        let link = dir.join("brace.scadproj");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, b"new").unwrap();
        assert_eq!(std::fs::read(&real).unwrap(), b"new");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.4: the web upload is the live document — a multi-file item edited in the editor and never
    /// flushed uploads an archive holding the edit (the site-save shipped the stored text before), and a
    /// one-file item uploads it as a config-baked `.scad`.
    #[test]
    fn the_web_upload_carries_the_unflushed_edit() {
        let parts = [Part::default()];
        let mut d = two_files(ProjectHome::WebModel("Brace.scadproj".into()));
        d.set_active(1);
        d.mark_saved(d.rev());
        let e = holding(&d, "module m(){cube(9);}");
        let site = DeferredSave::begin(&mut d, &e, &parts, BED);
        let (name, mime, bytes) = crate::jobs::project_source_variant(
            site.snapshot(),
            &parts,
            DeferredSave::printer(BED),
            "Brace",
        )
        .unwrap();
        assert_eq!(name, "Brace.scadproj");
        assert_eq!(mime, fab_scad::scadproj::PROJECT_MIME);
        let back = ProjectDoc::from_scadproj(&bytes, ProjectHome::Fresh).unwrap();
        let lib = back.files.iter().find(|f| f.name == "lib.scad").unwrap();
        assert_eq!(lib.text, "module m(){cube(9);}", "the archive has the edit");

        let mut one = ProjectDoc::single("demo.scad", "cube(1);", ProjectHome::Fresh);
        one.mark_saved(one.rev());
        let e = holding(&one, "cube(7);");
        let site = DeferredSave::begin(&mut one, &e, &parts, BED);
        let (name, _, bytes) = crate::jobs::project_source_variant(
            site.snapshot(),
            &parts,
            DeferredSave::printer(BED),
            "demo",
        )
        .unwrap();
        assert_eq!(name, "demo.scad");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("cube(7);"), "{text}");
        assert!(
            text.contains("fab:config"),
            "the config is baked in: {text}"
        );
    }

    /// TG.4: a successful site save clears exactly what it uploaded. Clean after an undisturbed upload;
    /// an edit typed, or a bed changed, while the upload ran stays unsaved; a failed upload never lands,
    /// so the document stays unsaved.
    #[test]
    fn a_site_save_lands_only_what_it_uploaded() {
        let parts = [Part::default()];
        let fresh = || {
            let mut d = two_files(ProjectHome::WebModel("Brace.scadproj".into()));
            d.mark_saved(d.rev());
            let e = holding(&d, "include <lib.scad>\nm(); // edited");
            let mut doc = DocState::default();
            doc.rebaseline(config::config_fp(&parts, BED));
            (d, e, doc)
        };
        let fp = config::config_fp(&parts, BED);

        // Undisturbed: the upload lands and everything reads saved.
        let (mut d, mut e, mut doc) = fresh();
        assert!(doc.derive(&d, e.dirty, &fp), "the edit is unsaved");
        let site = DeferredSave::begin(&mut d, &e, &parts, BED);
        assert!(site.land(&mut d, &mut e, &mut doc));
        assert!(
            !doc.derive(&d, e.dirty, &fp),
            "a landed site save reads saved"
        );
        assert!(!e.dirty);

        // Typed mid-upload: the bytes didn't have it, so it stays unsaved, and in the buffer.
        let (mut d, mut e, mut doc) = fresh();
        let site = DeferredSave::begin(&mut d, &e, &parts, BED);
        e.text.push_str("\n// later");
        assert!(!site.land(&mut d, &mut e, &mut doc), "rev moved");
        assert!(doc.derive(&d, e.dirty, &fp));
        assert!(d.files[0].text.contains("// later"));

        // The bed moved mid-upload: the baseline is what was uploaded, so the live bed reads unsaved.
        let (mut d, mut e, mut doc) = fresh();
        let site = DeferredSave::begin(&mut d, &e, &parts, BED);
        assert!(site.land(&mut d, &mut e, &mut doc));
        assert!(doc.derive(
            &d,
            e.dirty,
            &config::config_fp(&parts, [300.0, 256.0, 256.0])
        ));

        // A failed upload never lands: the flush recorded the edit, so the document stays unsaved.
        let (mut d, e, doc) = fresh();
        let _site = DeferredSave::begin(&mut d, &e, &parts, BED);
        assert!(doc.derive(&d, e.dirty, &fp));
        assert!(
            d.is_dirty(),
            "the edit is the document's own now, not just the buffer's"
        );
    }

    /// TG.4: the Save hover reads where this document's Save goes, home folded to `~`, in glyphs
    /// egui's fonts carry.
    #[test]
    fn plan_line_says_where_save_goes() {
        let home = Path::new("/Users/c");
        let cases = [
            (
                SavePlan::Rezip(PathBuf::from("/Users/c/models/bracket.scadproj")),
                "rewrite ~/models/bracket.scadproj",
            ),
            (
                SavePlan::WriteLoose(PathBuf::from("/Users/c/models/brace")),
                "write the changes into ~/models/brace",
            ),
            (SavePlan::NeedsSaveAs, "choose where to save it"),
            (
                SavePlan::Download("Brace.scadproj".into()),
                "download Brace.scadproj",
            ),
            (SavePlan::Site, "update on hotchkiss.io"),
        ];
        for (plan, want) in cases {
            let line = plan_line(&plan, Some(home));
            assert_eq!(line, want);
            assert!(line.is_ascii(), "tofu risk in {line:?}");
        }
        // No home (the web): the path stays whole.
        assert_eq!(
            plan_line(&SavePlan::Rezip(PathBuf::from("/m/b.scadproj")), None),
            "rewrite /m/b.scadproj"
        );
    }

    /// A desktop scene rooted at `tmp`, for the Save As landing.
    fn scene_at(tmp: &Path) -> SceneCfg {
        SceneCfg {
            source: None,
            stl: None,
            bed: BED,
            plate: [256.0, 256.0],
            root: None,
            tmp: tmp.to_path_buf(),
            reslice_on_start: false,
            cut_pct: 50.0,
        }
    }

    /// TG.5: what the Save As dialog starts with, per home × single/multi: the document's own name and
    /// folder (never the temp a `.scadproj` unpacked to), the user's home for a document with no file,
    /// and `.scad` only for one text file with no assets.
    #[test]
    fn save_as_defaults_per_home() {
        let user = Path::new("/Users/c");
        let with_temp = |mut d: ProjectDoc| {
            d.base_dir = Some(PathBuf::from("/tmp/fab-gui/scadproj/bracket"));
            d
        };
        let cases: [(ProjectDoc, &str, Option<&str>, SaveAsKind); 7] = [
            (
                crate::file_ops::untitled_doc(),
                "untitled.scad",
                Some("/Users/c"),
                SaveAsKind::Scad,
            ),
            (
                two_files(ProjectHome::Fresh),
                "main.scadproj",
                Some("/Users/c"),
                SaveAsKind::ScadProj,
            ),
            (
                ProjectDoc::single(
                    "main.scad",
                    "",
                    ProjectHome::ScadFile("/m/brace/main.scad".into()),
                ),
                "main.scad",
                Some("/m/brace"),
                SaveAsKind::Scad,
            ),
            (
                two_files(ProjectHome::ScadFile("/m/brace/main.scad".into())),
                "main.scadproj",
                Some("/m/brace"),
                SaveAsKind::ScadProj,
            ),
            (
                with_temp(two_files(ProjectHome::ScadProj(
                    "/m/bracket.scadproj".into(),
                ))),
                "bracket.scadproj",
                Some("/m"),
                SaveAsKind::ScadProj,
            ),
            // A one-file archive is one text file: the spec's rule offers `.scad`.
            (
                with_temp(ProjectDoc::single(
                    "main.scad",
                    "",
                    ProjectHome::ScadProj("/m/bracket.scadproj".into()),
                )),
                "bracket.scad",
                Some("/m"),
                SaveAsKind::Scad,
            ),
            (
                two_files(ProjectHome::WebModel("Shower Holder.scadproj".into())),
                "Shower Holder.scadproj",
                Some("/Users/c"),
                SaveAsKind::ScadProj,
            ),
        ];
        for (d, name, dir, kind) in cases {
            let got = save_as_defaults(&d, false, Some(user));
            assert_eq!(
                got,
                SaveAsDefaults {
                    kind,
                    name: name.into(),
                    dir: dir.map(PathBuf::from),
                },
                "{:?}",
                d.home
            );
            assert!(got.name.is_ascii(), "tofu risk in {:?}", got.name);
        }
        // An asset alone makes a one-file document an archive.
        let mut d = crate::file_ops::untitled_doc();
        d.assets.insert("logo.png".into(), vec![1]);
        assert_eq!(save_as_defaults(&d, false, None).kind, SaveAsKind::ScadProj);
        // No user home (unset): no folder at all, never a guess.
        assert_eq!(
            save_as_defaults(&crate::file_ops::untitled_doc(), false, None).dir,
            None
        );

        // A loose single file whose FOLDER holds an importable asset (SW.3 keeps it out of
        // `doc.assets`) is offered `.scadproj`: a lone `.scad` elsewhere would strand the import.
        let loose = scratch("defaults_disk_asset");
        std::fs::write(loose.join("badge.scad"), "import(\"logo.svg\");\n").unwrap();
        let d = ProjectDoc::from_disk(
            loose.clone(),
            &[loose.join("badge.scad")],
            &loose.join("badge.scad"),
        );
        assert!(!has_disk_assets(&d), "no asset yet");
        assert_eq!(
            save_as_defaults(&d, has_disk_assets(&d), None).kind,
            SaveAsKind::Scad
        );
        std::fs::write(loose.join("logo.svg"), "<svg/>").unwrap();
        assert!(has_disk_assets(&d));
        let got = save_as_defaults(&d, has_disk_assets(&d), None);
        assert_eq!(
            (got.kind, got.name.as_str()),
            (SaveAsKind::ScadProj, "badge.scadproj")
        );
        let _ = std::fs::remove_dir_all(&loose);

        // The pick keeps a matching extension (any case) and APPENDS a missing one.
        let t = |p: &str, k| save_as_target(Path::new(p), k);
        assert_eq!(
            t("/m/b.scadproj", SaveAsKind::ScadProj),
            PathBuf::from("/m/b.scadproj")
        );
        assert_eq!(t("/m/b.SCAD", SaveAsKind::Scad), PathBuf::from("/m/b.SCAD"));
        assert_eq!(t("/m/b", SaveAsKind::Scad), PathBuf::from("/m/b.scad"));
        assert_eq!(
            t("/m/b.v2", SaveAsKind::ScadProj),
            PathBuf::from("/m/b.v2.scadproj")
        );
        assert_eq!(
            t("/m/b.scad", SaveAsKind::ScadProj),
            PathBuf::from("/m/b.scad.scadproj")
        );
    }

    /// Run a Save As without the dialog, as `save_as_action` + `poll_save_as` do: refuse, capture,
    /// bake the bytes, write them to `target`, land and re-home. Returns the status line and the moves
    /// `panel_ui` re-keys its customizer defaults by.
    fn save_as_moves(
        d: &mut ProjectDoc,
        e: &mut EditorBuf,
        doc: &mut DocState,
        scene: &mut SceneCfg,
        target: &Path,
    ) -> (String, Vec<(PathBuf, PathBuf)>) {
        assert_eq!(save_as_refusal(d, e), None);
        let parts = [Part::default()];
        let save = DeferredSave::begin(d, e, &parts, scene.bed);
        let kind = SaveAsKind::of(d, has_disk_assets(d));
        let bytes = save_as_bytes(d, &save, &parts, scene.bed, kind).unwrap();
        atomic_write(&save_as_target(target, kind), &bytes).unwrap();
        let mut moved = Vec::new();
        let line = land_save_as(
            d,
            e,
            doc,
            scene,
            &save,
            kind,
            &save_as_target(target, kind),
            &mut moved,
        );
        (line, moved)
    }

    fn save_as(
        d: &mut ProjectDoc,
        e: &mut EditorBuf,
        doc: &mut DocState,
        scene: &mut SceneCfg,
        target: &Path,
    ) -> String {
        save_as_moves(d, e, doc, scene, target).0
    }

    /// TG.5: after a Save As the document IS the new file, per target kind — a `.scad` becomes a loose
    /// document in its folder, a `.scadproj` a container re-rooted to a fresh temp — reads saved, keeps
    /// its buffer, leaves the original untouched, and the next Save writes the new file.
    #[test]
    fn save_as_rehomes_per_target_kind() {
        let dir = scratch("save_as");
        let tmp = dir.join("temp");
        let home = crate::file_ops::home_dir();

        // A fresh session -> .scad: a loose document in the chosen folder, the lone file renamed.
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut d = crate::file_ops::untitled_doc();
        let mut e = EditorBuf::default();
        let (mut doc, mut sc) = (DocState::default(), scene_at(&tmp));
        crate::file_ops::adopt(
            &mut d,
            &mut e,
            &mut crate::state::PendingConfig::default(),
            &mut sc.source,
            &mut Status(String::new()),
            &mut doc,
            crate::file_ops::untitled_doc(),
        );
        e.text = "cube(4);".into();
        e.dirty = true;
        let (line, moved) = save_as_moves(&mut d, &mut e, &mut doc, &mut sc, &out.join("hook"));
        let file = out.join("hook.scad");
        assert_eq!(
            moved,
            [(PathBuf::from("untitled.scad"), file.clone())],
            "the customizer defaults follow the file"
        );
        assert_eq!(
            line,
            saved_line("hook.scad", &crate::file_ops::tilde(&out, home.as_deref()))
        );
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .starts_with("cube(4);")
        );
        assert_eq!(d.home, ProjectHome::ScadFile(file.clone()));
        assert_eq!(d.base_dir.as_deref(), Some(out.as_path()));
        assert_eq!(d.files[0].name, "hook.scad");
        assert!(d.editor_holds(&e), "the buffer is still this document's");
        assert_eq!(e.path, file);
        assert_eq!(sc.source.as_deref(), Some(file.as_path()));
        assert!(!d.is_dirty() && !e.dirty);
        assert_eq!(
            plan(&d, Platform::Desktop, false),
            SavePlan::WriteLoose(out.clone())
        );

        // A loose two-file folder (with an on-disk asset) -> .scadproj: a container now, the folder's
        // asset absorbed, rendering from a fresh temp; the folder itself untouched.
        let loose = dir.join("brace");
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(loose.join("main.scad"), "include <lib.scad>\nm();\n").unwrap();
        std::fs::write(loose.join("lib.scad"), "module m(){cube(1);}\n").unwrap();
        std::fs::write(loose.join("logo.png"), [0x89, b'P']).unwrap();
        let mut d = ProjectDoc::from_disk(
            loose.clone(),
            &[loose.join("lib.scad"), loose.join("main.scad")],
            &loose.join("main.scad"),
        );
        let mut e = EditorBuf::default();
        crate::state::doc_into_editor(&mut e, &d, d.entry);
        e.text = "include <lib.scad>\nm(); // edited\n".into();
        e.dirty = true;
        let packed = dir.join("brace.scadproj");
        let (line, moved) = save_as_moves(&mut d, &mut e, &mut doc, &mut sc, &packed);
        let root = tmp.join("scadproj").join("brace");
        assert_eq!(
            moved,
            [
                (loose.join("lib.scad"), root.join("lib.scad")),
                (loose.join("main.scad"), root.join("main.scad")),
            ],
            "every file moved"
        );
        assert!(line.starts_with("saved brace.scadproj -> "), "{line}");
        assert!(line.ends_with(" left unchanged"), "{line}");
        let back = fab_scad::scadproj::read_scadproj(&std::fs::read(&packed).unwrap()).unwrap();
        let names: Vec<&str> = back.files.keys().map(|n| n.as_str()).collect();
        for want in ["main.scad", "lib.scad", "logo.png"] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
        assert_eq!(
            std::fs::read_to_string(loose.join("main.scad")).unwrap(),
            "include <lib.scad>\nm();\n",
            "the loose original is unchanged"
        );
        assert_eq!(d.home, ProjectHome::ScadProj(packed.clone()));
        assert_eq!(d.base_dir.as_deref(), Some(root.as_path()));
        assert!(root.join("logo.png").exists() && root.join("lib.scad").exists());
        assert!(
            d.assets.contains_key("logo.png"),
            "the asset is the document's own"
        );
        assert!(d.editor_holds(&e));
        assert_eq!(e.path, root.join("main.scad"));
        assert_eq!(sc.source.as_deref(), Some(root.join("main.scad").as_path()));
        assert!(!d.is_dirty());
        assert_eq!(
            plan(&d, Platform::Desktop, false),
            SavePlan::Rezip(packed.clone())
        );

        // That .scadproj -> another .scadproj: the first archive is byte-for-byte what it was.
        let before = std::fs::read(&packed).unwrap();
        let v2 = dir.join("brace-v2.scadproj");
        e.text.push_str("// v2\n");
        e.dirty = true;
        let line = save_as(&mut d, &mut e, &mut doc, &mut sc, &v2);
        assert!(line.ends_with("— brace.scadproj left unchanged"), "{line}");
        assert_eq!(std::fs::read(&packed).unwrap(), before);
        assert_eq!(d.home, ProjectHome::ScadProj(v2.clone()));
        assert_eq!(
            d.base_dir.as_deref(),
            Some(tmp.join("scadproj").join("brace-v2").as_path())
        );
        assert!(!d.is_dirty());

        // An edit landing while the dialog was up stays unsaved — and the line says so.
        let parts = [Part::default()];
        e.text.push_str("// before\n");
        e.dirty = true;
        let save = DeferredSave::begin(&mut d, &e, &parts, BED);
        e.text.push_str("// during\n");
        let v3 = dir.join("brace-v3.scadproj");
        let bytes = save_as_bytes(&d, &save, &parts, BED, SaveAsKind::ScadProj).unwrap();
        atomic_write(&v3, &bytes).unwrap();
        let line = land_save_as(
            &mut d,
            &mut e,
            &mut doc,
            &mut sc,
            &save,
            SaveAsKind::ScadProj,
            &v3,
            &mut Vec::new(),
        );
        assert!(
            line.ends_with("edits made since are still unsaved"),
            "{line}"
        );
        assert!(d.is_dirty());
        assert_eq!(d.home, ProjectHome::ScadProj(v3));

        // A loose ONE-file folder with an importable asset beside it: the kind rule picks `.scadproj`,
        // so the asset travels with the model instead of being stranded in the old folder.
        let badge = dir.join("badge");
        std::fs::create_dir_all(&badge).unwrap();
        std::fs::write(badge.join("badge.scad"), "import(\"logo.svg\");\n").unwrap();
        std::fs::write(badge.join("logo.svg"), "<svg/>").unwrap();
        let mut d = ProjectDoc::from_disk(
            badge.clone(),
            &[badge.join("badge.scad")],
            &badge.join("badge.scad"),
        );
        let mut e = EditorBuf::default();
        crate::state::doc_into_editor(&mut e, &d, d.entry);
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let line = save_as(&mut d, &mut e, &mut doc, &mut sc, &other.join("badge"));
        assert!(line.starts_with("saved badge.scadproj -> "), "{line}");
        let back = fab_scad::scadproj::read_scadproj(
            &std::fs::read(other.join("badge.scadproj")).unwrap(),
        )
        .unwrap();
        assert!(
            back.files.keys().any(|n| n == "logo.svg"),
            "the asset rides along"
        );
        assert!(d.assets.contains_key("logo.svg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.5: the status names the original only when there was one and the pick wasn't it.
    #[test]
    fn save_as_line_says_the_original_is_unchanged() {
        let home = Some(Path::new("/Users/c"));
        let v2 = Path::new("/Users/c/models/bracket-v2.scadproj");
        let orig = ProjectHome::ScadProj("/Users/c/models/bracket.scadproj".into());
        assert_eq!(
            save_as_line(v2, &orig, true, home),
            "saved bracket-v2.scadproj -> ~/models — bracket.scadproj left unchanged"
        );
        assert_eq!(
            save_as_line(
                v2,
                &ProjectHome::ScadFile("/Users/c/brace/main.scad".into()),
                true,
                home
            ),
            "saved bracket-v2.scadproj -> ~/models — ~/brace left unchanged"
        );
        assert_eq!(
            save_as_line(v2, &ProjectHome::Fresh, true, home),
            "saved bracket-v2.scadproj -> ~/models"
        );
        assert_eq!(
            save_as_line(v2, &ProjectHome::ScadProj(v2.to_path_buf()), false, home),
            "saved bracket-v2.scadproj -> ~/models — edits made since are still unsaved"
        );
    }

    /// TG.5: a fresh desktop session owns its buffer, so typed text lands in `files[0]` — through a
    /// switch to a new file and back, and into what Save As would write — and Save routes to Save As.
    #[test]
    fn a_fresh_seed_owns_the_buffer() {
        let mut d = ProjectDoc::default();
        let mut e = EditorBuf::default();
        let mut doc = DocState::default();
        let mut source = None;
        crate::file_ops::adopt(
            &mut d,
            &mut e,
            &mut crate::state::PendingConfig::default(),
            &mut source,
            &mut Status(String::new()),
            &mut doc,
            crate::file_ops::untitled_doc(),
        );
        assert!(d.editor_holds(&e), "the seed owns the buffer");
        assert_eq!(d.files[0].name, "untitled.scad");
        assert!(!d.is_dirty(), "an empty session starts clean");

        e.text = "cube(3);".into();
        e.dirty = true;
        assert!(doc.derive(&d, e.dirty, &config::config_fp(&[], BED)));
        assert_eq!(
            DocSnapshot::capture(&d, &e).files()[0].text,
            "cube(3);",
            "Save As would write the typed text"
        );
        // New file, then view it: the switch flushes the typed text into files[0] (no base_dir — the
        // no-path view swap `apply_switch_file` now takes natively).
        let i = d.add_file("untitled.scad", String::new());
        crate::file_ops::switch(&mut d, &mut e, i);
        assert_eq!(d.files[0].text, "cube(3);");
        assert!(d.files[0].dirty);
        crate::file_ops::switch(&mut d, &mut e, 0);
        assert_eq!(e.text, "cube(3);", "and it comes back on view");
        assert_eq!(plan(&d, Platform::Desktop, false), SavePlan::NeedsSaveAs);
    }

    /// TG.5: a native boot always ends holding an owned document, so Save As can never write an
    /// unowned buffer's absence. No argument: `untitled.scad`. A launch `.scad` that isn't on disk (a
    /// "new model" launch, a typo): that file, homed at its path — typed text then Saves THERE. A
    /// `.scadproj` that fails to unpack: `untitled.scad`, with the error kept for the status. TG.2: a
    /// valid `.scadproj` opens as the project.
    #[test]
    fn a_boot_always_holds_a_document() {
        let dir = scratch("boot");
        let boot = |launch: Option<PathBuf>| {
            let mut sc = scene_at(&dir.join("temp"));
            sc.source = launch;
            let (mut d, mut e, mut doc) = (
                ProjectDoc::default(),
                EditorBuf::default(),
                DocState::default(),
            );
            let line = crate::file_ops::boot_document(
                &mut d,
                &mut e,
                &mut crate::state::PendingConfig::default(),
                &mut sc,
                &mut Status(String::new()),
                &mut doc,
            );
            assert!(
                d.editor_holds(&e),
                "{:?}: the boot owns its buffer",
                sc.source
            );
            assert!(!d.is_dirty());
            (d, e, doc, sc.source, line)
        };

        let (d, _, _, source, line) = boot(None);
        assert_eq!(
            (d.files[0].name.as_str(), &d.home),
            ("untitled.scad", &ProjectHome::Fresh)
        );
        assert_eq!((source, line), (None, None));

        // A launch `.scad` whose folder doesn't exist yet: open_loose finds no `.scad` and fails.
        let new = dir.join("new").join("thing.scad");
        let (d, mut e, doc, source, line) = boot(Some(new.clone()));
        assert_eq!(d.home, ProjectHome::ScadFile(new.clone()));
        assert_eq!(d.files[0].name, "thing.scad");
        assert_eq!(
            source.as_deref(),
            Some(new.as_path()),
            "the render identity"
        );
        let line = line.unwrap();
        assert!(
            line.starts_with("new file thing.scad — Save writes it to "),
            "{line}"
        );
        e.text = "cube(5);".into();
        e.dirty = true;
        let mut app = save_app(d, e, &dir.join("temp"), doc);
        app.world_mut().write_message(PanelCmd::Save);
        app.update();
        assert!(
            std::fs::read_to_string(&new)
                .unwrap()
                .starts_with("cube(5);"),
            "Save writes the typed model to the named path"
        );
        let doc = app.world().resource::<ProjectDoc>();
        assert!(!doc.is_dirty() && !app.world().resource::<EditorBuf>().dirty);

        // A `.scadproj` that isn't a zip: nothing opens, the session still has its own document.
        let bad = dir.join("bad.scadproj");
        std::fs::write(&bad, b"not a zip").unwrap();
        let (d, _, _, source, line) = boot(Some(bad));
        assert_eq!(d.home, ProjectHome::Fresh);
        assert_eq!(source, None);
        assert!(line.unwrap().starts_with("open: "));

        // A launch `.scadproj` (spec: launching with a .scadproj) opens as the project itself: homed at
        // the archive, rendering its entry out of the temp, the entry's plan stashed, saved, and named
        // by the title.
        let good = dir.join("bracket.scadproj");
        let entry = config::with_config_block(
            "include <lib.scad>\nm();\n",
            &[],
            Some(DeferredSave::printer([220.0, 220.0, 250.0])),
        );
        let files = [
            ("main.scad".to_string(), entry.into_bytes()),
            ("lib.scad".to_string(), b"module m(){cube(1);}\n".to_vec()),
        ]
        .into_iter()
        .collect();
        let proj = fab_scad::scadproj::project_from_files(
            files,
            Some("main.scad".into()),
            Some("Bracket".into()),
        )
        .unwrap();
        std::fs::write(&good, fab_scad::scadproj::write_scadproj(&proj).unwrap()).unwrap();
        let mut sc = scene_at(&dir.join("temp"));
        sc.source = Some(good.clone());
        let (mut d, mut e, mut doc, mut pending, mut st) = (
            ProjectDoc::default(),
            EditorBuf::default(),
            DocState::default(),
            crate::state::PendingConfig::default(),
            Status(String::new()),
        );
        let line = crate::file_ops::boot_document(
            &mut d,
            &mut e,
            &mut pending,
            &mut sc,
            &mut st,
            &mut doc,
        );
        assert_eq!(line, None);
        assert_eq!(d.home, ProjectHome::ScadProj(good.clone()));
        assert!(
            d.base_dir
                .as_deref()
                .is_some_and(|b| b.starts_with(dir.join("temp"))),
            "{:?}",
            d.base_dir
        );
        assert_eq!(d.files[d.entry].name, "main.scad");
        assert_eq!(sc.source, Some(d.editor_path(d.entry)), "the entry renders");
        assert!(d.editor_holds(&e) && !d.is_dirty() && !e.dirty);
        assert!(
            e.text.starts_with("include <lib.scad>") && !e.text.contains("fab:config"),
            "{}",
            e.text
        );
        assert_eq!(
            pending.0.and_then(|c| c.printer).map(|p| p.bed),
            Some([220.0, 220.0, 250.0]),
            "the entry's plan waits for the render"
        );
        assert_eq!(
            crate::doc_view::window_title(&d, false),
            "bracket.scadproj — fab-scad"
        );
        assert!(st.0.starts_with("opened bracket.scadproj ("), "{}", st.0);

        // The cfg-free picker: a launch `.scad` that IS on disk keeps its text; no status of its own.
        let there = dir.join("there.scad");
        std::fs::write(&there, "sphere(2);").unwrap();
        let (d, line) = crate::file_ops::boot_fallback(Some(&there), None);
        assert_eq!((d.files[0].text.as_str(), line), ("sphere(2);", None));
        assert_eq!(d.base_dir.as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.5: Save As refuses rather than write nothing and call it saved — no file in the document, or
    /// an edited buffer the document doesn't hold — and a `.scad` with no entry is an error, never "".
    #[test]
    fn save_as_refuses_a_document_less_buffer() {
        let unowned = EditorBuf {
            text: "cube(9);".into(),
            path: PathBuf::from("/m/thing.scad"),
            dirty: true,
            ..Default::default()
        };
        let empty = ProjectDoc::default();
        assert_eq!(
            save_as_refusal(&empty, &unowned),
            Some("no document is open")
        );
        let one = ProjectDoc::single("thing.scad", "", ProjectHome::Fresh);
        assert_eq!(
            save_as_refusal(&one, &unowned),
            Some("the editor's text belongs to no open document")
        );
        assert_eq!(save_as_refusal(&one, &holding(&one, "cube(9);")), None);
        let clean = EditorBuf {
            dirty: false,
            ..unowned
        };
        assert_eq!(save_as_refusal(&one, &clean), None, "nothing typed is lost");

        let mut empty = ProjectDoc::default();
        let save = DeferredSave::begin(&mut empty, &clean, &[], BED);
        assert!(save_as_bytes(&empty, &save, &[], BED, SaveAsKind::Scad).is_err());
    }

    /// TG.5: a Save As landing after ANOTHER document was opened (rfd's Windows/xdg dialogs aren't
    /// modal) writes the old bytes but leaves the new document exactly as it is: home, text, flags.
    #[test]
    fn a_save_as_never_lands_on_a_different_document() {
        let dir = scratch("save_as_other");
        let mut sc = scene_at(&dir.join("temp"));
        let mut doc = DocState::default();
        let a = two_files(ProjectHome::ScadProj(dir.join("a.scadproj")));
        let mut e = holding(&a, "include <lib.scad>\nm(); // a");
        let mut d = a;
        let parts = [Part::default()];
        let save = DeferredSave::begin(&mut d, &e, &parts, BED);
        let bytes = save_as_bytes(&d, &save, &parts, BED, SaveAsKind::ScadProj).unwrap();

        // The dialog is up; Open... lands B.
        let b_home = ProjectHome::ScadProj(dir.join("b.scadproj"));
        crate::file_ops::adopt(
            &mut d,
            &mut e,
            &mut crate::state::PendingConfig::default(),
            &mut sc.source,
            &mut Status(String::new()),
            &mut doc,
            ProjectDoc::single("b.scad", "cube(1);", b_home.clone()),
        );
        e.text = "cube(2);".into();
        e.dirty = true;
        let target = dir.join("a-v2.scadproj");
        atomic_write(&target, &bytes).unwrap();
        let mut moved = Vec::new();
        let line = land_save_as(
            &mut d,
            &mut e,
            &mut doc,
            &mut sc,
            &save,
            SaveAsKind::ScadProj,
            &target,
            &mut moved,
        );
        assert!(
            line.ends_with("— b.scadproj was opened meanwhile and stays where it was"),
            "{line}"
        );
        assert!(line.starts_with("saved a-v2.scadproj -> "), "{line}");
        assert_eq!(d.home, b_home, "B is not re-homed onto A's file");
        assert_eq!(d.files[0].text, "cube(1);");
        assert!(e.dirty, "B's edit is still unsaved");
        assert!(moved.is_empty());
        assert!(
            !save.land(&mut d, &mut e, &mut doc),
            "and the bare landing refuses too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.5: `apply_switch_file`'s native arm on a document with no `base_dir` (a fresh session's
    /// files have no paths) swaps the view instead of bailing — the typed text survives the round trip.
    #[test]
    fn a_fresh_switch_keeps_typed_text_natively() {
        let mut d = crate::file_ops::untitled_doc();
        let mut e = EditorBuf::default();
        crate::file_ops::adopt(
            &mut d,
            &mut e,
            &mut crate::state::PendingConfig::default(),
            &mut None,
            &mut Status(String::new()),
            &mut DocState::default(),
            crate::file_ops::untitled_doc(),
        );
        d.add_file("lib.scad", "module m(){}".to_string());
        e.text = "cube(7);".into();
        e.dirty = true;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<crate::state::SwitchFile>()
            .insert_resource(d)
            .insert_resource(e)
            .insert_resource(scene_at(&std::env::temp_dir()))
            .insert_resource(Status(String::new()))
            .insert_resource(crate::geom::GeomPool::new(1))
            .init_resource::<crate::state::Job>()
            .init_resource::<crate::state::PendingConfig>()
            .insert_resource(Parts(vec![Part::default()]))
            .init_resource::<crate::state::ActivePart>()
            .init_resource::<crate::state::EditCut>()
            .init_resource::<crate::state::XSection>()
            .init_resource::<crate::state::PrintView>()
            .init_resource::<crate::print::PrintJob>()
            .init_resource::<crate::print::PrintPieces>()
            .init_resource::<crate::state::Feas>()
            .init_resource::<DocState>()
            .add_systems(Update, crate::jobs::apply_switch_file);
        let switch = |app: &mut App, i| {
            app.world_mut().write_message(crate::state::SwitchFile(i));
            app.update();
        };
        switch(&mut app, 1);
        assert_eq!(app.world().resource::<EditorBuf>().text, "module m(){}");
        let d = app.world().resource::<ProjectDoc>();
        assert_eq!((d.active, d.files[0].text.as_str()), (1, "cube(7);"));
        switch(&mut app, 0);
        let (d, e) = (
            app.world().resource::<ProjectDoc>(),
            app.world().resource::<EditorBuf>(),
        );
        assert_eq!(e.text, "cube(7);", "the typed text comes back");
        assert!(d.editor_holds(e) && d.is_dirty());
    }
}
