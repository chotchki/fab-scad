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

Three files, `multipart/form-data`, one **`PATCH /media/<ref>`** — the ref is in the URL, NOT the body:

| part | how the server keys it | notes |
|---|---|---|
| source SCAD | **filename ending `.scad`** | the editor buffer's current text |
| low-res mesh | **filename ending `.stl` or `.3mf`** | decimated; same format as high (W.5.6) |
| full-res mesh | **filename ending `.stl` or `.3mf`** | whole solid; colored ⇒ 3MF (W.5.4/.5) |

The critical, non-obvious server behavior (frozen Phase-DO contract):

- **Files are classified by FILENAME EXTENSION, not by part name or Content-Type.** The handler
  streams any part that carries a `file_name()` and types it by extension (`.scad`→
  `application/x-openscad`, `.stl`→`model/stl`, `.3mf`→`model/3mf`). So: give each file part the right
  extension, and **do NOT set a per-part `Content-Type`** — web-sys `FormData` omitting it is exactly
  right (it also lets the browser own the multipart boundary, which a manual header would break).
- **Non-file fields are IGNORED** — the `media_ref` rides the URL path, not a form field. And PATCH is a
  COMPLETE replacement, so send ALL THREE variants every save (a partial would drop the omitted ones).

## Auth — ambient cookie, no token

- Session cookie **`id`** (tower-sessions default name), `HttpOnly`, `SameSite=Lax`, signed, `Secure`
  in release. The editor is **same-origin**, and Lax sends the cookie on a same-site PATCH, so a
  logged-in session's save is authed automatically.
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

## The endpoint — FROZEN (Phase DO)

**`PATCH /media/<ref>`** — the ref IS the resource, in the URL PATH (not a form field). Same-origin +
relative, so the ambient session cookie rides. The app builds it via `web_host::media_patch_url(ref)` =
`{data-media-base or "/media"}/{ref}`; the `data-media-base` attr only matters for a path-prefixed
deploy. The fail-closed mutation layer gates any non-GET to Admin automatically — no bespoke guard, no
CSRF token.

- **Body:** `multipart/form-data`, same shape as the mint upload — one file part per file, typed by
  filename EXTENSION (`.scad`→`application/x-openscad`, `.3mf`→`model/3mf`, `.stl`→`model/stl`). Non-file
  fields are ignored (so the `media_ref` is NOT in the body). No manual `Content-Type`.
- **Semantics — COMPLETE replacement:** the uploaded set BECOMES the item's entire variant set (wiped +
  re-inserted in one transaction). Identity — `media_ref`, `title`, `min_role` gate — is preserved, so
  every `![](/media/<ref>)` embed survives; anything not re-uploaded is DROPPED. So the app MUST send all
  three variants every save (source + low + high), never a partial.
- **Response:** `200` JSON `{media_ref, kind, variants:[{url_key, mime, bytes}...]}` — the final set, to
  confirm the swap. `404` unknown ref (deleted since load), `400` empty body.
- **`url_key` churn:** replacing bytes changes the per-variant `HMAC(sha)` `url_key`, so a pasted
  `/media/file/<url_key>` link goes stale — but `![](/media/<ref>)` embeds resolve live. Embed by ref.

## The ONE remaining blocker (chotchki, site-side)

Recon of the shipped site code (Phase DO) confirmed the PATCH endpoint above is real and matches this
contract **exactly** — but found the load side still open, and the site's own doc contradicts its code:

- **The editor deep-link carries NO ref.** `open_in_slicer_button` (`hotchkiss-io/src/web/features/media.rs:549`)
  emits only `/3d/editor?model=/media/file/<scad_url_key>`. That `url_key` is an `HMAC(sha)` of the SCAD
  variant's bytes — NOT the `media_ref` — and it CHANGES on every save, with no reverse `url_key→ref`
  map. So the app currently has no way to learn the `media_ref` at boot, and the round-trip can't close.
- **The fix (site):** add the ref to that link — `?model=…&ref=<media_ref>` — where `<media_ref>` is the
  raw 32-char hex UUIDv7 (`media.rs:296`, `Uuid::now_v7().simple()`). The app reads it verbatim via
  `web_host::query_param("ref")` (W.5.7) and PATCHes to `/media/<ref>` unchanged — no encoding, no
  normalization (the route matches the raw string, `media.rs:374`).
- Until then the app is READY and inert: no `?ref=` ⇒ no Save button (`MediaRef` is `None`), so it never
  dangles against the live endpoint. The whole client half (W.5.1–.8) is done and verified against the
  frozen contract; only this one site-side link change gates the live round-trip.
