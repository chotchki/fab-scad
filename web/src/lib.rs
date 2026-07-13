//! fab-web: the browser slicer. Upload STL / colored 3MF / raw .scad → auto-plan against the
//! bed → edit connectors on the cut planes → sliced pieces → packed Bambu 3mf download, all
//! client-side. ALL geometry runs off the main thread (C.2): OpenSCAD renders in one worker,
//! the Manifold kernel (weld/plan/slice/export/section) in another — fab-geom, a kernel-only
//! ~1 MB wasm speaking `geomsg` bytes over postMessage. This crate holds NO kernel: Solids
//! live only inside the workers (the !Send contract, as designed). The busy pulse is LIVE for
//! everything now. Runs native too (`cargo run -p fab-web -- --demo --bed=40`) — same seam,
//! the service just runs on a pool thread.

use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::picking::hover::HoverMap;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, futures_lite::future};

use fab_scad::geomsg::{GeomObject, PlanOut, Request, Response, WireConn};
use fab_scad::stl;
use fab_scad::{auto, cross_section};

mod geom_worker;
#[cfg(target_arch = "wasm32")]
mod scad_worker;
#[cfg(target_arch = "wasm32")]
mod worker_rpc;

/// Printer presets the panel cycles through — the common fleet, not MY printer (C.3).
/// `?bed=N` / `--bed=N` still wins at startup (deep-links), localStorage remembers the pick.
const PRESETS: &[(&str, [f64; 3])] = &[
    ("A1 mini", [180.0, 180.0, 180.0]),
    ("P1/X1", [256.0, 256.0, 256.0]),
    ("MK4", [250.0, 210.0, 220.0]),
    ("Ender 3", [220.0, 220.0, 250.0]),
    ("Voron 350", [350.0, 350.0, 350.0]),
];
const DEFAULT_PRESET: usize = 1;
#[cfg(target_arch = "wasm32")]
const BED_STORE_KEY: &str = "fab-web.bed";
/// Plate gap for the packed export (mm) — matches `fab make`'s default.
const GAP: f64 = 5.0;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    run();
}

pub fn run() {
    let printer = startup_printer();
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
        use bevy::feathers::{FeathersPlugins, dark_theme::create_dark_theme, theme::UiTheme};
        app.add_plugins(FeathersPlugins)
            .insert_resource(UiTheme(create_dark_theme()));
    }

    app.insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(printer)
        .init_resource::<Part>()
        .init_resource::<PickTask>()
        .init_resource::<RenderTask>()
        .init_resource::<GeomCall>()
        .init_resource::<Actions>()
        .init_resource::<EditMode>()
        .init_resource::<DragGuard>()
        .init_resource::<Busy>()
        .add_systems(
            Startup,
            (
                setup_scene,
                setup_ui,
                load_demo_if_requested.after(setup_ui),
                seed_source_request,
            ),
        )
        .add_systems(
            Update,
            (
                poll_picked_file,
                poll_render_task,
                poll_geom,
                busy_pulse,
                run_slice,
                run_export,
                run_edit_actions,
                cycle_printer,
                draw_section,
                sync_edit_ui,
                orbit_input,
            ),
        )
        .run();
}

/// The selected printer: display name + build volume mm. Cycled by the panel button; a
/// `?bed=N` deep-link shows as "custom".
#[derive(Resource, Clone)]
struct Bed {
    name: String,
    dims: [f64; 3],
}

