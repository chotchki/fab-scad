//! fab-web (Phase A): the browser slicer. Upload an STL → the Manifold kernel plans it against
//! the bed (rotate-to-fit + auto cuts + auto onions, A.2), cut planes render on the model, Slice
//! shows the pieces, Export packs plates and downloads a Bambu 3mf (A.4) — all client-side, zero
//! server-side outputs. `Solid` is !Send by design: state holds the upload BYTES and every op
//! rebuilds the Solid where it runs — the same discipline a future worker split needs (A.8).
//! Runs native too (`cargo run -p fab-web -- --demo --bed=40`).

use bevy::asset::RenderAssetUsages;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

use fab_scad::kernel::Solid;
use fab_scad::manifest::{Connector, Cut, Slicing};
use fab_scad::num::Num;
use fab_scad::{auto, auto_slice, slicing};

mod stl;

/// Default build volume (mm); `?bed=N` / `--bed=N` overrides (cube bed) until printers.toml
/// grows a browser home.
const DEFAULT_BED: f64 = 256.0;
/// Plate gap for the packed export (mm) — matches `fab make`'s default.
const GAP: f64 = 5.0;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    run();
}

pub fn run() {
    let bed = bed_override().unwrap_or(DEFAULT_BED);
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "fab".into(),
                // The hosting document provides <canvas id="fab-web"> (web-bundle.md contract) —
                // binding to it, instead of appending our own, leaves layout to the page.
                #[cfg(target_arch = "wasm32")]
                canvas: Some("#fab-web".into()),
                #[cfg(target_arch = "wasm32")]
                fit_canvas_to_parent: true,
                ..default()
            }),
            ..default()
        }),
        MeshPickingPlugin,
    ));

    {
        use bevy::feathers::{dark_theme::create_dark_theme, theme::UiTheme, FeathersPlugins};
        app.add_plugins(FeathersPlugins)
            .insert_resource(UiTheme(create_dark_theme()));
    }

    app.insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(Bed([bed, bed, bed]))
        .init_resource::<Part>()
        .init_resource::<PickTask>()
        .init_resource::<Actions>()
        .add_systems(
            Startup,
            (
                setup_scene,
                setup_ui,
                load_demo_if_requested.after(setup_ui),
            ),
        )
        .add_systems(Update, (poll_picked_file, run_slice, run_export))
        .run();
}

/// Printer build volume `[x, y, z]` mm.
#[derive(Resource)]
struct Bed([f64; 3]);

/// The loaded part: the upload BYTES (never a Solid — !Send) + what the kernel derived from
/// them. Every slice/export rebuilds the Solid from `stl` and re-derives the SAME fit (the
/// rotation search is deterministic), so display and export can't drift apart.
#[derive(Resource, Default)]
struct Part {
    name: String,
    stl: Vec<u8>,
    plan: Option<Plan>,
}

/// The auto-plan in the ROTATED (display) frame; `rot` maps upload bytes into that frame.
struct Plan {
    rot: [f64; 12],
    min: [f64; 3],
    max: [f64; 3],
    cuts: Vec<(char, f64)>,
    connectors: Vec<Connector>,
}

/// Button → system handoff: observers set flags, Update systems do the heavy work.
#[derive(Resource, Default)]
struct Actions {
    slice: bool,
    export: bool,
}

/// The currently displayed model/pieces (despawned and replaced on load/slice).
#[derive(Component)]
struct LoadedModel;

/// Translucent cut-plane quads (despawned with the model).
#[derive(Component)]
struct CutPlane;

/// Status line in the panel.
#[derive(Component, Clone, Default)]
struct StatusLabel;

/// In-flight file pick: `None` payload = dialog cancelled. Single-flight.
#[derive(Resource, Default)]
struct PickTask(Option<Task<Option<(String, Vec<u8>)>>>);

/// `?demo` (web) / `--demo` (native): push the embedded sample through the EXACT upload path.
fn demo_requested() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        query_string().is_some_and(|q| q.contains("demo"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::args().any(|a| a == "--demo")
    }
}

