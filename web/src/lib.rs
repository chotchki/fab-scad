//! fab-web (Phase A): the browser slicer. Upload an STL → the Manifold kernel plans it against
//! the bed (rotate-to-fit + auto cuts + auto onions, A.2), cut planes render on the model, Slice
//! shows the pieces, Export packs plates and downloads a Bambu 3mf (A.4) — all client-side, zero
//! server-side outputs. `Solid` is !Send by design: state holds the upload BYTES and every op
//! rebuilds the Solid where it runs — the same discipline a future worker split needs (A.8).
//! Runs native too (`cargo run -p fab-web -- --demo --bed=40`).

use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::picking::hover::HoverMap;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

use fab_scad::kernel::Solid;
use fab_scad::manifest::{Connector, Cut, Slicing};
use fab_scad::num::Num;
use fab_scad::{auto, auto_slice, cross_section, slicing};

use fab_scad::stl;

#[cfg(target_arch = "wasm32")]
mod scad_worker;

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
                seed_source_request,
            ),
        )
        .init_resource::<EditMode>()
        .init_resource::<DragGuard>()
        .add_systems(
            Update,
            (
                poll_picked_file,
                run_slice,
                run_export,
                run_edit_actions,
                draw_section,
                sync_edit_ui,
                orbit_input,
            ),
        )
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
    /// Per printable object: stl bytes ALREADY in the plan (rotated) frame + its 3mf color.
    /// A plain STL is one uncolored object; a 3mf assembly keeps its parts separate so the
    /// slice view can tint fragments by source part (A.9).
    objects: Vec<(Vec<u8>, Option<[f32; 4]>)>,
    plan: Option<Plan>,
}

/// One source object: triangle soup for display, a Solid when the geometry welds, 3mf color.
struct SourceObj {
    soup: stl::StlMesh,
    solid: Option<Solid>,
    color: Option<[f32; 4]>,
}

/// Extension-dispatched loader: STL = one uncolored object; 3MF = its built objects with
/// basematerial colors. `solid: None` = that object doesn't weld (the whole part goes view-only).
fn load_source(name: &str, bytes: &[u8]) -> anyhow::Result<Vec<SourceObj>> {
    if name.to_ascii_lowercase().ends_with(".3mf") {
        Ok(fab_scad::threemf_in::parse_3mf(bytes)?
            .into_iter()
            .map(|o| SourceObj {
                soup: soup_from_indexed(&o.verts, &o.tris),
                solid: Solid::from_indexed(&o.verts, &o.tris).ok(),
                color: o.color,
            })
            .collect())
    } else {
        Ok(vec![SourceObj {
            soup: stl::load_stl_bytes(bytes)?,
            solid: Solid::from_stl_bytes(bytes).ok(),
            color: None,
        }])
    }
}

/// Indexed mesh → flat soup with per-face normals (display only).
fn soup_from_indexed(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> stl::StlMesh {
    let mut positions = Vec::with_capacity(tris.len() * 3);
    let mut normals = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        let p: [Vec3; 3] = std::array::from_fn(|k| {
            let v = verts[t[k] as usize];
            Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32)
        });
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        for v in p {
            positions.push(v.to_array());
            normals.push(n.to_array());
        }
    }
    stl::StlMesh { positions, normals }
}

/// Rebuild the plan-frame union from the stored per-object bytes (they're already rotated).
fn rotated_union(objects: &[(Vec<u8>, Option<[f32; 4]>)]) -> anyhow::Result<Solid> {
    let solids: Vec<Solid> = objects
        .iter()
        .map(|(b, _)| Solid::from_stl_bytes(b))
        .collect::<anyhow::Result<_>>()?;
    Ok(match solids.len() {
        1 => solids.into_iter().next().expect("len checked"),
        _ => Solid::batch_union(&solids),
    })
}

