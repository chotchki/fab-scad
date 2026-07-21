//! Async render/slice/publish/auto-plan orchestration + source file IO.

use crate::*;

/// Idle seconds after the last editor keystroke before the buffer re-renders (U.3.2). Long enough
/// that typing doesn't kick a render mid-word, short enough to feel live.
pub(crate) const EDIT_DEBOUNCE: f64 = 0.5;

/// An off-thread auto-plan's payload: the fit-to-bed cuts + WIRE connectors + the part's component
/// count, straight off the service's `Planned` response (W.3.3).
type AutoPlanResult = Result<(Vec<(char, f64)>, Vec<WireConn>, usize), String>;

/// The in-flight auto-plan job (auto-slice + onion auto-place, off-thread) — auto-on-open's worker.
/// Carries the target part index alongside the task.
#[derive(Resource, Default)]
pub(crate) struct AutoJob(pub(crate) Option<(usize, Task<AutoPlanResult>)>);

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
    // Which stage a job feeds: render/reslice AND auto-plan produce GEOMETRY (Model+Parts); the print
    // job produces the LAYOUT (Orientation+Export) — mirroring `derive_dirty`'s geo/layout split.
    let auto_part = auto.0.as_ref().map(|(i, _)| *i);
    let geo = job.0.is_some() || auto_part.is_some();
    let layout = print_job.0.is_some();
    pipe.busy = geo || layout;
    pipe.loading = derive_loading(geo, layout);
    pipe.activity = busy_activity(auto_part, job.0.is_some(), layout);
}

/// Per-[`Tab`](crate::Tab) "computing now" flags (the testable core of the spinner badge). Geometry
/// work (render / reslice / auto-plan) lights Model + Parts; layout work (print) lights Orientation +
/// Export — the same geo/layout split as [`derive_dirty`], but keyed on IN-FLIGHT jobs so it fires on
/// the first compute too (when nothing is yet "dirty").
pub(crate) fn derive_loading(geo: bool, layout: bool) -> [bool; 4] {
    [geo, geo, layout, layout]
}

/// The accurate status label for what's running (the testable core of the status-bar pulse). Prefers
/// the most specific: an auto-plan names its part; else a geometry job; else the print layout; `None`
/// when idle. The status bar shows this while busy so the pulse can never read a stale terminal
/// status like "ready" mid-render.
pub(crate) fn busy_activity(
    auto_part: Option<usize>,
    geo_job: bool,
    print: bool,
) -> Option<String> {
    if let Some(i) = auto_part {
        Some(format!("auto-planning part {}…", i + 1))
    } else if geo_job {
        Some("rebuilding geometry…".into())
    } else if print {
        Some("orienting pieces…".into())
    } else {
        None
    }
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
    pool: Res<GeomPool>,
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
    let Some(base) = part.base else {
        return; // no held base yet (render still in flight) — retry once it lands
    };
    let conns = resolve_conns(&part.cuts, &part.conns);
    let orient = orient_inputs(&part.orient);
    bg.0 = true; // background rebuild → poll_job won't jump the view to exploded
    kick_reslice(&pool, &mut job, &mut status, ap, base, xs, conns, orient);
    parts.0[ap].sliced_hash = Some(h);
}

// (W.3.8: the reactive `project.toml`/toml_edit autosave retired — config now persists in the .scad's
// `fab:config` block on Save/download, one mechanism both platforms. See `config::with_config_block`.)

// ---- slicing job ----------------------------------------------------------------------
/// Explicit `ReSlice` (the scripted harness; Explode when there's no slice yet) → slice NOW and
/// show the pieces (foreground). The reactive UI path is `auto_reslice` (background).
pub(crate) fn request_reslice(
    mut ev: MessageReader<ReSlice>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut bg: ResMut<SliceInBackground>,
    pool: Res<GeomPool>,
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
    let Some(base) = part.base else {
        status.0 = "not rendered yet".into();
        return;
    };
    let conns = resolve_conns(&part.cuts, &part.conns);
    let orient = orient_inputs(&part.orient);
    bg.0 = false; // explicit → poll_job jumps to the exploded view when it lands
    kick_reslice(&pool, &mut job, &mut status, ap, base, xs, conns, orient);
}

/// The model-derived resources, bundled so `apply_switch_file` can wipe them in one system param
/// (Bevy caps a system at 16 params; a `SystemParam` struct counts as one). Everything here is a
/// pure function of the current source + user edits — stale the instant a different `.scad` loads.
#[derive(SystemParam)]
// On wasm `apply_switch_file`'s native tail is cfg'd out and `project_files_action` doesn't compile, so
// nothing reads these fields there — the struct stays (it's a param of the cross-platform switch system).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct ModelState<'w> {
    pub(crate) parts: ResMut<'w, Parts>,
    pub(crate) active: ResMut<'w, ActivePart>,
    pub(crate) edit_cut: ResMut<'w, EditCut>,
    pub(crate) xsection: ResMut<'w, XSection>,
    pub(crate) print: ResMut<'w, PrintView>,
    pub(crate) print_job: ResMut<'w, PrintJob>,
    pub(crate) print_pieces: ResMut<'w, PrintPieces>,
    pub(crate) feas: ResMut<'w, Feas>,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl ModelState<'_> {
    /// Reset to a clean slate for a freshly-loaded source: no cuts/connectors/orientations, bounds
    /// cleared so `poll_job` re-seeds the first cut, modes exited, cached meshes dropped (the whole/
    /// sliced handles live in `Part` now, so resetting `Parts` drops them), any in-flight print job
    /// cancelled.
    pub(crate) fn reset(&mut self) {
        *self.parts = Parts(vec![Part::default()]);
        self.active.0 = 0;
        *self.edit_cut = EditCut::default();
        *self.xsection = XSection::default();
        *self.print = PrintView::default();
        *self.print_job = PrintJob::default();
        *self.print_pieces = PrintPieces::default();
        *self.feas = Feas::default();
    }
}