/// `?bed=N` / `--bed=N`: cube-bed override in mm.
fn bed_override() -> Option<f64> {
    let arg: Option<String>;
    #[cfg(target_arch = "wasm32")]
    {
        arg = query_string();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        arg = std::env::args().find(|a| a.starts_with("--bed="));
    }
    let s = arg?;
    let tail = s.split("bed=").nth(1)?;
    tail.chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .parse()
        .ok()
}

#[cfg(target_arch = "wasm32")]
fn query_string() -> Option<String> {
    web_sys::window().and_then(|w| w.location().search().ok())
}

fn load_demo_if_requested(
    bed: Res<Bed>,
    part: ResMut<Part>,
    commands: Commands,
    meshes: ResMut<Assets<Mesh>>,
    mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    cams: Query<&mut Transform, With<Camera3d>>,
    labels: Query<&mut Text, With<StatusLabel>>,
) {
    if demo_requested() {
        present_model(
            "demo.stl",
            include_bytes!("../assets/demo.stl"),
            &bed,
            part,
            commands,
            meshes,
            mats,
            existing,
            cams,
            labels,
        );
    }
}

/// Bed plate + light + a Z-up camera framing the empty bed.
fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    bed: Res<Bed>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(bed.0[0] as f32, bed.0[1] as f32, 2.0))),
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.17, 0.20),
            perceptual_roughness: 0.9,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -1.0), // top face = the build plane z=0
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            ..default()
        },
        Transform::from_xyz(200.0, 300.0, 400.0).looking_at(Vec3::ZERO, Vec3::Z),
    ));
    // AmbientLight is per-camera in 0.19 — it rides the camera entity, not a resource.
    commands.spawn((
        Camera3d::default(),
        AmbientLight {
            brightness: 220.0,
            ..default()
        },
        frame_camera(Vec3::ZERO, bed.0[0].max(bed.0[1]) as f32),
    ));
}

/// Z-up orbit-style framing: fixed yaw/pitch, radius scaled to the content extent.
fn frame_camera(target: Vec3, extent: f32) -> Transform {
    let (yaw, pitch) = (-45f32.to_radians(), 30f32.to_radians());
    let r = (extent * 2.3).max(80.0);
    let eye = target
        + Vec3::new(
            r * pitch.cos() * yaw.cos(),
            r * pitch.cos() * yaw.sin(),
            r * pitch.sin(),
        );
    Transform::from_translation(eye).looking_at(target, Vec3::Z)
}

/// Top inset for the panel: the hosting page's chrome (back button etc.) overlays our top-left,
/// and the page knows its own chrome — it can declare the clearance on the canvas
/// (`<canvas id="fab-web" data-inset-top="44">`). Default clears a typical button row on web;
/// native has no page chrome.
fn ui_top_inset() -> f32 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("fab-web"))
            .and_then(|c| c.get_attribute("data-inset-top"))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(44.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        8.0
    }
}

/// Feathers panel: title, Open STL / Slice / Export buttons, status line.
fn setup_ui(world: &mut World) {
    use bevy::feathers::{
        controls::{ButtonVariant, FeathersButton},
        theme::{ThemeBackgroundColor, ThemedText},
        tokens,
    };
    use bevy::ui_widgets::Activate;

    let inset = ui_top_inset();
    let scene = bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(inset),
            left: px(8),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: UiRect::all(px(8)),
            min_width: px(240),
        }
        ThemeBackgroundColor(tokens::WINDOW_BG)
        Children [
            (Text("fab") ThemedText),
            (
                @FeathersButton { @variant: {ButtonVariant::Primary}, @caption: bsn!{ Text("Open STL") ThemedText } }
                on(|_: On<Activate>, mut task: ResMut<PickTask>| {
                    if task.0.is_some() {
                        return; // dialog already up
                    }
                    task.0 = Some(AsyncComputeTaskPool::get().spawn(async {
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("mesh", &["stl"])
                            .pick_file()
                            .await?;
                        let name = file.file_name();
                        let bytes = file.read().await;
                        Some((name, bytes))
                    }));
                })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Slice") ThemedText } }
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.slice = true; })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Export 3mf") ThemedText } }
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.export = true; })
            ),
            (Text("pick an STL to begin") ThemedText StatusLabel),
        ]
    };
    world.spawn_scene(scene).expect("spawn fab panel");
}

