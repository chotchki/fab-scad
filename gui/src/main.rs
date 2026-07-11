//! fab-gui — the slicing GUI (Phase 5.1). A Bevy 0.19 viewport over a model, with the printer
//! bed for reference and an egui control panel. A STACK of cut planes (each draggable in 3D
//! and toggleable on/off) drives `fab` in-process (the shared `fab_scad` lib) ON A BACKGROUND
//! THREAD; Re-slice swaps in the result. The cut stack is the unit a DAG cache will key on:
//! a slice is a pure function of (source, enabled cuts). Modes:
//!
//!   cargo run -p fab-gui -- part.scad                       # windowed: orbit, drag cuts, Re-slice
//!   cargo run -p fab-gui -- part.scad --screenshot out.png  # headless render to PNG (self-verify)
//!   cargo run -p fab-gui -- part.scad --script "addcut 30; reslice; shot a.png"  # scripted harness

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use bevy::ecs::system::SystemParam;
use bevy::{
    app::ScheduleRunnerPlugin,
    asset::{AssetPlugin, RenderAssetUsages},
    camera::{visibility::RenderLayers, RenderTarget},
    image::Image,
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
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
    tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task},
    window::ExitCondition,
    winit::WinitPlugin,
};
use bevy_egui::{egui, EguiContexts, EguiPlugin, EguiPrimaryContextPass};

mod fab;
use fab_scad::stl;
// The shared geometry types the auto-slice/planner APIs take (J.6 unified on `fab_lang`'s Vec3). Aliased
// `FVec3` so it doesn't shadow Bevy's `Vec3`, which the scene code uses everywhere.
use fab_lang::{Dims, Vec3 as FVec3};

const SPREAD: f64 = 50.0;

/// Scene inputs shared by both modes.
#[derive(Resource, Clone)]
struct SceneCfg {
    source: Option<PathBuf>, // .scad source (sliceable, preferred)
    stl: Option<PathBuf>,    // .stl to display directly (when there's no source)
    bed: [f32; 2],
    root: Option<PathBuf>,  // workspace root, for OPENSCADPATH
    tmp: PathBuf,           // scratch dir for rendered/sliced STLs
    reslice_on_start: bool, // screenshot --reslice: display the sliced result
    cut_pct: f32,           // screenshot --cut <0..100>: where along X to cut
}

/// Marks the displayed model entity, so re-slice can swap it out.
#[derive(Component)]
struct Model;

/// Marks the printer-bed slab, so `seat_bed` can drop it to the model's Z-floor (the model's native
/// coords may put its bottom below z=0; move the bed to meet it rather than shift the model — which
/// would desync the cut positions from the source the slicer re-renders).
#[derive(Component)]
struct Bed;

/// Button → "re-slice the source and swap the mesh".
#[derive(Message)]
struct ReSlice;

/// Button / `autoplace` verb → "fill the open cut's cross-section with auto-sized onions".
#[derive(Message)]
struct AutoPlace;

/// A file-list row click / the `open` script verb / the picker landing → "make file <i> the active
/// source": wipe the old model's state and render the new one (`apply_switch_file`).
#[derive(Message, Clone, Copy)]
struct SwitchFile(usize);

/// The browsable source list (5.3.2): every `.scad` the picker turned up, plus which one is active.
/// `SceneCfg.source` stays the single source of truth for "what's loaded" — this just adds the list
/// the panel shows and the switch machinery indexes. Empty until the first Open.
#[derive(Resource, Default)]
struct FileList {
    files: Vec<PathBuf>,
    active: Option<usize>,
}

/// The in-flight native folder pick (5.3.1), off the main thread like a render job. `Some(path)` on
/// pick, `None` if the user cancelled; `poll_open_dialog` drains it into `FileList`.
#[derive(Resource, Default)]
struct OpenDialog(Option<Task<Option<PathBuf>>>);

/// Auto-reload watch (5.3.3 + the DAG): the latest mtime across the source's whole include CLOSURE
/// (`fab_scad::deps`), and that closure cached. `watch_source` polls it each frame and re-renders
/// when ANY dep advances — edit an `include`d module and the preview rebuilds, not just the open
/// file. mtime-poll, not the `notify` crate — trivial syscalls, no thread/dep, same effect.
#[derive(Resource, Default)]
struct Watch {
    mtime: Option<std::time::SystemTime>,
    closure: Vec<PathBuf>,
}

/// The in-flight render/slice (off the main thread): `(was_reslice, task)`. The task yields
/// `Ok(stl)` when done, else an error string.
#[derive(Resource, Default)]
struct Job(Option<(bool, Task<Result<PathBuf, String>>)>);

/// One-line status shown in the panel (e.g. "slicing", "ready").
#[derive(Resource)]
struct Status(String);

/// The axis a cut plane is normal to (which way it slices).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
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
#[derive(Default)]
struct Cuts {
    list: Vec<CutDef>,
    active: usize,
}

impl Cuts {
    /// Enabled cuts as `(axis letter, position)`, the input to `fab::reslice`.
    fn enabled_cuts(&self) -> Vec<(char, f64)> {
        self.list
            .iter()
            .filter(|c| c.enabled)
            .map(|c| (c.axis.scad(), c.at as f64))
            .collect()
    }

    /// Stack indices of the enabled cuts, in order — a connector's stack-index maps to its
    /// position here to reference the right cut in the sliced spec.
    fn enabled_indices(&self) -> Vec<usize> {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, _)| i)
            .collect()
    }

    fn active_axis(&self) -> Axis {
        self.list
            .get(self.active)
            .map(|c| c.axis)
            .unwrap_or(Axis::X)
    }
}

/// Machine-screw size for bolt connectors; `label` is the manifest / BOSL2 string.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screw {
    M3,
    M4,
    M5,
}

impl Screw {
    fn label(self) -> &'static str {
        match self {
            Screw::M3 => "M3",
            Screw::M4 => "M4",
            Screw::M5 => "M5",
        }
    }
    /// Approx socket-head / counterbore radius (mm) — the bolt's footprint in the editor profile.
    fn head_r(self) -> f32 {
        match self {
            Screw::M3 => 2.75,
            Screw::M4 => 3.5,
            Screw::M5 => 4.25,
        }
    }
}

/// A placed connector: which cut (stack index) it sits on, its position in that cut plane's two
/// non-axis dims, its onion diameter (auto-sized at placement; ignored for a bolt), and its `kind`
/// + bolt `screw` — set from the active type when placed, so onion and bolt can mix on one cut.
#[derive(Clone, Copy)]
struct PlacedConn {
    cut: usize,
    pos: [f32; 2],
    size: f32,
    kind: fab::ConnKind,
    screw: Screw,
}

/// The placed connectors (manual face-pick). Like the cut stack, a pure input to the slice.
#[derive(Default)]
struct Conns {
    list: Vec<PlacedConn>,
}

/// The kind + screw NEW placements take (manual click + Auto-place). Existing connectors keep their
/// own — you can mix onion and bolt on a cut. Set by the connector editor's type selector.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
struct ActiveConn {
    kind: fab::ConnKind,
    screw: Screw,
}

impl Default for ActiveConn {
    fn default() -> Self {
        Self {
            kind: fab::ConnKind::Onion,
            screw: Screw::M3,
        }
    }
}

/// Which cut's 2D connector editor is open (None = normal 3D view). When set, the model hides and
/// the cut's cross-section profile is shown face-on for precise picking.
#[derive(Resource, Default)]
struct EditCut(Option<usize>);

/// The open cut's cross-section: profile loops in connector-pos coords (the cut's two non-axis
/// dims). `None` until computed / when no editor is open.
#[derive(Resource, Default)]
struct XSection(Option<Vec<Vec<[f32; 2]>>>);

/// Per-piece print orientations, keyed by slab multi-index — the build-up direction (model space)
/// each piece prints in. The preview seeds `map` with the auto-pick (`auto_orient::best_up`);
/// clicking a piece's face sets a MANUAL override (recorded in `manual` so a re-render keeps it).
/// Threaded into `reslice` so the slice gates its onions on how each piece actually prints. Empty =
/// every piece defaults to +Z (the pre-orientation behaviour).
/// A printable piece's identity: its slab multi-index + its connected-COMPONENT index within that
/// slab (0 when the slab is a single solid; a presliced blob splits into comps 0..N — T.2a). Every
/// per-piece orientation keys off this so each component orients on its own.
type PieceKey = ([usize; 3], usize);

#[derive(Default)]
struct Orient {
    map: HashMap<PieceKey, [f32; 3]>,
    manual: HashSet<PieceKey>,
}

impl Orient {
    /// Record a user-chosen build-up for `key` (model space, normalised by the caller).
    fn set_manual(&mut self, key: PieceKey, up: [f32; 3]) {
        self.map.insert(key, up);
        self.manual.insert(key);
    }
    /// This piece's build-up, falling back to `auto` (the auto-pick) when unset.
    fn up_or(&self, key: PieceKey, auto: [f32; 3]) -> [f32; 3] {
        self.map.get(&key).copied().unwrap_or(auto)
    }
}

/// One independent top-level part of the model (T.2b): its own cut stack, connectors, per-piece
/// orientations, model bbox, and auto-plan-done flag. The whole per-model state that USED to be five
/// global resources now lives here, one bundle per part. Increment A keeps exactly ONE Part so
/// behaviour is unchanged; Increment B builds N (one per `build_geo_parts` top-level item).
#[derive(Default)]
struct Part {
    cuts: Cuts,
    conns: Conns,
    orient: Orient,
    bounds: ModelBounds,
    auto_planned: AutoPlanned,
}

/// The model's parts. INVARIANT: always non-empty — `[ActivePart]` indexes the one the panel edits.
#[derive(Resource, Default)]
struct Parts(Vec<Part>);

/// Which part the panel + slice systems currently act on (index into [`Parts`]). Always valid.
#[derive(Resource, Default)]
struct ActivePart(usize);

/// Whether the print-orientation preview is showing: the model + cut planes hide, and every piece
/// is laid out on the bed rotated to its print-up. A workflow MODE, like the connector editor.
#[derive(Resource, Default)]
struct PrintView(bool);

/// The in-flight print-layout render (off-thread): renders + auto-orients every piece. Yields the
/// pieces (mesh + multi-index + build-up) on success, else an error string.
#[derive(Resource, Default)]
struct PrintJob(Option<Task<Result<Vec<fab::PiecePrint>, String>>>);

/// The last print-layout's rendered pieces, kept so a manual re-orient re-lays-out from the cached
/// meshes (no re-render). Cleared when the preview closes.
#[derive(Resource, Default)]
struct PrintPieces(Option<Vec<fab::PiecePrint>>);

/// The in-flight auto-plan job (auto-slice + onion auto-place, off-thread) — auto-on-open's worker.
#[derive(Resource, Default)]
struct AutoJob(Option<Task<Result<fab_scad::auto::AutoPlan, String>>>);

/// The source already auto-planned on open, so it fires ONCE per fresh too-big model — not every
/// frame, and not again after you clear the cuts by hand. Per-part ([`Part::auto_planned`]).
#[derive(Default)]
struct AutoPlanned(Option<PathBuf>);

/// The in-flight publish job (render artifacts + upload to hotchkiss.io, off-thread). Yields the
/// published page URL or an error string.
#[derive(Resource, Default)]
struct PublishJob(Option<Task<Result<String, String>>>);

/// The orbit camera (yaw, pitch, radius, target) as it was in NORMAL view, saved while there so a
/// mode that hijacks the camera (the 2D editor's face-on, the print preview's bed-frame) can hand
/// it back when you return. Without this, leaving a mode strands you at the mode's camera.
#[derive(Resource, Default)]
struct PrevCam(Option<(f32, f32, f32, Vec3)>);

/// Per-placed-connector onion feasibility (index-aligned with `Conns::list`): `true` = prints
/// support-free, `false` = downgrades to a bolt under the current orientations. Drives the marker
/// colour + the downgrade count. Recomputed when cuts / connectors / orientations change.
#[derive(Resource, Default)]
struct Feas(Vec<bool>);

/// One laid-out piece in the print-orientation preview (its slab + component key, for the
/// click→orient pick). Despawned when the preview closes.
#[derive(Component)]
struct PrintPiece(PieceKey);

/// The 3D point of a `(pos_a, pos_b)` on a cut plane: `at` along the axis, pos in the two non-axis
/// dims (ascending) — the inverse of the connector projection.
fn profile_point(axis: Axis, at: f32, pos: [f32; 2]) -> Vec3 {
    let ai = axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let mut p = with_comp(Vec3::ZERO, ai, at);
    p = with_comp(p, others[0], pos[0]);
    p = with_comp(p, others[1], pos[1]);
    p
}

/// A connector's 3D point: `at` along its cut's axis, `pos` in the two non-axis dims (matching
/// the driver's projection). `None` if the cut it references is gone.
fn conn_point(cuts: &Cuts, pc: &PlacedConn) -> Option<Vec3> {
    let c = cuts.list.get(pc.cut)?;
    let ai = c.axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let mut p = with_comp(Vec3::ZERO, ai, c.at);
    p = with_comp(p, others[0], pc.pos[0]);
    p = with_comp(p, others[1], pc.pos[1]);
    Some(p)
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

/// The whole model's AABB (min, max), set once on the first render — maps drag/positions. Per-part
/// ([`Part::bounds`]).
#[derive(Default)]
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

/// True while the in-flight slice was kicked by `auto_reslice` (a background rebuild), so `poll_job`
/// refreshes the pieces WITHOUT jumping the view to exploded — vs an explicit slice, which shows them.
#[derive(Resource, Default)]
struct SliceInBackground(bool);

/// How long inputs must settle (no change) before a background reslice fires — coalesces a cut drag
/// or a burst of connector edits into ONE rebuild instead of one per frame.
const AUTOSLICE_DEBOUNCE: f32 = 0.35;

/// A 3D marker (small sphere) for a placed connector, by its index in the `Conns` list.
#[derive(Component)]
struct ConnMarker(usize);

/// A panel button command that a heavy action system handles (U.1.2): the egui panel is
/// immediate-mode, so a click that needs params beyond the panel's own resources writes one of
/// these instead of mutating in place. The matching `*_action` system reads it.
#[derive(Message, Clone, Copy, PartialEq, Eq)]
enum PanelCmd {
    EditOpenscad,
    AutoSlice,
    ToggleView,
    Publish,
    Export,
}

/// Panel → seam outputs, written by `panel_ui` each frame and read by the 3D systems: `over_ui`
/// yields the camera orbit when the pointer is on the panel; `width_px` insets the 3D viewport to
/// the right of the panel. Bundled into one resource so `panel_ui` stays under Bevy's 16-param cap.
#[derive(Resource, Default)]
struct PanelSeam {
    over_ui: bool,
    width_px: f32,
}

/// The panel's outbound message writers, bundled so `panel_ui` spends ONE system param on all
/// three (Bevy caps a system at 16 params).
#[derive(SystemParam)]
struct PanelWriters<'w> {
    switch: MessageWriter<'w, SwitchFile>,
    autoplace: MessageWriter<'w, AutoPlace>,
    cmd: MessageWriter<'w, PanelCmd>,
}

