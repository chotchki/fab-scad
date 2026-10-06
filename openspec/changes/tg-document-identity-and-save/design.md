# Design

## Context

Phase Z made every document a project (`ProjectDoc` in `gui/src/project.rs`), and its home (`Fresh`, `ScadFile` for a loose folder, `ScadProj`, `WebModel`) decides where Save writes. The Save GATE predates that. It is W.3.40's `editor.dirty`, a view flag that `doc_into_editor` (`state.rs:119-126`) reloads from the viewed file on every switch. Every surface reads that one flag: the Save button (`panel.rs:749`), ⌘S (`panel.rs:237`) and the badge (`panel.rs:760`). The gate has been patched at call sites three times (W.3.40, Z.3.10's web `editor.dirty = true`, the bed write at `panel.rs:965`), and each patch is undone by the next file switch. Native New, Delete and SetEntry also bypass `file_ops` (`file_ops.rs:15-18`).

Save itself is three branches inside `panel_ui`'s `save_buffer`, plus `save_as_project_action` for the loose→`.scadproj` promote and `save_action` for the web site-save. `panel_ui` is at Bevy's 16-parameter cap. A `.scadproj` unpacks to `scene.tmp/scadproj/<stem>`, and `editor_holds` (`project.rs:147`) decides ownership of the live buffer by path equality alone. Two archives with the same stem therefore alias, and so does reopening the same file. The motivation and the full defect list are in proposal.md (Why); the audit behind it traced each defect to file:line on `62cc7850`.

## Goals / Non-Goals

**Goals:**
- One answer to "is the document unsaved?" that no future mutation can forget to update, and that no view switch can reset.
- One save path, with one result type, that every platform and home goes through, so the web and native Save can't drift again.
- The rules live in cfg-free functions, unit-tested on native. The wasm systems stay thin shims, because wasm has no test harness (the `file_ops.rs` doctrine).

**Non-Goals** (the follow-up after v1.4.2, filed in `openspec/backlog.md`):
- **Stale vs unsaved logic.** `sync_pipeline` hashes the VIEWED file against the entry's render, so viewing a library marks every tab stale. TG only makes the two markers look different; the hash fix is the follow-up.
- **Native file ops through `file_ops`.** Rename and container Delete still `state.reset()` and wipe the cut plan. TG's dirty model lives in the `ProjectDoc` mutators, so it does not depend on this.
- **Home path instead of temp path** for Export plates, the Publish title default, the self-update relaunch, and a per-process, per-path temp dir name. TG's DocId closes the aliasing hole without renaming the temp dir.
- **The data-loss net:**
  - a window-close guard (`close_when_requested: false` plus an inline hold-to-close);
  - a recovery snapshot for ⌘Q, which winit 0.30 cannot intercept, and for crashes;
  - web `beforeunload`;
  - the macOS edited dot (`WindowExtMacOS::set_document_edited`, which needs a direct winit dep);
  - Finder `CFBundleDocumentTypes`.
- **Other deferrals:** the `fab-gui` wordmark (branding, not identity), a browser-tab title on the web (the host page owns `<title>`, a `docs/web-embed.md` contract question), "Save a Copy…", and Revert.

## Decisions

**1. Unsaved is a document property: structural bits set inside the `ProjectDoc` mutators, config dirtiness by fingerprint.**
Every structural change already funnels through a handful of `ProjectDoc` methods: `add_file`, `import`, `remove_file`, `set_entry`, `rename_file` and `flush_active`. Each one marks the document there.
- Text files keep their per-file `dirty`, set by `flush_active` only when the text changed, as today.
- `import` of a binary records the name in `unsaved_assets`, so binary rows get a marker.
- Everything else sets `structure_dirty`.
- A monotonic `rev` bumps on every mutation.
- `is_dirty()` is: any file dirty, OR any unsaved asset, OR `structure_dirty` when the home PERSISTS structure. That is true for `ScadProj`, `Fresh` and `WebModel`, and false for a loose `ScadFile`, whose structural changes are immediate or session-only (Decision 7).
- `mark_saved(at_rev)` clears only if `rev` hasn't moved since the save captured its snapshot. A save that raced an edit (an rfd dialog on Windows, or a web upload) then leaves the later edit unsaved.

