# Multi-file SCAD projects on the web — design

**Verdict: this is mostly WIRING, not new architecture.** fab-lang already resolves `include`/`use`
from an in-memory `{path → source}` map with zero filesystem (`resolve_geometry_from_sources`,
`lang/src/lib.rs:297`; `drive_from_map` at `base_dir=""`, `lang/src/eval/io.rs:108`), and the wasm worker
already feeds it one (`Source::Bytes { main, libs: Vec<(String, Vec<u8>)> }`, `src/geomsg.rs:104`). A
local-include e2e already PASSES on that path (`src/geomsvc.rs:1140`, `include <lib/box.scad>` served from
an in-memory `libs`). What's missing is narrow: there's no CONTAINER to get a multi-file project into the
browser, and the web's lib pack is TEXT-only so binary assets can't ride it. A `.scadproj` zip closes both.

## The trigger

`models/shower_holder/shower_holder.scad` opens with `include <hook.scad>` — a PROJECT-LOCAL file, not a
library. On the desktop that resolves beside the model; on the web (fs-less) the app hands the worker a
single `.scad` plus a fetched LIBRARY closure (BOSL2 et al. from `libs.json`), and `hook.scad` isn't in it,
so the render dies. Every real project bigger than one file hits this. And chotchki's `.png`/`.svg`
(`import()`/`surface()`) assets are the same problem wearing a different hat — see "The asset win" below.

## What already works (and where the walls are)

The include resolver is split PURE-core / IO-shell (the M.4 boundary). The pure path never touches disk:

- `loader::resolve_graph` (`lang/src/eval/loader.rs:158`) BFS-walks `use`/`include` and NAMES a missing
  reference as a `ScadNeed` instead of reading it. The caller supplies a `SourceMap`
  (`BTreeMap<(PathBuf, String), ProvidedSource>`, `loader.rs:144`).
- `drive_from_map` (`io.rs:108`) fulfills those needs from an in-memory map, `base_dir = ""`, purely
  LEXICAL normalization (`from_dir.join(raw)` then a lib-root fallback, `io.rs:171`). No `std::fs`.
- The wasm worker already builds that map from the wire pack (`src/geomsvc.rs:358`), and the wire type
  `Source::Bytes.libs` is `Vec<(String, Vec<u8>)>` — BYTE-capable end to end. The asset callback
  (`geomsvc.rs:372`) already hands raw bytes to `read_import_bytes` (`src/import.rs:63`), which already
  decodes binary STL / 3MF / SVG.

So the eval layer is READY. The two walls are both upstream of it, in delivery:

1. **No project injection.** The web pack is a fixed server fetch — `lib_fetch::lib_closure(main)`
   (`gui/src/lib_fetch.rs:159`) GETs `libs.json` (a `HashMap<String,String>`, `:155`), caches it globally
   (`:133`), and BFS-scans `main`'s references against it. The user's OWN files have no way in.
2. **Text-only pack.** The pack is `{path: text}` and the closure emits `text.into_bytes()`
   (`lib_fetch.rs:116`) — binary can't survive JSON string encoding. `src/bin/pack_libs.rs`
   says so outright ("binary meshes would need a byte channel a text pack lacks"). So SVG (text)
   imports work on the web today; a binary STL, a 3MF, a PNG heightmap can't reach the byte-ready reader.

## The container: `.scadproj`

A zip. fab already owns the machinery — the `zip` crate is a stored-only, wasm-proven dep
(`Cargo.toml:124`, via the `mesh-io` feature that the wasm GUI compiles), fab writes OPC zips in-memory
today (`src/threemf_out.rs:148` `Cursor<Vec<u8>>`; `src/bambu.rs:237`) and reads them byte-first
(`src/threemf_in.rs:136`, `ZipArchive::new(Cursor::new(bytes))`). No new dependency — just a schema.

The schema, following the OPC / EPUB precedent fab already lives in (3MF is a zip; so is `.docx`, `.odt`):

- **Stored (no compression)**, matching fab's OPC convention (and dodging decompression bombs).
- **`mimetype` as the FIRST entry, uncompressed at offset 0** — the EPUB trick. Contains
  `application/x-openscad-project`. This makes the type BYTE-SNIFFABLE (read ~40 bytes, no unzip) and
  positively identifiable even when a browser insists the blob is `application/zip`.
