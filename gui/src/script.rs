//! The --script harness: action grammar, parser and the timeline stepper.

use crate::*;

// ---- scripted interaction harness -----------------------------------------------------
/// One step in a `--script` timeline. Drives the REAL systems (the cut stack, request_reslice,
/// poll_job) with synthetic input, then screenshots — interaction is verified, not just setup.
#[derive(Clone)]
pub(crate) enum Action {
    Cut(f32),                     // set the ACTIVE cut's position (along its axis)
    AddCut(f32),                  // add a cut at this position (on the active axis), make it active
    SetAxis(Axis),                // set the active cut's axis
    Toggle,                       // toggle the active cut on/off
    Next,                         // cycle the active cut
    Reslice,                      // trigger a slice, then wait for the async job
    Shot(PathBuf),                // screenshot the viewport to this path
    Wait(u32),                    // idle this many frames
    Conn(usize, f32, f32), // place a connector on cut <i> at (a, b) in its plane's non-axis dims
    Edit(usize),           // open cut <i>'s 2D connector editor
    PrintView,             // toggle the print-orientation preview (renders + auto-orients pieces)
    Orient([usize; 3], [f32; 3]), // manually set piece [ix,iy,iz]'s build-up to (ux,uy,uz)
    AutoPlace,             // auto-place connectors across the open cut's cross-section
    ConnType(fab::ConnKind), // set the active connector kind for new placements (onion|bolt)
    Open(PathBuf), // switch the active source to <path> (a dir → its .scad; a file → itself)
    Part(usize),   // make top-level part <i> the active one (T.2b multi-part switch)
    Export,        // export the print-oriented pieces to a Bambu .3mf (co-pack all parts, T.2b.4)
    Tab(Tab),      // switch the active workflow tab: model|parts|orientation|export (U.3.8)
    EditText(String), // append a snippet to the editor buffer → debounced buffer re-render (U.3.8)
    Settings,      // open the Settings modal (W.3.27) — headless verify of the publish-key screen
    Publish,       // fire Publish (W.3.28) — headless verify of the kernel render + offscreen cover
    // TG.8: the document verbs. Each calls the routine the Project tab's handler calls.
    AddFile(PathBuf), // add <path> to the document (file_ops::add_paths, the Add dialog's routine)
    NewFile,          // a blank untitled.scad, viewed
    View(usize),      // view file <i> in the editor (a Project-tab row click)
    SetEntry(usize),  // make file <i> the entry, the file that renders
    Rename(usize, String), // rename file <i>
    Delete(usize),    // delete file <i> (session-only in a loose folder)
    Save, // PanelCmd::Save, the Save button and Cmd/Ctrl+S; a file-less document needs saveas
    SaveAs(PathBuf), // Save As to <path>, dialog skipped: same bytes, landing and re-home
    Expect(bool), // assert the document is unsaved (true) or not; a mismatch fails the run
}

impl Action {
    /// TG.8: does this step need a rendered model (bounds) to mean anything? Cut, connector and
    /// orientation verbs do. Document verbs, tabs, shots and waits don't, so a document with no
    /// geometry (a fresh session, a broken model) can still be scripted.
    fn needs_geometry(&self) -> bool {
        matches!(
            self,
            Action::Cut(_)
                | Action::AddCut(_)
                | Action::SetAxis(_)
                | Action::Toggle
                | Action::Next
                | Action::Reslice
                | Action::Conn(..)
                | Action::Edit(_)
                | Action::PrintView
                | Action::Orient(..)
                | Action::AutoPlace
                | Action::Export
        )
    }
}

#[derive(Resource)]
pub(crate) struct ScriptRunner {
    pub(crate) actions: Vec<Action>,
    pub(crate) idx: usize,
    pub(crate) timer: u32,
    /// TG.8: what went wrong (a failed `expect`, an `open` that opened nothing). The first one ends the
    /// run, and the app exits non-zero.
    pub(crate) failures: Vec<String>,
}