Config has no chokepoint: `Parts` is written from about six systems (cuts, connectors, orient, AutoPlace, Reset-to-auto, the bed). So `config` gains a per-part fingerprint, a hash of exactly what `part_to_slicing` would persist for that part, plus the bed. A `DocState` resource holds the baseline. It is re-taken:
- after a fresh parts build, once the pending `fab:config` has been applied (open, or an entry change);
- for one part, when an auto-plan lands on a part whose baseline is empty (no saved plan, so a re-open would re-derive it);
- on save success.

The result: an unplanned model that auto-plans on open reads clean, and Reset-to-auto over a saved manual plan reads unsaved. That is "Save would write something different", as the spec defines it.

Alternatives:
- (a) Keep flipping `editor.dirty` at call sites. Three patches have shown it regresses with every new gesture.
- (b) A full saved-image diff, which removes every per-file flag and compares each document byte against a snapshot. It is exact (an edit then undo reads clean) and gives a "what changed" list. But it rewrites every dirty reader, including wasm code no test compiles, and buys nothing the report needs.
- (c) Fingerprinting the files too. Hashing every file's text each frame costs more than the flags the mutators already maintain.

**2. One predicate, one resource.** A `sync_doc_state` system runs next to `sync_pipeline` and sets `DocState.dirty = project.is_dirty() || editor.dirty || config_dirty`. Every surface reads `DocState`: Save, ⌘S, the badge, row markers, the window title, the header chip and the Open hold. `editor.dirty` shrinks to its honest meaning, "the live buffer was edited since it was loaded", which still matters because the live buffer only reaches `ProjectDoc` on a flush. The bed write at `panel.rs:965` is deleted; the bed is in the fingerprint. `DocState` joins `PanelView`, so `panel_ui` gains no parameter.

**3. A typed document identity, and `adopt` for every open.** Every `ProjectDoc` constructor stamps a process-unique `DocId`. `EditorBuf` records the `DocId` whose file it holds, and `editor_holds` requires both the id and the path to match. Aliasing is then unrepresentable for any future loader, not just today's call sites; this is chotchki's invariant-types rule (M.5.4.5). A cfg-free `file_ops::adopt` is the one way a loaded document becomes the open one: `poll_open_dialog`, boot, the web model fetch and the script `open` verb all use it. It stamps the owner, hydrates the editor from the entry, clears `scene.source` so the next switch reads as a target change (re-render, `state.reset()`, pending config applied), and posts `opened <name> (<folder>)`.

Alternative: just clear `editor.path` on open. That fixes today's two loaders and leaves the next one to rediscover the bug. A per-FILE id was considered and rejected for TG: nothing found needs it.

**4. One save path in a new cfg-free `gui/src/save.rs`.**
- `plan(&ProjectDoc, platform, has_site_target) -> SavePlan` returns one of `Rezip(path)`, `WriteLoose(dir)`, `NeedsSaveAs`, `Download` or `Site`. It is the single table of "where does Save go", and it also feeds the hover text and the card's one-line Save rule.
- `DocSnapshot::capture(&ProjectDoc, live_active_text)` is the ONLY input `rezip_project` and `project_source_variant` accept, and it always splices in the live editor text. The web site-save bug (an unflushed active file) then becomes a type error instead of a convention.
- `atomic_write(path, bytes)` writes a sibling temp file and renames it over the target. It preserves the original's permissions and removes the temp on failure.
- `save_doc_action` handles `PanelCmd::Save`, which the button and ⌘S both write. So `save_buffer` leaves `panel_ui`, and the parameter cap stops shaping the save logic. It reports every outcome through `Status`.
  - A loose save clears dirty only on the files whose write succeeded.
  - After a container save the temp is re-materialized, so `import()` reads what was saved; that closes `materialize_imports`' KNOWN LIMIT for containers.
  - The manifest title rides through the re-zip: `ProjectDoc` keeps it from `read_scadproj`, and `rezip_project` stops passing `None`.

