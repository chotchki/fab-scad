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

/// TG.1: derive [`DocState::dirty`] — the document's flags, the live buffer, and the cut plan + bed
/// against the baseline — so every Save surface reads one answer no file switch can reset. Runs next to
/// [`sync_pipeline`]. Fingerprinting a handful of small JSON blocks per frame is cheap, and `Parts` has
/// too many writers for change detection to gate it honestly.
pub(crate) fn sync_doc_state(
    project: Res<crate::project::ProjectDoc>,
    editor: Res<EditorBuf>,
    parts: Res<Parts>,
    scene: Res<SceneCfg>,
    mut doc: ResMut<DocState>,
) {
    // TG.2 (design Risks): a loader that replaced the document without `adopt` strands a buffer at one
    // of its paths under the old owner, and every flush then silently skips it.
    debug_assert!(
        !project.editor_aliases(&editor),
        "the editor holds {} for another document: load through file_ops::adopt",
        editor.path.display()
    );
    let live = config::config_fp(&parts.0, scene.bed);
    doc.baseline_or(&live);
    let dirty = doc.derive(&project, editor.dirty, &live);
    if doc.dirty != dirty {
        doc.dirty = dirty; // write only on a flip, so DocState's change tick means something
    }
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
    /// TG.1: its config baseline describes the model being wiped, so it goes with it.
    pub(crate) doc: ResMut<'w, DocState>,
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
        // TG.1: without this, the open/entry-change window before the fresh render lands compares
        // the reset (empty) plan to the OLD model's saved one and flashes "unsaved".
        self.doc.forget_baseline();
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
        if project.editor_holds(&editor) {
            project.flush_active(&editor.text);
        }
        project.set_active(i);
        // Same doc-hydration as native (SW.3) — one implementation, so the web can't drift. It also
        // strips the viewed file's `fab:config` block, which the hand-rolled web copy never did:
        // switching tabs on the browser used to surface the block as editable text.
        doc_into_editor(&mut editor, &project, i);
        // No `return`: this arm and the native one below are mutually exclusive cfgs, so it'd be
        // dead on both (clippy::needless_return on wasm).
    }
    // Native: the render paths ARE ProjectDoc's native projection (base_dir/name per file). The whole
    // tail is native — it reads a real path + kicks a Source::Path render — so it's cfg'd off wasm.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = project.native_paths().get(i).cloned() else {
            // TG.5: a document with no `base_dir` (a fresh session's `untitled.scad`) has no paths,
            // but its files are still viewable: swap the editor view exactly as the web does. Nothing
            // re-renders — the preview renders the buffer, and the cut plan stays.
            if i < project.files.len() {
                crate::file_ops::switch(&mut project, &mut editor, i);
            }
            return;
        };
        // Z.3.6 option A: EVERY project renders its ENTRY. Switching a file changes the editor VIEW, not
        // the render target — view a lib and the entry stays on screen; set-entry to change what renders.
        // Persist the OUTGOING file's live edit before moving off it — but ONLY when the editor actually
        // holds the current active file (a within-project switch). On a FRESH open the editor still carries
        // the PREVIOUS project's text, and flushing that would clobber the new entry. The flush lands in
        // the DOC only (SW.3) — the pack render reads the doc, so no render-root sync exists to write.
        if project.editor_holds(&editor) {
            project.flush_active(&editor.text);
        }
        project.set_active(i);
        // The render identity is the entry's REAL path (provenance/UI); the render CONTENT is the pack.
        let render_path = project
            .base_dir
            .as_ref()
            .zip(project.files.get(project.entry))
            .map(|(b, f)| b.join(&f.name))
            .unwrap_or_else(|| path.clone());
        // Only a CHANGE of render target re-renders — a view-switch (entry unchanged) just swaps the editor.
        let changed = scene.source.as_deref() != Some(render_path.as_path());
        scene.source = Some(render_path.clone());
        // Re-derive the workspace root by walking up from the REAL folder — that's where BOSL2/scad-lib
        // live. A container's `base_dir` is a temp under `tmp/` with no workspace above it
        // (loose_save_dir None), so its root stays the boot value (the packed lib root).
        if let Some(real_dir) = loose_save_dir(&project)
            && let Some(r) = std::fs::canonicalize(&real_dir)
                .ok()
                .as_deref()
                .and_then(fab::find_root_from)
        {
            scene.root = Some(r);
        }
        // The VIEWED file's DOC text (minus its fab:config block, W.3.8) becomes the editor buffer —
        // the doc is the truth (SW.3): a disk read here would drop unsaved edits made before the switch.
        doc_into_editor(&mut editor, &project, i);
        if changed {
            // The stashed block applies in poll_job once the fresh parts are built. TG.1: the ENTRY's
            // block, not the viewed file's: a target change while a library is on screen (a loose
            // delete of the entry, the script's re-render after a rename) used to stash that file's
            // block (none) and the saved plan gave way to an auto-plan that read clean. (A view-switch
            // leaves parts untouched.)
            pending_config.0 = project.entry_config();
            // Drop the outgoing model's held base solids before wiping — a file switch abandons them.
            free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
            state.reset();
            kick_render_pack(&pool, &mut job, &mut status, &scene, &project, None, true);
            info!("render: {}", render_path.display());
        }
    }
}

/// Drain the native file pick into a [`ProjectDoc`] (Phase Z): a loose `.scad` opens its FOLDER as the
/// project (siblings become files, that file the entry); a `.scadproj` materializes + opens as a project.
/// TG.2: the loaded doc is [adopted](crate::file_ops::adopt) — editor hydrated + owned, old render target
/// cleared — so the `SwitchFile` that follows re-renders and resets the plan even when the new entry's
/// path equals the old one. On cancel, nothing. The whole body is native — on wasm the picker is hidden
/// (no fs) so the dialog never has a task and every param reads unused.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
#[cfg_attr(target_arch = "wasm32", allow(unused_variables, unused_mut))]
pub(crate) fn poll_open_dialog(
    mut dlg: ResMut<OpenDialog>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut switch: MessageWriter<SwitchFile>,
    mut status: ResMut<Status>,
    mut scene: ResMut<SceneCfg>,
    mut editor: ResMut<EditorBuf>,
    mut pending_config: ResMut<PendingConfig>,
    mut doc_state: ResMut<DocState>,
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
    #[cfg(not(target_arch = "wasm32"))]
    match open_document(&picked, &scene.tmp) {
        Ok(doc) => {
            crate::file_ops::adopt(
                &mut project,
                &mut editor,
                &mut pending_config,
                &mut scene.source,
                &mut status,
                &mut doc_state,
                doc,
            );
            switch.write(SwitchFile(project.entry));
        }
        Err(e) => status.0 = format!("open: {e:#}"),
    }
}

/// TG.2: load the document at `path` for any native opener (the Open dialog, the launch argument): a
/// `.scadproj` (any case) materializes under `tmp` ([`unpack_scadproj`]); anything else opens as a loose
/// `.scad` and its folder ([`open_loose`]). The caller [adopts](crate::file_ops::adopt) the result.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn open_document(
    path: &std::path::Path,
    tmp: &std::path::Path,
) -> anyhow::Result<crate::project::ProjectDoc> {
    if is_scadproj(path) {
        unpack_scadproj(path, tmp)
    } else {
        open_loose(path)
    }
}

