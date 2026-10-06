//! Project-tab document MUTATIONS (Z.3.10) — rename / new / delete / set-entry, as pure functions over
//! [`ProjectDoc`] + [`EditorBuf`].
//!
//! Why a module instead of code inside the handlers: the handlers are per-platform (native drives real
//! files and the rfd picker; the web drives an in-memory `render_pack`), but the RULES are identical,
//! and the rules are where the bugs live. This repo has no wasm test harness, so anything
//! that ends up inside `#[cfg(target_arch = "wasm32")]` ships unexercised by CI. Everything here is
//! cfg-free and unit-tested on the native target; the wasm system is a wiring shim over it.
//!
//! The invariant every function upholds: **the editor holds `files[active]` of THIS document** —
//! `editor.owner` is the doc's [`DocId`](crate::project::DocId) and `editor.path` names `files[active]`
//! (on the web that path is a name, not a location). [`ProjectDoc::editor_holds`] checks both and is the
//! flush-before-switch predicate; if a rename leaves the path stale the next file switch silently
//! discards the user's unsaved edits (the buffer is judged to belong to some other file, so it's never
//! written back).

// The native build only calls `rename` from here — its other handlers still route New/Delete/SetEntry
// through `SwitchFile`, which is right on a platform whose switch also re-aims the render identity and
// the workspace root. The rest is live on wasm and exercised by the tests below on BOTH targets, which
// is the point: this is where the rules get tested, precisely because the wasm handler can't be.
#![allow(dead_code)]

use crate::project::ProjectDoc;
use crate::state::{EditorBuf, PendingConfig};
use std::path::PathBuf;

/// What the viewport owes a mutation. The caller pays it in platform terms — native frees its solid
/// handles and kicks a `Source::Path` render, the web resets model state and arms the debounced
/// `render_pack` preview — but the DECISION of which is owed is the same on both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rerender {
    /// Nothing renderable changed — don't spend a render.
    No,
    /// Same render target, possibly different bytes. Keep the user's cuts and parts.
    Same,
    /// A DIFFERENT model renders now — the old model's parts/cuts/print state is meaningless.
    Target,
}

/// A rename that landed. Both paths are [`ProjectDoc::editor_path`]-shaped — rooted under `base_dir` on
/// native, bare names on the web — because both consumers want them that way: the native handler moves
/// the file on disk, and `panel_ui` re-keys a map that is keyed by `editor.path`. `new` is the name that
/// actually landed, which is not necessarily the one typed — see [`rename`].
pub(crate) struct Renamed {
    pub(crate) old: PathBuf,
    pub(crate) new: PathBuf,
}

/// Rename file `i`. `None` when nothing happened (blank, out of range, or unchanged) so the caller can
/// skip the whole re-render tail.
///
/// The name that lands is read back out of the document rather than assumed: `rename_file`
/// de-duplicates against files AND assets, so typing `foo.scad` next to an existing one yields
/// `foo-1.scad`. Re-pointing `editor.path` at the TYPED name would leave it naming a file that doesn't
/// exist — the stale-token bug this module exists to prevent.
pub(crate) fn rename(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    i: usize,
    want: &str,
) -> Option<Renamed> {
    let old_name = project.rename_file(i, want)?;
    // Root the OLD name the same way `editor_path` roots the new one, so the pair is comparable.
    let old = match project.base_dir.as_ref() {
        Some(base) => base.join(&old_name),
        None => PathBuf::from(&old_name),
    };
    let new = project.editor_path(i);
    // Only the ACTIVE file owns the live buffer — re-point the token at it, and leave it alone
    // otherwise (renaming some other row must not steal the editor).
    if i == project.active {
        editor.path = new.clone();
    }
    Some(Renamed { old, new })
}

/// The refusal a native rename of file `i` to `want` earns, or `None` when it may go ahead. The rename
/// MOVES a file under `base_dir` (the user's real folder since SW.3), and `fs::rename` overwrites its
/// destination silently on Unix. `unique_name` can't catch the clash: it de-duplicates against the
/// DOCUMENT, which for a loose open holds only `.scad`. TG.8: shared by the Project tab's handler and the
/// harness's `rename` verb, so the refusal has one implementation.
pub(crate) fn rename_clash(project: &ProjectDoc, i: usize, want: &str) -> Option<String> {
    let trimmed = want.trim();
    let base = project.base_dir.as_ref()?;
    let dest = base.join(trimmed);
    let clashes = !trimmed.is_empty()
        && dest.exists()
        && project
            .files
            .get(i)
            .is_none_or(|f| base.join(&f.name) != dest);
    clashes.then(|| format!("rename: {trimmed} already exists — pick another name"))
}

/// Add a blank `.scad` and view it. Returns its index. Never re-renders: nothing `include`s a file the
/// moment it's created, so the entry's geometry is unchanged.
pub(crate) fn new_file(project: &mut ProjectDoc, editor: &mut EditorBuf) -> usize {
    let idx = project.add_file("untitled.scad", String::new());
    switch(project, editor, idx);
    idx
}

/// Delete file `i` and re-view whatever is active afterwards. `Err` when it's the project's only file —
/// a project always needs an entry.
///
/// A deleted NON-entry file still earns a re-render: the entry may have been `include`ing it, and the
/// worker tolerates a missing ref silently, so the geometry change would otherwise go unannounced.
pub(crate) fn delete(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    i: usize,
) -> Result<Rerender, &'static str> {
    let was_entry = i == project.entry;
    project
        .remove_file(i)
        .ok_or("can't delete the project's only file")?;
    let active = project.active;
    switch(project, editor, active);
    Ok(if was_entry {
        Rerender::Target
    } else {
        Rerender::Same
    })
}