impl ScriptRunner {
    pub(crate) fn new(actions: Vec<Action>) -> Self {
        ScriptRunner {
            actions,
            idx: 0,
            timer: 0,
            failures: Vec::new(),
        }
    }
}

/// The offscreen image the camera renders into, so scripted shots can grab it.
#[derive(Resource)]
pub(crate) struct RenderTargetImage(pub(crate) Handle<Image>);

/// Parse `"addcut 30; reslice; shot a.png; toggle; reslice; shot b.png"` into a timeline.
pub(crate) fn parse_script(s: &str) -> Vec<Action> {
    s.split(';')
        .filter_map(|part| {
            let mut it = part.split_whitespace();
            match it.next()? {
                "cut" => it.next()?.parse().ok().map(Action::Cut),
                "addcut" => it.next()?.parse().ok().map(Action::AddCut),
                "axis" => match it.next()? {
                    "x" => Some(Action::SetAxis(Axis::X)),
                    "y" => Some(Action::SetAxis(Axis::Y)),
                    "z" => Some(Action::SetAxis(Axis::Z)),
                    _ => None,
                },
                "toggle" => Some(Action::Toggle),
                "next" => Some(Action::Next),
                "conntype" => match it.next()? {
                    "onion" => Some(Action::ConnType(fab::ConnKind::Onion)),
                    "bolt" => Some(Action::ConnType(fab::ConnKind::Bolt)),
                    _ => None,
                },
                "open" => it.next().map(|p| Action::Open(PathBuf::from(p))),
                "reslice" => Some(Action::Reslice),
                "shot" => it.next().map(|p| Action::Shot(PathBuf::from(p))),
                "wait" => it.next()?.parse().ok().map(Action::Wait),
                "conn" => {
                    let i = it.next()?.parse().ok()?;
                    let a = it.next()?.parse().ok()?;
                    let b = it.next()?.parse().ok()?;
                    Some(Action::Conn(i, a, b))
                }
                "edit" => it.next()?.parse().ok().map(Action::Edit),
                "part" => it.next()?.parse().ok().map(Action::Part),
                "export" => Some(Action::Export),
                "settings" => Some(Action::Settings),
                "publish" => Some(Action::Publish),
                "tab" => match it.next()? {
                    // Phase Z added the Project tab but not its verb, so the file list was the one
                    // tab the screenshot harness couldn't reach (Z.3.10 needs it — that's where
                    // rename/new/delete live).
                    "project" => Some(Action::Tab(Tab::Project)),
                    "model" => Some(Action::Tab(Tab::Model)),
                    "customize" => Some(Action::Tab(Tab::Customize)),
                    "parts" => Some(Action::Tab(Tab::Parts)),
                    "orientation" => Some(Action::Tab(Tab::Orientation)),
                    "export" => Some(Action::Tab(Tab::Export)),
                    _ => None,
                },
                "edittext" => {
                    let snippet = it.collect::<Vec<_>>().join(" ");
                    (!snippet.is_empty()).then_some(Action::EditText(snippet))
                }
                "addfile" => it.next().map(|p| Action::AddFile(PathBuf::from(p))),
                "newfile" => Some(Action::NewFile),
                "view" => it.next()?.parse().ok().map(Action::View),
                "setentry" => it.next()?.parse().ok().map(Action::SetEntry),
                "rename" => {
                    let i = it.next()?.parse().ok()?;
                    it.next().map(|n| Action::Rename(i, n.to_string()))
                }
                "delete" => it.next()?.parse().ok().map(Action::Delete),
                "save" => Some(Action::Save),
                "saveas" => it.next().map(|p| Action::SaveAs(PathBuf::from(p))),
                "expect" => match it.next()? {
                    "dirty" => Some(Action::Expect(true)),
                    "clean" => Some(Action::Expect(false)),
                    _ => None,
                },
                "printview" => Some(Action::PrintView),
                "autoplace" => Some(Action::AutoPlace),
                "orient" => {
                    let piece = [
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                    ];
                    let up = [
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                        it.next()?.parse().ok()?,
                    ];
                    Some(Action::Orient(piece, up))
                }
                other => {
                    eprintln!("script: unknown action '{other}'");
                    None
                }
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // a Bevy startup system — params are dependencies, not a smell
pub(crate) fn setup_script(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))] mut scene: ResMut<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut editor: ResMut<EditorBuf>,
    mut project: ResMut<crate::project::ProjectDoc>,
    mut pending_config: ResMut<PendingConfig>,
    mut doc_state: ResMut<DocState>,
    pool: Res<GeomPool>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    // The script harness is a native CLI: this wasm arm compiles but never runs; it keeps it building.
    #[cfg(target_arch = "wasm32")]
    if let Some(src) = scene.source.clone() {
        let _ = &mut doc_state;
        pending_config.0 = read_into_editor(&mut editor, &src);
        let name = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model.scad".into());
        *project = crate::project::ProjectDoc::single(
            name,
            editor.text.clone(),
            crate::project::ProjectHome::ScadFile(src.clone()),
        );
    }
    // SW.3 + TG.2 + TG.5: the launch document opens exactly as `setup_windowed` opens it, through
    // `boot_document` — a `.scad` as a loose project at its REAL dir (an `edittext` preview rides the
    // pack, so no disk writes at all), a `.scadproj` unpacked to its temp, adopted so path identity,
    // ownership and text come from one place; a missing or failed one falls back to the same owned
    // document. The stashed fab:config is what poll_job applies (per-part cuts + the printer).
    #[cfg(not(target_arch = "wasm32"))]
    let boot_err = crate::file_ops::boot_document(
        &mut project,
        &mut editor,
        &mut pending_config,
        &mut scene,
        &mut status,
        &mut doc_state,
    );
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(e) = &boot_err {
        eprintln!("script setup: {e}");
    }
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
        Orbit {
            yaw: -0.7,
            pitch: 0.5,
            radius,
            target: Vec3::ZERO,
        },
    ));
    commands.insert_resource(PrevCam(Some((-0.7, 0.5, radius, Vec3::ZERO))));
    commands.insert_resource(RenderTargetImage(target));
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::jobs::kick_render_pack(&pool, &mut job, &mut status, &scene, &project, None, true);
        if let Some(e) = boot_err {
            status.0 = e;
        }
    }
    #[cfg(target_arch = "wasm32")]
    kick_render(&pool, &mut job, &mut status, &scene, true);
}

