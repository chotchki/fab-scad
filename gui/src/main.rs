//! fab-gui — the slicing GUI (Phase 5.1). A Bevy 0.19 viewport over a model, with the printer
//! bed for reference and a Feathers control panel. A STACK of cut planes (each draggable in 3D
//! and toggleable on/off) drives `fab` in-process (the shared `fab_scad` lib) ON A BACKGROUND
//! THREAD; Re-slice swaps in the result. The cut stack is the unit a DAG cache will key on:
//! a slice is a pure function of (source, enabled cuts). Modes:
//!
//!   cargo run -p fab-gui -- part.scad                       # windowed: orbit, drag cuts, Re-slice
//!   cargo run -p fab-gui -- part.scad --screenshot out.png  # headless render to PNG (self-verify)
//!   cargo run -p fab-gui -- part.scad --script "addcut 30; reslice; shot a.png"  # scripted harness

use std::path::{Path, PathBuf};

use bevy::{
    app::ScheduleRunnerPlugin,
    asset::{AssetPlugin, RenderAssetUsages},
    camera::RenderTarget,
    feathers::{
        controls::{ButtonVariant, FeathersButton, FeathersListRow, FeathersListView},
        dark_theme::create_dark_theme,
        theme::{ThemeBackgroundColor, ThemedText, UiTheme},
        tokens, FeathersPlugins,
    },
    image::Image,
    input::mouse::{MouseMotion, MouseWheel},
    mesh::Indices,
    picking::{
        events::{Click, Drag, DragEnd, DragStart, Pointer},
        mesh_picking::MeshPickingPlugin,
        pointer::PointerButton,
    },
    prelude::*,
    render::{
        render_resource::{PrimitiveTopology, TextureFormat, TextureUsages},
        view::screenshot::{save_to_disk, Screenshot},
    },
    scene::{Scene, SceneList}, // the bsn traits — shadow the prelude's `Scene` asset struct
    tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task},
    text::{FontSize, FontSource, TextFont},
    ui_widgets::Activate,
    window::ExitCondition,
    winit::WinitPlugin,
};

mod fab;
mod stl;

const SPREAD: f64 = 50.0;

/// Scene inputs shared by both modes.
#[derive(Resource, Clone)]
struct SceneCfg {
    source: Option<PathBuf>, // .scad source (sliceable, preferred)
    stl: Option<PathBuf>,    // .stl to display directly (when there's no source)
    bed: [f32; 2],
    root: Option<PathBuf>, // workspace root, for OPENSCADPATH
    tmp: PathBuf,          // scratch dir for rendered/sliced STLs
    reslice_on_start: bool, // screenshot --reslice: display the sliced result
    cut_pct: f32,          // screenshot --cut <0..100>: where along X to cut
}

/// Marks the displayed model entity, so re-slice can swap it out.
#[derive(Component)]
struct Model;

/// Button → "re-slice the source and swap the mesh".
#[derive(Message)]
struct ReSlice;

/// The in-flight render/slice (off the main thread): `(was_reslice, task)`. The task yields
/// `Ok(stl)` when done, else an error string.
#[derive(Resource, Default)]
struct Job(Option<(bool, Task<Result<PathBuf, String>>)>);

/// One-line status shown in the panel (e.g. "slicing", "ready").
#[derive(Resource)]
struct Status(String);

/// The axis a cut plane is normal to (which way it slices).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Axis {
    #[default]
    X,
    Y,
    Z,
}

impl Axis {
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
    fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Axis::X => "X",
            Axis::Y => "Y",
            Axis::Z => "Z",
        }
    }
    /// The slicer's axis letter.
    fn scad(self) -> char {
        match self {
            Axis::X => 'x',
            Axis::Y => 'y',
            Axis::Z => 'z',
        }
    }
}

/// One planar cut: which axis it's normal to, its position along that axis, and whether it's in
/// the slice.
#[derive(Clone)]
struct CutDef {
    axis: Axis,
    at: f32,
    enabled: bool,
}

/// The ordered cut stack + which cut the drag edits. A slice is a pure function of
/// (source, enabled cuts) — the node a DAG cache will key on.
#[derive(Resource, Default)]
struct Cuts {
    list: Vec<CutDef>,
    active: usize,
}

impl Cuts {
    /// Enabled cuts as `(axis letter, position)`, the input to `fab::reslice`.
    fn enabled_cuts(&self) -> Vec<(char, f64)> {
        self.list.iter().filter(|c| c.enabled).map(|c| (c.axis.scad(), c.at as f64)).collect()
    }

    fn active_axis(&self) -> Axis {
        self.list.get(self.active).map(|c| c.axis).unwrap_or(Axis::X)
    }
}