/// The display material for an object: its 3mf color, else fab gold.
fn part_material(color: Option<[f32; 4]>) -> StandardMaterial {
    let c = color.map_or(Color::srgb(0.90, 0.74, 0.20), |c| {
        Color::srgb(c[0], c[1], c[2])
    });
    StandardMaterial {
        base_color: c,
        perceptual_roughness: 0.7,
        ..default()
    }
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
    done: bool,
    remove: bool,
    grow: bool,
    shrink: bool,
}

/// A.3: the connector-editor mode. `Cut` = editing one cut's join face IN PLACE — the section
/// profile + onion markers draw on the cut plane in 3D (no separate 2D view to port), clicks on
/// the plane add/select, panel buttons act on the selection.
#[derive(Resource, Default)]
enum EditMode {
    #[default]
    Scene,
    Cut {
        cut: usize,
        /// Cached section profile (connector-pos coords) — recomputed on entry.
        loops: Vec<Vec<[f64; 2]>>,
        /// Index into `Part.plan.connectors` (the GLOBAL list, not per-cut).
        selected: Option<usize>,
    },
}

/// Marker for panel rows that only apply while editing a cut.
#[derive(Component, Clone, Default)]
struct EditUi;

/// Z-up orbit camera state (B.7) — same grammar as the desktop GUI: left-drag orbit,
/// right-drag pan, wheel zoom; the whole gesture yields over the panel and while editing.
#[derive(Component, Clone, Copy)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
    target: Vec3,
}

/// Accumulated pointer travel for the CURRENT left-button gesture — a plane click that ends an
/// orbit drag must not enter the editor (Click fires on release regardless of travel).
#[derive(Resource, Default)]
struct DragGuard {
    moved: f32,
}

/// The currently displayed model/pieces (despawned and replaced on load/slice).
#[derive(Component)]
struct LoadedModel;

/// Translucent cut-plane quads (despawned with the model); payload = cut index into the plan.
#[derive(Component)]
struct CutPlane(usize);

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

/// .scad sources take one extra hop — the OpenSCAD worker renders them to STL bytes — then
/// join the normal pipeline (present_model sees STL bytes either way, the name stays honest).
/// Native fab-web has no worker; the desktop GUI is the native .scad front-end.
async fn maybe_render_scad(name: String, bytes: Vec<u8>) -> Option<(String, Vec<u8>)> {
    if !name.to_ascii_lowercase().ends_with(".scad") {
        return Some((name, bytes));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let source = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                error!("{name}: not utf-8 scad ({e})");
                return None;
            }
        };
        match scad_worker::render(source).await {
            Ok(stl) => Some((name, stl)),
            Err(e) => {
                error!("{name}: {e:#}");
                None
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        error!("{name}: .scad rendering is web-only here (use fab-gui natively)");
        None
    }
}

#[cfg(target_arch = "wasm32")]
fn query_string() -> Option<String> {
    web_sys::window().and_then(|w| w.location().search().ok())
}