/// The Delete control's hover (TG.6). A loose folder's files are the user's real files, so a delete
/// there is session-only and has to say so; everywhere else the document owns its files.
pub(crate) fn delete_hover(project: &ProjectDoc) -> &'static str {
    if is_loose(project) {
        "hold to remove from this session — the file stays in the folder"
    } else {
        "hold to remove from the project"
    }
}

/// The Add control's hover (TG.6): a loose Add writes into the user's real folder on the spot, so the
/// hover says so before the click, not just the status line after it.
pub(crate) fn add_hover(project: &ProjectDoc) -> &'static str {
    if is_loose(project) {
        "copy existing files into this folder"
    } else {
        "add existing files to this project"
    }
}

/// TG.6: the on-disk copy a Delete of `name` removes, if any — `Some` ONLY for a container's temp
/// scratch. A loose folder's files are the user's real ones and a delete there is session-only, so
/// this is the single place that decides; the native `DeleteFile` arm just executes it.
pub(crate) fn delete_disk_target(project: &ProjectDoc, name: &str) -> Option<PathBuf> {
    match project.home {
        crate::project::ProjectHome::ScadProj(_) => project.base_dir.as_ref().map(|b| b.join(name)),
        _ => None,
    }
}

/// A loose `.scad` folder: the files are the user's real ones in `base_dir` (SW.3).
fn is_loose(project: &ProjectDoc) -> bool {
    matches!(project.home, crate::project::ProjectHome::ScadFile(_)) && project.base_dir.is_some()
}

/// What [`add_paths`] did, for the status line.
#[derive(Debug, Default)]
pub(crate) struct AddReport {
    /// The project-relative names that landed, in pick order.
    pub(crate) added: Vec<String>,
    /// One line per file that didn't land: unreadable, a refused clash, a failed copy.
    pub(crate) refused: Vec<String>,
    /// The loose folder the adds landed in; `None` for every other home.
    pub(crate) copied_into: Option<PathBuf>,
    /// How many of `added` were actually written into that folder — the rest were already in it.
    pub(crate) copied: usize,
}

impl AddReport {
    /// The status line, or `None` when nothing was picked. `home` `~`-abbreviates the folder.
    pub(crate) fn status(&self, home: Option<&std::path::Path>) -> Option<String> {
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            let n = self.added.len();
            parts.push(match &self.copied_into {
                Some(dir) if self.copied == n => {
                    format!("added {n} file(s) — copied into {}", tilde(dir, home))
                }
                Some(dir) if self.copied == 0 => {
                    format!("added {n} file(s) — already in {}", tilde(dir, home))
                }
                Some(dir) => format!(
                    "added {n} file(s) — {} copied into {}",
                    self.copied,
                    tilde(dir, home)
                ),
                None => format!("added {n} file(s) to the project"),
            });
        }
        parts.extend(self.refused.iter().cloned());
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// TG.6: add picked files to the project — the ONE routine the native Add dialog and the `addfile`
/// harness verb share. Each file goes through [`ProjectDoc::import`] (text → editable, binary → asset),
/// then by home:
/// - **loose folder:** copied into the folder NOW, `.scad` included (SW.3: an explicit file op touches
///   disk, design Decision 7), and left clean — the folder already holds it. A name the folder or the
///   document already has is REFUSED, never overwritten or silently renamed: `unique_name` only sees
///   the document, which for a loose open holds `.scad` alone, and a `-1` copy the user didn't ask for
///   is its own surprise. A file already INSIDE the folder (a session-deleted one, possibly nested)
///   re-imports under its folder-relative name with no write — see [`place_loose`].
/// - **container:** name de-duplicated, unsaved until Save, and written into the temp too, which is
///   what the render's `import()`/`surface()` reads.
/// - **no folder** (a fresh session): the document only, unsaved.
pub(crate) fn add_paths(project: &mut ProjectDoc, paths: &[PathBuf]) -> AddReport {
    let loose = is_loose(project);
    let base = project.base_dir.clone();
    let mut report = AddReport {
        copied_into: base.clone().filter(|_| loose),
        ..AddReport::default()
    };
    for src in paths {
        let bytes = match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                report.refused.push(format!("add {}: {e}", src.display()));
                continue;
            }
        };
        let name = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        match base.as_deref() {
            Some(dir) if loose => match place_loose(project, dir, src, &name, &bytes) {
                Ok((name, wrote)) => {
                    let landed = project.import(&name, bytes);
                    project.mark_on_disk(&landed);
                    report.added.push(landed);
                    report.copied += usize::from(wrote);
                }
                Err(why) => report.refused.push(why),
            },
            Some(dir) => {
                let landed = project.import(&name, bytes);
                let body = project
                    .files
                    .iter()
                    .find(|f| f.name == landed)
                    .map(|f| f.text.as_bytes())
                    .or_else(|| project.assets.get(&landed).map(Vec::as_slice))
                    .unwrap_or_default();
                // Scratch: a failed temp write costs the render an import, never the document.
                if let Err(e) = std::fs::write(dir.join(&landed), body) {
                    report
                        .refused
                        .push(format!("add {landed}: temp copy failed: {e}"));
                }
                report.added.push(landed);
            }
            None => report.added.push(project.import(&name, bytes)),
        }
    }
    report
}

