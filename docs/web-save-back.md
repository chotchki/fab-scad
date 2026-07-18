# Web save-back — the fab-scad side of the round-trip (W.5)

The counterpart to the shipped `?model=` LOAD (W.3.12): the wasm app POSTs an edited model back to
hotchkiss.io. The USER-facing contract lives in the site repo (`hotchkiss-io/docs/fab-scad-roundtrip.md`)
— this doc is the CODE-facing half: the exact wire fab-gui builds against, pinned against the real
server, with the cross-repo gaps called out so nobody re-discovers them.

## The split — who owns what

- **fab-scad (here):** produce the three variants, POST them, gate the Save affordance. W.5.2–.8.
- **hotchkiss-io (chotchki's cross-repo track):** the update-in-place endpoint, the `?ref=` param on
  the editor deep-link, the replace-vs-version call. As of the W.5.1 recon **none of these exist yet**
  — the site shipped only the LOAD half (Phase DN). So the app is built against the PROPOSED contract
  with a configurable endpoint, and every Save affordance is gated on `?ref=` being present (absent ⇒
  no button, so the app never dangles against a route that isn't there).

## What the app sends

Three files + one text field, `multipart/form-data`, one POST:

| part | how the server keys it | notes |
|---|---|---|
| source SCAD | **filename ending `.scad`** | the editor buffer's current text |
| low-res mesh | **filename ending `.stl` or `.3mf`** | decimated; same format as high (W.5.6) |
| full-res mesh | **filename ending `.stl` or `.3mf`** | whole solid; colored ⇒ 3MF (W.5.4/.5) |
| `media_ref` | a text part **named `media_ref`** | the item to update in place |

The critical, non-obvious server behavior (recon, `hotchkiss-io/src/web/features/admin/media.rs`):

- **Files are classified by FILENAME EXTENSION, not by part name or Content-Type.** The handler
  streams any part that carries a `file_name()` and re-probes the stored bytes; MIME comes from the
  extension (`src/media/probe.rs`: `.scad`→`application/x-openscad`, `.stl`→`model/stl`,
  `.3mf`→`model/3mf`). So: give each file part the right extension, and **do NOT set a per-part
  `Content-Type`** — web-sys `FormData` omitting it is exactly right (it also lets the browser own the
  multipart boundary, which a manual header would break).
- **Text fields ARE matched by name** — the literal `"media_ref"` is read (today only as a
  title-fallback on the mint path; the update path Half-2 adds will key on it).

## Auth — ambient cookie, no token

- Session cookie **`id`** (tower-sessions default name), `HttpOnly`, `SameSite=Lax`, signed, `Secure`
  in release. The editor is **same-origin**, and Lax sends the cookie on same-site POST, so a
  logged-in session's upload is authed automatically.
- **No CSRF token anywhere** in the server — the write-protection is `SameSite=Lax` +
  `require_admin_for_mutations` (fail-closed: every non-GET needs an authenticated **Admin**). So Save
  works only for an admin session (chotchki's), which is the intent — but the app should surface a
  401/403 as "log in as admin on the site", not a generic failure.
- fab-gui sends `credentials: 'same-origin'` (or `'include'`) on the fetch — the default already
  carries the cookie; COOP/COEP isolation on the editor doc does NOT strip a same-origin cookie.
- **Alt path (non-browser):** an `Authorization: Bearer hio_…` API key resolves to an Admin session
  with no cookie (how `fab publish` authenticates) — available if the app ever POSTs from outside a
  logged-in browser.

## The URL params

- `?model=/media/file/<url_key>` — SHIPPED. The SCAD bytes URL; the app `fetch_text`s it. `url_key`
  is a per-variant HMAC token, **NOT** the `media_ref`.
- `?ref=<media_ref>` — **does not exist yet** (site Half-2). The stable UUIDv7 item token the upload
  targets. The app reads it at boot (W.5.7); **absent ⇒ no Save affordance.** (Alternative the site
  may choose instead: resolve the ref server-side from the `url_key` — then the app wouldn't need
  `?ref=` at all. Either way the app degrades cleanly when it can't see a ref.)

## The endpoint

**Undecided — chotchki's site-side call.** Recon found no update-in-place route; the closest existing
shapes to pattern-match are `POST /admin/media/upload` (mints a NEW item, returns JSON
`{media_id, media_ref, markdown}`) and `POST /admin/media/{id}/encode` (adds a variant by numeric id,
insert-only). Half-2 wires "replace the same-format variant on an existing `media_ref`". The app
codes the endpoint as a **configurable constant** (a `data-*` attr on the host page, or a build
default) so it tracks whatever the site settles on without a rebuild. A successful update should
mirror `upload_media`'s JSON so the app has a machine-readable result.

## Storage model (why "update in place" is coherent)

One `media` row (stable `media_ref`, `media_id`) → N `media_variant` rows (content-addressed on disk
by `sha256`, DB holds metadata only). The three variants are three rows under one `media_id`,
distinguished at read time by MIME + size (viewer = smallest `model/3mf`, download = largest mesh).
"Update in place" = keep the `media` row, swap the same-format variant rows — so every
`![](/media/<ref>)` embed stays valid with zero rewrite. That swap path is the piece Half-2 builds
(today's `add_encode` only INSERTs; `delete_variant` removes one by id).

## Open cross-repo items (chotchki)

- The update-in-place endpoint (path/method + replace-vs-accumulate semantics).
- `?ref=` on the editor deep-link (or server-side ref resolution from `url_key`).
- Whether a re-upload REPLACES the same-format variant or accumulates edit history.
