# Web save-back — the fab-scad side of the round-trip (W.5)

The counterpart to the shipped `?model=` LOAD (W.3.12): the wasm app writes an edited model back to
hotchkiss.io. The USER-facing contract lives in the site repo (`hotchkiss-io/docs/media-design.md` —
§5 the resource, §4 auth, §10 the 3D round-trip); this doc is the CODE-facing half: the exact wire
fab-gui builds against, pinned to that contract.

## The split — who owns what

- **fab-scad (here):** produce the three variants, PUT them, gate the Save affordance. W.5.2–.8.
- **hotchkiss-io (chotchki's repo, SHIPPED — Phases DN/DP/DQ):** the load deep-link, the write
  endpoint, the auth. All of it is live: the site's media subsystem was rationalized onto a uniform
  HATEOAS resource, and the round-trip is a first-class part of it (nothing model-special). **No
  cross-repo blocker remains** — the earlier gap (the deep-link not carrying the ref) is closed.

## One `?model=` drives both load and save

The site's embed "Open in the slicer" button links **`?model=/media/<ref>?format=scad`** — the stable
`media_ref` rides the model URL's PATH.

- **Load:** the app `fetch_text`s the `?model=` value; the negotiated `GET /media/<ref>?format=scad`
  307-redirects to the SCAD source (W.3.12, unchanged).
- **Save target:** derived from the SAME value by **dropping the query and appending `/variants`** →
  **`PUT /media/<ref>/variants`**. This is §10's blessed derivation ("the SAVE target is derivable by
  dropping the query … or via the OPTIONS manifest"). `gui/src/save_target.rs::derive` does it, pure +
  native-tested; the wasm boot reads `?model=` and stores the result in the `SaveTarget` resource.

Why derive instead of a separate `?ref=` param: the site OWNS its URLs and hands us the item's path;
we follow it (HATEOAS) rather than reconstruct one from a memorized vocabulary. A path-prefixed deploy
(`/app/media/<ref>`) or an absolute same-origin URL survives the derivation untouched — no
`data-media-base` config needed. A `?model=` that ISN'T a single item — a generic external `.scad`
(the plain W.3.12 load), a `/media/file/<url_key>` byte URL, or an already-a-collection URL — yields
`None` ⇒ **no Save button**, so the app never dangles a write against a non-item URL.

## What the app sends

Three files, `multipart/form-data`, one **`PUT /media/<ref>/variants`** — the ref is in the URL, NOT
the body:

| part | how the server keys it | notes |
|---|---|---|
| source SCAD | **filename ending `.scad`** | the editor buffer's current text (config baked in, W.3.8) |
| low-res mesh | **filename ending `.stl` or `.3mf`** | decimated; same format as high (W.5.6) |
| full-res mesh | **filename ending `.stl` or `.3mf`** | whole solid; colored ⇒ 3MF (W.5.4/.5) |

The critical, non-obvious server behavior (media-design.md §5/§7):

- **Files are classified by FILENAME EXTENSION, not by part name or Content-Type.** The shared
  `ingest_multipart` streams any part carrying a `file_name()` and types it by extension
  (`.scad`→`application/x-openscad`, `.stl`→`model/stl`, `.3mf`→`model/3mf`). So give each file part
  the right extension, and **do NOT set a per-part `Content-Type`** — web-sys `FormData` omitting it is
  exactly right (it also lets the browser own the multipart boundary, which a manual header breaks).
- **PUT `…/variants` is a COMPLETE replacement of the variant COLLECTION** — the uploaded set becomes
  the item's whole variant set (wiped + re-inserted in one transaction). So send ALL THREE variants
  every save; a partial drops the omitted ones. The item's identity (`media_ref`, `title`, `min_role`
  gate) lives on the PARENT `/media/<ref>` and is untouched — which is exactly why the collection
  sub-resource beats DO's old PATCH-that-remembers-to-preserve-metadata.

## Auth — ambient cookie, no token

- Session cookie **`id`** (tower-sessions default name), `HttpOnly`, `SameSite=Lax`, signed, `Secure`
  in release. The editor is **same-origin**, and Lax sends the cookie on a same-site PUT, so a
  logged-in session's save is authed automatically.
- **No CSRF token anywhere** in the server — the write-protection is the site-wide fail-closed
  mutation gate (`require_admin_for_mutations`, §4a): every non-safe method (PUT here) needs an
  authenticated **Admin**. So Save works only for an admin session (chotchki's), which is the intent.
  The app surfaces a 401 as "log in as admin", a 403 as "not an admin".
- fab-gui sends `credentials: 'same-origin'` on the fetch — the default already carries the cookie;
  COOP/COEP isolation on the editor doc does NOT strip a same-origin cookie.
- **Alt path (non-browser):** an `Authorization: Bearer hio_…` API key resolves to an Admin with no
  cookie (how `fab publish` authenticates) — available if the app ever writes from outside a logged-in
  browser.

## The endpoint — `PUT /media/<ref>/variants` (SHIPPED, Phase DQ.1)

Re-verbed from DO's `PATCH /media/<ref>` as part of the site's HATEOAS rationalization: writes are
idempotent PUTs (replace) except the two server-assigns-identity creates (POST); **there is no PATCH.**
`…/variants` is the item's variant COLLECTION, and replace-all is a PUT on it. Same-origin + relative,
so the ambient session cookie rides. The fail-closed mutation layer gates it to Admin automatically —
no bespoke guard, no CSRF token.

- **Body:** `multipart/form-data`, same shape as create — one file part per file, typed by filename
  EXTENSION. Non-file fields ignored. No manual `Content-Type`.
- **Response:** `200` + the item **manifest** JSON `{ref, kind, title, min_role, variants:[{type, bytes,
  href, …}], controls?}` — the final set, confirming the swap. `404` unknown ref (deleted since load),
  `400` empty body (a replace-to-nothing is a DELETE; the app always sends three, so never hits it).
- **`url_key` churn:** replacing bytes mints new per-variant `HMAC(sha)` `url_key`s, so a pasted
  `/media/file/<url_key>` link goes stale — but every `![](/media/<ref>)` embed resolves live across
  the save (the whole point of the stable ref). Embed by ref, never by `url_key`.

The pure-HATEOAS alternative to the drop-the-query derivation is `OPTIONS /media/<ref>` →
`controls.replace-all.href` (§5's manifest). Both are sanctioned by §10; the derivation avoids the
extra round-trip and the Admin-gated controls block, so that's what the app does.

## Status: the client half is DONE and matches the shipped contract

W.5.1–.8 are complete and verified against `media-design.md`. The in-process wire e2e
(`save_back_pipeline_through_the_wire`) exercises the whole render→decimate→emit→envelope path; the
URL derivation is unit-tested (`save_target::tests`). The only thing not yet exercised end-to-end is
the **browser leg** (W.5.9): a real headless-Chrome load-`?model=` → click Save → PUT-with-cookie,
which folds into W.6.2's headless-Chrome refresh (it needs the parallel worker + a DOM-automation
driver the console-grep boot gate lacks). Nothing blocks it cross-repo anymore — it's purely a
test-harness gap on this side.
