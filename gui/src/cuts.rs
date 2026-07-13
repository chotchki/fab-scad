//! Cut planes, drag/click interactions, connector placement math, overlays + dim labels.

use crate::*;

/// The 3D point of a `(pos_a, pos_b)` on a cut plane: `at` along the axis, pos in the two non-axis
/// dims (ascending) — the inverse of the connector projection.
pub(crate) fn profile_point(axis: Axis, at: f32, pos: [f32; 2]) -> Vec3 {
    let ai = axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let mut p = with_comp(Vec3::ZERO, ai, at);
    p = with_comp(p, others[0], pos[0]);
    p = with_comp(p, others[1], pos[1]);
    p
}

/// A connector's 3D point: `at` along its cut's axis, `pos` in the two non-axis dims (matching
/// the driver's projection). `None` if the cut it references is gone.
pub(crate) fn conn_point(cuts: &Cuts, pc: &PlacedConn) -> Option<Vec3> {
    let c = cuts.list.get(pc.cut)?;
    let ai = c.axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let mut p = with_comp(Vec3::ZERO, ai, c.at);
    p = with_comp(p, others[0], pc.pos[0]);
    p = with_comp(p, others[1], pc.pos[1]);
    Some(p)
}

/// A 3D marker (small sphere) for a placed connector, by its index in the `Conns` list.
#[derive(Component)]
pub(crate) struct ConnMarker(pub(crate) usize);

/// A cut-plane overlay, tied to its cut in the stack by index. Tracks its axis so the plane mesh
/// can be rebuilt when the cut is rotated.
#[derive(Component)]
pub(crate) struct CutPlaneViz {
    pub(crate) idx: usize,
    pub(crate) axis: Axis,
}

/// A floating piece-width label (one per piece), positioned by projecting the piece centre to screen.
#[derive(Component)]
pub(crate) struct DimLabel {
    pub(crate) idx: usize,
}

// ---- cut stack: drag, buttons, overlays -----------------------------------------------
/// Begin dragging when a left-press lands on a cut plane: make it active + let orbit yield.
pub(crate) fn on_drag_start(
    ev: On<Pointer<DragStart>>,
    planes: Query<&CutPlaneViz>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut dragging: ResMut<DraggingCut>,
) {
    if ev.event.button != PointerButton::Primary {
        return;
    }
    let part = &mut parts.0[active_part.0];
    if part.spread > 0.0 {
        return; // exploded view is read-only — leave the drag to orbit the camera
    }
    if let Ok(cpv) = planes.get(ev.entity) {
        part.cuts.active = cpv.idx;
        dragging.0 = true;
    }
}

/// Drag the active cut along X: cast a ray from the cursor, find where it's closest to the cut
/// axis, and write that into the active cut (sync_overlay_visuals then moves the overlay).
pub(crate) fn on_drag(
    ev: On<Pointer<Drag>>,
    planes: Query<(), With<CutPlaneViz>>,
    dragging: Res<DraggingCut>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
) {
    if !dragging.0 || !planes.contains(ev.entity) {
        return;
    }
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let Ok((camera, cam_tf)) = cam.single() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_tf, ev.pointer_location.position) else {
        return;
    };
    // Axis line through the model centre, along the active cut's axis.
    let axis = cuts.active_axis();
    let ai = axis.index();
    let p0 = with_comp((min + max) * 0.5, ai, 0.0);
    let cut_at = closest_on_axis(p0, axis.unit(), ray.origin, *ray.direction)
        .clamp(comp(min, ai), comp(max, ai));
    let a = cuts.active;
    if let Some(c) = cuts.list.get_mut(a) {
        c.at = cut_at;
    }
}

pub(crate) fn on_drag_end(_ev: On<Pointer<DragEnd>>, mut dragging: ResMut<DraggingCut>) {
    dragging.0 = false;
}

/// Click a cut plane: select it (collapsed/editing), or — in the read-only exploded view — flash
/// the Collapse button to point the user back to editing.
pub(crate) fn on_click(
    ev: On<Pointer<Click>>,
    planes: Query<&CutPlaneViz>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
) {
    let part = &mut parts.0[active_part.0];
    let Ok(cpv) = planes.get(ev.entity) else {
        return;
    };
    // In the read-only exploded view a plane click does nothing; in editing it selects the cut.
    if part.spread == 0.0 {
        part.cuts.active = cpv.idx;
    }
}

/// Delete cut `idx`, keeping the connectors consistent: connectors store cut indices into the
/// stack, so a bare `remove` would silently re-point survivors at the wrong cut. Drop the deleted
/// cut's connectors and renumber the rest (a connector on a later cut shifts down one).
pub(crate) fn remove_cut(cuts: &mut Cuts, conns: &mut Conns, idx: usize) {
    if idx >= cuts.list.len() {
        return;
    }
    cuts.list.remove(idx);
    if !cuts.list.is_empty() && cuts.active >= cuts.list.len() {
        cuts.active = cuts.list.len() - 1;
    }
    conns.list.retain(|c| c.cut != idx);
    for c in conns.list.iter_mut() {
        if c.cut > idx {
            c.cut -= 1;
        }
    }
}

