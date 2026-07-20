//! W.3.29.4: the WEB Publish flow — create a NEW `/3d` gallery item straight from the browser, the
//! counterpart to the desktop [`crate::publish_native`]. Same shape (render → mesh variants → upload the
//! `.scad`+mesh as ONE item → create-or-update the page), but over `fetch` + the ambient same-origin
//! session cookie instead of reqwest + an `hio_` key, and — for now — WITHOUT a cover (W.3.29.3 deferred;
//! the site renders its own preview from the mesh until then).
//!
//! It reuses two things so the paths can't drift: the save-back's render→`SaveMeshes` pipeline
//! ([`crate::jobs::save_action`]) and the shared publish CONTRACT ([`fab_scad::publish_contract`], the
//! endpoints/fields/slug/markdown the desktop client also builds on). DISTINCT from the "Update ->
//! hotchkiss.io" save-back: that PUTs new variants onto the item you OPENED; this MINTS a new gallery
//! page. Admin-only server-side — a non-admin cookie 401/403s, and we say so LOUDLY.
//!
//! Wasm only. The whole module is `cfg`'d out on native, where [`crate::publish_native`] owns Publish.
#![cfg(target_arch = "wasm32")]

use crate::*;
use fab_scad::publish_contract as contract;

/// The in-flight web-publish job: render + export + fetch-upload, off the main thread. Yields the
/// published page URL or a loud error. One slot — a second Publish while one runs just says "already…".
#[derive(Resource, Default)]
pub(crate) struct PubWebJob(pub(crate) Option<Task<Result<String, String>>>);

/// On the Publish DIALOG's confirm (W.3.29.6 raises `confirmed`; the button opened the modal): bake the
/// live config into the source, then off-thread render full-res → the two mesh variants → fetch-upload the
/// model item + page. Mirrors [`crate::jobs::save_action`]'s pipeline; the title/description come from the
/// dialog, not the filename.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn publish_web_kick(
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    pieces: Res<crate::print::PrintPieces>,
    scene: Res<SceneCfg>,
    pool: Res<GeomPool>,
    mut dialog: ResMut<crate::publish_dialog::PublishDialog>,
    mut job: ResMut<PubWebJob>,
    mut status: ResMut<Status>,
) {
    // The dialog owns the title/description and raises `confirmed` on commit; take it (one-shot) to start.
    // The button → dialog → this: no auto-publish, same handshake the desktop flow uses.
    if !std::mem::take(&mut dialog.confirmed) {
        return;
    }
    if job.0.is_some() {
        status.0 = "already publishing…".into();
        return;
    }
    let title = dialog.title.trim().to_string();
    if title.is_empty() {
        // The dialog requires a title, so this is belt-and-suspenders — a blank one has no slug.
        status.0 = "a title is required to publish".into();
        return;
    }
    let description = dialog.description.clone();

    // Bake the live slicing config + bed into the source (exactly the .scad download / save-back path), so
    // the published, remixable source restores the plan on reload. This IS the source variant we upload.
    let printer = config::PrinterCfg {
        bed: [
            scene.bed[0] as f64,
            scene.bed[1] as f64,
            scene.bed[2] as f64,
        ],
    };
    let baked = config::with_config_block(&editor.text, &parts.0, Some(printer));
    let name = editor
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("model.scad");
    let stem = name.strip_suffix(".scad").unwrap_or(name).to_string();

    // The printable Bambu plate, if a plan was staged on the Export tab — a standalone download item, same
    // as the save-back. Best-effort: `None` (no pieces / a pack error) just publishes without it.
    let plate = crate::print::plate_3mf_bytes(&pieces, &parts, &scene);
    let plate_name = format!("{stem}-plates.3mf");

    // Same-origin base: the page origin, so the contract's URLs are absolute-same-origin and the cookie
    // rides. Empty on the (impossible-in-browser) no-window path — relative URLs still resolve same-origin.
    let base = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();

    status.0 = "publishing to hotchkiss.io…".into();
    info!("publish: rendering {stem} for the web");

    let pool = pool.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let main = baked.into_bytes();
        let libs = crate::lib_fetch::lib_closure(&String::from_utf8_lossy(&main)).await;

        // 1. full-res render → held handle.
        let id = match pool
            .call(Request::RenderWhole {
                source: Source::Bytes {
                    main: main.clone(),
                    libs,
                },
                root: None,
                preview: false,
                quality: Quality::Final,
            })
            .await
        {
            Ok(Response::Rendered { id, .. }) => id,
            Ok(Response::Failed { error }) => return Err(format!("render failed: {error}")),
            Ok(_) => return Err("render: unexpected service response".into()),
            Err(e) => return Err(format!("render transport: {e}")),
        };

        // 2. the two colored-3MF mesh variants off the handle; 3. free it regardless.
        let meshes = pool
            .call(Request::SaveMeshes {
                base: id,
                budget: 20_000,
            })
            .await;
        let _ = pool.call(Request::Free { ids: vec![id] }).await;
        let (low, high, ext) = match meshes {
            Ok(Response::SavedMeshes { low, high, ext }) => (low, high, ext),
            Ok(Response::Failed { error }) => return Err(format!("mesh export failed: {error}")),
            Ok(_) => return Err("save-meshes: unexpected service response".into()),
            Err(e) => return Err(format!("save-meshes transport: {e}")),
        };

        // 4. upload the model item (scad + mesh variants) + page, cookie-authenticated.
        upload(
            &base,
            &title,
            &description,
            &stem,
            &main,
            &low,
            &high,
            &ext,
            plate.as_deref(),
            &plate_name,
        )
        .await
    });
    job.0 = Some(task);
}

