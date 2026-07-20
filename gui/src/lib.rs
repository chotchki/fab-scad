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
    camera::{RenderTarget, visibility::RenderLayers},
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
        view::screenshot::{Screenshot, save_to_disk},
    },
    tasks::{AsyncComputeTaskPool, Task, block_on, futures_lite::future},
    window::ExitCondition,
    winit::WinitPlugin,
};
pub(crate) use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext, egui,
};
pub(crate) use fab_scad::stl;
// The geometry-service WIRE types (W.3): ungated (no kernel), so they name the seam on BOTH targets.
// `SolidId` is the base-solid handle a `Part` holds; the rest are the request/response envelope the
// task bodies drive `GeomPool` with. The Solid itself lives in the service, never here.
pub(crate) use fab_scad::geomsg::{Quality, Request, Response, SolidId, Source, WireConn};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::path::{Path, PathBuf};
// The shared geometry types the auto-slice/planner APIs take (J.6 unified on `fab_lang`'s Vec3). Aliased
// `FVec3` so it doesn't shadow Bevy's `Vec3`, which the scene code uses everywhere. Native-only: the sole
// user is `kick_auto_plan`'s bed-overflow pre-check (auto_slice), which is desktop-side (W.3.4).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use fab_lang::{Dims, Vec3 as FVec3};

mod config;
mod console; // W.3.16 — the in-app console (echo/warnings + tracing), a bottom-panel expander
mod customize;
mod cuts;
// Web lib-closure delivery (W.3.6 Stage 2) — pure scan/normalize/BFS (native-tested) + the wasm fetch.
#[cfg(test)]
mod harness_tests;
mod highlight;
mod jobs;
#[cfg(any(target_arch = "wasm32", test))]
mod lib_fetch;
mod panel;
mod print;
mod render_quality; // W.3.25.2 — the live view's Draft|Final quality (a global the render kicks read)
#[cfg(not(target_arch = "wasm32"))]
mod settings; // W.3.27 — the desktop Settings modal (hotchkiss.io publish key); native only
// Web save-back target derivation (W.5) — pure URL logic (native-tested), no web-sys. Derives the
// `PUT /media/<ref>/variants` target from the `?model=` deep-link; the wasm boot reads the param.
#[cfg(any(target_arch = "wasm32", test))]
mod save_target;
mod scene;
mod screenshot;
mod script;
mod state;
mod theme; // W.1 — central egui Visuals/Style + fonts + the 3D palette, ported from hotchkiss.io
mod view; // U.3.11 — headless script-driven state-assertion tests for the Parts drill
#[allow(unused_imports)]
// each module re-exports its whole surface; the builders below use most of it
pub(crate) use {
    cuts::*, jobs::*, panel::*, print::*, scene::*, screenshot::*, script::*, state::*, view::*,
};
// Explicit: an explicit use outranks the globs — bevy_input's `Axis<T>` collides in the prelude.
pub(crate) use state::Axis;

mod fab;
// The native geometry-service transport (W.3.3) — a pool of kernel threads. Uses fab_scad::geomsvc
// (kernel), so native-only; the wasm Worker transport lands at W.3.6. `GeomPool` is the cloneable
// Bevy Resource every render/slice/plan/section op routes through — the ONE geometry path.
#[cfg(not(target_arch = "wasm32"))]
pub mod geom;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use geom::GeomPool;
// wasm's transport (W.3.6): GeomPool talks to the fab-geom Web Worker over postMessage (worker_rpc),
// the transport twin of the native kernel-thread pool — same app systems, a canvas-vs-thread swap.
#[cfg(target_arch = "wasm32")]
pub mod geom_wasm;
#[cfg(target_arch = "wasm32")]
mod web_host;
#[cfg(target_arch = "wasm32")]
mod worker_rpc;
#[cfg(target_arch = "wasm32")]
pub(crate) use geom_wasm::GeomPool;

/// Native entry: parse args + dispatch to the windowed / screenshot / scripted app builder. The wasm
/// build enters through a `#[wasm_bindgen(start)]` on the canvas instead (W.3.2), not here.
pub fn native_entry() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (bed, plate) = bed_size().unwrap_or(([256.0; 3], [256.0; 3]));
    let source = args
        .iter()
        .find(|a| a.ends_with(".scad"))
        .map(PathBuf::from);
    // Prefer the OPENED MODEL's location for the workspace root over cwd (W.3.21): a double-clicked
    // `.app` launches with cwd `/`, so a cwd-based `find_root` returns None → no library paths → BOSL2
    // unresolvable → every module undefined → empty render. Walk up from the .scad's dir first; fall
    // back to cwd for a sourceless launch (dev `cargo run` from the workspace).
    let root = source
        .as_deref()
        .and_then(|p| p.parent())
        .and_then(fab::find_root_from)
        .or_else(fab::find_root);
    let cfg = SceneCfg {
        source,
        stl: args.iter().find(|a| a.ends_with(".stl")).map(PathBuf::from),
        bed: [bed[0] as f32, bed[1] as f32, bed[2] as f32],
        plate: [plate[0] as f32, plate[1] as f32],
        root,
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

/// wasm entry (W.3.5): the browser calls this on module load. Binds Bevy to the page's
/// `<canvas id="fab-gui">` (via [`window_plugin`]) and boots the SAME windowed app as desktop over an
/// EMPTY scene — geometry lands when the W.3.6 Worker fills the `GeomPool` stub. The panic hook routes
/// Rust panics to the console (a bare wasm trap is otherwise opaque). This is the egui-on-wasm smoke.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    run_windowed(
        SceneCfg {
            source: None,
            stl: None,
            bed: [256.0, 256.0, 256.0], // web has no printers.toml — a sane default the model's fab:config overrides
            plate: [256.0, 256.0],
            root: None,
            tmp: PathBuf::from("/tmp/fab-gui"),
            reslice_on_start: false,
            cut_pct: 50.0,
        },
        None,
    );
}

