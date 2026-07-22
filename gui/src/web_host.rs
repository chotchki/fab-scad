//! Browser host-surface helpers (W.3.12/W.3.13) — the file-IO half of the `docs/web-embed.md`
//! contract, which promised "Blob download + `<input type=file>`" back at W.3.7 and never wired the
//! download. Everything here is wasm-only plumbing over `web-sys`; failures are LOGGED and swallowed
//! (a download that doesn't start is a UX bug, not a reason to panic the app).

use wasm_bindgen::JsCast;

/// Trigger a browser download of `bytes` as `filename` (W.3.13): wrap the bytes in a `Blob`, mint an
/// object URL, click a detached anchor, revoke the URL. The anchor is never attached to the DOM —
/// `click()` works detached in every engine the app targets, and detachment keeps the page clean.
pub(crate) fn download_bytes(filename: &str, mime: &str, bytes: &[u8]) -> bool {
    let go = || -> Option<()> {
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::of1(&array.buffer());
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type(mime);
        let blob =
            web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        let document = web_sys::window()?.document()?;
        let anchor: web_sys::HtmlAnchorElement =
            document.create_element("a").ok()?.dyn_into().ok()?;
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.click();
        let _ = web_sys::Url::revoke_object_url(&url);
        Some(())
    };
    let ok = go().is_some();
    if !ok {
        bevy::log::error!("browser download of {filename} failed to start");
    }
    ok
}

/// Open the browser's file picker and send every chosen file's `(name, bytes)` down `tx` (Z.3.10) — the
/// wasm twin of the desktop rfd multi-picker, feeding the same `ProjectDoc::import`. A cancelled pick
/// fires no `change` event, so nothing is ever sent and the caller simply keeps waiting; there is no
/// in-flight guard because a second picker is harmless.
///
/// The `<input>` is never attached to the DOM, exactly like the download anchor above — one less node to
/// clean up on the cancel path, which has no event to hang the cleanup off. Bytes come from
/// `Blob::array_buffer` (a `File` IS a `Blob`), so no `FileReader` and no extra web-sys feature.
pub(crate) fn pick_files(tx: async_channel::Sender<Vec<(String, Vec<u8>)>>) {
    let go = || -> Option<()> {
        let document = web_sys::window()?.document()?;
        let input: web_sys::HtmlInputElement =
            document.create_element("input").ok()?.dyn_into().ok()?;
        input.set_type("file");
        input.set_multiple(true);
        let el = input.clone();
        // `once_into_js` — the picker fires `change` at most once, and the closure frees itself after.
        let handler = wasm_bindgen::closure::Closure::once_into_js(move |_: web_sys::Event| {
            let files = el.files();
            wasm_bindgen_futures::spawn_local(async move {
                let mut out = Vec::new();
                for i in 0..files.as_ref().map_or(0, |l| l.length()) {
                    let Some(file) = files.as_ref().and_then(|l| l.get(i)) else {
                        continue;
                    };
                    let name = file.name();
                    match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                        Ok(buf) => out.push((name, js_sys::Uint8Array::new(&buf).to_vec())),
                        // One unreadable file doesn't sink the others — report it and carry on.
                        Err(_) => bevy::log::error!("could not read {name} from the file picker"),
                    }
                }
                // Unbounded, so this can only fail if the app is tearing down — nothing to do then.
                let _ = tx.try_send(out);
            });
        });
        input
            .add_event_listener_with_callback("change", handler.unchecked_ref())
            .ok()?;
        input.click();
        Some(())
    };
    if go().is_none() {
        bevy::log::error!("could not open the browser file picker");
    }
}

/// The page URL's `?name=` query value, percent-decoded (W.3.12) — how a host page hands the app a
/// model reference (`?model=<url>`). `None` when absent or the URL machinery is unavailable.
pub(crate) fn query_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get(name)
}

/// PUT the save-back variants to `url` as `multipart/form-data` (W.5.8, shipped `PUT /media/<ref>/variants`
/// contract — media-design.md §5/§10, re-verbed from DO's PATCH). Each file rides as a `Blob` with a
/// FILENAME — the site types each by its filename EXTENSION, not the part name or declared Content-Type,
/// so the extension is what matters (the MIME is a courtesy); non-file fields are ignored, so the
/// `media_ref` lives in the URL PATH, not the body. PUT = COMPLETE variant-collection replacement in one
/// transaction (item identity — ref/title/min_role — preserved on the parent). Delegates to
/// [`fetch_multipart`] (shared with the W.3.29 publish path) and keeps the save-back's own error wording.
pub(crate) async fn upload_multipart(
    url: &str,
    files: &[(&str, &str, &str, &[u8])], // (field name, filename, mime, bytes)
) -> Result<String, String> {
    let (status, body) = fetch_multipart("PUT", url, &[], files).await?;
    if !(200..300).contains(&status) {
        // 401/403 = not a logged-in admin; 404 = the ref no longer exists (deleted since load).
        return Err(match status {
            401 => "save rejected (401) — log in as admin on hotchkiss.io first".to_string(),
            403 => "save rejected (403) — this session isn't an admin".to_string(),
            404 => "save rejected (404) — this model no longer exists on the site".to_string(),
            s => format!("save rejected: HTTP {s}"),
        });
    }
    Ok(body)
}