/// Does `path` name a `.scadproj` (extension, any case)? Cfg-free: the launch-argument picker uses it.
pub(crate) fn is_scadproj(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("scadproj"))
}

/// Materialize a `.scadproj` under `tmp` and return the [`ProjectDoc`] rooted there — text files editable,
/// binary assets ride-along, `base_dir` the temp ASSET root, `home` the original `.scadproj` (so save
/// re-zips it, Z.3.5). The ProjectDoc's bytes are the truth; since SW.3 the unpacked copy only has to
/// serve `import()`/`surface()`, because the loader gets its `.scad` from the pack.
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
    // Write the WHOLE unpacked image, not just the importables: a container's temp is also what the
    // user sees if they go looking, and what a crash leaves behind to recover from. The ProjectDoc
    // keeps the canonical copies.
    materialize_all(&doc, &dir)?;
    if doc.files.is_empty() {
        anyhow::bail!("no .scad in {}", path.display());
    }
    doc.base_dir = Some(dir);
    Ok(doc)
}

/// Open a loose `.scad` as a project (SW.3): `from_disk` loads the FOLDER's `.scad` files and
/// `base_dir` stays the REAL folder — the render rides `Source::Pack` (live buffers as the hybrid
/// overlay), so nothing is ever mirrored to a temp and preview writes NO file anywhere. Assets
/// (`import("logo.svg")`, STLs) resolve from the real dir, where they actually live — including
/// `../` refs the folder-shaped shadow could never have served. `home` is `ScadFile(real path)`, so
/// Save writes back in place.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn open_loose(picked: &std::path::Path) -> anyhow::Result<crate::project::ProjectDoc> {
    let dir = picked.parent().unwrap_or(picked).to_path_buf();
    let scads = scad_files(&dir);
    if scads.is_empty() {
        anyhow::bail!("no .scad under {}", dir.display());
    }
    Ok(crate::project::ProjectDoc::from_disk(dir, &scads, picked))
}

/// The REAL on-disk folder a loose project saves back to — its `home` `ScadFile` path's parent.
/// `None` for a container / web / paste. Since SW.3 this equals `base_dir` for a loose doc, but the
/// two are asked DIFFERENT questions ("where does Save write?" vs "what does the render root at?")
/// and a container answers them differently, so they stay separate lookups.
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
/// (so a reopen restores the bed), every other file + binary asset goes verbatim. TG.3: the input is a
/// [`DocSnapshot`](crate::save::DocSnapshot), so the live editor text is always in it, and the manifest
/// keeps the entry AND the title the archive opened with (it used to write `None` and drop it).
/// Cross-platform (Z.3.8): `scadproj` write works on wasm too, so the web save/publish can re-zip.
///
/// `extra` are assets that belong in the ARCHIVE but not in the document — a loose project's on-disk
/// siblings ([`loose_sibling_assets`]), which SW.3 deliberately stopped copying into the doc. The
/// doc's own copies win on a name collision.
pub(crate) fn rezip_project(
    snap: &crate::save::DocSnapshot,
    parts: &[Part],
    printer: config::PrinterCfg,
    extra: &std::collections::BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    use fab_scad::scadproj;
    let entry_baked = snap.entry_baked(parts, printer);
    // `extra` first, so a doc-owned name of the same path overwrites it.
    let mut files: std::collections::BTreeMap<String, Vec<u8>> = extra.clone();
    for (i, f) in snap.files().iter().enumerate() {
        let text = match &entry_baked {
            Some(baked) if i == snap.entry() => baked.clone(),
            _ => f.text.clone(),
        };
        files.insert(f.name.clone(), text.into_bytes());
    }
    for (name, body) in snap.assets() {
        files.insert(name.clone(), body.clone());
    }
    let proj = scadproj::project_from_files(
        files,
        Some(snap.entry_name().to_string()),
        snap.title().map(str::to_string),
    )?;
    scadproj::write_scadproj(&proj)
}

/// The SOURCE variant to upload for the current document (Z.3.8 save-back / Z.5 publish): a `.scadproj`
/// for a MULTI-FILE project — the site ingests it as `OpenscadProject` via its `.scadproj` probe (Z.4),
/// so the gallery item re-opens as a real project — else a config-baked `.scad`. Returns
/// `(filename, mime, bytes)`; `stem` names the file. TG.3: serialized from a
/// [`DocSnapshot`](crate::save::DocSnapshot), so the active file's live text rides in either shape —
/// the multi-file branch used to re-zip the stored text and drop an unflushed edit. The web upload
/// paths carry bytes; native publish rezips to a temp file for `upload_model`'s path API instead.
/// Cfg-free so TG.4's test can prove the web upload carries the live edit; only wasm calls it.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn project_source_variant(
    snap: &crate::save::DocSnapshot,
    parts: &[Part],
    printer: config::PrinterCfg,
    stem: &str,
) -> anyhow::Result<(String, &'static str, Vec<u8>)> {
    use fab_scad::scadproj::{PROJECT_EXT, PROJECT_MIME};
    if snap.is_multifile() {
        // No `extra`: the browser has no disk to sweep — a web document already holds every byte it owns.
        let bytes = rezip_project(snap, parts, printer, &std::collections::BTreeMap::new())?;
        Ok((format!("{stem}.{PROJECT_EXT}"), PROJECT_MIME, bytes))
    } else {
        let baked = snap.entry_baked(parts, printer).unwrap_or_default();
        Ok((
            format!("{stem}.scad"),
            "application/x-openscad",
            baked.into_bytes(),
        ))
    }
}