/// Apply a pending file switch: point `SceneCfg.source` at file `i`, wipe the old model's state,
/// kick a fresh whole render. Row clicks, the picker landing, and the `open` script verb all funnel
/// here via `SwitchFile`.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn apply_switch_file(
    mut ev: MessageReader<SwitchFile>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut scene: ResMut<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut editor: ResMut<EditorBuf>,
    mut pending_config: ResMut<PendingConfig>,
    pool: Res<GeomPool>,
    mut state: ModelState,
) {
    // Coalesce: only the last switch requested this frame matters.
    let Some(SwitchFile(i)) = ev.read().copied().last() else {
        return;
    };
    // Web (Z.3.4): no file paths — a switch operates on the ProjectDoc directly (FileList is native-only).
    // It just swaps the editor VIEW; the render target is the ENTRY (via render_pack), unaffected by a
    // view-switch, so no re-render — editing the newly-viewed file re-renders it through the preview.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (
            &mut scene,
            &mut job,
            &mut status,
            &pool,
            &mut state,
            &mut pending_config,
        );
        let holds = project
            .files
            .get(project.active)
            .is_some_and(|f| std::path::PathBuf::from(&f.name) == editor.path);
        if holds {
            project.flush_active(&editor.text);
        }
        project.set_active(i);
        if let Some(f) = project.files.get(i) {
            editor.text = f.text.clone();
            editor.path = std::path::PathBuf::from(&f.name);
            editor.dirty = f.dirty;
            editor.edited_at = None;
        }
        return;
    }
    // Native: the render paths ARE ProjectDoc's native projection (base_dir/name per file). The whole
    // tail is native — it reads a real path + kicks a Source::Path render — so it's cfg'd off wasm.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = project.native_paths().get(i).cloned() else {
            return;
        };
        // Z.3.6 option A: EVERY project renders its ENTRY. Switching a file changes the editor VIEW, not
        // the render target — view a lib and the entry stays on screen; set-entry to change what renders.
        // Persist the OUTGOING file's live edit before moving off it — but ONLY when the editor actually
        // holds the current active file (a within-project switch). On a FRESH open the editor still carries
        // the PREVIOUS project's text, and flushing that would clobber the new entry.
        let holds_active = project.files.get(project.active).is_some_and(|f| {
            let expected = match project.base_dir.as_ref() {
                Some(b) => b.join(&f.name),
                None => std::path::PathBuf::from(&f.name),
            };
            expected == editor.path
        });
        if holds_active {
            project.flush_active(&editor.text);
            // Sync the flushed edit into the render-root so the entry render sees it.
            if let Some(base) = project.base_dir.clone()
                && let Some(cur) = project.files.get(project.active)
            {
                let name = cur.name.clone();
                let _ = write_under(&base, &name, editor.text.as_bytes());
            }
        }
        project.set_active(i);
        // The render target is ALWAYS the entry, read from the render-root (shadow / temp).
        let render_path = project
            .base_dir
            .as_ref()
            .zip(project.files.get(project.entry))
            .map(|(b, f)| b.join(&f.name))
            .unwrap_or_else(|| path.clone());
        // Only a CHANGE of render target re-renders — a view-switch (entry unchanged) just swaps the editor.
        let changed = scene.source.as_deref() != Some(render_path.as_path());
        scene.source = Some(render_path.clone());
        // Re-derive the workspace root from the REAL folder (Z.3.6), NOT the render-root: the shadow lives
        // under `tmp/`, so walking up from it would lose BOSL2/scad-lib. A container has no real workspace
        // (loose_save_dir None) → root stays the boot value (packed lib root).
        if let Some(real_dir) = loose_save_dir(&project)
            && let Some(r) = std::fs::canonicalize(&real_dir)
                .ok()
                .as_deref()
                .and_then(fab::find_root_from)
        {
            scene.root = Some(r);
        }
        // The VIEWED file's disk text (minus its fab:config block, W.3.8) becomes the editor buffer.
        let view_cfg = read_into_editor(&mut editor, &path);
        if changed {
            // The stashed block applies in poll_job once the fresh parts are built. On a fresh open the
            // viewed file IS the entry, so its config is the entry's. (A view-switch leaves parts untouched.)
            pending_config.0 = view_cfg;
            // Drop the outgoing model's held base solids before wiping — a file switch abandons them.
            free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
            state.reset();
            kick_render(&pool, &mut job, &mut status, &scene, true);
            info!("render: {}", render_path.display());
        }
    }
}

/// Drain the native file pick into a [`ProjectDoc`] (Phase Z): a loose `.scad` opens its FOLDER as the
/// project (siblings become files, that file the entry); a `.scadproj` materializes + opens as a project.
/// FileList is the doc's native path projection. On cancel, nothing. The whole body is native — on wasm
/// the picker is hidden (no fs) so the dialog never has a task and every param reads unused.
#[cfg_attr(target_arch = "wasm32", allow(unused_variables, unused_mut))]
pub(crate) fn poll_open_dialog(
    mut dlg: ResMut<OpenDialog>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut switch: MessageWriter<SwitchFile>,
    mut status: ResMut<Status>,
    #[allow(unused_variables)] scene: Res<SceneCfg>,
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
    // A `.scadproj` (Phase Z): materialize it to a scratch dir and open it as a project rooted there,
    // so `Source::Path` + BOSL2 (via `scene.root`) resolve exactly as for a normal on-disk project. The
    // ProjectDoc holds the canonical bytes (its `home` re-zips on save); `base_dir` is the temp render root.
    #[cfg(not(target_arch = "wasm32"))]
    if picked
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("scadproj"))
    {
        match unpack_scadproj(&picked, &scene.tmp) {
            Ok(doc) => {
                let active = doc.entry;
                *project = doc;
                switch.write(SwitchFile(active));
            }
            Err(e) => status.0 = format!("open .scadproj: {e}"),
        }
        return;
    }
    // A loose `.scad` (Z.3.6, option A): open its FOLDER as a project rooted at a temp SHADOW (so preview
    // never touches the real files), homed at the real file so Save writes back in place. The entry
    // renders; other files are views/libs.
    #[cfg(not(target_arch = "wasm32"))]
    match open_loose(&picked, &scene.tmp) {
        Ok(doc) => {
            let active = doc.entry;
            *project = doc;
            switch.write(SwitchFile(active));
        }
        Err(e) => status.0 = format!("open: {e}"),
    }
}

/// Materialize a `.scadproj` under `tmp` and return the [`ProjectDoc`] rooted there — text files editable,
/// binary assets ride-along, `base_dir` the temp render root, `home` the original `.scadproj` (so save
/// re-zips it, Z.3.5). The unpacked copy is a render scratch; the ProjectDoc's bytes are the truth.
#[cfg(not(target_arch = "wasm32"))]
fn unpack_scadproj(
    path: &std::path::Path,
    tmp: &std::path::Path,
) -> anyhow::Result<crate::project::ProjectDoc> {
    use anyhow::Context;
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc = crate::project::ProjectDoc::from_scadproj(
        &bytes,
        crate::project::ProjectHome::ScadProj(path.to_path_buf()),
    )?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let dir = tmp.join("scadproj").join(&stem);
    let _ = std::fs::remove_dir_all(&dir); // a clean re-open
    std::fs::create_dir_all(&dir)?;
    // Write every file (text + binary asset) so `include`/`use` + `import`/`surface` all resolve from
    // the render root. The ProjectDoc keeps the canonical copies; this is the disk mirror the render reads.
    for f in &doc.files {
        write_under(&dir, &f.name, f.text.as_bytes())?;
    }
    for (name, body) in &doc.assets {
        write_under(&dir, name, body)?;
    }
    if doc.files.is_empty() {
        anyhow::bail!("no .scad in {}", path.display());
    }
    doc.base_dir = Some(dir);
    Ok(doc)
}

/// Open a loose `.scad` as a project (Z.3.6, option A): `from_disk` loads the FOLDER's `.scad`, then we
/// re-root to a temp SHADOW so preview writes the shadow, never the user's real files (killing
/// `.fab-preview`). `home` stays `ScadFile(real path)` so Save writes back to the REAL folder. The loose
/// twin of [`unpack_scadproj`] — same shape, the home + save target differ.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn open_loose(
    picked: &std::path::Path,
    tmp: &std::path::Path,
) -> anyhow::Result<crate::project::ProjectDoc> {
    let dir = picked.parent().unwrap_or(picked).to_path_buf();
    let scads = scad_files(&dir);
    if scads.is_empty() {
        anyhow::bail!("no .scad under {}", dir.display());
    }
    // from_disk loads the content + homes at the real entry path; we override base_dir to the shadow.
    let mut doc = crate::project::ProjectDoc::from_disk(dir, &scads, picked);
    let stem = picked
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into());
    let shadow = tmp.join("loose").join(&stem);
    let _ = std::fs::remove_dir_all(&shadow); // a clean re-open
    materialize_all(&doc, &shadow)?;
    doc.base_dir = Some(shadow);
    Ok(doc)
}

