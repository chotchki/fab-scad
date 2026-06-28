//! fab-gui — the slicing GUI (Phase 5.1). A Bevy 0.19 viewport over a `fab`-rendered mesh
//! with the printer bed for reference. Two entry modes:
//!
//!   cargo run -p fab-gui -- part.stl                 # windowed: orbit, Feathers UI
//!   cargo run -p fab-gui -- --screenshot out.png part.stl   # headless render to PNG (self-verify)
//!
//! The screenshot mode is the tooling that lets a display-less environment SEE the viewport.
//! Geometry stays in OpenSCAD (via `fab`); the GUI loads the rendered STL and owns the view.

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
    scene::{Scene, SceneList}, // the bsn traits — shadow the prelude's `Scene` asset struct
    ui_widgets::{Activate, SliderStep},
    render::{
        render_resource::{PrimitiveTopology, TextureFormat, TextureUsages},
        view::screenshot::{save_to_disk, Screenshot},
    },
    window::ExitCondition,
    winit::WinitPlugin,
};

mod stl;

/// Scene inputs shared by both modes (model STL path + printer bed footprint).
#[derive(Resource, Clone)]
struct SceneCfg {
    model: Option<String>,
    bed: [f32; 2],
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = args.iter().find(|a| a.ends_with(".stl")).cloned();
    let bed = bed_size().unwrap_or([256.0; 3]);
    let scene = SceneCfg {
        model,
        bed: [bed[0] as f32, bed[1] as f32],
    };
    match flag_value(&args, "--screenshot") {
        Some(png) => run_screenshot(scene, PathBuf::from(png)),
        None => run_windowed(scene),
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
        .add_systems(Startup, (setup_windowed, ui_root.spawn()))
        .add_systems(Update, orbit)
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
    spawn_world(&mut commands, &mut meshes, &mut materials, &scene);
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

// ---- headless screenshot --------------------------------------------------------------

#[derive(Resource)]
struct Shot {
    target: Handle<Image>,
    png: PathBuf,
    frame: u32,
    captured: bool,
}

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

#[derive(Resource)]
struct ScreenshotPng(PathBuf);

fn setup_offscreen(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    scene: Res<SceneCfg>,
    png: Res<ScreenshotPng>,
) {
    spawn_world(&mut commands, &mut meshes, &mut materials, &scene);

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
    // Let a few frames render, then screenshot the target image and save it.
    if !shot.captured && shot.frame >= 3 {
        let png = shot.png.clone();
        commands
            .spawn(Screenshot::image(shot.target.clone()))
            .observe(save_to_disk(png.clone()));
        shot.captured = true;
        info!("capturing -> {}", png.display());
    }
    // Give the async GPU readback + save time to finish, then exit.
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
    scene: &SceneCfg,
) {
    // Model: the STL (a `fab`-rendered part), else a placeholder block.
    let model = match scene.model.as_deref() {
        Some(p) if Path::new(p).exists() => match stl::load_stl(Path::new(p)) {
            Ok(s) => {
                info!("loaded {p} ({} tris)", s.positions.len() / 3);
                meshes.add(build_mesh(&s))
            }
            Err(e) => {
                error!("loading {p}: {e:#}");
                meshes.add(Cuboid::new(60.0, 40.0, 30.0))
            }
        },
        _ => meshes.add(Cuboid::new(60.0, 40.0, 30.0)),
    };
    commands.spawn((
        Mesh3d(model),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.90, 0.74, 0.20),
            perceptual_roughness: 0.7,
            ..default()
        })),
    ));

    // Printer bed reference at z=0 (from the shared fab_scad::printers lib).
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(scene.bed[0], scene.bed[1], 1.0))),
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

/// The control panel as a bsn scene: a title, a cut-position slider, and a re-slice button —
/// the first real Feathers/bsn widgets, to grow into the full cut + connector controls.
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
                on(|_: On<Activate>| info!("re-slice clicked"))
            ),
        ]
    }
}
