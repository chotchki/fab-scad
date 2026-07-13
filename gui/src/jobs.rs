//! Async render/slice/publish/auto-plan orchestration + source file IO/watch.

use crate::*;

/// Idle seconds after the last editor keystroke before the buffer re-renders (U.3.2). Long enough
/// that typing doesn't kick a render mid-word, short enough to feel live.
pub(crate) const EDIT_DEBOUNCE: f64 = 0.5;

/// Auto-reload watch (5.3.3 + the DAG): the latest mtime across the source's whole include CLOSURE
/// (`fab_scad::deps`), and that closure cached. `watch_source` polls it each frame and re-renders
/// when ANY dep advances — edit an `include`d module and the preview rebuilds, not just the open
/// file. mtime-poll, not the `notify` crate — trivial syscalls, no thread/dep, same effect.
#[derive(Resource, Default)]
pub(crate) struct Watch {
    pub(crate) mtime: Option<std::time::SystemTime>,
    pub(crate) closure: Vec<PathBuf>,
}

/// The in-flight auto-plan job (auto-slice + onion auto-place, off-thread) — auto-on-open's worker.
#[derive(Resource, Default)]
pub(crate) struct AutoJob(
    pub(crate) Option<(usize, Task<Result<fab_scad::auto::AutoPlan, String>>)>,
);

/// The in-flight publish job (render artifacts + upload to hotchkiss.io, off-thread). Yields the
/// published page URL or an error string.
#[derive(Resource, Default)]
pub(crate) struct PublishJob(pub(crate) Option<Task<Result<String, String>>>);

/// A content hash of EXACTLY the inputs the slice depends on — the enabled cuts, the placed
/// connectors, and the per-piece orientations — quantised so float jitter doesn't churn it, and
/// deliberately EXCLUDING UI state like the active cut. `auto_reslice` keys the rebuild on this, not
/// Bevy change-detection, which fires on any `ResMut` deref (re-selecting a cut, a same-value field
/// echo) and would re-slice endlessly.
pub(crate) fn slice_hash(cuts: &Cuts, conns: &Conns, orient: &Orient) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |x: f32| (x as f64 * 1000.0).round() as i64; // 0.001mm — below print resolution
    for c in cuts.list.iter().filter(|c| c.enabled) {
        c.axis.index().hash(&mut h);
        q(c.at).hash(&mut h);
    }
    0xC0FFEE_u64.hash(&mut h); // section marker so [cut] vs [conn] can't alias
    for pc in &conns.list {
        pc.cut.hash(&mut h);
        q(pc.pos[0]).hash(&mut h);
        q(pc.pos[1]).hash(&mut h);
        q(pc.size).hash(&mut h);
        matches!(pc.kind, fab::ConnKind::Bolt).hash(&mut h);
        pc.screw.label().hash(&mut h);
    }
    0xBEEF_u64.hash(&mut h);
    let mut om: Vec<_> = orient.map.iter().collect(); // HashMap — sort for a stable hash
    om.sort_by_key(|(p, _)| **p);
    for (piece, up) in om {
        piece.hash(&mut h);
        up.iter().for_each(|&x| q(x).hash(&mut h));
    }
    h.finish()
}

/// Hash any `Hash` value to a `u64` — the pipeline-feedback change detector (U.3.7).
fn hash_one<T: std::hash::Hash>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// A content hash of EVERY part's SLICE inputs — enabled cuts + connectors, but NOT orientation. A cut
/// or connector change means the print pieces need re-slicing (Orientation/Export behind); an ORIENT
/// change is applied live by `sync_orientation` / `estimate_copack`, so it must NOT read as stale.
/// Reuses `slice_hash` with an empty orient (its orient section then contributes nothing). Stamped into
/// [`Pipeline::layout_of`] by `poll_print_job`.
pub(crate) fn slice_config_hash(parts: &[Part]) -> u64 {
    use std::hash::{Hash, Hasher};
    let empty = Orient::default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        slice_hash(&p.cuts, &p.conns, &empty).hash(&mut h);
    }
    h.finish()
}

