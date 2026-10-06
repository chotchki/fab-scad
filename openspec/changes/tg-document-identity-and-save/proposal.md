# Phase TG - The GUI says what document is open, and Save saves all of it

## Why

Dogfooding v1.4.1 (chotchki, 2026-10-05): with a `.scadproj` open, nothing says what is open, and after adding a file there is no way to save it. Save is grey, ⌘S does nothing, and the Project tab has no Save at all. The bug isn't the button. The GUI has no notion of "this document has unsaved changes". Save, ⌘S and the "unsaved" badge read the EDITOR's flag, which every file switch reloads from the viewed file. Native Add, Delete and Set entry never set it, and cut, connector and orientation edits don't either, though Save writes them. The same gap hides two silent data-loss bugs. Opening a second `.scadproj` with the same name writes the old editor text into it. And the web site-save uploads the active file without its live edits.

## What Changes

- **Unsaved state belongs to the document.** The document is unsaved when Save would write something it hasn't written. That covers every file's text, the file set, the entry, binary assets, and the baked `fab:config` (cuts, connectors, manual orientations, bed). Switching files never hides it. Save, ⌘S, the badge, the row markers and the window title all read this one state.
- **Save is enabled exactly when the document is unsaved** (grey when clean, the macOS default; chotchki, 2026-10-05). ⌘S on a clean document still answers: "nothing to save — X is up to date".
- **Save always reports.** Success names what was written and where, and a failure says why and leaves the document unsaved (today it clears every marker either way). A `.scadproj` save is atomic, so an interrupted write leaves the old archive intact.
- **Save As (⇧⌘S) on every desktop document.** It writes `.scad` for one file and `.scadproj` for more, and the document moves to the new file. It replaces the gold "Save as .scadproj…" promote button. A session started without a file saves through it, where today Save silently writes nothing.
- **Opening replaces, never merges.** An opened document never inherits the previous editor buffer, and it always re-renders. While the document is unsaved, Open… is press-and-hold, like delete (no modal). The status names what opened and from where. `fab-gui x.scadproj` opens the project; today it hangs.
- **The open document is named everywhere.**
  - The Project tab leads with a document card: name, folder, kind, what Save does there, and Save / Save As… / Open….
  - The window title reads `<document> — fab-scad`, with "(unsaved)" when unsaved.
  - A header chip names the document on every tab.
  - The Model tab says which file is in the editor and which file renders.
  - The unsaved marker is a filled dot, distinct from the stale ring.
- **Loose folders keep the SW.3 doctrine, applied consistently** (chotchki, 2026-10-05). Add and Rename change the folder immediately, now for `.scad` files too, not only assets. Delete hides the file for the session and says so; the file stays on disk. Text edits wait for Save.
- **The web saves what is on screen.** "Save to hotchkiss.io" and web Publish upload the live text of the active file, and a successful site save clears the unsaved state.
- **The harness can drive the document.** `--script` gains open/add/new/view/set-entry/rename/delete/save/save-as/expect-dirty verbs. An e2e test opens a `.scadproj`, adds a file, saves and unzips the result. No test today checks that Save can be reached, which is how this shipped.
- **Out of scope**, recorded in `design.md` and in `openspec/backlog.md`:
  - **Follow-up after v1.4.2:**
    - viewing a non-entry file stops marking every tab stale;
    - native file operations go through the shared `file_ops` rules, so rename and delete keep the cut plan;
    - Export, Publish and the self-update relaunch use the real document path instead of the temp unpack.
  - **A separate safety net:**
    - a close/quit guard and a recovery snapshot;
    - web `beforeunload`;
    - the macOS edited dot;
    - Finder document association.

## Capabilities

### New Capabilities

- `gui/document`: what the desktop and web GUI show about the open document, when it counts as unsaved, what Save, Save As and Open do for each kind of document (`.scadproj`, loose `.scad` folder, unsaved, hotchkiss.io item), and what they report.

### Modified Capabilities

None. `openspec/specs/` is still empty (TF's `import/3mf` syncs at its archive).

## Impact

- **Code, all in `gui/src/`:**
  - `project.rs`: the document's unsaved state, revision, manifest title and identity.
  - A new `save.rs`: one save path, the save plan per home, a snapshot that always carries the live editor text, and the atomic write.
  - `file_ops.rs`: adopting an opened document.
  - `state.rs`: the document-state resource.
  - `panel.rs`: the document card, the Save / Save As controls, the header chip, the Model breadcrumb and the markers. `save_buffer` leaves `panel_ui`, which is at Bevy's 16-parameter cap.
  - `jobs.rs`: open, add, the Save As rewrite and the web save.
  - Also `config.rs` (config fingerprint), `publish_web.rs`, `scene.rs` (window title), `lib.rs` (`.scadproj` argument, harness registration) and `script.rs` (verbs).
  - A new `gui/tests` e2e, plus a small committed `.scadproj` fixture.
- **No new dependencies.** The window title goes through Bevy's `Window.title`.
- **Owning doc:** `docs/web-projects-design.md`, which holds the Phase Z / SW document doctrine for both platforms despite its name. It gains the document-state and save rules. `gui/CLAUDE.md` gains the convention that Save UI reads document state, never the editor's flag, and that a filled dot means unsaved while a ring means stale. README "Using it" gains the Project tab and Save / Save As.
- **Release reach:** CI on every push. A `v*` tag (v1.4.2, only on chotchki's word) puts the macOS dmg/.app, the Windows installer and the web bundle on one GitHub release and publishes `latest.json` to the macOS auto-updater. hotchkiss.io's editor gets the web half when its `build.rs` pin moves.
- **One-way doors:** none. `latest.json`'s shape and the signing key are untouched, and the `.scadproj` format is unchanged; it now keeps the manifest title it used to drop on re-zip. The update INTO v1.4.2 still runs 1.4.1's relaunch code, which reopens a `.scadproj`'s temp copy, so the release notes say to close any open project before updating.