/// Which modal layout the panel shows. Derived from the editor/print resources — the sub-modes are
/// mutually exclusive (`enforce_exclusive_modes`), so at most one is active; the panel shows ONLY
/// that mode's controls (full-focus), never a pile of buttons that don't apply.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelMode {
    View,              // assembled model: file list, cut cards, slice/explode/print controls
    Connectors(usize), // the 2D connector editor for cut i
    Print,             // the print-orientation preview
}

/// The active panel mode from the editor + print resources. `enforce_exclusive_modes` guarantees
/// they're never both set; the editor wins the tie regardless.
fn panel_mode(edit: &EditCut, print: &PrintView) -> PanelMode {
    if let Some(i) = edit.0 {
        PanelMode::Connectors(i)
    } else if print.0 {
        PanelMode::Print
    } else {
        PanelMode::View
    }
}

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
        source: args
            .iter()
            .find(|a| a.ends_with(".scad"))
            .map(PathBuf::from),
        stl: args.iter().find(|a| a.ends_with(".stl")).map(PathBuf::from),
        bed: [bed[0] as f32, bed[1] as f32],
        root: fab::find_root(),
        tmp: std::env::temp_dir().join("fab-gui"),
        reslice_on_start: args.iter().any(|a| a == "--reslice"),
        cut_pct: flag_value(&args, "--cut")
            .and_then(|v| v.parse().ok())
            .unwrap_or(50.0),
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
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Point the asset server at this crate's `assets/` (where the icon font lives), regardless of CWD.
/// Dev builds use the baked crate path; a packaged .app doesn't have it, so fall back to `assets/`
/// next to the executable, then the bundle's `Contents/Resources/assets`.
fn assets_dir() -> AssetPlugin {
    let dev = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"));
    let file_path = if dev.exists() {
        dev.to_path_buf()
    } else {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let bundled = exe_dir.join("../Resources/assets");
        if bundled.exists() {
            bundled
        } else {
            exe_dir.join("assets")
        }
    };
    AssetPlugin {
        file_path: file_path.to_string_lossy().into_owned(),
        ..default()
    }
}

// ---- windowed -------------------------------------------------------------------------

fn run_windowed(scene: SceneCfg) {
    App::new()
        .add_plugins((
            DefaultPlugins.set(assets_dir()),
            MeshPickingPlugin,
            EguiPlugin::default(),
        ))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .init_resource::<Job>()
        .insert_resource(Parts(vec![Part::default()]))
        .init_resource::<ActivePart>()
        .init_resource::<ActiveConn>()
        .init_resource::<EditCut>()
        .init_resource::<XSection>()
        .init_resource::<PrintView>()
        .init_resource::<PrintJob>()
        .init_resource::<PrintPieces>()
        .init_resource::<AutoJob>()
        .init_resource::<PublishJob>()
        .init_resource::<PrevCam>()
        .init_resource::<Feas>()
        .init_resource::<DraggingCut>()
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .init_resource::<Watch>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceInBackground>()
        .init_resource::<DisplaySpread>()
        .init_resource::<PanelSeam>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .add_observer(on_drag_start)
        .add_observer(on_drag)
        .add_observer(on_drag_end)
        .add_observer(on_click)
        .add_observer(place_on_profile_click)
        .add_observer(orient_piece_on_click)
        .add_systems(Startup, setup_windowed)
        .add_systems(
            Update,
            (
                orbit,
                request_reslice,
                poll_job,
                // Auto-on-open: a fresh too-big model auto-slices + connects (kick), then the plan
                // lands and seeds cuts + connectors (poll). After poll_job so bounds are set.
                (kick_auto_plan, poll_auto_plan).chain().after(poll_job),
                poll_publish,
                (poll_open_dialog, apply_switch_file, watch_source),
                sync_overlays,
                sync_overlay_visuals,
                sync_dim_labels,
                sync_conn_markers,
                edit_mode,
                draw_profile,
                auto_reslice,
                revert_on_edit,
                (auto_scale, split_viewport, seat_bed),
                // The panel's button commands (heavy actions the egui `panel_ui` writes as PanelCmd).
                (
                    toggle_view,
                    publish_action,
                    edit_in_openscad_action,
                    auto_slice_action,
                    export_plates_action,
                ),
                (
                    enforce_exclusive_modes,
                    apply_view_visibility,
                    manage_view_camera,
                    enter_exit_print,
                    // poll seeds the auto-orient, then sync_orientation lays out + flags downgrades.
                    (poll_print_job, sync_orientation).chain(),
                    color_conn_markers,
                    do_auto_place,
                ),
            ),
        )
        // After `orbit` so the corner axis gizmo reads THIS frame's orbit state (no swim/flicker).
        .add_systems(Update, draw_axis_gizmo.after(orbit))
        .add_systems(EguiPrimaryContextPass, panel_ui)
        .run();
}

/// The whole control panel (U.1.2), immediate-mode: a left egui SidePanel that reads + mutates the
/// SAME ECS resources the feathers panel did, one mode at a time (View / Connectors / Print, via
/// `panel_mode`). Cheap edits mutate in place; a heavy action (needing params beyond the panel's
/// own) writes a `PanelCmd` its dedicated system handles. Writes `PanelSeam` after drawing so
/// `orbit` yields the pointer over the panel and `split_viewport` insets the 3D camera beside it.
// TODO(U.1.2): the Material Symbols icon font — text labels ("+"/"on"/"off"/"del"/"conn") for now.
#[allow(clippy::too_many_arguments)]
fn panel_ui(
    mut contexts: EguiContexts,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut active: ResMut<ActiveConn>,
    mut edit: ResMut<EditCut>,
    mut print: ResMut<PrintView>,
    files: Res<FileList>,
    status: Res<Status>,
    dspread: Res<DisplaySpread>,
    job: Res<Job>,
    mut open_dialog: ResMut<OpenDialog>,
    mut writers: PanelWriters,
    mut seam: ResMut<PanelSeam>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // T.2b: the panel edits the ACTIVE part's state (one Part today; N after Increment B).
    let part = &mut parts.0[active_part.0];
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let bounds = &part.bounds;
    let mode = panel_mode(&edit, &print);
    let mut to_remove: Option<usize> = None; // a delete defers past the row loop (index stability)
    // egui 0.35 panels show INTO a Ui, not the Context — wrap the viewport in a background-layer Ui
    // first (the bevy_egui side_panel pattern) so the panel overlays the 3D view.
    let mut viewport = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("panel_viewport"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    let panel = egui::Panel::left("panel")
        .resizable(true)
        .default_size(220.0)
        .show(&mut viewport, |ui| {
            ui.heading("fab-gui");
            ui.separator();
            match mode {
                PanelMode::View => {
                    if ui.button("Open…").clicked() && open_dialog.0.is_none() {
                        open_dialog.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
                            rfd::AsyncFileDialog::new()
                                .pick_folder()
                                .await
                                .map(|h| h.path().to_path_buf())
                        }));
                    }
                    ui.label(format!("files ({})", files.files.len()));
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, path) in files.files.iter().enumerate() {
                                let name = path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "?".into());
                                if ui.selectable_label(files.active == Some(i), name).clicked() {
                                    writers.switch.write(SwitchFile(i));
                                }
                            }
                        });
                    if ui.button("Edit in OpenSCAD").clicked() {
                        writers.cmd.write(PanelCmd::EditOpenscad);
                    }
                    ui.separator();
                    for axis in [Axis::X, Axis::Y, Axis::Z] {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} plane", axis.label()));
                            if ui.button("+").clicked() {
                                if let Some((mn, mx)) = bounds.0 {
                                    let at = comp((mn + mx) * 0.5, axis.index());
                                    cuts.list.push(CutDef {
                                        axis,
                                        at,
                                        enabled: true,
                                    });
                                    cuts.active = cuts.list.len() - 1;
                                }
                            }
                        });
                        let idxs: Vec<usize> = cuts
                            .list
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.axis == axis)
                            .map(|(i, _)| i)
                            .collect();
                        for idx in idxs {
                            ui.horizontal(|ui| {
                                if idx == cuts.active {
                                    ui.label("▶");
                                }
                                let mut at = cuts.list[idx].at;
                                if ui.add(egui::DragValue::new(&mut at).speed(0.5)).changed() {
                                    cuts.list[idx].at = clamp_to_bounds(at, axis, &bounds);
                                    cuts.active = idx;
                                }
                                let en = cuts.list[idx].enabled;
                                if ui
                                    .selectable_label(en, if en { "on" } else { "off" })
                                    .clicked()
                                {
                                    cuts.list[idx].enabled = !en;
                                }
                                if ui
                                    .button(egui::RichText::new("del").color(
                                        egui::Color32::from_rgb(230, 130, 130),
                                    ))
                                    .clicked()
                                {
                                    to_remove = Some(idx);
                                }
                                let editing = edit.0 == Some(idx);
                                if ui.selectable_label(editing, "conn").clicked() {
                                    edit.0 = if editing { None } else { Some(idx) };
                                }
                            });
                        }
                    }
                    ui.separator();
                    if ui.button("Auto-slice").clicked() {
                        writers.cmd.write(PanelCmd::AutoSlice);
                    }
                    // Status — pulses blue while a background render/slice runs (the reactive standard).
                    if job.0.is_some() {
                        let p = ((ui.input(|i| i.time) * 5.0).sin() * 0.5 + 0.5) as f32;
                        let col = egui::Color32::from_rgb(
                            (115.0 + 115.0 * p) as u8,
                            (165.0 + 75.0 * p) as u8,
                            255,
                        );
                        ui.colored_label(col, status.0.as_str());
                        ui.ctx().request_repaint();
                    } else {
                        ui.label(status.0.as_str());
                    }
                    if ui
                        .button(if dspread.0 > 0.0 { "Assemble" } else { "Explode" })
                        .clicked()
                    {
                        writers.cmd.write(PanelCmd::ToggleView);
                    }
                    if ui.button("Print view").clicked() {
                        print.0 = true;
                    }
                    if ui.button("Publish").clicked() {
                        writers.cmd.write(PanelCmd::Publish);
                    }
                }
                PanelMode::Connectors(i) => {
                    let header = match cuts.list.get(i) {
                        Some(c) => {
                            let pos = if c.at.fract() == 0.0 {
                                format!("{}", c.at as i64)
                            } else {
                                format!("{:.1}", c.at)
                            };
                            format!("Connectors: {} cut @ {}", c.axis.label(), pos)
                        }
                        None => "Connectors".to_string(),
                    };
                    ui.label(header);
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(active.kind == fab::ConnKind::Onion, "Onion")
                            .clicked()
                        {
                            active.kind = fab::ConnKind::Onion;
                        }
                        if ui
                            .selectable_label(active.kind == fab::ConnKind::Bolt, "Bolt")
                            .clicked()
                        {
                            active.kind = fab::ConnKind::Bolt;
                        }
                    });
                    if active.kind == fab::ConnKind::Bolt {
                        ui.horizontal(|ui| {
                            for s in [Screw::M3, Screw::M4, Screw::M5] {
                                if ui.selectable_label(active.screw == s, s.label()).clicked() {
                                    active.screw = s;
                                }
                            }
                        });
                    }
                    if ui.button("Auto-place").clicked() {
                        writers.autoplace.write(AutoPlace);
                    }
                    if ui.button("Clear connectors").clicked() {
                        conns.list.retain(|c| c.cut != i);
                    }
                    if ui.button("Done").clicked() {
                        edit.0 = None;
                    }
                }
                PanelMode::Print => {
                    ui.label("Print orientation");
                    ui.label("click a piece to set which way it prints");
                    if ui.button("Export plates").clicked() {
                        writers.cmd.write(PanelCmd::Export);
                    }
                    if ui.button("Done").clicked() {
                        print.0 = false;
                    }
                }
            }
        });
    if let Some(idx) = to_remove {
        remove_cut(cuts, conns, idx);
    }
    seam.width_px = panel.response.rect.width() * ctx.pixels_per_point();
    seam.over_ui = ctx.egui_wants_pointer_input() || ctx.is_pointer_over_egui();
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
    mut gizmo_cfg: ResMut<GizmoConfigStore>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    // Two cameras: a full-window UI camera (draws the panel + clears the dark bg) and the 3D camera,
    // whose viewport `split_viewport` insets to the right of the panel so the model centres in the
    // VISIBLE area. UI layout follows a camera's viewport, so the panel needs its own full-window one.
    // The 3D camera renders FIRST (order 0) and clears the dark bg in its inset; the UI camera renders
    // on top (order 1, no clear) so panel + dimension NUMBERS sit over the model and cut-plane
    // overlays instead of being occluded by them.
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: bevy::camera::ClearColorConfig::None,
            ..default()
        },
        bevy::ui::IsDefaultUiCamera,
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
            ..default()
        },
        Transform::default(),
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
            target: Vec3::ZERO,
        },
        // Renders the model (layer 0) AND the gizmos (layer 1). Keeping gizmos on layer 1 means the
        // full-window Camera2d (layer 0) never re-draws them as flat 2D ghosts — the phantom
        // dimension bracket was exactly that: the 3D leader ortho-projected by the UI camera.
        RenderLayers::from_layers(&[0, 1]),
    ));
    // 3D gizmos live on layer 1 so ONLY the 3D camera draws them (see the Camera3d note above).
    gizmo_cfg
        .config_mut::<DefaultGizmoConfigGroup>()
        .0
        .render_layers = RenderLayers::layer(1);
    // Seed the camera-restore slot with the startup pose, so a mode entered before the first
    // normal-view frame still has something to hand back (manage_view_camera).
    commands.insert_resource(PrevCam(Some((-0.7, 0.5, radius, Vec3::ZERO))));
    // Render the model off-thread; poll_job seeds the first cut when bounds land.
    kick_job(&mut job, &mut status, &scene, false, vec![], vec![], vec![]);
}