/// Drain the picker task and hand the bytes to [`present_model`].
#[allow(clippy::too_many_arguments)] // a system-params relay, not an API
fn poll_picked_file(
    mut task: ResMut<PickTask>,
    bed: Res<Bed>,
    part: ResMut<Part>,
    commands: Commands,
    meshes: ResMut<Assets<Mesh>>,
    mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    cams: Query<&mut Transform, With<Camera3d>>,
    labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = task.0.as_mut() else { return };
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    task.0 = None;
    let Some((name, bytes)) = done else { return }; // cancelled
    present_model(
        &name, &bytes, &bed, part, commands, meshes, mats, existing, cams, labels,
    );
}

/// The one load path: bytes → kernel plan (rotate-to-fit + auto cuts/onions) → display the model
/// in the ROTATED frame with its cut planes, seated on the bed. A soup that Manifold rejects
/// still displays (view-only) — slicing just stays off.
#[allow(clippy::too_many_arguments)] // a system-params relay, not an API
fn present_model(
    name: &str,
    bytes: &[u8],
    bed: &Bed,
    mut part: ResMut<Part>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    mut cams: Query<&mut Transform, With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };

    // Kernel plan first — when the mesh is sliceable we DISPLAY the rotated frame, so the
    // planes/pieces/export all agree with what's on screen.
    let (display_bytes, plan) = match Solid::from_stl_bytes(bytes) {
        Ok(solid) => {
            let fit = auto_slice::best_fit_rotation(&solid, bed.0);
            let rotated = solid.transform(&fit.rot);
            match auto::plan(&rotated, fit.min, fit.max, bed.0) {
                Ok(p) => {
                    info!(
                        "auto-plan: {} cuts, {} connectors",
                        p.cuts.len(),
                        p.connectors.len()
                    );
                    (
                        rotated.to_stl_bytes(),
                        Some(Plan {
                            rot: fit.rot,
                            min: fit.min,
                            max: fit.max,
                            cuts: p.cuts,
                            connectors: p.connectors,
                        }),
                    )
                }
                Err(e) => {
                    warn!("auto-plan failed: {e:#}");
                    (bytes.to_vec(), None)
                }
            }
        }
        Err(e) => {
            warn!("not sliceable ({e:#}) — view only");
            (bytes.to_vec(), None)
        }
    };

    let m = match stl::load_stl_bytes(&display_bytes) {
        Ok(m) => m,
        Err(e) => {
            status(format!("{name}: not a readable STL ({e:#})"));
            error!("parsing {name}: {e:#}");
            return;
        }
    };

    let (min, max) = aabb(&m);
    let size = max - min;
    let offset = Vec3::new(-(min.x + max.x) / 2.0, -(min.y + max.y) / 2.0, -min.z);
    for e in &existing {
        commands.entity(e).despawn();
    }
    commands.spawn((
        Mesh3d(meshes.add(build_mesh(&m))),
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::srgb(0.90, 0.74, 0.20),
            perceptual_roughness: 0.7,
            ..default()
        })),
        Transform::from_translation(offset), // seat: XY-center on the bed, Z-floor
        LoadedModel,
    ));
    if let Some(p) = &plan {
        spawn_cut_planes(&mut commands, &mut meshes, &mut mats, p, offset);
    }
    let extent = size.length().max(1.0);
    for mut cam in &mut cams {
        *cam = frame_camera(Vec3::new(0.0, 0.0, size.z / 2.0), extent);
    }

    let dims = format!("{:.0} x {:.0} x {:.0} mm", size.x, size.y, size.z);
    match &plan {
        Some(p) if p.cuts.is_empty() => status(format!("{name}: {dims} - fits the bed")),
        Some(p) => status(format!(
            "{name}: {dims} - {} cut(s), {} onion(s) planned",
            p.cuts.len(),
            p.connectors.len()
        )),
        None => status(format!("{name}: {dims} - view only (mesh not sliceable)")),
    }
    info!("loaded {name} ({} tris)", m.positions.len() / 3);

    part.name = name.to_string();
    part.stl = bytes.to_vec();
    part.plan = plan;
}