/// The X/Y/Z component of `v`.
fn comp(v: Vec3, i: usize) -> f32 {
    match i {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

/// `v` with component `i` set to `val`.
fn with_comp(mut v: Vec3, i: usize, val: f32) -> Vec3 {
    match i {
        0 => v.x = val,
        1 => v.y = val,
        _ => v.z = val,
    }
    v
}

/// The whole model's AABB (min, max), set once on the first render — maps drag/positions.
#[derive(Resource, Default)]
struct ModelBounds(Option<(Vec3, Vec3)>);

/// True while a cut plane is being dragged, so the camera orbit yields to it.
#[derive(Resource, Default)]
struct DraggingCut(bool);

/// The uncut model's mesh, kept so editing can revert from the exploded view without re-rendering.
#[derive(Resource, Default)]
struct WholeMesh(Option<Handle<Mesh>>);

/// Spread applied to the currently-displayed mesh: 0 = uncut (editing), >0 = exploded result.
/// Overlays track it: at `cut.at` when 0, fanned into the piece gaps when >0.
#[derive(Resource, Default)]
struct DisplaySpread(f32);

/// The last sliced (exploded) mesh, so the view toggle can re-show it without re-slicing.
#[derive(Resource, Default)]
struct SlicedMesh(Option<Handle<Mesh>>);

/// Set when cuts change after the last slice — so Explode knows to re-slice first.
#[derive(Resource, Default)]
struct SliceDirty(bool);

/// The bundled Material Icons font (gui/assets/fonts), for button glyphs (trash, etc.).
#[derive(Resource)]
struct IconFont(Handle<Font>);

/// Material Icons codepoints used on buttons.
const ICON_DELETE: &str = "\u{e872}"; // trash can
const ICON_ADD: &str = "\u{e145}"; // plus
const ICON_ON: &str = "\u{e8f4}"; // eye (visible)
const ICON_OFF: &str = "\u{e8f5}"; // eye-off (hidden)

/// Marks the panel's status text so a system can update it.
#[derive(Component, Clone, Default)]
struct StatusLabel;
/// The Explode/Collapse view-toggle button and its caption (relabelled to match the state).
#[derive(Component, Clone, Default)]
struct ViewToggleButton;
#[derive(Component, Clone, Default)]
struct ViewToggleLabel;
/// The whole panel's root entity — despawned + rebuilt when the cut structure changes.
#[derive(Component, Clone, Default)]
struct PanelRoot;
/// A cut row in a plane card, tagged with the cut index it shows (for targeted text updates).
#[derive(Component, Clone, Default, Reflect)]
#[reflect(Component)]
struct RowFor(usize);
/// Marks a button caption that should render in the bundled icon font.
#[derive(Component, Clone, Default)]
struct IconText;
/// Set once the icon font has been applied to an IconText caption.
#[derive(Component)]
struct IconApplied;

/// The cut stack's structural signature (axis + enabled per cut) — the panel rebuilds when it
/// changes (add/remove/rotate/toggle); position-only edits update rows in place instead.
#[derive(Resource, Default)]
struct PanelSig(Option<Vec<(Axis, bool)>>);
/// A brief attention flash (seconds remaining), drawn as a fading outline.
#[derive(Component)]
struct Nudge(f32);

/// A cut-plane overlay, tied to its cut in the stack by index. Tracks its axis so the plane mesh
/// can be rebuilt when the cut is rotated.
#[derive(Component)]
struct CutPlaneViz {
    idx: usize,
    axis: Axis,
}

/// A floating piece-width label (one per piece), positioned by projecting the piece centre to screen.
#[derive(Component)]
struct DimLabel {
    idx: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bed = bed_size().unwrap_or([256.0; 3]);
    let cfg = SceneCfg {
        source: args.iter().find(|a| a.ends_with(".scad")).map(PathBuf::from),
        stl: args.iter().find(|a| a.ends_with(".stl")).map(PathBuf::from),
        bed: [bed[0] as f32, bed[1] as f32],
        root: fab::find_root(),
        tmp: std::env::temp_dir().join("fab-gui"),
        reslice_on_start: args.iter().any(|a| a == "--reslice"),
        cut_pct: flag_value(&args, "--cut").and_then(|v| v.parse().ok()).unwrap_or(50.0),
    };
    if let Some(script) = flag_value(&args, "--script") {
        run_scripted(cfg, parse_script(&script));
    } else if let Some(png) = flag_value(&args, "--screenshot") {
        run_screenshot(cfg, PathBuf::from(png));
    } else {
        run_windowed(cfg);
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// Point the asset server at this crate's `assets/` (where the icon font lives), regardless of CWD.
fn assets_dir() -> AssetPlugin {
    AssetPlugin {
        file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(),
        ..default()
    }
}

/// Load the bundled icon font (Startup, before the panel first builds).
fn load_icons(asset_server: Res<AssetServer>, mut commands: Commands) {
    commands.insert_resource(IconFont(asset_server.load("fonts/MaterialIcons-Regular.ttf")));
}

// ---- windowed -------------------------------------------------------------------------

fn run_windowed(scene: SceneCfg) {
    App::new()
        .add_plugins((DefaultPlugins.set(assets_dir()), FeathersPlugins, MeshPickingPlugin))
        .insert_resource(UiTheme(create_dark_theme()))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .init_resource::<Job>()
        .init_resource::<Cuts>()
        .init_resource::<ModelBounds>()
        .init_resource::<DraggingCut>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceDirty>()
        .init_resource::<DisplaySpread>()
        .init_resource::<PanelSig>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_observer(on_drag_start)
        .add_observer(on_drag)
        .add_observer(on_drag_end)
        .add_observer(on_click)
        .add_systems(Startup, (setup_windowed, load_icons))
        .add_systems(
            Update,
            (
                orbit,
                request_reslice,
                poll_job,
                update_status,
                sync_overlays,
                sync_overlay_visuals,
                sync_dim_labels,
                update_view_label,
                update_rows,
                sync_selected,
                apply_icon_font,
                rebuild_panel,
                nudge_buttons,
                mark_dirty,
                revert_on_edit,
                auto_scale,
            ),
        )
        .run();
}

#[derive(Component)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
    target: Vec3, // look-at point; right-drag pans it
}

fn setup_windowed(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    scene: Res<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera3d::default(),
        Transform::default(),
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
            target: Vec3::ZERO,
        },
    ));
    // Render the model off-thread; poll_job seeds the first cut when bounds land.
    kick_job(&mut job, &mut status, &scene, false, vec![]);
}