/// Materialize every project file + asset under `base` — the CONTAINER temp's disk image (a
/// `.scadproj` unpacks here; its assets are what the pack render's `read_import` reads). Loose
/// projects never call this (SW.3): their real dir already holds everything.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn materialize_all(
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
/// stay aligned) and re-renders from the pack. The NATIVE half — the ops touch the fs + the rfd
/// picker; the web half is [`project_files_action_web`], which shares the document rules via
/// [`crate::file_ops`]. Runs alongside apply_switch_file; a SwitchFile
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
    mut pending_config: ResMut<PendingConfig>,
    mut state: ModelState,
) {
    // SW.3: the DOC is the text truth AND the render source, so the live buffer must land in it
    // before ANY command reads or reshuffles the file set. Under the old shadow the preview kept a
    // disk mirror current, which papered over the missing flush — a delete then re-hydrated the
    // editor from that mirror and the unsaved edit survived by accident. With the mirror gone, an
    // unflushed edit is simply lost (and the pack renders the stale text). One flush, up front.
    if project.editor_holds(&editor) {
        project.flush_active(&editor.text);
    }
    // Inline rename (Z.3.3): apply the committed (row, new-name) — rename in the ProjectDoc, MOVE the
    // on-disk/temp copy, then re-render (the render target's name may have changed, and a rename can
    // break a sibling's `include`, which the re-render surfaces).
    if let Some((i, want)) = rename.commit.take() {
        let base = project.base_dir.clone();
        // The rename MOVES a real file (SW.3 made `base_dir` the user's folder), so refuse rather than
        // land on top of something — `file_ops::rename_clash`, shared with the harness (TG.8).
        // Z.3.10: the ProjectDoc + editor half is `file_ops::rename` — shared with the web handler, so
        // the `editor.path` re-point (the invariant a rename is most likely to break) has ONE
        // implementation and one set of tests. What stays here is the native tail: move the file on
        // disk, re-aim the render identity, kick a fresh render.
        if let Some(why) = crate::file_ops::rename_clash(&project, i, &want) {
            status.0 = why;
        } else if let Some(r) = crate::file_ops::rename(&mut project, &mut editor, i, &want) {
            rename.renamed.push((r.old.clone(), r.new.clone()));
            if base.is_some() {
                // Both are already rooted under `base_dir` (`file_ops::Renamed`), so this is the move.
                // A missing source is a never-materialized file (added but not yet saved) — fine.
                let _ = std::fs::rename(&r.old, &r.new);
                // Re-aim the render IDENTITY at the entry's current name. Always the ENTRY, both
                // homes: the pack renders the entry, so pointing `scene.source` at the ACTIVE file
                // (as this did for loose) left the two disagreeing and made the next view-switch
                // read `changed` and throw away every part for nothing.
                scene.source = Some(project.editor_path(project.entry));
                free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
                // TG.1: the fresh build re-takes the baseline, so it has to rebuild the SAVED plan
                // too; with nothing stashed an auto-plan replaced it and read clean (Save then wrote
                // it over the real one). Unsaved plan edits still go: keeping them is the
                // file_ops follow-up (openspec/backlog.md).
                pending_config.0 = project.entry_config();
                state.reset();
                kick_render_pack(&pool, &mut job, &mut status, &scene, &project, None, true);
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
                // No disk write (SW.3): the new file exists in the DOC, rides the pack render, and
                // hydrates from the doc on switch — it reaches disk on an explicit Save, not before.
                let idx = project.add_file("untitled.scad", String::new());
                switch.write(SwitchFile(idx)); // view the new file (entry unchanged → no re-render)
            }
            PanelCmd::DeleteFile(i) => {
                let container = matches!(project.home, crate::project::ProjectHome::ScadProj(_));
                let Some(name) = project.remove_file(i) else {
                    status.0 = "can't delete the project's only file".into();
                    continue;
                };
                // TG.6: what (if anything) leaves disk is decided in one tested place — `Some` only
                // for a container's temp; a loose folder's real file is never rm'd.
                if let Some(copy) = crate::file_ops::delete_disk_target(&project, &name) {
                    let _ = std::fs::remove_file(copy);
                }
                if container {
                    // A container OWNS its files (temp scratch): the deleted copy is gone above (tidy
                    // — the pack no longer reads it); re-hydrate the editor from the DOC, re-render.
                    // The hydrate is only safe because of the flush at the top of this system.
                    let _ = doc_into_editor(&mut editor, &project, project.active); // config stays the entry's
                    free_bases(&pool, state.parts.0.iter().filter_map(|p| p.base).collect());
                    pending_config.0 = project.entry_config(); // TG.1: as the rename above
                    state.reset();
                    kick_render_pack(&pool, &mut job, &mut status, &scene, &project, None, true);
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

/// The async bridge from the browser's file picker (off in a JS event + promise) back to a Bevy system
/// (Z.3.10) — `async-channel` because its ends are `Send + Sync` (Resource-safe), unbounded so the send
/// never blocks. The wasm twin of [`AddFileDialog`]'s rfd task; same landing point,
/// [`ProjectDoc::import`](crate::project::ProjectDoc::import).
#[cfg(target_arch = "wasm32")]
#[derive(Resource)]
pub(crate) struct WebFilePick {
    tx: async_channel::Sender<Vec<(String, Vec<u8>)>>,
    rx: async_channel::Receiver<Vec<(String, Vec<u8>)>>,
}

#[cfg(target_arch = "wasm32")]
impl Default for WebFilePick {
    fn default() -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self { tx, rx }
    }
}

/// Land a browser file pick (Z.3.10): import each file into the project — text becomes an editable file,
/// binary an asset, names de-duplicated — exactly as `poll_add_dialog` does on desktop. Adding files
/// doesn't re-render: nothing `include`s them until you say so. It DOES dirty the document, since the
/// saved `.scadproj` now carries more than it did.
#[cfg(target_arch = "wasm32")]
pub(crate) fn poll_web_file_pick(
    bridge: Res<WebFilePick>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut status: ResMut<Status>,
) {
    let Ok(picked) = bridge.rx.try_recv() else {
        return; // nothing picked this frame
    };
    let added = picked.len();
    for (name, bytes) in picked {
        project.import(&name, bytes);
    }
    if added > 0 {
        // TG.1: `import` marks the document unsaved itself — no `editor.dirty` patch to lose on a switch.
        status.0 = format!("added {added} file(s) to the project");
    }
}

/// Project-tab file management on the WEB (Z.3.10) — the twin of [`project_files_action`] for a document
/// with no `base_dir`. Every mutation it performs is the same cross-platform [`ProjectDoc`] call; what
/// differs is everything AROUND them, which is why this is a separate system rather than `cfg` blocks
/// inside one (the native signature carries five params — rfd, `SceneCfg`, `GeomPool` — that mean nothing
/// here).
///
/// Three things this must get right, each of which is a silent-corruption bug if missed:
///
/// 1. **`(editor.owner, editor.path)` is the buffer's OWNERSHIP TOKEN on web** — the path is a name, not
///    a location. `apply_switch_file` flushes the live text back into `files[active]` only when
///    [`ProjectDoc::editor_holds`](crate::project::ProjectDoc::editor_holds) — owner is this doc (TG.2)
///    AND `path == files[active].name`; let the path diverge
///    and the next row-click discards every unsaved edit without a word. So a rename of the ACTIVE file
///    re-points it — at the POST-dedup name, since `rename_file` may return `foo-1.scad` for a typed
///    `foo.scad`.
/// 2. **It rehydrates the editor ITSELF instead of writing `SwitchFile`.** `apply_switch_file`'s wasm
///    branch clears `edited_at`, and the two systems are registered unordered — emitting a switch AND
///    arming the render is a coin flip on whether the render ever happens.
/// 3. **The render kick is the armed debounce** (`edited_at = Some(0.0)`), not a direct
///    [`kick_render_bytes`]: it inherits the "a render is already in flight" guard and needs neither
///    `GeomPool` nor `Job`. It must be the LAST write to `editor` in the frame.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn project_files_action_web(
    mut ev: MessageReader<PanelCmd>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut rename: ResMut<crate::state::RenameUi>,
    mut editor: ResMut<EditorBuf>,
    mut pending_config: ResMut<PendingConfig>,
    mut status: ResMut<Status>,
    mut state: ModelState,
    picker: Res<WebFilePick>,
) {
    use crate::file_ops::{self, Rerender};

    // The strongest re-render owed this frame; paid once, at the end.
    let mut owed = Rerender::No;
    let mut escalate = |r: Rerender| {
        if r == Rerender::Target || owed == Rerender::No {
            owed = r;
        }
    };

    if let Some((i, want)) = rename.commit.take()
        && let Some(r) = file_ops::rename(&mut project, &mut editor, i, &want)
    {
        // Tell `panel_ui` to move this file's customizer-defaults entry with it.
        rename.renamed.push((r.old, r.new));
        // A rename can orphan a sibling's `use <old.scad>`, and the worker drops a missing ref
        // silently — so re-render to surface it. Geometry is otherwise untouched: keep the cuts.
        escalate(Rerender::Same);
    }
    // Snapshot the frame's commands (PanelCmd is Copy) so the reader borrow ends before we mutate.
    for cmd in ev.read().copied().collect::<Vec<_>>() {
        match cmd {
            PanelCmd::SetEntry(i) => {
                escalate(file_ops::set_entry(
                    &mut project,
                    &mut editor,
                    &mut pending_config,
                    i,
                ));
            }
            PanelCmd::NewFile => {
                file_ops::new_file(&mut project, &mut editor);
            }
            PanelCmd::DeleteFile(i) => match file_ops::delete(&mut project, &mut editor, i) {
                Ok(r) => escalate(r),
                Err(e) => status.0 = e.into(),
            },
            // The browser's own picker stands in for rfd; `poll_web_file_pick` lands the bytes.
            PanelCmd::AddFiles => crate::web_host::pick_files(picker.tx.clone()),
            _ => {}
        }
    }
    match owed {
        Rerender::No => {}
        Rerender::Same => editor.edited_at = Some(0.0),
        Rerender::Target => {
            // A different model renders now — the old one's parts/cuts/print state is meaningless.
            state.reset();
            editor.edited_at = Some(0.0);
        }
    }
}

/// Drain the "Add files" multi-pick (Z.3.3) into [`file_ops::add_paths`](crate::file_ops::add_paths)
/// (TG.6) — the routine the `addfile` harness verb shares, so the per-home rules (a loose folder gets
/// every file copied in and refuses clashes, a container de-dups and stays unsaved) have one
/// implementation. Adding files doesn't re-render: nothing `include`s a new file until you say so.
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
    let report = crate::file_ops::add_paths(&mut project, &paths);
    if let Some(line) = report.status(crate::file_ops::home_dir().as_deref()) {
        status.0 = line;
    }
}

/// The `import()`/`surface()` formats the render reads (the demux in `fab::import::read_import`) —
/// what [`loose_sibling_assets`] sweeps out of the folder when a loose project gets PACKAGED. Native
/// only: the browser has no folder to sweep.
#[cfg(not(target_arch = "wasm32"))]
const ASSET_EXTS: &[&str] = &["svg", "dxf", "stl", "3mf", "off", "dat", "png"];

/// [`collect_scads`]'s asset twin: every file under `dir` a model could `import()`/`surface()`, same
/// skip rules (hidden files/dirs and generated-output dirs are never source).
#[cfg(not(target_arch = "wasm32"))]
fn collect_assets(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if name.starts_with('.') || matches!(name.as_ref(), "out" | "renders" | "target") {
                continue;
            }
            collect_assets(&p, out);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| ASSET_EXTS.iter().any(|a| x.eq_ignore_ascii_case(a)))
            && !name.starts_with('.')
        {
            out.push(p);
        }
    }
}

