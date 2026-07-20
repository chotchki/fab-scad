//! W.3.29.4/.3: the WEB Publish flow — create a NEW `/3d` gallery item straight from the browser, the
//! counterpart to the desktop [`crate::publish_native`]. Same shape and the SAME phased state machine
//! (render → offscreen cover → capture → upload), but over `fetch` + the ambient same-origin session
//! cookie instead of reqwest + an `hio_` key, and the cover is captured to PNG BYTES (no fs) instead of a
//! file. Reuses the shared [`crate::cover`] scene and the shared publish CONTRACT
//! ([`fab_scad::publish_contract`]) so the two clients can't drift.
//!
//! DISTINCT from the "Update -> hotchkiss.io" save-back: that PUTs new variants onto the item you OPENED;
//! this MINTS a new gallery page. Admin-only server-side — a non-admin cookie 401/403s, said LOUDLY.
//!
//! Wasm only. The whole module is `cfg`'d out on native, where [`crate::publish_native`] owns Publish.
#![cfg(target_arch = "wasm32")]

use crate::cover::{cover_orbit, spawn_cover_scene};
use crate::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use fab_scad::publish_contract as contract;

/// Kernel-rendered artifacts headed for upload (the byte twin of the desktop `Arts`): the display STL that
/// drives the cover mesh + its bounds (to frame the cover) + the two mesh variants as BYTES.
pub(crate) struct WebArts {
    stl: Vec<u8>,
    min: [f64; 3],
    max: [f64; 3],
    low: Vec<u8>,
    high: Vec<u8>,
    ext: String,
}

/// Everything decided on the main thread at Publish time, carried through the phases: page fields, the
/// same-origin base, the live camera pose to frame the cover with, the baked `.scad` source bytes, and the
/// optional printable plate.
pub(crate) struct WebMeta {
    title: String,
    description: String,
    base: String,
    stem: String,
    source: Vec<u8>,
    orbit: (f32, f32, f32, Vec3),
    plate: Option<Vec<u8>>,
    plate_name: String,
}

/// The Publish state machine (mirrors [`crate::publish_native::PubFlow`]). `Default` = Idle so
/// [`std::mem::take`] can move a phase out to transition.
#[derive(Resource, Default)]
pub(crate) enum PubWebFlow {
    #[default]
    Idle,
    Rendering {
        task: Task<Result<WebArts, String>>,
        meta: WebMeta,
    },
    Cover {
        arts: WebArts,
        meta: WebMeta,
        ents: Vec<Entity>,
        target: Handle<Image>,
        frames: u32,
    },
    Capturing {
        arts: WebArts,
        meta: WebMeta,
        ents: Vec<Entity>,
        frames: u32,
    },
    Uploading {
        task: Task<Result<String, String>>,
    },
}

/// The cover PNG bytes bridge: the screenshot observer ([`cover_to_bytes`]) encodes on the main thread and
/// sends here; the `Capturing` phase drains it. `async-channel` because its ends are `Send + Sync`.
#[derive(Resource)]
pub(crate) struct CoverSink {
    tx: async_channel::Sender<Vec<u8>>,
    rx: async_channel::Receiver<Vec<u8>>,
}

impl Default for CoverSink {
    fn default() -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self { tx, rx }
    }
}