/// Startup order: deep-link param > localStorage > the fleet default. NOT my printer anymore.
fn startup_printer() -> Bed {
    if let Some(n) = bed_override() {
        if (10.0..=2000.0).contains(&n) {
            return Bed {
                name: "custom".into(),
                dims: [n, n, n],
            };
        }
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(b) = load_saved_bed() {
        return b;
    }
    let (name, dims) = PRESETS[DEFAULT_PRESET];
    Bed {
        name: name.into(),
        dims,
    }
}

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// Saved shape: `name|x|y|z` — no JSON dep for four fields.
#[cfg(target_arch = "wasm32")]
fn load_saved_bed() -> Option<Bed> {
    let raw = local_storage()?.get_item(BED_STORE_KEY).ok()??;
    let mut it = raw.split('|');
    let name = it.next()?.to_string();
    let mut dims = [0.0; 3];
    for d in dims.iter_mut() {
        *d = it.next()?.parse().ok()?;
    }
    (dims.iter().all(|d| (10.0..=2000.0).contains(d))).then_some(Bed { name, dims })
}

#[cfg(target_arch = "wasm32")]
fn save_bed(bed: &Bed) {
    if let Some(st) = local_storage() {
        let v = format!(
            "{}|{}|{}|{}",
            bed.name, bed.dims[0], bed.dims[1], bed.dims[2]
        );
        st.set_item(BED_STORE_KEY, &v).ok();
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn save_bed(_bed: &Bed) {}

/// The loaded part, exactly as the geometry service handed it back: per-object stl bytes in
/// the plan (rotated) frame + colors, and the plan the editor mutates. No Solids here — every
/// geometry op is a round-trip through the service.
#[derive(Resource, Default)]
struct Part {
    name: String,
    objects: Vec<GeomObject>,
    plan: Option<PlanOut>,
    /// The bytes Analyze last ran on (post-scad-render for .scad) — a printer change re-plans
    /// from these without re-picking (the reactive standard: no apply button).
    raw: Vec<u8>,
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

/// Button → system handoff: observers set flags, Update systems do the heavy work.
#[derive(Resource, Default)]
struct Actions {
    slice: bool,
    export: bool,
    done: bool,
    remove: bool,
    grow: bool,
    shrink: bool,
    cycle_printer: bool,
}

/// A.3: the connector-editor mode. `Cut` = editing one cut's join face IN PLACE — the section
/// profile + onion markers draw on the cut plane in 3D, clicks on the plane add/select, panel
/// buttons act on the selection. Entry is async now: the profile comes from the geometry
/// worker (Purpose::Section), so the mode flips when the loops arrive.
#[derive(Resource, Default)]
enum EditMode {
    #[default]
    Scene,
    Cut {
        cut: usize,
        /// Section profile (connector-pos coords), computed by the service on entry.
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

/// The printer button's caption — rewritten whenever the selection changes.
#[derive(Component, Clone, Default)]
struct PrinterLabel;

/// The bed plate mesh — resized when the printer changes.
#[derive(Component)]
struct BedPlate;

/// In-flight source load: `None` = dialog cancelled (silent); `Some(Err)` = a failure the
/// STATUS LINE must show (a 404'd worker script once failed silently — never again).
type PickResult = Option<Result<(String, Vec<u8>), String>>;

#[derive(Resource, Default)]
struct PickTask(Option<Task<PickResult>>);

/// A finished .scad → STL render: (source name, STL bytes) or the error string.
type RenderOut = Result<(String, Vec<u8>), String>;

/// The .scad → STL hop, split from PickTask so the panel can say WHOSE render is running.
#[derive(Resource, Default)]
struct RenderTask(Option<Task<RenderOut>>);

/// One in-flight geometry-service call + what to do with its answer. Single-flight: buttons
/// no-op (with a status note) while a call is out.
#[derive(Resource, Default)]
struct GeomCall {
    task: Option<Task<anyhow::Result<Response>>>,
    purpose: Purpose,
}

#[derive(Default, Clone)]
enum Purpose {
    #[default]
    Idle,
    Analyze {
        name: String,
        /// The analyzed bytes — committed to Part.raw only on SUCCESS, so a failed upload
        /// can't poison the printer re-plan of the still-displayed model (review finding).
        raw: Vec<u8>,
    },
    Slice,
    Export {
        out_name: String,
    },
    Section {
        cut: usize,
    },
}

/// The loading pulse (the desktop standard): while set, the status line animates `label |/-\`.
/// LIVE for everything since C.2 — all geometry is off the main thread.
#[derive(Resource, Default)]
struct Busy(Option<String>);

fn busy_pulse(busy: Res<Busy>, time: Res<Time>, mut labels: Query<&mut Text, With<StatusLabel>>) {
    let Some(label) = &busy.0 else { return };
    const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
    let c = FRAMES[(time.elapsed_secs() * 6.0) as usize % 4];
    for mut t in &mut labels {
        t.0 = format!("{label} {c}");
    }
}

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

/// Where the bundle's members live, as the PAGE declares it: `<canvas id="fab-web"
/// data-base="/3d/editor/<version>/">`. Defaults to document-relative (the reference-loader
/// layout). Real sites mount the bundle in a VERSIONED subdir while the document has a clean
/// URL — document-relative breaks there, which is exactly how beta found this.
#[cfg(target_arch = "wasm32")]
pub(crate) fn bundle_base() -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("fab-web"))
        .and_then(|c| c.get_attribute("data-base"))
        .map(|mut b| {
            if !b.ends_with('/') {
                b.push('/');
            }
            b
        })
        .unwrap_or_default()
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
/// deep-link half of showcase→slicer, and the perf harness's front door. Seeds [`PickTask`],
/// so it IS the upload path from there on.
fn seed_source_request(mut task: ResMut<PickTask>, mut busy: ResMut<Busy>) {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = &mut busy;
    #[cfg(target_arch = "wasm32")]
    if let Some(url) = param("stl") {
        info!("seeding from ?stl={url}");
        busy.0 = Some(format!("fetching {url}"));
        task.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
            let name = url.rsplit('/').next().unwrap_or(&url).to_string();
            match fetch_bytes(&url).await {
                Ok(bytes) => Some(Ok((name, bytes))),
                Err(e) => Some(Err(format!("fetching {url}: {e:#}"))),
            }
        }));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = std::env::args().find_map(|a| a.strip_prefix("--stl=").map(String::from)) {
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let bytes = std::fs::read(&path).ok();
        task.0 =
            Some(AsyncComputeTaskPool::get().spawn(async move { bytes.map(|b| Ok((name, b))) }));
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

/// The demo seeds the SAME pipeline as an upload — picker task → analyze → display.
fn load_demo_if_requested(mut task: ResMut<PickTask>, mut busy: ResMut<Busy>) {
    if demo_requested() && task.0.is_none() {
        busy.0 = Some("loading demo".into());
        task.0 = Some(AsyncComputeTaskPool::get().spawn(async {
            Some(Ok((
                "demo.stl".to_string(),
                include_bytes!("../assets/demo.stl").to_vec(),
            )))
        }));
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
        Mesh3d(meshes.add(Cuboid::new(bed.dims[0] as f32, bed.dims[1] as f32, 2.0))),
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.17, 0.20),
            perceptual_roughness: 0.9,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -1.0), // top face = the build plane z=0
        BedPlate,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            ..default()
        },
        Transform::from_xyz(200.0, 300.0, 400.0).looking_at(Vec3::ZERO, Vec3::Z),
    ));
    // AmbientLight is per-camera in 0.19 — it rides the camera entity, not a resource.
    let orbit = framed_orbit(Vec3::ZERO, bed.dims[0].max(bed.dims[1]) as f32);
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

/// Feathers panel: title, Open / Slice / Export buttons, edit-only rows, status line.
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
                @FeathersButton { @variant: {ButtonVariant::Primary}, @caption: bsn!{ Text("Open (stl / 3mf / scad)") ThemedText } }
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
                        Some(Ok((name, bytes)))
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
            (
                @FeathersButton { @caption: bsn!{ Text("Printer") ThemedText PrinterLabel } }
                on(|_: On<Activate>, mut act: ResMut<Actions>| { act.cycle_printer = true; })
            ),
            (Text("pick a model to begin") ThemedText StatusLabel),
        ]
    };
    world.spawn_scene(scene).expect("spawn fab panel");
}