/// TG.8: the document half of `run_script`'s world, bundled because Bevy caps a system at 16 params
/// (a `SystemParam` counts as one).
#[derive(SystemParam)]
pub(crate) struct ScriptDoc<'w> {
    project: ResMut<'w, crate::project::ProjectDoc>,
    switch: MessageWriter<'w, SwitchFile>,
    conn: ResMut<'w, ActiveConn>,
    editor: ResMut<'w, EditorBuf>,
    time: Res<'w, Time>,
    scene: ResMut<'w, SceneCfg>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // only the native `open` adopts
    pending: ResMut<'w, PendingConfig>,
    status: ResMut<'w, Status>,
    state: ResMut<'w, DocState>,
    rename: ResMut<'w, crate::state::RenameUi>,
}

impl ScriptDoc<'_> {
    /// Land the live buffer in the document before a structural verb, as `project_files_action` does
    /// up front every frame: a delete or rename must not strand an edit only the editor holds.
    fn flush(&mut self) {
        if self.project.editor_holds(&self.editor) {
            let text = self.editor.text.clone();
            self.project.flush_active(&text);
        }
    }

    /// Owe the viewport a fresh render of the entry, the way `file_ops::adopt` does: clear the render
    /// identity, then switch, so `apply_switch_file` reads a target change (reset, render, pending
    /// config). The headless tests run no `apply_switch_file`; there only the document moves.
    fn rerender(&mut self) {
        self.scene.source = None;
        let active = self.project.active;
        self.switch.write(SwitchFile(active));
    }

    /// A failure line: the status shows it, stderr carries it, and the run ends on it.
    fn fail(&mut self, failures: &mut Vec<String>, line: String) {
        eprintln!("script: {line}");
        self.status.0 = line.clone();
        failures.push(line);
    }
}