The cost is one frame: Save becomes a message instead of a direct call, which is invisible.

**5. Save As replaces the promote, for every native home.** ⇧⌘S is checked BEFORE ⌘S, because egui's `consume_key` matches modifiers logically, so plain ⌘S would also swallow ⇧⌘S.
- **Format:** the dialog offers `.scad` for one text file with no assets, and `.scadproj` otherwise.
- **Defaults:** the suggested name comes from `doc_stem()`. The directory comes from a new `home_dir()`: the `.scadproj`'s folder, the loose folder, or the user's home for `Fresh`. It is never `scene.source`, which points into the temp dir for a container.
- **The bytes** come from a `DocSnapshot` taken with its `rev` when the dialog opens, and success calls `mark_saved(rev)`.
- **Re-homing:**
  - To `.scadproj`: today's `poll_save_project` path. Absorb a loose folder's assets, set `home = ScadProj(new)`, and re-root to a fresh temp materialization.
  - To `.scad`: `home = ScadFile(new)`, `base_dir = new folder`, and the file takes the new name.
- **Native `Fresh`** boots seeded as `ProjectDoc::single("untitled.scad", "", Fresh)` with the editor owned by it, so typed or pasted text lands in a real document. `plan()` returns `NeedsSaveAs` for it, so Save opens Save As (the macOS convention). Today `fs::write("")` fails silently.

**6. Show the document; text from pure functions, layout in `panel_ui`.** `doc_card(&ProjectDoc, &DocState, plan) -> DocCard`, `window_title(..)` and `breadcrumb(..)` are pure and table-tested. The UI only lays them out:
- **The Project tab card:**
  - name at 16pt;
  - place and kind, e.g. `~/models · .scadproj`, `~/models/brace · loose folder`, `not saved yet`, or `on hotchkiss.io`;
  - the Save rule;
  - `[Save]` (gold and enabled when unsaved, grey when clean), `[Save As…]` and a text-only `[Open…]`. Today Open… and Add… share the `+` glyph.
- **The window title:** a native-only system writes `Window.title` only when the string changes. bevy_winit calls `set_title` on change.
- **The header:** a chip after the tagline shows the name and the unsaved marker, with the full path on hover; clicking it goes to the Project tab.
- **The Model tab:** a labelled `[Save]`, whose hover comes from `plan()` (e.g. "rewrite ~/models/bracket.scadproj"), and the breadcrumb `hook.scad · in bracket.scadproj · main.scad renders`.
- **The markers:** the unsaved marker is a painter-drawn FILLED circle (`circle_filled`). The stale marker stays `icons::DOT`, which the font subset instances at FILL=0 and so renders as a ring. That gives two shapes with no font regeneration and no tofu risk (gui/CLAUDE.md).

Alternative: a second Material Symbols glyph. That costs a subset rebuild for what a painter circle does.

**7. Loose folders: SW.3's rules made consistent and visible** (chotchki, 2026-10-05).
- **Add** copies EVERY added file into the folder immediately, not only the importable ones `materialize_imports` copies today. It refuses a name that already exists on disk, the way rename already refuses one. The copied file is then clean.
- **Rename** stays immediate.
- **Delete** stays session-only, and its hover and the card say "remove from this session — the file stays in the folder".
- **set-entry** in a loose folder is a session view change: nothing records a loose entry, the folder has no manifest. Save bakes the config into whichever file is the entry when it runs.

Text edits are the only loose change that waits for Save. Under Decision 1's rule, a loose document is therefore unsaved exactly when some file's text or the config differs from disk.

Alternative: "nothing touches disk until Save". It is a cleaner story, but an added `import()` asset would have to render from an in-memory overlay before Save, and Save would have to delete files from real folders. Rejected on chotchki's call.