- **`fab-project.json` at the root** — the manifest. Declares `entry` (the root `.scad` to render), plus
  `title` / `version` for publish. The entry-point is genuinely needed: a project can hold several
  top-level `.scad` and the app can't guess which one renders. Fallback for hand-zipped projects with no
  manifest: the single `.scad` that no other file `include`s/`use`s.
- **The rest is the project tree verbatim** — `.scad` files under their relative paths, assets under
  theirs.

Why NOT lean on the OS/browser MIME registry (chotchki's "second mime type"): a browser sniffs any `.zip`
as `application/zip` no matter what, and registering a new type with browsers is a losing fight. Instead
the type lives in TWO places for two consumers: the **extension** (`.scadproj`) is how hotchkiss-io routes
it (its ingest is extension-typed, no content-sniffing — `probe.rs:65`), and the **internal marker +
manifest** is how the fab-gui app positively IDs it and finds the entry-point. A distinct suffix is exactly
the disambiguator chotchki wanted — `.scadproj` never collides with a random `.zip`.

(`.scad.zip` is the human-obvious alternative — "it's plainly a zip of scad" — but its extension is bare
`.zip`, so the site would match on the full `.scad.zip` suffix. `.scadproj` is the cleaner single token.
Either works; the internal marker makes the app robust regardless.)

## The plumbing, concretely

- **Project VFS.** Unzip the `.scadproj` to an in-memory `{relative-path → bytes}` tree, merge it INTO the
  render pack before the BFS (`lib_fetch::closure`, `lib_fetch.rs:82`), keyed by relative path. The
  existing from-dir-first resolver picks up `include <hook.scad>` with zero resolver change. One key
  subtlety: `use`/`include` match by normalized RELATIVE PATH (`io.rs:171`) while `import()`/`surface()`
  match by BASENAME (`geomsvc.rs:376`) — so pack `.scad` neighbors under their project-relative keys and
  assets under theirs (both is safe).
- **Byte-clean the pack.** The wire and reader are already byte-clean; only the web PRODUCER re-encodes
  text (`lib_fetch.rs:116`). Carry the zip's real bytes through, and binary assets survive intact into
  `read_import_bytes` (`geomsvc.rs:380`).

## The asset win (folds W.3.24's residual)

Recon corrected an assumption: W.3.24's eval-level readers are DONE — `read_import_bytes` (`import.rs:63`)
already decodes binary STL / 3MF / SVG from `&[u8]`. The deferral that remains is purely TRANSPORT: the
text pack corrupts binary. So the `.scadproj` byte channel unblocks binary `import()`/`surface()` on the
web for FREE — no eval work. (PNG heightmaps and `.dat` are a separate matter: their READERS are still
loud-deferred at `import.rs:92`. The zip transports the bytes; wiring those two decoders is its own task.)

## hotchkiss-io side (small, no migration)

The site stores the zip OPAQUE — the app is the only thing that understands its innards. Per recon
(`hotchkiss-io/src/db/dao/media.rs`, `.../probe.rs`, `.../web/features/media.rs`):

- **One `MediaKind::OpenscadProject` variant** + its `as_str`/`parse` arms (`media.rs:10`). Kind/mime are
  TEXT columns — NO schema migration.
- **One extension branch** in `probe.rs:65` → the new kind + `application/x-openscad-project`.
- **One `render_embed_html` arm** (`media.rs:586`) that emits an "Open in the editor" link + a download
  button — because it's its own kind it never reaches the three.js 3D-viewer arm.
- **One `?format=project` token** (`media_select.rs:18`) + one `ext_for_mime` line for a nice download
  filename. Byte serving, CORP, and the COEP-isolated editor's cross-origin `fetch` are already free; the
  `/3d/editor` deep-link is consumed CLIENT-SIDE (`three_d.rs`), so "open a project" is a fab-gui concern,
  not a route change.

## Native parity + round-trip

Desktop already IS a project folder. `fab pack <dir> → project.scadproj` and `fab open <project.scadproj>`
give the CLI ↔ web the same portable unit. Publish a project = upload the `.scadproj` as the source
download (+ the rendered mesh variant + cover); "Open in fab-scad-web" loads the zip; Save / save-back
RE-ZIPS. The publish contract already co-uploads a `.scad` source variant — this swaps that single file for
the project archive when the source is multi-file.

## Security (non-negotiable — it's a web upload)

- **Zip-slip:** reject any entry whose normalized path escapes the root (`../../etc`). Sanitize on extract.
- **Zip-bomb:** cap total uncompressed size + entry count. Stored-only helps but cap anyway.
- **UTF-8 for `.scad`:** the include map is `String`-keyed (`geomsvc.rs:363` drops non-UTF-8 silently);
  a `.scad` that isn't UTF-8 should fail LOUD, not vanish.

## Decisions (resolved 2026-07-21, chotchki)

- **A `.scadproj` is a FOLDER, not an entry-point (Z.3).** Treat it exactly like a project folder is
  treated today: unzip into the web app's file list (the desktop already has `FileList`), let the user
  switch between and edit ANY file, and re-zip on save. NOT "edit the entry, includes read-only" — the
  whole project is live. The manifest's `entry` only names which file RENDERS; every file is editable.
- **Manifest is JSON** — `fab-project.json` at the root. Matches the browser's native format (and the app
  already parses JSON for `libs.json`). No TOML.