/// The loose half of [`add_paths`]: where `src` lands in `dir`, and whether it had to be written.
/// - `src` already under `dir` (the folder's own file, nested or not): its folder-relative name, the
///   way [`ProjectDoc::from_disk`] names it, and NO write — refused only if the document holds it.
/// - otherwise: `dir/<file name>`, written with `create_new`, so the no-overwrite rule is race-free —
///   a file that appears between the check and the write is refused by the OS, not clobbered.
fn place_loose(
    project: &ProjectDoc,
    dir: &std::path::Path,
    src: &std::path::Path,
    file_name: &str,
    bytes: &[u8],
) -> Result<(String, bool), String> {
    use std::io::Write;
    let held = |name: &str| project.unique_name(name) != name;
    let inside = std::fs::canonicalize(src).ok().and_then(|s| {
        let d = std::fs::canonicalize(dir).ok()?;
        let rel = s
            .strip_prefix(&d)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        (!rel.is_empty()).then_some(rel)
    });
    if let Some(rel) = inside {
        return if held(&rel) {
            Err(format!("add: {rel} is already in the project"))
        } else {
            Ok((rel, false))
        };
    }
    if held(file_name) {
        return Err(format!("add: {file_name} is already in the project"));
    }
    let dest = dir.join(file_name);
    let clash = || format!("add: {file_name} already exists in the folder — not overwritten");
    if dest.exists() {
        return Err(clash());
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => clash(),
            _ => format!("add {file_name}: {e}"),
        })?;
    f.write_all(bytes).map_err(|e| {
        // The file is ours (create_new): a half-written copy must not stay behind in their folder.
        let _ = std::fs::remove_file(&dest);
        format!("add {file_name}: {e}")
    })?;
    Ok((file_name.to_string(), true))
}

/// Make file `i` the render ENTRY (and view it).
///
/// The entry is the one file whose `fab:config` block is stripped-and-stashed — a project's non-entry
/// files are stored verbatim. So promoting a file that carries its own block must strip it HERE, or the
/// block shows up as raw text in the editor and its bed/parts never reach [`PendingConfig`].
pub(crate) fn set_entry(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    pending: &mut PendingConfig,
    i: usize,
) -> Rerender {
    if i >= project.files.len() || i == project.entry {
        return Rerender::No;
    }
    switch(project, editor, i);
    project.set_entry(i);
    if let Some(f) = project.files.get(i) {
        let raw = f.text.clone();
        pending.0 = crate::config::read_config_block(&raw);
        let stripped = crate::config::strip_config_block(&raw);
        if let Some(f) = project.files.get_mut(i) {
            f.text = stripped.clone();
        }
        editor.text = stripped;
    }
    // No `editor.dirty` patch (TG.1): the move is the DOCUMENT's (the manifest's entry), and
    // `ProjectDoc::set_entry` records it there, where the next file switch can't reload it away.
    Rerender::Target
}

/// Flush the live buffer back into its owning file, then hydrate the editor VIEW from `files[i]`. The
/// flush is conditional on [`ProjectDoc::editor_holds`]: if the buffer doesn't belong to `files[active]`,
/// writing it there would overwrite one file with another's text. Hydration goes through
/// [`doc_into_editor`](crate::state::doc_into_editor) — ONE hydrator for every path on both platforms,
/// so the `fab:config` strip can't apply on some switches and not others.
pub(crate) fn switch(project: &mut ProjectDoc, editor: &mut EditorBuf, i: usize) {
    if project.editor_holds(editor) {
        project.flush_active(&editor.text);
    }
    project.set_active(i);
    crate::state::doc_into_editor(editor, project, i);
}

/// TG.2: make a freshly loaded `doc` THE open document — the one way any loader (the Open dialog, the
/// launch argument, the web `?model=` fetch) swaps documents, so none can forget a step:
/// - the editor is hydrated from `doc`'s entry and stamped with its [`DocId`](crate::project::DocId),
///   so the previous buffer can never be flushed into it, even when the paths coincide (a same-stem
///   archive, or re-opening the same file);
/// - the entry's `fab:config` goes to `pending` for `poll_job` to apply once the parts are built;
/// - `source` (`SceneCfg::source`) is cleared, so the native `SwitchFile` that follows reads as a render
///   TARGET change — `state.reset()` drops the old plan, and the new entry re-renders — even when the
///   new entry's path equals the old one;
/// - the status names what opened, and `doc_state` holds that line for the render's landing.
///
/// The caller pays the platform half: native writes `SwitchFile(entry)` (or, at boot, aims `source`
/// and kicks the render itself); the web arms its debounced preview.
pub(crate) fn adopt(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    pending: &mut PendingConfig,
    source: &mut Option<PathBuf>,
    status: &mut crate::state::Status,
    doc_state: &mut crate::state::DocState,
    doc: ProjectDoc,
) {
    *project = doc;
    let entry = project.entry;
    project.set_active(entry);
    pending.0 = crate::state::doc_into_editor(editor, project, entry);
    *source = None;
    if let Some(line) = opened_status(project, home_dir().as_deref()) {
        status.0 = line.clone();
        doc_state.announce_open(line);
    }
}

/// TG.5: what a desktop session started without a file opens — an empty `untitled.scad` with no home,
/// so text typed or pasted lands in a real document (and [`adopt`] makes the buffer its own), and Save
/// routes to Save As ([`SavePlan::NeedsSaveAs`](crate::save::SavePlan::NeedsSaveAs)). It used to boot
/// with no document at all: the buffer belonged to nothing and Save wrote `""`, silently failing.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the web boots its demo or a `?model=` fetch
pub(crate) fn untitled_doc() -> ProjectDoc {
    ProjectDoc::single(
        "untitled.scad",
        String::new(),
        crate::project::ProjectHome::Fresh,
    )
}