fn orbit(
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    dragging: Res<DraggingCut>,
) {
    if dragging.0 {
        // A cut plane has the pointer — don't orbit underneath the drag.
        motion.clear();
        wheel.clear();
        return;
    }
    let Ok((mut t, mut o)) = cam.single_mut() else {
        return;
    };
    // Camera basis (for panning in the view plane), captured before we move it.
    let right = t.rotation * Vec3::X;
    let up = t.rotation * Vec3::Y;
    if buttons.pressed(MouseButton::Left) {
        for ev in motion.read() {
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
        o.radius = (o.radius * (1.0 - ev.y * 0.1)).clamp(10.0, 4000.0);
    }
    *t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
}

// ---- cut stack: drag, buttons, overlays -----------------------------------------------

/// Begin dragging when a left-press lands on a cut plane: make it active + let orbit yield.
fn on_drag_start(
    ev: On<Pointer<DragStart>>,
    planes: Query<&CutPlaneViz>,
    dspread: Res<DisplaySpread>,
    mut cuts: ResMut<Cuts>,
    mut dragging: ResMut<DraggingCut>,
) {
    if ev.event.button != PointerButton::Primary {
        return;
    }
    if dspread.0 > 0.0 {
        return; // exploded view is read-only — leave the drag to orbit the camera
    }
    if let Ok(cpv) = planes.get(ev.entity) {
        cuts.active = cpv.idx;
        dragging.0 = true;
    }
}

/// Drag the active cut along X: cast a ray from the cursor, find where it's closest to the cut
/// axis, and write that into the active cut (sync_overlay_visuals then moves the overlay).
fn on_drag(
    ev: On<Pointer<Drag>>,
    planes: Query<(), With<CutPlaneViz>>,
    dragging: Res<DraggingCut>,
    bounds: Res<ModelBounds>,
    cam: Query<(&Camera, &GlobalTransform)>,
    mut cuts: ResMut<Cuts>,
) {
    if !dragging.0 || !planes.contains(ev.entity) {
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let Ok((camera, cam_tf)) = cam.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_tf, ev.pointer_location.position) else {
        return;
    };
    // Axis line through the model centre, along the active cut's axis.
    let axis = cuts.active_axis();
    let ai = axis.index();
    let p0 = with_comp((min + max) * 0.5, ai, 0.0);
    let cut_at = closest_on_axis(p0, axis.unit(), ray.origin, *ray.direction)
        .clamp(comp(min, ai), comp(max, ai));
    let a = cuts.active;
    if let Some(c) = cuts.list.get_mut(a) {
        c.at = cut_at;
    }
}

fn on_drag_end(_ev: On<Pointer<DragEnd>>, mut dragging: ResMut<DraggingCut>) {
    dragging.0 = false;
}

/// Click a cut plane: select it (collapsed/editing), or — in the read-only exploded view — flash
/// the Collapse button to point the user back to editing.
fn on_click(
    ev: On<Pointer<Click>>,
    planes: Query<&CutPlaneViz>,
    dspread: Res<DisplaySpread>,
    buttons: Query<Entity, With<ViewToggleButton>>,
    mut cuts: ResMut<Cuts>,
    mut commands: Commands,
) {
    let Ok(cpv) = planes.get(ev.entity) else {
        return;
    };
    if dspread.0 > 0.0 {
        for e in &buttons {
            commands.entity(e).insert(Nudge(0.7));
        }
    } else {
        cuts.active = cpv.idx;
    }
}

/// The Explode/Collapse button: collapse to the uncut model, or explode the last sliced result —
/// auto-slicing first if the cuts changed (or were never sliced), so it works without Re-slice.
fn toggle_view(
    _ev: On<Activate>,
    whole: Res<WholeMesh>,
    sliced: Res<SlicedMesh>,
    dirty: Res<SliceDirty>,
    mut dspread: ResMut<DisplaySpread>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut models: Query<&mut Mesh3d, With<Model>>,
) {
    if dspread.0 > 0.0 {
        // Collapse → the uncut model.
        if let Some(h) = whole.0.clone() {
            for mut m in &mut models {
                m.0 = h.clone();
            }
            dspread.0 = 0.0;
        }
    } else if dirty.0 || sliced.0.is_none() {
        // Explode, but the slice is stale/missing — re-slice (poll_job explodes when it lands).
        reslice_w.write(ReSlice);
    } else if let Some(h) = sliced.0.clone() {
        // Explode the up-to-date result, no re-render needed.
        for mut m in &mut models {
            m.0 = h.clone();
        }
        dspread.0 = SPREAD as f32;
    }
}

/// Mark the slice stale whenever the cut stack changes, so Explode re-slices.
fn mark_dirty(cuts: Res<Cuts>, mut dirty: ResMut<SliceDirty>) {
    if cuts.is_changed() {
        dirty.0 = true;
    }
}

/// Relabel the toggle button to the action it performs from the current view.
fn update_view_label(dspread: Res<DisplaySpread>, mut q: Query<&mut Text, With<ViewToggleLabel>>) {
    if !dspread.is_changed() {
        return;
    }
    let label = if dspread.0 > 0.0 { "Collapse" } else { "Explode" };
    for mut t in &mut q {
        *t = Text::new(label);
    }
}

/// Fade out the attention flash on nudged buttons (drawn as an outline).
fn nudge_buttons(time: Res<Time>, mut q: Query<(Entity, &mut Nudge)>, mut commands: Commands) {
    for (e, mut n) in &mut q {
        n.0 -= time.delta_secs();
        if n.0 <= 0.0 {
            commands.entity(e).remove::<Nudge>().remove::<Outline>();
        } else {
            let a = (n.0 / 0.7).clamp(0.0, 1.0);
            commands.entity(e).insert(Outline {
                width: Val::Px(3.0),
                offset: Val::Px(2.0),
                color: Color::srgba(1.0, 0.8, 0.2, a),
            });
        }
    }
}

/// Keep one overlay per cut. When the cut COUNT changes (add/remove), the index→cut mapping can
/// shift, so despawn all + respawn fresh; positions/colours are then refreshed by
/// sync_overlay_visuals. (Runs on every cut change, but only rebuilds on a count change.)
fn sync_overlays(
    cuts: Res<Cuts>,
    bounds: Res<ModelBounds>,
    existing: Query<Entity, With<CutPlaneViz>>,
    mut last: Local<usize>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !cuts.is_changed() || *last == cuts.list.len() {
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    for e in &existing {
        commands.entity(e).despawn();
    }
    for (i, c) in cuts.list.iter().enumerate() {
        spawn_cut_plane(&mut commands, &mut meshes, &mut materials, min, max, c, i);
    }
    *last = cuts.list.len();
}

/// Position + orient + colour each overlay from its cut. Position tracks the displayed geometry:
/// at `cut.at` when editing, fanned into the piece gaps when exploded. The plane mesh is rebuilt
/// (thin along the cut axis) only when the cut is rotated.
fn sync_overlay_visuals(
    cuts: Res<Cuts>,
    bounds: Res<ModelBounds>,
    dspread: Res<DisplaySpread>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut overlays: Query<(&mut CutPlaneViz, &mut Transform, &mut Mesh3d, &MeshMaterial3d<StandardMaterial>)>,
) {
    if !cuts.is_changed() && !dspread.is_changed() {
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    for (mut cpv, mut tf, mut mesh3d, mat) in &mut overlays {
        let idx = cpv.idx;
        let Some(c) = cuts.list.get(idx) else {
            continue;
        };
        if cpv.axis != c.axis {
            cpv.axis = c.axis;
            mesh3d.0 = meshes.add(plane_cuboid(c.axis, min, max));
        }
        tf.translation = cut_center(&cuts, idx, min, max, dspread.0);
        if let Some(mut m) = materials.get_mut(&mat.0) {
            m.base_color = cut_color(idx == cuts.active, c.enabled);
        }
    }
}

/// Offset of cut `idx` along ITS axis in the exploded layout (the slicer fans piece k by
/// `k*spread`): an enabled cut sits in the gap (+0.5) above the same-axis cuts below it; a
/// disabled cut rides with the piece it's inside. 0 when not exploded.
fn spread_offset(cuts: &Cuts, idx: usize, spread: f32) -> f32 {
    if spread == 0.0 {
        return 0.0;
    }
    let Some(cut) = cuts.list.get(idx) else {
        return 0.0;
    };
    let rank = cuts
        .list
        .iter()
        .enumerate()
        .filter(|(j, o)| *j != idx && o.enabled && o.axis == cut.axis && o.at < cut.at)
        .count() as f32;
    if cut.enabled {
        (rank + 0.5) * spread
    } else {
        rank * spread
    }
}

/// World-space centre of a cut's overlay: at its position (+ explode offset) along its axis,
/// centred on the model in the other two.
fn cut_center(cuts: &Cuts, idx: usize, min: Vec3, max: Vec3, spread: f32) -> Vec3 {
    let Some(c) = cuts.list.get(idx) else {
        return (min + max) * 0.5;
    };
    with_comp((min + max) * 0.5, c.axis.index(), c.at + spread_offset(cuts, idx, spread))
}

/// A thin slab spanning the model in the two axes the cut doesn't slice.
fn plane_cuboid(axis: Axis, min: Vec3, max: Vec3) -> Cuboid {
    let s = (max - min) * 1.15;
    match axis {
        Axis::X => Cuboid::new(0.6, s.y.max(1.0), s.z.max(1.0)),
        Axis::Y => Cuboid::new(s.x.max(1.0), 0.6, s.z.max(1.0)),
        Axis::Z => Cuboid::new(s.x.max(1.0), s.y.max(1.0), 0.6),
    }
}

/// Floating piece-width labels in the 3D view: project each piece's centre to the screen and put
/// the width there, tracking the explode (and the camera, every frame, so they follow orbit/pan).
#[allow(clippy::too_many_arguments)]
fn sync_dim_labels(
    cuts: Res<Cuts>,
    bounds: Res<ModelBounds>,
    dspread: Res<DisplaySpread>,
    cam: Query<(&Camera, &GlobalTransform)>,
    existing: Query<&DimLabel>,
    mut labels: Query<(&DimLabel, &mut Node, &mut Text, &mut Visibility)>,
    mut commands: Commands,
) {
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let Ok((camera, cam_gt)) = cam.single() else {
        return;
    };
    // Build a (world position, width) for every piece segment on EVERY axis that has cuts.
    let center = (min + max) * 0.5;
    let mut segs: Vec<(Vec3, f32)> = Vec::new();
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let ai = axis.index();
        let mut xs: Vec<f32> =
            cuts.list.iter().filter(|c| c.enabled && c.axis == axis).map(|c| c.at).collect();
        if xs.is_empty() {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut edges = vec![comp(min, ai)];
        edges.extend(xs);
        edges.push(comp(max, ai));
        for (k, w) in edges.windows(2).enumerate() {
            let mid = (w[0] + w[1]) * 0.5 + k as f32 * dspread.0;
            let mut pos = with_comp(center, ai, mid);
            if ai != 2 {
                pos.z = max.z; // float above the model for X/Y segments
            }
            segs.push((pos, w[1] - w[0]));
        }
    }

    // Spawn a label entity for any segment that lacks one (count only grows).
    for i in existing.iter().count()..segs.len() {
        commands.spawn((
            Text::new(""),
            TextColor(Color::srgb(0.95, 0.95, 1.0)),
            TextFont::from_font_size(13.0),
            Node { position_type: PositionType::Absolute, ..default() },
            DimLabel { idx: i },
        ));
    }

    for (dl, mut node, mut text, mut vis) in &mut labels {
        let Some(&(pos, width)) = segs.get(dl.idx) else {
            *vis = Visibility::Hidden;
            continue;
        };
        match camera.world_to_viewport(cam_gt, pos) {
            Ok(p) => {
                node.left = px(p.x);
                node.top = px(p.y);
                *text = Text::new(format!("{width:.0}"));
                *vis = Visibility::Visible;
            }
            Err(_) => *vis = Visibility::Hidden,
        }
    }
}

/// On a change of what's displayed, frame it: centre on the (possibly exploded) bounds + fit.
fn auto_scale(
    dspread: Res<DisplaySpread>,
    cuts: Res<Cuts>,
    bounds: Res<ModelBounds>,
    mut cams: Query<&mut Orbit>,
) {
    if !dspread.is_changed() {
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let enabled = cuts.list.iter().filter(|c| c.enabled).count() as f32;
    let extra = enabled * dspread.0; // exploded fans pieces this much further along X
    let span = ((max.x - min.x) + extra).max(max.y - min.y).max(80.0);
    for mut o in &mut cams {
        o.target = Vec3::new((min.x + max.x) * 0.5 + extra * 0.5, (min.y + max.y) * 0.5, (min.z + max.z) * 0.5);
        o.radius = span * 1.3;
    }
}

/// Revert to the uncut model the moment a cut is edited, so editing is always on the intact part.
fn revert_on_edit(
    cuts: Res<Cuts>,
    whole: Res<WholeMesh>,
    mut dspread: ResMut<DisplaySpread>,
    mut models: Query<&mut Mesh3d, With<Model>>,
) {
    if dspread.0 == 0.0 || !cuts.is_changed() {
        return;
    }
    if let Some(h) = whole.0.clone() {
        for mut m in &mut models {
            m.0 = h.clone();
        }
    }
    dspread.0 = 0.0;
}

fn cut_color(active: bool, enabled: bool) -> Color {
    // Distinct hues, none of them the model's yellow: green = editing, blue = on, red = off.
    if !enabled {
        Color::srgba(0.95, 0.30, 0.30, 0.30) // off — red
    } else if active {
        Color::srgba(0.25, 1.0, 0.35, 0.65) // active — bright green
    } else {
        Color::srgba(0.20, 0.55, 1.0, 0.50) // on — blue
    }
}

/// Offset along `axis` (a unit vector through `p0`) of the point on that line closest to the ray
/// (`ray_d` unit). The classic skew-line solution; pure, so it's unit-tested.
fn closest_on_axis(p0: Vec3, axis: Vec3, ray_o: Vec3, ray_d: Vec3) -> f32 {
    let w0 = p0 - ray_o;
    let b = axis.dot(ray_d);
    let d = axis.dot(w0);
    let e = ray_d.dot(w0);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-6 {
        return 0.0; // ray ~parallel to the axis — no stable solution
    }
    (b * e - d) / denom
}

// ---- slicing job ----------------------------------------------------------------------

/// Re-slice button → start a background slice job from the enabled cuts (ignored if one's running).
fn request_reslice(
    mut ev: MessageReader<ReSlice>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    cfg: Res<SceneCfg>,
    cuts: Res<Cuts>,
) {
    if ev.read().count() == 0 {
        return;
    }
    if job.0.is_some() {
        info!("busy — ignoring re-slice");
        return;
    }
    let xs = cuts.enabled_cuts();
    if xs.is_empty() {
        status.0 = "no enabled cuts".into();
        return;
    }
    kick_job(&mut job, &mut status, &cfg, true, xs);
}

/// Spawn the render/slice on the async compute pool (blocking OpenSCAD work, off-thread).
fn kick_job(job: &mut Job, status: &mut Status, cfg: &SceneCfg, reslice: bool, cuts: Vec<(char, f64)>) {
    let Some(src) = cfg.source.clone() else {
        status.0 = "no .scad source".into();
        return;
    };
    let (root, tmp) = (cfg.root.clone(), cfg.tmp.clone());
    let task = AsyncComputeTaskPool::get().spawn(async move {
        if reslice {
            fab::reslice(root.as_deref(), &src, &cuts, SPREAD, &tmp).map_err(|e| format!("{e:#}"))
        } else {
            fab::render_whole(root.as_deref(), &src, &tmp).map_err(|e| format!("{e:#}"))
        }
    });
    job.0 = Some((reslice, task));
    status.0 = if reslice { "slicing".into() } else { "rendering".into() };
}

/// Poll the in-flight job; when it finishes, swap in the new mesh (and seed the first cut once).
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
fn poll_job(
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut bounds: ResMut<ModelBounds>,
    mut cuts: ResMut<Cuts>,
    mut whole: ResMut<WholeMesh>,
    mut sliced: ResMut<SlicedMesh>,
    mut dirty: ResMut<SliceDirty>,
    mut dspread: ResMut<DisplaySpread>,
    models: Query<Entity, With<Model>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some((is_reslice, task)) = job.0.as_mut() else {
        return;
    };
    let is_reslice = *is_reslice;
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(stl) => {
            let (mesh, aabb) = mesh_and_bounds(&mut meshes, &stl);
            for e in &models {
                commands.entity(e).despawn();
            }
            commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(part_material(&mut materials)), Model));
            if is_reslice {
                sliced.0 = Some(mesh); // remember it so the view toggle can re-show it
                dirty.0 = false; // this slice matches the current cuts
                dspread.0 = SPREAD as f32; // now showing the fanned pieces
            } else {
                whole.0 = Some(mesh); // remember the uncut mesh, so editing can revert to it
                dspread.0 = 0.0;
                // First whole render fixes the bounds and seeds a centre cut (sync_overlays draws it).
                if bounds.0.is_none() {
                    if let Some((min, max)) = aabb {
                        bounds.0 = Some((min, max));
                        if cuts.list.is_empty() {
                            cuts.list.push(CutDef {
                                axis: Axis::X,
                                at: (min.x + max.x) * 0.5,
                                enabled: true,
                            });
                            cuts.active = 0;
                        }
                    }
                }
            }
            status.0 = "ready".into();
        }
        Err(e) => {
            error!("{e}");
            status.0 = format!("error: {e}");
        }
    }
}

fn update_status(status: Res<Status>, mut q: Query<&mut Text, With<StatusLabel>>) {
    if !status.is_changed() {
        return;
    }
    for mut t in &mut q {
        *t = Text::new(status.0.clone());
    }
}

// ---- headless screenshot --------------------------------------------------------------

#[derive(Resource)]
struct Shot {
    target: Handle<Image>,
    png: PathBuf,
    frame: u32,
    captured: bool,
}

#[derive(Resource)]
struct ScreenshotPng(PathBuf);

fn run_screenshot(scene: SceneCfg, png: PathBuf) {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(assets_dir())
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                .disable::<WinitPlugin>(),
        )
        .add_plugins(ScheduleRunnerPlugin::run_loop(
            std::time::Duration::from_secs_f64(1.0 / 60.0),
        ))
        .add_plugins(FeathersPlugins)
        .insert_resource(UiTheme(create_dark_theme()))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .insert_resource(ScreenshotPng(png))
        .init_resource::<Cuts>()
        .init_resource::<ModelBounds>()
        .init_resource::<DraggingCut>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceDirty>()
        .init_resource::<DisplaySpread>()
        .init_resource::<PanelSig>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_systems(Startup, (setup_offscreen, load_icons))
        .add_systems(Update, (capture_then_exit, update_rows, sync_selected, apply_icon_font, rebuild_panel, update_status))
        .run();
}