/// Cycle the printer preset: update the bed, persist the pick, and — reactive standard, no
/// apply button — re-plan whatever's loaded through the service (live pulse). Also keeps the
/// button caption honest, including on the first frame.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not an API
fn cycle_printer(
    mut act: ResMut<Actions>,
    mut bed: ResMut<Bed>,
    mut geom: ResMut<GeomCall>,
    mut busy: ResMut<Busy>,
    part: Res<Part>,
    mut plabels: Query<&mut Text, (With<PrinterLabel>, Without<StatusLabel>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut plate: Query<&mut Mesh3d, With<BedPlate>>,
) {
    let clicked = act.cycle_printer;
    if clicked {
        if geom.task.is_some() {
            return; // leave the click queued — it applies the moment the current call drains
        }
        act.cycle_printer = false;
        let cur = PRESETS.iter().position(|(n, _)| *n == bed.name);
        let next = cur.map_or(0, |i| (i + 1) % PRESETS.len());
        let (name, dims) = PRESETS[next];
        bed.name = name.into();
        bed.dims = dims;
        save_bed(&bed);
        if !part.raw.is_empty() {
            busy.0 = Some(format!("re-planning for {name}"));
            spawn_geom(
                &mut geom,
                Purpose::Analyze {
                    name: part.name.clone(),
                    raw: part.raw.clone(),
                },
                Request::Analyze {
                    name: part.name.clone(),
                    bytes: part.raw.clone(),
                    bed: bed.dims,
                },
            );
        }
    }
    if clicked {
        for mut m in &mut plate {
            m.0 = meshes.add(Cuboid::new(bed.dims[0] as f32, bed.dims[1] as f32, 2.0));
        }
    }
    if clicked || bed.is_changed() || bed.is_added() {
        let d = bed.dims;
        let dims_txt = if d[0] == d[1] && d[1] == d[2] {
            format!("{:.0}", d[0])
        } else {
            format!("{:.0}x{:.0}x{:.0}", d[0], d[1], d[2])
        };
        for mut t in &mut plabels {
            t.0 = format!("Printer: {} ({dims_txt})", bed.name);
        }
    }
}

/// Kick a geometry-service call (single-flight; the caller set the busy label).
fn spawn_geom(geom: &mut GeomCall, purpose: Purpose, req: Request) {
    geom.purpose = purpose;
    geom.task = Some(AsyncComputeTaskPool::get().spawn(geom_worker::call(req)));
}

/// Drain the picker: .scad goes through the OpenSCAD worker first, everything else straight to
/// the geometry service for analysis.
fn poll_picked_file(
    mut task: ResMut<PickTask>,
    mut render: ResMut<RenderTask>,
    mut geom: ResMut<GeomCall>,
    mut busy: ResMut<Busy>,
    bed: Res<Bed>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = task.0.as_mut() else { return };
    if render.0.is_some() || geom.task.is_some() {
        return; // queue the pick — draining now would clobber the in-flight call (review HIGH)
    }
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    task.0 = None;
    busy.0 = None;
    let (name, bytes) = match done {
        None => return, // cancelled
        Some(Err(e)) => {
            error!("{e}");
            for mut t in &mut labels {
                t.0 = e.clone();
            }
            return;
        }
        Some(Ok(nb)) => nb,
    };
    if name.to_ascii_lowercase().ends_with(".scad") {
        #[cfg(target_arch = "wasm32")]
        {
            busy.0 = Some(format!("rendering {name} (OpenSCAD)"));
            let source = match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(e) => {
                    busy.0 = None;
                    for mut t in &mut labels {
                        t.0 = format!("{name}: not utf-8 scad ({e})");
                    }
                    return;
                }
            };
            render.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
                scad_worker::render(source)
                    .await
                    .map(|stl| (name.clone(), stl))
                    .map_err(|e| format!("{name}: {e:#}"))
            }));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = &mut render;
            for mut t in &mut labels {
                t.0 = format!("{name}: .scad rendering is web-only here (use fab-gui natively)");
            }
        }
        return;
    }
    busy.0 = Some(format!("analyzing {name}"));
    spawn_geom(
        &mut geom,
        Purpose::Analyze {
            name: name.clone(),
            raw: bytes.clone(),
        },
        Request::Analyze {
            name,
            bytes,
            bed: bed.dims,
        },
    );
}

