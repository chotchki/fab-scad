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
    Touch(PathBuf), // bump <path>'s mtime (rewrite same bytes) → exercise watch_source reload
    Part(usize),   // make top-level part <i> the active one (T.2b multi-part switch)
    Export,        // export the print-oriented pieces to a Bambu .3mf (co-pack all parts, T.2b.4)
    Tab(Tab),      // switch the active workflow tab: model|parts|orientation|export (U.3.8)
    EditText(String), // append a snippet to the editor buffer → debounced buffer re-render (U.3.8)
}

#[derive(Resource)]
pub(crate) struct ScriptRunner {
    pub(crate) actions: Vec<Action>,
    pub(crate) idx: usize,
    pub(crate) timer: u32,
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
                "touch" => it.next().map(|p| Action::Touch(PathBuf::from(p))),
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
                "tab" => match it.next()? {
                    "model" => Some(Action::Tab(Tab::Model)),
                    "parts" => Some(Action::Tab(Tab::Parts)),
                    "orientation" => Some(Action::Tab(Tab::Orientation)),
                    "export" => Some(Action::Tab(Tab::Export)),
                    _ => None,
                },
                "edittext" => {
                    let snippet = it.collect::<Vec<_>>().join(" ");
                    (!snippet.is_empty()).then_some(Action::EditText(snippet))
                }
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
    scene: Res<SceneCfg>,
    mut job: ResMut<Job>,
    mut status: ResMut<Status>,
    mut editor: ResMut<EditorBuf>,
    mut files: ResMut<FileList>,
) {
    spawn_environment(&mut commands, &mut meshes, &mut materials, &scene);
    if let Some(src) = scene.source.clone() {
        read_into_editor(&mut editor, &src);
        files.files = vec![src];
        files.active = Some(0);
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
    kick_render(&mut job, &mut status, &scene, true);
}

/// Step the script: each action drives the real systems, waiting on async work to settle.
#[allow(clippy::too_many_arguments)]
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
    // Bundled: Bevy caps a system at 16 params, and a tuple counts as one.
    mut sw: (
        ResMut<FileList>,
        MessageWriter<SwitchFile>,
        ResMut<ActiveConn>,
        ResMut<EditorBuf>,
        Res<Time>,
    ),
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
    let part = &mut parts.0[active_part.0];
    let bounds = &part.bounds;
    let cuts = &mut part.cuts;
    let conns = &mut part.conns;
    let orient = &mut part.orient;
    if bounds.0.is_none() {
        return; // wait for the initial render (model + bounds + first cut)
    }
    if runner.idx >= runner.actions.len() {
        exit.write(AppExit::Success);
        return;
    }
    runner.timer += 1;
    let done = match runner.actions[runner.idx].clone() {
        Action::Cut(v) => {
            if runner.timer == 1 {
                let a = cuts.active;
                let v = clamp_to_bounds(v, cuts.active_axis(), &bounds);
                if let Some(c) = cuts.list.get_mut(a) {
                    c.at = v;
                }
            }
            runner.timer >= 2
        }
        Action::AddCut(v) => {
            if runner.timer == 1 {
                let axis = cuts.active_axis();
                let at = clamp_to_bounds(v, axis, &bounds);
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
                if let Some((min, max)) = bounds.0 {
                    if let Some(c) = cuts.list.get_mut(a) {
                        c.axis = ax;
                        c.at = comp((min + max) * 0.5, ax.index());
                    }
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
                let size = auto_size(&xsection, &cuts, &bounds, i, [a, b]);
                toggle_connector(conns, i, [a, b], size, sw.2.kind, sw.2.screw);
            }
            runner.timer >= 2
        }
        Action::ConnType(k) => {
            if runner.timer == 1 {
                sw.2.kind = k;
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
        Action::Open(path) => {
            if runner.timer == 1 {
                let list = if path.is_dir() {
                    scad_files(&path)
                } else {
                    vec![path.clone()]
                };
                if list.is_empty() {
                    eprintln!("script: open — no .scad under {}", path.display());
                } else {
                    sw.0.files = list;
                    sw.1.write(SwitchFile(0));
                }
            }
            // apply_switch_file clears bounds → None; the top guard pauses run_script until the new
            // whole render lands (bounds Some again + job idle).
            runner.timer >= 2 && job.0.is_none()
        }
        Action::Touch(path) => {
            if runner.timer == 1 {
                // Rewrite identical bytes to bump the mtime — watch_source should catch it and
                // re-render. Fails quietly if the path is gone (the test asserts on the log).
                let _ = std::fs::read(&path).and_then(|b| std::fs::write(&path, b));
            }
            // Generous fixed floor so watch_source detects + the reload render completes; if watch
            // never fires, job stays idle and this still ends — the log grep is the real assertion.
            runner.timer >= 90 && job.0.is_none()
        }
        // Handled by the early-return block above (before the active-part borrow); never reached here.
        Action::Part(_) => true,
        Action::Export => {
            if runner.timer == 1 {
                *tab = Tab::Export; // keeps print.0 on (want_print) so the laid-out pieces survive
                cmd_w.write(PanelCmd::Export); // export_plates_action co-packs all parts inline
            }
            runner.timer >= 3 // let the inline export write + status update
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
                sw.3.text.push('\n');
                sw.3.text.push_str(&snippet);
                sw.3.text.push(';');
                sw.3.dirty = true;
                sw.3.edited_at = Some(sw.4.elapsed_secs_f64());
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
    }

    #[test]
    fn parse_script_rejects_unknown_tab_and_bare_edittext() {
        // An unknown tab name and an argument-less `edittext` are dropped (filter_map), never panic.
        assert!(parse_script("tab bogus; edittext").is_empty());
    }
}