fn setup_offscreen(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    scene: Res<SceneCfg>,
    png: Res<ScreenshotPng>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    // Synchronous here — no UI to freeze. Render whole for bounds + the cut plane, then
    // (if asked) slice at the chosen cut so the PNG verifies an off-center cut.
    let display = setup_offscreen_model(&mut commands, &mut meshes, &mut materials, &scene);
    commands.spawn((Mesh3d(display), MeshMaterial3d(part_material(&mut materials)), Model));

    // Offscreen render target the camera draws into and we screenshot.
    let (w, h) = (960u32, 720u32);
    let mut img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);

    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        orbit_transform(-0.7, 0.5, radius, Vec3::ZERO),
        bevy::ui::IsDefaultUiCamera,
    ));

    commands.insert_resource(Shot {
        target,
        png: png.0.clone(),
        frame: 0,
        captured: false,
    });
}

/// Headless model prep: render whole (→ bounds + cut plane), optionally slice at the cut.
/// Returns the mesh handle to display.
fn setup_offscreen_model(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    scene: &SceneCfg,
) -> Handle<Mesh> {
    let Some(src) = scene.source.as_deref() else {
        return load_model(meshes, scene.stl.as_deref());
    };
    let whole = match fab::render_whole(scene.root.as_deref(), src, &scene.tmp) {
        Ok(p) => p,
        Err(e) => {
            error!("{e:#}");
            return load_model(meshes, None);
        }
    };
    let (whole_mesh, aabb) = mesh_and_bounds(meshes, &whole);
    let mut cut_x = 0.0;
    if let Some((min, max)) = aabb {
        cut_x = min.x + (scene.cut_pct / 100.0) * (max.x - min.x);
        let cut = CutDef { axis: Axis::X, at: cut_x, enabled: true };
        spawn_cut_plane(commands, meshes, materials, min, max, &cut, 0);
    }
    if !scene.reslice_on_start {
        return whole_mesh;
    }
    match fab::reslice(scene.root.as_deref(), src, &[('x', cut_x as f64)], SPREAD, &scene.tmp) {
        Ok(sliced) => load_model(meshes, Some(&sliced)),
        Err(e) => {
            error!("{e:#}");
            whole_mesh
        }
    }
}