/// A LOOSE project's on-disk sibling assets, for the PACKAGING step — the only thing SW.3 still needs
/// `collect_assets` for. Rendering doesn't: `read_import` reads the real folder directly, which is the
/// whole point of killing the shadow. But a `.scadproj` (Save-As, and the publish upload) is a
/// self-contained archive, and a document whose `assets` are empty because the files "are already on
/// disk" produces one that renders nothing on the other end.
///
/// Returns rather than absorbing, deliberately: this is serialization state, not document state. An
/// earlier cut of it wrote straight into `doc.assets` and every folder STL showed up as a Project-tab
/// row the user never added — including when they cancelled the save dialog.
///
/// LIMIT: folder-scoped, like every other loose sweep here. A model reaching OUTSIDE its folder
/// (`import("../logo.svg")`) can't be made self-contained without rewriting the ref, so it isn't.
/// Empty for a container (its assets already live in the doc) and for the web (nothing on disk).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn loose_sibling_assets(
    project: &crate::project::ProjectDoc,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    loose_sibling_asset_paths(project)
        .into_iter()
        .filter_map(|(name, p)| Some((name, std::fs::read(&p).ok()?)))
        .collect()
}

/// TG.5: does a loose document's folder hold an asset its model could `import()`? Save As must then
/// offer a `.scadproj` — a lone `.scad` written elsewhere strands the import. A name scan, no reads.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn has_loose_sibling_assets(project: &crate::project::ProjectDoc) -> bool {
    !loose_sibling_asset_paths(project).is_empty()
}

