# Publish to hotchkiss.io

The last mile: take a finished model and put it on the site — a `/projects` page with a rendered
cover, an interactive preview, and downloadable files — from one command (CI) or one button (GUI).

```
HIO_API_KEY=hio_… fab publish Underdesk.scad
```

## What it uploads

- **Cover thumbnail** (`.png`) — OpenSCAD's auto-framed render (`--viewall`, Cornfield).
- **Preview mesh** (`.stl`) — a LOW-`$fn` render for the in-browser 3D viewer. Forced by a
  `$preview = true` include wrapper, so the source's `$fn = $preview ? low : high` takes the light
  path — a mesh a browser can spin without choking. (A model with no curves, or that doesn't follow
  the convention, just gets the same mesh as full-res — fine, it falls back.)
- **Downloads** — the full-res `.stl`, plus `<stem>-plates.3mf` if `fab make` left a printable plate
  next to the model.

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
- **Media**: `POST /admin/media/upload` (multipart `file` + `title`) → JSON `{media_ref}`. Content-
  addressed, so re-uploading identical bytes dedups server-side.
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