/// Derive the per-node pipeline feedback (U.3.7): a stage is DIRTY when its input hash drifted off the
/// hash it last computed for — `geo_of` (source→geometry, stamped by [`poll_job`]) drives Model+Parts,
/// `layout_of` (config→print layout, stamped by [`poll_print_job`]) drives Orientation+Export, and
/// geometry-dirty propagates downstream. A stage that hasn't computed yet (`None`) reads CLEAN, so tabs
/// aren't amber before first use. `busy` is any background job in flight (render/reslice/plan/print).
pub(crate) fn sync_pipeline(
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    job: Res<Job>,
    auto: Res<AutoJob>,
    print_job: Res<PrintJob>,
    mut pipe: ResMut<Pipeline>,
) {
    let src = hash_one(&editor.text);
    let cfg = slice_config_hash(&parts.0);
    pipe.dirty = derive_dirty(pipe.geo_of, pipe.layout_of, src, cfg);
    pipe.busy = job.0.is_some() || auto.0.is_some() || print_job.0.is_some();
}

/// Per-[`Tab`](crate::Tab) stale flags from the stored vs current input hashes (the testable core of
/// [`sync_pipeline`]). Model + Parts key on source→geometry; Orientation + Export on config→layout,
/// with geometry-dirty propagating down. A stage that never computed (`None`) is CLEAN, not stale.
pub(crate) fn derive_dirty(
    geo_of: Option<u64>,
    layout_of: Option<u64>,
    src: u64,
    cfg: u64,
) -> [bool; 4] {
    let geo_dirty = geo_of.is_some_and(|h| h != src);
    let layout_dirty = geo_dirty || layout_of.is_some_and(|h| h != cfg);
    [geo_dirty, geo_dirty, layout_dirty, layout_dirty]
}

/// The reactive core (the DAG success criterion): when the slice inputs change, rebuild in the
/// BACKGROUND after a short settle — no Re-slice button. `prev` debounces (reset the clock while the
/// inputs move frame-to-frame, e.g. a cut drag); `sliced_h` records what was last sliced so identical
/// inputs never re-fire. Skips while a job runs (retries once idle) or before the bounds land.
/// `poll_job` refreshes the exploded view in place when the result lands, or banks it if assembled.
#[allow(clippy::too_many_arguments)]
pub(crate) fn auto_reslice(
    time: Res<Time>,
    mut settle: Local<f32>,
    mut prev: Local<Option<u64>>,
    mut job: ResMut<Job>,
    mut bg: ResMut<SliceInBackground>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    cfg: Res<SceneCfg>,
    mut status: ResMut<Status>,
) {
    let ap = active_part.0;
    let part = &parts.0[ap];
    if part.bounds.0.is_none() {
        return;
    }
    // The slice-hash is compared against THIS part's own `sliced_hash` — editing part A never
    // reslices part B, and switching parts re-slices only if that part's inputs actually differ.
    let h = slice_hash(&part.cuts, &part.conns, &part.orient);
    if *prev != Some(h) {
        *settle = 0.0; // inputs moved this frame → re-arm the debounce
        *prev = Some(h);
    } else {
        *settle += time.delta_secs();
    }
    if part.sliced_hash == Some(h) || job.0.is_some() {
        return; // already sliced these exact inputs, or a job is running
    }
    if *settle < AUTOSLICE_DEBOUNCE {
        return; // still settling
    }
    let xs = part.cuts.enabled_cuts();
    if xs.is_empty() {
        parts.0[ap].sliced_hash = Some(h); // nothing enabled to slice — treat as done
        return;
    }
    let conns = resolve_conns(&part.cuts, &part.conns);
    let orient = orient_inputs(&part.orient);
    let base = part.base_stl.clone();
    bg.0 = true; // background rebuild → poll_job won't jump the view to exploded
    kick_reslice(&mut job, &mut status, &cfg, ap, base, xs, conns, orient);
    parts.0[ap].sliced_hash = Some(h);
}

/// How long the slicing config must sit unchanged before autosave writes it (Phase C). Longer than a
/// drag or a keystroke burst, so a run of edits persists once, not once per frame.
const AUTOSAVE_DEBOUNCE: f32 = 1.5;