/// On the Publish DIALOG's confirm (W.3.29.6 raises `confirmed`; the button opened the modal): bake the
/// live config into the source, snapshot the camera angle, then kick the off-thread render → mesh
/// variants. The cover + upload happen in [`publish_web_flow`] (they need main-thread ECS / GPU frames).
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn publish_web_kick(
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    pieces: Res<crate::print::PrintPieces>,
    scene: Res<SceneCfg>,
    pool: Res<GeomPool>,
    cams: Query<&Orbit>,
    mut dialog: ResMut<crate::publish_dialog::PublishDialog>,
    mut flow: ResMut<PubWebFlow>,
    mut status: ResMut<Status>,
) {
    // The dialog owns the title/description and raises `confirmed` on commit; take it (one-shot) to start.
    if !std::mem::take(&mut dialog.confirmed) {
        return;
    }
    if !matches!(*flow, PubWebFlow::Idle) {
        status.0 = "already publishing…".into();
        return;
    }
    let title = dialog.title.trim().to_string();
    if title.is_empty() {
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
    // Name the source from the deep-linked model's basename when there is one; else (a pasted buffer with no
    // `?model=`) from the provided TITLE — so the published `.scad` reads `<title-slug>.scad`, matching the
    // desktop path (W.3.33). Everything downstream (`src_name`, mesh + plate + cover names) keys off `stem`.
    let stem = editor
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.strip_suffix(".scad").unwrap_or(n))
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let slug = contract::slugify(&title);
            if slug.is_empty() {
                "model".into()
            } else {
                slug
            }
        });

    // The printable Bambu plate, if a plan was staged — a standalone download item, same as the save-back.
    let plate = crate::print::plate_3mf_bytes(&pieces, &parts, &scene);
    let plate_name = format!("{stem}-plates.3mf");

    // Frame the cover at the CURRENT view angle (fall back to a default pose if there's somehow no camera).
    let orbit = cams
        .iter()
        .next()
        .map(|o| (o.yaw, o.pitch, o.radius, o.target))
        .unwrap_or((
            -0.7,
            0.5,
            scene.bed[0].max(scene.bed[1]).max(80.0),
            Vec3::ZERO,
        ));

    // Same-origin base: the page origin, so the contract's URLs are absolute-same-origin and the cookie
    // rides. Empty on the (impossible-in-browser) no-window path — relative URLs still resolve same-origin.
    let base = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();

    // `main` (the baked source bytes) feeds BOTH the render source and the uploaded `.scad` variant.
    let main = baked.into_bytes();
    let source = main.clone();
    let pool = pool.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let libs = crate::lib_fetch::lib_closure(&String::from_utf8_lossy(&main)).await;

        // 1. full-res render → held handle + display STL + bounds.
        let (id, stl, min, max) = match pool
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
            Ok(Response::Rendered {
                id, stl, min, max, ..
            }) => (id, stl, min, max),
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

        Ok(WebArts {
            stl,
            min,
            max,
            low,
            high,
            ext,
        })
    });

    status.0 = "publishing to hotchkiss.io…".into();
    info!("publish: rendering {stem} for the web");
    *flow = PubWebFlow::Rendering {
        task,
        meta: WebMeta {
            title,
            description,
            base,
            stem,
            source,
            orbit,
            plate,
            plate_name,
        },
    };
}

/// Drive the phases (mirrors [`crate::publish_native::publish_flow`]): poll the render, build/settle the
/// offscreen cover, capture it to PNG bytes, then upload. One system owns the whole machine — it needs
/// `Commands` + the asset stores for the cover scene.
pub(crate) fn publish_web_flow(
    mut commands: Commands,
    mut flow: ResMut<PubWebFlow>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    sink: Res<CoverSink>,
    mut status: ResMut<Status>,
) {
    match std::mem::take(&mut *flow) {
        PubWebFlow::Idle => {}

        PubWebFlow::Rendering { mut task, meta } => match block_on(future::poll_once(&mut task)) {
            None => *flow = PubWebFlow::Rendering { task, meta },
            Some(Ok(arts)) => {
                let orbit = cover_orbit(meta.orbit.0, meta.orbit.1, arts.min, arts.max);
                let (target, ents) = spawn_cover_scene(
                    &mut commands,
                    &mut images,
                    &mut meshes,
                    &mut materials,
                    &arts.stl,
                    orbit,
                );
                status.0 = "publishing: cover…".into();
                *flow = PubWebFlow::Cover {
                    arts,
                    meta,
                    ents,
                    target,
                    frames: 0,
                };
            }
            Some(Err(e)) => fail(&mut flow, &mut status, &e),
        },

        // Let the cover camera render a few frames into its target before we grab it.
        PubWebFlow::Cover {
            arts,
            meta,
            ents,
            target,
            frames,
        } => {
            if frames < 3 {
                *flow = PubWebFlow::Cover {
                    arts,
                    meta,
                    ents,
                    target,
                    frames: frames + 1,
                };
            } else {
                commands
                    .spawn(Screenshot::image(target))
                    .observe(cover_to_bytes(sink.tx.clone()));
                *flow = PubWebFlow::Capturing {
                    arts,
                    meta,
                    ents,
                    frames: 0,
                };
            }
        }

        // Wait for the encoded PNG bytes; tear the cover scene down and upload. A capture that never lands
        // (a WebGL offscreen-target quirk) falls back to a COVERLESS publish after the timeout — the site
        // renders its own preview from the mesh, so the publish still succeeds.
        PubWebFlow::Capturing {
            arts,
            meta,
            ents,
            frames,
        } => {
            let cover = sink.rx.try_recv().ok();
            if cover.is_none() && frames < 90 {
                *flow = PubWebFlow::Capturing {
                    arts,
                    meta,
                    ents,
                    frames: frames + 1,
                };
            } else {
                for e in ents {
                    commands.entity(e).despawn();
                }
                if cover.is_none() {
                    info!("publish: cover capture timed out — publishing coverless");
                }
                let task = spawn_upload(meta, arts, cover);
                status.0 = "publishing: uploading…".into();
                *flow = PubWebFlow::Uploading { task };
            }
        }

        PubWebFlow::Uploading { mut task } => match block_on(future::poll_once(&mut task)) {
            None => *flow = PubWebFlow::Uploading { task },
            Some(Ok(url)) => {
                status.0 = format!("published -> {url}");
                info!("{}", status.0);
                *flow = PubWebFlow::Idle;
            }
            Some(Err(e)) => fail(&mut flow, &mut status, &e),
        },
    }
}