#[cfg(target_arch = "wasm32")]
fn param(key: &str) -> Option<String> {
    let q = query_string()?;
    q.trim_start_matches('?').split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// `?stl=<same-origin url>` (web) / `--stl=<path>` (native): load without the picker — the
/// deep-link half of showcase→slicer (a project page hands its STL straight in), and the perf
/// harness's front door. Seeds [`PickTask`], so it IS the upload path from there on.
fn seed_source_request(mut task: ResMut<PickTask>) {
    #[cfg(target_arch = "wasm32")]
    if let Some(url) = param("stl") {
        info!("seeding from ?stl={url}");
        task.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
            let name = url.rsplit('/').next().unwrap_or(&url).to_string();
            match fetch_bytes(&url).await {
                Ok(bytes) => maybe_render_scad(name, bytes).await,
                Err(e) => {
                    error!("?stl fetch: {e:#}");
                    None
                }
            }
        }));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = std::env::args().find_map(|a| a.strip_prefix("--stl=").map(String::from)) {
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let bytes = std::fs::read(&path).ok();
        task.0 = Some(AsyncComputeTaskPool::get().spawn(async move { bytes.map(|b| (name, b)) }));
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let err = |w: &str| anyhow::anyhow!("fetch {w}");
    let win = web_sys::window().ok_or_else(|| err("window"))?;
    let resp = JsFuture::from(win.fetch_with_str(url))
        .await
        .map_err(|_| err("request"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| err("response"))?;
    if !resp.ok() {
        anyhow::bail!("fetch {url}: HTTP {}", resp.status());
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|_| err("body"))?)
        .await
        .map_err(|_| err("body await"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

fn load_demo_if_requested(
    mut mode: ResMut<EditMode>,
    bed: Res<Bed>,
    part: ResMut<Part>,
    commands: Commands,
    meshes: ResMut<Assets<Mesh>>,
    mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    cams: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    labels: Query<&mut Text, With<StatusLabel>>,
) {
    if demo_requested() {
        present_model(
            "demo.stl",
            include_bytes!("../assets/demo.stl"),
            &bed,
            part,
            &mut mode,
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
    let orbit = framed_orbit(Vec3::ZERO, bed.0[0].max(bed.0[1]) as f32);
    commands.spawn((
        Camera3d::default(),
        AmbientLight {
            brightness: 220.0,
            ..default()
        },
        orbit_transform(&orbit),
        orbit,
    ));
}

/// Auto-framing: the default view of `extent`-sized content at `target` (user orbits from here).
fn framed_orbit(target: Vec3, extent: f32) -> Orbit {
    Orbit {
        yaw: -45f32.to_radians(),
        pitch: 30f32.to_radians(),
        radius: (extent * 3.2).max(120.0),
        target,
    }
}

/// Z-up spherical camera placement from the orbit state.
fn orbit_transform(o: &Orbit) -> Transform {
    let eye = o.target
        + Vec3::new(
            o.radius * o.pitch.cos() * o.yaw.cos(),
            o.radius * o.pitch.cos() * o.yaw.sin(),
            o.radius * o.pitch.sin(),
        );
    Transform::from_translation(eye).looking_at(o.target, Vec3::Z)
}

/// The desktop's orbit, ported: left-drag orbit, right-drag pan, wheel zoom (Line one notch at
/// a time, Pixel trackpad streams scaled way down). Yields entirely while the pointer is over
/// the panel or a cut is being edited (the editor owns clicks; desktop does the same).
#[allow(clippy::too_many_arguments)] // an input relay, not an API
fn orbit_input(
    mut cam: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mode: Res<EditMode>,
    hover: Res<HoverMap>,
    ui_nodes: Query<(), With<Node>>,
    mut guard: ResMut<DragGuard>,
) {
    if buttons.just_pressed(MouseButton::Left) {
        guard.moved = 0.0;
    }
    let over_ui = hover
        .get(&PointerId::Mouse)
        .is_some_and(|hits| hits.keys().any(|e| ui_nodes.contains(*e)));
    if over_ui || matches!(&*mode, EditMode::Cut { .. }) {
        motion.clear();
        wheel.clear();
        return;
    }
    let Ok((mut t, mut o)) = cam.single_mut() else {
        return;
    };
    let right = t.rotation * Vec3::X;
    let up = t.rotation * Vec3::Y;
    if buttons.pressed(MouseButton::Left) {
        for ev in motion.read() {
            guard.moved += ev.delta.length();
            o.yaw -= ev.delta.x * 0.008;
            o.pitch = (o.pitch + ev.delta.y * 0.008).clamp(-1.5, 1.5);
        }
    } else if buttons.pressed(MouseButton::Right) {
        let scale = o.radius * 0.0015;
        for ev in motion.read() {
            o.target += (-right * ev.delta.x + up * ev.delta.y) * scale;
        }
    } else {
        motion.clear();
    }
    for ev in wheel.read() {
        let step = match ev.unit {
            MouseScrollUnit::Line => ev.y * 0.05,
            MouseScrollUnit::Pixel => ev.y * 0.004,
        };
        o.radius = (o.radius * (1.0 - step)).clamp(10.0, 4000.0);
    }
    *t = orbit_transform(&o);
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
            max_width: px(300),
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
                        #[cfg(target_arch = "wasm32")]
                        let filter: &[&str] = &["stl", "3mf", "scad"];
                        #[cfg(not(target_arch = "wasm32"))]
                        let filter: &[&str] = &["stl", "3mf"];
                        let file = rfd::AsyncFileDialog::new()
                            .add_filter("model", filter)
                            .pick_file()
                            .await?;
                        let name = file.file_name();
                        let bytes = file.read().await;
                        maybe_render_scad(name, bytes).await
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
            (
                @FeathersButton { @caption: bsn!{ Text("Done editing") ThemedText } } EditUi
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.done = true; })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Remove onion") ThemedText } } EditUi
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.remove = true; })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Bigger") ThemedText } } EditUi
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.grow = true; })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Smaller") ThemedText } } EditUi
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.shrink = true; })
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
    mut mode: ResMut<EditMode>,
    bed: Res<Bed>,
    part: ResMut<Part>,
    commands: Commands,
    meshes: ResMut<Assets<Mesh>>,
    mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    cams: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = task.0.as_mut() else { return };
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    task.0 = None;
    let Some((name, bytes)) = done else { return }; // cancelled
    present_model(
        &name, &bytes, &bed, part, &mut mode, commands, meshes, mats, existing, cams, labels,
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
    mode: &mut EditMode,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    mut cams: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    info!("presenting {name} ({} bytes)", bytes.len());

    let sources = match load_source(name, bytes) {
        Ok(o) => o,
        Err(e) => {
            status(format!("{name}: not readable ({e:#})"));
            error!("parsing {name}: {e:#}");
            return;
        }
    };
    let n_parts = sources.len();

    // Kernel plan on the UNION when every object welds — display in the rotated frame so the
    // planes/pieces/export all agree with what's on screen. Any non-manifold object degrades
    // the whole part to view-only (raw frame, no plan).
    let mut soups: Vec<(stl::StlMesh, Option<[f32; 4]>)> = Vec::new();
    let mut objects: Vec<(Vec<u8>, Option<[f32; 4]>)> = Vec::new();
    let mut plan: Option<Plan> = None;
    let mut solids: Vec<Solid> = Vec::new();
    let mut all_solid = true;
    for o in sources {
        if let Some(sol) = o.solid {
            solids.push(sol);
        } else {
            all_solid = false;
        }
        soups.push((o.soup, o.color));
    }
    if all_solid {
        let union = match solids.len() {
            1 => solids[0].transform(&[1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.]),
            _ => Solid::batch_union(&solids),
        };
        let fit = auto_slice::best_fit_rotation(&union, bed.0);
        match auto::plan(&union.transform(&fit.rot), fit.min, fit.max, bed.0) {
            Ok(p) => {
                info!(
                    "auto-plan: {} cuts, {} connectors",
                    p.cuts.len(),
                    p.connectors.len()
                );
                // Rotate every object into the plan frame; display + slice + export share it.
                objects = solids
                    .iter()
                    .zip(soups.iter())
                    .map(|(s, (_, c))| (s.transform(&fit.rot).to_stl_bytes(), *c))
                    .collect();
                soups = objects
                    .iter()
                    .filter_map(|(b, c)| stl::load_stl_bytes(b).ok().map(|m| (m, *c)))
                    .collect();
                plan = Some(Plan {
                    rot: fit.rot,
                    min: fit.min,
                    max: fit.max,
                    cuts: p.cuts,
                    connectors: p.connectors,
                });
            }
            Err(e) => warn!("auto-plan failed: {e:#}"),
        }
    } else {
        warn!("not sliceable — view only");
    }

    let (min, max) = soups
        .iter()
        .map(|(m, _)| aabb(m))
        .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), (a, b)| {
            (lo.min(a), hi.max(b))
        });
    let size = max - min;
    let offset = Vec3::new(-(min.x + max.x) / 2.0, -(min.y + max.y) / 2.0, -min.z);
    for e in &existing {
        commands.entity(e).despawn();
    }
    let mut tris = 0usize;
    for (m, color) in &soups {
        tris += m.positions.len() / 3;
        commands.spawn((
            Mesh3d(meshes.add(build_mesh(m))),
            MeshMaterial3d(mats.add(part_material(*color))),
            Transform::from_translation(offset), // seat: XY-center on the bed, Z-floor
            LoadedModel,
        ));
    }
    if let Some(p) = &plan {
        spawn_cut_planes(&mut commands, &mut meshes, &mut mats, p, offset);
    }
    let extent = size.length().max(1.0);
    for (mut t, mut o) in &mut cams {
        *o = framed_orbit(Vec3::new(0.0, 0.0, size.z / 2.0), extent);
        *t = orbit_transform(&o);
    }

    let parts_note = if n_parts > 1 {
        format!("{n_parts} parts, ")
    } else {
        String::new()
    };
    let dims = format!(
        "{parts_note}{:.0} x {:.0} x {:.0} mm",
        size.x, size.y, size.z
    );
    match &plan {
        Some(p) if p.cuts.is_empty() => status(format!("{name}: {dims} - fits the bed")),
        Some(p) => status(format!(
            "{name}: {dims} - {} cut(s), {} onion(s) planned",
            p.cuts.len(),
            p.connectors.len()
        )),
        None => status(format!("{name}: {dims} - view only (mesh not sliceable)")),
    }
    info!("loaded {name} ({tris} tris)");

    part.name = name.to_string();
    part.stl = bytes.to_vec();
    part.objects = objects;
    part.plan = plan;
    *mode = EditMode::Scene;
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
    for (ci, &(axis, at)) in plan.cuts.iter().enumerate() {
        let ai = match axis {
            'x' => 0,
            'y' => 1,
            _ => 2,
        };
        let mut dims = [size[0] + M, size[1] + M, size[2] + M];
        dims[ai] = 0.4;
        let mut pos = mid;
        pos[ai] = at as f32;
        commands
            .spawn((
                Mesh3d(meshes.add(Cuboid::new(dims[0], dims[1], dims[2]))),
                MeshMaterial3d(mat.clone()),
                Transform::from_translation(Vec3::from_array(pos) + offset),
                CutPlane(ci),
            ))
            .observe(on_cut_plane_click);
    }
}

