//! Print preview: per-piece orientation, shelf packing, feasibility + 3MF export.

use crate::*;

/// Rendered print pieces, each tagged with its PART index (T.2b co-pack — one preview holds all
/// parts' pieces).
pub(crate) type PartPieces = Vec<(usize, fab::PiecePrint)>;

/// The in-flight print-layout render (off-thread): renders + auto-orients every piece. Yields the
/// pieces (mesh + multi-index + build-up) on success, else an error string. The job fans
/// `print_layout_kernel` over every top-level part and concatenates.
#[derive(Resource, Default)]
pub(crate) struct PrintJob(pub(crate) Option<Task<Result<PartPieces, String>>>);

/// The last print-layout's rendered pieces (part index + piece), kept so a manual re-orient
/// re-lays-out from the cached meshes (no re-render). Cleared when the preview closes.
#[derive(Resource, Default)]
pub(crate) struct PrintPieces(pub(crate) Option<PartPieces>);

/// One laid-out piece in the print-orientation preview, tagged with its cross-part [`PrintId`]
/// (part + slab + comp) so a click→orient pick routes to the right part's orient. Despawned when
/// the preview closes.
#[derive(Component)]
pub(crate) struct PrintPiece(pub(crate) PrintId);

// ---- print-orientation preview --------------------------------------------------------
/// Enter/leave the print-orientation preview on a toggle. Entering hides the model + cut planes and
/// kicks the per-piece render/auto-orient job; leaving despawns the laid-out pieces and restores
/// the model. A `Local` tracks the last state so the initial (false) frame isn't a spurious leave.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enter_exit_print(
    print: Res<PrintView>,
    edit: Res<EditCut>,
    mut was_on: Local<bool>,
    parts: Res<Parts>,
    mut job: ResMut<PrintJob>,
    mut cache: ResMut<PrintPieces>,
    mut status: ResMut<Status>,
    pieces: Query<Entity, With<PrintPiece>>,
    mut commands: Commands,
) {
    if print.0 == *was_on {
        return; // no transition (and not the initial add) — nothing to do
    }
    *was_on = print.0;
    if print.0 {
        cache.0 = None; // cuts may have moved — wait for a fresh render before laying out
        if job.0.is_none() {
            // CO-PACK (T.2b.4): gather EVERY part's slice spec so the job lays out + packs them all.
            let specs: Vec<PartPrintSpec> = parts
                .0
                .iter()
                .enumerate()
                .map(|(i, p)| PartPrintSpec {
                    part: i,
                    base_stl: p.base_stl.clone(),
                    cuts: p.cuts.enabled_cuts(),
                    conns: resolve_conns(&p.cuts, &p.conns),
                })
                .collect();
            kick_print_job(&mut job, &mut status, specs);
        }
    } else {
        for e in &pieces {
            commands.entity(e).despawn();
        }
        cache.0 = None;
        if edit.0.is_none() {
            status.0 = "ready".into(); // don't clobber the editor when print closed because it opened
        }
    }
    // The model + cut-plane visibility (hidden in the preview, shown otherwise) is owned by
    // apply_view_visibility; the camera hand-back is owned by manage_view_camera.
}

/// One part's inputs for the co-pack print layout (T.2b.4). All owned + Send — moved into the
/// off-thread job, which slices each part off its own cached STL (no Solid crosses the boundary).
pub(crate) struct PartPrintSpec {
    pub(crate) part: usize,
    pub(crate) base_stl: PathBuf,
    pub(crate) cuts: Vec<(char, f64)>,
    pub(crate) conns: Vec<fab::Conn>,
}

/// Spawn the per-piece render + auto-orient on the compute pool (off-thread, so the UI stays live
/// while the plate lays out). CO-PACK (T.2b.4): fans `print_layout_kernel` over EVERY part's spec
/// and concatenates the pieces, each tagged with its part index, so one preview holds all parts.
pub(crate) fn kick_print_job(job: &mut PrintJob, status: &mut Status, specs: Vec<PartPrintSpec>) {
    if specs.iter().all(|s| s.base_stl.as_os_str().is_empty()) {
        status.0 = "nothing to print".into();
        return;
    }
    let task = AsyncComputeTaskPool::get().spawn(async move {
        // In-process via the Manifold kernel (11.12): each part sliced off its cached base, both
        // passes off that base. Pieces from all parts concatenate, tagged with the part index.
        let mut all: Vec<(usize, fab::PiecePrint)> = Vec::new();
        for s in &specs {
            if s.base_stl.as_os_str().is_empty() {
                continue; // an un-rendered part — skip it
            }
            let pieces = fab::print_layout_kernel(&s.base_stl, &s.cuts, &s.conns)
                .map_err(|e| format!("{e:#}"))?;
            all.extend(pieces.into_iter().map(|pp| (s.part, pp)));
        }
        Ok(all)
    });
    job.0 = Some(task);
    status.0 = "orienting pieces".into();
}

