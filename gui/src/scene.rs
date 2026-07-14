//! Scene/environment spawning + the STL->Bevy mesh plumbing every mode shares.

use crate::*;

/// Point the asset server at this crate's `assets/` (where the icon font lives), regardless of CWD.
/// Dev builds use the baked crate path; a packaged .app doesn't have it, so fall back to `assets/`
/// next to the executable, then the bundle's `Contents/Resources/assets`.
pub(crate) fn assets_dir() -> AssetPlugin {
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

/// The primary-window config the windowed app runs under (W.3.5). On wasm it binds Bevy to the page's
/// `<canvas id="fab-web">` and tracks its parent's size (the hosting document owns layout); on desktop
/// it's the default OS window. Same builder, both targets — a canvas swap, not a second app.
pub(crate) fn window_plugin() -> WindowPlugin {
    #[cfg(target_arch = "wasm32")]
    {
        WindowPlugin {
            primary_window: Some(Window {
                // The page provides <canvas id="fab-web"> BEFORE init() — missing = panic (see
                // gui/web/index.html). fit_canvas_to_parent tracks the parent's size.
                canvas: Some("#fab-web".into()),
                fit_canvas_to_parent: true,
                ..default()
            }),
            ..default()
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        WindowPlugin::default()
    }
}

/// A NO-INCLUDE demo the wasm smoke renders (W.3.6) — pure CSG, so Stage-1 bytes-eval handles it (no
/// lib closure). Seeded into the editor buffer when the web app boots without a source.
#[cfg(target_arch = "wasm32")]
const WEB_DEMO: &str = "\
// fab-gui on the web — a box with a bored hole (CSG, no includes)\n\
$fn = $preview ? 24 : 64;\n\
difference() {\n\
  cube([60, 40, 30], center = true);\n\
  translate([0, 0, 6]) cylinder(h = 24, r = 12, center = true);\n\
}\n";

#[allow(clippy::too_many_arguments)] // a Bevy startup system — params are dependencies, not a smell
pub(crate) fn setup_windowed(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    scene: Res<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut gizmo_cfg: ResMut<GizmoConfigStore>,
    mut editor: ResMut<EditorBuf>,
    mut files: ResMut<FileList>,
    pool: Res<GeomPool>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    // Seed the file-tab + editor from the launch source (U.3.2): a folder pick repopulates both.
    if let Some(src) = scene.source.clone() {
        read_into_editor(&mut editor, &src);
        files.files = vec![src];
        files.active = Some(0);
    }
    // wasm smoke (W.3.6): no launch file → seed a NO-INCLUDE demo into the editor buffer and arm the
    // debounced preview, so the geom Worker renders it (the browser's source is the buffer, not a path).
    #[cfg(target_arch = "wasm32")]
    if scene.source.is_none() {
        editor.text = WEB_DEMO.to_string();
        editor.edited_at = Some(0.0);
    }
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
        // U.3.9: the primary egui context lives HERE, explicitly. Auto-created, bevy_egui attaches
        // it to the "first found" camera — a registration-order lottery that can (and did) land on
        // the Camera3d, whose `split_viewport`-inset viewport becomes egui's screen_rect: the whole
        // UI then draws inset by one seam (the left/top black margin). egui's rect must derive from
        // a camera that always covers the window.
        PrimaryEguiContext,
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
    // Render the model's parts off-thread; poll_job seeds each part + its first cut when bounds land.
    kick_render(&pool, &mut job, &mut status, &scene, true);
}

/// Drop the bed slab so its top meets the model's Z-floor — the model rests on the bed instead of
/// dipping below it (its native coords needn't put the bottom at z=0). Runs when the bounds change.
pub(crate) fn seat_bed(
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

// ---- shared scene ---------------------------------------------------------------------
/// The bed + lights (everything but the model + cut planes, which load via a job / synchronously).
pub(crate) fn spawn_environment(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cfg: &SceneCfg,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(cfg.bed[0], cfg.bed[1], 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: theme::BED_SLATE,
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

/// Load an STL into a mesh and its AABB (None on failure → placeholder mesh, no bounds).
pub(crate) fn mesh_and_bounds(
    meshes: &mut Assets<Mesh>,
    stl: &Path,
) -> (Handle<Mesh>, Option<(Vec3, Vec3)>) {
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

/// Build a mesh from in-memory STL bytes (the geometry service's render/reslice output, W.3.3) —
/// the byte twin of [`mesh_and_bounds`], with no disk round-trip. Bounds come from the wire bbox, so
/// this returns only the handle. A parse failure falls back to a placeholder box (and logs).
pub(crate) fn mesh_from_bytes(meshes: &mut Assets<Mesh>, bytes: &[u8]) -> Handle<Mesh> {
    match stl::load_stl_bytes(bytes) {
        Ok(s) => meshes.add(build_mesh(&s)),
        Err(e) => {
            error!("parsing service STL ({} bytes): {e:#}", bytes.len());
            meshes.add(Cuboid::new(60.0, 40.0, 30.0))
        }
    }
}

pub(crate) fn aabb_of(s: &stl::StlMesh) -> Option<(Vec3, Vec3)> {
    let mut iter = s.positions.iter().map(|p| Vec3::from_array(*p));
    let first = iter.next()?;
    let (mut min, mut max) = (first, first);
    for v in iter {
        min = min.min(v);
        max = max.max(v);
    }
    Some((min, max))
}

pub(crate) fn load_model(meshes: &mut Assets<Mesh>, stl: Option<&Path>) -> Handle<Mesh> {
    match stl {
        Some(p) if p.exists() => mesh_and_bounds(meshes, p).0,
        _ => meshes.add(Cuboid::new(60.0, 40.0, 30.0)),
    }
}

pub(crate) fn part_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: theme::MODEL_GOLD,
        perceptual_roughness: 0.7,
        ..default()
    })
}

pub(crate) fn build_mesh(s: &stl::StlMesh) -> Mesh {
    let n = s.positions.len() as u32;
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, s.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, s.normals.clone())
    .with_inserted_indices(Indices::U32((0..n).collect()))
}

/// The default printer, read from the nearest printers.toml (walking up from CWD) via the shared lib.
fn load_default_printer() -> Option<fab_scad::printers::Printer> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml = dir.join("printers.toml");
        if toml.exists() {
            let printers = fab_scad::printers::load(&toml).ok()?;
            return fab_scad::printers::select(&printers, None).ok().cloned();
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The default printer's `(usable bed, real plate size)`. `bed` is what pieces pack within (extruder
/// reach); `plate` is the real plate size the Bambu export grid/`printable_area` uses (= `bed` unless
/// printers.toml sets a larger `plate`, e.g. the H2D 350).
pub(crate) fn bed_size() -> Option<([f64; 3], [f64; 3])> {
    load_default_printer().map(|p| (p.bed, p.plate))
}

/// The default printer's Bambu preset ids (printer/process/filament names) for a prompt-free `.3mf`
/// import — `None` when printers.toml has no `[printer.bambu]` block (the writer falls back to a
/// minimal config that still loads the plates but shows the "customized presets" prompt).
pub(crate) fn default_bambu_preset() -> Option<fab_scad::printers::BambuPreset> {
    load_default_printer().and_then(|p| p.bambu)
}