- **Extension `.scadproj`, and SAY it's a zip.** The distinct suffix is the disambiguator (no collision
  with a random `.zip`), but the UX LOUDLY tells people a `.scadproj` is just a zip they can rename and
  unzip — in the file-open filter, the docs, and a tooltip. No magic, no lock-in: it's their folder in a
  zip. (Revised in Phase TG: the DOCS still say it, the UI no longer does. The "rename to .zip to peek"
  tooltip read as trivia next to a Save that didn't say what it wrote, so user-facing text now describes
  what Save does, never the archive format.)

## Phase Z sequence

1. **Z.1 — the `.scadproj` container.** Schema + reader/writer in fab-scad (stored zip, `mimetype`
   first-entry, `fab-project.json` manifest, entry-point resolution + the single-`.scad` fallback, path
   sanitize). Pure + unit-tested. Native `fab pack` / `fab open`.
2. **Z.2 — project VFS into the render pack.** Merge project files (relative-path keyed) into the
   include/asset pack; byte-clean the web producer so binary assets survive. Unblocks project-local
   includes AND binary `import()`/`surface()` on native + web. (Subsumes the W.3.24 transport residual.)
3. **Z.3 — fab-gui web open/save.** Open a `.scadproj` (drag-drop / file-open / `?model=` fetch) → in-memory
   project in the file list (FOLDER treatment, like native `FileList`) → switch + edit any file; Save /
   publish re-zip. The open-file UX says plainly it's a zip.
4. **Z.4 — hotchkiss-io kind.** `MediaKind::OpenscadProject` + probe extension + embed arm + format token +
   `ext_for_mime` (their repo, no migration).
5. **Z.5 — publish round-trip + validate.** Publish a project zip, re-open it from the gallery; e2e;
   native + wasm + fmt/clippy/tests green; dogfood shower_holder end to end on the web.

## Phase SW — native renders from-sources too (supersedes the SHADOW)

