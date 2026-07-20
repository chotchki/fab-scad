//! W.3.28: the desktop Publish flow — render the model AND a clean cover through fab's OWN kernel +
//! renderer, then upload. Replaces the external-OpenSCAD `publish_model` (a `.app` has no `openscad` on
//! its PATH, and we ARE OpenSCAD now). Native only — the web publishes via the save-back (W.5).
//!
//! A phased state machine ([`PubFlow`]) because the cover needs GPU frames to render:
//!   Idle → Rendering (off-thread kernel render → mesh 3MFs) → Cover (build an OFFSCREEN scene on a
//!   private render layer at the live camera's angle, let it settle) → Capturing (screenshot the target,
//!   wait for the PNG, tear the scene down) → Uploading (off-thread upload) → Idle.
//! The cover renders to its own image target on `RenderLayers::layer(2)`, so it's immune to the live
//! view's slice/visibility state and never shows the UI chrome — a clean whole-model shot.

use crate::console::{self, Kind};
use crate::cover::{cover_orbit, spawn_cover_scene};
use crate::*;

/// Kernel-rendered artifacts headed for upload: the display STL (drives the cover mesh) + its bounds
/// (to frame the cover) + the two mesh variant files already written to the scratch dir.
pub(crate) struct Arts {
    stl: Vec<u8>,
    min: [f64; 3],
    max: [f64; 3],
    low: std::path::PathBuf,
    high: std::path::PathBuf,
}

/// Everything decided on the main thread at Publish time, carried through the phases: page fields, the
/// resolved endpoint + key, the live camera pose to frame the cover with, and the scratch paths.
pub(crate) struct PubMeta {
    title: String,
    description: String,
    base_url: String,
    key: String,
    orbit: (f32, f32, f32, Vec3),
    cover_png: std::path::PathBuf,
    /// The model's `.scad` source — uploaded as a download so a published design ships remixable source.
    source: std::path::PathBuf,
    plates: Option<std::path::PathBuf>,
}

/// The Publish state machine. `Default` = Idle so [`std::mem::take`] can move a phase out to transition.
#[derive(Resource, Default)]
pub(crate) enum PubFlow {
    #[default]
    Idle,
    Rendering {
        task: Task<Result<Arts, String>>,
        meta: PubMeta,
    },
    Cover {
        arts: Arts,
        meta: PubMeta,
        ents: Vec<Entity>,
        target: Handle<Image>,
        frames: u32,
    },
    Capturing {
        arts: Arts,
        meta: PubMeta,
        ents: Vec<Entity>,
        frames: u32,
    },
    Uploading {
        task: Task<Result<String, String>>,
    },
}