/// The seat translation the DISPLAY applies to plan-frame geometry (XY-center + Z-floor).
fn seat_offset(plan: &Plan) -> Vec3 {
    Vec3::new(
        -((plan.min[0] + plan.max[0]) / 2.0) as f32,
        -((plan.min[1] + plan.max[1]) / 2.0) as f32,
        -plan.min[2] as f32,
    )
}

/// Cut axis index + the two non-axis dims in ascending order — the section's 2D basis, matching
/// BOTH `Solid::cross_section`'s output convention and `Connector.pos`.
fn cut_basis(axis: char) -> (usize, [usize; 2]) {
    match axis {
        'x' => (0, [1, 2]),
        'y' => (1, [0, 2]),
        _ => (2, [0, 1]),
    }
}

/// Clicking a cut plane: Scene mode → enter the editor for that cut. Already editing → the click
/// is an ADD (empty spot, sized by the same fit rule auto-place uses) or a SELECT (near an
/// existing onion). Uses the pick's world-space hit mapped into section coords.
fn on_cut_plane_click(
    ev: On<Pointer<Click>>,
    planes: Query<&CutPlane>,
    guard: Res<DragGuard>,
    mut mode: ResMut<EditMode>,
    mut part: ResMut<Part>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Ok(&CutPlane(ci)) = planes.get(ev.entity) else {
        return;
    };
    if guard.moved > 8.0 {
        return; // that release ended an orbit drag, not a click
    }
    let Some(hit) = ev.event.hit.position else {
        return;
    };
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    let part = &mut *part; // split field borrows (plan &mut, stl &)
    let Some(plan) = &mut part.plan else { return };
    let Some(&(axis, _at)) = plan.cuts.get(ci) else {
        return;
    };
    let (_, others) = cut_basis(axis);
    let rf = hit - seat_offset(plan); // display space → plan (rotated) frame
    let p2d = [rf[others[0]] as f64, rf[others[1]] as f64];

    let entering = !matches!(&*mode, EditMode::Cut { cut, .. } if *cut == ci);
    if entering {
        let loops = match rotated_union(&part.objects) {
            Ok(union) => {
                let (_, at) = plan.cuts[ci];
                union.cross_section(cut_basis(plan.cuts[ci].0).0, at)
            }
            Err(e) => {
                error!("section: {e:#}");
                return;
            }
        };
        let n = plan.connectors.iter().filter(|c| c.cut == ci).count();
        status(format!(
            "editing cut {} - {n} onion(s); click the plane to add, an onion to select",
            ci + 1
        ));
        *mode = EditMode::Cut {
            cut: ci,
            loops,
            selected: None,
        };
        return;
    }

    // Same cut, already editing: select-or-add.
    let EditMode::Cut {
        loops, selected, ..
    } = &mut *mode
    else {
        return;
    };
    // Select: nearest connector on this cut whose disc covers the click (min 4mm halo).
    let mut best: Option<(usize, f64)> = None;
    for (gi, c) in plan.connectors.iter().enumerate() {
        if c.cut != ci {
            continue;
        }
        let d = ((c.pos[0].f() - p2d[0]).powi(2) + (c.pos[1].f() - p2d[1]).powi(2)).sqrt();
        let halo = (c.size.unwrap_or(10.0) / 2.0).max(4.0);
        if d <= halo && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((gi, d));
        }
    }
    if let Some((gi, _)) = best {
        *selected = Some(gi);
        let c = &plan.connectors[gi];
        status(format!(
            "selected onion at ({:.0}, {:.0}), d={:.1}mm - Remove / Bigger / Smaller",
            c.pos[0].f(),
            c.pos[1].f(),
            c.size.unwrap_or(10.0)
        ));
        return;
    }
    // Add: same sizing rule as auto-place (teardrop fit against the profile).
    if !cross_section::point_in_material(loops, p2d) {
        status("no material there - click inside the profile".into());
        return;
    }
    let (ai, _) = cut_basis(axis);
    let d = cross_section::fit_onion(
        loops,
        p2d,
        auto::ONION_WALL,
        auto::ONION_MAX_D,
        auto::cap_dir(ai),
        auto::ONION_TIP,
    );
    if d < auto::MIN_ONION {
        status(format!("too tight here (fit {d:.1}mm) - pick an open spot"));
        return;
    }
    plan.connectors.push(Connector {
        cut: ci,
        kind: "onion".to_string(),
        screw: None,
        pos: [Num::Float(p2d[0]), Num::Float(p2d[1])],
        through: None,
        size: Some(d),
    });
    *selected = Some(plan.connectors.len() - 1);
    status(format!(
        "added onion d={d:.1}mm - {} on this cut",
        plan.connectors.iter().filter(|c| c.cut == ci).count()
    ));
}