fn orbit(
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    dragging: Res<DraggingCut>,
    edit: Res<EditCut>,
    seam: Res<PanelSeam>,
) {
    // Yield the whole gesture (wheel + drag) when the pointer is over the egui panel or a widget
    // wants it (`panel_ui` sets `seam.over_ui` from egui's wants_pointer_input / is_pointer_over_area),
    // so scrolling the file list doesn't ALSO zoom the camera.
    if dragging.0 || edit.0.is_some() || seam.over_ui {
        // A cut plane has the pointer, the connector editor holds a fixed face-on view, or the
        // pointer is over the panel (egui owns the wheel there).
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
        // Zoom per notch is a fraction of the current radius (constant feel at any distance). Line
        // events (a mouse wheel) come one notch at a time; Pixel events (a trackpad) stream many
        // small deltas per gesture, so scale them WAY down or the zoom rockets past what you're on.
        let step = match ev.unit {
            MouseScrollUnit::Line => ev.y * 0.05,
            MouseScrollUnit::Pixel => ev.y * 0.004,
        };
        o.radius = (o.radius * (1.0 - step)).clamp(10.0, 4000.0);
    }
    *t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
}

// ---- cut stack: drag, buttons, overlays -----------------------------------------------

/// Begin dragging when a left-press lands on a cut plane: make it active + let orbit yield.
fn on_drag_start(
    ev: On<Pointer<DragStart>>,
    planes: Query<&CutPlaneViz>,
    dspread: Res<DisplaySpread>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut dragging: ResMut<DraggingCut>,
) {
    let cuts = &mut parts.0[active_part.0].cuts;
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
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
) {
    if !dragging.0 || !planes.contains(ev.entity) {
        return;
    }
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
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
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
) {
    let cuts = &mut parts.0[active_part.0].cuts;
    let Ok(cpv) = planes.get(ev.entity) else {
        return;
    };
    // In the read-only exploded view a plane click does nothing; in editing it selects the cut.
    if dspread.0 == 0.0 {
        cuts.active = cpv.idx;
    }
}

/// Delete cut `idx`, keeping the connectors consistent: connectors store cut indices into the
/// stack, so a bare `remove` would silently re-point survivors at the wrong cut. Drop the deleted
/// cut's connectors and renumber the rest (a connector on a later cut shifts down one).
fn remove_cut(cuts: &mut Cuts, conns: &mut Conns, idx: usize) {
    if idx >= cuts.list.len() {
        return;
    }
    cuts.list.remove(idx);
    if !cuts.list.is_empty() && cuts.active >= cuts.list.len() {
        cuts.active = cuts.list.len() - 1;
    }
    conns.list.retain(|c| c.cut != idx);
    for c in conns.list.iter_mut() {
        if c.cut > idx {
            c.cut -= 1;
        }
    }
}

/// Smallest onion worth placing (mm): below this the wall/slab is too thin for a useful joint, so
/// we decline rather than punch an oversized peg through it.
const MIN_ONION: f32 = 2.0;
/// Material to leave between the onion's equator and the nearest edge / slab face.
const ONION_WALL: f64 = 1.2;
/// Largest onion the auto-sizer will grow to in open material.
const ONION_MAX_D: f64 = 16.0;
/// Max gap between alignment onions (mm) — auto-place guarantees every stretch of a join face is
/// within this of an onion, so no long span sags. The alignment interval, not a fill pitch.
const ONION_SPACING: f64 = 80.0;
/// The onion teardrop's tip reaches r/sin(ang) past centre in the cap (+build) direction. `ang` is
/// set by the piece's print orientation — decided AFTER the onion is sized — so the sizer bounds for
/// the WORST case: the steepest cap the slicer emits (`CAP_ANG_MIN` = 20° in slicing.rs), tip
/// 1/sin(20°) ≈ 2.92·r. Onions near the +build edge shrink so the tip fits at any orientation; they
/// guide alignment for clamp-and-glue, so smaller is fine (chotchki's call).
const ONION_TIP: f64 = 2.9238; // 1 / sin(20°)

/// The onion cap direction (+build = +Z) in a cut's 2D cross-section coords, or `None` when the cap
/// points OUT of the section plane (a Z cut) — there the cap is bounded axially, not in-section.
fn cap_dir_2d(axis: Axis) -> Option<[f64; 2]> {
    match axis {
        Axis::X | Axis::Y => Some([0.0, 1.0]), // +Z is the section's second coord for X/Y cuts
        Axis::Z => None,
    }
}

/// Place a `kind` connector on `cut` at `pos` (onion diameter `size`, or the `screw` for a bolt), or
/// — if the click lands on one already there — remove it (click-to-toggle). Declines a sub-`MIN_ONION`
/// onion (too thin a spot); a bolt has no such gate. Returns a one-line status describing what it did.
fn toggle_connector(
    conns: &mut Conns,
    cut: usize,
    pos: [f32; 2],
    size: f32,
    kind: fab::ConnKind,
    screw: Screw,
) -> &'static str {
    const HIT: f32 = 5.0; // mm; a bit larger than the 3mm marker, so it's a forgiving target
    if let Some(j) = conns
        .list
        .iter()
        .position(|c| c.cut == cut && (c.pos[0] - pos[0]).hypot(c.pos[1] - pos[1]) < HIT)
    {
        conns.list.remove(j);
        "removed connector"
    } else if kind == fab::ConnKind::Onion && size < MIN_ONION {
        "too thin for an onion here"
    } else {
        conns.list.push(PlacedConn {
            cut,
            pos,
            size,
            kind,
            screw,
        });
        match kind {
            fab::ConnKind::Onion => "placed onion",
            fab::ConnKind::Bolt => "placed bolt",
        }
    }
}

/// Auto-size a connector at `pos` on `cut`: the largest onion that FITS — both the cut's
/// cross-section wall (`fit_diameter`) AND the slab thickness either side of the cut (the onion is
/// a sphere, so it reaches d/2 into each piece along the cut axis — cap it so it can't pierce the
/// thinner slab). No lower clamp: a thin spot returns a small (sub-`MIN_ONION`) value and the caller
/// declines. Falls back to a modest default when there's no cross-section (headless `conn` verb).
fn auto_size(
    xsection: &XSection,
    cuts: &Cuts,
    bounds: &ModelBounds,
    cut: usize,
    pos: [f32; 2],
) -> f32 {
    const DEFAULT: f32 = 6.0;
    let axis = cuts.list.get(cut).map(|c| c.axis).unwrap_or(Axis::X);
    let cross = match &xsection.0 {
        Some(loops) => {
            let loops: Vec<Vec<[f64; 2]>> = loops
                .iter()
                .map(|l| l.iter().map(|&[a, b]| [a as f64, b as f64]).collect())
                .collect();
            fab_scad::cross_section::fit_onion(
                &loops,
                [pos[0] as f64, pos[1] as f64],
                ONION_WALL,
                ONION_MAX_D,
                cap_dir_2d(axis),
                ONION_TIP,
            ) as f32
        }
        None => DEFAULT,
    };
    cross.min(axial_cap(cuts, cut, bounds))
}

/// The onion-diameter cap from the slab thickness either side of `cut`. The onion reaches d/2 (the
/// sphere) into each piece along the cut axis, EXCEPT for a Z cut, where the cap points +Z (the
/// build) into the upper slab and reaches √2·d/2 — so that side reserves the tip, not the sphere.
fn axial_cap(cuts: &Cuts, cut: usize, bounds: &ModelBounds) -> f32 {
    let (below, above) = axial_room(cuts, cut, bounds);
    let is_z = cuts
        .list
        .get(cut)
        .map(|c| c.axis == Axis::Z)
        .unwrap_or(false);
    let wall = ONION_WALL as f32;
    let below_d = 2.0 * (below - wall);
    // Z cut: the cap (+Z) reaches into the upper (above) slab as the teardrop tip, √2·r deep.
    let above_d = if is_z {
        2.0 * (above - wall) / ONION_TIP as f32
    } else {
        2.0 * (above - wall)
    };
    below_d.min(above_d).max(0.0)
}

/// The room bordering `cut` along its axis on each side (below, above): distance from the cut to its
/// nearest same-axis neighbour (or the model bound) on each side. Huge (no cap) if bounds are unset.
fn axial_room(cuts: &Cuts, cut: usize, bounds: &ModelBounds) -> (f32, f32) {
    let (Some(c), Some((min, max))) = (cuts.list.get(cut), bounds.0) else {
        return (f32::INFINITY, f32::INFINITY);
    };
    let (ai, at) = (c.axis.index(), c.at);
    let mut below = comp(min, ai);
    let mut above = comp(max, ai);
    for (j, o) in cuts.list.iter().enumerate() {
        if j == cut || o.axis != c.axis {
            continue;
        }
        if o.at <= at && o.at > below {
            below = o.at;
        }
        if o.at >= at && o.at < above {
            above = o.at;
        }
    }
    (at - below, above - at)
}

/// In the 2D connector editor: a click on the (face-on) cut plane drops a connector on the cut
/// being edited, at the clicked point — the precise picking the assembled-model click can't give.
#[allow(clippy::too_many_arguments)] // a Bevy observer — params are dependencies, not a smell
fn place_on_profile_click(
    ev: On<Pointer<Click>>,
    editing: Res<EditCut>,
    planes: Query<&CutPlaneViz>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    xsection: Res<XSection>,
    active: Res<ActiveConn>,
    mut status: ResMut<Status>,
) {
    let part = &mut parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    let conns = &mut part.conns;
    let Some(i) = editing.0 else {
        return;
    };
    if ev.event.button != PointerButton::Primary || planes.get(ev.entity).is_err() {
        return;
    }
    let (Some(hit), Some(c)) = (ev.event.hit.position, cuts.list.get(i)) else {
        return;
    };
    let others: Vec<usize> = (0..3).filter(|&a| a != c.axis.index()).collect();
    let pos = [comp(hit, others[0]), comp(hit, others[1])];
    let size = auto_size(&xsection, &cuts, &bounds, i, pos);
    status.0 = toggle_connector(conns, i, pos, size, active.kind, active.screw).into();
}

/// Auto-place connectors across the OPEN cut's cross-section (#41): a grid of wall-fitting onions
/// over the cut face (`cross_section::auto_place`), each capped by the slab's axial room, replacing
/// that cut's existing connectors with a fresh auto-layout. Manual tweaks (place/remove) still work
/// on top. No-op with a hint if no editor is open.
fn do_auto_place(
    mut ev: MessageReader<AutoPlace>,
    edit: Res<EditCut>,
    xsection: Res<XSection>,
    mut parts: ResMut<Parts>,
    active: Res<ActiveConn>,
    active_part: Res<ActivePart>,
    mut status: ResMut<Status>,
) {
    if ev.read().count() == 0 {
        return;
    }
    let part = &mut parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    let conns = &mut part.conns;
    let (Some(i), Some(loops)) = (edit.0, xsection.0.as_ref()) else {
        status.0 = "open a cut's connector editor to auto-place".into();
        return;
    };
    let loops: Vec<Vec<[f64; 2]>> = loops
        .iter()
        .map(|l| l.iter().map(|&[a, b]| [a as f64, b as f64]).collect())
        .collect();
    let axis = cuts.list.get(i).map(|c| c.axis).unwrap_or(Axis::X);
    let placements = fab_scad::cross_section::auto_place(
        &loops,
        ONION_WALL,
        ONION_MAX_D,
        ONION_SPACING,
        MIN_ONION as f64,
        cap_dir_2d(axis),
        ONION_TIP,
    );
    let cap = axial_cap(&cuts, i, &bounds);

    // Corner clearance: an onion near where ANOTHER cut crosses this one straddles the intersection
    // — the messy jigsaw case. Project each PERPENDICULAR enabled cut into this cross-section's 2D
    // coords (a line at `coord = at`) and drop placements whose footprint (radius d/2 + a wall) would
    // reach it. Cuts on this cut's own axis are parallel — they never cross this face.
    let ai = axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let perp: Vec<(usize, f64)> = cuts
        .list
        .iter()
        .enumerate()
        .filter(|&(j, c)| j != i && c.enabled)
        .filter_map(|(_, c)| match c.axis.index() {
            a if a == others[0] => Some((0, c.at as f64)),
            a if a == others[1] => Some((1, c.at as f64)),
            _ => None, // same axis as this cut → parallel, no intersection line
        })
        .collect();

    conns.list.retain(|c| c.cut != i); // fresh auto-layout for this cut
    let mut n = 0;
    for (p, d) in placements {
        // Skip onions that don't clear the perpendicular cuts (see above).
        let clearance = d / 2.0 + ONION_WALL;
        if perp.iter().any(|&(c, at)| (p[c] - at).abs() < clearance) {
            continue;
        }
        // The fitted onion diameter doubles as a "has room" proxy for a bolt (auto-place fits to the
        // cross-section either way); the active type decides what actually lands here.
        let size = (d as f32).min(cap);
        if size >= MIN_ONION {
            conns.list.push(PlacedConn {
                cut: i,
                pos: [p[0] as f32, p[1] as f32],
                size,
                kind: active.kind,
                screw: active.screw,
            });
            n += 1;
        }
    }
    let noun = match active.kind {
        fab::ConnKind::Onion => "onion",
        fab::ConnKind::Bolt => "bolt",
    };
    info!("auto-place: {n} {noun}(s) on cut {i}");
    status.0 = format!("auto-placed {n} {noun}{}", if n == 1 { "" } else { "s" });
}