/// On the Publish command: resolve the key (no key ⇒ pop Settings, the W.3.27 loud cue), snapshot the
/// live camera angle, and kick the off-thread kernel render. No OpenSCAD.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn publish_kick(
    scene: Res<SceneCfg>,
    editor: Res<EditorBuf>,
    pool: Res<GeomPool>,
    mut flow: ResMut<PubFlow>,
    mut dialog: ResMut<crate::publish_dialog::PublishDialog>,
    mut settings: ResMut<crate::settings::SettingsUi>,
    mut status: ResMut<Status>,
    cams: Query<&Orbit>,
) {
    // The Publish DIALOG (W.3.29.6) supplies the title/description and raises `confirmed` on commit; take
    // it (one-shot) to start. The button → dialog → this: no auto-publish.
    if !std::mem::take(&mut dialog.confirmed) {
        return;
    }
    if !matches!(*flow, PubFlow::Idle) {
        status.0 = "already publishing…".into();
        return;
    }
    let resolved = fab_scad::credentials::resolve();
    let Some(key) = resolved.api_key else {
        settings.request_open();
        status.0 = "no hotchkiss.io key — add one in Settings (just opened)".into();
        return;
    };
    let base_url = resolved.url;

    // The cover frames the model at the CURRENT view angle (fall back to the startup pose if, somehow,
    // there's no camera). Same orbit the main camera uses, so the cover matches what you were looking at.
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

    // The page TITLE/description come from the dialog (the user's, pre-filled from the manifest/filename).
    // A blank title can't happen (the dialog requires it), but fall back to the opened file's stem, then a
    // generic name, defensively.
    let title = {
        let t = dialog.title.trim();
        if !t.is_empty() {
            t.to_string()
        } else {
            scene
                .source
                .as_deref()
                .and_then(|s| s.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model".into())
        }
    };
    let description = dialog.description.clone();
    let out_dir = scene.tmp.join("publish");

    // Resolve the publish source: an opened `.scad` on disk, else the editor BUFFER staged to a temp file
    // named from the provided TITLE (W.3.33 — a pasted model has no file). `src` is a real path either way:
    // the render reads it and the `.scad` upload rides it, and since the upload names the source part by
    // its file NAME, the pasted case ships `<title-slug>.scad` (chotchki's call) — not a temp hash. `stem`
    // names the scratch cover/mesh artifacts.
    let (src, stem) = match scene.source.clone() {
        Some(src) => {
            let stem = src
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model".into());
            (src, stem)
        }
        None => {
            let slug = fab_scad::publish_contract::slugify(&title);
            let slug = if slug.is_empty() {
                "model".into()
            } else {
                slug
            };
            if let Err(e) = std::fs::create_dir_all(&out_dir) {
                status.0 = format!("publish: scratch dir failed ({e})");
                return;
            }
            let staged = out_dir.join(format!("{slug}.scad"));
            if let Err(e) = std::fs::write(&staged, editor.text.as_bytes()) {
                status.0 = format!("publish: couldn't stage the pasted model ({e})");
                console::push(Kind::Scad, format!("publish: stage buffer failed: {e}"));
                return;
            }
            (staged, slug)
        }
    };
    let cover_png = out_dir.join(format!("{stem}-cover.png"));
    // The printable plate .3mf, if `fab make` / the Export tab left one beside the source (best-effort; a
    // staged buffer sits in the scratch dir with no sibling plate, so this is None there).
    let plates = src.with_file_name(format!("{stem}-plates.3mf"));
    let plates = plates.exists().then_some(plates);

    let root = scene
        .root
        .as_ref()
        .map(|r| r.to_string_lossy().into_owned());
    let src_path = src.to_string_lossy().into_owned();
    let pool = pool.clone();
    let out2 = out_dir.clone();
    let stem2 = stem.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        std::fs::create_dir_all(&out2).map_err(|e| format!("scratch dir: {e}"))?;
        // 1. full-res whole render → base handle + the display STL (the cover mesh source).
        let (base, stl, min, max) = match pool
            .call(Request::RenderWhole {
                source: Source::Path(src_path),
                root,
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
        let variants = pool
            .call(Request::SaveMeshes {
                base,
                budget: 20_000,
            })
            .await;
        let _ = pool.call(Request::Free { ids: vec![base] }).await;
        let (low_b, high_b, ext) = match variants {
            Ok(Response::SavedMeshes { low, high, ext }) => (low, high, ext),
            Ok(Response::Failed { error }) => return Err(format!("mesh export failed: {error}")),
            Ok(_) => return Err("save-meshes: unexpected service response".into()),
            Err(e) => return Err(format!("save-meshes transport: {e}")),
        };
        let low = out2.join(format!("{stem2}-preview.{ext}"));
        let high = out2.join(format!("{stem2}.{ext}"));
        std::fs::write(&low, low_b).map_err(|e| format!("write preview mesh: {e}"))?;
        std::fs::write(&high, high_b).map_err(|e| format!("write full mesh: {e}"))?;
        Ok(Arts {
            stl,
            min,
            max,
            low,
            high,
        })
    });

    status.0 = format!("publishing {stem}: rendering…");
    console::push(
        Kind::Scad,
        format!("publish: rendering {stem} for {base_url}"),
    );
    info!("publish: rendering {stem} → {base_url}");
    *flow = PubFlow::Rendering {
        task,
        meta: PubMeta {
            title,
            description,
            base_url,
            key,
            orbit,
            cover_png,
            source: src,
            plates,
        },
    };
}

/// Drive the phases: poll the render, build/settle/capture the offscreen cover, then upload. One system
/// owns the whole machine (it needs `Commands` + the asset stores for the cover scene); it `take`s the
/// current phase and writes the next.
pub(crate) fn publish_flow(
    mut commands: Commands,
    mut flow: ResMut<PubFlow>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut status: ResMut<Status>,
) {
    match std::mem::take(&mut *flow) {
        PubFlow::Idle => {}

        PubFlow::Rendering { mut task, meta } => match block_on(future::poll_once(&mut task)) {
            None => *flow = PubFlow::Rendering { task, meta },
            Some(Ok(arts)) => {
                // Frame the cover at the live ANGLE (yaw/pitch) but a bounds-derived distance + center, so
                // it's not hostage to the user's zoom and the model lands inside the wide letterbox's safe
                // center square (W.3.28.8).
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
                *flow = PubFlow::Cover {
                    arts,
                    meta,
                    ents,
                    target,
                    frames: 0,
                };
            }
            Some(Err(e)) => fail(&mut flow, &mut status, "publish render", &e),
        },

        // Let the cover camera render a few frames into its target before we grab it.
        PubFlow::Cover {
            arts,
            meta,
            ents,
            target,
            frames,
        } => {
            if frames < 3 {
                *flow = PubFlow::Cover {
                    arts,
                    meta,
                    ents,
                    target,
                    frames: frames + 1,
                };
            } else {
                commands
                    .spawn(Screenshot::image(target))
                    .observe(save_to_disk(meta.cover_png.clone()));
                *flow = PubFlow::Capturing {
                    arts,
                    meta,
                    ents,
                    frames: 0,
                };
            }
        }

        // Wait for the async PNG save to land, then tear the cover scene down and upload.
        PubFlow::Capturing {
            arts,
            meta,
            ents,
            frames,
        } => {
            let ready = meta.cover_png.exists();
            if !ready && frames < 90 {
                *flow = PubFlow::Capturing {
                    arts,
                    meta,
                    ents,
                    frames: frames + 1,
                };
            } else {
                for e in ents {
                    commands.entity(e).despawn();
                }
                if !ready {
                    fail(&mut flow, &mut status, "publish cover", "capture timed out");
                } else {
                    let task = spawn_upload(meta, arts);
                    status.0 = "publishing: uploading…".into();
                    console::push(Kind::Scad, "publish: uploading to hotchkiss.io…");
                    *flow = PubFlow::Uploading { task };
                }
            }
        }

        PubFlow::Uploading { mut task } => match block_on(future::poll_once(&mut task)) {
            None => *flow = PubFlow::Uploading { task },
            Some(Ok(url)) => {
                status.0 = format!("published -> {url}");
                console::push(Kind::Scad, format!("published -> {url}"));
                info!("published -> {url}");
                *flow = PubFlow::Idle;
            }
            Some(Err(e)) => fail(&mut flow, &mut status, "publish upload", &e),
        },
    }
}

/// Report a failure LOUDLY — the status bar AND the console AND the log — then reset to Idle. The silent
/// status-line-only no-op is exactly what hid the OpenSCAD breakage; every failure shouts now.
fn fail(flow: &mut PubFlow, status: &mut Status, stage: &str, err: &str) {
    status.0 = format!("publish failed: {err}");
    console::push(Kind::Scad, format!("{stage} failed: {err}"));
    error!("{stage}: {err}");
    *flow = PubFlow::Idle;
}

/// Upload the cover + the two mesh variants + the `.scad` source (+ the plate, if any) off-thread via the
/// pure-upload path.
fn spawn_upload(meta: PubMeta, arts: Arts) -> Task<Result<String, String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        let mut downloads = Vec::new();
        if let Some(p) = &meta.plates {
            downloads.push(fab_scad::publish::Media {
                path: p,
                title: format!("{} — print plates (.3mf)", meta.title),
            });
        }
        // The .scad source rides the SAME model item (a variant), so the embed offers "Open in the
        // slicer" — not a standalone download item.
        let source = meta.source.exists().then_some(meta.source.as_path());
        fab_scad::publish::upload_model(
            &meta.base_url,
            &meta.key,
            &meta.title,
            &meta.description,
            Some(&meta.cover_png),
            &[&arts.low, &arts.high],
            source,
            downloads,
        )
        .map_err(|e| format!("{e:#}"))
    })
}