/// Drain the OpenSCAD render hop: STL bytes go to the geometry service like any upload.
fn poll_render_task(
    mut render: ResMut<RenderTask>,
    mut geom: ResMut<GeomCall>,
    mut busy: ResMut<Busy>,
    bed: Res<Bed>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = render.0.as_mut() else { return };
    if geom.task.is_some() {
        return; // queue behind the in-flight geometry call (review HIGH)
    }
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    render.0 = None;
    match done {
        Ok((name, bytes)) => {
            busy.0 = Some(format!("analyzing {name}"));
            spawn_geom(
                &mut geom,
                Purpose::Analyze {
                    name: name.clone(),
                    raw: bytes.clone(),
                },
                Request::Analyze {
                    name,
                    bytes,
                    bed: bed.dims,
                },
            );
        }
        Err(e) => {
            busy.0 = None;
            error!("{e}");
            for mut t in &mut labels {
                t.0 = e.clone();
            }
        }
    }
}

/// Drain the geometry service and apply the answer per purpose. This is where every heavy
/// result lands: analyzed models, sliced pieces, packed exports, section profiles.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a system-params relay, not an API
fn poll_geom(
    mut geom: ResMut<GeomCall>,
    mut busy: ResMut<Busy>,
    mut mode: ResMut<EditMode>,
    mut part: ResMut<Part>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    mut cams: Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    let Some(t) = geom.task.as_mut() else { return };
    let Some(done) = block_on(future::poll_once(t)) else {
        return;
    };
    geom.task = None;
    busy.0 = None;
    let purpose = std::mem::take(&mut geom.purpose);
    let mut status = |s: String| {
        for mut t in &mut labels {
            t.0 = s.clone();
        }
    };
    let response = match done {
        Ok(r) => r,
        Err(e) => {
            error!("{e:#}");
            status(format!("{e:#}"));
            return;
        }
    };
    match (purpose, response) {
        (_, Response::Failed { error }) => {
            error!("{error}");
            status(error);
        }
        (
            Purpose::Analyze { name, raw },
            Response::Analyzed {
                objects,
                plan,
                tris,
            },
        ) => {
            info!("loaded {name} ({tris} tris)");
            if let Some(p) = &plan {
                info!(
                    "auto-plan: {} cuts, {} connectors",
                    p.cuts.len(),
                    p.connectors.len()
                );
            }
            present_display(
                &name,
                objects,
                plan,
                raw,
                &mut part,
                &mut mode,
                &mut commands,
                &mut meshes,
                &mut mats,
                &existing,
                &mut cams,
                &mut status,
            );
        }
        (Purpose::Slice, Response::Sliced { pieces }) => {
            let Some(plan) = &part.plan else { return };
            for e in &existing {
                commands.entity(e).despawn();
            }
            let size = plan_size(plan);
            let spread = (size[0].max(size[1]).max(size[2]) * 0.18).max(8.0);
            let offset = seat_offset(plan);
            let n = pieces.len();
            for p in &pieces {
                let m = match stl::load_stl_bytes(&p.stl) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("piece mesh: {e:#}");
                        continue;
                    }
                };
                let fan = Vec3::new(
                    p.idx[0] as f32 * spread,
                    p.idx[1] as f32 * spread,
                    p.idx[2] as f32 * spread,
                );
                commands.spawn((
                    Mesh3d(meshes.add(build_mesh(&m))),
                    MeshMaterial3d(mats.add(part_material(p.color))),
                    Transform::from_translation(offset + fan),
                    LoadedModel,
                ));
            }
            let extent =
                (size[0].powi(2) + size[1].powi(2) + size[2].powi(2)).sqrt() + spread * 2.0;
            for (mut t, mut o) in &mut cams {
                *o = framed_orbit(Vec3::new(0.0, 0.0, (size[2] / 2.0) + spread / 2.0), extent);
                *t = orbit_transform(&o);
            }
            if part.objects.len() > 1 {
                status(format!(
                    "{n} pieces - colors kept; connector preview off for assemblies (export carries them)"
                ));
            } else {
                status(format!("{n} pieces - onions carried on the cut faces"));
            }
            info!("sliced: {n} pieces");
        }
        (
            Purpose::Export { out_name },
            Response::Exported {
                threemf,
                pieces,
                plates,
            },
        ) => match download_bytes(&out_name, &threemf) {
            Ok(()) => {
                status(format!("{out_name}: {pieces} pieces on {plates} plate(s)"));
                info!("exported {out_name} ({} bytes)", threemf.len());
            }
            Err(e) => status(format!("download failed: {e:#}")),
        },
        (Purpose::Section { cut }, Response::Sectioned { loops }) => {
            let n = part
                .plan
                .as_ref()
                .map(|p| p.connectors.iter().filter(|c| c.cut == cut).count())
                .unwrap_or(0);
            status(format!(
                "editing cut {} - {n} onion(s); click the plane to add, an onion to select",
                cut + 1
            ));
            *mode = EditMode::Cut {
                cut,
                loops,
                selected: None,
            };
        }
        (p, _) => {
            // A mismatched pair means a logic bug, not a user problem — say so plainly.
            let which = match p {
                Purpose::Idle => "idle",
                Purpose::Analyze { .. } => "analyze",
                Purpose::Slice => "slice",
                Purpose::Export { .. } => "export",
                Purpose::Section { .. } => "section",
            };
            error!("geometry service replied out of order (purpose: {which})");
            status("internal: geometry reply mismatch".into());
        }
    }
}