/// TG.5: what a native boot opens when the launch argument opened nothing — never no document, which
/// left an unowned buffer that Save As wrote out as an empty file and reported saved:
/// - a launch `.scad` that didn't open (missing: a "new model" launch or a typo; or a folder the scan
///   skips) becomes that one file, homed at its path with its folder as `base_dir`, so Save writes it
///   there. Its text is whatever is on disk, else empty. A missing one also returns the status to post
///   (`opened` would be a lie);
/// - otherwise (no argument, or a `.scadproj` that failed to unpack: a zip is no single file) the
///   owned [`untitled_doc`].
pub(crate) fn boot_fallback(
    launch: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> (ProjectDoc, Option<String>) {
    let Some(src) = launch.filter(|p| !crate::jobs::is_scadproj(p)) else {
        return (untitled_doc(), None);
    };
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled.scad".into());
    let text = std::fs::read_to_string(src);
    let line = text.is_err().then(|| {
        let dir = src.parent().unwrap_or(src);
        format!("new file {name} — Save writes it to {}", tilde(dir, home))
    });
    let mut doc = ProjectDoc::single(
        name,
        text.unwrap_or_default(),
        crate::project::ProjectHome::ScadFile(src.to_path_buf()),
    );
    doc.base_dir = Some(src.parent().unwrap_or(src).to_path_buf());
    (doc, line)
}

/// TG.2 + TG.5: open the launch document (`scene.source`) for a native boot, `setup_windowed` and
/// `setup_script` alike, and adopt it — or, when there is none or it fails, the [`boot_fallback`]: a
/// boot always ends holding an owned document. `scene.source` ends on the entry's path (render
/// identity, not content: the pack is the content), or `None` for a document with no folder. Returns
/// the line the boot leaves in status — an open failure, or a new file — for the caller to post AFTER
/// its render kick, whose own line would bury it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn boot_document(
    project: &mut ProjectDoc,
    editor: &mut EditorBuf,
    pending: &mut PendingConfig,
    scene: &mut crate::state::SceneCfg,
    status: &mut crate::state::Status,
    doc_state: &mut crate::state::DocState,
) -> Option<String> {
    let mut line = None;
    let mut launch = None;
    if let Some(src) = scene.source.clone() {
        match crate::jobs::open_document(&src, &scene.tmp) {
            Ok(doc) => {
                adopt(
                    project,
                    editor,
                    pending,
                    &mut scene.source,
                    status,
                    doc_state,
                    doc,
                );
                scene.source = Some(project.editor_path(project.entry));
                return None;
            }
            Err(e) => {
                line = Some(format!("open: {e:#}"));
                launch = Some(src);
            }
        }
    }
    let (doc, new) = boot_fallback(launch.as_deref(), home_dir().as_deref());
    adopt(
        project,
        editor,
        pending,
        &mut scene.source,
        status,
        doc_state,
        doc,
    );
    if project.base_dir.is_some() {
        scene.source = Some(project.editor_path(project.entry));
    }
    if let Some(l) = &new {
        // `adopt` announced "opened <name>" for a file that isn't on disk; this replaces it.
        doc_state.announce_open(l.clone());
    }
    new.or(line)
}