/// Smallest onion worth placing (mm): below this the wall/slab is too thin for a useful joint, so
/// we decline rather than punch an oversized peg through it.
pub(crate) const MIN_ONION: f32 = 2.0;

/// Material to leave between the onion's equator and the nearest edge / slab face.
pub(crate) const ONION_WALL: f64 = 1.2;

/// Largest onion the auto-sizer will grow to in open material.
pub(crate) const ONION_MAX_D: f64 = 16.0;

/// Max gap between alignment onions (mm) — auto-place guarantees every stretch of a join face is
/// within this of an onion, so no long span sags. The alignment interval, not a fill pitch.
pub(crate) const ONION_SPACING: f64 = 80.0;

/// The onion teardrop's tip reaches r/sin(ang) past centre in the cap (+build) direction. `ang` is
/// set by the piece's print orientation — decided AFTER the onion is sized — so the sizer bounds for
/// the WORST case: the steepest cap the slicer emits (`CAP_ANG_MIN` = 20° in slicing.rs), tip
/// 1/sin(20°) ≈ 2.92·r. Onions near the +build edge shrink so the tip fits at any orientation; they
/// guide alignment for clamp-and-glue, so smaller is fine (chotchki's call).
pub(crate) const ONION_TIP: f64 = 2.9238; // 1 / sin(20°)

/// The onion cap direction (+build = +Z) in a cut's 2D cross-section coords, or `None` when the cap
/// points OUT of the section plane (a Z cut) — there the cap is bounded axially, not in-section.
pub(crate) fn cap_dir_2d(axis: Axis) -> Option<[f64; 2]> {
    match axis {
        Axis::X | Axis::Y => Some([0.0, 1.0]), // +Z is the section's second coord for X/Y cuts
        Axis::Z => None,
    }
}

/// Place a `kind` connector on `cut` at `pos` (onion diameter `size`, or the `screw` for a bolt), or
/// — if the click lands on one already there — remove it (click-to-toggle). Declines a sub-`MIN_ONION`
/// onion (too thin a spot); a bolt has no such gate. Returns a one-line status describing what it did.
pub(crate) fn toggle_connector(
    conns: &mut Conns,
    cut: usize,
    pos: [f32; 2],
    size: f32,
    kind: fab::ConnKind,
    screw: Screw,
) -> &'static str {
    const HIT: f32 = 5.0; // mm; a bit larger than the 3mm marker, so it's a forgiving target
    if let Some(j) = conns
        .list
        .iter()
        .position(|c| c.cut == cut && (c.pos[0] - pos[0]).hypot(c.pos[1] - pos[1]) < HIT)
    {
        conns.list.remove(j);
        "removed connector"
    } else if kind == fab::ConnKind::Onion && size < MIN_ONION {
        "too thin for an onion here"
    } else {
        conns.list.push(PlacedConn {
            cut,
            pos,
            size,
            kind,
            screw,
        });
        match kind {
            fab::ConnKind::Onion => "placed onion",
            fab::ConnKind::Bolt => "placed bolt",
        }
    }
}

/// Auto-size a connector at `pos` on `cut`: the largest onion that FITS — both the cut's
/// cross-section wall (`fit_diameter`) AND the slab thickness either side of the cut (the onion is
/// a sphere, so it reaches d/2 into each piece along the cut axis — cap it so it can't pierce the
/// thinner slab). No lower clamp: a thin spot returns a small (sub-`MIN_ONION`) value and the caller
/// declines. Falls back to a modest default when there's no cross-section (headless `conn` verb).
pub(crate) fn auto_size(
    xsection: &XSection,
    cuts: &Cuts,
    bounds: &ModelBounds,
    cut: usize,
    pos: [f32; 2],
) -> f32 {
    const DEFAULT: f32 = 6.0;
    let axis = cuts.list.get(cut).map(|c| c.axis).unwrap_or(Axis::X);
    let cross = match &xsection.0 {
        Some(loops) => {
            let loops: Vec<Vec<[f64; 2]>> = loops
                .iter()
                .map(|l| l.iter().map(|&[a, b]| [a as f64, b as f64]).collect())
                .collect();
            fab_scad::cross_section::fit_onion(
                &loops,
                [pos[0] as f64, pos[1] as f64],
                ONION_WALL,
                ONION_MAX_D,
                cap_dir_2d(axis),
                ONION_TIP,
            ) as f32
        }
        None => DEFAULT,
    };
    cross.min(axial_cap(cuts, cut, bounds))
}

/// The onion-diameter cap from the slab thickness either side of `cut`. The onion reaches d/2 (the
/// sphere) into each piece along the cut axis, EXCEPT for a Z cut, where the cap points +Z (the
/// build) into the upper slab and reaches √2·d/2 — so that side reserves the tip, not the sphere.
pub(crate) fn axial_cap(cuts: &Cuts, cut: usize, bounds: &ModelBounds) -> f32 {
    let (below, above) = axial_room(cuts, cut, bounds);
    let is_z = cuts
        .list
        .get(cut)
        .map(|c| c.axis == Axis::Z)
        .unwrap_or(false);
    let wall = ONION_WALL as f32;
    let below_d = 2.0 * (below - wall);
    // Z cut: the cap (+Z) reaches into the upper (above) slab as the teardrop tip, √2·r deep.
    let above_d = if is_z {
        2.0 * (above - wall) / ONION_TIP as f32
    } else {
        2.0 * (above - wall)
    };
    below_d.min(above_d).max(0.0)
}