/// TG.8: step one DOCUMENT verb; `None` for every other action. Each calls the cfg-free routine the
/// Project tab's handler calls (`file_ops`, the `ProjectDoc` mutators, `save`), so the harness can't
/// pass on a path the UI doesn't take. Save goes through `PanelCmd::Save` itself, so `save_doc_action`
/// must be registered.
fn step_doc(
    act: &Action,
    timer: u32,
    d: &mut ScriptDoc,
    parts: &Parts,
    job: &Job,
    cmd_w: &mut MessageWriter<PanelCmd>,
    failures: &mut Vec<String>,
) -> Option<bool> {
    let first = timer == 1;
    let settled = timer >= 3 && job.0.is_none(); // a re-render owed: kicked, then landed
    Some(match act {
        Action::Open(path) => {
            if first {
                // TG.2: every open is an adopt. A dir opens its first `.scad` as a loose folder; a
                // `.scadproj` unpacks to its temp. No real file is written until a save verb.
                let entry = if path.is_dir() {
                    scad_files(path).into_iter().next()
                } else {
                    Some(path.clone())
                };
                match entry {
                    None => d.fail(failures, format!("open: no .scad under {}", path.display())),
                    #[cfg(not(target_arch = "wasm32"))]
                    Some(f) => match crate::jobs::open_document(&f, &d.scene.tmp) {
                        Ok(doc) => {
                            let d = &mut *d;
                            crate::file_ops::adopt(
                                &mut d.project,
                                &mut d.editor,
                                &mut d.pending,
                                &mut d.scene.source,
                                &mut d.status,
                                &mut d.state,
                                doc,
                            );
                            let entry = d.project.entry;
                            d.switch.write(SwitchFile(entry));
                        }
                        Err(e) => d.fail(failures, format!("open: {e:#}")),
                    },
                    #[cfg(target_arch = "wasm32")]
                    Some(_) => {}
                }
            }
            settled
        }
        Action::AddFile(path) => {
            if first {
                d.flush();
                let report = crate::file_ops::add_paths(&mut d.project, std::slice::from_ref(path));
                if let Some(line) = report.status(crate::file_ops::home_dir().as_deref()) {
                    d.status.0 = line;
                }
                for r in &report.refused {
                    eprintln!("script: {r}");
                }
            }
            timer >= 2
        }
        Action::NewFile => {
            if first {
                let idx = crate::file_ops::new_file(&mut d.project, &mut d.editor);
                d.switch.write(SwitchFile(idx));
            }
            timer >= 2
        }
        Action::View(i) => {
            if first {
                if *i < d.project.files.len() {
                    crate::file_ops::switch(&mut d.project, &mut d.editor, *i);
                    d.switch.write(SwitchFile(*i));
                } else {
                    d.fail(failures, format!("view {i}: no such file"));
                }
            }
            timer >= 2
        }
        Action::SetEntry(i) => {
            if first {
                d.flush();
                if *i < d.project.files.len() {
                    // The native handler's order: move the entry, then view it. The switch's target
                    // change re-renders and stashes the new entry's fab:config.
                    d.project.set_entry(*i);
                    crate::file_ops::switch(&mut d.project, &mut d.editor, *i);
                    d.switch.write(SwitchFile(*i));
                } else {
                    d.fail(failures, format!("setentry {i}: no such file"));
                }
            }
            settled
        }
        Action::Rename(i, want) => {
            if first {
                d.flush();
                if *i >= d.project.files.len() {
                    d.fail(failures, format!("rename {i}: no such file"));
                } else if let Some(why) = crate::file_ops::rename_clash(&d.project, *i, want) {
                    d.status.0 = why;
                } else if let Some(r) =
                    crate::file_ops::rename(&mut d.project, &mut d.editor, *i, want)
                {
                    d.rename.renamed.push((r.old.clone(), r.new.clone()));
                    if d.project.base_dir.is_some() {
                        // A never-materialized file (added, not saved) has nothing to move.
                        let _ = std::fs::rename(&r.old, &r.new);
                        d.rerender();
                    }
                }
            }
            settled
        }
        Action::Delete(i) => {
            if first && *i >= d.project.files.len() {
                d.fail(failures, format!("delete {i}: no such file"));
            } else if first {
                d.flush();
                let name = d.project.files.get(*i).map(|f| f.name.clone());
                let container = matches!(d.project.home, crate::project::ProjectHome::ScadProj(_));
                match crate::file_ops::delete(&mut d.project, &mut d.editor, *i) {
                    Ok(_) => {
                        // TG.6: only a container's temp copy leaves disk; a loose file stays.
                        if let Some(copy) = name
                            .as_deref()
                            .and_then(|n| crate::file_ops::delete_disk_target(&d.project, n))
                        {
                            let _ = std::fs::remove_file(copy);
                        }
                        if container {
                            d.rerender();
                        } else {
                            let active = d.project.active;
                            d.switch.write(SwitchFile(active));
                        }
                    }
                    Err(e) => d.status.0 = e.into(),
                }
            }
            settled
        }
        Action::Save => {
            if first {
                cmd_w.write(PanelCmd::Save);
            }
            timer >= 3 // save_doc_action reads it next frame and writes inline
        }
        Action::SaveAs(path) => {
            if first {
                #[cfg(not(target_arch = "wasm32"))]
                save_as_to(d, parts, path);
                #[cfg(target_arch = "wasm32")]
                let _ = (parts, path);
            }
            timer >= 2
        }
        Action::Expect(want) => {
            // Derived fresh, as `save_doc_action` does: `sync_doc_state` may not have run since the
            // previous verb.
            let fp = crate::config::config_fp(&parts.0, d.scene.bed);
            let got = d.state.derive(&d.project, d.editor.dirty, &fp);
            if got != *want {
                let word = |b: bool| if b { "unsaved" } else { "saved" };
                d.fail(
                    failures,
                    format!(
                        "expect {}: the document is {}",
                        if *want { "dirty" } else { "clean" },
                        word(got)
                    ),
                );
            }
            true
        }
        _ => return None,
    })
}