/// The REAL on-disk folder a loose project saves back to — its `home` `ScadFile` path's parent (the
/// shadow `base_dir` is a render scratch; save must NOT write there). `None` for a container / web / paste.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn loose_save_dir(project: &crate::project::ProjectDoc) -> Option<std::path::PathBuf> {
    match &project.home {
        crate::project::ProjectHome::ScadFile(p) => {
            Some(p.parent().unwrap_or(p.as_path()).to_path_buf())
        }
        _ => None,
    }
}

/// Write `body` to `base/rel`, creating parent dirs. Shared by the `.scadproj` materialize + save-back.
#[cfg(not(target_arch = "wasm32"))]
fn write_under(base: &std::path::Path, rel: &str, body: &[u8]) -> anyhow::Result<()> {
    let dest = base.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, body)?;
    Ok(())
}

/// Re-zip the whole project to `.scadproj` bytes (Z.3.5): the ENTRY carries the baked `fab:config` block
/// (so a reopen restores the bed), every other file + binary asset goes verbatim from the ProjectDoc's
/// current (flushed) state. The manifest keeps the original entry; the title stays whatever it was.
/// Cross-platform (Z.3.8): `scadproj` write works on wasm too, so the web save/publish can re-zip.
pub(crate) fn rezip_project(
    project: &crate::project::ProjectDoc,
    parts: &[Part],
    printer: config::PrinterCfg,
) -> anyhow::Result<Vec<u8>> {
    use fab_scad::scadproj;
    let entry_name = project.entry_name().to_string();
    // Bake config into the entry ONCE (outside the loop so `printer` is used once, not per file).
    let entry_baked = project
        .files
        .get(project.entry)
        .map(|f| config::with_config_block(&f.text, parts, Some(printer)));
    let mut files: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
    for (i, f) in project.files.iter().enumerate() {
        let bytes = if i == project.entry {
            entry_baked
                .clone()
                .unwrap_or_else(|| f.text.clone())
                .into_bytes()
        } else {
            f.text.clone().into_bytes()
        };
        files.insert(f.name.clone(), bytes);
    }
    for (name, body) in &project.assets {
        files.insert(name.clone(), body.clone());
    }
    let proj = scadproj::project_from_files(files, Some(entry_name), None)?;
    scadproj::write_scadproj(&proj)
}

/// The SOURCE variant to upload for the current document (Z.3.8 save-back / Z.5 publish): a `.scadproj`
/// for a MULTI-FILE project — the site ingests it as `OpenscadProject` via its `.scadproj` probe (Z.4),
/// so the gallery item re-opens as a real project — else a config-baked `.scad`. Returns
/// `(filename, mime, bytes)`; `stem` names the file, `entry_text` is the live single-file source. The web
/// upload paths carry bytes; native publish rezips to a temp file for `upload_model`'s path API instead.
#[cfg(target_arch = "wasm32")]
pub(crate) fn project_source_variant(
    project: &crate::project::ProjectDoc,
    parts: &[Part],
    printer: config::PrinterCfg,
    entry_text: &str,
    stem: &str,
) -> anyhow::Result<(String, &'static str, Vec<u8>)> {
    use fab_scad::scadproj::{PROJECT_EXT, PROJECT_MIME};
    if project.is_multifile() {
        let bytes = rezip_project(project, parts, printer)?;
        Ok((format!("{stem}.{PROJECT_EXT}"), PROJECT_MIME, bytes))
    } else {
        let baked = config::with_config_block(entry_text, parts, Some(printer));
        Ok((format!("{stem}.scad"), "application/x-openscad", baked.into_bytes()))
    }
}

/// Materialize every project file + asset under `base` — the disk mirror the native render reads
/// (`Source::Path`). Shared by add-file / new-file so a change reaches the render root.
#[cfg(not(target_arch = "wasm32"))]
fn materialize_all(
    project: &crate::project::ProjectDoc,
    base: &std::path::Path,
) -> anyhow::Result<()> {
    for f in &project.files {
        write_under(base, &f.name, f.text.as_bytes())?;
    }
    for (name, body) in &project.assets {
        write_under(base, name, body)?;
    }
    Ok(())
}

/// Project-tab file management (Z.3.3): set the render entry, and add / new / delete files. Each
/// structural change re-derives FileList (the ProjectDoc's native path projection, so switch-indices
/// stay aligned) and materializes to the render root. Native only — the ops touch the fs + the rfd
/// picker; web file management rides Z.3.4's render_pack. Runs alongside apply_switch_file; a SwitchFile
/// it emits lands within a frame or two (messages are double-buffered).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_files_action(
    mut ev: MessageReader<PanelCmd>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut add_dialog: ResMut<AddFileDialog>,
    mut rename: ResMut<crate::state::RenameUi>,
    mut editor: ResMut<EditorBuf>,
    mut switch: MessageWriter<SwitchFile>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    pool: Res<GeomPool>,
    mut scene: ResMut<SceneCfg>,
    mut state: ModelState,
) {
    // Inline rename (Z.3.3): apply the committed (row, new-name) — rename in the ProjectDoc, MOVE the
    // on-disk/temp copy, then re-render (the render target's name may have changed, and a rename can
    // break a sibling's `include`, which the re-render surfaces).
    if let Some((i, new_name)) = rename.commit.take() {
        let base = project.base_dir.clone();
        if let Some(old) = project.rename_file(i, &new_name) {
            let new = project.files.get(i).map(|f| f.name.clone()).unwrap_or(new_name);
            if let Some(base) = base.as_ref() {
                let _ = std::fs::rename(base.join(&old), base.join(&new)); // missing source? a never-materialized file
                // Recompute the render target with the CURRENT names + re-render.
                let target = if matches!(project.home, crate::project::ProjectHome::ScadProj(_)) {
                    project.entry
                } else {
                    project.active
                };
                if let Some(f) = project.files.get(target) {
                    scene.source = Some(base.join(&f.name));
                }
                // The active file's path changed if IT was renamed — keep the editor pointed at it.
                if i == project.active
                    && let Some(f) = project.files.get(i)
                {
                    editor.path = base.join(&f.name);
                }
                free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
                state.reset();
                kick_render(&pool, &mut job, &mut status, &scene, true);
            }
        }
    }
    // Snapshot the frame's commands (PanelCmd is Copy) so the reader borrow ends before we mutate.
    for cmd in ev.read().copied().collect::<Vec<_>>() {
        match cmd {
            PanelCmd::SetEntry(i) => {
                project.set_entry(i);
                // Make it active too + let apply_switch_file render it (the target changed → it will).
                switch.write(SwitchFile(i));
            }
            PanelCmd::NewFile => {
                let idx = project.add_file("untitled.scad", String::new());
                if let Some(base) = project.base_dir.clone() {
                    let _ = materialize_all(&project, &base);
                }
                switch.write(SwitchFile(idx)); // view the new file (entry unchanged → no re-render)
            }
            PanelCmd::DeleteFile(i) => {
                let container = matches!(project.home, crate::project::ProjectHome::ScadProj(_));
                let base = project.base_dir.clone();
                let Some(name) = project.remove_file(i) else {
                    status.0 = "can't delete the project's only file".into();
                    continue;
                };
                if container {
                    // A container OWNS its files (temp scratch): drop the deleted copy so the entry
                    // re-renders WITHOUT it, and re-hydrate the editor from whatever's active now.
                    if let Some(base) = base.as_ref() {
                        let _ = std::fs::remove_file(base.join(&name));
                        if let Some(p) = project.native_paths().get(project.active).cloned() {
                            let _ = read_into_editor(&mut editor, &p); // config stays the entry's
                        }
                    }
                    free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
                    state.reset();
                    kick_render(&pool, &mut job, &mut status, &scene, true);
                } else {
                    // A loose folder's files are the user's REAL files — never rm behind their back;
                    // delete is a session-view removal. Re-view the active file (re-renders if it moved).
                    switch.write(SwitchFile(project.active));
                }
            }
            PanelCmd::AddFiles if add_dialog.0.is_none() => {
                add_dialog.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
                    rfd::AsyncFileDialog::new()
                        .pick_files()
                        .await
                        .map(|hs| hs.into_iter().map(|h| h.path().to_path_buf()).collect())
                }));
            }
            _ => {}
        }
    }
}