// ---- windowed -------------------------------------------------------------------------
fn run_windowed(scene: SceneCfg, shot: Option<PathBuf>) {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .set(assets_dir())
            .set(window_plugin())
            // W.3.16: mirror the tracing stream into the in-app console (the "Full" feed) — the only
            // way to see the app's logs on web, where there's no terminal.
            .set(bevy::log::LogPlugin {
                custom_layer: console::log_layer,
                // W.3.23: admit DEBUG from OUR crates so the console's level dropdown can reach it; keep
                // bevy/wgpu at their defaults so DEBUG doesn't flood. (The console still shows INFO by
                // default; on native the terminal also gets fab DEBUG — a dev-tool tradeoff, and the web
                // has no terminal.)
                filter: "wgpu=error,naga=warn,fab_scad=debug,fab_lang=debug,fab_gui=debug,\
                         fab_manifold=debug,fab_geom=debug"
                    .into(),
                ..default()
            }),
        MeshPickingPlugin,
        EguiPlugin::default(),
    ))
    .insert_resource(ClearColor(theme::VIEWPORT))
    .insert_resource(scene)
    // The geometry service (W.3.3): one kernel-thread shard to start. Every render/slice/plan/
    // section op routes through it — Solids stay on this thread, only bytes/handles cross.
    .insert_resource(GeomPool::new(1))
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
    .init_resource::<CoPack>()
    .init_resource::<Platform>()
    .init_resource::<Pipeline>()
    .init_resource::<AutoJob>()
    .init_resource::<PublishJob>()
    // W.5.7: the save-back target (derived from `?model=` on wasm; always None on desktop → no Save
    // affordance).
    .init_resource::<SaveTarget>()
    .init_resource::<console::ConsoleUi>()
    .init_resource::<Tab>()
    .init_resource::<theme::ThemeReady>()
    .init_resource::<EditorBuf>()
    .init_resource::<PrevCam>()
    .init_resource::<Feas>()
    .init_resource::<DraggingCut>()
    .init_resource::<FileList>()
    .init_resource::<OpenDialog>()
    .init_resource::<Watch>()
    .init_resource::<SliceInBackground>()
    .init_resource::<PendingConfig>()
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
            (auto_reslice, revert_on_edit),
            (auto_scale, split_viewport, seat_bed, resize_bed),
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
                estimate_copack, // U.3.5 — reactive co-pack metric for the Export tab
                sync_pipeline,   // U.3.7 — per-node stale flags + busy for the tab-bar feedback
            ),
        ),
    )
    // After `orbit` so the corner axis gizmo reads THIS frame's orbit state (no swim/flicker).
    .add_systems(Update, draw_axis_gizmo.after(orbit))
    .add_systems(Update, window_shot)
    .add_systems(
        EguiPrimaryContextPass,
        (
            theme::install_theme,
            panel_ui.run_if(theme::theme_ready),
            // The host's splash-removal cue, fired once the themed UI is up (docs/web-embed.md).
            signal_ready.run_if(theme::theme_ready),
        )
            .chain(),
    );
    // W.3.27: the desktop Settings modal (hotchkiss.io publish key). Native only — the web publishes via
    // the site session cookie, so there's no key to set there. Draws its own egui Modal in the egui pass.
    #[cfg(not(target_arch = "wasm32"))]
    app.init_resource::<settings::SettingsUi>().add_systems(
        EguiPrimaryContextPass,
        settings::settings_modal.run_if(theme::theme_ready),
    );
    // Browser-only file-IO surface (W.3.12): the `?model=` fetch resource + its landing system, plus
    // the save-back (W.5.7/.8): derive the `PUT /media/<ref>/variants` target from the SAME `?model=`
    // deep-link (the stable ref rides its path — no separate param), which gates the Save affordance,
    // and run the save-mesh export + upload job.
    #[cfg(target_arch = "wasm32")]
    app.init_resource::<jobs::ModelFetch>()
        .init_resource::<jobs::SaveJob>()
        .insert_resource(SaveTarget(
            crate::web_host::query_param("model")
                .as_deref()
                .and_then(save_target::derive),
        ))
        // W.5.9: `?e2e=save` auto-fires the Save once the model renders (headless boot-gate hook).
        .insert_resource(jobs::E2eSave(
            crate::web_host::query_param("e2e").as_deref() == Some("save"),
        ))
        .add_systems(
            Update,
            (
                jobs::poll_model_fetch,
                jobs::save_action,
                jobs::poll_save,
                jobs::e2e_autosave,
            ),
        );
    app.run();
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
        .insert_resource(ClearColor(theme::VIEWPORT))
        .insert_resource(scene)
        .insert_resource(GeomPool::new(1)) // the geometry service (W.3.3); setup_offscreen renders through it
        .insert_resource(ScreenshotPng(png))
        .init_resource::<FileList>()
        .init_resource::<OpenDialog>()
        .insert_resource(Parts(vec![Part::default()]))
        .init_resource::<ActivePart>()
        .init_resource::<ActiveConn>()
        .init_resource::<EditCut>()
        .init_resource::<PrintView>()
        .init_resource::<Tab>()
        .init_resource::<theme::ThemeReady>()
        .init_resource::<EditorBuf>()
        .init_resource::<XSection>()
        .init_resource::<DraggingCut>()
        .init_resource::<SliceInBackground>()
        .init_resource::<Job>()
        .init_resource::<PendingConfig>()
        .init_resource::<PanelSeam>()
        .init_resource::<PrintPieces>()
        .init_resource::<CoPack>()
        .init_resource::<Platform>()
        .init_resource::<Pipeline>()
        // panel_ui reads SaveTarget (W.5) + ConsoleUi (W.3.16); the harness apps must init them too or
        // panel_ui panics on a missing resource.
        .init_resource::<SaveTarget>()
        .init_resource::<console::ConsoleUi>()
        .insert_resource(Status("rendering".into()))
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .add_systems(Startup, setup_offscreen)
        .add_systems(Update, (capture_then_exit, split_viewport, seat_bed))
        .add_systems(
            EguiPrimaryContextPass,
            (theme::install_theme, panel_ui.run_if(theme::theme_ready)).chain(),
        )
        .run();
}