fn capture_then_exit(
    mut commands: Commands,
    mut shot: ResMut<Shot>,
    mut exit: MessageWriter<AppExit>,
) {
    shot.frame += 1;
    if !shot.captured && shot.frame >= 3 {
        let png = shot.png.clone();
        commands
            .spawn(Screenshot::image(shot.target.clone()))
            .observe(save_to_disk(png.clone()));
        shot.captured = true;
        info!("capturing -> {}", png.display());
    }
    if shot.captured && shot.frame >= 30 {
        if shot.png.exists() {
            info!("saved {}", shot.png.display());
        } else {
            error!("screenshot did not appear at {}", shot.png.display());
        }
        exit.write(AppExit::Success);
    }
}

// ---- scripted interaction harness -----------------------------------------------------

/// One step in a `--script` timeline. Drives the REAL systems (the cut stack, request_reslice,
/// poll_job) with synthetic input, then screenshots — interaction is verified, not just setup.
#[derive(Clone)]
enum Action {
    Cut(f32),      // set the ACTIVE cut's position (along its axis)
    AddCut(f32),   // add a cut at this position (on the active axis), make it active
    SetAxis(Axis), // set the active cut's axis
    Toggle,        // toggle the active cut on/off
    Next,          // cycle the active cut
    Reslice,       // trigger a slice, then wait for the async job
    Shot(PathBuf), // screenshot the viewport to this path
    Wait(u32),     // idle this many frames
}