/// Drain the "Add files" multi-pick (Z.3.3): read each picked file's bytes into the project (text →
/// editable, binary → asset, names de-duplicated) + materialize to the render root. Adding files doesn't
/// re-render — a new file isn't `include`d by the entry until you say so.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn poll_add_dialog(
    mut dlg: ResMut<AddFileDialog>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut status: ResMut<Status>,
) {
    let Some(task) = dlg.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return; // picker still open
    };
    dlg.0 = None;
    let Some(paths) = result else {
        return; // cancelled
    };
    let mut added = 0;
    for p in paths {
        match std::fs::read(&p) {
            Ok(bytes) => {
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into());
                project.import(&name, bytes);
                added += 1;
            }
            Err(e) => status.0 = format!("add {}: {e}", p.display()),
        }
    }
    if added > 0 {
        if let Some(base) = project.base_dir.clone() {
            let _ = materialize_all(&project, &base);
        }
        status.0 = format!("added {added} file(s) to the project");
    }
}

/// Save-As `.scadproj` for a project with no container home yet (Z.3.7 — a loose `.scad` that grew a
/// second file): build the zip bytes NOW (the live active edit flushed in), then pop a native Save dialog
/// + write off-thread (rfd can't block Bevy's loop). [`poll_save_project`] adopts the chosen path as home.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_as_project_action(
    mut ev: MessageReader<PanelCmd>,
    mut project: ResMut<crate::project::ProjectDoc>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    editor: Res<EditorBuf>,
    mut save_job: ResMut<crate::state::SaveProjJob>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::SaveAsProject) {
        return;
    }
    if save_job.0.is_some() {
        status.0 = "already saving…".into();
        return;
    }
    project.flush_active(&editor.text); // capture the live active edit in the file set
    let printer = config::PrinterCfg {
        bed: [scene.bed[0] as f64, scene.bed[1] as f64, scene.bed[2] as f64],
    };
    let bytes = match rezip_project(&project, &parts.0, printer) {
        Ok(b) => b,
        Err(e) => {
            status.0 = format!("save failed: {e:#}");
            return;
        }
    };
    let default_name = format!(
        "{}.scadproj",
        std::path::Path::new(project.entry_name())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
    );
    let dir = scene
        .source
        .as_ref()
        .and_then(|s| s.parent())
        .map(std::path::Path::to_path_buf);
    save_job.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
        let mut dlg = rfd::AsyncFileDialog::new()
            .add_filter("OpenSCAD project", &["scadproj"])
            .set_file_name(&default_name);
        if let Some(d) = dir {
            dlg = dlg.set_directory(d);
        }
        let Some(handle) = dlg.save_file().await else {
            return Err("save cancelled".to_string());
        };
        let path = handle.path().to_path_buf();
        std::fs::write(&path, &bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(path)
    }));
    status.0 = "save as .scadproj: choose where…".into();
}

/// Land the Save-As `.scadproj` (Z.3.7): adopt the chosen path as the ScadProj home AND re-root the render
/// to a fresh temp materialization — so from here on the promoted project behaves exactly like an opened
/// `.scadproj` (preview writes the temp, never the user's real loose files) and Save re-zips in place.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn poll_save_project(
    mut save_job: ResMut<crate::state::SaveProjJob>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut editor: ResMut<EditorBuf>,
    mut scene: ResMut<SceneCfg>,
    mut status: ResMut<Status>,
) {
    let Some(task) = save_job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    save_job.0 = None;
    let path = match result {
        Ok(p) => p,
        Err(e) => {
            status.0 = e;
            return;
        }
    };
    status.0 = format!("saved -> {}", path.display());
    info!("{}", status.0);
    project.home = crate::project::ProjectHome::ScadProj(path.clone());
    // Re-root to a temp materialization (like an opened .scadproj), so container-preview writes the temp,
    // not the user's real loose files. scene.source + the editor path follow the new root.
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let dir = scene.tmp.join("scadproj").join(&stem);
    let _ = std::fs::remove_dir_all(&dir);
    if materialize_all(&project, &dir).is_ok() {
        project.base_dir = Some(dir.clone());
        if let Some(f) = project.files.get(project.entry) {
            scene.source = Some(dir.join(&f.name));
        }
        if let Some(f) = project.files.get(project.active) {
            editor.path = dir.join(&f.name);
        }
    }
    editor.dirty = false;
    for f in &mut project.files {
        f.dirty = false;
    }
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

/// Fire-and-forget: tell the geometry service to drop these held base solids (W.3.3). Detached so a
/// frame never blocks on the reply — freeing is a cheap map removal, and a missed free only costs a
/// bounded-store eviction (an op on an evicted handle self-heals as a re-render). No-op when empty.
fn free_bases(pool: &GeomPool, ids: Vec<SolidId>) {
    if ids.is_empty() {
        return;
    }
    let pool = pool.clone();
    AsyncComputeTaskPool::get()
        .spawn(async move {
            let _ = pool.call(Request::Free { ids }).await;
        })
        .detach();
}

/// Spawn a WHOLE render of every top-level part through the geometry service (T.2b, W.3.3) — the
/// service splits the model into its implicit-union children and MINTS a base handle per part. `fresh`
/// distinguishes a new source (replace the parts list) from a reload of the same one (refresh geometry,
/// keep edits).
pub(crate) fn kick_render(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    fresh: bool,
) {
    let Some(src) = cfg.source.clone() else {
        status.0 = "no .scad source".into();
        return;
    };
    kick_render_from(pool, job, status, cfg, &src, fresh);
}

/// Whole-render an EXPLICIT source path (U.3.2) — `cfg` still supplies root/tmp, but the content +
/// include base come from `src`, not `cfg.source`. The preview renders a project's render-root entry
/// this way without repointing `cfg.source` at every keystroke.
pub(crate) fn kick_render_from(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    src: &Path,
    fresh: bool,
) {
    let source = Source::Path(src.to_string_lossy().into_owned());
    let root = cfg.root.as_ref().map(|r| r.to_string_lossy().into_owned());
    spawn_render(pool, job, status, source, root, fresh);
}

/// Whole-render from in-memory source BYTES (W.3.6, wasm) — the browser has no fs, so the render
/// source is the editor buffer, sent as `Source::Bytes`. First gathers the model's include CLOSURE
/// from the packed lib tree (fetched once) so `use`/`include` (BOSL2, scad-lib) resolve on the worker.
#[cfg(target_arch = "wasm32")]
pub(crate) fn kick_render_bytes(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    main: Vec<u8>,
    pack: Vec<(String, Vec<u8>)>,
    fresh: bool,
) {
    let pool = pool.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let main_str = String::from_utf8_lossy(&main).into_owned();
        // Z.3.4: the worker libs are `main`'s closure PLUS the project pack (each file + its own lib
        // closure + binary assets). For a single-file project `pack` is empty → identical to before.
        let libs = crate::lib_fetch::project_libs(&main_str, pack).await;
        render_result(
            pool.call(Request::RenderParts {
                source: Source::Bytes { main, libs },
                root: None,
                quality: crate::render_quality::current(),
            })
            .await,
            fresh,
        )
    });
    job.0 = Some(task);
    status.0 = "rendering".into();
}

