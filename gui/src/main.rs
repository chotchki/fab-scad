//! fab-gui — the slicing GUI (Phase 5.1). A Bevy 0.19 viewport over a model, with the printer
//! bed for reference and an egui control panel. A STACK of cut planes (each draggable in 3D
//! and toggleable on/off) drives `fab` in-process (the shared `fab_scad` lib) ON A BACKGROUND
//! THREAD; Re-slice swaps in the result. The cut stack is the unit a DAG cache will key on:
//! a slice is a pure function of (source, enabled cuts). Modes:
//!
//!   cargo run -p fab-gui -- part.scad                       # windowed: orbit, drag cuts, Re-slice
//!   cargo run -p fab-gui -- part.scad --shot out.png        # windowed self-verify: REAL window capture
//!   cargo run -p fab-gui -- part.scad --screenshot out.png  # headless render to PNG (self-verify)
//!   cargo run -p fab-gui -- part.scad --script "addcut 30; reslice; shot a.png"  # scripted harness

pub(crate) use bevy::ecs::system::SystemParam;
pub(crate) use bevy::{
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
pub(crate) use bevy_egui::{
    egui, EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext,
};
pub(crate) use fab_scad::stl;
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::path::{Path, PathBuf};
// The shared geometry types the auto-slice/planner APIs take (J.6 unified on `fab_lang`'s Vec3). Aliased
// `FVec3` so it doesn't shadow Bevy's `Vec3`, which the scene code uses everywhere.
pub(crate) use fab_lang::{Dims, Vec3 as FVec3};

mod cuts;
#[cfg(test)]
mod harness_tests;
mod jobs;
mod panel;
mod print;
mod scene;
mod screenshot;
mod script;
mod state;
mod view; // U.3.11 — headless script-driven state-assertion tests for the Parts drill
#[allow(unused_imports)]
// each module re-exports its whole surface; the builders below use most of it
pub(crate) use {
    cuts::*, jobs::*, panel::*, print::*, scene::*, screenshot::*, script::*, state::*, view::*,
};
// Explicit: an explicit use outranks the globs — bevy_input's `Axis<T>` collides in the prelude.
pub(crate) use state::Axis;

mod fab;

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
        run_windowed(cfg, flag_value(&args, "--shot").map(PathBuf::from));
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// ---- windowed -------------------------------------------------------------------------
fn run_windowed(scene: SceneCfg, shot: Option<PathBuf>) {
    App::new()
        .add_plugins((
            DefaultPlugins.set(assets_dir()),
            MeshPickingPlugin,
            EguiPlugin::default(),
        ))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .insert_resource(WindowShot(shot))
        // U.3.9: we pin the primary egui context to the full-window Camera2d ourselves (see
        // setup_windowed). Auto-create picks the "first found" camera — an archetype-order lottery.
        .insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        })
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
        .init_resource::<Tab>()
        .init_resource::<EditorBuf>()
        .init_resource::<PrevCam>()
        .init_resource::<Feas>()
        .init_resource::<DraggingCut>()
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .init_resource::<Watch>()
        .init_resource::<SliceInBackground>()
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
                (
                    poll_open_dialog,
                    apply_switch_file,
                    watch_source,
                    preview_edited_buffer,
                ),
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
                    auto_slice_action,
                    export_plates_action,
                ),
                (
                    sync_tab_modes,
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
        .add_systems(Update, window_shot)
        .add_systems(EguiPrimaryContextPass, (install_fonts, panel_ui).chain())
        .run();
}

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
        // U.3.9: explicit primary context (see run_windowed) — no auto-attach lottery.
        .insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        })
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
        .init_resource::<Tab>()
        .init_resource::<EditorBuf>()
        .init_resource::<XSection>()
        .init_resource::<DraggingCut>()
        .init_resource::<SliceInBackground>()
        .init_resource::<Job>()
        .init_resource::<PanelSeam>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .add_systems(Startup, setup_offscreen)
        .add_systems(Update, (capture_then_exit, split_viewport, seat_bed))
        .add_systems(EguiPrimaryContextPass, (install_fonts, panel_ui).chain())
        .run();
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
        // U.3.9: explicit primary context (see run_windowed) — no auto-attach lottery.
        .insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        })
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
        .init_resource::<Tab>()
        .init_resource::<EditorBuf>()
        .init_resource::<PrevCam>()
        .init_resource::<Feas>()
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .init_resource::<Watch>()
        .init_resource::<SliceInBackground>()
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
                preview_edited_buffer,
                sync_overlays,
                sync_overlay_visuals,
                sync_dim_labels,
                sync_conn_markers,
                edit_mode,
                draw_profile,
                auto_reslice,
                revert_on_edit,
                (
                    sync_tab_modes,
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
                    export_plates_action, // the `export` script verb → co-pack .3mf (T.2b.4)
                ),
                run_script,
            ),
        )
        .add_systems(EguiPrimaryContextPass, (install_fonts, panel_ui).chain())
        .run();
}