/// The Explode/Collapse button: collapse to the uncut model, or explode the last sliced result —
/// auto-slicing first if the cuts changed (or were never sliced), so it works without Re-slice.
fn toggle_view(
    mut ev: MessageReader<PanelCmd>,
    whole: Res<WholeMesh>,
    sliced: Res<SlicedMesh>,
    mut dspread: ResMut<DisplaySpread>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut models: Query<&mut Mesh3d, With<Model>>,
) {
    if !ev.read().any(|c| *c == PanelCmd::ToggleView) {
        return;
    }
    if dspread.0 > 0.0 {
        // Collapse → the uncut model.
        if let Some(h) = whole.0.clone() {
            for mut m in &mut models {
                m.0 = h.clone();
            }
            dspread.0 = 0.0;
        }
    } else if let Some(h) = sliced.0.clone() {
        // Explode the sliced pieces — `auto_reslice` keeps them fresh in the background, and a
        // pending rebuild refreshes them in place when it lands (poll_job, dspread > 0).
        for mut m in &mut models {
            m.0 = h.clone();
        }
        dspread.0 = SPREAD as f32;
    } else {
        // Nothing sliced yet — kick one explicitly; poll_job explodes it when it arrives.
        reslice_w.write(ReSlice);
    }
}

/// A content hash of EXACTLY the inputs the slice depends on — the enabled cuts, the placed
/// connectors, and the per-piece orientations — quantised so float jitter doesn't churn it, and
/// deliberately EXCLUDING UI state like the active cut. `auto_reslice` keys the rebuild on this, not
/// Bevy change-detection, which fires on any `ResMut` deref (re-selecting a cut, a same-value field
/// echo) and would re-slice endlessly.
fn slice_hash(cuts: &Cuts, conns: &Conns, orient: &Orient) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |x: f32| (x as f64 * 1000.0).round() as i64; // 0.001mm — below print resolution
    for c in cuts.list.iter().filter(|c| c.enabled) {
        c.axis.index().hash(&mut h);
        q(c.at).hash(&mut h);
    }
    0xC0FFEE_u64.hash(&mut h); // section marker so [cut] vs [conn] can't alias
    for pc in &conns.list {
        pc.cut.hash(&mut h);
        q(pc.pos[0]).hash(&mut h);
        q(pc.pos[1]).hash(&mut h);
        q(pc.size).hash(&mut h);
        matches!(pc.kind, fab::ConnKind::Bolt).hash(&mut h);
        pc.screw.label().hash(&mut h);
    }
    0xBEEF_u64.hash(&mut h);
    let mut om: Vec<_> = orient.map.iter().collect(); // HashMap — sort for a stable hash
    om.sort_by_key(|(p, _)| **p);
    for (piece, up) in om {
        piece.hash(&mut h);
        up.iter().for_each(|&x| q(x).hash(&mut h));
    }
    h.finish()
}

/// The reactive core (the DAG success criterion): when the slice inputs change, rebuild in the
/// BACKGROUND after a short settle — no Re-slice button. `prev` debounces (reset the clock while the
/// inputs move frame-to-frame, e.g. a cut drag); `sliced_h` records what was last sliced so identical
/// inputs never re-fire. Skips while a job runs (retries once idle) or before the bounds land.
/// `poll_job` refreshes the exploded view in place when the result lands, or banks it if assembled.
#[allow(clippy::too_many_arguments)]
fn auto_reslice(
    time: Res<Time>,
    mut settle: Local<f32>,
    mut prev: Local<Option<u64>>,
    mut sliced_h: Local<Option<u64>>,
    mut job: ResMut<Job>,
    mut bg: ResMut<SliceInBackground>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    cfg: Res<SceneCfg>,
    mut status: ResMut<Status>,
) {
    let part = &parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &part.cuts;
    let conns = &part.conns;
    let orient = &part.orient;
    if bounds.0.is_none() {
        return;
    }
    let h = slice_hash(cuts, conns, orient);
    if *prev != Some(h) {
        *settle = 0.0; // inputs moved this frame → re-arm the debounce
        *prev = Some(h);
    } else {
        *settle += time.delta_secs();
    }
    if *sliced_h == Some(h) || job.0.is_some() {
        return; // already sliced these exact inputs, or a job is running
    }
    if *settle < AUTOSLICE_DEBOUNCE {
        return; // still settling
    }
    let xs = cuts.enabled_cuts();
    if xs.is_empty() {
        *sliced_h = Some(h); // nothing enabled to slice — treat as done
        return;
    }
    bg.0 = true; // background rebuild → poll_job won't jump the view to exploded
    kick_job(
        &mut job,
        &mut status,
        &cfg,
        true,
        xs,
        resolve_conns(&cuts, &conns),
        orient_inputs(&orient),
    );
    *sliced_h = Some(h);
}