/// The `saveas` verb (TG.8): `save_as_action` + `poll_save_as` with the dialog's pick already made —
/// the same refusal, dialog-time snapshot, bytes, atomic write, landing and re-home. The kind is the
/// one the dialog would offer, and `save_as_target` fixes the extension as it does for a pick.
#[cfg(not(target_arch = "wasm32"))]
fn save_as_to(d: &mut ScriptDoc, parts: &Parts, path: &std::path::Path) {
    use crate::save;
    if let Some(why) = save::save_as_refusal(&d.project, &d.editor) {
        d.status.0 = save::failed_line(why);
        eprintln!("script: {}", d.status.0);
        return;
    }
    let bed = d.scene.bed;
    let deferred = save::DeferredSave::begin(&mut d.project, &d.editor, &parts.0, bed);
    let kind = save::save_as_defaults(
        &d.project,
        save::has_disk_assets(&d.project),
        crate::file_ops::home_dir().as_deref(),
    )
    .kind;
    let target = save::save_as_target(path, kind);
    let written = save::save_as_bytes(&d.project, &deferred, &parts.0, bed, kind).and_then(|b| {
        save::atomic_write(&target, &b).map_err(|e| anyhow::anyhow!("{}: {e}", target.display()))
    });
    d.status.0 = match written {
        Ok(()) => {
            let d = &mut *d;
            save::land_save_as(
                &mut d.project,
                &mut d.editor,
                &mut d.state,
                &mut d.scene,
                &deferred,
                kind,
                &target,
                &mut d.rename.renamed,
            )
        }
        Err(e) => save::failed_line(&format!("{e:#}")),
    };
    info!("{}", d.status.0);
}