/// Debounced background autosave (U.3.14 Phase C — the reactive standard, no Save button): when the
/// live slicing config drifts off the [`SaveBaseline`] and settles, write it to `project.toml` and
/// advance the baseline. A bare open sits AT the baseline (`poll_job` seeded it from disk) → never
/// writes; an edit moves the hash → one write once the edits stop. A write error warns but still
/// advances the baseline, so a persistent failure (unwritable file) doesn't retry-storm every frame.
pub(crate) fn autosave_config(
    time: Res<Time>,
    parts: Res<Parts>,
    cfg: Res<SceneCfg>,
    mut baseline: ResMut<SaveBaseline>,
    mut settle: Local<f32>,
    mut prev: Local<Option<u64>>,
) {
    let Some(src) = cfg.source.clone() else {
        return; // no source → no project.toml to persist to
    };
    let Some(base) = baseline.0 else {
        return; // not seeded yet (before the first render)
    };
    let h = config::config_hash(&parts.0);
    if *prev != Some(h) {
        *settle = 0.0; // config moved this frame → re-arm the debounce
        *prev = Some(h);
    } else {
        *settle += time.delta_secs();
    }
    if h == base || *settle < AUTOSAVE_DEBOUNCE {
        return; // matches disk, or still settling
    }
    match config::save_slicing_config(&parts.0, &src) {
        Ok(()) => info!("autosaved slicing config"),
        Err(e) => warn!("autosave slicing config: {e:#}"),
    }
    baseline.0 = Some(h); // advance regardless — a failed write shouldn't retry-storm
}

// ---- slicing job ----------------------------------------------------------------------
/// Explicit `ReSlice` (the scripted harness; Explode when there's no slice yet) → slice NOW and
/// show the pieces (foreground). The reactive UI path is `auto_reslice` (background).
pub(crate) fn request_reslice(
    mut ev: MessageReader<ReSlice>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut bg: ResMut<SliceInBackground>,
    cfg: Res<SceneCfg>,
    parts: Res<Parts>,
    active_part: Res<ActivePart>,
) {
    if ev.read().count() == 0 {
        return;
    }
    if job.0.is_some() {
        info!("busy — ignoring re-slice");
        return;
    }
    let ap = active_part.0;
    let part = &parts.0[ap];
    let xs = part.cuts.enabled_cuts();
    if xs.is_empty() {
        status.0 = "no enabled cuts".into();
        return;
    }
    let conns = resolve_conns(&part.cuts, &part.conns);
    let orient = orient_inputs(&part.orient);
    let base = part.base_stl.clone();
    bg.0 = false; // explicit → poll_job jumps to the exploded view when it lands
    kick_reslice(&mut job, &mut status, &cfg, ap, base, xs, conns, orient);
}

/// The model-derived resources, bundled so `apply_switch_file` can wipe them in one system param
/// (Bevy caps a system at 16 params; a `SystemParam` struct counts as one). Everything here is a
/// pure function of the current source + user edits — stale the instant a different `.scad` loads.
#[derive(SystemParam)]
pub(crate) struct ModelState<'w> {
    pub(crate) parts: ResMut<'w, Parts>,
    pub(crate) active: ResMut<'w, ActivePart>,
    pub(crate) edit_cut: ResMut<'w, EditCut>,
    pub(crate) xsection: ResMut<'w, XSection>,
    pub(crate) print: ResMut<'w, PrintView>,
    pub(crate) print_job: ResMut<'w, PrintJob>,
    pub(crate) print_pieces: ResMut<'w, PrintPieces>,
    pub(crate) feas: ResMut<'w, Feas>,
    pub(crate) watch: ResMut<'w, Watch>,
}

impl ModelState<'_> {
    /// Reset to a clean slate for a freshly-loaded source: no cuts/connectors/orientations, bounds
    /// cleared so `poll_job` re-seeds the first cut, modes exited, cached meshes dropped (the whole/
    /// sliced handles live in `Part` now, so resetting `Parts` drops them), any in-flight print job
    /// cancelled, and the watch disarmed so `watch_source` records the new file's mtime.
    pub(crate) fn reset(&mut self) {
        *self.parts = Parts(vec![Part::default()]);
        self.active.0 = 0;
        *self.edit_cut = EditCut::default();
        *self.xsection = XSection::default();
        *self.print = PrintView::default();
        *self.print_job = PrintJob::default();
        *self.print_pieces = PrintPieces::default();
        *self.feas = Feas::default();
        *self.watch = Watch::default();
    }
}

