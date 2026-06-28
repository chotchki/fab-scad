//! fab-gui — the slicing GUI (Phase 5.1). A Bevy 0.19 viewport over a model, with the printer
//! bed for reference and a Feathers control panel. "Re-slice" drives `fab` in-process (the
//! shared `fab_scad` lib) and swaps in the sliced result. Modes:
//!
//!   cargo run -p fab-gui -- part.scad                       # windowed: orbit, Re-slice
//!   cargo run -p fab-gui -- part.scad --screenshot out.png  # headless render to PNG (self-verify)
//!   cargo run -p fab-gui -- part.scad --screenshot out.png --reslice   # ...showing the sliced result

use std::path::{Path, PathBuf};

use bevy::{
    app::ScheduleRunnerPlugin,
    asset::RenderAssetUsages,
    camera::RenderTarget,
    feathers::{
        controls::{FeathersButton, FeathersSlider},
        dark_theme::create_dark_theme,
        theme::{ThemeBackgroundColor, ThemedText, UiTheme},
        tokens, FeathersPlugins,
    },
    image::Image,
    input::mouse::{MouseMotion, MouseWheel},
    mesh::Indices,
    prelude::*,
    render::{
        render_resource::{PrimitiveTopology, TextureFormat, TextureUsages},
        view::screenshot::{save_to_disk, Screenshot},
    },
    scene::{Scene, SceneList}, // the bsn traits — shadow the prelude's `Scene` asset struct
    ui_widgets::{Activate, SliderStep},
    window::ExitCondition,
    winit::WinitPlugin,
};

mod fab;
mod stl;

/// Scene inputs shared by both modes.
#[derive(Resource, Clone)]
struct SceneCfg {
    source: Option<PathBuf>, // .scad source (sliceable, preferred)
    stl: Option<PathBuf>,    // .stl to display directly (when there's no source)
    bed: [f32; 2],
    root: Option<PathBuf>, // workspace root, for OPENSCADPATH
    tmp: PathBuf,          // scratch dir for rendered/sliced STLs
    reslice_on_start: bool, // screenshot --reslice: display the sliced result
}

/// Marks the displayed model entity, so re-slice can swap it out.
#[derive(Component)]
struct Model;

/// Button → "re-slice the source and swap the mesh".
#[derive(Message)]
struct ReSlice;

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
    };
    match flag_value(&args, "--screenshot") {
        Some(png) => run_screenshot(cfg, PathBuf::from(png)),
        None => run_windowed(cfg),
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

// ---- windowed -------------------------------------------------------------------------

fn run_windowed(scene: SceneCfg) {
    App::new()
        .add_plugins((DefaultPlugins, FeathersPlugins))
        .insert_resource(UiTheme(create_dark_theme()))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .insert_resource(scene)
        .add_message::<ReSlice>()
        .add_systems(Startup, (setup_windowed, ui_root.spawn()))
        .add_systems(Update, (orbit, do_reslice))
        .run();
}

#[derive(Component)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
}

fn setup_windowed(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    scene: Res<SceneCfg>,
) {
    spawn_world(&mut commands, &mut meshes, &mut materials, &scene, scene.reslice_on_start);
    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera3d::default(),
        Transform::default(),
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
        },
    ));
}

fn orbit(
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
) {
    let Ok((mut t, mut o)) = cam.single_mut() else {
        return;
    };
    if buttons.pressed(MouseButton::Left) {
        for ev in motion.read() {
            o.yaw -= ev.delta.x * 0.008;
            o.pitch = (o.pitch + ev.delta.y * 0.008).clamp(-1.5, 1.5);
        }
    } else {
        motion.clear();
    }
    for ev in wheel.read() {
        o.radius = (o.radius * (1.0 - ev.y * 0.1)).clamp(10.0, 4000.0);
    }
    *t = orbit_transform(o.yaw, o.pitch, o.radius);
}