/// Report a failure LOUDLY (status bar + log) and reset to Idle.
fn fail(flow: &mut PubWebFlow, status: &mut Status, err: &str) {
    status.0 = format!("publish failed: {err}");
    error!("publish: {err}");
    *flow = PubWebFlow::Idle;
}

/// A screenshot observer that encodes the captured render-target [`Image`] to PNG BYTES and sends them to
/// the [`CoverSink`] — the wasm twin of `save_to_disk` (which writes a file). Drops the alpha (it carries
/// brightness under HDR), same as `save_to_disk`.
fn cover_to_bytes(tx: async_channel::Sender<Vec<u8>>) -> impl FnMut(On<ScreenshotCaptured>) {
    move |captured: On<ScreenshotCaptured>| {
        let img = captured.image.clone();
        match img.try_into_dynamic() {
            Ok(dyn_img) => {
                let rgb = dyn_img.to_rgb8();
                let mut buf = std::io::Cursor::new(Vec::new());
                match rgb.write_to(&mut buf, image::ImageFormat::Png) {
                    Ok(()) => {
                        let _ = tx.try_send(buf.into_inner());
                    }
                    Err(e) => error!("cover PNG encode failed: {e}"),
                }
            }
            Err(e) => error!("cover image convert failed: {e}"),
        }
    }
}

/// Upload off-thread via the pure fetch path.
fn spawn_upload(
    meta: WebMeta,
    arts: WebArts,
    cover: Option<Vec<u8>>,
) -> Task<Result<String, String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        upload(
            &meta.base,
            &meta.title,
            &meta.description,
            &meta.stem,
            &meta.source,
            &arts.low,
            &arts.high,
            &arts.ext,
            meta.plate.as_deref(),
            &meta.plate_name,
            cover.as_deref(),
        )
        .await
    })
}

/// The upload orchestration — the wasm mirror of [`fab_scad::publish::publish`], driven off the SAME
/// contract so the two clients agree. Uploads the mesh + `.scad` as ONE model item (so the embed shows the
/// spinning viewer + "Open in the slicer"), the plate as a standalone download, the cover as the page
/// banner, composes the markdown, then create-or-updates the page under `/3d`. Returns the page URL.
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
    cover: Option<&[u8]>,
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

    // The cover PNG → its own media item → the page banner (`page_cover_media_ref`). Absent ⇒ coverless,
    // and the site renders its own preview from the mesh.
    let cover_ref = match cover {
        Some(png) => {
            let cover_name = format!("{stem}-cover.png");
            let files = [(
                contract::MEDIA_FILE_FIELD,
                cover_name.as_str(),
                "image/png",
                png,
            )];
            Some(post_media(base, &files, &format!("{title} — cover")).await?)
        }
        None => None,
    };

    let markdown = contract::compose_markdown(description, &model_ref, &downloads);

    // Derive the slug locally (mirroring the server) and create-or-update: GET to check, POST to create if
    // missing, then PUT the body + cover.
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
            (
                contract::PAGE_COVER_FIELD,
                cover_ref.as_deref().unwrap_or(""),
            ),
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