/// Apply a pending file switch: point `SceneCfg.source` at file `i`, wipe the old model's state,
/// kick a fresh whole render. Row clicks, the picker landing, and the `open` script verb all funnel
/// here via `SwitchFile`.
pub(crate) fn apply_switch_file(
    mut ev: MessageReader<SwitchFile>,
    mut files: ResMut<FileList>,
    mut scene: ResMut<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut editor: ResMut<EditorBuf>,
    mut state: ModelState,
) {
    // Coalesce: only the last switch requested this frame matters.
    let Some(SwitchFile(i)) = ev.read().copied().last() else {
        return;
    };
    let Some(path) = files.files.get(i).cloned() else {
        return;
    };
    files.active = Some(i);
    scene.source = Some(path.clone());
    read_into_editor(&mut editor, &path); // the new file's disk text becomes the editor buffer (U.3.2)
    state.reset();
    kick_render(&mut job, &mut status, &scene, true);
    info!("open: {}", path.display());
}

/// Drain the native `.scad` file pick: expand the chosen file's FOLDER into the tab set (its sibling
/// `.scad`) and switch to the picked file; on cancel, nothing. The dialog future was spawned by the ＋.
pub(crate) fn poll_open_dialog(
    mut dlg: ResMut<OpenDialog>,
    mut files: ResMut<FileList>,
    mut switch: MessageWriter<SwitchFile>,
    mut status: ResMut<Status>,
) {
    let Some(task) = dlg.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return; // dialog still open
    };
    dlg.0 = None;
    let Some(picked) = result else {
        return; // cancelled
    };
    // Open the picked model's folder as tabs (both flat + `src/`-nested layouts), that file active.
    let dir = picked.parent().unwrap_or(picked.as_path());
    let scads = scad_files(dir);
    if scads.is_empty() {
        status.0 = format!("no .scad under {}", dir.display());
        return;
    }
    let active = scads.iter().position(|p| p == &picked).unwrap_or(0);
    files.files = scads;
    switch.write(SwitchFile(active));
}

/// Auto-reload (5.3.3): if the active source's mtime advanced since its last load, re-render the
/// whole model — an external editor / OpenSCAD saved it. Fires only when no job is in flight (which
/// debounces multi-write saves); the cut stack is PRESERVED (re-slice to refresh the exploded view).
pub(crate) fn watch_source(
    scene: Res<SceneCfg>,
    mut watch: ResMut<Watch>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
) {
    let Some(src) = scene.source.as_deref() else {
        return;
    };
    // Resolve the include closure once per (re)load (empty = first sight after a reset); it's
    // re-derived on a real change below so newly added/removed `include`s are tracked.
    if watch.closure.is_empty() {
        watch.closure = dep_closure(src, &scene);
    }
    // Latest mtime across the WHOLE closure — editing any included module advances it.
    let mtime = watch
        .closure
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max();
    match (watch.mtime, mtime) {
        (None, m) => watch.mtime = m, // first sight: arm, don't render (the load already did)
        // A render in flight (mtime advanced but job busy) falls through to `_` and does nothing —
        // the next idle frame retries, so a save mid-render is never lost.
        (Some(prev), Some(m)) if m > prev && job.0.is_none() => {
            watch.mtime = Some(m);
            watch.closure = dep_closure(src, &scene); // the edit may have changed the include set
            info!(
                "reload {} (+{} deps)",
                src.display(),
                watch.closure.len().saturating_sub(1)
            );
            // Reload (fresh = false): refresh geometry in place, keep each part's cuts/connectors.
            kick_render(&mut job, &mut status, &scene, false);
        }
        _ => {}
    }
}

/// The transitive `include`/`use` closure of `src`, resolved against the workspace OPENSCADPATH
/// (`root/libs` + `root/scad-lib`) — the files whose edits should trigger a rebuild.
pub(crate) fn dep_closure(src: &Path, scene: &SceneCfg) -> Vec<PathBuf> {
    let search: Vec<PathBuf> = scene
        .root
        .as_ref()
        .map(|r| vec![r.join("libs"), r.join("scad-lib")])
        .unwrap_or_default();
    fab_scad::deps::closure(src, &search).into_iter().collect()
}

/// Every `.scad` under `dir` (recursive), sorted, skipping generated/VCS/hidden dirs. The picker's
/// project→files expansion — handles both flat (`foo/bar.scad`) and `src/`-nested layouts.
pub(crate) fn scad_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_scads(dir, &mut out);
    out.sort();
    out
}