/// Keep one sphere marker per placed connector: respawn the set when the count changes, and each
/// frame park each marker at its connector's point (so dragging a cut moves its markers too).
/// Hidden in the exploded view, since the pieces (and their pockets) have fanned apart.
#[allow(clippy::too_many_arguments)]
fn sync_conn_markers(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    dspread: Res<DisplaySpread>,
    print: Res<PrintView>,
    mut last: Local<usize>,
    existing: Query<Entity, With<ConnMarker>>,
    mut markers: Query<(&ConnMarker, &mut Transform, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    let part = &parts.0[active_part.0];
    let conns = &part.conns;
    let cuts = &part.cuts;
    if conns.list.len() != *last {
        *last = conns.list.len();
        for e in &existing {
            commands.entity(e).despawn();
        }
        let mesh = meshes.add(Sphere::new(3.0));
        for i in 0..conns.list.len() {
            // Each marker gets its OWN material so color_conn_markers can tint it by feasibility.
            let mat = mats.add(StandardMaterial {
                base_color: Color::srgb(0.30, 0.85, 0.70),
                unlit: true,
                depth_bias: 1.0e8, // render on top — the connector point sits inside the solid model
                ..default()
            });
            commands.spawn((
                ConnMarker(i),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat),
                Transform::default(),
                Visibility::Hidden,
            ));
        }
        return; // positions land next frame, once the new markers exist
    }
    // Show a marker only in the collapsed assembled view, and only for a connector on an ENABLED
    // cut — one on a disabled cut isn't in the slice, so showing it (teal/clean) would mislead.
    let live = dspread.0 == 0.0 && !print.0;
    for (m, mut tf, mut vis) in &mut markers {
        let point = live
            .then(|| conns.list.get(m.0))
            .flatten()
            .filter(|pc| cuts.list.get(pc.cut).is_some_and(|c| c.enabled))
            .and_then(|pc| conn_point(&cuts, pc));
        match point {
            Some(p) => {
                tf.translation = p;
                *vis = Visibility::Visible;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

/// React to opening/closing a cut's connector editor: compute the cut's cross-section (blocking,
/// but fast — projects the already-rendered preview STL, no re-render) and face the camera onto the
/// cut. Editing is always on the collapsed whole model, so opening drops any explode. The model's
/// visibility + the camera restore on close are owned by `apply_view_visibility` / `manage_view_camera`.
#[allow(clippy::too_many_arguments)]
fn edit_mode(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    cfg: Res<SceneCfg>,
    whole: Res<WholeMesh>,
    mut xsection: ResMut<XSection>,
    mut dspread: ResMut<DisplaySpread>,
    mut models: Query<&mut Mesh3d, With<Model>>,
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    mut status: ResMut<Status>,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if !edit.is_changed() {
        return;
    }
    let Some(i) = edit.0 else {
        xsection.0 = None;
        if !print.0 {
            status.0 = "ready".into(); // closed the editor (unless print took over)
        }
        return;
    };
    // Edit on the collapsed whole model so the profile + the cut plane overlay line up.
    dspread.0 = 0.0;
    if let Some(h) = whole.0.clone() {
        for mut m in &mut models {
            m.0 = h.clone();
        }
    }
    let (Some(c), Some(src)) = (cuts.list.get(i), cfg.source.clone()) else {
        xsection.0 = None;
        return;
    };
    // Face the camera square onto the cut (Z avoids the up=Z gimbal with a near-top-down pitch).
    // Set the transform here directly — `orbit` yields while editing, so it won't apply it for us.
    if let Ok((mut t, mut o)) = cam.single_mut() {
        use std::f32::consts::FRAC_PI_2;
        (o.yaw, o.pitch) = match c.axis {
            Axis::X => (0.0, 0.0),
            Axis::Y => (FRAC_PI_2, 0.0),
            Axis::Z => (-FRAC_PI_2, FRAC_PI_2 - 0.01),
        };
        // Look at the cut's centre: model centre in the non-axis dims, `at` along the axis.
        let center = bounds
            .0
            .map(|(mn, mx)| (mn + mx) * 0.5)
            .unwrap_or(Vec3::ZERO);
        o.target = with_comp(center, c.axis.index(), c.at);
        *t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
    }
    let stl = fab::whole_stl(&src, &cfg.tmp);
    match fab::cross_section(&stl, c.axis.index(), c.at as f64) {
        Ok(loops) => {
            xsection.0 = Some(
                loops
                    .into_iter()
                    .map(|l| l.into_iter().map(|[a, b]| [a as f32, b as f32]).collect())
                    .collect(),
            );
            status.0 = format!("editing connectors on {} cut", c.axis.label());
        }
        Err(e) => {
            status.0 = format!("cross-section failed: {e}");
            xsection.0 = None;
        }
    }
}

/// Draw the open cut's profile as a line-loop outline, plus — at each placed connector — its
/// footprint, drawn PER KIND so the editor shows what you picked: a teal circle at the onion's
/// auto-sized diameter, or an amber circle + cross sized to the bolt's screw head.
fn draw_profile(
    xsection: Res<XSection>,
    edit: Res<EditCut>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut gizmos: Gizmos,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    let (Some(loops), Some(i)) = (&xsection.0, edit.0) else {
        return;
    };
    let Some(c) = cuts.list.get(i) else {
        return;
    };
    let outline = Color::srgb(0.35, 0.8, 1.0);
    for lp in loops {
        let n = lp.len();
        for j in 0..n {
            let a = profile_point(c.axis, c.at, lp[j]);
            let b = profile_point(c.axis, c.at, lp[(j + 1) % n]);
            gizmos.line(a, b, outline);
        }
    }
    // Connector footprints on the cut plane, coloured + shaped by kind (matches the 3D markers).
    let onion_col = Color::srgb(0.30, 0.85, 0.70); // teal
    let bolt_col = Color::srgb(0.95, 0.70, 0.20); // amber
    let rot = Quat::from_rotation_arc(Vec3::Z, c.axis.unit());
    for pc in conns.list.iter().filter(|pc| pc.cut == i) {
        let center = profile_point(c.axis, c.at, pc.pos);
        let iso = Isometry3d::new(center, rot);
        match pc.kind {
            fab::ConnKind::Onion => {
                // The onion is a TEARDROP, cap along the build (+Z): the tip reaches √2·r past centre
                // (ang=45). Draw that, not a bare circle, so the real footprint — and any poke past
                // the profile edge — is visible. When the cut is ⟂ the build (Z-cut) the cap points
                // out of plane, so the 2D footprint IS the equatorial circle.
                let r = pc.size / 2.0;
                let normal = c.axis.unit();
                let cap = Vec3::Z - normal * Vec3::Z.dot(normal); // build, projected into the cut plane
                gizmos.circle(iso, r, onion_col);
                if cap.length() > 1e-3 {
                    let cap = cap.normalize();
                    let tip = center + cap * (r * std::f32::consts::SQRT_2);
                    let t1 = center
                        + (Quat::from_axis_angle(normal, std::f32::consts::FRAC_PI_4) * cap) * r;
                    let t2 = center
                        + (Quat::from_axis_angle(normal, -std::f32::consts::FRAC_PI_4) * cap) * r;
                    gizmos.line(t1, tip, onion_col);
                    gizmos.line(t2, tip, onion_col);
                }
            }
            fab::ConnKind::Bolt => {
                let r = pc.screw.head_r();
                gizmos.circle(iso, r, bolt_col);
                // a small cross so a bolt reads as a screw hole, not just a smaller onion
                let right = rot * (Vec3::X * r);
                let up = rot * (Vec3::Y * r);
                gizmos.line(center - right, center + right, bolt_col);
                gizmos.line(center - up, center + up, bolt_col);
            }
        }
    }
}

/// Keep one overlay per cut. When the cut COUNT changes (add/remove), the index→cut mapping can
/// shift, so despawn all + respawn fresh; positions/colours are then refreshed by
/// sync_overlay_visuals. (Runs on every cut change, but only rebuilds on a count change.)
fn sync_overlays(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    existing: Query<Entity, With<CutPlaneViz>>,
    mut last: Local<usize>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if !parts.is_changed() || *last == cuts.list.len() {
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
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    dspread: Res<DisplaySpread>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut overlays: Query<(
        &mut CutPlaneViz,
        &mut Transform,
        &mut Mesh3d,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if !parts.is_changed() && !dspread.is_changed() {
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
    with_comp(
        (min + max) * 0.5,
        c.axis.index(),
        c.at + spread_offset(cuts, idx, spread),
    )
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

/// Drop the bed slab so its top meets the model's Z-floor — the model rests on the bed instead of
/// dipping below it (its native coords needn't put the bottom at z=0). Runs when the bounds change.
fn seat_bed(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut beds: Query<&mut Transform, With<Bed>>,
) {
    if !parts.is_changed() {
        return;
    }
    let bounds = &parts.0[active_part.0].bounds;
    let Some((min, _)) = bounds.0 else {
        return;
    };
    for mut t in &mut beds {
        t.translation.z = min.z - 0.5; // the slab is 1.0 thick → its top lands at min.z
    }
}

/// Piece-width dimensions for EVERY axis that has an enabled cut: per axis, a leader line parallel to
/// the cut, offset a hair off the part, with end ticks and the width as a white centred number, in
/// that axis's colour (X red / Y green / Z blue). Safe to show all at once now that gizmos render on
/// the 3D camera only — the old "scatter" was the UI camera ghosting each leader, not the extra axes.
#[allow(clippy::too_many_arguments)]
fn sync_dim_labels(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    print: Res<PrintView>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    existing: Query<&DimLabel>,
    mut labels: Query<(&DimLabel, &mut Node, &mut Text, &mut Visibility)>,
    mut commands: Commands,
    mut gizmos: Gizmos,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if print.0 {
        // The print preview lays pieces out apart — the assembled-part width labels don't apply.
        for (_, _, _, mut vis) in &mut labels {
            *vis = Visibility::Hidden;
        }
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let Ok((camera, cam_gt)) = cam.single() else {
        return;
    };
    let gap = ((max - min).max_element() * 0.05).clamp(8.0, 22.0);
    let tick = gap * 0.35;
    let mut segs: Vec<(Vec3, f32)> = Vec::new();
    // Dimension every axis that has an enabled cut — each on its own edge, in its own colour.
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let ai = axis.index();
        let dim_col = match axis {
            Axis::X => Color::srgb(1.0, 0.4, 0.4),
            Axis::Y => Color::srgb(0.4, 0.9, 0.45),
            Axis::Z => Color::srgb(0.5, 0.6, 1.0),
        };
        let mut xs: Vec<f32> = cuts
            .list
            .iter()
            .filter(|c| c.enabled && c.axis == axis)
            .map(|c| c.at)
            .collect();
        if xs.is_empty() {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut edges = vec![comp(min, ai)];
        edges.extend(xs);
        edges.push(comp(max, ai));

        // Leader line offset off the model on p0, at the near face on p1 (index-addressed).
        let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
        let (p0, p1) = (others[0], others[1]);
        let off0 = comp(min, p0) - gap;
        let face1 = comp(min, p1);
        let dim_pt = |v: f32| {
            let mut a = [0.0f32; 3];
            a[ai] = v;
            a[p0] = off0;
            a[p1] = face1;
            Vec3::from_array(a)
        };
        let face_pt = |v: f32| {
            let mut a = [0.0f32; 3];
            a[ai] = v;
            a[p0] = comp(min, p0);
            a[p1] = face1;
            Vec3::from_array(a)
        };
        let tvec = {
            let mut a = [0.0f32; 3];
            a[p0] = tick;
            Vec3::from_array(a)
        };
        // Dimensions stay at the ASSEMBLED positions — they don't ride the explode.
        for w in edges.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            let (a, b) = (dim_pt(lo), dim_pt(hi));
            gizmos.line(a, b, dim_col);
            gizmos.line(face_pt(lo), a, dim_col.with_alpha(0.4));
            gizmos.line(face_pt(hi), b, dim_col.with_alpha(0.4));
            gizmos.line(a - tvec, a + tvec, dim_col);
            gizmos.line(b - tvec, b + tvec, dim_col);
            segs.push(((a + b) * 0.5, w[1] - w[0]));
        }
    }

    // Spawn a label entity for any segment that lacks one (count only grows).
    for i in existing.iter().count()..segs.len() {
        commands.spawn((
            Text::new(""),
            TextColor(Color::srgb(0.95, 0.95, 1.0)),
            TextFont::from_font_size(13.0),
            Node {
                position_type: PositionType::Absolute,
                ..default()
            },
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
                let s = format!("{width:.0}");
                // Centre the number on the anchor — Node left/top is its top-left corner, so back
                // off half the glyph run (~7px/char at size 13) and half the line height.
                node.left = px(p.x - s.len() as f32 * 3.5);
                node.top = px(p.y - 8.0);
                *text = Text::new(s);
                *vis = Visibility::Visible;
            }
            Err(_) => *vis = Visibility::Hidden,
        }
    }
}

/// A small XYZ orientation gizmo pinned to the lower-left of the 3D viewport: arrows along world X
/// (red), Y (green), Z (blue), drawn at a fixed camera-relative offset. Because the arrows point
/// along the WORLD axes but the anchor rides with the camera, it spins as you orbit yet stays put on
/// pan/zoom — the "which way is the origin" indicator.
fn draw_axis_gizmo(cam: Query<(&Orbit, &Projection), With<Camera3d>>, mut gizmos: Gizmos) {
    let Ok((o, proj)) = cam.single() else {
        return;
    };
    let Projection::Perspective(p) = proj else {
        return;
    };
    // Recompute the camera transform from the orbit state THIS frame — the camera's GlobalTransform
    // is a frame stale (propagated in PostUpdate), which makes a camera-locked gizmo swim/flicker.
    let t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
    // Place the anchor at the lower-left of the frustum, a short distance in front of the camera.
    let d = 12.0;
    let half_h = d * (p.fov * 0.5).tan();
    let half_w = half_h * p.aspect_ratio;
    let origin =
        t.translation + t.forward() * d - t.right() * (half_w * 0.85) - t.up() * (half_h * 0.78);
    let len = half_h * 0.22;
    gizmos.arrow(origin, origin + Vec3::X * len, Color::srgb(1.0, 0.4, 0.4)); // X red
    gizmos.arrow(origin, origin + Vec3::Y * len, Color::srgb(0.4, 0.9, 0.45)); // Y green
    gizmos.arrow(origin, origin + Vec3::Z * len, Color::srgb(0.5, 0.6, 1.0)); // Z blue
}

/// On a change of what's displayed, frame it: centre on the (possibly exploded) bounds + fit.
fn auto_scale(
    dspread: Res<DisplaySpread>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut cams: Query<&mut Orbit>,
) {
    if !dspread.is_changed() {
        return;
    }
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let enabled = cuts.list.iter().filter(|c| c.enabled).count() as f32;
    let extra = enabled * dspread.0; // exploded fans pieces this much further along X
    let span = ((max.x - min.x) + extra).max(max.y - min.y).max(80.0);
    for mut o in &mut cams {
        o.target = Vec3::new(
            (min.x + max.x) * 0.5 + extra * 0.5,
            (min.y + max.y) * 0.5,
            (min.z + max.z) * 0.5,
        );
        o.radius = span * 1.3;
    }
}

/// Inset the 3D camera's viewport to the area RIGHT of the panel, so the model auto-frames in the
/// visible region instead of centering behind the floating panel. Keyed off the panel's rendered
/// width (it tracks the file list) and the camera's target size, so it follows resize + panel growth.
fn split_viewport(seam: Res<PanelSeam>, mut cam: Query<&mut Camera, With<Camera3d>>) {
    let Ok(mut camera) = cam.single_mut() else {
        return;
    };
    let Some(target) = camera.physical_target_size() else {
        return;
    };
    // `panel_ui` writes the panel's right edge in physical px (egui SidePanel is flush to the window
    // left, so its width IS the inset); leave a small gap after it.
    let x0 = ((seam.width_px + 6.0).round() as u32).min(target.x.saturating_sub(1));
    let pos = UVec2::new(x0, 0);
    let size = UVec2::new(target.x - x0, target.y.max(1));
    // Viewport isn't PartialEq — compare the fields we set to skip redundant writes.
    let unchanged = camera
        .viewport
        .as_ref()
        .is_some_and(|v| v.physical_position == pos && v.physical_size == size);
    if !unchanged {
        camera.viewport = Some(bevy::camera::Viewport {
            physical_position: pos,
            physical_size: size,
            ..default()
        });
    }
}

/// Revert to the uncut model the moment a cut is edited, so editing is always on the intact part.
fn revert_on_edit(
    parts: Res<Parts>,
    whole: Res<WholeMesh>,
    mut dspread: ResMut<DisplaySpread>,
    mut models: Query<&mut Mesh3d, With<Model>>,
) {
    if dspread.0 == 0.0 || !parts.is_changed() {
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

/// Explicit `ReSlice` (the scripted harness; Explode when there's no slice yet) → slice NOW and
/// show the pieces (foreground). The reactive UI path is `auto_reslice` (background).
fn request_reslice(
    mut ev: MessageReader<ReSlice>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut bg: ResMut<SliceInBackground>,
    cfg: Res<SceneCfg>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
) {
    if ev.read().count() == 0 {
        return;
    }
    if job.0.is_some() {
        info!("busy — ignoring re-slice");
        return;
    }
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    let orient = &part.orient;
    let xs = cuts.enabled_cuts();
    if xs.is_empty() {
        status.0 = "no enabled cuts".into();
        return;
    }
    bg.0 = false; // explicit → poll_job jumps to the exploded view when it lands
    kick_job(
        &mut job,
        &mut status,
        &cfg,
        true,
        xs,
        resolve_conns(&cuts, &conns),
        orient_inputs(&orient),
    );
}

/// The auto-picked (eventually manual) orientations as `fab::Orient3` for `reslice`. Empty until the
/// print-orientation preview runs and seeds the map — then every slice honours them. The slice
/// codegen gates onions / teardrops per SLAB, so this projects the per-component map to slab-level
/// via component 0 (a multi-component slab is presliced ⇒ no connectors ⇒ this gates nothing).
fn orient_inputs(orient: &Orient) -> Vec<fab::Orient3> {
    orient
        .map
        .iter()
        .filter(|((_, comp), _)| *comp == 0)
        .map(|((piece, _), up)| fab::Orient3 {
            piece: *piece,
            up: [up[0] as f64, up[1] as f64, up[2] as f64],
        })
        .collect()
}

/// Map placed connectors to the sliced spec: a connector's stack-cut index → its position in the
/// enabled-cut list (which is what `fab::reslice` indexes). Connectors on a disabled cut drop out.
fn resolve_conns(cuts: &Cuts, conns: &Conns) -> Vec<fab::Conn> {
    let enabled = cuts.enabled_indices();
    conns
        .list
        .iter()
        .filter_map(|pc| {
            enabled
                .iter()
                .position(|&si| si == pc.cut)
                .map(|ei| fab::Conn {
                    cut: ei,
                    pos: [pc.pos[0] as f64, pc.pos[1] as f64],
                    size: pc.size as f64,
                    kind: pc.kind,
                    screw: pc.screw.label(),
                })
        })
        .collect()
}

/// The model-derived resources, bundled so `apply_switch_file` can wipe them in one system param
/// (Bevy caps a system at 16 params; a `SystemParam` struct counts as one). Everything here is a
/// pure function of the current source + user edits — stale the instant a different `.scad` loads.
#[derive(SystemParam)]
struct ModelState<'w> {
    parts: ResMut<'w, Parts>,
    active: ResMut<'w, ActivePart>,
    edit_cut: ResMut<'w, EditCut>,
    xsection: ResMut<'w, XSection>,
    print: ResMut<'w, PrintView>,
    print_job: ResMut<'w, PrintJob>,
    print_pieces: ResMut<'w, PrintPieces>,
    feas: ResMut<'w, Feas>,
    whole: ResMut<'w, WholeMesh>,
    sliced: ResMut<'w, SlicedMesh>,
    watch: ResMut<'w, Watch>,
}

impl ModelState<'_> {
    /// Reset to a clean slate for a freshly-loaded source: no cuts/connectors/orientations, bounds
    /// cleared so `poll_job` re-seeds the first cut, modes exited, cached meshes dropped, any
    /// in-flight print job cancelled, panel signature invalidated (forces a rebuild), and the watch
    /// disarmed so `watch_source` records the new file's mtime instead of re-triggering.
    fn reset(&mut self) {
        *self.parts = Parts(vec![Part::default()]);
        self.active.0 = 0;
        *self.edit_cut = EditCut::default();
        *self.xsection = XSection::default();
        *self.print = PrintView::default();
        *self.print_job = PrintJob::default();
        *self.print_pieces = PrintPieces::default();
        *self.feas = Feas::default();
        *self.whole = WholeMesh::default();
        *self.sliced = SlicedMesh::default();
        *self.watch = Watch::default();
    }
}

/// Apply a pending file switch: point `SceneCfg.source` at file `i`, wipe the old model's state,
/// kick a fresh whole render. Row clicks, the picker landing, and the `open` script verb all funnel
/// here via `SwitchFile`.
fn apply_switch_file(
    mut ev: MessageReader<SwitchFile>,
    mut files: ResMut<FileList>,
    mut scene: ResMut<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut state: ModelState,
) {
    // Coalesce: only the last switch requested this frame matters.
    let Some(SwitchFile(i)) = ev.read().copied().last() else {
        return;
    };
    let Some(path) = files.files.get(i).cloned() else {
        return;
    };
    files.active = Some(i);
    scene.source = Some(path.clone());
    state.reset();
    kick_job(&mut job, &mut status, &scene, false, vec![], vec![], vec![]);
    info!("open: {}", path.display());
}

/// Drain the native folder pick: on a chosen directory, list its `.scad` files and switch to the
/// first; on cancel, nothing. The dialog future was spawned by the Open button.
fn poll_open_dialog(
    mut dlg: ResMut<OpenDialog>,
    mut files: ResMut<FileList>,
    mut switch: MessageWriter<SwitchFile>,
    mut status: ResMut<Status>,
) {
    let Some(task) = dlg.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return; // dialog still open
    };
    dlg.0 = None;
    let Some(dir) = result else {
        return; // cancelled
    };
    let scads = scad_files(&dir);
    if scads.is_empty() {
        status.0 = format!("no .scad under {}", dir.display());
        return;
    }
    files.files = scads;
    switch.write(SwitchFile(0));
}

/// Auto-reload (5.3.3): if the active source's mtime advanced since its last load, re-render the
/// whole model — an external editor / OpenSCAD saved it. Fires only when no job is in flight (which
/// debounces multi-write saves); the cut stack is PRESERVED (re-slice to refresh the exploded view).
fn watch_source(
    scene: Res<SceneCfg>,
    mut watch: ResMut<Watch>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
) {
    let Some(src) = scene.source.as_deref() else {
        return;
    };
    // Resolve the include closure once per (re)load (empty = first sight after a reset); it's
    // re-derived on a real change below so newly added/removed `include`s are tracked.
    if watch.closure.is_empty() {
        watch.closure = dep_closure(src, &scene);
    }
    // Latest mtime across the WHOLE closure — editing any included module advances it.
    let mtime = watch
        .closure
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max();
    match (watch.mtime, mtime) {
        (None, m) => watch.mtime = m, // first sight: arm, don't render (the load already did)
        // A render in flight (mtime advanced but job busy) falls through to `_` and does nothing —
        // the next idle frame retries, so a save mid-render is never lost.
        (Some(prev), Some(m)) if m > prev && job.0.is_none() => {
            watch.mtime = Some(m);
            watch.closure = dep_closure(src, &scene); // the edit may have changed the include set
            info!(
                "reload {} (+{} deps)",
                src.display(),
                watch.closure.len().saturating_sub(1)
            );
            kick_job(&mut job, &mut status, &scene, false, vec![], vec![], vec![]);
        }
        _ => {}
    }
}

/// The transitive `include`/`use` closure of `src`, resolved against the workspace OPENSCADPATH
/// (`root/libs` + `root/scad-lib`) — the files whose edits should trigger a rebuild.
fn dep_closure(src: &Path, scene: &SceneCfg) -> Vec<PathBuf> {
    let search: Vec<PathBuf> = scene
        .root
        .as_ref()
        .map(|r| vec![r.join("libs"), r.join("scad-lib")])
        .unwrap_or_default();
    fab_scad::deps::closure(src, &search).into_iter().collect()
}

/// Every `.scad` under `dir` (recursive), sorted, skipping generated/VCS/hidden dirs. The picker's
/// project→files expansion — handles both flat (`foo/bar.scad`) and `src/`-nested layouts.
fn scad_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_scads(dir, &mut out);
    out.sort();
    out
}

fn collect_scads(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // generated output + VCS + hidden dirs are never source
            if name.starts_with('.') || matches!(name.as_ref(), "out" | "renders" | "target") {
                continue;
            }
            collect_scads(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("scad"))
        {
            out.push(p);
        }
    }
}

/// Spawn the render/slice on the async compute pool (blocking OpenSCAD work, off-thread).
#[allow(clippy::too_many_arguments)]
fn kick_job(
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    reslice: bool,
    cuts: Vec<(char, f64)>,
    conns: Vec<fab::Conn>,
    orient: Vec<fab::Orient3>,
) {
    let Some(src) = cfg.source.clone() else {
        status.0 = "no .scad source".into();
        return;
    };
    let (root, tmp) = (cfg.root.clone(), cfg.tmp.clone());
    let task = AsyncComputeTaskPool::get().spawn(async move {
        if reslice {
            // In-process kernel slice off the cached base (Track C 11.10) — no per-edit spawn. The
            // Solid lives and dies inside reslice_kernel; only the STL path returns (Solid is !Send).
            fab::reslice_kernel(root.as_deref(), &src, &cuts, &conns, &orient, SPREAD, &tmp)
                .map_err(|e| format!("{e:#}"))
        } else {
            fab::render_whole(root.as_deref(), &src, &tmp).map_err(|e| format!("{e:#}"))
        }
    });
    job.0 = Some((reslice, task));
    status.0 = if reslice {
        "slicing".into()
    } else {
        "rendering".into()
    };
}

/// Poll the in-flight job; when it finishes, swap in the new mesh (and seed the first cut once).
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
fn poll_job(
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut whole: ResMut<WholeMesh>,
    mut sliced: ResMut<SlicedMesh>,
    mut dspread: ResMut<DisplaySpread>,
    bg: Res<SliceInBackground>,
    models: Query<Entity, With<Model>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let part = &mut parts.0[active_part.0];
    let bounds = &mut part.bounds;
    let cuts = &mut part.cuts;
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
            if is_reslice {
                sliced.0 = Some(mesh.clone()); // bank it so the view toggle can re-show it
                                               // A BACKGROUND rebuild refreshes the display only if the user is already exploded;
                                               // an explicit slice (or a background one while exploded) shows the fanned pieces.
                let show = !bg.0;
                if show || dspread.0 > 0.0 {
                    for e in &models {
                        commands.entity(e).despawn();
                    }
                    commands.spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(part_material(&mut materials)),
                        Model,
                    ));
                    if show {
                        dspread.0 = SPREAD as f32;
                    }
                }
            } else {
                for e in &models {
                    commands.entity(e).despawn();
                }
                commands.spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(part_material(&mut materials)),
                    Model,
                ));
                whole.0 = Some(mesh); // remember the uncut mesh, so editing can revert to it
                dspread.0 = 0.0;
                // First whole render fixes the bounds. A model that FITS the bed gets a manual
                // centre-cut starting point; one that OVERFLOWS is left empty for auto-on-open
                // (kick_auto_plan) to slice + connect.
                if bounds.0.is_none() {
                    if let Some((min, max)) = aabb {
                        bounds.0 = Some((min, max));
                        let bed = bed_size().unwrap_or([256.0; 3]);
                        let fits = fab_scad::auto_slice::auto_slice(
                            FVec3::new(min.x as f64, min.y as f64, min.z as f64),
                            FVec3::new(max.x as f64, max.y as f64, max.z as f64),
                            Dims::from_array(bed),
                        )
                        .is_empty();
                        if cuts.list.is_empty() && fits {
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

// ---- print-orientation preview --------------------------------------------------------

/// Enter/leave the print-orientation preview on a toggle. Entering hides the model + cut planes and
/// kicks the per-piece render/auto-orient job; leaving despawns the laid-out pieces and restores
/// the model. A `Local` tracks the last state so the initial (false) frame isn't a spurious leave.
#[allow(clippy::too_many_arguments)]
fn enter_exit_print(
    print: Res<PrintView>,
    edit: Res<EditCut>,
    mut was_on: Local<bool>,
    cfg: Res<SceneCfg>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut job: ResMut<PrintJob>,
    mut cache: ResMut<PrintPieces>,
    mut status: ResMut<Status>,
    pieces: Query<Entity, With<PrintPiece>>,
    mut commands: Commands,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    if print.0 == *was_on {
        return; // no transition (and not the initial add) — nothing to do
    }
    *was_on = print.0;
    if print.0 {
        cache.0 = None; // cuts may have moved — wait for a fresh render before laying out
        if job.0.is_none() {
            kick_print_job(
                &mut job,
                &mut status,
                &cfg,
                cuts.enabled_cuts(),
                resolve_conns(&cuts, &conns),
            );
        }
    } else {
        for e in &pieces {
            commands.entity(e).despawn();
        }
        cache.0 = None;
        if edit.0.is_none() {
            status.0 = "ready".into(); // don't clobber the editor when print closed because it opened
        }
    }
    // The model + cut-plane visibility (hidden in the preview, shown otherwise) is owned by
    // apply_view_visibility; the camera hand-back is owned by manage_view_camera.
}

/// Model + cut-plane visibility, derived authoritatively from the active view mode every frame, so
/// a mode transition can never leave the wrong things on screen: the model shows only in normal
/// view (not the 2D editor, not the print preview); the cut planes hide in the print preview.
fn apply_view_visibility(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    mut models: Query<&mut Visibility, (With<Model>, Without<CutPlaneViz>)>,
    mut planes: Query<&mut Visibility, (With<CutPlaneViz>, Without<Model>)>,
) {
    let model_vis = if edit.0.is_none() && !print.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut models {
        if *v != model_vis {
            *v = model_vis;
        }
    }
    let plane_vis = if print.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut v in &mut planes {
        if *v != plane_vis {
            *v = plane_vis;
        }
    }
}

/// The 2D editor and the print preview are mutually-exclusive view modes — if both end up active,
/// keep whichever was just toggled. Reconciles whether the toggle came from a button or the harness.
fn enforce_exclusive_modes(mut edit: ResMut<EditCut>, mut print: ResMut<PrintView>) {
    if !(edit.0.is_some() && print.0) {
        return;
    }
    if edit.is_changed() && !print.is_changed() {
        print.0 = false; // the editor just opened — leave the print preview
    } else {
        edit.0 = None; // print just opened (or both at once) — close the editor
    }
}

/// Save the orbit camera while in normal view and hand it back when a hijacking mode (2D editor,
/// print preview) closes — so leaving a mode restores the pan/orbit/zoom you had, not the mode's
/// view. Writes the transform directly on restore (like `edit_mode` does), so it doesn't depend on
/// `orbit` running that frame.
fn manage_view_camera(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    mut prev: ResMut<PrevCam>,
    mut cams: Query<(&mut Transform, &mut Orbit)>,
    mut was_hijack: Local<bool>,
) {
    let hijack = edit.0.is_some() || print.0;
    let Ok((mut t, mut o)) = cams.single_mut() else {
        *was_hijack = hijack;
        return;
    };
    if !hijack && *was_hijack {
        if let Some((yaw, pitch, radius, target)) = prev.0 {
            (o.yaw, o.pitch, o.radius, o.target) = (yaw, pitch, radius, target);
            *t = orbit_transform(yaw, pitch, radius, target);
        }
    } else if !hijack {
        prev.0 = Some((o.yaw, o.pitch, o.radius, o.target)); // steady normal view — remember it
    }
    *was_hijack = hijack;
}

/// Spawn the per-piece render + auto-orient on the compute pool (the OpenSCAD work is off-thread,
/// so the UI stays live while the plate lays out). Carries the connectors so the preview pieces show
/// their joints.
fn kick_print_job(
    job: &mut PrintJob,
    status: &mut Status,
    cfg: &SceneCfg,
    cuts: Vec<(char, f64)>,
    conns: Vec<fab::Conn>,
) {
    let Some(src) = cfg.source.clone() else {
        status.0 = "no .scad source".into();
        return;
    };
    if cuts.is_empty() {
        status.0 = "no enabled cuts".into();
        return;
    }
    let (root, tmp) = (cfg.root.clone(), cfg.tmp.clone());
    let task = AsyncComputeTaskPool::get().spawn(async move {
        // In-process via the Manifold kernel (11.12): base rendered once, both passes off the cache.
        fab::print_layout_kernel(root.as_deref(), &src, &cuts, &conns, &tmp)
            .map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "orienting pieces".into();
}

/// Poll the print-layout job; when it lands, cache the rendered pieces (so a manual re-orient can
/// re-lay-out without re-rendering) and seed every piece's auto-orientation. A fresh render is a
/// fresh auto-pick: it resets the orientations (dropping prior manual overrides — re-entering the
/// preview is the reset gesture). `relayout_pieces` does the actual layout from here.
fn poll_print_job(
    mut job: ResMut<PrintJob>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut cache: ResMut<PrintPieces>,
    mut status: ResMut<Status>,
) {
    let orient = &mut parts.0[active_part.0].orient;
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    let pieces = match result {
        Ok(p) => p,
        Err(e) => {
            error!("{e}");
            status.0 = format!("error: {e}");
            return;
        }
    };
    orient.map.clear();
    orient.manual.clear();
    for pp in &pieces {
        orient.map.insert((pp.piece, pp.comp), pp.up);
    }
    let n = pieces.len();
    cache.0 = Some(pieces);
    status.0 = format!(
        "{n} piece{}: click a face to set print-down",
        if n == 1 { "" } else { "s" }
    );
}

/// React to a change in cuts / connectors / orientations / cache: recompute every connector's onion
/// feasibility (drives the marker colour + the downgrade count), and — in the preview — lay the
/// cached pieces out rotated to their build-up, shelf-packed and centred on the bed (z=0). ONE
/// system so feasibility and layout share a run: the status's downgrade count is always the count
/// for the orientation just laid out, never a frame stale. Re-runs from the mesh cache (no OpenSCAD).
#[allow(clippy::too_many_arguments)]
fn sync_orientation(
    print: Res<PrintView>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    cache: Res<PrintPieces>,
    cfg: Res<SceneCfg>,
    mut feas: ResMut<Feas>,
    existing: Query<Entity, With<PrintPiece>>,
    mut cams: Query<&mut Orbit>,
    mut status: ResMut<Status>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !(parts.is_changed() || cache.is_changed()) {
        return;
    }
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    let orient = &part.orient;

    // Feasibility: resolve placed connectors to the enabled-cut indexing (tracking their source
    // index), run the SAME gate the slice applies, write flags back aligned with `Conns::list`.
    let enabled = cuts.enabled_indices();
    let mut resolved = Vec::new();
    let mut src = Vec::new();
    for (i, pc) in conns.list.iter().enumerate() {
        if let Some(ei) = enabled.iter().position(|&si| si == pc.cut) {
            resolved.push(fab::Conn {
                cut: ei,
                pos: [pc.pos[0] as f64, pc.pos[1] as f64],
                size: pc.size as f64,
                kind: pc.kind,
                screw: pc.screw.label(),
            });
            src.push(i);
        }
    }
    let mut flags = vec![true; conns.list.len()];
    match fab::conn_feasibility(&cuts.enabled_cuts(), &resolved, &orient_inputs(&orient)) {
        Ok(f) => {
            for (k, ok) in f.into_iter().enumerate() {
                flags[src[k]] = ok;
            }
        }
        Err(e) => error!("feasibility: {e:#}"),
    }
    let down = flags.iter().filter(|&&ok| !ok).count();
    feas.0 = flags;

    // Layout: only in the preview, only once a render has populated the cache.
    if !print.0 {
        return;
    }
    let Some(pieces) = &cache.0 else {
        return;
    };
    for e in &existing {
        commands.entity(e).despawn();
    }

    // Pass 1: shelf-pack from the origin (walk a row left→right, wrap at the bed width), each piece
    // rotated to its build-up. Translation lands the rotated min-corner at the cursor, on z=0.
    let gap = 6.0_f32;
    let bw = cfg.bed[0];
    let (mut cx, mut cy, mut row_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let (mut bb_x, mut bb_y, mut bb_z) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut placed: Vec<(usize, Quat, Vec3)> = Vec::new(); // (piece index, rotation, translation)
    for (i, pp) in pieces.iter().enumerate() {
        let up = Vec3::from_array(orient.up_or((pp.piece, pp.comp), pp.up)).normalize_or_zero();
        let rot = if up == Vec3::ZERO {
            Quat::IDENTITY
        } else {
            Quat::from_rotation_arc(up, Vec3::Z)
        };
        let (rmin, rmax) = rotated_bounds(&pp.mesh.positions, rot);
        let (w, h) = (rmax.x - rmin.x, rmax.y - rmin.y);
        if cx > 0.0 && cx + w > bw {
            cx = 0.0;
            cy += row_h + gap;
            row_h = 0.0;
        }
        placed.push((i, rot, Vec3::new(cx - rmin.x, cy - rmin.y, -rmin.z)));
        bb_x = bb_x.max(cx + w);
        bb_y = bb_y.max(cy + h);
        bb_z = bb_z.max(rmax.z - rmin.z); // a tilted piece stands tall — frame for it too
        cx += w + gap;
        row_h = row_h.max(h);
    }

    // Pass 2: centre the block on the bed origin (like a slicer auto-arrange) and spawn.
    let shift = Vec3::new(-bb_x * 0.5, -bb_y * 0.5, 0.0);
    for &(i, rot, t) in &placed {
        let mat = materials.add(StandardMaterial {
            base_color: Color::hsl((i as f32 * 47.0) % 360.0, 0.55, 0.55),
            perceptual_roughness: 0.7,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(build_mesh(&pieces[i].mesh))),
            MeshMaterial3d(mat),
            Transform {
                translation: t + shift,
                rotation: rot,
                ..default()
            },
            PrintPiece((pieces[i].piece, pieces[i].comp)),
        ));
    }
    // Frame the laid-out block — including a tall tilted piece (account for height, raise the target).
    let span = bb_x.max(bb_y).max(bb_z).max(80.0);
    for mut o in &mut cams {
        o.target = Vec3::new(0.0, 0.0, bb_z * 0.4);
        o.radius = span * 1.3;
    }
    let n = pieces.len();
    status.0 = if down > 0 {
        format!(
            "{n} pieces, {down} onion{} -> bolt (this orientation)",
            if down == 1 { "" } else { "s" }
        )
    } else {
        format!("{n} pieces oriented, onions print clean")
    };
}

/// Click a piece's face in the preview to lay that face on the bed: the new build-up is the
/// model-space direction opposite the clicked face (the piece's current rotation maps model→world,
/// so invert it to bring the hit normal back to model space). Recorded as a manual override.
fn orient_piece_on_click(
    ev: On<Pointer<Click>>,
    print: Res<PrintView>,
    pieces: Query<(&PrintPiece, &Transform)>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
) {
    if !print.0 || ev.event.button != PointerButton::Primary {
        return;
    }
    let (Ok((pp, tf)), Some(world_n)) = (pieces.get(ev.entity), ev.event.hit.normal) else {
        return;
    };
    let up_model = -(tf.rotation.inverse() * world_n);
    parts.0[active_part.0]
        .orient
        .set_manual(pp.0, up_model.normalize_or_zero().to_array());
}

/// Colour each connector marker by kind + feasibility: amber = a bolt (explicit); teal = an onion
/// that prints support-free; red = an onion that can't and downgrades to a bolt under the current
/// orientations. Live feedback in the assembled/exploded view.
fn color_conn_markers(
    feas: Res<Feas>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    markers: Query<(&ConnMarker, &MeshMaterial3d<StandardMaterial>)>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let conns = &parts.0[active_part.0].conns;
    for (m, mat) in &markers {
        let want = match conns.list.get(m.0).map(|c| c.kind) {
            Some(fab::ConnKind::Bolt) => Color::srgb(0.95, 0.70, 0.20), // amber = bolt
            _ if feas.0.get(m.0).copied().unwrap_or(true) => Color::srgb(0.30, 0.85, 0.70), // teal onion
            _ => Color::srgb(0.95, 0.35, 0.25), // red = onion that downgrades to a bolt
        };
        if let Some(mut material) = mats.get_mut(&mat.0) {
            if material.base_color != want {
                material.base_color = want;
            }
        }
    }
}

/// AABB of `positions` after applying `rot` (the print-up rotation), for shelf-packing the piece.
fn rotated_bounds(positions: &[[f32; 3]], rot: Quat) -> (Vec3, Vec3) {
    let mut it = positions.iter().map(|p| rot * Vec3::from_array(*p));
    let first = it.next().unwrap_or(Vec3::ZERO);
    let (mut min, mut max) = (first, first);
    for v in it {
        min = min.min(v);
        max = max.max(v);
    }
    (min, max)
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
        .add_plugins(EguiPlugin::default())
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .insert_resource(ScreenshotPng(png))
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .insert_resource(Parts(vec![Part::default()]))
        .init_resource::<ActivePart>()
        .init_resource::<ActiveConn>()
        .init_resource::<EditCut>()
        .init_resource::<PrintView>()
        .init_resource::<XSection>()
        .init_resource::<DraggingCut>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceInBackground>()
        .init_resource::<DisplaySpread>()
        .init_resource::<Job>()
        .init_resource::<PanelSeam>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .add_systems(Startup, setup_offscreen)
        .add_systems(Update, (capture_then_exit, split_viewport, seat_bed))
        .add_systems(EguiPrimaryContextPass, panel_ui)
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
    commands.spawn((
        Mesh3d(display),
        MeshMaterial3d(part_material(&mut materials)),
        Model,
    ));

    // Offscreen render target the camera draws into and we screenshot.
    let (w, h) = (960u32, 720u32);
    let mut img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);

    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        bevy::ui::IsDefaultUiCamera,
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: bevy::camera::ClearColorConfig::None,
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        orbit_transform(-0.7, 0.5, radius, Vec3::ZERO),
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
        let cut = CutDef {
            axis: Axis::X,
            at: cut_x,
            enabled: true,
        };
        spawn_cut_plane(commands, meshes, materials, min, max, &cut, 0);
    }
    if !scene.reslice_on_start {
        return whole_mesh;
    }
    match fab::reslice(
        scene.root.as_deref(),
        src,
        &[('x', cut_x as f64)],
        &[],
        &[],
        SPREAD,
        &scene.tmp,
    ) {
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
    Cut(f32),                     // set the ACTIVE cut's position (along its axis)
    AddCut(f32),                  // add a cut at this position (on the active axis), make it active
    SetAxis(Axis),                // set the active cut's axis
    Toggle,                       // toggle the active cut on/off
    Next,                         // cycle the active cut
    Reslice,                      // trigger a slice, then wait for the async job
    Shot(PathBuf),                // screenshot the viewport to this path
    Wait(u32),                    // idle this many frames
    Conn(usize, f32, f32), // place a connector on cut <i> at (a, b) in its plane's non-axis dims
    Edit(usize),           // open cut <i>'s 2D connector editor
    PrintView,             // toggle the print-orientation preview (renders + auto-orients pieces)
    Orient([usize; 3], [f32; 3]), // manually set piece [ix,iy,iz]'s build-up to (ux,uy,uz)
    AutoPlace,             // auto-place connectors across the open cut's cross-section
    ConnType(fab::ConnKind), // set the active connector kind for new placements (onion|bolt)
    Open(PathBuf), // switch the active source to <path> (a dir → its .scad; a file → itself)
    Touch(PathBuf), // bump <path>'s mtime (rewrite same bytes) → exercise watch_source reload
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
                "conntype" => match it.next()? {
                    "onion" => Some(Action::ConnType(fab::ConnKind::Onion)),
                    "bolt" => Some(Action::ConnType(fab::ConnKind::Bolt)),
                    _ => None,
                },
                "open" => it.next().map(|p| Action::Open(PathBuf::from(p))),
                "touch" => it.next().map(|p| Action::Touch(PathBuf::from(p))),
                "reslice" => Some(Action::Reslice),
                "shot" => it.next().map(|p| Action::Shot(PathBuf::from(p))),
                "wait" => it.next()?.parse().ok().map(Action::Wait),
                "conn" => {
                    let i = it.next()?.parse().ok()?;
                    let a = it.next()?.parse().ok()?;
                    let b = it.next()?.parse().ok()?;
                    Some(Action::Conn(i, a, b))
                }
                "edit" => it.next()?.parse().ok().map(Action::Edit),
                "printview" => Some(Action::PrintView),
                "autoplace" => Some(Action::AutoPlace),
                "orient" => {
                    let piece = [
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                    ];
                    let up = [
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                    ];
                    Some(Action::Orient(piece, up))
                }
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
        .add_plugins(EguiPlugin::default())
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .init_resource::<Job>()
        .insert_resource(Parts(vec![Part::default()]))
        .init_resource::<ActivePart>()
        .init_resource::<ActiveConn>()
        .init_resource::<EditCut>()
        .init_resource::<XSection>()
        .init_resource::<PrintView>()
        .init_resource::<PrintJob>()
        .init_resource::<PrintPieces>()
        .init_resource::<PrevCam>()
        .init_resource::<Feas>()
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .init_resource::<Watch>()
        .init_resource::<WholeMesh>()
        .init_resource::<SlicedMesh>()
        .init_resource::<SliceInBackground>()
        .init_resource::<DisplaySpread>()
        .init_resource::<PanelSeam>()
        .insert_resource(Status("rendering".into()))
        .insert_resource(ScriptRunner {
            actions,
            idx: 0,
            timer: 0,
        })
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .add_systems(Startup, setup_script)
        .add_systems(
            Update,
            (
                request_reslice,
                poll_job,
                apply_switch_file,
                watch_source,
                sync_overlays,
                sync_overlay_visuals,
                sync_dim_labels,
                sync_conn_markers,
                edit_mode,
                draw_profile,
                auto_reslice,
                revert_on_edit,
                (
                    enforce_exclusive_modes,
                    apply_view_visibility,
                    manage_view_camera,
                    enter_exit_print,
                    // poll seeds the auto-orient, then sync_orientation lays out + flags downgrades.
                    (poll_print_job, sync_orientation).chain(),
                    color_conn_markers,
                    do_auto_place,
                    split_viewport,
                    seat_bed,
                ),
                run_script,
            ),
        )
        .add_systems(EguiPrimaryContextPass, panel_ui)
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
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        bevy::ui::IsDefaultUiCamera,
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: bevy::camera::ClearColorConfig::None,
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        orbit_transform(-0.7, 0.5, radius, Vec3::ZERO),
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
            target: Vec3::ZERO,
        },
    ));
    commands.insert_resource(PrevCam(Some((-0.7, 0.5, radius, Vec3::ZERO))));
    commands.insert_resource(RenderTargetImage(target));
    kick_job(&mut job, &mut status, &scene, false, vec![], vec![], vec![]);
}

/// Step the script: each action drives the real systems, waiting on async work to settle.
#[allow(clippy::too_many_arguments)]
fn run_script(
    mut runner: ResMut<ScriptRunner>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    job: Res<Job>,
    target: Res<RenderTargetImage>,
    mut edit_cut: ResMut<EditCut>,
    mut print: ResMut<PrintView>,
    print_job: Res<PrintJob>,
    xsection: Res<XSection>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut autoplace_w: MessageWriter<AutoPlace>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    // Bundled: Bevy caps a system at 16 params, and a tuple counts as one.
    mut sw: (
        ResMut<FileList>,
        MessageWriter<SwitchFile>,
        ResMut<ActiveConn>,
    ),
) {
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let orient = &mut part.orient;
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
                cuts.list.push(CutDef {
                    axis,
                    at,
                    enabled: true,
                });
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
        Action::Conn(i, a, b) => {
            if runner.timer == 1 {
                let size = auto_size(&xsection, &cuts, &bounds, i, [a, b]);
                toggle_connector(conns, i, [a, b], size, sw.2.kind, sw.2.screw);
            }
            runner.timer >= 2
        }
        Action::ConnType(k) => {
            if runner.timer == 1 {
                sw.2.kind = k;
            }
            runner.timer >= 2
        }
        Action::Edit(i) => {
            if runner.timer == 1 {
                edit_cut.0 = if edit_cut.0 == Some(i) { None } else { Some(i) };
            }
            runner.timer >= 10 // give the cross-section render + profile build time
        }
        Action::PrintView => {
            if runner.timer == 1 {
                print.0 = !print.0;
            }
            // enter_exit_print kicks the render next frame; wait for the off-thread layout to land.
            runner.timer > 3 && print_job.0.is_none()
        }
        Action::Orient(piece, up) => {
            if runner.timer == 1 {
                orient.set_manual((piece, 0), Vec3::from_array(up).normalize_or_zero().to_array());
            }
            runner.timer >= 3 // let relayout + feasibility catch up
        }
        Action::AutoPlace => {
            if runner.timer == 1 {
                autoplace_w.write(AutoPlace);
            }
            runner.timer >= 3 // let do_auto_place run + conns update
        }
        Action::Open(path) => {
            if runner.timer == 1 {
                let list = if path.is_dir() {
                    scad_files(&path)
                } else {
                    vec![path.clone()]
                };
                if list.is_empty() {
                    eprintln!("script: open — no .scad under {}", path.display());
                } else {
                    sw.0.files = list;
                    sw.1.write(SwitchFile(0));
                }
            }
            // apply_switch_file clears bounds → None; the top guard pauses run_script until the new
            // whole render lands (bounds Some again + job idle).
            runner.timer >= 2 && job.0.is_none()
        }
        Action::Touch(path) => {
            if runner.timer == 1 {
                // Rewrite identical bytes to bump the mtime — watch_source should catch it and
                // re-render. Fails quietly if the path is gone (the test asserts on the log).
                let _ = std::fs::read(&path).and_then(|b| std::fs::write(&path, b));
            }
            // Generous fixed floor so watch_source detects + the reload render completes; if watch
            // never fires, job stays idle and this still ends — the log grep is the real assertion.
            runner.timer >= 90 && job.0.is_none()
        }
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
        Bed,
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
            // The cut plane sits INSIDE the solid model, so without this the model occludes it and
            // its coplanar faces z-fight (the "layering" mess). Bias it to the front like the
            // connector markers do (line ~1205) → a clean translucent guide seen through the part.
            depth_bias: 1.0e8,
            ..default()
        })),
        Transform::from_translation(with_comp((min + max) * 0.5, cut.axis.index(), cut.at)),
        CutPlaneViz {
            idx,
            axis: cut.axis,
        },
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
    let off = Vec3::new(
        radius * cp * yaw.cos(),
        radius * cp * yaw.sin(),
        radius * pitch.sin(),
    );
    Transform::from_translation(target + off).looking_at(target, Vec3::Z)
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

/// The default printer's bed, read from fab-scad's printers.toml via the shared lib.
fn bed_size() -> Option<[f64; 3]> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml = dir.join("printers.toml");
        if toml.exists() {
            let printers = fab_scad::printers::load(&toml).ok()?;
            return fab_scad::printers::select(&printers, None)
                .ok()
                .map(|p| p.bed);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Publish the active model to hotchkiss.io off-thread: render the cover + low-`$fn` preview + full
/// STL and upload them via `fab_scad::publish::publish_model`, reusing the CLI's exact path. Auth +
/// base URL come from `$HIO_API_KEY` / `$HIO_URL`; title/description from the project.toml.
fn publish_action(
    mut ev: MessageReader<PanelCmd>,
    scene: Res<SceneCfg>,
    mut job: ResMut<PublishJob>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::Publish) {
        return;
    }
    if job.0.is_some() {
        status.0 = "already publishing…".into();
        return;
    }
    let Some(src) = scene.source.clone() else {
        status.0 = "no .scad to publish".into();
        return;
    };
    let Ok(key) = std::env::var("HIO_API_KEY") else {
        status.0 = "set $HIO_API_KEY to publish".into();
        return;
    };
    let base = std::env::var("HIO_URL").unwrap_or_else(|_| "https://hotchkiss.io".to_string());
    let (root, out) = (scene.root.clone(), scene.tmp.join("publish"));
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let oscad = fab_scad::openscad::Openscad::discover(root.as_deref())
            .map_err(|e| format!("{e:#}"))?;
        // Title/description from the nearest project.toml; fall back to the file stem.
        let (title, description) = match fab_scad::manifest::Manifest::load_near(&src) {
            Ok(m) => {
                let title = m.title().to_string();
                (title, m.publish.map(|p| p.description).unwrap_or_default())
            }
            Err(_) => (
                src.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "model".into()),
                String::new(),
            ),
        };
        fab_scad::publish::publish_model(
            &oscad,
            &src,
            &title,
            &description,
            &base,
            &key,
            &out,
            std::time::Duration::from_secs(180),
        )
        .map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "publishing…".into();
}

/// Land the publish job: show the URL, or the error.
fn poll_publish(mut job: ResMut<PublishJob>, mut status: ResMut<Status>) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(url) => {
            status.0 = format!("published → {url}");
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("publish failed: {e}"),
    }
}