/// Step the script: each action drives the real systems, waiting on async work to settle.
#[allow(clippy::too_many_arguments)] // a Bevy system — params are dependencies, not a smell
pub(crate) fn run_script(
    mut runner: ResMut<ScriptRunner>,
    mut parts: ResMut<Parts>,
    mut active_part: ResMut<ActivePart>,
    job: Res<Job>,
    target: Res<RenderTargetImage>,
    mut edit_cut: ResMut<EditCut>,
    mut tab: ResMut<Tab>,
    print_job: Res<PrintJob>,
    xsection: Res<XSection>,
    mut reslice_w: MessageWriter<ReSlice>,
    mut autoplace_w: MessageWriter<AutoPlace>,
    mut cmd_w: MessageWriter<PanelCmd>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    mut doc: ScriptDoc,
) {
    // The part-switch action rebinds what "active" means, so handle it BEFORE the active-part borrow
    // below — set the index + mark parts changed so the display systems refresh onto the new part.
    if let Some(Action::Part(i)) = runner.actions.get(runner.idx).cloned() {
        runner.timer += 1;
        if runner.timer == 1 && i < parts.0.len() {
            active_part.0 = i;
            parts.set_changed();
        }
        if runner.timer >= 2 {
            runner.idx += 1;
            runner.timer = 0;
        }
        return;
    }
    // Wait for the render in flight; a geometry verb also waits for bounds, which only a landed render
    // sets. TG.8: everything else proceeds on a model with no geometry once the render is idle — a
    // fresh session's empty `untitled.scad` never gets bounds, and the harness used to wait on it
    // forever.
    let has_bounds = parts
        .0
        .get(active_part.0)
        .is_some_and(|p| p.bounds.0.is_some());
    let wants_geometry = runner
        .actions
        .get(runner.idx)
        .is_some_and(Action::needs_geometry);
    if !has_bounds && (job.0.is_some() || wants_geometry) {
        return;
    }
    if runner.idx >= runner.actions.len() {
        exit.write(if runner.failures.is_empty() {
            AppExit::Success
        } else {
            AppExit::error()
        });
        return;
    }
    runner.timer += 1;
    let act = runner.actions[runner.idx].clone();
    let timer = runner.timer;
    if let Some(done) = step_doc(
        &act,
        timer,
        &mut doc,
        &parts,
        &job,
        &mut cmd_w,
        &mut runner.failures,
    ) {
        if !runner.failures.is_empty() {
            runner.idx = runner.actions.len(); // the first failure ends the run
        } else if done {
            runner.idx += 1;
            runner.timer = 0;
        }
        return;
    }
    let Some(part) = parts.0.get_mut(active_part.0) else {
        return;
    };
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let orient = &mut part.orient;
    let done = match act {
        Action::Cut(v) => {
            if runner.timer == 1 {
                let a = cuts.active;
                let v = clamp_to_bounds(v, cuts.active_axis(), bounds);
                if let Some(c) = cuts.list.get_mut(a) {
                    c.at = v;
                }
            }
            runner.timer >= 2
        }
        Action::AddCut(v) => {
            if runner.timer == 1 {
                let axis = cuts.active_axis();
                let at = clamp_to_bounds(v, axis, bounds);
                cuts.list.push(CutDef {
                    axis,
                    at,
                    enabled: true,
                });
                cuts.active = cuts.list.len() - 1;
            }
            runner.timer >= 2
        }
        Action::SetAxis(ax) => {
            if runner.timer == 1 {
                let a = cuts.active;
                if let Some((min, max)) = bounds.0
                    && let Some(c) = cuts.list.get_mut(a)
                {
                    c.axis = ax;
                    c.at = comp((min + max) * 0.5, ax.index());
                }
            }
            runner.timer >= 2
        }
        Action::Toggle => {
            if runner.timer == 1 {
                let a = cuts.active;
                if let Some(c) = cuts.list.get_mut(a) {
                    c.enabled = !c.enabled;
                }
            }
            runner.timer >= 2
        }
        Action::Next => {
            if runner.timer == 1 {
                let n = cuts.list.len();
                if n > 0 {
                    cuts.active = (cuts.active + 1) % n;
                }
            }
            runner.timer >= 2
        }
        Action::Reslice => {
            if runner.timer == 1 {
                reslice_w.write(ReSlice);
            }
            runner.timer > 3 && job.0.is_none() // kicked, then completed
        }
        Action::Shot(path) => {
            if runner.timer == 1 {
                commands
                    .spawn(Screenshot::image(target.0.clone()))
                    .observe(save_to_disk(path.clone()));
                info!("script: shot -> {}", path.display());
            }
            runner.timer >= 30 // give the GPU readback + save time
        }
        Action::Wait(n) => runner.timer >= n,
        Action::Conn(i, a, b) => {
            if runner.timer == 1 {
                let size = auto_size(&xsection, cuts, bounds, i, [a, b]);
                toggle_connector(conns, i, [a, b], size, doc.conn.kind, doc.conn.screw);
            }
            runner.timer >= 2
        }
        Action::ConnType(k) => {
            if runner.timer == 1 {
                doc.conn.kind = k;
            }
            runner.timer >= 2
        }
        Action::Edit(i) => {
            if runner.timer == 1 {
                *tab = Tab::Parts; // connector editing lives in Parts; else sync_tab_modes clears it
                edit_cut.0 = if edit_cut.0 == Some(i) { None } else { Some(i) };
            }
            runner.timer >= 10 // give the cross-section render + profile build time
        }
        Action::PrintView => {
            if runner.timer == 1 {
                // Orientation drives print.0 via sync_tab_modes — toggle the tab, not the flag.
                *tab = if *tab == Tab::Orientation {
                    Tab::Model
                } else {
                    Tab::Orientation
                };
            }
            // enter_exit_print kicks the render next frame; wait for the off-thread layout to land.
            runner.timer > 3 && print_job.0.is_none()
        }
        Action::Orient(piece, up) => {
            if runner.timer == 1 {
                *tab = Tab::Orientation;
                orient.set_manual(
                    (piece, 0),
                    Vec3::from_array(up).normalize_or_zero().to_array(),
                );
            }
            runner.timer >= 3 // let relayout + feasibility catch up
        }
        Action::AutoPlace => {
            if runner.timer == 1 {
                autoplace_w.write(AutoPlace);
            }
            runner.timer >= 3 // let do_auto_place run + conns update
        }
        // Handled above: `Part` by the early-return block, the document verbs by `step_doc`.
        Action::Part(_)
        | Action::Open(_)
        | Action::AddFile(_)
        | Action::NewFile
        | Action::View(_)
        | Action::SetEntry(_)
        | Action::Rename(..)
        | Action::Delete(_)
        | Action::Save
        | Action::SaveAs(_)
        | Action::Expect(_) => true,
        Action::Export => {
            if runner.timer == 1 {
                *tab = Tab::Export; // keeps print.0 on (want_print) so the laid-out pieces survive
                cmd_w.write(PanelCmd::Export); // export_plates_action co-packs all parts inline
            }
            runner.timer >= 3 // let the inline export write + status update
        }
        Action::Settings => {
            if runner.timer == 1 {
                cmd_w.write(PanelCmd::OpenSettings); // settings_modal opens on the next egui pass
            }
            runner.timer >= 3 // let the modal draw before the next shot
        }
        Action::Publish => {
            if runner.timer == 1 {
                cmd_w.write(PanelCmd::Publish); // publish_kick renders + captures the cover, then uploads
            }
            runner.timer >= 3 // the flow runs async across many frames; a following `wait` covers it
        }
        Action::Tab(t) => {
            if runner.timer == 1 {
                *tab = t; // sync_tab_modes maps it onto the print/edit flags next frame (U.3.8)
            }
            runner.timer >= 3
        }
        Action::EditText(snippet) => {
            if runner.timer == 1 {
                *tab = Tab::Model;
                // Append the snippet as a fresh top-level STATEMENT (auto-terminated — the `;` that
                // would end it is the script's own action delimiter, so the verb supplies it).
                doc.editor.text.push('\n');
                doc.editor.text.push_str(&snippet);
                doc.editor.text.push(';');
                doc.editor.dirty = true;
                doc.editor.edited_at = Some(doc.time.elapsed_secs_f64());
            }
            // preview_edited_buffer fires past the debounce + kicks a render — wait for it to land.
            runner.timer > 60 && job.0.is_none()
        }
    };
    if done {
        runner.idx += 1;
        runner.timer = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_script_reads_tab_and_edittext_verbs() {
        // U.3.8: the tab switch + the editor-edit verb (the snippet keeps its inner spaces).
        let acts = parse_script("tab parts; edittext cube([8, 8, 8]); tab model");
        assert!(matches!(
            acts.as_slice(),
            [Action::Tab(Tab::Parts), Action::EditText(s), Action::Tab(Tab::Model)]
            if s.as_str() == "cube([8, 8, 8])"
        ));
        // Z.3.10: the Project tab is reachable too — it's where file management renders.
        assert!(matches!(
            parse_script("tab project").as_slice(),
            [Action::Tab(Tab::Project)]
        ));
    }

    #[test]
    fn parse_script_rejects_unknown_tab_and_bare_edittext() {
        // An unknown tab name and an argument-less `edittext` are dropped (filter_map), never panic.
        assert!(parse_script("tab bogus; edittext").is_empty());
    }

    #[test]
    fn parse_script_reads_every_document_verb() {
        // TG.8: one line per verb, in the order the e2e uses them.
        let acts = parse_script(
            "open a/b.scadproj; addfile hook.scad; newfile; view 0; setentry 2; rename 1 lib2.scad; \
             delete 3; save; saveas out/c.scadproj; expect dirty; expect clean",
        );
        assert!(matches!(
            acts.as_slice(),
            [
                Action::Open(o),
                Action::AddFile(a),
                Action::NewFile,
                Action::View(0),
                Action::SetEntry(2),
                Action::Rename(1, r),
                Action::Delete(3),
                Action::Save,
                Action::SaveAs(s),
                Action::Expect(true),
                Action::Expect(false),
            ] if o == &PathBuf::from("a/b.scadproj")
                && a == &PathBuf::from("hook.scad")
                && r == "lib2.scad"
                && s == &PathBuf::from("out/c.scadproj")
        ));
    }

    #[test]
    fn parse_script_drops_malformed_document_verbs() {
        // A missing or unparseable argument drops the step (filter_map), never panics; `expect` takes
        // only dirty|clean.
        let bad = "addfile; view x; setentry; rename 1; rename x y.scad; delete -1; saveas; \
                   expect; expect maybe";
        assert!(parse_script(bad).is_empty());
    }

    #[test]
    fn only_cut_and_layout_verbs_wait_for_geometry() {
        // TG.8: a document with no bounds (a fresh session) must still step its document verbs.
        for v in [
            "open x.scad",
            "addfile x",
            "newfile",
            "view 0",
            "setentry 0",
            "rename 0 y.scad",
            "delete 0",
            "save",
            "saveas x.scad",
            "expect clean",
            "tab project",
            "shot a.png",
            "wait 1",
        ] {
            let a = parse_script(v);
            assert!(!a[0].needs_geometry(), "{v}");
        }
        for v in [
            "addcut 3",
            "reslice",
            "edit 0",
            "printview",
            "autoplace",
            "export",
        ] {
            let a = parse_script(v);
            assert!(a[0].needs_geometry(), "{v}");
        }
    }
}