/// Poll the print-layout job; when it lands, cache the rendered pieces (so a manual re-orient can
/// re-lay-out without re-rendering) and seed every piece's auto-orientation. A fresh render is a
/// fresh auto-pick: it resets the orientations (dropping prior manual overrides — re-entering the
/// preview is the reset gesture). `relayout_pieces` does the actual layout from here.
pub(crate) fn poll_print_job(
    mut job: ResMut<PrintJob>,
    mut parts: ResMut<Parts>,
    mut cache: ResMut<PrintPieces>,
    mut status: ResMut<Status>,
) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    let pieces = match result {
        Ok(p) => p,
        Err(e) => {
            error!("{e}");
            status.0 = format!("error: {e}");
            return;
        }
    };
    // A fresh render is a fresh auto-pick across ALL parts — clear every part's orient, then seed
    // each piece's build-up into ITS OWN part's map ((slab,comp) collides across parts, so route by
    // part index) (T.2b.4 co-pack).
    for p in &mut parts.0 {
        p.orient.map.clear();
        p.orient.manual.clear();
    }
    for (part, pp) in &pieces {
        if let Some(p) = parts.0.get_mut(*part) {
            p.orient.map.insert((pp.piece, pp.comp), pp.up);
        }
    }
    let n = pieces.len();
    cache.0 = Some(pieces);
    status.0 = format!(
        "{n} piece{}: click a face to set print-down",
        if n == 1 { "" } else { "s" }
    );
}

