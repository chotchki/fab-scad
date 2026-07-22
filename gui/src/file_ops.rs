//! Project-tab document MUTATIONS (Z.3.10) — rename / new / delete / set-entry, as pure functions over
//! [`ProjectDoc`] + [`EditorBuf`].
//!
//! Why a module instead of code inside the handlers: the handlers are per-platform (native drives a
//! filesystem render root and the rfd picker; the web drives an in-memory `render_pack`), but the RULES
//! are identical, and the rules are where the bugs live. This repo has no wasm test harness, so anything
//! that ends up inside `#[cfg(target_arch = "wasm32")]` ships unexercised by CI. Everything here is
//! cfg-free and unit-tested on the native target; the wasm system is a wiring shim over it.
//!
//! The invariant every function upholds: **`editor.path` names `files[active]`**. On the web that path
//! is an identity token, not a location — [`ProjectDoc::editor_holds`] is the flush-before-switch
//! predicate, and if a rename leaves it stale the next file switch silently discards the user's unsaved
//! edits (the buffer is judged to belong to some other file, so it's never written back).

// The native build only calls `rename` from here — its other handlers still route New/Delete/SetEntry
// through `SwitchFile`, which is right on a platform with a real render root to materialize into. The
// rest is live on wasm and exercised by the tests below on BOTH targets, which is the point: this is
// where the rules get tested, precisely because the wasm handler can't be.
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
    editor.dirty = true;
    Some(Renamed { old, new })
}

/// Add a blank `.scad` and view it. Returns its index. Never re-renders: nothing `include`s a file the
/// moment it's created, so the entry's geometry is unchanged.
pub(crate) fn new_file(project: &mut ProjectDoc, editor: &mut EditorBuf) -> usize {
    let idx = project.add_file("untitled.scad", String::new());
    switch(project, editor, idx);
    editor.dirty = true;
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
    editor.dirty = true;
    Ok(if was_entry {
        Rerender::Target
    } else {
        Rerender::Same
    })
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
    // `set_entry` marks no file dirty — the change belongs to the DOCUMENT (the .scadproj manifest),
    // not to any one file. Without this the web Save button, which gates on `editor.dirty`, stays grey
    // and the new entry can never be saved.
    editor.dirty = true;
    Rerender::Target
}

/// Flush the live buffer back into its owning file, then hydrate the editor VIEW from `files[i]` — the
/// in-memory twin of native's `read_into_editor`. The flush is conditional on [`ProjectDoc::editor_holds`]:
/// if the buffer doesn't belong to `files[active]`, writing it there would overwrite one file with
/// another's text.
fn switch(project: &mut ProjectDoc, editor: &mut EditorBuf, i: usize) {
    if project.editor_holds(&editor.path) {
        project.flush_active(&editor.text);
    }
    project.set_active(i);
    if let Some(f) = project.files.get(i) {
        editor.text = f.text.clone();
        editor.dirty = f.dirty;
    }
    editor.path = project.editor_path(i);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectHome;

    /// A web-shaped document (no `base_dir`), the case with no test harness of its own.
    fn web_doc() -> (ProjectDoc, EditorBuf) {
        let mut p = ProjectDoc::single("main.scad", "cube(1);", ProjectHome::Fresh);
        p.add_file("hook.scad", "module hook(){}".to_string());
        p.files[1].dirty = false;
        let e = EditorBuf {
            text: "cube(1);".into(),
            path: p.editor_path(0),
            ..Default::default()
        };
        (p, e)
    }

    /// THE invariant: after renaming the ACTIVE file, `editor.path` must still name it — using the name
    /// that LANDED, not the one typed. Get this wrong and the next switch throws the buffer away.
    #[test]
    fn renaming_the_active_file_repoints_the_editor_at_the_landed_name() {
        let (mut p, mut e) = web_doc();
        let r = rename(&mut p, &mut e, 0, "base.scad").expect("renamed");
        assert_eq!(r.old, PathBuf::from("main.scad"));
        assert_eq!(r.new, PathBuf::from("base.scad"));
        assert!(p.editor_holds(&e.path), "editor.path went stale");
        assert!(e.dirty);

        // A COLLIDING rename de-dups; the editor must follow the de-duped name, not the typed one.
        let r = rename(&mut p, &mut e, 0, "hook.scad").expect("renamed");
        assert_eq!(r.new, PathBuf::from("hook-1.scad"));
        assert_eq!(e.path, PathBuf::from("hook-1.scad"));
        assert!(p.editor_holds(&e.path));
    }

    /// Renaming a NON-active file must leave the editor pointed where it was.
    #[test]
    fn renaming_another_file_leaves_the_editor_alone() {
        let (mut p, mut e) = web_doc();
        let before = e.path.clone();
        rename(&mut p, &mut e, 1, "brace.scad").expect("renamed");
        assert_eq!(e.path, before, "a non-active rename moved the editor");
        assert!(p.editor_holds(&e.path));
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
        assert!(p.editor_holds(&e.path));
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
        assert!(p.editor_holds(&e.path));
        assert!(e.dirty);
        // The flush on the way out preserved the edit to the file we left.
        assert_eq!(p.files[0].text, "cube(3);");
    }

    /// Delete: the entry going away is a TARGET change; a non-entry going away is not. The last file
    /// refuses outright.
    #[test]
    fn delete_reports_whether_the_render_target_moved() {
        let (mut p, mut e) = web_doc();
        assert_eq!(delete(&mut p, &mut e, 1), Ok(Rerender::Same));
        assert!(p.editor_holds(&e.path));
        assert_eq!(
            delete(&mut p, &mut e, 0),
            Err("can't delete the project's only file")
        );

        let (mut p, mut e) = web_doc();
        assert_eq!(delete(&mut p, &mut e, 0), Ok(Rerender::Target)); // index 0 was the entry
        assert_eq!(p.files.len(), 1);
        assert!(p.editor_holds(&e.path));
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
        let mut e = EditorBuf {
            path: p.editor_path(0),
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
        assert!(e.dirty, "the Save button would stay grey");
        assert!(p.editor_holds(&e.path));
        // Re-promoting the same file is a no-op — no wasted render.
        assert_eq!(set_entry(&mut p, &mut e, &mut pending, 1), Rerender::No);
    }
}