/// The `?model=` fetch in flight (W.3.12): the .scad text arriving from the page URL's `model`
/// parameter, plus the basename it gives the editor path (so the tab + Save-download carry the real
/// model name). Spawned by `setup_windowed`, landed by [`poll_model_fetch`].
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct ModelFetch {
    pub task: Option<bevy::tasks::Task<Option<Vec<u8>>>>,
    pub name: String,
}

/// Land the `?model=` fetch (W.3.12): on arrival the text seeds the editor exactly like a native file
/// open — `fab:config` block parsed into [`PendingConfig`] + stripped from the buffer (the W.3.8
/// codec, string-level) — and the armed debounce renders it through the geom Worker. A failed fetch
/// reports and falls back to the demo, so the app never boots to a dead editor.
#[cfg(target_arch = "wasm32")]
pub(crate) fn poll_model_fetch(
    mut fetch: ResMut<ModelFetch>,
    mut editor: ResMut<EditorBuf>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut pending_config: ResMut<PendingConfig>,
    mut status: ResMut<Status>,
) {
    let Some(task) = fetch.task.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return; // still fetching
    };
    fetch.task = None;
    let name = fetch.name.clone();
    match result {
        // A `.scadproj` deep-link (Z.3.4): the bytes are a zip (PK magic) → open it as a multi-file
        // project. The entry file seeds the editor (config block stripped + stashed); render_pack sends
        // the whole project to the worker. A bad zip falls through to the text path.
        Some(bytes) if bytes.starts_with(b"PK\x03\x04") => {
            match crate::project::ProjectDoc::from_scadproj(
                &bytes,
                crate::project::ProjectHome::WebModel(name.clone()),
            ) {
                Ok(mut doc) => {
                    let entry_raw = doc
                        .files
                        .get(doc.entry)
                        .map(|f| f.text.clone())
                        .unwrap_or_default();
                    pending_config.0 = config::read_config_block(&entry_raw);
                    let stripped = config::strip_config_block(&entry_raw);
                    editor.text = stripped.clone();
                    editor.path = std::path::PathBuf::from(doc.entry_name());
                    // Keep the ProjectDoc's entry text in step with the editor (both stripped).
                    if let Some(f) = doc.files.get_mut(doc.entry) {
                        f.text = stripped;
                    }
                    status.0 = format!("loaded project {name}");
                    *project = doc;
                }
                Err(e) => {
                    editor.text = crate::scene::WEB_DEMO.to_string();
                    status.0 = format!("bad .scadproj ({e:#}) — rendering the demo");
                    *project = crate::project::ProjectDoc::single(
                        "demo.scad",
                        crate::scene::WEB_DEMO,
                        crate::project::ProjectHome::Fresh,
                    );
                }
            }
        }
        Some(bytes) => {
            let raw = String::from_utf8_lossy(&bytes).into_owned();
            pending_config.0 = config::read_config_block(&raw);
            editor.text = config::strip_config_block(&raw);
            editor.path = std::path::PathBuf::from(&name);
            status.0 = format!("loaded {name}");
            // A plain `.scad` deep-link is a one-file project; WebModel carries the download/save name.
            *project = crate::project::ProjectDoc::single(
                name.clone(),
                editor.text.clone(),
                crate::project::ProjectHome::WebModel(name),
            );
        }
        None => {
            editor.text = crate::scene::WEB_DEMO.to_string();
            status.0 = "model fetch failed (URL reachable? CORS/CORP?) — rendering the demo".into();
            *project = crate::project::ProjectDoc::single(
                "demo.scad",
                crate::scene::WEB_DEMO,
                crate::project::ProjectHome::Fresh,
            );
        }
    }
    editor.dirty = false;
    editor.edited_at = Some(0.0); // arm the debounced preview — the render kick
}

/// The shared render task-spawn (T.2b, W.3.3): fire `RenderParts` at the service and bank the minted
/// handles + display STL bytes + bboxes as a [`JobResult::Rendered`]. The service builds + HOLDS each
/// part Solid (!Send stays on its shard); only bytes cross back. Source is `Path` (native fs) or
/// `Bytes` (wasm) — the one call the two front doors share.
fn spawn_render(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    source: Source,
    root: Option<String>,
    fresh: bool,
) {
    let pool = pool.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        render_result(
            pool.call(Request::RenderParts {
                source,
                root,
                quality: crate::render_quality::current(),
            })
            .await,
            fresh,
        )
    });
    job.0 = Some(task);
    status.0 = "rendering".into();
}

/// Map a `RenderParts` service reply to a [`JobResult`] (or an error string) — shared by the native
/// (Path) and wasm (Bytes) render front doors.
fn render_result(resp: anyhow::Result<Response>, fresh: bool) -> Result<JobResult, String> {
    match resp {
        Ok(Response::PartsRendered { parts, messages }) => {
            // W.3.16: the model's echo/warnings land in the in-app console (the only place to see them
            // on web). This is the shared render consume point, so both platforms get them.
            crate::console::push_scad_messages(&messages);
            Ok(JobResult::Rendered {
                fresh,
                parts: parts
                    .into_iter()
                    .map(|w| RenderedPart {
                        base: w.id,
                        stl: w.stl,
                        min: w.min,
                        max: w.max,
                        name: w.name,
                    })
                    .collect(),
            })
        }
        Ok(Response::Failed { error, line }) => {
            // W.3.37: prefix the failing USER line when the eval error mapped to one, so the console AND the
            // status bar (both surface this Err string) point the user straight at it.
            let msg = match line {
                Some(l) => format!("line {l}: {error}"),
                None => error,
            };
            crate::console::push(crate::console::Kind::Scad, format!("render error: {msg}"));
            Err(msg)
        }
        Ok(_) => Err("render: unexpected service response".to_string()),
        Err(e) => {
            let msg = format!("{e:#}");
            crate::console::push(crate::console::Kind::Scad, format!("render error: {msg}"));
            Err(msg)
        }
    }
}