/// Open the active `.scad` source in the OpenSCAD GUI (detached) so you can edit it; the file-watch
/// re-renders here on save.
fn edit_in_openscad_action(
    mut ev: MessageReader<PanelCmd>,
    scene: Res<SceneCfg>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::EditOpenscad) {
        return;
    }
    let Some(src) = scene.source.clone() else {
        status.0 = "no .scad source to edit".into();
        return;
    };
    match fab::open_in_openscad(scene.root.as_deref(), &src) {
        Ok(()) => status.0 = format!("opened {} in OpenSCAD", src.display()),
        Err(e) => status.0 = format!("couldn't launch OpenSCAD: {e:#}"),
    }
}

/// Auto-slice the loaded model to fit the printer bed: replace the cut stack with
/// `fab_scad::auto_slice`'s plan (equal division on each overflowing axis), clear connectors (they
/// referenced the old cuts), and let the reactive loop reslice. The seed you then refine by hand.
fn auto_slice_action(
    mut ev: MessageReader<PanelCmd>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::AutoSlice) {
        return;
    }
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let Some((min, max)) = bounds.0 else {
        status.0 = "no model loaded yet".into();
        return;
    };
    let bed = bed_size().unwrap_or([256.0; 3]);
    let (lo, hi) = (
        [min.x as f64, min.y as f64, min.z as f64],
        [max.x as f64, max.y as f64, max.z as f64],
    );
    let planned = fab_scad::auto_slice::auto_slice(
        FVec3::from_array(lo),
        FVec3::from_array(hi),
        Dims::from_array(bed),
    );
    if planned.is_empty() {
        status.0 = "model already fits the bed — no cuts needed".into();
        return;
    }
    cuts.list = planned
        .iter()
        .map(|c| CutDef {
            axis: match c.axis {
                0 => Axis::X,
                1 => Axis::Y,
                _ => Axis::Z,
            },
            at: c.at as f32,
            enabled: true,
        })
        .collect();
    cuts.active = 0;
    conns.list.clear(); // the old connectors referenced the replaced cut stack
    let pieces = fab_scad::auto_slice::piece_count(
        FVec3::from_array(lo),
        FVec3::from_array(hi),
        Dims::from_array(bed),
    );
    status.0 = format!("auto-sliced: {} cut(s) → {pieces} piece(s)", planned.len());
    info!("{}", status.0);
}

