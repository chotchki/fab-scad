//! Camera, orbit, viewport split and view-mode switching.

use crate::*;

#[derive(Component)]
pub(crate) struct Orbit {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) radius: f32,
    pub(crate) target: Vec3, // look-at point; right-drag pans it
}

// A Bevy system's params ARE its dependencies; the wheel-gate needs cam + input + three state
// resources + the window, one past clippy's default arg cap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn orbit(
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    dragging: Res<DraggingCut>,
    edit: Res<EditCut>,
    seam: Res<PanelSeam>,
    windows: Query<&Window>,
) {
    // Is the cursor over the egui panel? `seam.over_ui` can't answer that reliably: egui's
    // `is_pointer_over_egui()` returns false over our BACKGROUND-layer panels (they shrink a SEPARATE
    // viewport Ui, never egui's root_ui, so its available-rect stays the whole window → the check is
    // false everywhere over the panel, and a wheel-scroll leaks straight to the camera). Gate on the
    // panel RECT instead — the same physical-px seam bands `split_viewport` insets the 3D camera by
    // (view.rs:186). Stable frame-to-frame, so the PostUpdate→Update seam lag is harmless. Keep the
    // `seam.over_ui` term too: it still catches an ACTIVE egui drag (scrollbar/text) that strays out.
    let over_panel = windows.single().is_ok_and(|w| {
        w.physical_cursor_position().is_some_and(|c| {
            c.x <= seam.width_px + 6.0
                || c.y <= seam.top_px
                || c.y >= w.physical_height() as f32 - seam.bottom_px
        })
    });
    // Yield the whole gesture (wheel + drag) when a cut plane has the pointer, the connector editor
    // holds a fixed face-on view, or the pointer is over the panel — so scrolling the editor / file
    // list doesn't ALSO zoom the camera.
    if dragging.0 || edit.0.is_some() || seam.over_ui || over_panel {
        motion.clear();
        wheel.clear();
        return;
    }
    let Ok((mut t, mut o)) = cam.single_mut() else {
        return;
    };
    // Camera basis (for panning in the view plane), captured before we move it.
    let right = t.rotation * Vec3::X;
    let up = t.rotation * Vec3::Y;
    if buttons.pressed(MouseButton::Left) {
        for ev in motion.read() {
            o.yaw -= ev.delta.x * 0.008;
            o.pitch = (o.pitch + ev.delta.y * 0.008).clamp(-1.5, 1.5);
        }
    } else if buttons.pressed(MouseButton::Right) {
        let scale = o.radius * 0.0015;
        for ev in motion.read() {
            o.target += (-right * ev.delta.x + up * ev.delta.y) * scale;
        }
    } else {
        motion.clear();
    }
    for ev in wheel.read() {
        // Zoom per notch is a fraction of the current radius (constant feel at any distance). Line
        // events (a mouse wheel) come one notch at a time; Pixel events (a trackpad) stream many
        // small deltas per gesture, so scale them WAY down or the zoom rockets past what you're on.
        let step = match ev.unit {
            MouseScrollUnit::Line => ev.y * 0.05,
            MouseScrollUnit::Pixel => ev.y * 0.004,
        };
        o.radius = (o.radius * (1.0 - step)).clamp(10.0, 4000.0);
    }
    *t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
}

/// The Explode/Collapse button: collapse to the uncut model, or explode the last sliced result —
/// auto-slicing first if the cuts changed (or were never sliced), so it works without Re-slice.
pub(crate) fn toggle_view(
    mut ev: MessageReader<PanelCmd>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut models: Query<(&mut Mesh3d, &PartId), With<Model>>,
) {
    if !ev.read().any(|c| *c == PanelCmd::ToggleView) {
        return;
    }
    let ap = active_part.0;
    let part = &mut parts.0[ap];
    if part.spread > 0.0 {
        // Collapse → the uncut model.
        if let Some(h) = part.whole.clone() {
            swap_part_mesh(&mut models, ap, &h);
            part.spread = 0.0;
        }
    } else if let Some(h) = part.sliced.clone() {
        // Explode the sliced pieces — `auto_reslice` keeps them fresh in the background, and a
        // pending rebuild refreshes them in place when it lands (poll_job, spread > 0).
        swap_part_mesh(&mut models, ap, &h);
        part.spread = SPREAD as f32;
    } else {
        // Nothing sliced yet — kick one explicitly; poll_job explodes it when it arrives.
        reslice_w.write(ReSlice);
    }
}

/// Point part `ap`'s displayed model entity(ies) at mesh `h`, leaving every OTHER part's mesh
/// untouched — the per-part explode/collapse/revert primitive (T.2b).
pub(crate) fn swap_part_mesh(
    models: &mut Query<(&mut Mesh3d, &PartId), With<Model>>,
    ap: usize,
    h: &Handle<Mesh>,
) {
    for (mut m, pid) in models {
        if pid.0 == ap {
            m.0 = h.clone();
        }
    }
}