/// POST/PUT a `multipart/form-data` body (text fields + files) to `url`, returning `(status, body)` so the
/// caller decides what a given code means. Files ride as `Blob`s with a FILENAME (the site types each by
/// its extension); text fields are plain parts. Credentials are `same-origin` so the ambient session
/// cookie (`id`, SameSite=Lax) authenticates the admin — no token, no CSRF header (none exists
/// server-side). NO manual `Content-Type`: the browser owns the multipart boundary. The generalized
/// transport behind both the save-back (`PUT /media/<ref>/variants`) and the publish `POST /media`.
pub(crate) async fn fetch_multipart(
    method: &str,
    url: &str,
    text_fields: &[(&str, &str)],
    files: &[(&str, &str, &str, &[u8])], // (field name, filename, mime, bytes)
) -> Result<(u16, String), String> {
    let build = || -> Option<web_sys::FormData> {
        let form = web_sys::FormData::new().ok()?;
        for (field, value) in text_fields {
            form.append_with_str(field, value).ok()?;
        }
        for (field, filename, mime, bytes) in files {
            let array = js_sys::Uint8Array::from(*bytes);
            let parts = js_sys::Array::of1(&array.buffer());
            let opts = web_sys::BlobPropertyBag::new();
            opts.set_type(mime);
            let blob =
                web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts).ok()?;
            form.append_with_blob_and_filename(field, &blob, filename)
                .ok()?;
        }
        Some(form)
    };
    let form = build().ok_or_else(|| "could not assemble the upload form".to_string())?;

    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_body(&form);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    send(url, &init).await
}

/// POST/PUT an `application/x-www-form-urlencoded` body to `url`, returning `(status, body)`. The page
/// write endpoints (`POST /pages/3d`, `PUT /pages/3d/<slug>`) take form fields, not files — this mirrors
/// the native reqwest client's `.form(&[…])`. `UrlSearchParams` as the fetch body makes the browser set
/// the `application/x-www-form-urlencoded` content-type itself. Same-origin cookie auth, as above.
pub(crate) async fn fetch_form(
    method: &str,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(u16, String), String> {
    let params =
        web_sys::UrlSearchParams::new().map_err(|_| "could not build form body".to_string())?;
    for (k, v) in fields {
        params.append(k, v);
    }
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_body(&params);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    send(url, &init).await
}

/// PUT/POST a JSON body to `url`, returning `(status, body)`. The item-metadata route
/// (`PUT /media/<ref>`, media-design.md §5 `controls.metadata`) takes JSON, not a form — unlike the
/// variant routes — so it needs an explicit `Content-Type`: axum's `Json` extractor 415s without one.
/// That's the only reason this can't reuse [`fetch_form`]. Same-origin cookie auth, as above; the route
/// is Admin-gated by the site's `require_admin_for_mutations` (any non-safe method), so a logged-out
/// caller gets a 401/403 rather than a silent no-op.
pub(crate) async fn fetch_json(
    method: &str,
    url: &str,
    body: &str,
) -> Result<(u16, String), String> {
    let headers =
        web_sys::Headers::new().map_err(|_| "could not build request headers".to_string())?;
    headers
        .set("content-type", "application/json")
        .map_err(|_| "could not set the JSON content-type".to_string())?;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_body(&wasm_bindgen::JsValue::from_str(body));
    init.set_headers(&headers);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    send(url, &init).await
}

/// GET `url`, returning just the HTTP status code — the page-exists check before create-or-update. Public
/// (no admin needed) but sent same-origin so any auth-gated variant still resolves.
pub(crate) async fn fetch_status(url: &str) -> Result<u16, String> {
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    Ok(send(url, &init).await?.0)
}

/// Fire a prepared request and read `(status, body)`. The shared tail of the fetch helpers above: run the
/// promise, coerce the `Response`, read its status, then drain the text body (empty string if none).
async fn send(url: &str, init: &web_sys::RequestInit) -> Result<(u16, String), String> {
    use wasm_bindgen_futures::JsFuture;
    let win = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let resp = JsFuture::from(win.fetch_with_str_and_init(url, init))
        .await
        .map_err(|_| "request failed (network / offline?)".to_string())?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a Response".to_string())?;
    let status = resp.status();
    let body = match resp.text() {
        Ok(promise) => JsFuture::from(promise)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    Ok((status, body))
}

/// A fetched `?model=` body plus the response's raw `Content-Disposition` value (Z.3.9) — the model's
/// bytes and the name the serving host gave them.
pub(crate) type FetchedModel = (Vec<u8>, Option<String>);

/// Fetch `url` as raw BYTES (Z.3.4) — the `?model=` body. Relative URLs resolve against the PAGE (not the
/// bundle base), so a host page can point at its own assets; same-origin always works under the bundle's
/// COOP/COEP, cross-origin needs CORS + CORP on the model host (docs/web-embed.md). Bytes (not text) so a
/// `.scadproj` (a zip) survives — a `.scad` round-trips fine (its bytes decode back to the same text).
/// `None` on any failure — the caller reports and falls back to the demo.
///
/// Also returns the response's `Content-Disposition` value (Z.3.9), which is where the model's REAL name
/// lives: `?model=` points at a media ITEM, so its URL basename is an opaque hash. `fetch` follows the
/// site's 307 to the byte route, so this reads the header off that FINAL response. `None` for the header
/// on a cross-origin host that doesn't send `Access-Control-Expose-Headers: Content-Disposition` — the
/// caller's fallback chain ([`crate::web_name::model_name`]) then lands on today's basename.
pub(crate) async fn fetch_bytes(url: &str) -> Option<FetchedModel> {
    use wasm_bindgen_futures::JsFuture;
    let win = web_sys::window()?;
    let resp = JsFuture::from(win.fetch_with_str(url)).await.ok()?;
    let resp: web_sys::Response = resp.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }
    // Read the header BEFORE draining the body — `headers()` is cheap and infallible-ish, but doing it
    // first keeps the failure of one from costing the other.
    let disposition = resp.headers().get("content-disposition").ok().flatten();
    let buf = JsFuture::from(resp.array_buffer().ok()?).await.ok()?;
    Some((js_sys::Uint8Array::new(&buf).to_vec(), disposition))
}
