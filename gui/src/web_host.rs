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

/// The page URL's `?name=` query value, percent-decoded (W.3.12) — how a host page hands the app a
/// model reference (`?model=<url>`). `None` when absent or the URL machinery is unavailable.
pub(crate) fn query_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get(name)
}

/// The save-back endpoint (W.5.8), from the host page's `data-save-url` on the `#fab-gui` canvas — the
/// site configures it in its Half-2 wiring (the endpoint the app POSTs the three variants to). Absent
/// ⇒ a documented same-origin default. Relative URLs resolve against the page, so the ambient session
/// cookie rides (same-origin). See `docs/web-save-back.md` — the exact route is chotchki's site-side call.
pub(crate) fn save_endpoint() -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("fab-gui"))
        .and_then(|c| c.get_attribute("data-save-url"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/admin/media/update".to_string())
}

/// POST the save-back variants as `multipart/form-data` (W.5.8). Each file rides as a named `Blob`
/// with a FILENAME — the site classifies by extension, not part name or Content-Type (recon), so the
/// filename's extension is what matters; the declared MIME is a courtesy. `media_ref` is a text field
/// naming the item to update in place. Credentials are `same-origin` so the ambient session cookie
/// (`id`, SameSite=Lax) authenticates the Admin — no token, no CSRF header (none exists server-side).
/// NO manual `Content-Type`: the browser owns the multipart boundary. `Err(msg)` on any failure.
pub(crate) async fn upload_multipart(
    endpoint: &str,
    media_ref: &str,
    files: &[(&str, &str, &str, &[u8])], // (field name, filename, mime, bytes)
) -> Result<String, String> {
    use wasm_bindgen_futures::JsFuture;
    let build = || -> Option<web_sys::FormData> {
        let form = web_sys::FormData::new().ok()?;
        for (field, filename, mime, bytes) in files {
            let array = js_sys::Uint8Array::from(*bytes);
            let parts = js_sys::Array::of1(&array.buffer());
            let opts = web_sys::BlobPropertyBag::new();
            opts.set_type(mime);
            let blob =
                web_sys::Blob::new_with_buffer_source_sequence_and_options(&parts, &opts).ok()?;
            form.append_with_blob_and_filename(field, &blob, filename).ok()?;
        }
        form.append_with_str("media_ref", media_ref).ok()?;
        Some(form)
    };
    let form = build().ok_or_else(|| "could not assemble the upload form".to_string())?;

    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&form);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);

    let win = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let resp = JsFuture::from(win.fetch_with_str_and_init(endpoint, &init))
        .await
        .map_err(|_| "upload request failed (network / offline?)".to_string())?;
    let resp: web_sys::Response = resp
        .dyn_into()
        .map_err(|_| "upload: not a Response".to_string())?;
    if !resp.ok() {
        // 401/403 = not a logged-in admin on the site; surface it so the UX can say so.
        return Err(match resp.status() {
            401 => "upload rejected (401) — log in as admin on hotchkiss.io first".to_string(),
            403 => "upload rejected (403) — this session isn't an admin".to_string(),
            s => format!("upload rejected: HTTP {s}"),
        });
    }
    let text = JsFuture::from(resp.text().map_err(|_| "no response body".to_string())?)
        .await
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    Ok(text)
}

/// Fetch `url` as text (W.3.12) — the `?model=` .scad body. Relative URLs resolve against the PAGE
/// (not the bundle base), so a host page can point at its own assets; same-origin always works under
/// the bundle's COOP/COEP, cross-origin needs CORS + CORP on the model host (docs/web-embed.md).
/// `None` on any failure — the caller reports and falls back to the demo.
pub(crate) async fn fetch_text(url: &str) -> Option<String> {
    use wasm_bindgen_futures::JsFuture;
    let win = web_sys::window()?;
    let resp = JsFuture::from(win.fetch_with_str(url)).await.ok()?;
    let resp: web_sys::Response = resp.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }
    JsFuture::from(resp.text().ok()?).await.ok()?.as_string()
}
