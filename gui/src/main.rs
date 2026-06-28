//! fab-gui — the slicing GUI (Phase 5.1). A Bevy viewport over a `fab`-rendered mesh with the
//! printer bed for reference; cut-plane dragging, connector placement, and the Feathers UI
//! build out from here. The GUI shares fab's types (printer beds, the slicing spec) via the
//! `fab_scad` lib and drives geometry by calling `fab`.
//!
//! Run: `cargo run -p fab-gui -- path/to/part.stl`   (no arg → a placeholder block)

use std::path::Path;

use bevy::{
    asset::RenderAssetUsages,
    feathers::{dark_theme::create_dark_theme, theme::UiTheme, FeathersPlugins},
    input::mouse::{MouseMotion, MouseWheel},
    mesh::Indices,
    prelude::*,
    render::render_resource::PrimitiveTopology,
};

mod stl;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, FeathersPlugins))
        .insert_resource(UiTheme(create_dark_theme()))
        .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .add_systems(Startup, setup)
        .add_systems(Update, orbit)
        .run();
}

/// Orbit state for the camera. Z-up, matching the printer / OpenSCAD frame.
#[derive(Component)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // The model: an STL path on the CLI (a `fab`-rendered part), else a placeholder block.
    let model = match std::env::args().nth(1) {
        Some(p) if Path::new(&p).exists() => match stl::load_binary_stl(Path::new(&p)) {
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

    // Printer bed from fab-scad's printers.toml (the SHARED lib) — reference footprint at z=0.
    let bed = bed_size().unwrap_or([256.0; 3]);
    let (bx, by) = (bed[0] as f32, bed[1] as f32);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(bx, by, 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.18, 0.18, 0.22),
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -0.5),
    ));

    // Key + fill directional lights (Z-up; no AmbientLight, to keep the API surface small).
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

    // Orbit camera (Z-up). `orbit` drives its transform each frame.
    let radius = bx.max(by).max(80.0);
    commands.spawn((
        Camera3d::default(),
        Transform::default(),
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
        },
    ));

    // Minimal overlay (plain bevy_ui for now; Feathers controls land next).
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
        Text::new("fab-gui — drag: orbit \u{b7} scroll: zoom"),
    ));
}

/// Left-drag orbits, scroll zooms. Camera stays aimed at the origin, Z-up.
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
    let cp = o.pitch.cos();
    let pos = Vec3::new(
        o.radius * cp * o.yaw.cos(),
        o.radius * cp * o.yaw.sin(),
        o.radius * o.pitch.sin(),
    );
    *t = Transform::from_translation(pos).looking_at(Vec3::ZERO, Vec3::Z);
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