pub(crate) fn collect_scads(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // generated output + VCS + hidden dirs are never source
            if name.starts_with('.') || matches!(name.as_ref(), "out" | "renders" | "target") {
                continue;
            }
            collect_scads(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("scad"))
            && !name.starts_with('.')
        // hidden files aren't source — this also hides the editor's `.fab-preview-*.scad` (U.3.2)
        {
            out.push(p);
        }
    }
}

/// Spawn a WHOLE render of every top-level part on the async compute pool (T.2b) — `render_parts`
/// splits the model into its implicit-union children, one STL each. `fresh` distinguishes a new
/// source (replace the parts list) from a reload of the same one (refresh geometry, keep edits).
pub(crate) fn kick_render(job: &mut Job, status: &mut Status, cfg: &SceneCfg, fresh: bool) {
    let Some(src) = cfg.source.clone() else {
        status.0 = "no .scad source".into();
        return;
    };
    kick_render_from(job, status, cfg, &src, fresh);
}

/// Whole-render an EXPLICIT source path (U.3.2) — `cfg` still supplies root/tmp, but the content +
/// include base come from `src`, not `cfg.source`. The editor buffer's preview renders its hidden
/// temp this way WITHOUT repointing `cfg.source`, so `watch_source` keeps watching the real file and
/// never fights the preview.
pub(crate) fn kick_render_from(
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    src: &Path,
    fresh: bool,
) {
    let src = src.to_path_buf();
    let (root, tmp) = (cfg.root.clone(), cfg.tmp.clone());
    let task = AsyncComputeTaskPool::get().spawn(async move {
        // Each part Solid is built AND consumed inside render_parts (written to STL) — none crosses
        // this async boundary; only the STL paths return (Solid is !Send).
        fab::render_parts(root.as_deref(), &src, &tmp)
            .map(|v| JobResult::Rendered {
                fresh,
                parts: v.into_iter().map(|(p, _bbox, name)| (p, name)).collect(),
            })
            .map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "rendering".into();
}

/// Debounced live preview (U.3.2): once the editor buffer has sat un-touched for `EDIT_DEBOUNCE` and
/// no render is in flight, write it to a hidden temp beside the real file (so relative `include`s
/// resolve) and whole-render it (`fresh = false` — keep each part's cuts/connectors). The buffer,
/// not the disk file, is the truth; the disk file only changes on an explicit Save.
pub(crate) fn preview_edited_buffer(
    mut editor: ResMut<EditorBuf>,
    scene: Res<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    time: Res<Time>,
) {
    let Some(t) = editor.edited_at else {
        return;
    };
    if job.0.is_some() || time.elapsed_secs_f64() - t < EDIT_DEBOUNCE {
        return; // still typing, or a render's already running — retry next idle frame
    }
    editor.edited_at = None;
    let Some(dir) = editor.path.parent() else {
        return;
    };
    let stem = editor
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "preview".into());
    let preview = dir.join(format!(".fab-preview-{stem}.scad"));
    if std::fs::write(&preview, editor.text.as_bytes()).is_err() {
        status.0 = "preview write failed".into();
        return;
    }
    kick_render_from(&mut job, &mut status, &scene, &preview, false);
}

/// Spawn a per-part reslice off `part_stl` (part `part`'s cached whole STL) on the async compute
/// pool (T.2b). Only that part's geometry is touched; its `Model` entity (tagged `PartId(part)`) is
/// the one `poll_job` swaps when the slice lands. The Solid lives + dies inside the kernel (!Send).
#[allow(clippy::too_many_arguments)]
pub(crate) fn kick_reslice(
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    part: usize,
    part_stl: PathBuf,
    cuts: Vec<(char, f64)>,
    conns: Vec<fab::Conn>,
    orient: Vec<fab::Orient3>,
) {
    let tmp = cfg.tmp.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        fab::reslice_part_kernel(&part_stl, &cuts, &conns, &orient, SPREAD, &tmp)
            .map(|stl| JobResult::Resliced { part, stl })
            .map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "slicing".into();
}

/// Poll the in-flight job; when it lands, apply it (T.2b). A whole render seeds/refreshes ALL parts
/// and their `Model` entities; a reslice swaps exactly one part's mesh.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn poll_job(
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut parts: ResMut<Parts>,
    mut active_part: ResMut<ActivePart>,
    mut save_baseline: ResMut<SaveBaseline>,
    mut pipeline: ResMut<Pipeline>,
    editor: Res<EditorBuf>,
    cfg: Res<SceneCfg>,
    bg: Res<SliceInBackground>,
    models: Query<(Entity, &PartId), With<Model>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(JobResult::Rendered {
            fresh,
            parts: paths,
        }) => {
            // A structural change (the model's part COUNT moved) forces a full rebuild even on a
            // reload — the old Part↔entity mapping no longer holds.
            if fresh || paths.len() != parts.0.len() {
                for (e, _) in &models {
                    commands.entity(e).despawn();
                }
                let mut new: Vec<Part> = paths
                    .iter()
                    .enumerate()
                    .map(|(i, (path, name))| {
                        build_part(
                            &mut commands,
                            &mut meshes,
                            &mut materials,
                            i,
                            path,
                            name.clone(),
                        )
                    })
                    .collect();
                // Load any per-part slicing config (U.3.14 Phase B) BEFORE `new` goes live: a part
                // whose block set cuts makes `kick_auto_plan` stand down, so config wins over auto-
                // derive. No config (or a flat/legacy `[slicing]`) → every part auto-derives as before.
                if let Some(src) = &cfg.source {
                    if let Ok(m) = fab_scad::manifest::Manifest::load_near(src) {
                        config::apply_slicing_config(&mut new, &m);
                    }
                }
                // Seed the autosave baseline to the config as it stands on disk (loaded blocks, or
                // empty when none) — `autosave_config` writes only once the live config drifts off this
                // (Phase C). A config-less model auto-derives, drifts, and persists that derive once.
                save_baseline.0 = Some(config::config_hash(&new));
                *parts = Parts(new);
                active_part.0 = 0;
            } else {
                // Reload of the SAME source: refresh each part's geometry, KEEP its cuts/connectors.
                for (i, (path, name)) in paths.iter().enumerate() {
                    refresh_part(
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &models,
                        &mut parts.0[i],
                        i,
                        path,
                        name.clone(),
                    );
                }
            }
            status.0 = "ready".into();
            // The displayed geometry now matches the current source — clear the Model/Parts stale flag
            // (U.3.7). `sync_pipeline` compares the live source hash against this each frame.
            pipeline.geo_of = Some(hash_one(&editor.text));
        }
        Ok(JobResult::Resliced { part, stl }) => {
            let (mesh, _) = mesh_and_bounds(&mut meshes, &stl);
            let Some(p) = parts.0.get_mut(part) else {
                return; // the part went away under us (a reload changed the count) — drop the slice
            };
            p.sliced = Some(mesh.clone()); // bank it so the view toggle can re-show it
                                           // A BACKGROUND rebuild refreshes the display only if already exploded; an explicit
                                           // slice (or a background one while exploded) shows the fanned pieces.
            let show = !bg.0;
            if show || p.spread > 0.0 {
                despawn_part_models(&mut commands, &models, part);
                commands.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(part_material(&mut materials)),
                    Model,
                    PartId(part),
                ));
                if show {
                    p.spread = SPREAD as f32;
                }
            }
            status.0 = "ready".into();
        }
        Err(e) => {
            error!("{e}");
            status.0 = format!("error: {e}");
        }
    }
}