/// One translucent quad per planned cut, in display coordinates (plan frame + seat offset).
fn spawn_cut_planes(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mats: &mut Assets<StandardMaterial>,
    plan: &Plan,
    offset: Vec3,
) {
    let mat = mats.add(StandardMaterial {
        base_color: Color::srgba(0.25, 0.55, 0.95, 0.35),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        cull_mode: None,
        ..default()
    });
    let size = [
        (plan.max[0] - plan.min[0]) as f32,
        (plan.max[1] - plan.min[1]) as f32,
        (plan.max[2] - plan.min[2]) as f32,
    ];
    let mid = [
        ((plan.min[0] + plan.max[0]) / 2.0) as f32,
        ((plan.min[1] + plan.max[1]) / 2.0) as f32,
        ((plan.min[2] + plan.max[2]) / 2.0) as f32,
    ];
    const M: f32 = 6.0; // margin past the model so planes read as planes
    for &(axis, at) in &plan.cuts {
        let ai = match axis {
            'x' => 0,
            'y' => 1,
            _ => 2,
        };
        let mut dims = [size[0] + M, size[1] + M, size[2] + M];
        dims[ai] = 0.4;
        let mut pos = mid;
        pos[ai] = at as f32;
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(dims[0], dims[1], dims[2]))),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(Vec3::from_array(pos) + offset),
            CutPlane,
        ));
    }
}

/// Slice in-process and show the pieces fanned apart by slab index — auto onions included
/// (pegs proud on the lower piece, sockets carved from the upper).
fn run_slice(
    mut act: ResMut<Actions>,
    part: Res<Part>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    mut cams: Query<&mut Transform, With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    if !act.slice {
        return;
    }
    act.slice = false;
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    let Some(plan) = &part.plan else {
        status("nothing sliceable loaded".into());
        return;
    };
    if plan.cuts.is_empty() {
        status("fits the bed - nothing to cut".into());
        return;
    }
    let pieces = match slice_current(&part.stl, plan) {
        Ok(p) => p,
        Err(e) => {
            status(format!("slice failed: {e:#}"));
            error!("slice: {e:#}");
            return;
        }
    };

    for e in &existing {
        commands.entity(e).despawn();
    }
    let size = [
        (plan.max[0] - plan.min[0]) as f32,
        (plan.max[1] - plan.min[1]) as f32,
        (plan.max[2] - plan.min[2]) as f32,
    ];
    let spread = (size[0].max(size[1]).max(size[2]) * 0.18).max(8.0);
    let offset = Vec3::new(
        -((plan.min[0] + plan.max[0]) / 2.0) as f32,
        -((plan.min[1] + plan.max[1]) / 2.0) as f32,
        -plan.min[2] as f32,
    );
    let mat = mats.add(StandardMaterial {
        base_color: Color::srgb(0.90, 0.74, 0.20),
        perceptual_roughness: 0.7,
        ..default()
    });
    let n = pieces.len();
    for (idx, solid) in &pieces {
        let m = match stl::load_stl_bytes(&solid.to_stl_bytes()) {
            Ok(m) => m,
            Err(e) => {
                error!("piece mesh: {e:#}");
                continue;
            }
        };
        let fan = Vec3::new(
            idx[0] as f32 * spread,
            idx[1] as f32 * spread,
            idx[2] as f32 * spread,
        );
        commands.spawn((
            Mesh3d(meshes.add(build_mesh(&m))),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(offset + fan),
            LoadedModel,
        ));
    }
    let extent = (size[0].powi(2) + size[1].powi(2) + size[2].powi(2)).sqrt() + spread * 2.0;
    for mut cam in &mut cams {
        *cam = frame_camera(Vec3::new(0.0, 0.0, (size[2] / 2.0) + spread / 2.0), extent);
    }
    status(format!("{n} pieces - onions carried on the cut faces"));
    info!("sliced: {n} pieces");
}