/// Debounced live preview (U.3.2): once the editor buffer has sat un-touched for `EDIT_DEBOUNCE` and
/// no render is in flight, write it to a hidden temp beside the real file (so relative `include`s
/// resolve) and whole-render it (`fresh = false` — keep each part's cuts/connectors). The buffer,
/// not the disk file, is the truth; the disk file only changes on an explicit Save.
pub(crate) fn preview_edited_buffer(
    mut editor: ResMut<EditorBuf>,
    scene: Res<SceneCfg>,
    project: Res<crate::project::ProjectDoc>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    time: Res<Time>,
    pool: Res<GeomPool>,
) {
    let Some(t) = editor.edited_at else {
        return;
    };
    if job.0.is_some() || time.elapsed_secs_f64() - t < EDIT_DEBOUNCE {
        return; // still typing, or a render's already running — retry next idle frame
    }
    editor.edited_at = None;
    // wasm: no fs — the PROJECT is the source (Z.3.4). render_pack gives the ENTRY bytes (with the live
    // active text spliced) + the pack (other files + assets), sent as Source::Bytes to the geom Worker.
    // A single-file project → (editor.text, []) → identical to the pre-project web render.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = &scene;
        let (main, pack) = project.render_pack(&editor.text);
        kick_render_bytes(&pool, &mut job, &mut status, main, pack, false);
    }
    // native, ANY project with a render-root (Z.3.6): write the edited file into the root (a container's
    // temp OR a loose folder's shadow), then render the ENTRY — editing ANY project file re-renders the
    // entry with the edit. The root is the render truth (never the user's real files); save persists it.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(base) = project.base_dir.clone() {
        if let Some(active) = project.files.get(project.active) {
            let name = active.name.clone();
            if write_under(&base, &name, editor.text.as_bytes()).is_err() {
                status.0 = "preview write failed".into();
                return;
            }
        }
        if let Some(entry) = project.files.get(project.entry) {
            let entry_path = base.join(&entry.name);
            kick_render_from(&pool, &mut job, &mut status, &scene, &entry_path, false);
        }
        return;
    }
    // native, NO render-root (a fresh launch the user PASTED into — W.3.33, base_dir None): write the
    // buffer to a hidden temp in the scratch dir and render that. `<BOSL2/…>` still resolves via the
    // packed lib root on `scene.root`; a pasted standalone model has no siblings to miss.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = editor
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| scene.tmp.clone());
        let stem = editor
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "preview".into());
        if std::fs::create_dir_all(&dir).is_err() {
            status.0 = "preview dir failed".into();
            return;
        }
        let preview = dir.join(format!(".fab-preview-{stem}.scad"));
        if std::fs::write(&preview, editor.text.as_bytes()).is_err() {
            status.0 = "preview write failed".into();
            return;
        }
        kick_render_from(&pool, &mut job, &mut status, &scene, &preview, false);
    }
}

/// Spawn a per-part reslice off part `part`'s HELD base handle through the geometry service (T.2b,
/// W.3.3). Only that part's geometry is touched; its `Model` entity (tagged `PartId(part)`) is the
/// one `poll_job` swaps when the sliced STL bytes land. The Solid never leaves the service (!Send).
#[allow(clippy::too_many_arguments)]
pub(crate) fn kick_reslice(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    part: usize,
    base: SolidId,
    cuts: Vec<(char, f64)>,
    conns: Vec<fab::Conn>,
    orient: Vec<fab::Orient3>,
) {
    let pool = pool.clone();
    let connectors = fab::to_wire_conns(&conns);
    let orient = fab::to_wire_orient(&orient);
    let task = AsyncComputeTaskPool::get().spawn(async move {
        match pool
            .call(Request::Reslice {
                base,
                cuts,
                connectors,
                orient,
                spread: SPREAD,
            })
            .await
        {
            Ok(Response::Resliced { stl }) => Ok(JobResult::Resliced { part, stl }),
            Ok(Response::Failed { error, .. }) => Err(error),
            Ok(_) => Err("reslice: unexpected service response".to_string()),
            Err(e) => Err(format!("{e:#}")),
        }
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
    mut pending_config: ResMut<PendingConfig>,
    mut pipeline: ResMut<Pipeline>,
    mut scene: ResMut<SceneCfg>,
    editor: Res<EditorBuf>,
    bg: Res<SliceInBackground>,
    pool: Res<GeomPool>,
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
            parts: rendered,
        }) => {
            // A structural change (the model's part COUNT moved) forces a full rebuild even on a
            // reload — the old Part↔entity mapping no longer holds.
            if fresh || rendered.len() != parts.0.len() {
                // The outgoing parts' held base solids are abandoned — free them on the service.
                free_bases(&pool, parts.0.iter().filter_map(|p| p.base).collect());
                for (e, _) in &models {
                    commands.entity(e).despawn();
                }
                let mut new: Vec<Part> = rendered
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        build_part(
                            &mut commands,
                            &mut meshes,
                            &mut materials,
                            i,
                            r.base,
                            &r.stl,
                            r.min,
                            r.max,
                            r.name.clone(),
                        )
                    })
                    .collect();
                // Apply the source's fab:config block BEFORE `new` goes live (a part whose block set
                // cuts makes `kick_auto_plan` stand down, so config wins over auto-derive, W.3.8). The
                // block was stashed at load (both platforms); no block → every part auto-derives. The
                // legacy project.toml load is GONE — the .scad block is the one config mechanism.
                if let Some(cfg) = pending_config.0.take() {
                    config::apply_blocks(&mut new, &cfg.parts);
                    // The model's own printer (if it declared one) overrides the boot bed — the web
                    // has no printers.toml, so the .scad's fab:config IS the bed AND plate source there
                    // (W.3.8), which is why this slaves the export plate to the bed (set_configured_bed).
                    if let Some(p) = cfg.printer {
                        scene.set_configured_bed([
                            p.bed[0] as f32,
                            p.bed[1] as f32,
                            p.bed[2] as f32,
                        ]);
                    }
                }
                *parts = Parts(new);
                active_part.0 = 0;
            } else {
                // Reload of the SAME source: the render minted fresh handles for every part — free the
                // old ones, then refresh each part's geometry in place, KEEPING its cuts/connectors.
                free_bases(&pool, parts.0.iter().filter_map(|p| p.base).collect());
                for (i, r) in rendered.iter().enumerate() {
                    refresh_part(
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &models,
                        &mut parts.0[i],
                        i,
                        r.base,
                        &r.stl,
                        r.min,
                        r.max,
                        r.name.clone(),
                    );
                }
            }
            status.0 = "ready".into();
            // A distinct, greppable signal that geometry rendered end-to-end (on wasm this rode the geom
            // Worker round-trip) — the release boot gate waits for it to prove the bundle isn't
            // dead-on-arrival (release-web.yml). Bevy's LogPlugin routes it to the browser console.
            info!("fab-gui render complete: {} part(s)", parts.0.len());
            // The displayed geometry now matches the current source — clear the Model/Parts stale flag
            // (U.3.7). `sync_pipeline` compares the live source hash against this each frame.
            pipeline.geo_of = Some(hash_one(&editor.text));
        }
        Ok(JobResult::Resliced { part, stl }) => {
            let mesh = mesh_from_bytes(&mut meshes, &stl);
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
                    // The solid Model must be TRANSPARENT to picking: a no-Pickable mesh blocks the
                    // ray, and the cut planes sit INSIDE it — so a drag would land on the Model, never
                    // the plane. Nothing picks the Model (drag/click → CutPlaneViz, orient → PrintPiece).
                    Pickable::IGNORE,
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

/// A wire `[f64; 3]` bbox corner → Bevy `Vec3`.
fn vec3_of(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// Build a fresh [`Part`] for top-level part `i` from its rendered STL bytes + minted base handle —
/// spawn its `Model` entity (tagged `PartId(i)`) and fix its bounds off the wire bbox.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_part(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    i: usize,
    base: SolidId,
    stl: &[u8],
    min: [f64; 3],
    max: [f64; 3],
    name: Option<String>,
) -> Part {
    let mesh = mesh_from_bytes(meshes, stl);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials)),
        Model,
        PartId(i),
        Pickable::IGNORE, // pick-transparent so a cut-plane drag reaches the plane behind the solid
    ));
    let mut part = Part {
        base: Some(base),
        whole: Some(mesh),
        name,
        ..default()
    };
    // Bounds come straight off the wire bbox (the service guarantees it). No seed cut: kick_auto_plan
    // derives overflowing parts (fit-to-bed + connectors) and leaves fitting parts WHOLE (U.3.15).
    part.bounds.0 = Some((vec3_of(min), vec3_of(max)));
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
    base: SolidId,
    stl: &[u8],
    min: [f64; 3],
    max: [f64; 3],
    name: Option<String>,
) {
    let mesh = mesh_from_bytes(meshes, stl);
    despawn_part_models(commands, models, i);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials)),
        Model,
        PartId(i),
        Pickable::IGNORE, // pick-transparent so a cut-plane drag reaches the plane behind the solid
    ));
    part.base = Some(base);
    part.whole = Some(mesh);
    part.name = name;
    part.spread = 0.0; // reload drops back to the intact model
    part.sliced = None;
    part.sliced_hash = None; // force a reslice off the new geometry if the part has cuts
    refresh_bounds_on_reload(part, min, max);
}