/// Build a fresh [`Part`] for top-level part `i` from its whole STL — spawn its `Model` entity
/// (tagged `PartId(i)`), fix its bounds, and seed a centre cut if it fits the bed.
pub(crate) fn build_part(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    i: usize,
    path: &Path,
    name: Option<String>,
) -> Part {
    let (mesh, aabb) = mesh_and_bounds(meshes, path);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials)),
        Model,
        PartId(i),
    ));
    let mut part = Part {
        base_stl: path.to_path_buf(),
        whole: Some(mesh),
        name,
        ..default()
    };
    if let Some((min, max)) = aabb {
        part.bounds.0 = Some((min, max));
        // No seed cut: kick_auto_plan derives overflowing parts (fit-to-bed + connectors) and leaves
        // fitting parts WHOLE (U.3.15). A part that fits the bed doesn't need slicing.
    }
    part
}

/// Refresh part `i`'s geometry on a RELOAD (the source was re-saved) without dropping its cuts:
/// repoint the whole mesh + base STL, respawn its `Model` showing the fresh intact part, and clear
/// its slice cache so a still-exploded part reslices off the new geometry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_part(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    models: &Query<(Entity, &PartId), With<Model>>,
    part: &mut Part,
    i: usize,
    path: &Path,
    name: Option<String>,
) {
    let (mesh, aabb) = mesh_and_bounds(meshes, path);
    despawn_part_models(commands, models, i);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials)),
        Model,
        PartId(i),
    ));
    part.base_stl = path.to_path_buf();
    part.whole = Some(mesh);
    part.name = name;
    part.spread = 0.0; // reload drops back to the intact model
    part.sliced = None;
    part.sliced_hash = None; // force a reslice off the new geometry if the part has cuts
    if part.bounds.0.is_none() {
        if let Some((min, max)) = aabb {
            part.bounds.0 = Some((min, max));
        }
    }
}