/// [`loose_sibling_assets`]' names + paths, before any read: the folder's importables minus every name
/// the doc already carries (the doc's copy is the live one).
#[cfg(not(target_arch = "wasm32"))]
fn loose_sibling_asset_paths(project: &crate::project::ProjectDoc) -> Vec<(String, PathBuf)> {
    let Some(dir) = loose_save_dir(project) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    collect_assets(&dir, &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let name = p
                .strip_prefix(&dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            (name, p)
        })
        .filter(|(name, _)| {
            !project.assets.contains_key(name) && !project.files.iter().any(|f| &f.name == name)
        })
        .collect()
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
/// include base come from `src`, not `cfg.source`. Since SW.3 the one caller is the PASTE preview
/// (no project on disk, so its buffer goes to a scratch file); every project render goes through
/// [`kick_render_pack`] instead.
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

/// Whole-render the PROJECT as a `Source::Pack` (SW.3): the doc's live buffers ride as the hybrid
/// overlay — optionally with `live` spliced over the ACTIVE file (the debounced-edit case) — and
/// imports/fs-fallback root at `base_dir` (the REAL folder for a loose open, the materialized
/// asset root for a container). No disk mirror, no shadow: the pack IS the render truth. A doc
/// with no `base_dir` (a paste, W.3.33) falls back to the path render of `cfg.source`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn kick_render_pack(
    pool: &GeomPool,
    job: &mut Job,
    status: &mut Status,
    cfg: &SceneCfg,
    project: &crate::project::ProjectDoc,
    live: Option<&str>,
    fresh: bool,
) {
    let (Some(base), false) = (project.base_dir.as_ref(), project.files.is_empty()) else {
        kick_render(pool, job, status, cfg, fresh);
        return;
    };
    let active_text = live
        .map(str::to_string)
        .or_else(|| project.files.get(project.active).map(|f| f.text.clone()))
        .unwrap_or_default();
    let (files, entry) = project.hybrid_pack(&active_text);
    let source = Source::Pack {
        files,
        entry,
        asset_dir: base.to_string_lossy().into_owned(),
    };
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

/// The `?model=` fetch in flight (W.3.12): the bytes arriving from the page URL's `model` parameter,
/// plus that URL. The NAME is derived on arrival, not here (Z.3.9) — it prefers the response's
/// `Content-Disposition` filename and only falls back to the URL, so it can't be computed until the
/// response lands. Spawned by `setup_windowed`, landed by [`poll_model_fetch`].
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct ModelFetch {
    /// `(bytes, Content-Disposition)` — see [`crate::web_host::fetch_bytes`].
    pub task: Option<bevy::tasks::Task<Option<crate::web_host::FetchedModel>>>,
    /// The `?model=` URL, kept for the name fallback when the header is absent/unreadable.
    pub url: String,
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
    mut scene: ResMut<SceneCfg>,
    mut doc_state: ResMut<DocState>,
) {
    let Some(task) = fetch.task.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return; // still fetching
    };
    fetch.task = None;
    // Z.3.9: name the document from the RESPONSE (`Content-Disposition`) when it says something, else
    // from the URL — a `?model=/media/<ref>` basename is an opaque hash, not a name.
    let name =
        crate::web_name::model_name(result.as_ref().and_then(|(_, d)| d.as_deref()), &fetch.url);
    let demo = || {
        crate::project::ProjectDoc::single(
            "demo.scad",
            crate::scene::WEB_DEMO,
            crate::project::ProjectHome::Fresh,
        )
    };
    // TG.2: every arm builds a document and `adopt` makes it the open one — the entry hydrates the
    // editor (fab:config stripped + stashed) and owns it, so the buffer and the doc can't disagree on
    // whose text this is. A fallback demo is `Fresh`, so adopt posts nothing and the arm's line stands.
    let (doc, fallback) = match result.map(|(bytes, _)| bytes) {
        // A `.scadproj` deep-link (Z.3.4): the bytes are a zip (PK magic) → open it as a multi-file
        // project; render_pack sends the whole project to the worker.
        Some(bytes) if bytes.starts_with(b"PK\x03\x04") => {
            match crate::project::ProjectDoc::from_scadproj(
                &bytes,
                crate::project::ProjectHome::WebModel(name.clone()),
            ) {
                Ok(doc) => (doc, None),
                Err(e) => (
                    demo(),
                    Some(format!("bad .scadproj ({e:#}) — rendering the demo")),
                ),
            }
        }
        // A plain `.scad` deep-link is a one-file project; WebModel carries the download/save name.
        Some(bytes) => (
            crate::project::ProjectDoc::single(
                name.clone(),
                String::from_utf8_lossy(&bytes).into_owned(),
                crate::project::ProjectHome::WebModel(name),
            ),
            None,
        ),
        None => (
            demo(),
            Some("model fetch failed (URL reachable? CORS/CORP?) — rendering the demo".into()),
        ),
    };
    crate::file_ops::adopt(
        &mut project,
        &mut editor,
        &mut pending_config,
        &mut scene.source,
        &mut status,
        &mut doc_state,
        doc,
    );
    if let Some(line) = fallback {
        status.0 = line;
    }
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
                        colors: w.colors,
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
/// no render is in flight, whole-render it (`fresh = false` — keep each part's cuts/connectors). The
/// buffer, not the disk file, is the truth; the disk file only changes on an explicit Save. Since
/// SW.3 that's literal on every path with a project — the buffer IS the render input. Only a PASTE
/// (no project on disk at all) still stages to a scratch file, because a path is all the paste flow
/// has to hand the renderer.
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
    // native, ANY project with a base (SW.3): the live buffers ride the Source::Pack overlay —
    // editing ANY project file re-renders the entry with the edit spliced, NO disk write anywhere
    // (the shadow died with the pack; the user's real files change only on an explicit Save).
    #[cfg(not(target_arch = "wasm32"))]
    if project.base_dir.is_some() {
        kick_render_pack(
            &pool,
            &mut job,
            &mut status,
            &scene,
            &project,
            Some(&editor.text),
            false,
        );
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
            Ok(Response::Resliced { stl, colors }) => Ok(JobResult::Resliced { part, stl, colors }),
            Ok(Response::Failed { error, .. }) => Err(error),
            Ok(_) => Err("reslice: unexpected service response".to_string()),
            Err(e) => Err(format!("{e:#}")),
        }
    });
    job.0 = Some(task);
    status.0 = "slicing".into();
}