#[derive(Resource)]
struct ScriptRunner {
    actions: Vec<Action>,
    idx: usize,
    timer: u32,
}

/// The offscreen image the camera renders into, so scripted shots can grab it.
#[derive(Resource)]
struct RenderTargetImage(Handle<Image>);

/// Parse `"addcut 30; reslice; shot a.png; toggle; reslice; shot b.png"` into a timeline.
fn parse_script(s: &str) -> Vec<Action> {
    s.split(';')
        .filter_map(|part| {
            let mut it = part.split_whitespace();
            match it.next()? {
                "cut" => it.next()?.parse().ok().map(Action::Cut),
                "addcut" => it.next()?.parse().ok().map(Action::AddCut),
                "axis" => match it.next()? {
                    "x" => Some(Action::SetAxis(Axis::X)),
                    "y" => Some(Action::SetAxis(Axis::Y)),
                    "z" => Some(Action::SetAxis(Axis::Z)),
                    _ => None,
                },
                "toggle" => Some(Action::Toggle),
                "next" => Some(Action::Next),
                "reslice" => Some(Action::Reslice),
                "shot" => it.next().map(|p| Action::Shot(PathBuf::from(p))),
                "wait" => it.next()?.parse().ok().map(Action::Wait),
                other => {
                    eprintln!("script: unknown action '{other}'");
                    None
                }
            }
        })
        .collect()
}

/// Headless, but runs the FULL windowed systems + an offscreen camera, then walks the script.
fn run_scripted(scene: SceneCfg, actions: Vec<Action>) {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(assets_dir())
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                .disable::<WinitPlugin>(),
        )
        .add_plugins(ScheduleRunnerPlugin::run_loop(
            std::time::Duration::from_secs_f64(1.0 / 60.0),
        ))
        .add_plugins(FeathersPlugins)
        .insert_resource(UiTheme(create_dark_theme()))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .init_resource::<Job>()
        .init_resource::<Cuts>()
        .init_resource::<ModelBounds>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceDirty>()
        .init_resource::<DisplaySpread>()
        .init_resource::<PanelSig>()
        .insert_resource(Status("rendering".into()))
        .insert_resource(ScriptRunner { actions, idx: 0, timer: 0 })
        .add_message::<ReSlice>()
        .add_systems(Startup, (setup_script, load_icons))
        .add_systems(
            Update,
            (
                request_reslice,
                poll_job,
                update_status,
                sync_overlays,
                sync_overlay_visuals,
                sync_dim_labels,
                update_view_label,
                update_rows,
                sync_selected,
                apply_icon_font,
                rebuild_panel,
                mark_dirty,
                revert_on_edit,
                run_script,
            ),
        )
        .run();
}

fn setup_script(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    scene: Res<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    let (w, h) = (960u32, 720u32);
    let mut img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);
    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        orbit_transform(-0.7, 0.5, radius, Vec3::ZERO),
        bevy::ui::IsDefaultUiCamera,
    ));
    commands.insert_resource(RenderTargetImage(target));
    kick_job(&mut job, &mut status, &scene, false, vec![]);
}

/// Step the script: each action drives the real systems, waiting on async work to settle.
#[allow(clippy::too_many_arguments)]
fn run_script(
    mut runner: ResMut<ScriptRunner>,
    bounds: Res<ModelBounds>,
    job: Res<Job>,
    target: Res<RenderTargetImage>,
    mut cuts: ResMut<Cuts>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    if bounds.0.is_none() {
        return; // wait for the initial render (model + bounds + first cut)
    }
    if runner.idx >= runner.actions.len() {
        exit.write(AppExit::Success);
        return;
    }
    runner.timer += 1;
    let done = match runner.actions[runner.idx].clone() {
        Action::Cut(v) => {
            if runner.timer == 1 {
                let a = cuts.active;
                let v = clamp_to_bounds(v, cuts.active_axis(), &bounds);
                if let Some(c) = cuts.list.get_mut(a) {
                    c.at = v;
                }
            }
            runner.timer >= 2
        }
        Action::AddCut(v) => {
            if runner.timer == 1 {
                let axis = cuts.active_axis();
                let at = clamp_to_bounds(v, axis, &bounds);
                cuts.list.push(CutDef { axis, at, enabled: true });
                cuts.active = cuts.list.len() - 1;
            }
            runner.timer >= 2
        }
        Action::SetAxis(ax) => {
            if runner.timer == 1 {
                let a = cuts.active;
                if let Some((min, max)) = bounds.0 {
                    if let Some(c) = cuts.list.get_mut(a) {
                        c.axis = ax;
                        c.at = comp((min + max) * 0.5, ax.index());
                    }
                }
            }
            runner.timer >= 2
        }
        Action::Toggle => {
            if runner.timer == 1 {
                let a = cuts.active;
                if let Some(c) = cuts.list.get_mut(a) {
                    c.enabled = !c.enabled;
                }
            }
            runner.timer >= 2
        }
        Action::Next => {
            if runner.timer == 1 {
                let n = cuts.list.len();
                if n > 0 {
                    cuts.active = (cuts.active + 1) % n;
                }
            }
            runner.timer >= 2
        }
        Action::Reslice => {
            if runner.timer == 1 {
                reslice_w.write(ReSlice);
            }
            runner.timer > 3 && job.0.is_none() // kicked, then completed
        }
        Action::Shot(path) => {
            if runner.timer == 1 {
                commands
                    .spawn(Screenshot::image(target.0.clone()))
                    .observe(save_to_disk(path.clone()));
                info!("script: shot -> {}", path.display());
            }
            runner.timer >= 30 // give the GPU readback + save time
        }
        Action::Wait(n) => runner.timer >= n,
    };
    if done {
        runner.idx += 1;
        runner.timer = 0;
    }
}

