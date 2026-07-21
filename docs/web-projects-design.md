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
   (`lib_fetch.rs:116`) — binary can't survive JSON string encoding. `packaging/web/pack_scad_libs.py`
   says so outright ("binary meshes would need a byte channel a text pack lacks (deferred)"). So SVG (text)
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
  zip.

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