/// Rebuild the Solid from the stored bytes, move it into the plan's frame, slice with the
/// stored cuts + connectors — display and geometry can't disagree because the SAME `rot`
/// produced both.
fn slice_current(stl_bytes: &[u8], plan: &Plan) -> anyhow::Result<Vec<([usize; 3], Solid)>> {
    let rotated = Solid::from_stl_bytes(stl_bytes)?.transform(&plan.rot);
    let spec = Slicing {
        printer: None,
        cut: plan
            .cuts
            .iter()
            .map(|&(ax, at)| Cut {
                axis: ax.to_string(),
                at: Num::Float(at),
            })
            .collect(),
        connector: plan.connectors.clone(),
        orient: vec![],
    };
    slicing::slice_solid(&spec, &rotated)
}

/// Export: the full `fab make` pipeline (fit → plan → orient → pack → Bambu 3mf) from the stored
/// bytes into memory, then a browser download / native file. Zero server-side outputs.
fn run_export(
    mut act: ResMut<Actions>,
    part: Res<Part>,
    bed: Res<Bed>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    if !act.export {
        return;
    }
    act.export = false;
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    if part.plan.is_none() {
        status("nothing sliceable loaded".into());
        return;
    }
    let out_name = format!(
        "{}-plates.3mf",
        part.name.strip_suffix(".stl").unwrap_or(&part.name)
    );
    let result = (|| -> anyhow::Result<(usize, usize, Vec<u8>)> {
        let solid = Solid::from_stl_bytes(&part.stl)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        let sum = auto::make_solid(solid, bed.0, &mut buf, GAP)?;
        Ok((sum.pieces, sum.plates, buf.into_inner()))
    })();
    match result {
        Ok((pieces, plates, bytes)) => match download_bytes(&out_name, &bytes) {
            Ok(()) => {
                status(format!("{out_name}: {pieces} pieces on {plates} plate(s)"));
                info!("exported {out_name} ({} bytes)", bytes.len());
            }
            Err(e) => status(format!("download failed: {e:#}")),
        },
        Err(e) => {
            status(format!("export failed: {e:#}"));
            error!("export: {e:#}");
        }
    }
}

/// Hand bytes to the user: a Blob download in the browser, a file beside the cwd natively.
#[cfg(target_arch = "wasm32")]
fn download_bytes(name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    use wasm_bindgen::JsCast;
    let err = |what: &str| anyhow::anyhow!("browser download: {what}");
    let array = js_sys::Array::new();
    array.push(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&array).map_err(|_| err("blob"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| err("url"))?;
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| err("document"))?;
    let a: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| err("anchor"))?
        .dyn_into()
        .map_err(|_| err("anchor cast"))?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    web_sys::Url::revoke_object_url(&url).ok();
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn download_bytes(name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(name, bytes)?;
    Ok(())
}

fn aabb(s: &stl::StlMesh) -> (Vec3, Vec3) {
    let mut min = Vec3::INFINITY;
    let mut max = Vec3::NEG_INFINITY;
    for p in &s.positions {
        let v = Vec3::from_array(*p);
        min = min.min(v);
        max = max.max(v);
    }
    (min, max)
}

fn build_mesh(s: &stl::StlMesh) -> Mesh {
    let n = s.positions.len() as u32;
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, s.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, s.normals.clone())
    .with_inserted_indices(Indices::U32((0..n).collect()))
}