/// On a Re-slice message: slice the source in-process and swap in the pieces.
fn do_reslice(
    mut ev: MessageReader<ReSlice>,
    cfg: Res<SceneCfg>,
    models: Query<Entity, With<Model>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if ev.read().count() == 0 {
        return;
    }
    let Some(src) = cfg.source.as_deref() else {
        warn!("no .scad source to slice (launch fab-gui with a source .scad)");
        return;
    };
    info!("re-slicing {}", src.display());
    match fab::reslice(cfg.root.as_deref(), src, 0.0, 50.0, &cfg.tmp) {
        Ok(stl) => {
            for e in &models {
                commands.entity(e).despawn();
            }
            let mesh = load_model(&mut meshes, Some(&stl));
            commands.spawn((Mesh3d(mesh), MeshMaterial3d(part_material(&mut materials)), Model));
        }
        Err(e) => error!("re-slice failed: {e:#}"),
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
        .add_systems(Startup, (setup_offscreen, ui_root.spawn()))
        .add_systems(Update, capture_then_exit)
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
    spawn_world(&mut commands, &mut meshes, &mut materials, &scene, scene.reslice_on_start);

    // Offscreen render target the camera draws into and we screenshot.
    let (w, h) = (960u32, 720u32);
    let mut img = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);

    let radius = scene.bed[0].max(scene.bed[1]).max(80.0);
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        orbit_transform(-0.7, 0.5, radius),
        // No window here, so make this offscreen camera the UI target (so the panel renders).
        bevy::ui::IsDefaultUiCamera,
    ));

    commands.insert_resource(Shot {
        target,
        png: png.0.clone(),
        frame: 0,
        captured: false,
    });
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

// ---- shared scene ---------------------------------------------------------------------

fn spawn_world(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cfg: &SceneCfg,
    reslice: bool,
) {
    let stl = resolve_stl(cfg, reslice);
    let mesh = load_model(meshes, stl.as_deref());
    commands.spawn((Mesh3d(mesh), MeshMaterial3d(part_material(materials)), Model));

    // Printer bed reference at z=0 (from the shared fab_scad::printers lib).
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(cfg.bed[0], cfg.bed[1], 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.18, 0.18, 0.22),
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -0.5),
    ));

    // Key + fill directional lights (Z-up).
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

/// The STL to display: render the source whole (or sliced, if `reslice`), else the given .stl.
fn resolve_stl(cfg: &SceneCfg, reslice: bool) -> Option<PathBuf> {
    if let Some(src) = cfg.source.as_deref() {
        let r = if reslice {
            fab::reslice(cfg.root.as_deref(), src, 0.0, 50.0, &cfg.tmp)
        } else {
            fab::render_whole(cfg.root.as_deref(), src, &cfg.tmp)
        };
        match r {
            Ok(p) => Some(p),
            Err(e) => {
                error!("{e:#}");
                None
            }
        }
    } else {
        cfg.stl.clone()
    }
}

fn load_model(meshes: &mut Assets<Mesh>, stl: Option<&Path>) -> Handle<Mesh> {
    match stl {
        Some(p) if p.exists() => match stl::load_stl(p) {
            Ok(s) => {
                info!("loaded {} ({} tris)", p.display(), s.positions.len() / 3);
                meshes.add(build_mesh(&s))
            }
            Err(e) => {
                error!("loading {}: {e:#}", p.display());
                meshes.add(Cuboid::new(60.0, 40.0, 30.0))
            }
        },
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

/// Camera transform orbiting the origin at (yaw, pitch, radius), Z-up.
fn orbit_transform(yaw: f32, pitch: f32, radius: f32) -> Transform {
    let cp = pitch.cos();
    let pos = Vec3::new(radius * cp * yaw.cos(), radius * cp * yaw.sin(), radius * pitch.sin());
    Transform::from_translation(pos).looking_at(Vec3::ZERO, Vec3::Z)
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

// ---- Feathers UI ----------------------------------------------------------------------

/// The control panel as a bsn scene: a title, a cut-position slider, and a Re-slice button.
fn ui_root() -> impl SceneList {
    bsn_list![panel()]
}

fn panel() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            left: px(8),
            flex_direction: FlexDirection::Column,
            row_gap: px(8),
            padding: UiRect::all(px(10)),
            min_width: px(190),
        }
        ThemeBackgroundColor(tokens::WINDOW_BG)
        Children[
            (Text("fab-gui") ThemedText),
            (Text("cut position") ThemedText),
            (@FeathersSlider { @max: 100.0, @value: 50.0 } SliderStep(1.0)),
            (
                @FeathersButton { @caption: bsn!{ Text("Re-slice") ThemedText } }
                on(|_: On<Activate>, mut w: MessageWriter<ReSlice>| { w.write(ReSlice); })
            ),
        ]
    }
}
