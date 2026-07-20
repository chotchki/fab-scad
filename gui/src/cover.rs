//! W.3.29.3: the offscreen COVER scene — a clean, chrome-free whole-model shot for the publish gallery
//! cover, shared by the desktop ([`crate::publish_native`]) and web ([`crate::publish_web`]) flows. It
//! renders the model on a PRIVATE render layer at the live view's angle but a bounds-framed distance, so
//! the cover matches what you were looking at without the UI or the slice state. The two flows diverge
//! only at CAPTURE: desktop screenshots to a PNG file (`save_to_disk`), web to PNG bytes (no fs).

use bevy::camera::visibility::RenderLayers;
use bevy::render::render_resource::{TextureFormat, TextureUsages};

use crate::*;

/// The private render layer the cover scene lives on — the main cameras render `[0, 1]` (model + gizmos),
/// so layer 2 is ours alone: the cover camera sees only the mesh + lights we spawn here.
pub(crate) const COVER_LAYER: usize = 2;
// W.3.28.8: a WIDE letterbox (~3:1), not 4:3 — the site's page banner is a fixed-height, full-width,
// center-cropped band, so a tall cover loses its top+bottom. 3:1 survives both the banner and the ~3:1
// index card; ≥1536 wide keeps the largest resize-ladder variant crisp at full desktop width.
pub(crate) const COVER_W: u32 = 1600;
pub(crate) const COVER_H: u32 = 540;

/// Frame the cover: keep the user's ANGLE (`yaw`/`pitch`) but derive the target + distance from the model
/// BOUNDS, so the cover is independent of the live zoom and the model sits inside the wide letterbox's
/// safe center square. Fits the bounding sphere within the camera's vertical FOV with margin (the banner
/// shows only a short center stripe; the mobile banner crops the SIDES to ~square — the center square
/// survives both).
pub(crate) fn cover_orbit(
    yaw: f32,
    pitch: f32,
    min: [f64; 3],
    max: [f64; 3],
) -> (f32, f32, f32, Vec3) {
    let mn = Vec3::new(min[0] as f32, min[1] as f32, min[2] as f32);
    let mx = Vec3::new(max[0] as f32, max[1] as f32, max[2] as f32);
    let center = (mn + mx) * 0.5;
    let bound_radius = ((mx - mn).length() * 0.5).max(1.0); // bounding sphere ≈ AABB diagonal / 2
    // Bevy's PerspectiveProjection default vertical FOV is π/4. distance to fit the sphere's vertical
    // extent within the view, times a margin so it lands in the center square (≈ half the frame height).
    const FOV_V: f32 = std::f32::consts::FRAC_PI_4;
    const MARGIN: f32 = 1.9;
    let dist = bound_radius * MARGIN / (FOV_V * 0.5).tan();
    (yaw, pitch, dist, center)
}

/// Build the OFFSCREEN cover scene on [`COVER_LAYER`]: an image render target, a fresh mesh from the
/// rendered STL, two lights (mirroring `spawn_environment`), and a camera at the framed orbit. Everything's
/// on the private layer, so the main cameras don't draw it and it doesn't draw the live scene. Returns the
/// target to screenshot + the entities to despawn.
pub(crate) fn spawn_cover_scene(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    stl: &[u8],
    orbit: (f32, f32, f32, Vec3),
) -> (Handle<Image>, Vec<Entity>) {
    let mut img = Image::new_target_texture(COVER_W, COVER_H, TextureFormat::Rgba8UnormSrgb, None);
    img.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(img);

    let layer = RenderLayers::layer(COVER_LAYER);
    let (yaw, pitch, radius, tgt) = orbit;
    let ents = vec![
        commands
            .spawn((
                Mesh3d(mesh_from_bytes(meshes, stl)),
                MeshMaterial3d(part_material(materials)),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                DirectionalLight {
                    illuminance: 6000.0,
                    ..default()
                },
                Transform::from_xyz(80.0, -120.0, 160.0).looking_at(Vec3::ZERO, Vec3::Z),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                DirectionalLight {
                    illuminance: 2000.0,
                    ..default()
                },
                Transform::from_xyz(-120.0, 100.0, 60.0).looking_at(Vec3::ZERO, Vec3::Z),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((
                Camera3d::default(),
                Camera {
                    // A distinct order from the window cameras; it targets its own image, so this only
                    // orders it against itself. Clears to the ClearColor resource (theme::VIEWPORT).
                    order: -1,
                    ..default()
                },
                RenderTarget::Image(target.clone().into()),
                orbit_transform(yaw, pitch, radius, tgt),
                layer,
            ))
            .id(),
    ];
    (target, ents)
}