/// The status line an open posts (TG.2): `opened <name> (<folder>)`, the folder `~`-abbreviated — the
/// REAL home, never the temp a container materializes to. A web item has no folder, so just its name;
/// a `Fresh` document (the demo, a fetch that fell back) isn't an open at all, so `None`. A loose
/// folder's name is its entry file (the folder has no name of its own in the document).
pub(crate) fn opened_status(doc: &ProjectDoc, home: Option<&std::path::Path>) -> Option<String> {
    use crate::project::ProjectHome;
    let file_name = |p: &std::path::Path| {
        p.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    match &doc.home {
        ProjectHome::ScadProj(p) | ProjectHome::ScadFile(p) => {
            let dir = p.parent().unwrap_or(p.as_path());
            Some(format!("opened {} ({})", file_name(p), tilde(dir, home)))
        }
        ProjectHome::WebModel(name) => Some(format!("opened {name}")),
        ProjectHome::Fresh => None,
    }
}

/// `p` with a leading `home` shown as `~` — the way a path reads in a status line.
pub(crate) fn tilde(p: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// The user's home folder, for [`tilde`]. `None` on the web (no environment) or when unset.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectHome;

    /// A web-shaped document (no `base_dir`), the case with no test harness of its own.
    fn web_doc() -> (ProjectDoc, EditorBuf) {
        let mut p = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        p.add_file("hook.scad", "module hook(){}".to_string());
        p.mark_saved(p.rev()); // a SAVED two-file doc, so every test below starts clean
        let e = EditorBuf {
            text: "cube(1);".into(),
            path: p.editor_path(0),
            owner: Some(p.id()),
            ..Default::default()
        };
        (p, e)
    }

    /// THE invariant: after renaming the ACTIVE file, `editor.path` must still name it — using the name
    /// that LANDED, not the one typed. Get this wrong and the next switch throws the buffer away.
    #[test]
    fn renaming_the_active_file_repoints_the_editor_at_the_landed_name() {
        let (mut p, mut e) = web_doc();
        assert!(!p.is_dirty());
        let r = rename(&mut p, &mut e, 0, "base.scad").expect("renamed");
        assert_eq!(r.old, PathBuf::from("main.scad"));
        assert_eq!(r.new, PathBuf::from("base.scad"));
        assert!(p.editor_holds(&e), "editor.path went stale");
        assert!(
            p.is_dirty(),
            "a rename is unsaved where the home persists structure"
        );

        // A COLLIDING rename de-dups; the editor must follow the de-duped name, not the typed one.
        let r = rename(&mut p, &mut e, 0, "hook.scad").expect("renamed");
        assert_eq!(r.new, PathBuf::from("hook-1.scad"));
        assert_eq!(e.path, PathBuf::from("hook-1.scad"));
        assert!(p.editor_holds(&e));
    }

    /// Renaming a NON-active file must leave the editor pointed where it was.
    #[test]
    fn renaming_another_file_leaves_the_editor_alone() {
        let (mut p, mut e) = web_doc();
        let before = e.path.clone();
        rename(&mut p, &mut e, 1, "brace.scad").expect("renamed");
        assert_eq!(e.path, before, "a non-active rename moved the editor");
        assert!(p.editor_holds(&e));
        // No-op renames report nothing, so the caller skips the re-render.
        assert!(rename(&mut p, &mut e, 1, "brace.scad").is_none());
        assert!(rename(&mut p, &mut e, 1, "   ").is_none());
        assert!(rename(&mut p, &mut e, 99, "x.scad").is_none());
    }

    /// The data-loss path in full: edit the active file, rename it, then switch away. The edit must
    /// survive — it only does because the rename re-pointed `editor.path`.
    #[test]
    fn an_unsaved_edit_survives_rename_then_switch() {
        let (mut p, mut e) = web_doc();
        e.text = "cube(2);".into(); // the user types
        rename(&mut p, &mut e, 0, "base.scad").expect("renamed");
        switch(&mut p, &mut e, 1); // click the other row
        assert_eq!(p.files[0].text, "cube(2);", "the edit was discarded");
        assert_eq!(e.text, "module hook(){}");
        assert!(p.editor_holds(&e));
    }

    /// New file: appended, viewed, dirty, and NOT worth a render.
    #[test]
    fn new_file_views_the_blank_and_leaves_the_render_alone() {
        let (mut p, mut e) = web_doc();
        e.text = "cube(3);".into();
        let idx = new_file(&mut p, &mut e);
        assert_eq!(idx, 2);
        assert_eq!(p.active, 2);
        assert_eq!(e.text, "");
        assert!(p.editor_holds(&e));
        assert!(
            e.dirty,
            "the blank file is unsaved, and the editor views it"
        );
        assert!(p.is_dirty());
        // The flush on the way out preserved the edit to the file we left.
        assert_eq!(p.files[0].text, "cube(3);");
    }

    /// Delete: the entry going away is a TARGET change; a non-entry going away is not. The last file
    /// refuses outright.
    #[test]
    fn delete_reports_whether_the_render_target_moved() {
        let (mut p, mut e) = web_doc();
        assert_eq!(delete(&mut p, &mut e, 1), Ok(Rerender::Same));
        assert!(p.editor_holds(&e));
        assert!(
            p.is_dirty(),
            "the archive Save writes no longer has the file"
        );
        assert_eq!(
            delete(&mut p, &mut e, 0),
            Err("can't delete the project's only file")
        );

        let (mut p, mut e) = web_doc();
        assert_eq!(delete(&mut p, &mut e, 0), Ok(Rerender::Target)); // index 0 was the entry
        assert_eq!(p.files.len(), 1);
        assert!(p.editor_holds(&e));
    }

    /// Set-entry strips the promoted file's own `fab:config` block into `PendingConfig` — otherwise it
    /// renders as raw text in the editor and its bed never loads.
    #[test]
    fn set_entry_strips_and_stashes_the_promoted_files_config() {
        let mut p = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        // A printer is enough to make a block — `with_config_block` emits nothing for no parts AND no
        // printer, which would make this test pass vacuously.
        let baked = crate::config::with_config_block(
            "sphere(2);",
            &[],
            Some(crate::config::PrinterCfg {
                bed: [200.0, 200.0, 200.0],
            }),
        );
        assert!(baked.contains("fab:config"), "fixture has no config block");
        p.add_file("alt.scad", baked);
        p.mark_saved(p.rev());
        let mut e = EditorBuf {
            path: p.editor_path(0),
            owner: Some(p.id()),
            ..Default::default()
        };
        let mut pending = PendingConfig::default();

        assert_eq!(set_entry(&mut p, &mut e, &mut pending, 1), Rerender::Target);
        assert_eq!(p.entry, 1);
        assert!(pending.0.is_some(), "the config block was not stashed");
        assert!(
            !e.text.contains("fab:config"),
            "raw config leaked into the editor"
        );
        assert!(!p.files[1].text.contains("fab:config"));
        assert!(p.is_dirty(), "the Save button would stay grey");
        assert!(p.editor_holds(&e));
        // Re-promoting the same file is a no-op — no wasted render.
        assert_eq!(set_entry(&mut p, &mut e, &mut pending, 1), Rerender::No);
    }

    /// A `.scadproj` homed at `home`, materialized (as `unpack_scadproj` would) under the SAME temp dir
    /// for every archive with that stem — the aliasing setup.
    fn archive(home: &str, entry_text: &str) -> ProjectDoc {
        use fab_scad::scadproj;
        let mut files = std::collections::BTreeMap::new();
        files.insert("main.scad".to_string(), entry_text.as_bytes().to_vec());
        files.insert("lib.scad".to_string(), b"module m(){}".to_vec());
        let bytes = scadproj::write_scadproj(
            &scadproj::project_from_files(files, Some("main.scad".into()), None).unwrap(),
        )
        .unwrap();
        let mut d =
            ProjectDoc::from_scadproj(&bytes, ProjectHome::ScadProj(PathBuf::from(home))).unwrap();
        d.base_dir = Some(PathBuf::from("/tmp/fab-gui/scadproj/brace"));
        d
    }

    /// Adopt `doc` into fresh-ish app state; returns what the status line and DocState read after.
    struct World {
        project: ProjectDoc,
        editor: EditorBuf,
        pending: PendingConfig,
        source: Option<PathBuf>,
        status: crate::state::Status,
        docs: crate::state::DocState,
    }

    impl World {
        fn new() -> Self {
            World {
                project: ProjectDoc::default(),
                editor: EditorBuf::default(),
                pending: PendingConfig::default(),
                source: Some(PathBuf::from("/tmp/fab-gui/scadproj/brace/main.scad")),
                status: crate::state::Status(String::new()),
                docs: crate::state::DocState::default(),
            }
        }
        fn adopt(&mut self, doc: ProjectDoc) {
            adopt(
                &mut self.project,
                &mut self.editor,
                &mut self.pending,
                &mut self.source,
                &mut self.status,
                &mut self.docs,
                doc,
            );
        }
        /// The native `SwitchFile(entry)` that follows an open: flush only a HELD buffer, then view.
        fn switch_to_entry(&mut self) {
            let entry = self.project.entry;
            switch(&mut self.project, &mut self.editor, entry);
        }
        fn unsaved(&self) -> bool {
            let live = crate::config::config_fp(&[], [256.0; 3]);
            self.docs.derive(&self.project, self.editor.dirty, &live)
        }
    }

    /// TG.2, finding A5 repro B: `a/brace.scadproj` is open with an unsaved edit, and the user opens
    /// `b/brace.scadproj`. Both entries are `main.scad` under the same temp dir, so the paths coincide
    /// — and path equality used to hand A's buffer to B (and Save then wrote A's code into B).
    #[test]
    fn adopting_a_same_stem_archive_never_inherits_the_buffer() {
        let mut w = World::new();
        w.adopt(archive("/m/a/brace.scadproj", "cube(1); // A"));
        w.editor.text = "cube(1); // A, edited".into();
        w.editor.dirty = true;

        let b = archive("/m/b/brace.scadproj", "sphere(2); // B");
        assert_eq!(
            b.editor_path(b.entry),
            w.editor.path,
            "the alias: same path"
        );
        assert!(
            !b.editor_holds(&w.editor),
            "B never claims A's buffer, path or no path"
        );

        w.adopt(b);
        assert_eq!(w.editor.text, "sphere(2); // B", "B's own entry text");
        assert!(w.project.editor_holds(&w.editor), "and B owns it now");
        assert!(!w.editor.dirty);
        assert_eq!(w.source, None, "cleared, so the switch re-renders + resets");
        w.switch_to_entry();
        assert_eq!(w.project.files[w.project.entry].text, "sphere(2); // B");
        assert!(!w.unsaved(), "a just-opened document has nothing to save");
        assert_eq!(w.status.0, "opened brace.scadproj (/m/b)");
    }

    /// TG.2: re-opening the SAME file is a revert — the saved text, clean — not a no-op that keeps the
    /// unsaved edit (finding A5 repro A).
    #[test]
    fn readopting_the_same_file_shows_the_saved_text_and_reads_clean() {
        let mut w = World::new();
        w.adopt(archive("/m/brace.scadproj", "cube(1);"));
        w.editor.text = "cube(9);".into();
        w.editor.dirty = true;
        w.project.flush_active(&w.editor.text); // the edit reached the document, too
        assert!(w.unsaved());

        w.adopt(archive("/m/brace.scadproj", "cube(1);"));
        w.switch_to_entry();
        assert_eq!(w.editor.text, "cube(1);");
        assert_eq!(w.project.files[w.project.entry].text, "cube(1);");
        assert!(!w.unsaved());
    }

    /// TG.2: the entry's fab:config is stripped from the editor and stashed for `poll_job`, and the
    /// document keeps the block (Save re-bakes it) without reading unsaved.
    #[test]
    fn adopt_stashes_the_entrys_config() {
        let baked = crate::config::with_config_block(
            "cube(1);",
            &[],
            Some(crate::config::PrinterCfg {
                bed: [180.0, 180.0, 180.0],
            }),
        );
        let mut w = World::new();
        w.adopt(archive("/m/brace.scadproj", &baked));
        assert!(!w.editor.text.contains("fab:config"));
        assert!(w.pending.0.is_some(), "the block waits for the parts");
        w.switch_to_entry();
        assert!(w.project.files[w.project.entry].text.contains("fab:config"));
        assert!(!w.unsaved());
    }

    /// TG.2: what the open says — the document and its REAL folder, `~`-abbreviated; no folder for a web
    /// item; nothing for a `Fresh` document (a demo is not an open).
    #[test]
    fn opened_status_names_the_document_and_its_folder() {
        let home = std::path::Path::new("/Users/c");
        let at =
            |h: ProjectHome| opened_status(&ProjectDoc::single("main.scad", "", h), Some(home));
        assert_eq!(
            at(ProjectHome::ScadProj(
                "/Users/c/models/bracket.scadproj".into()
            ))
            .as_deref(),
            Some("opened bracket.scadproj (~/models)")
        );
        assert_eq!(
            at(ProjectHome::ScadFile(
                "/Users/c/models/brace/main.scad".into()
            ))
            .as_deref(),
            Some("opened main.scad (~/models/brace)")
        );
        assert_eq!(
            at(ProjectHome::ScadFile("/srv/x/main.scad".into())).as_deref(),
            Some("opened main.scad (/srv/x)")
        );
        assert_eq!(
            at(ProjectHome::WebModel("Brace.scadproj".into())).as_deref(),
            Some("opened Brace.scadproj")
        );
        assert_eq!(at(ProjectHome::Fresh), None);
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(
            tilde(std::path::Path::new("/Users/cx"), Some(home)),
            "/Users/cx"
        );
    }

    /// A fresh scratch folder for one test (TG.6), holding `files` — the shape of a user's model folder.
    fn scratch(tag: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fab_tg6_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    /// A loose document opened on `dir/main.scad`, as `open_loose` builds it (its `.scad` siblings).
    fn loose_doc(dir: &std::path::Path, scads: &[&str]) -> ProjectDoc {
        let paths: Vec<PathBuf> = scads.iter().map(|n| dir.join(n)).collect();
        ProjectDoc::from_disk(dir.to_path_buf(), &paths, &dir.join("main.scad"))
    }

    /// TG.6: a loose Add copies EVERY file into the folder at once, `.scad` included (it used to wait for
    /// Save), and the folder then holds it — so the document reads clean, rows unmarked.
    #[test]
    fn a_loose_add_copies_into_the_folder_and_reads_clean() {
        let dir = scratch("add", &[("main.scad", b"cube(1);")]);
        let elsewhere = scratch(
            "add_src",
            &[("hook.scad", b"module hook(){}"), ("logo.png", b"\x89PNG")],
        );
        let mut doc = loose_doc(&dir, &["main.scad"]);
        assert!(!doc.is_dirty());
        assert_eq!(add_hover(&doc), "copy existing files into this folder");

        let r = add_paths(
            &mut doc,
            &[elsewhere.join("hook.scad"), elsewhere.join("logo.png")],
        );

        assert_eq!(r.added, ["hook.scad", "logo.png"]);
        assert!(r.refused.is_empty(), "{:?}", r.refused);
        assert_eq!(
            std::fs::read(dir.join("hook.scad")).unwrap(),
            b"module hook(){}"
        );
        assert_eq!(std::fs::read(dir.join("logo.png")).unwrap(), b"\x89PNG");
        assert!(!doc.is_dirty(), "the folder already holds both");
        let e = EditorBuf::default();
        assert!(!doc.file_unsaved(1, &e) && !doc.asset_unsaved("logo.png"));
        assert_eq!(
            r.status(None).as_deref(),
            Some(format!("added 2 file(s) — copied into {}", dir.display()).as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    /// TG.6: a name the folder already has is REFUSED with a status and nothing is overwritten — not
    /// even a file the document can't see (a loose open loads `.scad` only, so `unique_name` is blind
    /// to `logo.svg`), and not de-duplicated into a `-1` copy nobody asked for. A file the document
    /// already holds is refused too. Re-adding the folder's OWN file (a session-deleted one) re-imports.
    #[test]
    fn a_loose_add_refuses_an_on_disk_clash_and_overwrites_nothing() {
        let dir = scratch(
            "clash",
            &[
                ("main.scad", b"cube(1);"),
                ("lib.scad", b"// mine"),
                ("logo.svg", b"<svg/>"),
            ],
        );
        let elsewhere = scratch(
            "clash_src",
            &[
                ("logo.svg", b"<svg>theirs</svg>"),
                ("main.scad", b"// theirs"),
            ],
        );
        let mut doc = loose_doc(&dir, &["main.scad", "lib.scad"]);
        assert_eq!(
            doc.unique_name("logo.svg"),
            "logo.svg",
            "the document is blind to it"
        );

        let r = add_paths(
            &mut doc,
            &[elsewhere.join("logo.svg"), elsewhere.join("main.scad")],
        );

        assert!(r.added.is_empty());
        assert_eq!(
            r.refused,
            [
                "add: logo.svg already exists in the folder — not overwritten",
                "add: main.scad is already in the project",
            ]
        );
        assert_eq!(std::fs::read(dir.join("logo.svg")).unwrap(), b"<svg/>");
        assert_eq!(std::fs::read(dir.join("main.scad")).unwrap(), b"cube(1);");
        assert!(!dir.join("logo-1.svg").exists(), "no silent rename");
        assert!(!doc.assets.contains_key("logo.svg") && doc.files.len() == 2);
        assert_eq!(
            r.status(None).as_deref(),
            Some(
                "add: logo.svg already exists in the folder — not overwritten; \
                 add: main.scad is already in the project"
            )
        );

        // The folder's own file, removed from the session, comes back without a write or a clash.
        let mut e = EditorBuf::default();
        delete(&mut doc, &mut e, 1).unwrap();
        let back = add_paths(&mut doc, &[dir.join("lib.scad")]);
        assert_eq!(back.added, ["lib.scad"]);
        assert!(back.refused.is_empty(), "{:?}", back.refused);
        assert!(!doc.is_dirty());
        assert_eq!(
            back.status(None).as_deref(),
            Some(format!("added 1 file(s) — already in {}", dir.display()).as_str()),
            "no copy happened, so the status doesn't claim one"
        );
        assert_eq!(std::fs::read(dir.join("lib.scad")).unwrap(), b"// mine");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    /// TG.8: the rename refusal the Project tab and the harness share — a name another file in the folder
    /// already has (even one the document doesn't list) is refused; renaming a file to its own name, a
    /// blank name, or a document with no folder is not a clash.
    #[test]
    fn rename_clash_refuses_an_existing_folder_file() {
        let dir = scratch(
            "clash",
            &[("main.scad", b"cube(1);"), ("logo.png", b"\x89PNG")],
        );
        let doc = loose_doc(&dir, &["main.scad"]);
        assert_eq!(
            rename_clash(&doc, 0, " logo.png "),
            Some("rename: logo.png already exists — pick another name".into())
        );
        assert_eq!(rename_clash(&doc, 0, "main.scad"), None, "its own name");
        assert_eq!(rename_clash(&doc, 0, "  "), None);
        assert_eq!(rename_clash(&doc, 0, "new.scad"), None);
        let mut web = ProjectDoc::single("main.scad", "", ProjectHome::Fresh);
        web.base_dir = None;
        assert_eq!(
            rename_clash(&web, 0, "logo.png"),
            None,
            "no folder, no clash"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.6: a loose Delete is session-only — the file stays in the folder, the hover says so, and
    /// neither it nor a set-entry marks the document unsaved (the folder has no manifest to record
    /// either). A container's delete keeps the plain wording.
    #[test]
    fn a_loose_delete_leaves_the_file_on_disk_and_set_entry_stays_clean() {
        let dir = scratch(
            "del",
            &[
                ("main.scad", b"cube(1);"),
                ("lib.scad", b"// l"),
                ("old.scad", b"// o"),
            ],
        );
        let mut doc = loose_doc(&dir, &["lib.scad", "main.scad", "old.scad"]);
        let mut e = EditorBuf::default();
        assert_eq!(
            delete_hover(&doc),
            "hold to remove from this session — the file stays in the folder"
        );

        // The native DeleteFile arm rm's exactly what `delete_disk_target` names: nothing, here.
        assert_eq!(delete_disk_target(&doc, "old.scad"), None);
        delete(&mut doc, &mut e, 2).unwrap();
        assert!(doc.files.iter().all(|f| f.name != "old.scad"));
        assert_eq!(std::fs::read(dir.join("old.scad")).unwrap(), b"// o");
        let mut pending = PendingConfig::default();
        assert_eq!(
            set_entry(&mut doc, &mut e, &mut pending, 0),
            Rerender::Target
        );
        assert!(!doc.is_dirty() && !e.dirty, "nothing for Save to write");

        doc.home = ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj"));
        assert_eq!(delete_hover(&doc), "hold to remove from the project");
        assert_eq!(add_hover(&doc), "add existing files to this project");
        assert_eq!(
            delete_disk_target(&doc, "old.scad"),
            Some(dir.join("old.scad")),
            "a container's temp copy IS removed"
        );
        doc.home = ProjectHome::Fresh;
        assert_eq!(delete_disk_target(&doc, "old.scad"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.6: a loose open recurses, so the document can hold `lib/util.scad`. Re-adding that file after a
    /// session delete must come back as `lib/util.scad` with no write — not as a stray `util.scad` copy
    /// in the folder root — and adding a nested file the document still holds is refused, not copied.
    #[test]
    fn a_loose_add_of_a_nested_folder_file_reimports_in_place() {
        let dir = scratch(
            "nested",
            &[("main.scad", b"cube(1);"), ("lib/util.scad", b"// u")],
        );
        let mut doc = loose_doc(&dir, &["main.scad", "lib/util.scad"]);
        let held = add_paths(&mut doc, &[dir.join("lib/util.scad")]);
        assert!(held.added.is_empty());
        assert_eq!(
            held.refused,
            ["add: lib/util.scad is already in the project"]
        );

        let mut e = EditorBuf::default();
        delete(&mut doc, &mut e, 1).unwrap();
        let back = add_paths(&mut doc, &[dir.join("lib/util.scad")]);
        assert_eq!(back.added, ["lib/util.scad"]);
        assert!(back.refused.is_empty(), "{:?}", back.refused);
        assert!(doc.files.iter().any(|f| f.name == "lib/util.scad"));
        assert!(!dir.join("util.scad").exists(), "no stray root copy");
        assert!(!doc.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.6: a container Add keeps the document's rules — de-duplicated, unsaved until Save — and
    /// writes the temp too, which is where the render's `import()` reads. With no folder at all (a
    /// fresh session) it lands in the document only.
    #[test]
    fn a_container_add_dedups_stays_unsaved_and_feeds_the_temp() {
        let temp = scratch("cont", &[("main.scad", b"cube(1);")]);
        let elsewhere = scratch(
            "cont_src",
            &[("main.scad", b"// other"), ("logo.png", b"P")],
        );
        let mut doc = ProjectDoc::single(
            "main.scad",
            "cube(1);",
            ProjectHome::ScadProj(PathBuf::from("/m/brace.scadproj")),
        );
        doc.base_dir = Some(temp.clone());

        let r = add_paths(
            &mut doc,
            &[elsewhere.join("main.scad"), elsewhere.join("logo.png")],
        );

        assert_eq!(r.added, ["main-1.scad", "logo.png"]);
        assert!(doc.is_dirty());
        assert_eq!(std::fs::read(temp.join("logo.png")).unwrap(), b"P");
        assert_eq!(
            std::fs::read(temp.join("main-1.scad")).unwrap(),
            b"// other"
        );
        assert_eq!(std::fs::read(temp.join("main.scad")).unwrap(), b"cube(1);");
        assert_eq!(
            r.status(None).as_deref(),
            Some("added 2 file(s) to the project")
        );

        let mut fresh = ProjectDoc::single("untitled.scad", "", ProjectHome::Fresh);
        let r = add_paths(&mut fresh, &[elsewhere.join("logo.png")]);
        assert_eq!(r.added, ["logo.png"]);
        assert!(fresh.is_dirty() && fresh.asset_unsaved("logo.png"));
        let missing = add_paths(&mut fresh, &[elsewhere.join("nope.scad")]);
        assert!(missing.added.is_empty() && missing.refused.len() == 1);
        let _ = std::fs::remove_dir_all(&temp);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }
}