/// The room bordering `cut` along its axis on each side (below, above): distance from the cut to its
/// nearest same-axis neighbour (or the model bound) on each side. Huge (no cap) if bounds are unset.
pub(crate) fn axial_room(cuts: &Cuts, cut: usize, bounds: &ModelBounds) -> (f32, f32) {
    let (Some(c), Some((min, max))) = (cuts.list.get(cut), bounds.0) else {
        return (f32::INFINITY, f32::INFINITY);
    };
    let (ai, at) = (c.axis.index(), c.at);
    let mut below = comp(min, ai);
    let mut above = comp(max, ai);
    for (j, o) in cuts.list.iter().enumerate() {
        if j == cut || o.axis != c.axis {
            continue;
        }
        if o.at <= at && o.at > below {
            below = o.at;
        }
        if o.at >= at && o.at < above {
            above = o.at;
        }
    }
    (at - below, above - at)
}

/// In the 2D connector editor: a click on the (face-on) cut plane drops a connector on the cut
/// being edited, at the clicked point — the precise picking the assembled-model click can't give.
#[allow(clippy::too_many_arguments)] // a Bevy observer — params are dependencies, not a smell
pub(crate) fn place_on_profile_click(
    ev: On<Pointer<Click>>,
    editing: Res<EditCut>,
    planes: Query<&CutPlaneViz>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    xsection: Res<XSection>,
    active: Res<ActiveConn>,
    mut status: ResMut<Status>,
) {
    let part = &mut parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    let conns = &mut part.conns;
    let Some(i) = editing.0 else {
        return;
    };
    if ev.event.button != PointerButton::Primary || planes.get(ev.entity).is_err() {
        return;
    }
    let (Some(hit), Some(c)) = (ev.event.hit.position, cuts.list.get(i)) else {
        return;
    };
    let others: Vec<usize> = (0..3).filter(|&a| a != c.axis.index()).collect();
    let pos = [comp(hit, others[0]), comp(hit, others[1])];
    let size = auto_size(&xsection, cuts, bounds, i, pos);
    status.0 = toggle_connector(conns, i, pos, size, active.kind, active.screw).into();
}

/// Auto-place connectors across the OPEN cut's cross-section (#41): a grid of wall-fitting onions
/// over the cut face (`cross_section::auto_place`), each capped by the slab's axial room, replacing
/// that cut's existing connectors with a fresh auto-layout. Manual tweaks (place/remove) still work
/// on top. No-op with a hint if no editor is open.
pub(crate) fn do_auto_place(
    mut ev: MessageReader<AutoPlace>,
    edit: Res<EditCut>,
    xsection: Res<XSection>,
    mut parts: ResMut<Parts>,
    active: Res<ActiveConn>,
    active_part: Res<ActivePart>,
    mut status: ResMut<Status>,
) {
    if ev.read().count() == 0 {
        return;
    }
    let part = &mut parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    let conns = &mut part.conns;
    let (Some(i), Some(loops)) = (edit.0, xsection.0.as_ref()) else {
        status.0 = "open a cut's connector editor to auto-place".into();
        return;
    };
    let loops: Vec<Vec<[f64; 2]>> = loops
        .iter()
        .map(|l| l.iter().map(|&[a, b]| [a as f64, b as f64]).collect())
        .collect();
    let axis = cuts.list.get(i).map(|c| c.axis).unwrap_or(Axis::X);
    let placements = fab_scad::cross_section::auto_place(
        &loops,
        ONION_WALL,
        ONION_MAX_D,
        ONION_SPACING,
        MIN_ONION as f64,
        cap_dir_2d(axis),
        ONION_TIP,
    );
    let cap = axial_cap(cuts, i, bounds);

    // Corner clearance: an onion near where ANOTHER cut crosses this one straddles the intersection
    // — the messy jigsaw case. Project each PERPENDICULAR enabled cut into this cross-section's 2D
    // coords (a line at `coord = at`) and drop placements whose footprint (radius d/2 + a wall) would
    // reach it. Cuts on this cut's own axis are parallel — they never cross this face.
    let ai = axis.index();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    let perp: Vec<(usize, f64)> = cuts
        .list
        .iter()
        .enumerate()
        .filter(|&(j, c)| j != i && c.enabled)
        .filter_map(|(_, c)| match c.axis.index() {
            a if a == others[0] => Some((0, c.at as f64)),
            a if a == others[1] => Some((1, c.at as f64)),
            _ => None, // same axis as this cut → parallel, no intersection line
        })
        .collect();

    conns.list.retain(|c| c.cut != i); // fresh auto-layout for this cut
    let mut n = 0;
    for (p, d) in placements {
        // Skip onions that don't clear the perpendicular cuts (see above).
        let clearance = d / 2.0 + ONION_WALL;
        if perp.iter().any(|&(c, at)| (p[c] - at).abs() < clearance) {
            continue;
        }
        // The fitted onion diameter doubles as a "has room" proxy for a bolt (auto-place fits to the
        // cross-section either way); the active type decides what actually lands here.
        let size = (d as f32).min(cap);
        if size >= MIN_ONION {
            conns.list.push(PlacedConn {
                cut: i,
                pos: [p[0] as f32, p[1] as f32],
                size,
                kind: active.kind,
                screw: active.screw,
            });
            n += 1;
        }
    }
    let noun = match active.kind {
        fab::ConnKind::Onion => "onion",
        fab::ConnKind::Bolt => "bolt",
    };
    info!("auto-place: {n} {noun}(s) on cut {i}");
    status.0 = format!("auto-placed {n} {noun}{}", if n == 1 { "" } else { "s" });
}