/// A fresh parts build goes live (`poll_job`): the stashed `fab:config` applies to `new` BEFORE it
/// does (a part whose block set cuts makes `kick_auto_plan` stand down, so config wins over
/// auto-derive, W.3.8; no block means every part auto-derives), the model's printer (if it declared
/// one) overrides the boot bed, then the config baseline is re-taken (TG.1).
pub(crate) fn seat_fresh_parts(
    parts: &mut Parts,
    mut new: Vec<Part>,
    pending: &mut PendingConfig,
    scene: &mut SceneCfg,
    doc: &mut DocState,
) {
    if let Some(cfg) = pending.0.take() {
        config::apply_blocks(&mut new, &cfg.parts);
        // The web has no printers.toml, so the .scad's fab:config IS the bed AND plate source there
        // (W.3.8), which is why this slaves the export plate to the bed (set_configured_bed).
        if let Some(p) = cfg.printer {
            scene.set_configured_bed([p.bed[0] as f32, p.bed[1] as f32, p.bed[2] as f32]);
        }
    }
    *parts = Parts(new);
    // TG.1: the parts now hold exactly what the document's fab:config says (or nothing, for a part
    // that auto-derives), and the bed is the model's: that IS the saved config. Taken from the live
    // parts, never by re-parsing the block, so float round-trips can't make a fresh open read unsaved.
    // A part-COUNT rebuild of the same source lands here too: its plans were just wiped, and the text
    // edit that caused it is unsaved anyway.
    doc.rebaseline(config::config_fp(&parts.0, scene.bed));
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
    mut doc: ResMut<DocState>,
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
                let new: Vec<Part> = rendered
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
                            r.colors.as_deref(),
                            r.min,
                            r.max,
                            r.name.clone(),
                        )
                    })
                    .collect();
                seat_fresh_parts(&mut parts, new, &mut pending_config, &mut scene, &mut doc);
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
                        r.colors.as_deref(),
                        r.min,
                        r.max,
                        r.name.clone(),
                    );
                }
            }
            // TG.2: an open's "opened <name> (<folder>)" outlives its own render's statuses.
            status.0 = match doc.take_opened() {
                Some(opened) => format!("{opened} — ready"),
                None => "ready".into(),
            };
            // A distinct, greppable signal that geometry rendered end-to-end (on wasm this rode the geom
            // Worker round-trip) — the release boot gate waits for it to prove the bundle isn't
            // dead-on-arrival (release-web.yml). Bevy's LogPlugin routes it to the browser console.
            info!("fab-gui render complete: {} part(s)", parts.0.len());
            // The displayed geometry now matches the current source — clear the Model/Parts stale flag
            // (U.3.7). `sync_pipeline` compares the live source hash against this each frame.
            pipeline.geo_of = Some(hash_one(&editor.text));
        }
        Ok(JobResult::Resliced { part, stl, colors }) => {
            let mesh = mesh_from_bytes(&mut meshes, &stl, colors.as_deref());
            let colored = colors.is_some();
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
                    MeshMaterial3d(part_material(&mut materials, colored)),
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
            // TG.2: still say what opened when its first render fails — and drop the held line, so a
            // later render doesn't claim an open that's long past.
            status.0 = match doc.take_opened() {
                Some(opened) => format!("{opened} — error: {e}"),
                None => format!("error: {e}"),
            };
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
    colors: Option<&[Option<[f32; 4]>]>,
    min: [f64; 3],
    max: [f64; 3],
    name: Option<String>,
) -> Part {
    let mesh = mesh_from_bytes(meshes, stl, colors);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials, colors.is_some())),
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
    colors: Option<&[Option<[f32; 4]>]>,
    min: [f64; 3],
    max: [f64; 3],
    name: Option<String>,
) {
    let mesh = mesh_from_bytes(meshes, stl, colors);
    despawn_part_models(commands, models, i);
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(part_material(materials, colors.is_some())),
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
/// TG.4: rides with the [`DeferredSave`](crate::save::DeferredSave) it uploads — the snapshot and its `rev` —
/// so `poll_save` lands exactly those bytes.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct SaveJob(
    pub(crate) Option<(Task<Result<String, String>>, crate::save::DeferredSave)>,
);

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
    mut project: ResMut<crate::project::ProjectDoc>,
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
    // TG.4: the upload's bytes AND what success lands, captured together at one `rev` — the live
    // buffer flushed first, so an edit typed mid-upload stays unsaved instead of being cleared with it.
    let site = crate::save::DeferredSave::begin(&mut project, &editor, &parts.0, scene.bed);
    let printer = crate::save::DeferredSave::printer(scene.bed);
    // Z.3.9: every uploaded part is named off the DOCUMENT, not off `editor.path` (the ACTIVE file) —
    // which for a `.scadproj` was the archive's inside name, and for a multi-file project was whichever
    // file the editor happened to be showing. These names are load-bearing: the site types each part by
    // its extension (see `upload_multipart`).
    let stem = project.doc_stem().unwrap_or_else(|| {
        let name = editor
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("model.scad");
        name.strip_suffix(".scad")
            .or_else(|| name.strip_suffix(".scadproj"))
            .unwrap_or(name)
            .to_string()
    });
    // Z.3.8: the SOURCE variant is the whole `.scadproj` for a project (lifts the destructive-save guard —
    // PUT /variants replaces the set, and now the set carries the archive), else the config-baked `.scad`.
    let (src_name, src_mime, src_bytes) =
        match project_source_variant(site.snapshot(), &parts.0, printer, &stem) {
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
    job.0 = Some((task, site));
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

/// The in-flight item-rename job (Z.3.10) — `PUT /media/<ref>` with a JSON title. Yields the new local
/// document name on success (the handler already knows it) or an error string.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
pub(crate) struct RenameItemJob(pub(crate) Option<Task<Result<String, String>>>);

/// The suffix `publish_web` gives the MODEL item of a published trio (`"{title} — model"`, alongside
/// `— print plates` and `— cover`). A rename re-applies it so the three stay grouped in the gallery;
/// [`crate::web_name`] strips it back off on load, so the editor only ever shows the bare title.
#[cfg(target_arch = "wasm32")]
const ITEM_TITLE_SUFFIX: &str = " — model";

/// Rename the hotchkiss.io media item (Z.3.10): `PUT /media/<ref>` a JSON `{title}` under the ambient
/// session cookie. The metadata twin of [`save_action`]'s byte write — same origin, same cookie, same
/// Admin gate (the site's `require_admin_for_mutations` catches every non-safe method), so the failure
/// modes and their wording match. An absent field KEEPS its value server-side, so sending only `title`
/// can never disturb the item's visibility.
///
/// The affordance is gated on a derived [`MediaItem`], not on being Admin — the app can't know its own
/// role without an extra round trip, and the existing Save button already takes the same bet: attempt
/// the write, report the 401/403 plainly. Better a clear rejection than a hidden button.
#[cfg(target_arch = "wasm32")]
pub(crate) fn rename_item_action(
    mut ev: MessageReader<PanelCmd>,
    mut ui_state: ResMut<crate::state::ItemRenameUi>,
    item: Res<crate::state::MediaItem>,
    project: Res<crate::project::ProjectDoc>,
    mut job: ResMut<RenameItemJob>,
    mut status: ResMut<Status>,
) {
    if !ev.read().any(|c| *c == PanelCmd::RenameOnSite) {
        return;
    }
    let Some(title) = ui_state.commit.take().map(|t| t.trim().to_string()) else {
        return;
    };
    if title.is_empty() {
        status.0 = "a title is required".into();
        return;
    }
    if job.0.is_some() {
        status.0 = "already renaming…".into();
        return;
    }
    let Some(url) = item.0.clone() else {
        status.0 = "this model isn't a hotchkiss.io item".into();
        return;
    };
    // Keep the document's EXTENSION — `ProjectHome::WebModel` holds a filename (Z.3.9) and `doc_stem`
    // takes its stem, so dropping the extension here would rename the next upload to `<title>` with no
    // suffix at all.
    let ext = match &project.home {
        crate::project::ProjectHome::WebModel(n) => std::path::Path::new(n)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("scad")
            .to_string(),
        _ => "scad".to_string(),
    };
    let local = format!("{title}.{ext}");
    // Serialized by hand: one field, and pulling serde_json into the wasm bundle for it would be silly.
    // A title can contain `"` and `\`, both of which MUST be escaped or the body is malformed JSON.
    let body = format!(
        "{{\"title\":\"{}\"}}",
        format!("{title}{ITEM_TITLE_SUFFIX}")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    );
    status.0 = format!("renaming to {title}…");
    job.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
        let (code, _body) = crate::web_host::fetch_json("PUT", &url, &body).await?;
        match code {
            200..=299 => Ok(local),
            401 => Err("rename rejected (401) — log in as admin on hotchkiss.io first".into()),
            403 => Err("rename rejected (403) — this session isn't an admin".into()),
            404 => Err("rename rejected (404) — this model no longer exists on the site".into()),
            s => Err(format!("rename rejected: HTTP {s}")),
        }
    }));
}

/// Land the item rename (Z.3.10): on success adopt the new name LOCALLY too, so the Project-tab header,
/// the Save download filename and the publish stem (all of which read `ProjectHome` via
/// [`ProjectDoc::doc_stem`](crate::project::ProjectDoc::doc_stem)) agree with the site immediately —
/// rather than waiting for a reload, which the byte route's year-long immutable cache would answer with
/// the OLD name anyway.
#[cfg(target_arch = "wasm32")]
pub(crate) fn poll_rename_item(
    mut job: ResMut<RenameItemJob>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut status: ResMut<Status>,
) {
    let Some(task) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    job.0 = None;
    match result {
        Ok(local) => {
            project.home = crate::project::ProjectHome::WebModel(local);
            status.0 = "renamed on hotchkiss.io".into();
            info!("{}", status.0);
        }
        Err(e) => {
            status.0 = e;
            error!("{}", status.0);
        }
    }
}