/// React to a change in cuts / connectors / orientations / cache: recompute every connector's onion
/// feasibility (drives the marker colour + the downgrade count), and — in the preview — lay the
/// cached pieces out rotated to their build-up, shelf-packed and centred on the bed (z=0). ONE
/// system so feasibility and layout share a run: the status's downgrade count is always the count
/// for the orientation just laid out, never a frame stale. Re-runs from the mesh cache (no OpenSCAD).
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_orientation(
    print: Res<PrintView>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
    cache: Res<PrintPieces>,
    cfg: Res<SceneCfg>,
    mut feas: ResMut<Feas>,
    existing: Query<Entity, With<PrintPiece>>,
    mut cams: Query<&mut Orbit>,
    mut status: ResMut<Status>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !(parts.is_changed() || cache.is_changed()) {
        return;
    }
    let part = &parts.0[active_part.0];
    let cuts = &part.cuts;
    let conns = &part.conns;
    let orient = &part.orient;

    // Feasibility: resolve placed connectors to the enabled-cut indexing (tracking their source
    // index), run the SAME gate the slice applies, write flags back aligned with `Conns::list`.
    let enabled = cuts.enabled_indices();
    let mut resolved = Vec::new();
    let mut src = Vec::new();
    for (i, pc) in conns.list.iter().enumerate() {
        if let Some(ei) = enabled.iter().position(|&si| si == pc.cut) {
            resolved.push(fab::Conn {
                cut: ei,
                pos: [pc.pos[0] as f64, pc.pos[1] as f64],
                size: pc.size as f64,
                kind: pc.kind,
                screw: pc.screw.label(),
            });
            src.push(i);
        }
    }
    let mut flags = vec![true; conns.list.len()];
    match fab::conn_feasibility(&cuts.enabled_cuts(), &resolved, &orient_inputs(orient)) {
        Ok(f) => {
            for (k, ok) in f.into_iter().enumerate() {
                flags[src[k]] = ok;
            }
        }
        Err(e) => error!("feasibility: {e:#}"),
    }
    let down = flags.iter().filter(|&&ok| !ok).count();
    feas.0 = flags;

    // Layout: only in the preview, only once a render has populated the cache.
    if !print.0 {
        return;
    }
    let Some(pieces) = &cache.0 else {
        return;
    };
    for e in &existing {
        commands.entity(e).despawn();
    }

    // Pass 1: shelf-pack from the origin (walk a row left→right, wrap at the bed width), each piece
    // rotated to its build-up. Translation lands the rotated min-corner at the cursor, on z=0.
    let gap = 6.0_f32;
    let bw = cfg.bed[0];
    let (mut cx, mut cy, mut row_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let (mut bb_x, mut bb_y, mut bb_z) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut placed: Vec<(usize, Quat, Vec3)> = Vec::new(); // (piece index, rotation, translation)
    for (i, (pidx, pp)) in pieces.iter().enumerate() {
        // Each piece resolves its build-up against ITS OWN part's orient (co-pack, T.2b.4).
        let up = Vec3::from_array(parts.0[*pidx].orient.up_or((pp.piece, pp.comp), pp.up))
            .normalize_or_zero();
        let rot = if up == Vec3::ZERO {
            Quat::IDENTITY
        } else {
            Quat::from_rotation_arc(up, Vec3::Z)
        };
        let (rmin, rmax) = rotated_bounds(&pp.mesh.positions, rot);
        let (w, h) = (rmax.x - rmin.x, rmax.y - rmin.y);
        if cx > 0.0 && cx + w > bw {
            cx = 0.0;
            cy += row_h + gap;
            row_h = 0.0;
        }
        placed.push((i, rot, Vec3::new(cx - rmin.x, cy - rmin.y, -rmin.z)));
        bb_x = bb_x.max(cx + w);
        bb_y = bb_y.max(cy + h);
        bb_z = bb_z.max(rmax.z - rmin.z); // a tilted piece stands tall — frame for it too
        cx += w + gap;
        row_h = row_h.max(h);
    }

    // Pass 2: centre the block on the bed origin (like a slicer auto-arrange) and spawn.
    let shift = Vec3::new(-bb_x * 0.5, -bb_y * 0.5, 0.0);
    for &(i, rot, t) in &placed {
        let mat = materials.add(StandardMaterial {
            base_color: Color::hsl((i as f32 * 47.0) % 360.0, 0.55, 0.55),
            perceptual_roughness: 0.7,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(build_mesh(&pieces[i].1.mesh))),
            MeshMaterial3d(mat),
            Transform {
                translation: t + shift,
                rotation: rot,
                ..default()
            },
            PrintPiece((pieces[i].0, pieces[i].1.piece, pieces[i].1.comp)),
        ));
    }
    // Frame the laid-out block — including a tall tilted piece (account for height, raise the target).
    let span = bb_x.max(bb_y).max(bb_z).max(80.0);
    for mut o in &mut cams {
        o.target = Vec3::new(0.0, 0.0, bb_z * 0.4);
        o.radius = span * 1.3;
    }
    let n = pieces.len();
    status.0 = if down > 0 {
        format!(
            "{n} pieces, {down} onion{} -> bolt (this orientation)",
            if down == 1 { "" } else { "s" }
        )
    } else {
        format!("{n} pieces oriented, onions print clean")
    };
}

/// Click a piece's face in the preview to lay that face on the bed: the new build-up is the
/// model-space direction opposite the clicked face (the piece's current rotation maps model→world,
/// so invert it to bring the hit normal back to model space). Recorded as a manual override.
pub(crate) fn orient_piece_on_click(
    ev: On<Pointer<Click>>,
    print: Res<PrintView>,
    pieces: Query<(&PrintPiece, &Transform)>,
    mut parts: ResMut<Parts>,
) {
    if !print.0 || ev.event.button != PointerButton::Primary {
        return;
    }
    let (Ok((pp, tf)), Some(world_n)) = (pieces.get(ev.entity), ev.event.hit.normal) else {
        return;
    };
    let up_model = -(tf.rotation.inverse() * world_n);
    // The PrintId carries the part index — route the manual override to THAT part's orient (co-pack).
    let (part, slab, comp) = pp.0;
    if let Some(p) = parts.0.get_mut(part) {
        p.orient
            .set_manual((slab, comp), up_model.normalize_or_zero().to_array());
    }
}