/// Despawn only part `ap`'s displayed model entity(ies), leaving the other parts on screen (T.2b).
pub(crate) fn despawn_part_models(
    commands: &mut Commands,
    models: &Query<(Entity, &PartId), With<Model>>,
    ap: usize,
) {
    for (e, pid) in models {
        if pid.0 == ap {
            commands.entity(e).despawn();
        }
    }
}

/// Publish the active model to hotchkiss.io off-thread: render the cover + low-`$fn` preview + full
/// STL and upload them via `fab_scad::publish::publish_model`, reusing the CLI's exact path. Auth +
/// base URL come from `$HIO_API_KEY` / `$HIO_URL`; title/description from the project.toml.
pub(crate) fn publish_action(
    mut ev: MessageReader<PanelCmd>,
    scene: Res<SceneCfg>,
    mut job: ResMut<PublishJob>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::Publish) {
        return;
    }
    if job.0.is_some() {
        status.0 = "already publishing…".into();
        return;
    }
    let Some(src) = scene.source.clone() else {
        status.0 = "no .scad to publish".into();
        return;
    };
    let Ok(key) = std::env::var("HIO_API_KEY") else {
        status.0 = "set $HIO_API_KEY to publish".into();
        return;
    };
    let base = std::env::var("HIO_URL").unwrap_or_else(|_| "https://hotchkiss.io".to_string());
    let (root, out) = (scene.root.clone(), scene.tmp.join("publish"));
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let oscad = fab_scad::openscad::Openscad::discover(root.as_deref())
            .map_err(|e| format!("{e:#}"))?;
        // Title/description from the nearest project.toml; fall back to the file stem.
        let (title, description) = match fab_scad::manifest::Manifest::load_near(&src) {
            Ok(m) => {
                let title = m.title().to_string();
                (title, m.publish.map(|p| p.description).unwrap_or_default())
            }
            Err(_) => (
                src.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "model".into()),
                String::new(),
            ),
        };
        fab_scad::publish::publish_model(
            &oscad,
            &src,
            &title,
            &description,
            &base,
            &key,
            &out,
            std::time::Duration::from_secs(180),
        )
        .map_err(|e| format!("{e:#}"))
    });
    job.0 = Some(task);
    status.0 = "publishing…".into();
}

/// Land the publish job: show the URL, or the error.
pub(crate) fn poll_publish(mut job: ResMut<PublishJob>, mut status: ResMut<Status>) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(url) => {
            status.0 = format!("published → {url}");
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("publish failed: {e}"),
    }
}

/// Open the active `.scad` source in the OpenSCAD GUI (detached) so you can edit it; the file-watch
/// re-renders here on save.
/// "Reset to auto" (the `PanelCmd::AutoSlice` button): wipe the active part's cuts + connectors and
/// re-arm `kick_auto_plan` to re-derive the FULL plan — fit-to-bed cuts + auto-placed connectors, or
/// WHOLE if the part fits. The reactive loop reslices. (U.3.15: the old action re-derived cuts only
/// and dropped connectors; this restores them.)
pub(crate) fn auto_slice_action(
    mut ev: MessageReader<PanelCmd>,
    mut parts: ResMut<Parts>,
    active_part: Res<ActivePart>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::AutoSlice) {
        return;
    }
    let part = &mut parts.0[active_part.0];
    part.cuts.list.clear();
    part.conns.list.clear();
    part.auto_planned.0 = None; // re-arm kick_auto_plan to re-derive cuts + connectors
    status.0 = "reset to auto — re-deriving…".into();
}

