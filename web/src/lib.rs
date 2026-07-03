//! fab-web (Phase A): the browser slicer. A.1 scope — canvas-bound Bevy app, STL upload → view:
//! pick a file (rfd: native dialog on desktop, `<input type=file>` on wasm), parse the bytes,
//! seat the model on the bed plane and auto-frame the camera, Z-up like the desktop GUI.
//! The slicing pipeline (rotate-to-fit + auto::plan + 3mf export) wires in at A.2+ on the same
//! kernel the desktop uses. Runs native too (`cargo run -p fab-web`) for quick iteration.

use bevy::asset::RenderAssetUsages;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

mod stl;

/// Default build volume (mm) until printers.toml grows a browser home (A.2).
const BED: [f32; 3] = [256.0, 256.0, 256.0];

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    run();
}

pub fn run() {
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
        .init_resource::<PickTask>()
        .add_systems(
            Startup,
            (
                setup_scene,
                setup_ui,
                load_demo_if_requested.after(setup_ui),
            ),
        )
        .add_systems(Update, poll_picked_file)
        .run();
}

/// `?demo` (web) / `--demo` (native): push the embedded sample through the EXACT upload path —
/// the headless e2e drives this, and the site can link it as "try it without a file".
fn demo_requested() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .map(|q| q.contains("demo"))
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::args().any(|a| a == "--demo")
    }
}

fn load_demo_if_requested(
    commands: Commands,
    meshes: ResMut<Assets<Mesh>>,
    mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, With<LoadedModel>>,
    cams: Query<&mut Transform, With<Camera3d>>,
    labels: Query<&mut Text, With<StatusLabel>>,
) {
    if demo_requested() {
        present_model(
            "demo.stl",
            include_bytes!("../assets/demo.stl"),
            commands,
            meshes,
            mats,
            existing,
            cams,
            labels,
        );
    }
}

/// The currently loaded model (despawned and replaced on each upload).
#[derive(Component)]
struct LoadedModel;

/// Status line in the panel.
#[derive(Component, Clone, Default)]
struct StatusLabel;

/// In-flight file pick: `None` payload = dialog cancelled. Single-flight — the Open button
/// no-ops while a dialog is already up.
#[derive(Resource, Default)]
struct PickTask(Option<Task<Option<(String, Vec<u8>)>>>);

/// Bed plate + light + a Z-up camera framing the empty bed.
fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(BED[0], BED[1], 2.0))),
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
        frame_camera(Vec3::ZERO, BED[0].max(BED[1])),
    ));
}

/// Z-up orbit-style framing: fixed yaw/pitch, radius scaled to the content extent.
fn frame_camera(target: Vec3, extent: f32) -> Transform {
    let (yaw, pitch) = (-45f32.to_radians(), 30f32.to_radians());
    let r = (extent * 1.9).max(80.0);
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

/// Feathers panel: title, Open STL (kicks off the async picker), status line.
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
            min_width: px(220),
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
            (Text("pick an STL to begin") ThemedText StatusLabel),
        ]
    };
    world.spawn_scene(scene).expect("spawn fab panel");
}

/// Drain the picker task and hand the bytes to [`present_model`].
fn poll_picked_file(
    mut task: ResMut<PickTask>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, With<LoadedModel>>,
    mut cams: Query<&mut Transform, With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = task.0.as_mut() else { return };
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    task.0 = None;
    let Some((name, bytes)) = done else { return }; // cancelled
    present_model(
        &name, &bytes, commands, meshes, mats, existing, cams, labels,
    );
}

/// The one load path: bytes → mesh → replace the loaded model, seat it on the bed (Z-floor,
/// XY-centered — the desktop convention), reframe the camera, report in the panel. Picker and
/// demo mode both land here; A.2's slicing hooks in after this.
#[allow(clippy::too_many_arguments)] // a system-params relay, not an API
fn present_model(
    name: &str,
    bytes: &[u8],
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, With<LoadedModel>>,
    mut cams: Query<&mut Transform, With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    match stl::load_stl_bytes(bytes) {
        Ok(m) => {
            let (min, max) = aabb(&m);
            let size = max - min;
            let center = (min + max) / 2.0;
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
                // Seat: XY-center on the bed, Z-floor to the build plane.
                Transform::from_xyz(-center.x, -center.y, -min.z),
                LoadedModel,
            ));
            let extent = size.length().max(1.0);
            for mut cam in &mut cams {
                *cam = frame_camera(Vec3::new(0.0, 0.0, size.z / 2.0), extent);
            }
            status(format!(
                "{name}: {} tris, {:.0} x {:.0} x {:.0} mm",
                m.positions.len() / 3,
                size.x,
                size.y,
                size.z
            ));
            info!("loaded {name} ({} tris)", m.positions.len() / 3);
        }
        Err(e) => {
            status(format!("{name}: not a readable STL ({e:#})"));
            error!("parsing {name}: {e:#}");
        }
    }
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