/// A small XYZ orientation gizmo pinned to the lower-left of the 3D viewport: arrows along world X
/// (red), Y (green), Z (blue), drawn at a fixed camera-relative offset. Because the arrows point
/// along the WORLD axes but the anchor rides with the camera, it spins as you orbit yet stays put on
/// pan/zoom — the "which way is the origin" indicator.
pub(crate) fn draw_axis_gizmo(
    cam: Query<(&Orbit, &Projection), With<Camera3d>>,
    mut gizmos: Gizmos,
) {
    let Ok((o, proj)) = cam.single() else {
        return;
    };
    let Projection::Perspective(p) = proj else {
        return;
    };
    // Recompute the camera transform from the orbit state THIS frame — the camera's GlobalTransform
    // is a frame stale (propagated in PostUpdate), which makes a camera-locked gizmo swim/flicker.
    let t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
    // Place the anchor at the lower-left of the frustum, a short distance in front of the camera.
    let d = 12.0;
    let half_h = d * (p.fov * 0.5).tan();
    let half_w = half_h * p.aspect_ratio;
    let origin =
        t.translation + t.forward() * d - t.right() * (half_w * 0.85) - t.up() * (half_h * 0.78);
    let len = half_h * 0.22;
    gizmos.arrow(origin, origin + Vec3::X * len, Color::srgb(1.0, 0.4, 0.4)); // X red
    gizmos.arrow(origin, origin + Vec3::Y * len, Color::srgb(0.4, 0.9, 0.45)); // Y green
    gizmos.arrow(origin, origin + Vec3::Z * len, Color::srgb(0.5, 0.6, 1.0)); // Z blue
}

/// On a change of what's displayed, frame it: centre on the (possibly exploded) bounds + fit.
/// Keyed on a `Local` snapshot of (spread, bounds) — fires on explode/collapse (spread moves) and on
/// the first render (bounds land), but NOT on a cut drag (which changes neither), so the camera
/// doesn't yank while you're dragging a plane. `spread` used to be its own resource whose
/// change-detection was this narrow signal; folding it into `Part` widened `parts.is_changed()` to
/// every edit, so the snapshot restores the old narrowness.
pub(crate) fn auto_scale(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut cams: Query<&mut Orbit>,
    mut last: Local<Option<(f32, Vec3, Vec3)>>,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let Some((min, max)) = part.bounds.0 else {
        return;
    };
    let key = (part.spread, min, max);
    if *last == Some(key) {
        return; // neither the explode distance nor the bounds moved — don't re-frame
    }
    *last = Some(key);
    let enabled = cuts.list.iter().filter(|c| c.enabled).count() as f32;
    let extra = enabled * part.spread; // exploded fans pieces this much further along X
    let span = ((max.x - min.x) + extra).max(max.y - min.y).max(80.0);
    for mut o in &mut cams {
        o.target = Vec3::new(
            (min.x + max.x) * 0.5 + extra * 0.5,
            (min.y + max.y) * 0.5,
            (min.z + max.z) * 0.5,
        );
        o.radius = span * 1.3;
    }
}

/// Inset the 3D camera's viewport to the area RIGHT of the panel, so the model auto-frames in the
/// visible region instead of centering behind the floating panel. Keyed off the panel's rendered
/// width (it tracks the file list) and the camera's target size, so it follows resize + panel growth.
pub(crate) fn split_viewport(seam: Res<PanelSeam>, mut cam: Query<&mut Camera, With<Camera3d>>) {
    let Ok(mut camera) = cam.single_mut() else {
        return;
    };
    let Some(target) = camera.physical_target_size() else {
        return;
    };
    // `panel_ui` writes each panel edge in physical px: the left panel's width, plus the top tab bar
    // + bottom status bar heights (U.3). Inset the 3D viewport inside all three so no bar occludes
    // it; leave a small gap after the left panel.
    let x0 = ((seam.width_px + 6.0).round() as u32).min(target.x.saturating_sub(1));
    let y0 = (seam.top_px.round() as u32).min(target.y.saturating_sub(1));
    let bottom = seam.bottom_px.round() as u32;
    let pos = UVec2::new(x0, y0);
    let size = UVec2::new(target.x - x0, target.y.saturating_sub(y0 + bottom).max(1));
    // Viewport isn't PartialEq — compare the fields we set to skip redundant writes.
    let unchanged = camera
        .viewport
        .as_ref()
        .is_some_and(|v| v.physical_position == pos && v.physical_size == size);
    if !unchanged {
        camera.viewport = Some(bevy::camera::Viewport {
            physical_position: pos,
            physical_size: size,
            ..default()
        });
    }
}