**Verdict: the browser had the better architecture the whole time, and desktop was carrying a mirror it
didn't need.** Z.3.6 gave a loose `.scad` open a temp SHADOW: copy the folder's `.scad` into `tmp/loose/`,
point `base_dir` there, render `Source::Path` at the shadow entry, and re-write the edited file into the
shadow on every debounced keystroke. It solved the real problem (a live preview must not scribble in the
user's folder — the `.fab-preview-*.scad` litter) but paid for it with a SECOND COPY of the truth, and
every bug in that era was the same bug: the copy and the document disagreeing.

Backlog #6 was the loudest instance. `import("FamilyLogo.svg")` resolved against the shadow, which only
mirrored `.scad`, so the GUI ENOENT'd on a model the CLI rendered fine. The first fix mirrored the assets
too — which works until the ref reaches OUTSIDE the folder (`import("../FamilyLogo.svg")`, which
`models/wall_screen` actually used), and a folder-shaped mirror structurally cannot serve that.

The fix is to stop mirroring. `Source::Pack { files, entry, asset_dir }` (SW.2) hands the kernel the
document's live buffers as the hybrid loader's OVERLAY (SW.1: overlay first, then the fs, so BOSL2 and
scad-lib still come off disk), and roots `import()` at the entry's real directory. So:

- **`base_dir` for a loose open is the user's REAL folder.** Assets resolve where they live, `../` included.
- **The preview writes NOTHING, anywhere.** Not the real folder, not a temp. There is no second copy to go
  stale, which retires the whole disagreement class rather than fixing instances of it.
- **The document is the text truth.** Editor hydration reads the doc (`doc_into_editor`), not disk — a disk
  read would silently drop an unsaved edit to a non-active file.
- **Desktop and web are now the same shape**: doc → pack → kernel. `hybrid_pack` and `render_pack` differ
  only in that the browser must ship asset BYTES (no disk to lean on) while native leaves them on disk.

Two things the shadow was quietly doing that had to be replaced deliberately, not deleted:

- **Packaging.** A `.scadproj` (Save-As, publish upload) is a SELF-CONTAINED archive, and a loose document's
  assets are no longer inside it. `collect_assets` survives, retargeted: `loose_sibling_assets` sweeps the
  folder at serialization time and the bytes go into the ZIP, never into the live document (an earlier cut
  absorbed them and every folder STL showed up as a Project-tab row the user never added).
- **Publish liveness.** Publish rendered `Source::Path(scene.source)`, which under the shadow WAS the live
  text because the preview kept writing it. Post-SW that path is the last-SAVED file, so publish renders the
  pack like everything else and stages the baked live entry text as the upload — the mesh and the source
  can't disagree.

**Doctrine, stated once so it stops being re-derived:** the PREVIEW never writes. Explicit file operations
— Save, Add-files, Rename — do write the user's real folder, because that is what the user asked for; they
carry a no-clobber guard (`unique_name` only knows the document, which for a loose open is `.scad` only, so
it is blind to every real asset sitting next to it). DELETE stays view-only: `rm` is the one that can't be
undone.

The paste flow (W.3.33) keeps its scratch-file path render — a pasted buffer has no `base_dir` for a pack to
root at, and nothing to be stale against.

## Phase TG — the document is what you save

**Verdict: unsaved is a property of the DOCUMENT, not of the editor.** Through v1.4.1, Save, ⌘S and the
"unsaved" badge read `EditorBuf::dirty`, which `doc_into_editor` reloads from whichever file is being
viewed. Clicking a clean library file hid an unsaved edit to the entry, and Add, Delete, Set entry and
every cut-plan edit never set the flag at all. That is how a `.scadproj` with a freshly added file ended up
with a grey Save and a ⌘S that did nothing (chotchki, dogfooding 1.4.1). Three earlier fixes patched the
flag at call sites and the next file switch undid each one. Now one predicate answers "would Save write
something it hasn't?", and every surface reads it: Save, ⌘S, the Model badge, the row markers, the window
title, the header chip and the Open hold.

### What counts as unsaved

`sync_doc_state` (`gui/src/jobs.rs`) derives `DocState::dirty` every frame as an OR of three things:

- **`ProjectDoc::is_dirty()`:** any file with unsaved text, any unsaved binary asset or a structural change
  (a file added, deleted or renamed, or the entry moved) when the home PERSISTS structure. The mutators
  mark the document themselves (`add_file`, `import`, `remove_file`, `set_entry`, `rename_file`,
  `flush_active`), so a new gesture can't forget to.
- **`EditorBuf::dirty`:** the viewed file's stored flag (loaded on every switch) plus edits since. It still
  counts because an edit only reaches the document on a flush, but it's one INPUT now, never the gate.
- **The config fingerprint:** `config::config_fp` hashes exactly what `part_to_slicing` bakes into
  `fab:config` per part, plus the bed, and `DocState` compares it to a baseline. `Parts` has about six
  writers (cuts, connectors, orient, AutoPlace, Reset-to-auto, the bed) and no chokepoint, so config
  dirtiness is a COMPARISON, not a flag. Hashing the persisted projection means what Save drops (a
  disabled cut, an auto orientation) can't read as a change.

Viewing a different file touches none of the three.

The config baseline is re-taken in exactly three places:

1. **After a fresh parts build,** once the pending `fab:config` is applied (an open, or an entry change).
   Both sides come from the same live `f32`s, never a re-parse of the file, so a saved plan reopens clean.
2. **Per part, when an auto-plan lands on a part whose baseline is EMPTY** (`DocState::auto_planned`). No
   saved plan means a reopen re-derives the same one, so a model that auto-plans on open reads clean. A
   part that HAD a saved plan keeps its baseline, so Reset-to-auto over a hand-tuned plan reads unsaved
   (correctly: Save would write something different).
3. **On save success,** to what that save baked.

`ModelState::reset` drops the baseline along with the model it describes, so the frames between an open
and its render never compare the new document against the OLD model's plan.

A save that lands on a later frame (a Save As dialog, a hotchkiss.io upload) captures `rev`, a counter
every mutation bumps, and lands through `mark_saved(rev)`, which does nothing if `rev` moved. An edit typed
while the dialog was up stays unsaved instead of being counted as saved by bytes that never held it.

### Where Save goes

`save::plan` is the ONE table, and the Save hover and the Project card's Save rule are written from it, so
the text can't promise something the button doesn't do:

- **Desktop `.scadproj`:** rewrite the archive: every file and asset, the manifest title (which a re-zip used
  to drop) and the entry baked with the cut plan and printer.
- **Desktop loose folder:** write the entry (config baked) plus every other file and asset with unsaved
  changes back into the folder. Each write is independent and atomic, and only the files that landed
  clear, so a half-failed save keeps its failures marked.
- **Desktop with no file yet:** Save opens Save As (the macOS convention). A session started without a
  file now boots holding an owned, empty `untitled.scad`, so typed text lands in a real document (it used
  to `fs::write("")` and fail silently).
- **The web:** Save downloads the document, `.scad` for one file and `.scadproj` for more, and a download
  the browser accepts counts as saved. A hotchkiss.io item downloads TOO (`save_route` folds the site
  plan into a download), because a keystroke shouldn't replace a published item's files. "Save to
  hotchkiss.io" on the Model tab is the deliberate site update, and a successful upload clears unsaved.

Every outcome lands in the status line: `saved <name> -> <folder>`, `save failed: <why> — still unsaved`
or `nothing to save — <name> is up to date` (⌘S on a clean document answers rather than doing nothing).
Save is enabled exactly when the document is unsaved and grey when clean, and the Project card's Save goes
gold while unsaved (chotchki, 2026-10-05).

Two of the rules are types, not conventions:

- **`DocSnapshot::capture` is the ONLY input the serializers take** (`rezip_project`,
  `project_source_variant`), and it always splices the live editor text over the active file. The web
  site save used to upload the active file WITHOUT its unflushed edit (the uploaded mesh and source then
  disagreed), because splicing was something each caller had to remember. Web Publish goes through it
  too.
- **`atomic_write`** writes a sibling temp file, syncs it, renames it over the target and keeps the
  original's permissions. On any failure the temp is removed and the old file is intact. A SIBLING, so the
  rename never crosses a filesystem.

After a container save, the temp unpack is re-materialized from the saved document, so `import()` reads
what was just saved.

### Save As (desktop only)

⇧⌘S (Ctrl+Shift+S) or the card's Save As…, on every desktop document; it replaced Z.3.7's gold "Save as
.scadproj…" promote. It writes a `.scad` for one text file with no assets and a `.scadproj` otherwise (a
loose folder's on-disk siblings count as assets, so a plain-file Save As can't strand an `import()`ed
neighbor). The dialog starts in the document's own folder (`ProjectDoc::home_dir`), or your home folder for
a document with no file, NEVER `scene.source`: for a `.scadproj` that's the temp unpack, and a file saved
there is gone on the next same-stem open. It suggests the document's name. The bytes and their `rev` are
captured when the dialog opens. On success the document MOVES to the new file (`save::rehome`), later
Saves write there, and the status says the original was left unchanged. The web has no Save As, so the
shifted chord there falls through to Save.