/// Slice in-process and show the pieces fanned apart by slab index — auto onions included
/// (pegs proud on the lower piece, sockets carved from the upper).
fn run_slice(
    mut act: ResMut<Actions>,
    mut mode: ResMut<EditMode>,
    part: Res<Part>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    mut cams: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    if !act.slice {
        return;
    }
    act.slice = false;
    *mode = EditMode::Scene;
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
    // Multi-part (3mf assembly): cut each part separately so fragments keep their colors.
    // Connector booleans only make sense against the whole join, so the multi-part VIEW skips
    // them — the export (union) still carries them.
    let multi = part.objects.len() > 1;
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
        connector: if multi {
            vec![]
        } else {
            plan.connectors.clone()
        },
        orient: vec![],
    };
    let mut pieces: Vec<([usize; 3], Solid, Option<[f32; 4]>)> = Vec::new();
    for (obytes, color) in &part.objects {
        let sliced =
            Solid::from_stl_bytes(obytes).and_then(|sol| slicing::slice_solid(&spec, &sol));
        match sliced {
            Ok(ps) => pieces.extend(ps.into_iter().map(|(i, sol)| (i, sol, *color))),
            Err(e) => {
                status(format!("slice failed: {e:#}"));
                error!("slice: {e:#}");
                return;
            }
        }
    }

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
    let n = pieces.len();
    for (idx, solid, color) in &pieces {
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
            MeshMaterial3d(mats.add(part_material(*color))),
            Transform::from_translation(offset + fan),
            LoadedModel,
        ));
    }
    let extent = (size[0].powi(2) + size[1].powi(2) + size[2].powi(2)).sqrt() + spread * 2.0;
    for (mut t, mut o) in &mut cams {
        *o = framed_orbit(Vec3::new(0.0, 0.0, (size[2] / 2.0) + spread / 2.0), extent);
        *t = orbit_transform(&o);
    }
    if multi {
        status(format!(
            "{n} pieces - colors kept; connector preview off for assemblies (export carries them)"
        ));
    } else {
        status(format!("{n} pieces - onions carried on the cut faces"));
    }
    info!("sliced: {n} pieces");
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
    let stem = part
        .name
        .strip_suffix(".stl")
        .or_else(|| part.name.strip_suffix(".3mf"))
        .unwrap_or(&part.name);
    let out_name = format!("{stem}-plates.3mf");
    let result = (|| -> anyhow::Result<(usize, usize, Vec<u8>)> {
        // make_planned, not make_solid: the user's edited connectors must survive the export.
        // The union loses per-part colors — the extruder-mapping follow-up owns that.
        let plan = part.plan.as_ref().expect("checked above");
        let rotated = rotated_union(&part.objects)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        let sum = auto::make_planned(
            rotated,
            &plan.cuts,
            plan.connectors.clone(),
            bed.0,
            &mut buf,
            GAP,
        )?;
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

/// Immediate-mode overlay while editing a cut: profile loops + one circle per onion on the
/// plane (selected = orange). Gizmos redraw per frame; nothing to despawn on exit.
fn draw_section(mode: Res<EditMode>, part: Res<Part>, mut gizmos: Gizmos) {
    let EditMode::Cut {
        cut,
        loops,
        selected,
    } = &*mode
    else {
        return;
    };
    let Some(plan) = &part.plan else { return };
    let Some(&(axis, at)) = plan.cuts.get(*cut) else {
        return;
    };
    let (ai, others) = cut_basis(axis);
    let offset = seat_offset(plan);
    // Section 2D → display 3D, nudged off the plane so lines beat the quad's depth.
    let lift = 0.6;
    let to_world = |p: [f64; 2], side: f32| {
        let mut v = [0.0f32; 3];
        v[ai] = at as f32 + side * lift;
        v[others[0]] = p[0] as f32;
        v[others[1]] = p[1] as f32;
        Vec3::from_array(v) + offset
    };
    for lp in loops {
        if lp.len() < 2 {
            continue;
        }
        let mut pts: Vec<Vec3> = lp.iter().map(|&p| to_world(p, 1.0)).collect();
        pts.push(pts[0]);
        gizmos.linestrip(pts.clone(), Color::srgb(0.95, 0.95, 0.98));
        for p in &mut pts {
            *p -= Vec3::from_array({
                let mut n = [0.0f32; 3];
                n[ai] = 2.0 * lift;
                n
            });
        }
        gizmos.linestrip(pts, Color::srgb(0.95, 0.95, 0.98));
    }
    let mut normal = [0.0f32; 3];
    normal[ai] = 1.0;
    let normal = Dir3::new(Vec3::from_array(normal)).unwrap();
    for (gi, c) in plan.connectors.iter().enumerate() {
        if c.cut != *cut {
            continue;
        }
        let center = to_world([c.pos[0].f(), c.pos[1].f()], 1.0);
        let color = if Some(gi) == *selected {
            Color::srgb(0.95, 0.55, 0.15)
        } else {
            Color::srgb(0.25, 0.85, 0.45)
        };
        let iso = Isometry3d::new(center, Quat::from_rotation_arc(Vec3::Z, *normal));
        gizmos.circle(iso, (c.size.unwrap_or(10.0) / 2.0) as f32, color);
        gizmos.circle(iso, 1.2, color);
    }
}

/// Apply the edit buttons to the selection; sizes clamp to the same fit rule that placed them.
fn run_edit_actions(
    mut act: ResMut<Actions>,
    mut mode: ResMut<EditMode>,
    mut part: ResMut<Part>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let (done, remove, grow, shrink) = (act.done, act.remove, act.grow, act.shrink);
    if !(done || remove || grow || shrink) {
        return;
    }
    act.done = false;
    act.remove = false;
    act.grow = false;
    act.shrink = false;
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    if done {
        *mode = EditMode::Scene;
        if let Some(plan) = &part.plan {
            status(format!(
                "{} cut(s), {} onion(s) - Slice / Export when ready",
                plan.cuts.len(),
                plan.connectors.len()
            ));
        }
        return;
    }
    let EditMode::Cut {
        cut,
        loops,
        selected,
    } = &mut *mode
    else {
        return;
    };
    let Some(plan) = &mut part.plan else { return };
    let Some(gi) = *selected else {
        status("nothing selected - click an onion first".into());
        return;
    };
    if remove {
        plan.connectors.remove(gi);
        *selected = None;
        status(format!(
            "removed - {} onion(s) left on this cut",
            plan.connectors.iter().filter(|c| c.cut == *cut).count()
        ));
        return;
    }
    let Some(&(axis, _)) = plan.cuts.get(*cut) else {
        return;
    };
    let (ai, _) = cut_basis(axis);
    let c = &mut plan.connectors[gi];
    let p2d = [c.pos[0].f(), c.pos[1].f()];
    let max_fit = cross_section::fit_onion(
        loops,
        p2d,
        auto::ONION_WALL,
        auto::ONION_MAX_D,
        auto::cap_dir(ai),
        auto::ONION_TIP,
    );
    let cur = c.size.unwrap_or(10.0);
    let next = if grow { cur + 1.0 } else { cur - 1.0 }
        .clamp(auto::MIN_ONION, max_fit.max(auto::MIN_ONION));
    c.size = Some(next);
    status(format!("onion d={next:.1}mm (fit caps at {max_fit:.1})"));
}

/// Show the edit-only buttons exactly while a cut is being edited — and hide the MODEL while
/// editing: it occludes the cut plane (both visually and for picking), and the section overlay
/// is the editing surface. The desktop uses a separate 2D view for the same reason.
fn sync_edit_ui(
    mode: Res<EditMode>,
    mut rows: Query<&mut Node, With<EditUi>>,
    mut model: Query<&mut Visibility, With<LoadedModel>>,
) {
    let editing = matches!(&*mode, EditMode::Cut { .. });
    for mut node in &mut rows {
        node.display = if editing {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut vis in &mut model {
        *vis = if editing {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
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