**8. The open hold reuses `hold_to_delete`.** That helper generalizes to `hold_to_confirm(ui, id, label, hover, danger)`. While the document is unsaved, Open… becomes a hold button reading "hold to discard changes and open…"; when clean it is a plain click. There is no modal, which is house style (`panel.rs:155`).

**9. The harness drives the same functions the UI does.**
- **New `--script` verbs:** `open <path>` (a `.scadproj` goes through `unpack_scadproj` + `adopt`), `addfile <path>`, `newfile`, `view <i>`, `setentry <i>`, `rename <i> <name>`, `delete <i>`, `save`, `saveas <path>` (bypasses rfd and runs the same re-home), and `expect dirty|clean` (a mismatch exits non-zero).
- **The verbs** call the same cfg-free functions as the handlers (`ProjectDoc` mutators, `file_ops`, `save`). The add-dialog handler and the `addfile` verb share one `add_paths` routine.
- **The CI-run e2e** is an in-crate headless test in the `harness_tests.rs` style: MinimalPlugins, no render, `run_script` + doc systems. It runs a committed two-file `.scadproj` fixture with one asset through: open → addfile → expect dirty → view 0 → expect dirty → save → expect clean. Then it unzips the written archive and checks that the added file and the title are there. It also covers the same-stem re-open.
- **`run_scripted`** registers the project and save systems, so offscreen screenshots of the new card are possible. The visual check is a manual task box (offscreen `--script` renders egui reliably), not CI.
- **`native_entry`** accepts a `.scadproj` argument and routes it through unpack + adopt. Today it hangs.

## Risks / Trade-offs

- [The config fingerprint misreads float round-trips, so a freshly opened model reads unsaved] → The baseline is taken from the LIVE parts after the pending config is applied, never by re-parsing the file. Both sides of the comparison come from the same projection of the same `f32`s. TG.1's tests open a saved plan and assert clean.
- [The auto-plan baseline rule misfires (an auto-plan lands after a manual edit to a no-saved-plan part)] → The re-baseline is per part and only when that part's baseline is EMPTY, and AutoSlice replaces a part's plan wholesale. A manual edit followed by an auto-plan that replaces it reads clean, which matches what a re-open would show. Tested in TG.1.
- [A `DocId` mismatch strands a legitimate buffer (e.g., a code path that replaces `ProjectDoc` without `adopt`)] → `adopt` is the only constructor path the loaders use. A debug assertion in `sync_doc_state` fires when `editor.path` names a doc file but the owner differs.
- [Moving Save out of `panel_ui` changes ⌘S timing] → It is one frame, the same latency every other `PanelCmd` already has.
- [`atomic_write` across a filesystem boundary] → The temp file is a SIBLING of the target, so the rename stays on the same filesystem. On failure the original is untouched and the status says so.
- [Save As `.scad` to another folder loses on-disk sibling imports] → That is the same as any OpenSCAD save-as. The dialog only offers `.scad` when the document has no assets, and the status names the new folder.
- [Loose Add of a `.scad` now writes into the user's real folder immediately] → That is the recorded SW.3 doctrine ("explicit file ops do"), now applied to `.scad` too. Collisions are refused, never overwritten.
- [Web halves (site-save snapshot, `rev` on `SaveJob`) have no wasm harness] → The serialization goes through `DocSnapshot`, which is cfg-free and unit-tested. The wasm shim is a call site, and boot-gate compiles it.
- [The update into v1.4.2 runs 1.4.1's relaunch code, which reopens a container's temp copy] → The release notes say to close any open project before updating. The relaunch fix is in the follow-up.

## Migration Plan

No data migration: the `.scadproj` format is unchanged, and the title now survives a re-zip instead of being dropped. Ship TG.1–TG.7 on main behind nothing (each task leaves the app working). Run the TG.7 offscreen visual check, then tag v1.4.2 on chotchki's word. Rollback is the previous release; no file written by TG is unreadable by v1.4.1.