/// Auto-on-open: when a fresh model that OVERFLOWS the bed finishes its whole render, kick the
/// auto-plan (auto-slice + onion auto-place, off-thread) — ONCE per source. Fits-the-bed models,
/// already-planned sources, and ones that already have cuts are left alone.
fn kick_auto_plan(
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    scene: Res<SceneCfg>,
    mut job: ResMut<AutoJob>,
    mut status: ResMut<Status>,
) {
    if job.0.is_some() {
        return; // one already in flight
    }
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &part.cuts;
    let planned = &mut part.auto_planned;
    let (Some((min, max)), Some(src)) = (bounds.0, scene.source.clone()) else {
        return;
    };
    if planned.0.as_deref() == Some(src.as_path()) || !cuts.list.is_empty() {
        return; // already planned this source, or it already has cuts
    }
    let (lo, hi) = (
        [min.x as f64, min.y as f64, min.z as f64],
        [max.x as f64, max.y as f64, max.z as f64],
    );
    let bed = bed_size().unwrap_or([256.0; 3]);
    if fab_scad::auto_slice::auto_slice(
        FVec3::from_array(lo),
        FVec3::from_array(hi),
        Dims::from_array(bed),
    )
    .is_empty()
    {
        return; // fits the bed — nothing to auto
    }
    let base_stl = fab::whole_stl(&src, &scene.tmp);
    if !base_stl.exists() {
        return; // base not rendered to disk yet
    }
    planned.0 = Some(src.clone()); // fire once per source
    let task = AsyncComputeTaskPool::get().spawn(async move {
        // In-process cross-sections — the base Solid lives + dies inside fab::auto_plan (!Send).
        fab::auto_plan(&base_stl, lo, hi, bed).map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "auto-planning…".into();
}

/// Land the auto-plan: seed the cut stack + connectors from it, and the reactive loop reslices.
fn poll_auto_plan(
    mut job: ResMut<AutoJob>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut status: ResMut<Status>,
) {
    let part = &mut parts.0[active_part.0];
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(plan) => {
            cuts.list = plan
                .cuts
                .iter()
                .map(|&(ax, at)| CutDef {
                    axis: match ax {
                        'y' => Axis::Y,
                        'z' => Axis::Z,
                        _ => Axis::X,
                    },
                    at: at as f32,
                    enabled: true,
                })
                .collect();
            cuts.active = 0;
            conns.list = plan
                .connectors
                .iter()
                .map(|c| PlacedConn {
                    cut: c.cut,
                    pos: [c.pos[0].f() as f32, c.pos[1].f() as f32],
                    size: c.size.unwrap_or(6.0) as f32,
                    kind: if c.kind == "bolt" {
                        fab::ConnKind::Bolt
                    } else {
                        fab::ConnKind::Onion
                    },
                    screw: Screw::M3,
                })
                .collect();
            status.0 = format!(
                "auto-planned: {} cut(s), {} connector(s)",
                cuts.list.len(),
                conns.list.len()
            );
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("auto-plan failed: {e:#}"),
    }
}

/// Between-piece + edge spacing left on the export plates (mm).
const PLATE_GAP: f64 = 5.0;

/// Export the print-oriented pieces as a Bambu multi-plate project `.3mf` next to the source. Runs
/// inline — a handful of piece meshes to a zip is quick — and the status line reports the plate count
/// + fill so you can see how tight it packed. The bed comes from the loaded scene, so it must match
/// the printer the project opens on (Bambu bins pieces to plates by position).
fn export_plates_action(
    mut ev: MessageReader<PanelCmd>,
    pieces: Res<PrintPieces>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    scene: Res<SceneCfg>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::Export) {
        return;
    }
    let orient = &parts.0[active_part.0].orient;
    let Some(list) = pieces.0.as_ref().filter(|l| !l.is_empty()) else {
        status.0 = "no pieces to export — slice first".into();
        return;
    };
    // Resolve each piece's build-up: the manual override if set, else the auto-pick.
    let ups: Vec<[f64; 3]> = list
        .iter()
        .map(|pp| {
            let u = orient.up_or((pp.piece, pp.comp), pp.up);
            [u[0] as f64, u[1] as f64, u[2] as f64]
        })
        .collect();
    let out = match &scene.source {
        Some(s) => {
            let stem = s.file_stem().and_then(|n| n.to_str()).unwrap_or("part");
            s.with_file_name(format!("{stem}-plates.3mf"))
        }
        None => scene.tmp.join("plates.3mf"),
    };
    let bed = [scene.bed[0] as f64, scene.bed[1] as f64];
    match fab::export_plates(list, &ups, bed, PLATE_GAP, &out) {
        Ok(sum) => {
            status.0 = format!(
                "exported {} piece(s) on {} plate(s), {}% full → {}",
                sum.pieces,
                sum.plates,
                (sum.fill * 100.0).round() as i32,
                out.display()
            );
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("export failed: {e:#}"),
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
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 5.0,
                    enabled: false,
                },
                CutDef {
                    axis: Axis::Y,
                    at: 20.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        assert_eq!(cuts.enabled_cuts(), vec![('x', -10.0), ('y', 20.0)]);
    }

    #[test]
    fn toggle_connector_places_then_removes_on_a_second_nearby_click() {
        let mut conns = Conns::default();
        let onion = fab::ConnKind::Onion;
        toggle_connector(&mut conns, 0, [20.0, -10.0], 10.0, onion, Screw::M3); // place
        assert_eq!(conns.list.len(), 1);
        toggle_connector(&mut conns, 0, [22.0, -8.0], 10.0, onion, Screw::M3); // within 5mm → removes it
        assert!(conns.list.is_empty());
        toggle_connector(&mut conns, 0, [20.0, -10.0], 10.0, onion, Screw::M3); // place again
        toggle_connector(&mut conns, 0, [0.0, 0.0], 10.0, onion, Screw::M3); // far away → a second
        assert_eq!(conns.list.len(), 2);
        toggle_connector(&mut conns, 1, [20.0, -10.0], 10.0, onion, Screw::M3); // diff cut → places
        assert_eq!(conns.list.len(), 3);
    }

    #[test]
    fn toggle_connector_declines_a_too_thin_onion_but_not_a_bolt() {
        let mut conns = Conns::default();
        toggle_connector(
            &mut conns,
            0,
            [0.0, 0.0],
            1.0,
            fab::ConnKind::Onion,
            Screw::M3,
        ); // sub-MIN_ONION
        assert!(conns.list.is_empty(), "a too-thin onion is declined");
        toggle_connector(
            &mut conns,
            0,
            [0.0, 0.0],
            5.0,
            fab::ConnKind::Onion,
            Screw::M3,
        ); // fits
        assert_eq!(conns.list.len(), 1);
        // A bolt has no onion thin-gate — it places regardless of the fitted diameter.
        toggle_connector(
            &mut conns,
            1,
            [0.0, 0.0],
            1.0,
            fab::ConnKind::Bolt,
            Screw::M4,
        );
        assert_eq!(conns.list.len(), 2);
        assert!(matches!(conns.list[1].kind, fab::ConnKind::Bolt));
    }

    #[test]
    fn axial_room_reports_both_bordering_slabs() {
        let cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 0.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 16.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        let bounds = ModelBounds(Some((Vec3::splat(-20.0), Vec3::splat(20.0))));
        // middle cut: (below to -10 = 10, above to 16 = 16)
        assert_eq!(axial_room(&cuts, 1, &bounds), (10.0, 16.0));
        // first cut: (below to the -20 bound = 10, above to the cut at 0 = 10)
        assert_eq!(axial_room(&cuts, 0, &bounds), (10.0, 10.0));
        // last cut: (below to the cut at 0 = 16, above to the +20 bound = 4 — the thin end slab)
        assert_eq!(axial_room(&cuts, 2, &bounds), (16.0, 4.0));
    }

    #[test]
    fn remove_cut_renumbers_surviving_connectors() {
        let mut cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 0.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 10.0,
                    enabled: true,
                },
            ],
            active: 2,
        };
        let onion = fab::ConnKind::Onion;
        let mut conns = Conns {
            list: vec![
                PlacedConn {
                    cut: 0,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                },
                PlacedConn {
                    cut: 1,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                }, // deleted
                PlacedConn {
                    cut: 2,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                }, // shifts down
            ],
        };
        remove_cut(&mut cuts, &mut conns, 1);
        assert_eq!(cuts.list.len(), 2);
        assert_eq!(cuts.active, 1); // clamped from 2
        let cuts_of: Vec<usize> = conns.list.iter().map(|c| c.cut).collect();
        assert_eq!(cuts_of, vec![0, 1]); // cut-0 connector kept; cut-1 dropped; cut-2 → cut-1
    }

    #[test]
    fn spread_offset_is_per_axis() {
        // Two X cuts + one Y cut. The second X cut (rank 1) sits in the gap above one X piece;
        // the Y cut (rank 0 on its own axis) is unaffected by the X cuts.
        let cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 20.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::Y,
                    at: 0.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        assert_eq!(spread_offset(&cuts, 0, 10.0), 5.0); // X rank 0 → (0+0.5)*10
        assert_eq!(spread_offset(&cuts, 1, 10.0), 15.0); // X rank 1 → (1+0.5)*10
        assert_eq!(spread_offset(&cuts, 2, 10.0), 5.0); // Y rank 0 → (0+0.5)*10
    }
}