/// Keep one sphere marker per placed connector: respawn the set when the count changes, and each
/// frame park each marker at its connector's point (so dragging a cut moves its markers too).
/// Hidden in the exploded view, since the pieces (and their pockets) have fanned apart.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_conn_markers(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    print: Res<PrintView>,
    mut last: Local<usize>,
    existing: Query<Entity, With<ConnMarker>>,
    mut markers: Query<(&ConnMarker, &mut Transform, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    let part = &parts.0[active_part.0];
    let conns = &part.conns;
    let cuts = &part.cuts;
    if conns.list.len() != *last {
        *last = conns.list.len();
        for e in &existing {
            commands.entity(e).despawn();
        }
        let mesh = meshes.add(Sphere::new(3.0));
        for i in 0..conns.list.len() {
            // Each marker gets its OWN material so color_conn_markers can tint it by feasibility.
            let mat = mats.add(StandardMaterial {
                base_color: Color::srgb(0.30, 0.85, 0.70),
                unlit: true,
                depth_bias: 1.0e8, // render on top — the connector point sits inside the solid model
                ..default()
            });
            commands.spawn((
                ConnMarker(i),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat),
                Transform::default(),
                Visibility::Hidden,
            ));
        }
        return; // positions land next frame, once the new markers exist
    }
    // Show a marker only in the collapsed assembled view, and only for a connector on an ENABLED
    // cut — one on a disabled cut isn't in the slice, so showing it (teal/clean) would mislead.
    let live = part.spread == 0.0 && !print.0;
    for (m, mut tf, mut vis) in &mut markers {
        let point = live
            .then(|| conns.list.get(m.0))
            .flatten()
            .filter(|pc| cuts.list.get(pc.cut).is_some_and(|c| c.enabled))
            .and_then(|pc| conn_point(cuts, pc));
        match point {
            Some(p) => {
                tf.translation = p;
                *vis = Visibility::Visible;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

/// React to opening/closing a cut's connector editor: compute the cut's cross-section (blocking,
/// but fast — projects the already-rendered preview STL, no re-render) and face the camera onto the
/// cut. Editing is always on the collapsed whole model, so opening drops any explode. The model's
/// visibility + the camera restore on close are owned by `apply_view_visibility` / `manage_view_camera`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn edit_mode(
    edit: Res<EditCut>,
    print: Res<PrintView>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut xsection: ResMut<XSection>,
    mut models: Query<(&mut Mesh3d, &PartId), With<Model>>,
    mut cam: Query<(&mut Transform, &mut Orbit)>,
    mut status: ResMut<Status>,
) {
    if !edit.is_changed() {
        return;
    }
    let ap = active_part.0;
    let Some(i) = edit.0 else {
        xsection.0 = None;
        if !print.0 {
            status.0 = "ready".into(); // closed the editor (unless print took over)
        }
        return;
    };
    // Edit on the collapsed whole model so the profile + the cut plane overlay line up.
    let whole = {
        let part = &mut parts.0[ap];
        part.spread = 0.0;
        part.whole.clone()
    };
    if let Some(h) = whole {
        swap_part_mesh(&mut models, ap, &h);
    }
    let part = &parts.0[ap];
    let Some(c) = part.cuts.list.get(i) else {
        xsection.0 = None;
        return;
    };
    let (axis, at) = (c.axis, c.at); // copy out so the `part` borrow can drop before the cam block
    let bounds = part.bounds.0;
    let base_stl = part.base_stl.clone(); // this part's whole STL — the cross-section source
    // Face the camera square onto the cut (Z avoids the up=Z gimbal with a near-top-down pitch).
    // Set the transform here directly — `orbit` yields while editing, so it won't apply it for us.
    if let Ok((mut t, mut o)) = cam.single_mut() {
        use std::f32::consts::FRAC_PI_2;
        (o.yaw, o.pitch) = match axis {
            Axis::X => (0.0, 0.0),
            Axis::Y => (FRAC_PI_2, 0.0),
            Axis::Z => (-FRAC_PI_2, FRAC_PI_2 - 0.01),
        };
        // Look at the cut's centre: model centre in the non-axis dims, `at` along the axis.
        let center = bounds.map(|(mn, mx)| (mn + mx) * 0.5).unwrap_or(Vec3::ZERO);
        o.target = with_comp(center, axis.index(), at);
        *t = orbit_transform(o.yaw, o.pitch, o.radius, o.target);
    }
    match fab::cross_section(&base_stl, axis.index(), at as f64) {
        Ok(loops) => {
            xsection.0 = Some(
                loops
                    .into_iter()
                    .map(|l| l.into_iter().map(|[a, b]| [a as f32, b as f32]).collect())
                    .collect(),
            );
            status.0 = format!("editing connectors on {} cut", axis.label());
        }
        Err(e) => {
            status.0 = format!("cross-section failed: {e}");
            xsection.0 = None;
        }
    }
}

/// Draw the open cut's profile as a line-loop outline, plus — at each placed connector — its
/// footprint, drawn PER KIND so the editor shows what you picked: a teal circle at the onion's
/// auto-sized diameter, or an amber circle + cross sized to the bolt's screw head.
pub(crate) fn draw_profile(
    xsection: Res<XSection>,
    edit: Res<EditCut>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut gizmos: Gizmos,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    let (Some(loops), Some(i)) = (&xsection.0, edit.0) else {
        return;
    };
    let Some(c) = cuts.list.get(i) else {
        return;
    };
    let outline = Color::srgb(0.35, 0.8, 1.0);
    for lp in loops {
        let n = lp.len();
        for j in 0..n {
            let a = profile_point(c.axis, c.at, lp[j]);
            let b = profile_point(c.axis, c.at, lp[(j + 1) % n]);
            gizmos.line(a, b, outline);
        }
    }
    // Connector footprints on the cut plane, coloured + shaped by kind (matches the 3D markers).
    let onion_col = Color::srgb(0.30, 0.85, 0.70); // teal
    let bolt_col = Color::srgb(0.95, 0.70, 0.20); // amber
    let rot = Quat::from_rotation_arc(Vec3::Z, c.axis.unit());
    for pc in conns.list.iter().filter(|pc| pc.cut == i) {
        let center = profile_point(c.axis, c.at, pc.pos);
        let iso = Isometry3d::new(center, rot);
        match pc.kind {
            fab::ConnKind::Onion => {
                // The onion is a TEARDROP, cap along the build (+Z): the tip reaches √2·r past centre
                // (ang=45). Draw that, not a bare circle, so the real footprint — and any poke past
                // the profile edge — is visible. When the cut is ⟂ the build (Z-cut) the cap points
                // out of plane, so the 2D footprint IS the equatorial circle.
                let r = pc.size / 2.0;
                let normal = c.axis.unit();
                let cap = Vec3::Z - normal * Vec3::Z.dot(normal); // build, projected into the cut plane
                gizmos.circle(iso, r, onion_col);
                if cap.length() > 1e-3 {
                    let cap = cap.normalize();
                    let tip = center + cap * (r * std::f32::consts::SQRT_2);
                    let t1 = center
                        + (Quat::from_axis_angle(normal, std::f32::consts::FRAC_PI_4) * cap) * r;
                    let t2 = center
                        + (Quat::from_axis_angle(normal, -std::f32::consts::FRAC_PI_4) * cap) * r;
                    gizmos.line(t1, tip, onion_col);
                    gizmos.line(t2, tip, onion_col);
                }
            }
            fab::ConnKind::Bolt => {
                let r = pc.screw.head_r();
                gizmos.circle(iso, r, bolt_col);
                // a small cross so a bolt reads as a screw hole, not just a smaller onion
                let right = rot * (Vec3::X * r);
                let up = rot * (Vec3::Y * r);
                gizmos.line(center - right, center + right, bolt_col);
                gizmos.line(center - up, center + up, bolt_col);
            }
        }
    }
}

/// Keep one overlay per cut. When the cut COUNT changes (add/remove), the index→cut mapping can
/// shift, so despawn all + respawn fresh; positions/colours are then refreshed by
/// sync_overlay_visuals. (Runs on every cut change, but only rebuilds on a count change.)
pub(crate) fn sync_overlays(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    existing: Query<Entity, With<CutPlaneViz>>,
    mut last: Local<usize>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if !parts.is_changed() || *last == cuts.list.len() {
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    for e in &existing {
        commands.entity(e).despawn();
    }
    for (i, c) in cuts.list.iter().enumerate() {
        spawn_cut_plane(&mut commands, &mut meshes, &mut materials, min, max, c, i);
    }
    *last = cuts.list.len();
}

/// Position + orient + colour each overlay from its cut. Position tracks the displayed geometry:
/// at `cut.at` when editing, fanned into the piece gaps when exploded. The plane mesh is rebuilt
/// (thin along the cut axis) only when the cut is rotated.
pub(crate) fn sync_overlay_visuals(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut overlays: Query<(
        &mut CutPlaneViz,
        &mut Transform,
        &mut Mesh3d,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) {
    if !parts.is_changed() {
        return;
    }
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let spread = part.spread; // spread now lives in Part → parts.is_changed() covers it
    let Some((min, max)) = part.bounds.0 else {
        return;
    };
    for (mut cpv, mut tf, mut mesh3d, mat) in &mut overlays {
        let idx = cpv.idx;
        let Some(c) = cuts.list.get(idx) else {
            continue;
        };
        if cpv.axis != c.axis {
            cpv.axis = c.axis;
            mesh3d.0 = meshes.add(plane_cuboid(c.axis, min, max));
        }
        tf.translation = cut_center(cuts, idx, min, max, spread);
        if let Some(mut m) = materials.get_mut(&mat.0) {
            m.base_color = cut_color(idx == cuts.active, c.enabled);
        }
    }
}

/// Offset of cut `idx` along ITS axis in the exploded layout (the slicer fans piece k by
/// `k*spread`): an enabled cut sits in the gap (+0.5) above the same-axis cuts below it; a
/// disabled cut rides with the piece it's inside. 0 when not exploded.
pub(crate) fn spread_offset(cuts: &Cuts, idx: usize, spread: f32) -> f32 {
    if spread == 0.0 {
        return 0.0;
    }
    let Some(cut) = cuts.list.get(idx) else {
        return 0.0;
    };
    let rank = cuts
        .list
        .iter()
        .enumerate()
        .filter(|(j, o)| *j != idx && o.enabled && o.axis == cut.axis && o.at < cut.at)
        .count() as f32;
    if cut.enabled {
        (rank + 0.5) * spread
    } else {
        rank * spread
    }
}

/// World-space centre of a cut's overlay: at its position (+ explode offset) along its axis,
/// centred on the model in the other two.
pub(crate) fn cut_center(cuts: &Cuts, idx: usize, min: Vec3, max: Vec3, spread: f32) -> Vec3 {
    let Some(c) = cuts.list.get(idx) else {
        return (min + max) * 0.5;
    };
    with_comp(
        (min + max) * 0.5,
        c.axis.index(),
        c.at + spread_offset(cuts, idx, spread),
    )
}

/// A thin slab spanning the model in the two axes the cut doesn't slice.
pub(crate) fn plane_cuboid(axis: Axis, min: Vec3, max: Vec3) -> Cuboid {
    let s = (max - min) * 1.15;
    match axis {
        Axis::X => Cuboid::new(0.6, s.y.max(1.0), s.z.max(1.0)),
        Axis::Y => Cuboid::new(s.x.max(1.0), 0.6, s.z.max(1.0)),
        Axis::Z => Cuboid::new(s.x.max(1.0), s.y.max(1.0), 0.6),
    }
}

/// Piece-width dimensions for EVERY axis that has an enabled cut: per axis, a leader line parallel to
/// the cut, offset a hair off the part, with end ticks and the width as a white centred number, in
/// that axis's colour (X red / Y green / Z blue). Safe to show all at once now that gizmos render on
/// the 3D camera only — the old "scatter" was the UI camera ghosting each leader, not the extra axes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_dim_labels(
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    print: Res<PrintView>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    existing: Query<&DimLabel>,
    mut labels: Query<(&DimLabel, &mut Node, &mut Text, &mut Visibility)>,
    mut commands: Commands,
    mut gizmos: Gizmos,
) {
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let bounds = &part.bounds;
    if print.0 {
        // The print preview lays pieces out apart — the assembled-part width labels don't apply.
        for (_, _, _, mut vis) in &mut labels {
            *vis = Visibility::Hidden;
        }
        return;
    }
    let Some((min, max)) = bounds.0 else {
        return;
    };
    let Ok((camera, cam_gt)) = cam.single() else {
        return;
    };
    let gap = ((max - min).max_element() * 0.05).clamp(8.0, 22.0);
    let tick = gap * 0.35;
    let mut segs: Vec<(Vec3, f32)> = Vec::new();
    // Dimension every axis that has an enabled cut — each on its own edge, in its own colour.
    for axis in [Axis::X, Axis::Y, Axis::Z] {
        let ai = axis.index();
        let dim_col = match axis {
            Axis::X => Color::srgb(1.0, 0.4, 0.4),
            Axis::Y => Color::srgb(0.4, 0.9, 0.45),
            Axis::Z => Color::srgb(0.5, 0.6, 1.0),
        };
        let mut xs: Vec<f32> = cuts
            .list
            .iter()
            .filter(|c| c.enabled && c.axis == axis)
            .map(|c| c.at)
            .collect();
        if xs.is_empty() {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut edges = vec![comp(min, ai)];
        edges.extend(xs);
        edges.push(comp(max, ai));

        // Leader line offset off the model on p0, at the near face on p1 (index-addressed).
        let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
        let (p0, p1) = (others[0], others[1]);
        let off0 = comp(min, p0) - gap;
        let face1 = comp(min, p1);
        let dim_pt = |v: f32| {
            let mut a = [0.0f32; 3];
            a[ai] = v;
            a[p0] = off0;
            a[p1] = face1;
            Vec3::from_array(a)
        };
        let face_pt = |v: f32| {
            let mut a = [0.0f32; 3];
            a[ai] = v;
            a[p0] = comp(min, p0);
            a[p1] = face1;
            Vec3::from_array(a)
        };
        let tvec = {
            let mut a = [0.0f32; 3];
            a[p0] = tick;
            Vec3::from_array(a)
        };
        // Dimensions stay at the ASSEMBLED positions — they don't ride the explode.
        for w in edges.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            let (a, b) = (dim_pt(lo), dim_pt(hi));
            gizmos.line(a, b, dim_col);
            gizmos.line(face_pt(lo), a, dim_col.with_alpha(0.4));
            gizmos.line(face_pt(hi), b, dim_col.with_alpha(0.4));
            gizmos.line(a - tvec, a + tvec, dim_col);
            gizmos.line(b - tvec, b + tvec, dim_col);
            segs.push(((a + b) * 0.5, w[1] - w[0]));
        }
    }

    // Spawn a label entity for any segment that lacks one (count only grows).
    for i in existing.iter().count()..segs.len() {
        commands.spawn((
            Text::new(""),
            TextColor(Color::srgb(0.95, 0.95, 1.0)),
            TextFont::from_font_size(13.0),
            Node {
                position_type: PositionType::Absolute,
                ..default()
            },
            DimLabel { idx: i },
        ));
    }

    for (dl, mut node, mut text, mut vis) in &mut labels {
        let Some(&(pos, width)) = segs.get(dl.idx) else {
            *vis = Visibility::Hidden;
            continue;
        };
        match camera.world_to_viewport(cam_gt, pos) {
            Ok(p) => {
                let s = format!("{width:.0}");
                // Centre the number on the anchor — Node left/top is its top-left corner, so back
                // off half the glyph run (~7px/char at size 13) and half the line height.
                node.left = px(p.x - s.len() as f32 * 3.5);
                node.top = px(p.y - 8.0);
                *text = Text::new(s);
                *vis = Visibility::Visible;
            }
            Err(_) => *vis = Visibility::Hidden,
        }
    }
}

pub(crate) fn cut_color(active: bool, enabled: bool) -> Color {
    // Distinct hues, none of them the model's yellow: green = editing, blue = on, red = off.
    if !enabled {
        Color::srgba(0.95, 0.30, 0.30, 0.30) // off — red
    } else if active {
        Color::srgba(0.25, 1.0, 0.35, 0.65) // active — bright green
    } else {
        Color::srgba(0.20, 0.55, 1.0, 0.50) // on — blue
    }
}

/// Offset along `axis` (a unit vector through `p0`) of the point on that line closest to the ray
/// (`ray_d` unit). The classic skew-line solution; pure, so it's unit-tested.
pub(crate) fn closest_on_axis(p0: Vec3, axis: Vec3, ray_o: Vec3, ray_d: Vec3) -> f32 {
    let w0 = p0 - ray_o;
    let b = axis.dot(ray_d);
    let d = axis.dot(w0);
    let e = ray_d.dot(w0);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-6 {
        return 0.0; // ray ~parallel to the axis — no stable solution
    }
    (b * e - d) / denom
}

/// Colour each connector marker by kind + feasibility: amber = a bolt (explicit); teal = an onion
/// that prints support-free; red = an onion that can't and downgrades to a bolt under the current
/// orientations. Live feedback in the assembled/exploded view.
pub(crate) fn color_conn_markers(
    feas: Res<Feas>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    markers: Query<(&ConnMarker, &MeshMaterial3d<StandardMaterial>)>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let conns = &parts.0[active_part.0].conns;
    for (m, mat) in &markers {
        let want = match conns.list.get(m.0).map(|c| c.kind) {
            Some(fab::ConnKind::Bolt) => Color::srgb(0.95, 0.70, 0.20), // amber = bolt
            _ if feas.0.get(m.0).copied().unwrap_or(true) => Color::srgb(0.30, 0.85, 0.70), // teal onion
            _ => Color::srgb(0.95, 0.35, 0.25), // red = onion that downgrades to a bolt
        };
        if let Some(mut material) = mats.get_mut(&mat.0)
            && material.base_color != want
        {
            material.base_color = want;
        }
    }
}

pub(crate) fn clamp_to_bounds(x: f32, axis: Axis, bounds: &ModelBounds) -> f32 {
    match bounds.0 {
        Some((min, max)) => x.clamp(comp(min, axis.index()), comp(max, axis.index())),
        None => x,
    }
}

/// A translucent slab on a cut, thin along its axis, spanning the model in the other two.
pub(crate) fn spawn_cut_plane(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    min: Vec3,
    max: Vec3,
    cut: &CutDef,
    idx: usize,
) {
    commands.spawn((
        Mesh3d(meshes.add(plane_cuboid(cut.axis, min, max))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: cut_color(true, cut.enabled),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            // The cut plane sits INSIDE the solid model, so without this the model occludes it and
            // its coplanar faces z-fight (the "layering" mess). Bias it to the front like the
            // connector markers do (line ~1205) → a clean translucent guide seen through the part.
            depth_bias: 1.0e8,
            ..default()
        })),
        Transform::from_translation(with_comp((min + max) * 0.5, cut.axis.index(), cut.at)),
        CutPlaneViz {
            idx,
            axis: cut.axis,
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_ray_maps_to_axis_coordinate() {
        // A ray pointing straight down (-Z) through x=5 is closest to the X axis at x=5.
        let sc = closest_on_axis(Vec3::ZERO, Vec3::X, Vec3::new(5.0, 0.0, 10.0), Vec3::NEG_Z);
        assert!((sc - 5.0).abs() < 1e-4, "expected 5.0, got {sc}");
    }

    #[test]
    fn drag_ray_parallel_to_axis_is_safe() {
        // Ray parallel to the axis is degenerate — return 0, never NaN.
        let sc = closest_on_axis(Vec3::ZERO, Vec3::X, Vec3::new(0.0, 1.0, 0.0), Vec3::X);
        assert_eq!(sc, 0.0);
    }

    #[test]
    fn enabled_cuts_filter_out_disabled_and_carry_axis() {
        let cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 5.0,
                    enabled: false,
                },
                CutDef {
                    axis: Axis::Y,
                    at: 20.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        assert_eq!(cuts.enabled_cuts(), vec![('x', -10.0), ('y', 20.0)]);
    }

    #[test]
    fn toggle_connector_places_then_removes_on_a_second_nearby_click() {
        let mut conns = Conns::default();
        let onion = fab::ConnKind::Onion;
        toggle_connector(&mut conns, 0, [20.0, -10.0], 10.0, onion, Screw::M3); // place
        assert_eq!(conns.list.len(), 1);
        toggle_connector(&mut conns, 0, [22.0, -8.0], 10.0, onion, Screw::M3); // within 5mm → removes it
        assert!(conns.list.is_empty());
        toggle_connector(&mut conns, 0, [20.0, -10.0], 10.0, onion, Screw::M3); // place again
        toggle_connector(&mut conns, 0, [0.0, 0.0], 10.0, onion, Screw::M3); // far away → a second
        assert_eq!(conns.list.len(), 2);
        toggle_connector(&mut conns, 1, [20.0, -10.0], 10.0, onion, Screw::M3); // diff cut → places
        assert_eq!(conns.list.len(), 3);
    }

    #[test]
    fn toggle_connector_declines_a_too_thin_onion_but_not_a_bolt() {
        let mut conns = Conns::default();
        toggle_connector(
            &mut conns,
            0,
            [0.0, 0.0],
            1.0,
            fab::ConnKind::Onion,
            Screw::M3,
        ); // sub-MIN_ONION
        assert!(conns.list.is_empty(), "a too-thin onion is declined");
        toggle_connector(
            &mut conns,
            0,
            [0.0, 0.0],
            5.0,
            fab::ConnKind::Onion,
            Screw::M3,
        ); // fits
        assert_eq!(conns.list.len(), 1);
        // A bolt has no onion thin-gate — it places regardless of the fitted diameter.
        toggle_connector(
            &mut conns,
            1,
            [0.0, 0.0],
            1.0,
            fab::ConnKind::Bolt,
            Screw::M4,
        );
        assert_eq!(conns.list.len(), 2);
        assert!(matches!(conns.list[1].kind, fab::ConnKind::Bolt));
    }

    #[test]
    fn axial_room_reports_both_bordering_slabs() {
        let cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 0.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 16.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        let bounds = ModelBounds(Some((Vec3::splat(-20.0), Vec3::splat(20.0))));
        // middle cut: (below to -10 = 10, above to 16 = 16)
        assert_eq!(axial_room(&cuts, 1, &bounds), (10.0, 16.0));
        // first cut: (below to the -20 bound = 10, above to the cut at 0 = 10)
        assert_eq!(axial_room(&cuts, 0, &bounds), (10.0, 10.0));
        // last cut: (below to the cut at 0 = 16, above to the +20 bound = 4 — the thin end slab)
        assert_eq!(axial_room(&cuts, 2, &bounds), (16.0, 4.0));
    }

    #[test]
    fn remove_cut_renumbers_surviving_connectors() {
        let mut cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 0.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 10.0,
                    enabled: true,
                },
            ],
            active: 2,
        };
        let onion = fab::ConnKind::Onion;
        let mut conns = Conns {
            list: vec![
                PlacedConn {
                    cut: 0,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                },
                PlacedConn {
                    cut: 1,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                }, // deleted
                PlacedConn {
                    cut: 2,
                    pos: [0.0, 0.0],
                    size: 6.0,
                    kind: onion,
                    screw: Screw::M3,
                }, // shifts down
            ],
        };
        remove_cut(&mut cuts, &mut conns, 1);
        assert_eq!(cuts.list.len(), 2);
        assert_eq!(cuts.active, 1); // clamped from 2
        let cuts_of: Vec<usize> = conns.list.iter().map(|c| c.cut).collect();
        assert_eq!(cuts_of, vec![0, 1]); // cut-0 connector kept; cut-1 dropped; cut-2 → cut-1
    }

    #[test]
    fn spread_offset_is_per_axis() {
        // Two X cuts + one Y cut. The second X cut (rank 1) sits in the gap above one X piece;
        // the Y cut (rank 0 on its own axis) is unaffected by the X cuts.
        let cuts = Cuts {
            list: vec![
                CutDef {
                    axis: Axis::X,
                    at: -10.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::X,
                    at: 20.0,
                    enabled: true,
                },
                CutDef {
                    axis: Axis::Y,
                    at: 0.0,
                    enabled: true,
                },
            ],
            active: 0,
        };
        assert_eq!(spread_offset(&cuts, 0, 10.0), 5.0); // X rank 0 → (0+0.5)*10
        assert_eq!(spread_offset(&cuts, 1, 10.0), 15.0); // X rank 1 → (1+0.5)*10
        assert_eq!(spread_offset(&cuts, 2, 10.0), 5.0); // Y rank 0 → (0+0.5)*10
    }
}
