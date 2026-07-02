# Publish to hotchkiss.io

The last mile: take a finished model and put it on the site — a `/projects` page with a rendered
cover, an interactive preview, and downloadable files — from one command (CI) or one button (GUI).

```
HIO_API_KEY=hio_… fab publish Underdesk.scad
```

## What it uploads

- **Cover** (`.png`) — OpenSCAD's auto-framed render; the page cover (its own media item).
- **Model mesh** — the low-`$fn` COLORED 3MF (viewer) + the full-res COLORED 3MF (download), uploaded
  as ONE media item with **LOD variants**. The site groups a multi-file upload into one item (a
  variant per file), so the viewer renders the light variant and the full one is its download —
  uploading them separately would make two unrelated items. OpenSCAD's 3MF export carries the model's
  `color()` as base materials (STL is colorless — that's why the mesh is 3MF); low-`$fn` (via the
  `$preview = true` include wrapper) keeps the viewer variant light — a bowtie's full mesh went 23 MB
  STL → 1.5 MB 3MF. (A model with no curves just gets the same mesh both ways — fine, it falls back.)
- **Print plates** — `<stem>-plates.3mf` if `fab make` left one beside the model, as its own download.

## Setup

- **API key** — mint one in the site admin (`/admin/api-keys`); it's an `hio_…` token shown once.
  Pass it as `--api-key` or `$HIO_API_KEY`. The key delegates its user's role, so an admin key can
  create pages headless.
- **URL** — `--url` or `$HIO_URL`; defaults to `https://hotchkiss.io`. Point it at
  `http://localhost:<port>` for a local dev server.
- **Metadata** — from the project's `project.toml`:
  ```toml
  [project]
  name = "underdesk-mount"      # the URL slug root
  title = "Underdesk Mount"     # the page title (→ slug via the server's slugify)

  [publish]
  description = "A bracket that clamps under a desk. Printed in two pieces, onion-aligned."
  ```
  The `[publish] description` is the markdown body; the preview + downloads are appended below it.

## GUI

The **Publish** button (view panel) does the same off-thread — reads `$HIO_API_KEY` / `$HIO_URL`,
resolves the manifest, renders + uploads, and reports the page URL in the status line. Same
`publish_model` path as the CLI, so CLI and GUI land identical pages.

## How it talks to the site (contract)

The site has no bespoke publish API — `fab publish` drives the *existing* admin + media endpoints:

- **Auth**: `Authorization: Bearer hio_…`.
- **Media**: `POST /admin/media/upload` (multipart: one or more `file` parts + `title`) → JSON
  `{media_ref}`. Multiple `file` parts in ONE request = one item with a variant each (this is how LOD
  works — batch them, don't upload separately). Content-addressed, so identical bytes dedup. Served
  at `/media/file/<ref>` (what the markdown embeds/links point at).
- **Page**: create `POST /pages/projects` (form `page_title`, server slugifies), then write
  `PUT /pages/projects/{slug}` (markdown body + `page_cover_media_ref`).
- **Idempotent by slug**: there's no upsert route, so the client mirrors the server's `slugify`
  byte-for-byte, `GET`s to check existence, `POST`s only if new, then `PUT`s the body. Re-publishing
  the same title UPDATES the page.
- **Retry**: exponential backoff on transient failure (connection error, timeout, 5xx) — the
  "overload retry". Fatal 4xx (bad request / auth) fails fast.

## Honest limits

- **Slug drift** would split pages — the client's `slugify` is a verbatim mirror of the server's
  (`web/util/slug.rs`), locked by a test using the server's own cases. If the server's ever changes,
  update the mirror.
- **Errors are HTML, not JSON** — a failed upload/page-write surfaces the HTTP status; the styled
  error page body isn't parsed. Auth failures read as `403`.
- **One cover, downloads by link** — the cover is the page cover; other files are markdown download
  links, not a structured gallery. A richer project content-type (a 3D viewer widget, print-settings
  fields) is a future server-side upgrade.