/// Land the save-back job: report success or the error (per gui-reactive-standard — the status bar is
/// the feedback surface, no modal). Both outcomes LOG (not just set status) so the W.5.9 headless boot
/// gate can grep a save success/failure off the console. TG.4: success lands the captured snapshot
/// (`mark_saved(rev)`, the config baseline, the buffer), so the document reads saved; a failure
/// touches nothing, so it stays unsaved.
#[cfg(target_arch = "wasm32")]
pub(crate) fn poll_save(
    mut job: ResMut<SaveJob>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut editor: ResMut<EditorBuf>,
    mut doc: ResMut<DocState>,
    mut status: ResMut<Status>,
) {
    let Some((task, _)) = job.0.as_mut() else {
        return;
    };
    let Some(result) = block_on(future::poll_once(task)) else {
        return;
    };
    let Some((_, site)) = job.0.take() else {
        return;
    };
    match result {
        Ok(_) => {
            status.0 = if site.land(&mut project, &mut editor, &mut doc) {
                "saved to hotchkiss.io".into()
            } else {
                "saved to hotchkiss.io — changes made since are still unsaved".into()
            };
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
    mut doc: ResMut<DocState>,
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
    // TG.1: a derived plan over no saved one is what a reopen derives too — not an unsaved change.
    doc.auto_planned(i, config::part_fp(&parts.0, i));
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness: unwrap/expect ARE the assertions"
)]
mod tests {
    use crate::project::{ProjectDoc, ProjectHome};
    use crate::*;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fab_jobs_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One part carrying a single X cut at `at` — the plan a saved `fab:config` restores.
    fn planned(at: f32) -> Part {
        Part {
            cuts: Cuts {
                list: vec![CutDef {
                    axis: Axis::X,
                    at,
                    enabled: true,
                }],
                active: 0,
            },
            ..default()
        }
    }

    /// `model` with a saved plan (one X cut at `at`) baked in, the way Save writes the entry.
    fn saved_entry(model: &str, at: f32) -> String {
        config::with_config_block(
            model,
            &[planned(at)],
            Some(config::PrinterCfg {
                bed: [256.0, 256.0, 256.0],
            }),
        )
    }

    fn scene_at(tmp: &Path) -> SceneCfg {
        SceneCfg {
            source: None,
            stl: None,
            bed: [256.0; 3],
            plate: [256.0; 2],
            root: None,
            tmp: tmp.to_path_buf(),
            reslice_on_start: false,
            cut_pct: 50.0,
        }
    }

    /// The live state a landed render of `d` leaves (what `poll_job` does): the editor on the entry,
    /// the render identity on its path, the entry's saved plan on the parts, the baseline taken.
    fn rendered(d: ProjectDoc, tmp: &Path) -> App {
        let mut e = EditorBuf::default();
        let mut pending = PendingConfig::default();
        let mut scene = scene_at(tmp);
        let mut doc = DocState::default();
        let mut project = ProjectDoc::default();
        crate::file_ops::adopt(
            &mut project,
            &mut e,
            &mut pending,
            &mut scene.source,
            &mut Status(String::new()),
            &mut doc,
            d,
        );
        scene.source = Some(project.editor_path(project.entry));
        let mut parts = Parts(vec![]);
        super::seat_fresh_parts(
            &mut parts,
            vec![Part::default()],
            &mut pending,
            &mut scene,
            &mut doc,
        );
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<SwitchFile>()
            .add_message::<PanelCmd>()
            .insert_resource(project)
            .insert_resource(e)
            .insert_resource(scene)
            .insert_resource(parts)
            .insert_resource(doc)
            .insert_resource(pending)
            .insert_resource(Status(String::new()))
            .insert_resource(crate::geom::GeomPool::new(1))
            .init_resource::<Job>()
            .init_resource::<ActivePart>()
            .init_resource::<EditCut>()
            .init_resource::<XSection>()
            .init_resource::<PrintView>()
            .init_resource::<crate::print::PrintJob>()
            .init_resource::<crate::print::PrintPieces>()
            .init_resource::<Feas>()
            .init_resource::<AddFileDialog>()
            .init_resource::<crate::state::RenameUi>();
        app
    }

    /// Is the document unsaved, derived as `sync_doc_state` derives it?
    fn unsaved(app: &App) -> bool {
        let w = app.world();
        let fp = config::config_fp(&w.resource::<Parts>().0, w.resource::<SceneCfg>().bed);
        w.resource::<DocState>().derive(
            w.resource::<ProjectDoc>(),
            w.resource::<EditorBuf>().dirty,
            &fp,
        )
    }

    /// After a model-state reset: a render is in flight, the old plan is gone, and what `poll_job`
    /// will apply is a plan with one X cut at `at`. Then land it and read the cut back.
    fn rebuilds_with(app: &mut App, at: f32) -> f32 {
        let w = app.world();
        assert!(w.resource::<Job>().0.is_some(), "a fresh render was kicked");
        assert!(
            w.resource::<Parts>()
                .0
                .iter()
                .all(|p| p.cuts.list.is_empty()),
            "the old model's plan is wiped"
        );
        let stashed = w
            .resource::<PendingConfig>()
            .0
            .as_ref()
            .expect("a stashed plan");
        assert_eq!(stashed.parts[0].cut[0].at.f(), f64::from(at));
        // The render lands: the same step `poll_job` takes.
        app.world_mut().resource_scope(|w, mut parts: Mut<Parts>| {
            w.resource_scope(|w, mut pending: Mut<PendingConfig>| {
                w.resource_scope(|w, mut scene: Mut<SceneCfg>| {
                    let mut doc = w.resource_mut::<DocState>();
                    super::seat_fresh_parts(
                        &mut parts,
                        vec![Part::default()],
                        &mut pending,
                        &mut scene,
                        &mut doc,
                    );
                })
            })
        });
        app.world().resource::<Parts>().0[0].cuts.list[0].at
    }

    /// TG.1 (critic gap): a native rename and a container delete reset the model and re-render it.
    /// They used to stash no `fab:config`, so the fresh build re-took the baseline from EMPTY parts,
    /// an auto-plan then replaced the saved plan and read clean, and the next Save wrote it over the
    /// real one. Now the entry's saved block rides the rebuild.
    #[test]
    fn a_rename_or_container_delete_rebuilds_the_saved_plan() {
        let dir = scratch("replan");
        let folder = dir.join("brace");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("main.scad"), saved_entry("cube(10);\n", 2.5)).unwrap();
        std::fs::write(folder.join("lib.scad"), "// lib\n").unwrap();
        let loose = ProjectDoc::from_disk(
            folder.clone(),
            &[folder.join("lib.scad"), folder.join("main.scad")],
            &folder.join("main.scad"),
        );
        let lib = loose
            .files
            .iter()
            .position(|f| f.name == "lib.scad")
            .unwrap();
        let mut app = rendered(loose, &dir.join("tmp"));
        assert!(!unsaved(&app), "a saved plan re-applied reads clean");
        app.add_systems(Update, super::project_files_action);

        // Loose: rename the library (the file moves on disk at once; nothing for Save to write).
        app.world_mut()
            .resource_mut::<crate::state::RenameUi>()
            .commit = Some((lib, "hook.scad".into()));
        app.update();
        assert!(folder.join("hook.scad").exists() && !folder.join("lib.scad").exists());
        assert_eq!(rebuilds_with(&mut app, 2.5), 2.5, "the saved cut is back");
        assert!(!unsaved(&app), "and the document is still saved");

        // A container: delete the library from the temp image.
        let tmp = dir.join("tmp");
        let mut boxed = ProjectDoc::single(
            "main.scad",
            saved_entry("cube(10);\n", 4.0),
            ProjectHome::ScadProj(dir.join("brace.scadproj")),
        );
        boxed.add_file("lib.scad", "// lib\n".into());
        let root = tmp.join("scadproj").join("brace");
        super::materialize_all(&boxed, &root).unwrap();
        boxed.base_dir = Some(root.clone());
        boxed.mark_saved(boxed.rev());
        let mut app = rendered(boxed, &tmp);
        assert!(!unsaved(&app));
        app.add_systems(Update, super::project_files_action);
        app.world_mut().write_message(PanelCmd::DeleteFile(1));
        app.update();
        assert!(!root.join("lib.scad").exists(), "the temp copy goes");
        assert_eq!(rebuilds_with(&mut app, 4.0), 4.0, "the saved cut is back");
        let w = app.world();
        let fp = config::config_fp(&w.resource::<Parts>().0, w.resource::<SceneCfg>().bed);
        assert!(
            !w.resource::<DocState>().config_dirty(&fp),
            "the plan matches the saved one"
        );
        assert!(unsaved(&app), "the archive Save writes has lost a file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TG.2 (critic gap; spec: opening another archive with the same name): the open re-renders and
    /// applies the NEW document's own plan even though its entry's path is the old one's exactly.
    /// `adopt` clears the render identity, so `apply_switch_file` reads a target change.
    #[test]
    fn a_same_stem_open_rerenders_with_its_own_plan() {
        let dir = scratch("same_stem");
        let tmp = dir.join("tmp");
        let archive = |sub: &str, model: &str, at: f32| {
            let d = dir.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            let files = [
                ("main.scad".to_string(), saved_entry(model, at).into_bytes()),
                ("lib.scad".to_string(), b"// lib\n".to_vec()),
            ]
            .into_iter()
            .collect();
            let proj =
                fab_scad::scadproj::project_from_files(files, Some("main.scad".into()), None)
                    .unwrap();
            let path = d.join("brace.scadproj");
            std::fs::write(&path, fab_scad::scadproj::write_scadproj(&proj).unwrap()).unwrap();
            super::open_document(&path, &tmp).unwrap()
        };
        let a = archive("a", "cube(1);\n", 1.0);
        let mut app = rendered(a, &tmp);
        assert_eq!(app.world().resource::<Parts>().0[0].cuts.list[0].at, 1.0);
        let before = app.world().resource::<SceneCfg>().source.clone();
        let b = archive("b", "sphere(2);\n", 7.0);
        assert_eq!(
            Some(b.editor_path(b.entry)),
            before,
            "the same entry path: only identity tells them apart"
        );
        app.add_systems(Update, super::apply_switch_file);
        app.world_mut()
            .resource_scope(|w, mut project: Mut<ProjectDoc>| {
                w.resource_scope(|w, mut editor: Mut<EditorBuf>| {
                    w.resource_scope(|w, mut pending: Mut<PendingConfig>| {
                        w.resource_scope(|w, mut scene: Mut<SceneCfg>| {
                            w.resource_scope(|w, mut status: Mut<Status>| {
                                let mut doc = w.resource_mut::<DocState>();
                                crate::file_ops::adopt(
                                    &mut project,
                                    &mut editor,
                                    &mut pending,
                                    &mut scene.source,
                                    &mut status,
                                    &mut doc,
                                    b,
                                );
                            })
                        })
                    })
                })
            });
        let entry = app.world().resource::<ProjectDoc>().entry;
        app.world_mut().write_message(SwitchFile(entry));
        app.update();
        let w = app.world();
        assert_eq!(w.resource::<SceneCfg>().source, before);
        assert_eq!(w.resource::<EditorBuf>().text, "sphere(2);\n");
        assert!(
            w.resource::<ProjectDoc>()
                .editor_holds(w.resource::<EditorBuf>())
        );
        assert_eq!(rebuilds_with(&mut app, 7.0), 7.0, "B's own saved cut");
        assert!(!unsaved(&app), "a freshly opened document is saved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SW.3 — the shadow is DEAD: a loose open keeps `base_dir` at the REAL folder (the render
    /// rides `Source::Pack`, assets resolve where they live) and writes NOTHING anywhere — the
    /// user's dir is untouched and no temp mirror exists to go stale (the whole backlog-#6 class,
    /// killed structurally).
    #[test]
    fn a_loose_open_keeps_the_real_dir_and_writes_nothing() {
        let real = std::env::temp_dir().join(format!("fab_loose_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&real);
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("model.scad"), "import(\"logo.svg\");\n").unwrap();
        std::fs::write(real.join("logo.svg"), "<svg/>").unwrap();
        let before = std::fs::read_dir(&real).unwrap().flatten().count();

        let doc = super::open_loose(&real.join("model.scad")).expect("opens");

        assert_eq!(
            doc.base_dir.as_deref(),
            Some(real.as_path()),
            "base_dir IS the real folder — no shadow re-root"
        );
        assert!(doc.assets.is_empty(), "no byte-copies: assets stay on disk");
        let after = std::fs::read_dir(&real).unwrap().flatten().count();
        assert_eq!(before, after, "the real folder is untouched");
        let _ = std::fs::remove_dir_all(&real);
    }

    /// The sweep rules that used to decide what a loose open MIRRORED now decide what a `.scadproj`
    /// (Save-As, publish upload) CONTAINS — a shipped archive, so getting them wrong is worse than it
    /// was. Same assertions the shadow-era test made, re-aimed at the surviving code.
    #[test]
    fn the_packaging_sweep_takes_siblings_and_skips_generated_output() {
        let real = std::env::temp_dir().join(format!("fab_sweep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&real);
        std::fs::create_dir_all(real.join("things")).unwrap();
        std::fs::create_dir_all(real.join("out")).unwrap();
        std::fs::write(real.join("model.scad"), "import(\"logo.svg\");\n").unwrap();
        std::fs::write(real.join("logo.svg"), "<svg/>").unwrap();
        std::fs::write(real.join("things/part.stl"), b"solid t\nendsolid t\n").unwrap();
        std::fs::write(real.join("out/render.stl"), b"solid o\nendsolid o\n").unwrap();
        std::fs::write(real.join(".hidden.svg"), "<svg/>").unwrap();

        let mut doc = super::open_loose(&real.join("model.scad")).expect("opens");
        let swept = super::loose_sibling_assets(&doc);
        let keys: Vec<&str> = swept.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["logo.svg", "things/part.stl"],
            "siblings ride (subdir keeps its relative path); out/ + hidden don't"
        );
        assert!(
            doc.assets.is_empty(),
            "the sweep RETURNS, it doesn't absorb"
        );

        // A name the document already owns is the document's — the sweep never shadows a live copy.
        doc.assets.insert("logo.svg".into(), b"live".to_vec());
        assert_eq!(
            super::loose_sibling_assets(&doc)
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["things/part.stl"]
        );

        // A container has no loose folder to sweep.
        doc.home = crate::project::ProjectHome::ScadProj(real.join("p.scadproj"));
        assert!(super::loose_sibling_assets(&doc).is_empty());
        let _ = std::fs::remove_dir_all(&real);
    }
}