/// Auto-derive on open: EVERY part that overflows the bed auto-plans (fit-to-bed cuts + onion
/// auto-place, off-thread) — ONE part at a time (`AutoJob` is single-slot). A part that FITS the bed
/// stays WHOLE (no cuts) and is marked planned so it's not re-checked. Once per source per part;
/// parts that already have cuts are left alone. (U.3.15: was active-part-only, so parts ≥1 never
/// derived until you clicked into them.)
pub(crate) fn kick_auto_plan(
    mut parts: ResMut<Parts>,
    scene: Res<SceneCfg>,
    mut job: ResMut<AutoJob>,
    mut status: ResMut<Status>,
) {
    if job.0.is_some() {
        return; // one already in flight
    }
    let Some(src) = scene.source.clone() else {
        return;
    };
    let bed = bed_size().unwrap_or([256.0; 3]);
    for i in 0..parts.0.len() {
        let part = &mut parts.0[i];
        if part.auto_planned.0.as_deref() == Some(src.as_path()) || !part.cuts.list.is_empty() {
            continue; // already planned this source, or already has cuts
        }
        let Some((min, max)) = part.bounds.0 else {
            continue; // not built yet — try next frame
        };
        let (lo, hi) = (
            [min.x as f64, min.y as f64, min.z as f64],
            [max.x as f64, max.y as f64, max.z as f64],
        );
        if fab_scad::auto_slice::auto_slice(
            FVec3::from_array(lo),
            FVec3::from_array(hi),
            Dims::from_array(bed),
        )
        .is_empty()
        {
            part.auto_planned.0 = Some(src.clone()); // fits the bed → stays whole, stop re-checking
            continue;
        }
        let base_stl = part.base_stl.clone(); // this part's whole STL (render_parts output)
        if base_stl.as_os_str().is_empty() || !base_stl.exists() {
            continue; // this part's base not rendered to disk yet — try next frame
        }
        part.auto_planned.0 = Some(src.clone()); // fire once per source
        let task = AsyncComputeTaskPool::get().spawn(async move {
            // In-process cross-sections — the base Solid lives + dies inside fab::auto_plan (!Send).
            fab::auto_plan(&base_stl, lo, hi, bed).map_err(|e| format!("{e:#}"))
        });
        job.0 = Some((i, task));
        status.0 = format!("auto-planning part {}…", i + 1);
        return; // one at a time
    }
}

/// Land the auto-plan onto the part it was kicked for: seed that part's cut stack + connectors, and
/// the reactive loop reslices.
pub(crate) fn poll_auto_plan(
    mut job: ResMut<AutoJob>,
    mut parts: ResMut<Parts>,
    mut status: ResMut<Status>,
) {
    let Some((i, task)) = job.0.as_mut() else {
        return;
    };
    let i = *i;
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    let part = &mut parts.0[i];
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    match result {
        Ok(plan) => {
            cuts.list = plan
                .cuts
                .iter()
                .map(|&(ax, at)| CutDef {
                    axis: match ax {
                        'y' => Axis::Y,
                        'z' => Axis::Z,
                        _ => Axis::X,
                    },
                    at: at as f32,
                    enabled: true,
                })
                .collect();
            cuts.active = 0;
            conns.list = plan
                .connectors
                .iter()
                .map(|c| PlacedConn {
                    cut: c.cut,
                    pos: [c.pos[0].f() as f32, c.pos[1].f() as f32],
                    size: c.size.unwrap_or(6.0) as f32,
                    kind: if c.kind == "bolt" {
                        fab::ConnKind::Bolt
                    } else {
                        fab::ConnKind::Onion
                    },
                    screw: Screw::M3,
                })
                .collect();
            status.0 = format!(
                "auto-planned part {}: {} cut(s), {} connector(s)",
                i + 1,
                cuts.list.len(),
                conns.list.len()
            );
            info!("{}", status.0);
        }
        Err(e) => status.0 = format!("auto-plan failed: {e:#}"),
    }
}