/// On a RELOAD, reconcile a part's bbox + auto-plan state with the freshly-rendered geometry.
///
/// A part carrying USER CUTS keeps its FROZEN bbox and plan: the cut coordinates are absolute in that
/// frame, so re-seating the bbox would desync the cut planes from the geometry the slicer re-renders,
/// and the user owns those cuts — we don't silently re-derive them. But a CUTLESS part (whole, or
/// PRESLICED into disjoint components) hasn't been sliced, so an edit is free to RESIZE it: take the
/// fresh bbox, re-arm `kick_auto_plan`, and drop the stale component count. Without this the bbox froze
/// at the FIRST render, so removing a part's pre-slices (or any resize) left the bed-overflow check —
/// and "Reset to auto" after it — judging the NEW solid by the OLD size, and it would refuse to slice.
pub(crate) fn refresh_bounds_on_reload(part: &mut Part, min: [f64; 3], max: [f64; 3]) {
    if part.cuts.list.is_empty() {
        part.bounds.0 = Some((vec3_of(min), vec3_of(max)));
        part.auto_planned.0 = None; // re-run the overflow pre-check against the fresh geometry
        part.pieces = 0; // stale presliced count — the re-plan restamps it (a fitting part reads whole)
    } else if part.bounds.0.is_none() {
        part.bounds.0 = Some((vec3_of(min), vec3_of(max)));
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

// W.3.28: the desktop Publish flow moved to `publish_native` — it renders the model AND the cover through
// fab's OWN kernel/renderer (no external OpenSCAD) as a phased state machine. The old OpenSCAD-shelling
// publish_action + poll_publish (+ PublishJob) are gone.

/// The in-flight save-back job (W.5.8) — render full-res, export the two mesh variants, upload all
/// three files. Yields the endpoint's response body or an error. Web only (web-sys FormData + fetch).
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct SaveJob(pub(crate) Option<Task<Result<String, String>>>);

/// The headless-Chrome save-round-trip hook (W.5.9): `?e2e=save` on the page URL makes the app auto-fire
/// the Save ONCE the model has loaded + rendered, so the console-grep boot gate can drive the whole
/// save pipeline in a browser WITHOUT a DOM/canvas click (egui buttons are canvas pixels, not DOM — and
/// egui exposes no accessibility node on wasm, so a11y-driven clicking isn't available either). Default
/// `false` on every real load. See `packaging/web/e2e-save.sh` + `docs/web-save-back.md`.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct E2eSave(pub(crate) bool);

/// Save the edited model back to hotchkiss.io (W.5.8): bake the config block into the source (exactly
/// the download path), then off-thread — full-res render (mints a handle) -> SaveMeshes off it (colored
/// -> both 3MF, else both STL) -> Free the handle -> multipart PUT {source, low, high} to the item's
/// variant collection under the ambient session cookie (a COMPLETE replace). All three are the SAME
/// format (the roundtrip rule). The button is gated on a derived save target, so this only fires when
/// the deep-link named an item to update in place.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn save_action(
    mut ev: MessageReader<PanelCmd>,
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    pieces: Res<crate::print::PrintPieces>,
    scene: Res<SceneCfg>,
    save_target: Res<SaveTarget>,
    project: Res<crate::project::ProjectDoc>,
    pool: Res<GeomPool>,
    mut job: ResMut<SaveJob>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::SaveToSite) {
        return;
    }
    if job.0.is_some() {
        status.0 = "already saving…".into();
        return;
    }
    let Some(url) = save_target.0.clone() else {
        status.0 = "this model isn't a saveable hotchkiss.io item".into();
        return;
    };
    let printer = config::PrinterCfg {
        bed: [
            scene.bed[0] as f64,
            scene.bed[1] as f64,
            scene.bed[2] as f64,
        ],
    };
    let name = editor
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("model.scad");
    let stem = name
        .strip_suffix(".scad")
        .or_else(|| name.strip_suffix(".scadproj"))
        .unwrap_or(name)
        .to_string();
    // Z.3.8: the SOURCE variant is the whole `.scadproj` for a project (lifts the destructive-save guard —
    // PUT /variants replaces the set, and now the set carries the archive), else the config-baked `.scad`.
    let (src_name, src_mime, src_bytes) =
        match project_source_variant(&project, &parts.0, printer, &editor.text, &stem) {
            Ok(v) => v,
            Err(e) => {
                status.0 = format!("save failed: {e:#}");
                return;
            }
        };
    // The mesh renders from the FULL project (entry + its files), so a project's meshes match its geometry.
    let (render_main, render_pack) = project.render_pack(&editor.text);
    // W.3.18: also push the printable Bambu plate when a plan has been staged (pieces only exist once
    // sliced/oriented on the Export tab). Built here — a quick in-memory zip, same as the Export button —
    // then moved into the upload; best-effort, a `None` (no pieces or a pack error) still saves the rest.
    let plate = crate::print::plate_3mf_bytes(&pieces, &parts, &scene);
    let plate_name = format!("{stem}-plates.3mf");
    // `url` is the `PUT /media/<ref>/variants` target, derived from `?model=` at boot (SaveTarget).
    let pool = pool.clone();

    let task = AsyncComputeTaskPool::get().spawn(async move {
        let main_str = String::from_utf8_lossy(&render_main).into_owned();
        let libs = crate::lib_fetch::project_libs(&main_str, render_pack).await;

        // 1. Full-res render → held handle.
        let id = match pool
            .call(Request::RenderWhole {
                source: Source::Bytes {
                    main: render_main,
                    libs,
                },
                root: None,
                preview: false,
                quality: Quality::Final,
            })
            .await
        {
            Ok(Response::Rendered { id, .. }) => id,
            Ok(Response::Failed { error, .. }) => return Err(format!("render failed: {error}")),
            Ok(_) => return Err("render: unexpected service response".into()),
            Err(e) => return Err(format!("render transport: {e}")),
        };

        // 2. Produce the two mesh variants off the handle; 3. drop the handle regardless.
        let meshes = pool
            .call(Request::SaveMeshes {
                base: id,
                budget: None,
            })
            .await;
        let _ = pool.call(Request::Free { ids: vec![id] }).await;

        let (low, high, ext) = match meshes {
            Ok(Response::SavedMeshes { low, high, ext }) => (low, high, ext),
            Ok(Response::Failed { error, .. }) => {
                return Err(format!("mesh export failed: {error}"));
            }
            Ok(_) => return Err("save-meshes: unexpected service response".into()),
            Err(e) => return Err(format!("save-meshes transport: {e}")),
        };

        // 4. Upload all three — same format for low+high, cookie-authenticated. The source variant is the
        // `.scadproj`/`.scad` decided up front (Z.3.8); its filename extension tells the server the kind.
        let mesh_mime = if ext == "3mf" {
            "model/3mf"
        } else {
            "model/stl"
        };
        let low_name = format!("{stem}_low.{ext}");
        let high_name = format!("{stem}.{ext}");
        let mut files: Vec<(&str, &str, &str, &[u8])> = vec![
            ("source", src_name.as_str(), src_mime, src_bytes.as_slice()),
            ("low", low_name.as_str(), mesh_mime, low.as_slice()),
            ("high", high_name.as_str(), mesh_mime, high.as_slice()),
        ];
        // The printable plate rides along when a plan was staged (W.3.18).
        if let Some(ref pb) = plate {
            files.push(("plate", plate_name.as_str(), "model/3mf", pb.as_slice()));
        }
        crate::web_host::upload_multipart(&url, &files).await
    });
    job.0 = Some(task);
    status.0 = "saving to hotchkiss.io…".into();
}

