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

use bevy::camera::visibility::RenderLayers;
use bevy::render::render_resource::{TextureFormat, TextureUsages};

use crate::console::{self, Kind};
use crate::*;

/// The private render layer the cover scene lives on — the main cameras render `[0, 1]` (model + gizmos),
/// so layer 2 is ours alone: the cover camera sees only the mesh + lights we spawn here.
const COVER_LAYER: usize = 2;
const COVER_W: u32 = 1200;
const COVER_H: u32 = 900;

/// Kernel-rendered artifacts headed for upload: the display STL (drives the cover mesh) + the two mesh
/// variant files already written to the scratch dir.
pub(crate) struct Arts {
    stl: Vec<u8>,
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
pub(crate) fn publish_kick(
    mut ev: MessageReader<PanelCmd>,
    scene: Res<SceneCfg>,
    pool: Res<GeomPool>,
    mut flow: ResMut<PubFlow>,
    mut settings: ResMut<crate::settings::SettingsUi>,
    mut status: ResMut<Status>,
    cams: Query<&Orbit>,
) {
    if !ev.read().any(|c| *c == PanelCmd::Publish) {
        return;
    }
    if !matches!(*flow, PubFlow::Idle) {
        status.0 = "already publishing…".into();
        return;
    }
    let Some(src) = scene.source.clone() else {
        status.0 = "no .scad to publish".into();
        console::push(Kind::Scad, "publish: no .scad open");
        return;
    };
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

    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into());
    let (title, description) = match fab_scad::manifest::Manifest::load_near(&src) {
        Ok(m) => (
            m.title().to_string(),
            m.publish.map(|p| p.description).unwrap_or_default(),
        ),
        Err(_) => (stem.clone(), String::new()),
    };
    let out_dir = scene.tmp.join("publish");
    let cover_png = out_dir.join(format!("{stem}-cover.png"));
    // The printable plate .3mf, if `fab make` / the Export tab left one beside the source (best-effort).
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
        let (base, stl) = match pool
            .call(Request::RenderWhole {
                source: Source::Path(src_path),
                root,
                preview: false,
                quality: Quality::Final,
            })
            .await
        {
            Ok(Response::Rendered { id, stl, .. }) => (id, stl),
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
        Ok(Arts { stl, low, high })
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
                let (target, ents) = spawn_cover_scene(
                    &mut commands,
                    &mut images,
                    &mut meshes,
                    &mut materials,
                    &arts.stl,
                    meta.orbit,
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

/// Build the OFFSCREEN cover scene on [`COVER_LAYER`]: an image render target, a fresh mesh from the
/// rendered STL, two lights (mirroring `spawn_environment`), and a camera at the live orbit. Everything's
/// on the private layer, so the main cameras don't draw it and it doesn't draw the live scene. Returns the
/// target to screenshot + the entities to despawn.
fn spawn_cover_scene(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    stl: &[u8],
    orbit: (f32, f32, f32, Vec3),
) -> (Handle<Image>, Vec<Entity>) {
    let mut img = Image::new_target_texture(COVER_W, COVER_H, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);

    let layer = RenderLayers::layer(COVER_LAYER);
    let (yaw, pitch, radius, tgt) = orbit;
    let ents = vec![
        commands
            .spawn((
                Mesh3d(mesh_from_bytes(meshes, stl)),
                MeshMaterial3d(part_material(materials)),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                DirectionalLight {
                    illuminance: 6000.0,
                    ..default()
                },
                Transform::from_xyz(80.0, -120.0, 160.0).looking_at(Vec3::ZERO, Vec3::Z),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                DirectionalLight {
                    illuminance: 2000.0,
                    ..default()
                },
                Transform::from_xyz(-120.0, 100.0, 60.0).looking_at(Vec3::ZERO, Vec3::Z),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                Camera3d::default(),
                Camera {
                    // A distinct order from the window cameras; it targets its own image, so this only
                    // orders it against itself. Clears to the ClearColor resource (theme::VIEWPORT).
                    order: -1,
                    ..default()
                },
                RenderTarget::Image(target.clone().into()),
                orbit_transform(yaw, pitch, radius, tgt),
                layer,
            ))
            .id(),
    ];
    (target, ents)
}

/// Upload the cover + the two mesh variants (+ the plate, if any) off-thread via the pure-upload path.
fn spawn_upload(meta: PubMeta, arts: Arts) -> Task<Result<String, String>> {
    AsyncComputeTaskPool::get().spawn(async move {
        let mut downloads = Vec::new();
        if let Some(p) = &meta.plates {
            downloads.push(fab_scad::publish::Media {
                path: p,
                title: format!("{} — print plates (.3mf)", meta.title),
            });
        }
        fab_scad::publish::upload_model(
            &meta.base_url,
            &meta.key,
            &meta.title,
            &meta.description,
            Some(&meta.cover_png),
            &[&arts.low, &arts.high],
            downloads,
        )
        .map_err(|e| format!("{e:#}"))
    })
}