### Opening replaces, never merges

`file_ops::adopt` is the one way a loaded document becomes the open one: the Open dialog, the launch
argument, the web `?model=` fetch and the `--script` `open` verb all go through it. It hydrates the editor
from the entry, stamps the buffer with the document's `DocId`, stages the entry's `fab:config` for the
render to apply, and clears `scene.source` so the switch reads as a render-target change (old plan reset,
entry re-rendered). Then it posts `opened <name> (<folder>)` with the REAL folder, never the temp.

`DocId` exists because path equality isn't identity. `editor_holds` used to decide who owned the live
buffer by path alone, and two `.scadproj`s with the same stem unpack to the same temp path, so opening
`b/brace.scadproj` after `a/brace.scadproj` wrote a's editor text into b (re-opening the same file did the
same). Now the id AND the path have to match, which makes the alias unrepresentable for any future loader,
not just the current ones (the M.5.4.5 invariant-types rule). `sync_doc_state` debug-asserts it. The egui
code editor's own state (cursor plus an undo history of whole-text snapshots) is dropped whenever the
buffer's owner or path changes, or ⌘Z right after an open would put the previous document's text back.

While the document is unsaved, Open… is press-and-hold ("hold to discard changes and open…",
`hold_to_confirm`, 0.6s), the same gesture as a row Delete and no modal. A plain click does nothing.
Re-opening the same file is therefore a revert to the saved text. `fab-gui x.scadproj` opens the project
(it used to hang). Open is desktop-only: on the web the document is whatever `?model=` handed over, and
swapping it would orphan the save-back target.