/// Show an analyzed part: per-object meshes (colors kept), cut planes from the plan, camera
/// framed, status told. Pure display — all geometry already happened in the service.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a poll_geom helper, not an API
fn present_display(
    name: &str,
    objects: Vec<GeomObject>,
    plan: Option<PlanOut>,
    raw: Vec<u8>,
    part: &mut Part,
    mode: &mut EditMode,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mats: &mut Assets<StandardMaterial>,
    existing: &Query<Entity, Or<(With<LoadedModel>, With<CutPlane>)>>,
    cams: &mut Query<(&mut Transform, &mut Orbit), With<Camera3d>>,
    status: &mut dyn FnMut(String),
) {
    let soups: Vec<(stl::StlMesh, Option<[f32; 4]>)> = objects
        .iter()
        .filter_map(|o| stl::load_stl_bytes(&o.stl).ok().map(|m| (m, o.color)))
        .collect();
    if soups.is_empty() {
        status(format!("{name}: nothing displayable came back"));
        return;
    }
    let (min, max) = soups
        .iter()
        .map(|(m, _)| aabb(m))
        .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), (a, b)| {
            (lo.min(a), hi.max(b))
        });
    let size = max - min;
    let offset = Vec3::new(-(min.x + max.x) / 2.0, -(min.y + max.y) / 2.0, -min.z);
    for e in existing {
        commands.entity(e).despawn();
    }
    for (m, color) in &soups {
        commands.spawn((
            Mesh3d(meshes.add(build_mesh(m))),
            MeshMaterial3d(mats.add(part_material(*color))),
            Transform::from_translation(offset), // seat: XY-center on the bed, Z-floor
            LoadedModel,
        ));
    }
    if let Some(p) = &plan {
        spawn_cut_planes(commands, meshes, mats, p, offset);
    }
    let extent = size.length().max(1.0);
    for (mut t, mut o) in cams.iter_mut() {
        *o = framed_orbit(Vec3::new(0.0, 0.0, size.z / 2.0), extent);
        *t = orbit_transform(&o);
    }

    let parts_note = if objects.len() > 1 {
        format!("{} parts, ", objects.len())
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

    part.name = name.to_string();
    part.objects = objects;
    part.plan = plan;
    part.raw = raw;
    *mode = EditMode::Scene;
}