fn clamp_to_bounds(x: f32, axis: Axis, bounds: &ModelBounds) -> f32 {
    match bounds.0 {
        Some((min, max)) => x.clamp(comp(min, axis.index()), comp(max, axis.index())),
        None => x,
    }
}

// ---- shared scene ---------------------------------------------------------------------

/// The bed + lights (everything but the model + cut planes, which load via a job / synchronously).
fn spawn_environment(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cfg: &SceneCfg,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(cfg.bed[0], cfg.bed[1], 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.18, 0.18, 0.22),
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -0.5),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 6000.0,
            ..default()
        },
        Transform::from_xyz(80.0, -120.0, 160.0).looking_at(Vec3::ZERO, Vec3::Z),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2000.0,
            ..default()
        },
        Transform::from_xyz(-120.0, 100.0, 60.0).looking_at(Vec3::ZERO, Vec3::Z),
    ));
}

/// A translucent slab on a cut, thin along its axis, spanning the model in the other two.
fn spawn_cut_plane(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    min: Vec3,
    max: Vec3,
    cut: &CutDef,
    idx: usize,
) {
    commands.spawn((
        Mesh3d(meshes.add(plane_cuboid(cut.axis, min, max))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: cut_color(true, cut.enabled),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        })),
        Transform::from_translation(with_comp((min + max) * 0.5, cut.axis.index(), cut.at)),
        CutPlaneViz { idx, axis: cut.axis },
    ));
}

/// Load an STL into a mesh and its AABB (None on failure → placeholder mesh, no bounds).
fn mesh_and_bounds(meshes: &mut Assets<Mesh>, stl: &Path) -> (Handle<Mesh>, Option<(Vec3, Vec3)>) {
    match stl::load_stl(stl) {
        Ok(s) => {
            info!("loaded {} ({} tris)", stl.display(), s.positions.len() / 3);
            (meshes.add(build_mesh(&s)), aabb_of(&s))
        }
        Err(e) => {
            error!("loading {}: {e:#}", stl.display());
            (meshes.add(Cuboid::new(60.0, 40.0, 30.0)), None)
        }
    }
}

fn aabb_of(s: &stl::StlMesh) -> Option<(Vec3, Vec3)> {
    let mut iter = s.positions.iter().map(|p| Vec3::from_array(*p));
    let first = iter.next()?;
    let (mut min, mut max) = (first, first);
    for v in iter {
        min = min.min(v);
        max = max.max(v);
    }
    Some((min, max))
}

fn load_model(meshes: &mut Assets<Mesh>, stl: Option<&Path>) -> Handle<Mesh> {
    match stl {
        Some(p) if p.exists() => mesh_and_bounds(meshes, p).0,
        _ => meshes.add(Cuboid::new(60.0, 40.0, 30.0)),
    }
}

fn part_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(0.90, 0.74, 0.20),
        perceptual_roughness: 0.7,
        ..default()
    })
}

/// Camera transform orbiting `target` at (yaw, pitch, radius), Z-up.
fn orbit_transform(yaw: f32, pitch: f32, radius: f32, target: Vec3) -> Transform {
    let cp = pitch.cos();
    let off = Vec3::new(radius * cp * yaw.cos(), radius * cp * yaw.sin(), radius * pitch.sin());
    Transform::from_translation(target + off).looking_at(target, Vec3::Z)
}

fn build_mesh(s: &stl::StlMesh) -> Mesh {
    let n = s.positions.len() as u32;
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, s.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, s.normals.clone())
        .with_inserted_indices(Indices::U32((0..n).collect()))
}

/// The default printer's bed, read from fab-scad's printers.toml via the shared lib.
fn bed_size() -> Option<[f64; 3]> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml = dir.join("printers.toml");
        if toml.exists() {
            let printers = fab_scad::printers::load(&toml).ok()?;
            return fab_scad::printers::select(&printers, None).ok().map(|p| p.bed);
        }
        if !dir.pop() {
            return None;
        }
    }
}

// ---- Feathers UI: plane-grouped cards -------------------------------------------------

/// Label for a cut row — its position (the active row is highlighted via `Selected`, on/off by
/// its toggle button).
fn pos_text(at: f32) -> String {
    format!("{at:.0} mm")
}

/// One plane card: a header (plane name + per-plane "+cut") and a list of that plane's cuts.
fn plane_card(cuts: &Cuts, axis: Axis) -> impl Scene + 'static {
    let rows: Vec<_> = cuts
        .list
        .iter()
        .enumerate()
        .filter(|(_, c)| c.axis == axis)
        .map(|(idx, c)| {
            // Per-row actions, each a move-closure capturing this cut's index.
            let toggle = move |_: On<Activate>, mut cuts: ResMut<Cuts>| {
                if let Some(c) = cuts.list.get_mut(idx) {
                    c.enabled = !c.enabled;
                }
            };
            let del = move |_: On<Activate>, mut cuts: ResMut<Cuts>| {
                if idx < cuts.list.len() {
                    cuts.list.remove(idx);
                    if !cuts.list.is_empty() && cuts.active >= cuts.list.len() {
                        cuts.active = cuts.list.len() - 1;
                    }
                }
            };
            let eye = if c.enabled { ICON_ON } else { ICON_OFF };
            // On = blue (Primary), off = grey (Normal) — state as colour; eye/eye-off icon too.
            let on_variant = if c.enabled { ButtonVariant::Primary } else { ButtonVariant::Normal };
            bsn! {
                @FeathersListRow
                RowFor(idx)
                Children [
                    (Text(pos_text(c.at)) ThemedText),
                    (
                        @FeathersButton { @variant: {on_variant}, @caption: bsn!{
                            Text(eye) IconText
                        } }
                        on(toggle)
                    ),
                    (
                        @FeathersButton { @caption: bsn!{
                            Text(ICON_DELETE)
                            IconText
                            TextColor({Color::srgb(0.95, 0.5, 0.5)})
                        } }
                        on(del)
                    ),
                ]
            }
        })
        .collect();
    // Per-plane add: a move-closure capturing this card's axis.
    let add = move |_: On<Activate>, mut cuts: ResMut<Cuts>, bounds: Res<ModelBounds>| {
        if let Some((mn, mx)) = bounds.0 {
            let at = comp((mn + mx) * 0.5, axis.index());
            cuts.list.push(CutDef { axis, at, enabled: true });
            cuts.active = cuts.list.len() - 1;
        }
    };
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(3),
            padding: UiRect::all(px(4)),
        }
        Children [
            (
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: px(8),
                    justify_content: JustifyContent::SpaceBetween,
                }
                Children [
                    (Text(format!("{} plane", axis.label())) ThemedText),
                    (
                        @FeathersButton { @variant: {ButtonVariant::Primary}, @caption: bsn!{
                            Text(ICON_ADD) IconText
                        } }
                        on(add)
                    ),
                ]
            ),
            (@FeathersListView { @rows: { Box::new(rows) as Box<dyn SceneList> } }),
        ]
    }
}