/// Revert to the uncut model the moment a cut is edited, so editing is always on the intact part.
/// Acts on the ACTIVE part — the only one the panel/drag can edit — swapping just its mesh.
pub(crate) fn revert_on_edit(
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut models: Query<(&mut Mesh3d, &PartId), With<Model>>,
) {
    if !parts.is_changed() {
        return;
    }
    let ap = active_part.0;
    let part = &mut parts.0[ap];
    if part.spread == 0.0 {
        return;
    }
    if let Some(h) = part.whole.clone() {
        swap_part_mesh(&mut models, ap, &h);
    }
    part.spread = 0.0;
}

/// Model + cut-plane visibility, derived authoritatively from the active view mode every frame, so
/// a mode transition can never leave the wrong things on screen: the model shows only in normal
/// view (not the 2D editor, not the print preview); the cut planes hide in the print preview.
pub(crate) fn apply_view_visibility(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    tab: Res<Tab>,
    mut models: Query<&mut Visibility, (With<Model>, Without<CutPlaneViz>)>,
    mut planes: Query<&mut Visibility, (With<CutPlaneViz>, Without<Model>)>,
) {
    let model_vis = if edit.0.is_none() && !print.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut models {
        if *v != model_vis {
            *v = model_vis;
        }
    }
    // Cut planes stay hidden in the print preview AND on the Model tab — the editor shows the
    // UNSLICED model (U.3.2); the cut overlays belong to the Parts tab.
    let plane_vis = if print.0 || *tab == Tab::Model {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut v in &mut planes {
        if *v != plane_vis {
            *v = plane_vis;
        }
    }
}

/// The 2D editor and the print preview are mutually-exclusive view modes — if both end up active,
/// keep whichever was just toggled. Reconciles whether the toggle came from a button or the harness.
pub(crate) fn enforce_exclusive_modes(mut edit: ResMut<EditCut>, mut print: ResMut<PrintView>) {
    if !(edit.0.is_some() && print.0) {
        return;
    }
    if edit.is_changed() && !print.is_changed() {
        print.0 = false; // the editor just opened — leave the print preview
    } else {
        edit.0 = None; // print just opened (or both at once) — close the editor
    }
}

/// Map the active `Tab` onto the print/editor flags the reactive systems already react to (U.3).
/// Orientation + Export both show the laid-out print pieces, so both hold `print.0` — Export needs the
/// pieces alive to co-pack them. Connector editing (`edit.0`) only belongs to the Parts tab; leaving
/// Parts closes it. Guarded writes so change-detection fires only on a real transition.
pub(crate) fn sync_tab_modes(
    tab: Res<Tab>,
    mut print: ResMut<PrintView>,
    mut edit: ResMut<EditCut>,
) {
    let want_print = matches!(*tab, Tab::Orientation | Tab::Export);
    if print.0 != want_print {
        print.0 = want_print;
    }
    if *tab != Tab::Parts && edit.0.is_some() {
        edit.0 = None;
    }
}

/// Save the orbit camera while in normal view and hand it back when a hijacking mode (2D editor,
/// print preview) closes — so leaving a mode restores the pan/orbit/zoom you had, not the mode's
/// view. Writes the transform directly on restore (like `edit_mode` does), so it doesn't depend on
/// `orbit` running that frame.
pub(crate) fn manage_view_camera(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    mut prev: ResMut<PrevCam>,
    mut cams: Query<(&mut Transform, &mut Orbit)>,
    mut was_hijack: Local<bool>,
) {
    let hijack = edit.0.is_some() || print.0;
    let Ok((mut t, mut o)) = cams.single_mut() else {
        *was_hijack = hijack;
        return;
    };
    if !hijack && *was_hijack {
        if let Some((yaw, pitch, radius, target)) = prev.0 {
            (o.yaw, o.pitch, o.radius, o.target) = (yaw, pitch, radius, target);
            *t = orbit_transform(yaw, pitch, radius, target);
        }
    } else if !hijack {
        prev.0 = Some((o.yaw, o.pitch, o.radius, o.target)); // steady normal view — remember it
    }
    *was_hijack = hijack;
}

/// Camera transform orbiting `target` at (yaw, pitch, radius), Z-up.
pub(crate) fn orbit_transform(yaw: f32, pitch: f32, radius: f32, target: Vec3) -> Transform {
    let cp = pitch.cos();
    let off = Vec3::new(
        radius * cp * yaw.cos(),
        radius * cp * yaw.sin(),
        radius * pitch.sin(),
    );
    Transform::from_translation(target + off).looking_at(target, Vec3::Z)
}