fn plan_size(plan: &PlanOut) -> [f32; 3] {
    [
        (plan.max[0] - plan.min[0]) as f32,
        (plan.max[1] - plan.min[1]) as f32,
        (plan.max[2] - plan.min[2]) as f32,
    ]
}

/// One translucent quad per planned cut, in display coordinates (plan frame + seat offset).
fn spawn_cut_planes(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mats: &mut Assets<StandardMaterial>,
    plan: &PlanOut,
    offset: Vec3,
) {
    let mat = mats.add(StandardMaterial {
        base_color: Color::srgba(0.25, 0.55, 0.95, 0.35),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        cull_mode: None,
        ..default()
    });
    let size = plan_size(plan);
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
fn seat_offset(plan: &PlanOut) -> Vec3 {
    Vec3::new(
        -((plan.min[0] + plan.max[0]) / 2.0) as f32,
        -((plan.min[1] + plan.max[1]) / 2.0) as f32,
        -plan.min[2] as f32,
    )
}

/// Cut axis index + the two non-axis dims in ascending order — the section's 2D basis, matching
/// BOTH the service's section convention and `WireConn.pos`.
fn cut_basis(axis: char) -> (usize, [usize; 2]) {
    match axis {
        'x' => (0, [1, 2]),
        'y' => (1, [0, 2]),
        _ => (2, [0, 1]),
    }
}

/// Clicking a cut plane: Scene mode → ask the service for the section (the editor opens when
/// the profile arrives). Already editing → the click is an ADD (sized by the same fit rule
/// auto-place uses) or a SELECT (near an existing onion).
#[allow(clippy::too_many_arguments)] // an observer relay, not an API
fn on_cut_plane_click(
    ev: On<Pointer<Click>>,
    planes: Query<&CutPlane>,
    guard: Res<DragGuard>,
    mut mode: ResMut<EditMode>,
    mut part: ResMut<Part>,
    mut geom: ResMut<GeomCall>,
    mut busy: ResMut<Busy>,
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
    let part = &mut *part;
    let Some(plan) = &mut part.plan else { return };
    let Some(&(axis, at)) = plan.cuts.get(ci) else {
        return;
    };
    let (ai, others) = cut_basis(axis);
    let rf = hit - seat_offset(plan); // display space → plan (rotated) frame
    let p2d = [rf[others[0]] as f64, rf[others[1]] as f64];

    let entering = !matches!(&*mode, EditMode::Cut { cut, .. } if *cut == ci);
    if entering {
        if geom.task.is_some() {
            status("still working - one moment".into());
            return;
        }
        busy.0 = Some(format!("sectioning cut {}", ci + 1));
        spawn_geom(
            &mut geom,
            Purpose::Section { cut: ci },
            Request::Section {
                objects: part.objects.clone(),
                axis: ai,
                at,
            },
        );
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
        let d = ((c.pos[0] - p2d[0]).powi(2) + (c.pos[1] - p2d[1]).powi(2)).sqrt();
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
            c.pos[0],
            c.pos[1],
            c.size.unwrap_or(10.0)
        ));
        return;
    }
    // Add: same sizing rule as auto-place (teardrop fit against the profile).
    if !cross_section::point_in_material(loops, p2d) {
        status("no material there - click inside the profile".into());
        return;
    }
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
    plan.connectors.push(WireConn {
        cut: ci,
        kind: "onion".to_string(),
        screw: None,
        pos: p2d,
        through: None,
        size: Some(d),
    });
    *selected = Some(plan.connectors.len() - 1);
    status(format!(
        "added onion d={d:.1}mm - {} on this cut",
        plan.connectors.iter().filter(|c| c.cut == ci).count()
    ));
}