/// The whole panel scene, built from the current cut stack: title, an X/Y/Z card stack, and a
/// bottom bar (status + Re-slice + Explode/Collapse).
fn build_panel(cuts: &Cuts) -> impl Scene + 'static {
    let cards: Vec<_> =
        [Axis::X, Axis::Y, Axis::Z].into_iter().map(|a| plane_card(cuts, a)).collect();
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            left: px(8),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: UiRect::all(px(8)),
            min_width: px(220),
        }
        PanelRoot
        ThemeBackgroundColor(tokens::WINDOW_BG)
        Children [
            (Text("fab-gui") ThemedText),
            (
                Node { flex_direction: FlexDirection::Column, row_gap: px(6) }
                Children [ { Box::new(cards) as Box<dyn SceneList> } ]
            ),
            (Text("rendering") ThemedText StatusLabel),
            (
                @FeathersButton { @variant: {ButtonVariant::Primary}, @caption: bsn!{ Text("Re-slice") ThemedText } }
                on(|_: On<Activate>, mut w: MessageWriter<ReSlice>| { w.write(ReSlice); })
            ),
            (
                @FeathersButton { @caption: bsn!{ Text("Explode") ThemedText ViewToggleLabel } }
                ViewToggleButton
                on(toggle_view)
            ),
        ]
    }
}

/// Rebuild the whole panel when the cut STRUCTURE changes (a cut added/removed/rotated → the
/// per-cut axis sequence differs). Value-only edits leave it alone; update_rows refreshes those.
fn rebuild_panel(
    cuts: Res<Cuts>,
    mut sig: ResMut<PanelSig>,
    roots: Query<Entity, With<PanelRoot>>,
    mut commands: Commands,
) {
    let cur: Vec<(Axis, bool)> = cuts.list.iter().map(|c| (c.axis, c.enabled)).collect();
    if sig.0.as_deref() == Some(cur.as_slice()) {
        return;
    }
    sig.0 = Some(cur);
    for e in &roots {
        commands.entity(e).despawn();
    }
    commands.queue(|world: &mut World| {
        let scene = build_panel(world.resource::<Cuts>());
        if let Err(e) = world.spawn_scene(scene) {
            error!("panel spawn failed: {e:?}");
        }
    });
}

/// Refresh each row's text from its cut, in place — so position/on-off edits show without a rebuild.
fn update_rows(cuts: Res<Cuts>, rows: Query<(&RowFor, &Children)>, mut texts: Query<&mut Text>) {
    if !cuts.is_changed() {
        return;
    }
    for (rf, children) in &rows {
        let Some(c) = cuts.list.get(rf.0) else {
            continue;
        };
        let label = pos_text(c.at);
        // The position is the row's first direct Text child (button captions are grandchildren).
        for child in children.iter() {
            if let Ok(mut t) = texts.get_mut(child) {
                *t = Text::new(label.clone());
                break;
            }
        }
    }
}

/// Highlight the active cut's row (and only it) via `Selected`. Idempotent — only touches rows
/// whose state is wrong — so it's cheap to run every frame and survives panel rebuilds.
fn sync_selected(cuts: Res<Cuts>, rows: Query<(Entity, &RowFor, Has<bevy::ui::Selected>)>, mut commands: Commands) {
    for (e, rf, selected) in &rows {
        let should = rf.0 == cuts.active;
        if should && !selected {
            commands.entity(e).insert(bevy::ui::Selected);
        } else if !should && selected {
            commands.entity(e).remove::<bevy::ui::Selected>();
        }
    }
}

/// Render `IconText` captions in the bundled icon font — set the real TextFont once it differs
/// from the theme default (idempotent, so it catches freshly-spawned rows after a panel rebuild).
fn apply_icon_font(
    icon: Res<IconFont>,
    mut q: Query<(Entity, &mut TextFont), (With<IconText>, Without<IconApplied>)>,
    mut commands: Commands,
) {
    for (e, mut tf) in &mut q {
        tf.font = FontSource::Handle(icon.0.clone());
        tf.font_size = FontSize::Px(16.0);
        commands.entity(e).insert(IconApplied);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_ray_maps_to_axis_coordinate() {
        // A ray pointing straight down (-Z) through x=5 is closest to the X axis at x=5.
        let sc = closest_on_axis(Vec3::ZERO, Vec3::X, Vec3::new(5.0, 0.0, 10.0), Vec3::NEG_Z);
        assert!((sc - 5.0).abs() < 1e-4, "expected 5.0, got {sc}");
    }

    #[test]
    fn drag_ray_parallel_to_axis_is_safe() {
        // Ray parallel to the axis is degenerate — return 0, never NaN.
        let sc = closest_on_axis(Vec3::ZERO, Vec3::X, Vec3::new(0.0, 1.0, 0.0), Vec3::X);
        assert_eq!(sc, 0.0);
    }

    #[test]
    fn enabled_cuts_filter_out_disabled_and_carry_axis() {
        let cuts = Cuts {
            list: vec![
                CutDef { axis: Axis::X, at: -10.0, enabled: true },
                CutDef { axis: Axis::X, at: 5.0, enabled: false },
                CutDef { axis: Axis::Y, at: 20.0, enabled: true },
            ],
            active: 0,
        };
        assert_eq!(cuts.enabled_cuts(), vec![('x', -10.0), ('y', 20.0)]);
    }

    #[test]
    fn spread_offset_is_per_axis() {
        // Two X cuts + one Y cut. The second X cut (rank 1) sits in the gap above one X piece;
        // the Y cut (rank 0 on its own axis) is unaffected by the X cuts.
        let cuts = Cuts {
            list: vec![
                CutDef { axis: Axis::X, at: -10.0, enabled: true },
                CutDef { axis: Axis::X, at: 20.0, enabled: true },
                CutDef { axis: Axis::Y, at: 0.0, enabled: true },
            ],
            active: 0,
        };
        assert_eq!(spread_offset(&cuts, 0, 10.0), 5.0); // X rank 0 → (0+0.5)*10
        assert_eq!(spread_offset(&cuts, 1, 10.0), 15.0); // X rank 1 → (1+0.5)*10
        assert_eq!(spread_offset(&cuts, 2, 10.0), 5.0); // Y rank 0 → (0+0.5)*10
    }
}