### Loose folders: the SW doctrine, applied consistently

The SW rule above stands (the PREVIEW never writes, explicit file operations do), and TG applies it to
every file kind and says it in the UI (chotchki, 2026-10-05):

- **Add** copies EVERY added file into the folder at once, `.scad` included (it used to copy only the
  importable assets). A name the folder or the document already has is REFUSED with a status, never
  overwritten or quietly renamed, and the copy uses `create_new`, so a file that appears between the check
  and the write is refused by the OS rather than clobbered. The added file reads clean, because the folder
  already holds it.
- **Rename** stays immediate, with its on-disk clash refusal (`file_ops::rename_clash`).
- **Delete** stays session-only. Its hover reads "hold to remove from this session — the file stays
  in the folder", and a multi-file folder's Save rule says the same.
- **Set entry** is a session view change: a loose folder has no manifest to record it in. Save bakes the
  config into whichever file is the entry when it runs.

So `ProjectHome::persists_structure` is false for a loose home and none of those four marks it unsaved.
Text edits (New file included: it's unsaved text until Save writes it) and the cut plan are the ONLY
loose changes that wait for Save. "Nothing touches disk until
Save" was the cleaner story and got rejected: an added asset would have to render from an in-memory
overlay until Save, and Save would have to delete files out of real folders.

### What the GUI says

The text comes from pure functions in `gui/src/doc_view.rs` (table-tested, plus a glyph audit for tofu),
and `panel_ui` only lays it out:

- **The Project tab** leads with the document card: name, place and kind (`~/models · .scadproj`,
  `~/models/brace · loose folder`, `not saved yet`, `on hotchkiss.io`), the Save rule, then Save, Save As…
  and Open….
- **The desktop window title:** `bracket.scadproj (unsaved) — fab-scad`, written only when it changes. On
  the web the host page owns `<title>`.
- **The header chip:** the document's name and unsaved marker on every tab, the full path on hover, and a
  click goes to the Project tab.
- **The Model tab:** a labelled Save (hover from `plan`) and the breadcrumb
  `hook.scad · in bracket.scadproj · main.scad renders`.
- **Markers:** unsaved is a painter-drawn FILLED circle, and stale stays `icons::DOT`, which the FILL=0 font
  subset renders as a RING. Two shapes, no font regeneration.

### Known limits (filed in `openspec/backlog.md`)

- **Stale still over-fires.** Viewing a non-entry file lights every tab's stale ring, because
  `sync_pipeline` hashes the VIEWED file against the entry's render. TG only made the two markers look
  different.
- **Nothing guards a close.** Closing the window (or ⌘Q, which winit 0.30 can't intercept) with unsaved
  changes doesn't ask, there's no recovery snapshot and the web has no `beforeunload`. Save before you
  quit.
- **Native Rename and container Delete still `state.reset()`** after the file op and rebuild the model.
  The rebuild re-applies the entry's SAVED plan (`ProjectDoc::entry_config`, the same block every fresh
  build stashes), so the document still reads right, but an UNSAVED plan edit made before the rename or
  delete is gone. Before the spec-coverage pass the rebuild stashed nothing: an auto-plan replaced the
  saved plan and read clean, and the next Save wrote it over the real one.
- **Temp paths leak.** Export, the Publish title default and the self-update relaunch still see the temp
  unpack path instead of the real one.
