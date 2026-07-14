//! Self-verify captures: the offscreen --screenshot mode + the windowed --shot harness.

use crate::*;

/// Windowed self-verify (U.3.10): `--shot <path>` captures the REAL window surface at a settled
/// frame, then exits. The offscreen harness renders through its own camera set at scale 1.0, so
/// it can't see windowed-only wiring bugs (HiDPI, the egui-context/camera lottery of U.3.9) —
/// this path sees exactly what the user sees.
#[derive(Resource, Default)]
pub(crate) struct WindowShot(pub(crate) Option<PathBuf>);

/// `--shot` (U.3.10): at a SETTLED frame (90 — frame-1 samples race the HiDPI scale handshake)
/// dump the Window's ground truth + every camera's order/viewport/egui-context ownership (WHICH
/// camera hosts the primary context was the whole of bug U.3.9), capture the real window, then
/// exit a second later so the async PNG save flushes and a scripted run self-terminates.
#[allow(clippy::type_complexity)] // a diag query — the tuple IS the report
pub(crate) fn window_shot(
    mut n: Local<u32>,
    mut commands: Commands,
    shot: Res<WindowShot>,
    windows: Query<&Window>,
    cams: Query<(
        Entity,
        &Camera,
        Has<bevy_egui::EguiContext>,
        Has<bevy_egui::PrimaryEguiContext>,
        Has<Camera2d>,
        Has<Camera3d>,
    )>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = shot.0.as_ref() else {
        return;
    };
    *n += 1;
    if *n == 150 {
        exit.write(AppExit::Success);
    }
    if *n != 90 {
        return;
    }
    for w in &windows {
        eprintln!(
            "WIN DIAG physical={}x{} logical={}x{} scale={}",
            w.physical_width(),
            w.physical_height(),
            w.width(),
            w.height(),
            w.scale_factor()
        );
    }
    for (e, cam, ctx, primary, is2d, is3d) in &cams {
        eprintln!(
            "CAM DIAG {e} order={} 2d={is2d} 3d={is3d} egui_ctx={ctx} primary={primary} viewport={:?} target={:?}",
            cam.order,
            cam.physical_viewport_rect(),
            cam.physical_target_size(),
        );
    }
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path.clone()));
}

// ---- headless screenshot --------------------------------------------------------------
#[derive(Resource)]
pub(crate) struct Shot {
    pub(crate) target: Handle<Image>,
    pub(crate) png: PathBuf,
    pub(crate) frame: u32,
    pub(crate) captured: bool,
}

#[derive(Resource)]
pub(crate) struct ScreenshotPng(pub(crate) PathBuf);

#[allow(clippy::too_many_arguments)] // a Bevy startup system — params are dependencies, not a smell
pub(crate) fn setup_offscreen(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    scene: Res<SceneCfg>,
    png: Res<ScreenshotPng>,
    mut editor: ResMut<EditorBuf>,
    mut files: ResMut<FileList>,
    pool: Res<GeomPool>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    if let Some(src) = scene.source.clone() {
        read_into_editor(&mut editor, &src);
        files.files = vec![src];
        files.active = Some(0);
    }
    // Synchronous here — no UI to freeze. Render whole for bounds + the cut plane, then
    // (if asked) slice at the chosen cut so the PNG verifies an off-center cut.
    let display = setup_offscreen_model(&mut commands, &mut meshes, &mut materials, &scene, &pool);
    commands.spawn((
        Mesh3d(display),
        MeshMaterial3d(part_material(&mut materials)),
        Model,
        PartId(0),
        Pickable::IGNORE, // uniform with the windowed Model spawns (a Model never blocks picking)
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
            // Mirror the windowed layering (U.3.9): 3D renders first (order 0, clears the target),
            // the egui/UI camera last with no clear. The egui pass runs inside its HOST camera's
            // graph, so the host must render after the 3D or floating egui elements over the 3D
            // viewport get overdrawn — a divergence the offscreen harness would never show.
            order: 1,
            clear_color: bevy::camera::ClearColorConfig::None,
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        bevy::ui::IsDefaultUiCamera,
        // U.3.9: explicit primary context — never the viewport-inset Camera3d (see setup_windowed).
        PrimaryEguiContext,
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
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

/// Headless model prep: render whole (→ bounds + cut plane), optionally slice at the cut. Routes both
/// ops through the geometry service (W.3.3) — blocking, since there's no UI to freeze — and returns
/// the mesh handle to display.
pub(crate) fn setup_offscreen_model(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    scene: &SceneCfg,
    pool: &GeomPool,
) -> Handle<Mesh> {
    let Some(src) = scene.source.as_deref() else {
        return load_model(meshes, scene.stl.as_deref());
    };
    // Render WHOLE through the service: mints a base handle we then reslice off. block_on drives the
    // reply here while the kernel thread runs it.
    let root = scene
        .root
        .as_ref()
        .map(|r| r.to_string_lossy().into_owned());
    let (base, min, max, whole_mesh) = match block_on(pool.call(Request::RenderWhole {
        source: Source::Path(src.to_string_lossy().into_owned()),
        root,
    })) {
        Ok(Response::Rendered { id, stl, min, max }) => {
            (id, min, max, mesh_from_bytes(meshes, &stl))
        }
        Ok(Response::Failed { error }) => {
            error!("{error}");
            return load_model(meshes, None);
        }
        Ok(_) => {
            error!("render: unexpected service response");
            return load_model(meshes, None);
        }
        Err(e) => {
            error!("{e:#}");
            return load_model(meshes, None);
        }
    };
    let (mn, mx) = (
        Vec3::new(min[0] as f32, min[1] as f32, min[2] as f32),
        Vec3::new(max[0] as f32, max[1] as f32, max[2] as f32),
    );
    let cut_x = mn.x + (scene.cut_pct / 100.0) * (mx.x - mn.x);
    let cut = CutDef {
        axis: Axis::X,
        at: cut_x,
        enabled: true,
    };
    spawn_cut_plane(commands, meshes, materials, mn, mx, &cut, 0);
    if !scene.reslice_on_start {
        return whole_mesh;
    }
    match block_on(pool.call(Request::Reslice {
        base,
        cuts: vec![('x', cut_x as f64)],
        connectors: vec![],
        orient: vec![],
        spread: SPREAD,
    })) {
        Ok(Response::Resliced { stl }) => mesh_from_bytes(meshes, &stl),
        Ok(Response::Failed { error }) => {
            error!("{error}");
            whole_mesh
        }
        Ok(_) => {
            error!("reslice: unexpected service response");
            whole_mesh
        }
        Err(e) => {
            error!("{e:#}");
            whole_mesh
        }
    }
}

pub(crate) fn capture_then_exit(
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