/// The upload orchestration — the wasm mirror of [`fab_scad::publish::publish`], driven off the SAME
/// contract so the two clients agree. Uploads the mesh + `.scad` as ONE model item (so the embed shows the
/// spinning viewer + "Open in the slicer"), the plate as a standalone download, composes the markdown,
/// then create-or-updates the page under `/3d`. Returns the published page URL.
#[allow(clippy::too_many_arguments)]
async fn upload(
    base: &str,
    title: &str,
    description: &str,
    stem: &str,
    source: &[u8],
    low: &[u8],
    high: &[u8],
    ext: &str,
    plate: Option<&[u8]>,
    plate_name: &str,
) -> Result<String, String> {
    let slug = contract::slugify(title);
    if slug.is_empty() {
        return Err(format!("title {title:?} has no slug-able characters"));
    }
    let mesh_mime = if ext == "3mf" {
        "model/3mf"
    } else {
        "model/stl"
    };
    let src_name = format!("{stem}.scad");
    let low_name = format!("{stem}_low.{ext}");
    let high_name = format!("{stem}.{ext}");

    // The model item: mesh LOD variants AND the `.scad` source in ONE `POST /media` → ONE ref. Same order
    // the desktop client sends (low, high, source). Kinds come from the filename extensions; identical
    // low==high bytes dedup server-side (fine for small models).
    let model_files: Vec<(&str, &str, &str, &[u8])> = vec![
        (
            contract::MEDIA_FILE_FIELD,
            low_name.as_str(),
            mesh_mime,
            low,
        ),
        (
            contract::MEDIA_FILE_FIELD,
            high_name.as_str(),
            mesh_mime,
            high,
        ),
        (
            contract::MEDIA_FILE_FIELD,
            src_name.as_str(),
            "application/x-openscad",
            source,
        ),
    ];
    let model_ref = post_media(base, &model_files, &format!("{title} — model")).await?;

    // The plate rides as its own download item when a plan was staged.
    let mut downloads: Vec<(String, String)> = Vec::new();
    if let Some(pb) = plate {
        let dl_title = format!("{title} — print plates (.3mf)");
        let files = [(contract::MEDIA_FILE_FIELD, plate_name, "model/3mf", pb)];
        let r = post_media(base, &files, &dl_title).await?;
        downloads.push((dl_title, r));
    }

    let markdown = contract::compose_markdown(description, &model_ref, &downloads);

    // Derive the slug locally (mirroring the server) and create-or-update: GET to check, POST to create if
    // missing, then PUT the body. No cover (W.3.29.3) — the empty field leaves the site's own preview.
    let page = contract::page_url(base, &slug);
    let exists = crate::web_host::fetch_status(&page).await?;
    if !(200..300).contains(&exists) {
        let (s, _) = crate::web_host::fetch_form(
            "POST",
            &contract::create_page_url(base),
            &[(contract::PAGE_TITLE_FIELD, title)],
        )
        .await?;
        // A create returns the page envelope (2xx) or a 303-to-HTML (3xx, if Accept negotiation slipped).
        if !(200..400).contains(&s) {
            return Err(publish_http_error("page create", s));
        }
    }
    let (s, _) = crate::web_host::fetch_form(
        "PUT",
        &page,
        &[
            (contract::PAGE_TITLE_FIELD, title),
            ("page_category", ""),
            (contract::PAGE_MARKDOWN_FIELD, &markdown),
            (contract::PAGE_COVER_FIELD, ""),
            (contract::PAGE_ORDER_FIELD, "0"),
            ("page_creation_date", ""),
        ],
    )
    .await?;
    if !(200..300).contains(&s) {
        return Err(publish_http_error("page update", s));
    }
    Ok(contract::public_url(base, &slug))
}

/// `POST /media` one item (files + a title text part) and read back the minted `ref`. Loud on the
/// admin-gate codes — a Publish from a non-admin browser is the expected failure to name clearly.
async fn post_media(
    base: &str,
    files: &[(&str, &str, &str, &[u8])],
    title: &str,
) -> Result<String, String> {
    let (status, body) = crate::web_host::fetch_multipart(
        "POST",
        &contract::media_url(base),
        &[(contract::MEDIA_TITLE_FIELD, title)],
        files,
    )
    .await?;
    if !(200..300).contains(&status) {
        return Err(publish_http_error("media upload", status));
    }
    parse_ref(&body).ok_or_else(|| "media upload: response carried no ref".to_string())
}

/// Map an HTTP status to a loud, actionable message. 401/403 = not a logged-in admin (the common
/// dogfood failure); everything else surfaces the code + the stage.
fn publish_http_error(stage: &str, status: u16) -> String {
    match status {
        401 => "publish rejected (401) — log in as admin on hotchkiss.io first".to_string(),
        403 => "publish rejected (403) — this session isn't an admin".to_string(),
        s => format!("{stage} -> HTTP {s}"),
    }
}

/// Pull the `ref` field out of the `POST /media` 201 manifest with the browser's own `JSON.parse` (this
/// crate's `serde_json` is dev-only, and the contract module deliberately leaves parsing to each side).
fn parse_ref(body: &str) -> Option<String> {
    let val = js_sys::JSON::parse(body).ok()?;
    js_sys::Reflect::get(
        &val,
        &wasm_bindgen::JsValue::from_str(contract::MEDIA_REF_FIELD),
    )
    .ok()?
    .as_string()
}

/// Land the web-publish job: report the URL or the error LOUDLY (status bar + log), per
/// gui-reactive-standard — no modal, the status line is the feedback surface.
pub(crate) fn poll_publish_web(mut job: ResMut<PubWebJob>, mut status: ResMut<Status>) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(url) => {
            status.0 = format!("published -> {url}");
            info!("{}", status.0);
        }
        Err(e) => {
            status.0 = format!("publish failed: {e}");
            error!("{}", status.0);
        }
    }
}