/// Slice: ask the service, live pulse while it works. Multi-part (3mf assembly) cuts each part
/// separately so fragments keep colors — connector booleans only make sense against the whole
/// join, so the multi-part VIEW skips them (the export union still carries them).
fn run_slice(
    mut act: ResMut<Actions>,
    mut busy: ResMut<Busy>,
    mut mode: ResMut<EditMode>,
    mut geom: ResMut<GeomCall>,
    part: Res<Part>,
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
    if geom.task.is_some() {
        status("still working - one moment".into());
        return;
    }
    let Some(plan) = &part.plan else {
        status("nothing sliceable loaded".into());
        return;
    };
    if plan.cuts.is_empty() {
        status("fits the bed - nothing to cut".into());
        return;
    }
    *mode = EditMode::Scene;
    busy.0 = Some("slicing".into());
    spawn_geom(
        &mut geom,
        Purpose::Slice,
        Request::Slice {
            objects: part.objects.clone(),
            cuts: plan.cuts.clone(),
            connectors: plan.connectors.clone(),
            with_connectors: part.objects.len() == 1,
        },
    );
}

/// Export: the full make pipeline in the service, then a browser download. Zero server-side
/// outputs; user-edited connectors ride along (the service uses make_planned).
fn run_export(
    mut act: ResMut<Actions>,
    mut busy: ResMut<Busy>,
    mut geom: ResMut<GeomCall>,
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
    if geom.task.is_some() {
        status("still working - one moment".into());
        return;
    }
    let Some(plan) = &part.plan else {
        status("nothing sliceable loaded".into());
        return;
    };
    let stem = part
        .name
        .strip_suffix(".stl")
        .or_else(|| part.name.strip_suffix(".3mf"))
        .or_else(|| part.name.strip_suffix(".scad"))
        .unwrap_or(&part.name);
    busy.0 = Some("packing plates".into());
    spawn_geom(
        &mut geom,
        Purpose::Export {
            out_name: format!("{stem}-plates.3mf"),
        },
        Request::Export {
            objects: part.objects.clone(),
            cuts: plan.cuts.clone(),
            connectors: plan.connectors.clone(),
            bed: bed.dims,
            gap: GAP,
        },
    );
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
        let center = to_world(c.pos, 1.0);
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
    let max_fit = cross_section::fit_onion(
        loops,
        c.pos,
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
