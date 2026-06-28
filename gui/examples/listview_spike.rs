//! SPIKE (throwaway): does a Feathers `FeathersListView` take a DATA-BUILT row list, and can the
//! row COUNT change at runtime? That's the one open risk for the plane-card UI (Option B).
//!
//! It builds a list from `(0..n)` rows (a `Vec<RowScene>`, which is a `SceneList`), then re-spawns
//! it at n = 3 → 6 → 2 over a few frames, screenshotting each. Headless, like the GUI's harness.
//!
//!   cargo run -p fab-gui --example listview_spike

use std::path::PathBuf;

use bevy::{
    app::ScheduleRunnerPlugin,
    camera::RenderTarget,
    feathers::{
        controls::{FeathersListRow, FeathersListView},
        dark_theme::create_dark_theme,
        theme::{ThemedText, UiTheme},
        FeathersPlugins,
    },
    image::Image,
    prelude::*,
    render::{
        render_resource::{TextureFormat, TextureUsages},
        view::screenshot::{save_to_disk, Screenshot},
    },
    scene::{SceneList, WorldSceneExt},
    window::ExitCondition,
    winit::WinitPlugin,
};

#[derive(Resource)]
struct Spike {
    frame: u32,
    list: Option<Entity>,
    target: Handle<Image>,
}

fn main() {
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
        .add_systems(Startup, setup)
        .add_systems(Update, drive)
        .run();
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut img = Image::new_target_texture(600, 500, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);
    commands.spawn((
        Camera2d,
        RenderTarget::Image(target.clone().into()),
        bevy::ui::IsDefaultUiCamera,
    ));
    commands.insert_resource(Spike { frame: 0, list: None, target });
}

/// Re-spawn the list with `n` rows, built from data.
fn rebuild(commands: &mut Commands, old: Option<Entity>, n: usize) {
    if let Some(e) = old {
        commands.entity(e).despawn();
    }
    commands.queue(move |world: &mut World| {
        let rows: Vec<_> = (0..n)
            .map(|i| {
                bsn! {
                    @FeathersListRow Children [ (Text(format!("cut {i}")) ThemedText) ]
                }
            })
            .collect();
        let scene = bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: px(10),
                left: px(10),
                width: px(240),
                height: px(420),
                flex_direction: FlexDirection::Column,
                row_gap: px(6),
            }
            Children [
                (Text(format!("{n} rows")) ThemedText),
                (@FeathersListView { @rows: { Box::new(rows) as Box<dyn SceneList> } }),
            ]
        };
        match world.spawn_scene(scene) {
            Ok(ent) => {
                let id = ent.id();
                world.resource_mut::<Spike>().list = Some(id);
            }
            Err(e) => error!("spawn_scene failed: {e:?}"),
        }
    });
}

fn shot(commands: &mut Commands, target: &Handle<Image>, name: &str) {
    let path: PathBuf = std::env::temp_dir().join(name);
    commands.spawn(Screenshot::image(target.clone())).observe(save_to_disk(path.clone()));
    info!("shot {}", path.display());
}

fn drive(mut commands: Commands, mut spike: ResMut<Spike>, mut exit: MessageWriter<AppExit>) {
    spike.frame += 1;
    let (f, old, target) = (spike.frame, spike.list, spike.target.clone());
    match f {
        3 => rebuild(&mut commands, old, 3),
        12 => shot(&mut commands, &target, "spike-3.png"),
        35 => rebuild(&mut commands, old, 6),
        45 => shot(&mut commands, &target, "spike-6.png"),
        68 => rebuild(&mut commands, old, 2),
        78 => shot(&mut commands, &target, "spike-2.png"),
        110 => {
            exit.write(AppExit::Success);
        }
        _ => {}
    }
}