/// Headless, but runs the FULL windowed systems + an offscreen camera, then walks the script.
fn run_scripted(scene: SceneCfg, actions: Vec<Action>) {
    let mut app = App::new();
    app.add_plugins(
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
    .insert_resource(ClearColor(theme::VIEWPORT))
    .insert_resource(scene)
    .insert_resource(GeomPool::new(1)) // the geometry service (W.3.3); setup_script + the poll loop drive it
    .init_resource::<Job>()
    .insert_resource(Parts(vec![Part::default()]))
    .init_resource::<ActivePart>()
    .init_resource::<ActiveConn>()
    .init_resource::<EditCut>()
    .init_resource::<XSection>()
    .init_resource::<PrintView>()
    .init_resource::<PrintJob>()
    .init_resource::<PrintPieces>()
    .init_resource::<CoPack>()
    .init_resource::<Platform>()
    .init_resource::<AutoJob>() // sync_pipeline reads it for the busy/loading feedback
    .init_resource::<Pipeline>()
    .init_resource::<SaveTarget>() // panel_ui reads it (W.5); default None = no Save affordance
    .init_resource::<console::ConsoleUi>() // panel_ui reads it (W.3.16)
    .init_resource::<Tab>()
    .init_resource::<theme::ThemeReady>()
    .init_resource::<EditorBuf>()
    .init_resource::<PrevCam>()
    .init_resource::<Feas>()
    .init_resource::<FileList>()
    .init_resource::<OpenDialog>()
    .init_resource::<Watch>()
    .init_resource::<SliceInBackground>()
    .init_resource::<PendingConfig>()
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
            // Auto-on-open (kick) + the plan landing (poll) — the offscreen harness ran WITHOUT
            // these, so it never auto-sliced an overflowing part (unfaithful to run_windowed).
            (kick_auto_plan, poll_auto_plan).chain().after(poll_job),
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
                resize_bed,
                export_plates_action, // the `export` script verb → co-pack .3mf (T.2b.4)
            ),
            sync_pipeline, // U.3.7 feedback: keep the offscreen harness faithful to run_windowed
            run_script,
        ),
    )
    .add_systems(
        EguiPrimaryContextPass,
        (theme::install_theme, panel_ui.run_if(theme::theme_ready)).chain(),
    );
    // W.3.27: the Settings modal in the scripted harness — a headless real-frame check of the publish
    // key screen (the `settings` verb opens it). Native only (settings/credentials are desktop-only).
    #[cfg(not(target_arch = "wasm32"))]
    app.init_resource::<settings::SettingsUi>().add_systems(
        EguiPrimaryContextPass,
        settings::settings_modal.run_if(theme::theme_ready),
    );
    app.run();
}