/// The `?e2e=save` hook (W.5.9): once the deep-linked model has loaded + rendered through the geom
/// worker (a part holds a base handle — the same condition under which the real Save button is
/// clickable), fire `PanelCmd::SaveToSite` EXACTLY ONCE. Drives the whole save pipeline in headless
/// Chrome with no DOM/canvas click; the sentinel + `poll_save`'s outcome log are what the boot gate
/// greps. Inert unless `?e2e=save` was on the page URL.
#[cfg(target_arch = "wasm32")]
pub(crate) fn e2e_autosave(
    e2e: Res<E2eSave>,
    save_target: Res<SaveTarget>,
    parts: Res<Parts>,
    mut fired: Local<bool>,
    mut cmd: MessageWriter<PanelCmd>,
) {
    if *fired || !e2e.0 {
        return;
    }
    // The enable-gate (`?model=` named an item) AND a completed worker round-trip (a part has a base
    // handle) — exactly what gates the real button, so the hook can't fire before Save would be live.
    if save_target.0.is_none() || !parts.0.iter().any(|p| p.base.is_some()) {
        return;
    }
    *fired = true;
    cmd.write(PanelCmd::SaveToSite);
    info!("fab-gui e2e: save dispatched");
}

/// Land the save-back job: report success or the error (per gui-reactive-standard — the status bar is
/// the feedback surface, no modal). Both outcomes LOG (not just set status) so the W.5.9 headless boot
/// gate can grep a save success/failure off the console.
#[cfg(target_arch = "wasm32")]
pub(crate) fn poll_save(mut job: ResMut<SaveJob>, mut status: ResMut<Status>) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(_) => {
            status.0 = "saved to hotchkiss.io".into();
            info!("{}", status.0);
        }
        Err(e) => {
            status.0 = format!("save failed: {e}");
            error!("{}", status.0);
        }
    }
}

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
/// derived until you clicked into them.) The bed-overflow pre-check uses `auto_slice` (kernel), so
/// this is native; on wasm auto-plan lands with the W.3.6 Worker (empty scene until then).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn kick_auto_plan(
    mut parts: ResMut<Parts>,
    scene: Res<SceneCfg>,
    mut job: ResMut<AutoJob>,
    mut status: ResMut<Status>,
    pool: Res<GeomPool>,
) {
    if job.0.is_some() {
        return; // one already in flight
    }
    let Some(src) = scene.source.clone() else {
        return;
    };
    // Pieces fit the USABLE bed — SceneCfg is the single bed source of truth (boot printers.toml, or
    // the model's fab:config, or the Parts-tab override), NOT a fresh printers.toml read (web has none).
    let bed = [
        scene.bed[0] as f64,
        scene.bed[1] as f64,
        scene.bed[2] as f64,
    ];
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
        let Some(base) = part.base else {
            continue; // this part's base not rendered yet (no held handle) — try next frame
        };
        part.auto_planned.0 = Some(src.clone()); // fire once per source
        let pool = pool.clone();
        let task = AsyncComputeTaskPool::get().spawn(async move {
            // The base Solid stays held on its shard; only the plain-data plan crosses back.
            match pool
                .call(Request::AutoPlan {
                    base,
                    min: lo,
                    max: hi,
                    bed,
                })
                .await
            {
                Ok(Response::Planned {
                    cuts,
                    connectors,
                    pieces,
                }) => Ok((cuts, connectors, pieces)),
                Ok(Response::Failed { error, .. }) => Err(error),
                Ok(_) => Err("auto-plan: unexpected service response".to_string()),
                Err(e) => Err(format!("{e:#}")),
            }
        });
        job.0 = Some((i, task));
        status.0 = format!("auto-planning part {}…", i + 1);
        return; // one at a time
    }
}

/// wasm has no `auto_slice` (kernel) for the bed-overflow pre-check — auto-plan arrives with the
/// W.3.6 Worker; until then the model just opens whole (empty scene). `poll_auto_plan` no-ops (AutoJob
/// stays empty).
#[cfg(target_arch = "wasm32")]
pub(crate) fn kick_auto_plan() {}

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
    // Destructure the result BEFORE borrowing the part, so `part.pieces` can be stamped alongside the
    // cut/connector writes without fighting the cuts/conns reborrows.
    let (cuts_plan, conns_plan, pieces) = match result {
        Ok(v) => v,
        Err(e) => {
            status.0 = format!("auto-plan failed: {e:#}");
            return;
        }
    };
    let part = &mut parts.0[i];
    part.pieces = pieces; // the part's connected-component count (drives the "N pcs" header)
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    cuts.list = cuts_plan
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
    conns.list = conns_plan
        .iter()
        .map(|c| PlacedConn {
            cut: c.cut,
            pos: [c.pos[0] as f32, c.pos[1] as f32],
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
