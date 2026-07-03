//! 18.5 GO/NO-GO: the real GUI's risk surface, in a browser. Exercises exactly the pieces
//! bevy#22620 says break on web: winit-on-web event loop, WebGL2 render, feathers UI materials,
//! mesh-picking plumbing. Scene = lit cube with click/hover observers + a feathers panel
//! (root Node, FeathersButton, ThemedText label) spawned through `bsn!` like gui/src/main.rs.
//! Compile success proves nothing; the failures are runtime panics in the JS console.

use bevy::{
    picking::{
        events::{Click, Out, Over, Pointer},
        mesh_picking::MeshPickingPlugin,
    },
    prelude::*,
};
use wasm_bindgen::prelude::*;

/// Marks the pickable cube so observers can find its material.
#[derive(Component)]
struct ProbeCube;

/// Marks the status label the click observer rewrites. Clone+Default because `bsn!` templates
/// require both.
#[derive(Component, Clone, Default)]
struct StatusLabel;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    run();
}

pub fn run() {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "wasm-gui-spike".into(),
                resolution: bevy::window::WindowResolution::new(1200, 800),
                ..default()
            }),
            ..default()
        }),
        MeshPickingPlugin,
    ));

    #[cfg(feature = "feathers")]
    {
        use bevy::feathers::{dark_theme::create_dark_theme, theme::UiTheme, FeathersPlugins};
        app.add_plugins(FeathersPlugins).insert_resource(UiTheme(create_dark_theme()));
    }

    app.insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
        .add_systems(Startup, (setup_3d, setup_ui))
        .add_systems(Update, heartbeat)
        .run();
}

/// Camera + light + one pickable cube. Click turns it green (observer must actually fire for
/// the color to change — end-to-end proof the picking backend raycasts on web).
fn setup_3d(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(4.0, 4.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 8000.0, ..default() },
        Transform::from_xyz(4.0, 8.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands
        .spawn((
            Mesh3d(meshes.add(Cuboid::new(2.5, 2.5, 2.5))),
            MeshMaterial3d(mats.add(StandardMaterial::from(Color::srgb(0.85, 0.30, 0.20)))),
            Transform::default(),
            ProbeCube,
        ))
        .observe(on_cube_click)
        .observe(on_cube_over)
        .observe(on_cube_out);
}

fn recolor(
    entity: Entity,
    color: Color,
    q: &Query<&MeshMaterial3d<StandardMaterial>>,
    mats: &mut Assets<StandardMaterial>,
) {
    if let Ok(h) = q.get(entity) {
        if let Some(mut m) = mats.get_mut(&h.0) {
            m.base_color = color;
        }
    }
}

fn on_cube_click(
    ev: On<Pointer<Click>>,
    q: Query<&MeshMaterial3d<StandardMaterial>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut labels: Query<&mut Text, With<StatusLabel>>,
) {
    recolor(ev.entity, Color::srgb(0.20, 0.80, 0.30), &q, &mut mats);
    for mut t in &mut labels {
        t.0 = "status: cube clicked".into();
    }
    info!("PROBE: cube clicked — picking observer fired");
}

fn on_cube_over(
    ev: On<Pointer<Over>>,
    q: Query<&MeshMaterial3d<StandardMaterial>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    recolor(ev.entity, Color::srgb(0.95, 0.60, 0.20), &q, &mut mats);
}

fn on_cube_out(
    ev: On<Pointer<Out>>,
    q: Query<&MeshMaterial3d<StandardMaterial>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    recolor(ev.entity, Color::srgb(0.85, 0.30, 0.20), &q, &mut mats);
}

/// Feathers panel through `bsn!` + `spawn_scene` — the exact idiom gui/src/main.rs uses, so a
/// pass here transfers.
#[cfg(feature = "feathers")]
fn setup_ui(world: &mut World) {
    use bevy::feathers::{
        controls::{ButtonVariant, FeathersButton},
        theme::{ThemeBackgroundColor, ThemedText},
        tokens,
    };
    use bevy::ui_widgets::Activate;

    let scene = bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            left: px(8),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: UiRect::all(px(8)),
            min_width: px(200),
        }
        ThemeBackgroundColor(tokens::WINDOW_BG)
        Children [
            (Text("wasm-gui-spike") ThemedText),
            (
                @FeathersButton { @variant: {ButtonVariant::Primary}, @caption: bsn!{ Text("Probe") ThemedText } }
                on(|_: On<Activate>, mut labels: Query<&mut Text, With<StatusLabel>>| {
                    for mut t in &mut labels {
                        t.0 = "status: button activated".into();
                    }
                    info!("PROBE: feathers button activated");
                })
            ),
            (Text("status: alive") ThemedText StatusLabel),
        ]
    };
    world.spawn_scene(scene).expect("spawn feathers panel");
}

/// Fallback-cost probe: plain bevy_ui Node/Button/Text, no feathers anywhere in the binary.
#[cfg(not(feature = "feathers"))]
fn setup_ui(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            left: px(8),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: UiRect::all(px(8)),
            min_width: px(200),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        children![
            Text::new("wasm-gui-spike (plain ui)"),
            (
                Button,
                Node { padding: UiRect::all(px(6)), ..default() },
                BackgroundColor(Color::srgb(0.25, 0.35, 0.60)),
                children![Text::new("Probe")],
            ),
            (Text::new("status: alive"), StatusLabel),
        ],
    ));
}

/// One console line after 60 rendered frames — the render loop surviving past startup is the
/// signal; a panicked app never prints it.
fn heartbeat(mut frames: Local<u32>) {
    *frames += 1;
    if *frames == 60 {
        info!("PROBE: 60 frames rendered — render loop alive");
    }
}