/// AABB of `positions` after applying `rot` (the print-up rotation), for shelf-packing the piece.
pub(crate) fn rotated_bounds(positions: &[[f32; 3]], rot: Quat) -> (Vec3, Vec3) {
    let mut it = positions.iter().map(|p| rot * Vec3::from_array(*p));
    let first = it.next().unwrap_or(Vec3::ZERO);
    let (mut min, mut max) = (first, first);
    for v in it {
        min = min.min(v);
        max = max.max(v);
    }
    (min, max)
}

/// Between-piece + edge spacing left on the export plates (mm).
pub(crate) const PLATE_GAP: f64 = 5.0;

/// Export the print-oriented pieces as a Bambu multi-plate project `.3mf` next to the source. Runs
/// inline — a handful of piece meshes to a zip is quick — and the status line reports the plate
/// count + fill so you can see how tight it packed. The bed comes from the loaded scene, so it must
/// match the printer the project opens on (Bambu bins pieces to plates by position).
pub(crate) fn export_plates_action(
    mut ev: MessageReader<PanelCmd>,
    pieces: Res<PrintPieces>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::Export) {
        return;
    }
    let Some(list) = pieces.0.as_ref().filter(|l| !l.is_empty()) else {
        status.0 = "no pieces to export — slice first".into();
        return;
    };
    // Co-pack ALL parts' pieces (T.2b.4): each piece's build-up resolves against ITS OWN part's
    // orient (manual override if set, else the auto-pick); bambu bin-packs the flat list onto shared
    // plates, re-seating every piece so cross-part model positions don't matter.
    let refs: Vec<&fab::PiecePrint> = list.iter().map(|(_, pp)| pp).collect();
    let ups: Vec<[f64; 3]> = list
        .iter()
        .map(|(part, pp)| {
            let u = parts.0[*part].orient.up_or((pp.piece, pp.comp), pp.up);
            [u[0] as f64, u[1] as f64, u[2] as f64]
        })
        .collect();
    let out = match &scene.source {
        Some(s) => {
            let stem = s.file_stem().and_then(|n| n.to_str()).unwrap_or("part");
            s.with_file_name(format!("{stem}-plates.3mf"))
        }
        None => scene.tmp.join("plates.3mf"),
    };
    let bed = [scene.bed[0] as f64, scene.bed[1] as f64];
    match fab::export_plates(&refs, &ups, bed, PLATE_GAP, &out) {
        Ok(sum) => {
            status.0 = format!(
                "exported {} piece(s) on {} plate(s), {}% full → {}",
                sum.pieces,
                sum.plates,
                (sum.fill * 100.0).round() as i32,
                out.display()
            );
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("export failed: {e:#}"),
    }
}

/// Reactively recompute the co-pack preview summary (U.3.5) whenever the print pieces or their
/// orientations change — the Export tab's `plates · pieces · fits WxH` metric, no button. Cheap: a
/// footprint-only bin-pack (`fab::copack_summary`), no 3mf written. Clears the summary when there are
/// no pieces (or none fit the bed). Guards stale part indices — it runs on ANY `parts` change, which
/// can briefly precede `PrintPieces` being rebuilt.
pub(crate) fn estimate_copack(
    pieces: Res<PrintPieces>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    mut copack: ResMut<CoPack>,
) {
    if !pieces.is_changed() && !parts.is_changed() {
        return;
    }
    copack.bed = scene.bed;
    let Some(list) = pieces.0.as_ref().filter(|l| !l.is_empty()) else {
        copack.summary = None;
        return;
    };
    // Co-pack ALL parts' pieces, each build-up resolved against its OWN part's orient. Skip a piece
    // whose part index is momentarily out of range (PrintPieces stale vs a just-changed Parts).
    let (refs, ups): (Vec<&fab::PiecePrint>, Vec<[f64; 3]>) = list
        .iter()
        .filter_map(|(part, pp)| {
            parts.0.get(*part).map(|p| {
                let u = p.orient.up_or((pp.piece, pp.comp), pp.up);
                (pp, [u[0] as f64, u[1] as f64, u[2] as f64])
            })
        })
        .unzip();
    let bed = [scene.bed[0] as f64, scene.bed[1] as f64];
    copack.summary = fab::copack_summary(&refs, &ups, bed, PLATE_GAP).ok();
}
