//! U.3.11 — headless script-driven STATE-ASSERTION tests for the Parts drill (U.3.3).
//!
//! Integration path only: parse a script → step a MINIMAL Bevy app (no render/egui/window) →
//! assert the resource state the panel drives. `fab-gui` is a binary crate, so this is an IN-crate
//! `#[cfg(test)] mod` (a `gui/tests/` file couldn't see the `pub(crate)` surface).
//!
//! The app runs `MinimalPlugins` + exactly four HEADLESS-CLEAN systems (`run_script`,
//! `sync_tab_modes`, `enforce_exclusive_modes`, `do_auto_place`). Systems that touch
//! `Assets`/egui/gizmos/disk-STL/Manifold are deliberately excluded — anything needing a real slice
//! (`Shot`, `Reslice`, render/print/export) is out of scope for state assertions by design.
//!
//! The one gotcha the harness bakes in: `run_script` spins forever while the active part's `bounds`
//! is `None` (only a completed render sets it, which we don't run), so every seeded part carries
//! `ModelBounds(Some(_))`.

use crate::*;
use bevy::input::touch::TouchPhase; // MouseWheel carries a phase field; not in the crate's re-export
use bevy::math::DVec2; // physical cursor position setter takes DVec2 (not in the re-exported prelude)

const LO: Vec3 = Vec3::splat(-50.0);
const HI: Vec3 = Vec3::splat(50.0);

/// A part whose bounds are already set (the settle-gate passes without a render), carrying `cuts`.
fn seeded_part(cuts: Vec<CutDef>) -> Part {
    Part {
        bounds: ModelBounds(Some((LO, HI))),
        cuts: Cuts {
            list: cuts,
            active: 0,
        },
        ..default()
    }
}

fn x_cut(at: f32) -> CutDef {
    CutDef {
        axis: Axis::X,
        at,
        enabled: true,
    }
}

/// The headless App: `MinimalPlugins` (Time + TaskPool + loop; no render/egui/window), the four
/// state systems, and every resource the script pipeline reads — one part pre-seeded.
fn harness() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_message::<ReSlice>()
        .add_message::<AutoPlace>()
        .add_message::<SwitchFile>()
        .add_message::<PanelCmd>()
        .init_resource::<Job>()
        .init_resource::<ActivePart>()
        .init_resource::<ActiveConn>()
        .init_resource::<EditCut>()
        .init_resource::<XSection>()
        .init_resource::<PrintView>()
        .init_resource::<PrintJob>()
        .init_resource::<Tab>()
        .init_resource::<EditorBuf>()
        .init_resource::<FileList>()
        .init_resource::<crate::project::ProjectDoc>()
        .init_resource::<crate::state::RenameUi>()
        .insert_resource(Status("test".into()))
        .insert_resource(RenderTargetImage(Handle::default())) // dummy: only the Shot verb reads it
        .insert_resource(Parts(vec![seeded_part(vec![x_cut(0.0)])]))
        .insert_resource(ScriptRunner {
            actions: vec![],
            idx: 0,
            timer: 0,
        })
        .add_systems(
            Update,
            (
                run_script,
                sync_tab_modes,
                enforce_exclusive_modes,
                do_auto_place,
            ),
        );
    app
}

/// Build, run `seed` (plant XSection / extra parts / empty cuts before stepping), inject the parsed
/// timeline, then `app.update()` until it drains (or a frame cap trips), plus a few settle frames.
fn drive_seeded(script: &str, seed: impl FnOnce(&mut World)) -> App {
    let mut app = harness();
    seed(app.world_mut());
    let actions = parse_script(script);
    let n = actions.len();
    app.insert_resource(ScriptRunner {
        actions,
        idx: 0,
        timer: 0,
    });

    let cap = 200 + n as u32 * 60; // every headless action drains in <=10 frames (Edit floor)
    let mut frames = 0u32;
    while app.world().resource::<ScriptRunner>().idx < n {
        app.update();
        frames += 1;
        assert!(
            frames < cap,
            "script stalled: idx {}/{n} after {frames} frames",
            app.world().resource::<ScriptRunner>().idx
        );
    }
    for _ in 0..4 {
        app.update(); // let sync_tab_modes/enforce settle the final transition
    }
    app
}

fn drive(script: &str) -> App {
    drive_seeded(script, |_| {})
}

/// The active part — every assertion reads `Parts.0[ActivePart.0]` (there is no top-level `Cuts`).
fn active(app: &App) -> &Part {
    let p = app.world().resource::<Parts>();
    &p.0[app.world().resource::<ActivePart>().0]
}

// ── S1 — addcut appends a CutDef on the ACTIVE axis ──────────────────────────────────────────────
#[test]
fn addcut_appends_on_active_axis() {
    let app = drive_seeded("addcut 5; axis y; addcut 20", |w| {
        w.insert_resource(Parts(vec![seeded_part(vec![])])); // start with NO cuts
    });
    let c = &active(&app).cuts;
    assert_eq!(c.list.len(), 2);
    assert_eq!(c.list[0].axis, Axis::Y); // `axis y` rotated cut 0 + recentered
    assert_eq!(c.list[0].at, 0.0); // recentre to bbox mid
    assert_eq!(c.list[1].axis, Axis::Y); // 2nd add followed the active axis
    assert_eq!(c.list[1].at, 20.0); // clamp_to_bounds(20) within [-50,50]
    assert!(c.list[1].enabled);
    assert_eq!(c.active, 1); // new cut becomes active
}

// ── S2 — edit sets EditCut; a second edit on the same index clears it ─────────────────────────────
#[test]
fn edit_opens_then_toggles_closed() {
    let opened = drive("edit 0");
    assert_eq!(opened.world().resource::<EditCut>().0, Some(0));
    assert_eq!(*opened.world().resource::<Tab>(), Tab::Parts);

    let closed = drive("edit 0; edit 0");
    assert_eq!(closed.world().resource::<EditCut>().0, None);
    assert_eq!(*closed.world().resource::<Tab>(), Tab::Parts); // still Parts, editor just closed
}

// ── S3 — conntype switches ActiveConn.kind ────────────────────────────────────────────────────────
#[test]
fn conntype_switches_active_kind() {
    let app = drive("conntype bolt");
    assert_eq!(
        app.world().resource::<ActiveConn>().kind,
        fab::ConnKind::Bolt
    );

    let back = drive("conntype bolt; conntype onion");
    assert_eq!(
        back.world().resource::<ActiveConn>().kind,
        fab::ConnKind::Onion
    );
}

// ── S4 — autoplace populates Conns for the edited cut (seeds XSection) ─────────────────────────────
#[test]
fn autoplace_populates_conns_for_edited_cut() {
    let app = drive_seeded("edit 0; autoplace", |w| {
        // A cut-plane profile in the cut's two non-axis dims (an X cut → (Y,Z) coords).
        let square = vec![vec![
            [-40.0, -40.0],
            [40.0, -40.0],
            [40.0, 40.0],
            [-40.0, 40.0],
        ]];
        w.resource_mut::<XSection>().0 = Some(square);
    });
    let conns = &active(&app).conns.list;
    assert!(
        !conns.is_empty(),
        "auto_place should fit >=1 onion in an 80x80 face"
    );
    assert!(conns.iter().all(|c| c.cut == 0)); // all on the edited cut
    assert!(conns.iter().all(|c| c.kind == fab::ConnKind::Onion)); // default ActiveConn kind
}

// ── S5 — conn places a folded-in connector; a nearby re-click toggles it off ──────────────────────
#[test]
fn conn_places_then_toggles_off() {
    let placed = drive("conn 0 5 5");
    let c = &active(&placed).conns.list;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].cut, 0);
    assert_eq!(c[0].pos, [5.0, 5.0]);
    assert_eq!(c[0].kind, fab::ConnKind::Onion);

    let toggled = drive("conn 0 5 5; conn 0 5 5"); // 2nd click on the same point removes it
    assert!(active(&toggled).conns.list.is_empty());
}

// ── S6 — conntype governs what conn places ────────────────────────────────────────────────────────
#[test]
fn conntype_governs_placed_kind() {
    let app = drive("conntype bolt; conn 0 5 5");
    let c = &active(&app).conns.list;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].kind, fab::ConnKind::Bolt);
}

// ── S7 — leaving the Parts tab clears EditCut; Orientation raises PrintView ───────────────────────
#[test]
fn leaving_parts_tab_clears_editcut() {
    let app = drive("edit 0; tab model"); // edit → Parts + Some(0), then leave Parts
    assert_eq!(*app.world().resource::<Tab>(), Tab::Model);
    assert_eq!(app.world().resource::<EditCut>().0, None); // sync_tab_modes cleared it
}

#[test]
fn orientation_tab_raises_print_view() {
    let app = drive("tab orientation");
    assert_eq!(*app.world().resource::<Tab>(), Tab::Orientation);
    assert!(app.world().resource::<PrintView>().0); // want_print
}

#[test]
fn pipeline_dirty_propagates_downstream() {
    // U.3.7 per-node stale derivation (Model, Parts, Orientation, Export):
    // nothing computed yet → all clean (not-yet-run reads clean, not stale).
    assert_eq!(jobs::derive_dirty(None, None, 1, 2), [false; 4]);
    // source drifted off the rendered geometry → Model + Parts stale, propagating to Orient + Export.
    assert_eq!(
        jobs::derive_dirty(Some(1), Some(9), 2, 9),
        [true, true, true, true]
    );
    // geometry current but the slice config drifted → only Orientation + Export stale.
    assert_eq!(
        jobs::derive_dirty(Some(1), Some(9), 1, 8),
        [false, false, true, true]
    );
    // everything matches → all clean.
    assert_eq!(jobs::derive_dirty(Some(1), Some(9), 1, 9), [false; 4]);
}

#[test]
fn pipeline_loading_lights_the_computing_stage() {
    // Spinner-badge core: geometry work → Model+Parts; layout work → Orientation+Export; both → all;
    // idle → none (unlike `dirty`, this fires on the FIRST compute, before anything is stale).
    assert_eq!(jobs::derive_loading(false, false), [false; 4]);
    assert_eq!(
        jobs::derive_loading(true, false),
        [true, true, false, false]
    );
    assert_eq!(
        jobs::derive_loading(false, true),
        [false, false, true, true]
    );
    assert_eq!(jobs::derive_loading(true, true), [true; 4]);
}

#[test]
fn status_activity_names_what_is_running() {
    // The pulse label can never read a stale "ready": idle → None; a geometry job → generic rebuild;
    // the print layout → orienting; an auto-plan is most specific and WINS (names its 1-based part).
    assert_eq!(jobs::busy_activity(None, false, false), None);
    assert_eq!(
        jobs::busy_activity(None, true, false).as_deref(),
        Some("rebuilding geometry…")
    );
    assert_eq!(
        jobs::busy_activity(None, false, true).as_deref(),
        Some("orienting pieces…")
    );
    assert_eq!(
        jobs::busy_activity(Some(1), true, true).as_deref(),
        Some("auto-planning part 2…") // 0-based index → 1-based label, and it beats the others
    );
}

#[test]
fn platform_gates_the_file_picker() {
    // U.3.6: desktop shows the ＋/folder picker; web (one presupplied file, no fs access) hides it.
    assert!(Platform::Desktop.shows_picker());
    assert!(!Platform::Web.shows_picker());
    assert_eq!(Platform::default(), Platform::Desktop); // native build → desktop
}

#[test]
fn orient_reset_drops_the_manual_override_back_to_auto() {
    // The Orientation tab's per-piece "reset to auto" (U.3.4): drop the manual key so `up_or` falls
    // back to the auto-pick and the list re-flags the piece as auto.
    let mut o = Orient::default();
    let key = ([0, 1, 0], 2);
    o.set_manual(key, [1.0, 0.0, 0.0]);
    assert!(o.manual.contains(&key));
    assert_eq!(o.up_or(key, [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]); // manual override wins
    o.reset(key);
    assert!(!o.manual.contains(&key));
    assert_eq!(o.up_or(key, [0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]); // back to the auto fallback
}

// ── S8 — part selects the active part and routes subsequent edits to it ───────────────────────────
#[test]
fn part_switch_routes_edits_to_the_active_part() {
    let app = drive_seeded("part 1; addcut 7", |w| {
        w.insert_resource(Parts(vec![
            seeded_part(vec![x_cut(0.0)]), // part 0
            seeded_part(vec![x_cut(0.0)]), // part 1 — MUST also have bounds (settle-gate)
        ]));
    });
    assert_eq!(app.world().resource::<ActivePart>().0, 1);
    let p = app.world().resource::<Parts>();
    assert_eq!(p.0[1].cuts.list.len(), 2); // the addcut landed in part 1
    assert_eq!(p.0[0].cuts.list.len(), 1); // part 0 untouched
}

#[test]
fn part_switch_out_of_range_is_ignored() {
    let app = drive_seeded("part 5", |w| {
        w.insert_resource(Parts(vec![
            seeded_part(vec![x_cut(0.0)]),
            seeded_part(vec![]),
        ]));
    });
    assert_eq!(app.world().resource::<ActivePart>().0, 0); // i<len false → no change
}

// ── S9 — toggle / next cut-stack navigation (drill state) ─────────────────────────────────────────
#[test]
fn next_cycles_active_and_toggle_flips_enabled() {
    let app = drive_seeded("next; toggle", |w| {
        w.insert_resource(Parts(vec![seeded_part(vec![x_cut(-10.0), x_cut(10.0)])]));
    });
    let c = &active(&app).cuts;
    assert_eq!(c.active, 1); // next: (0+1)%2
    assert!(!c.list[1].enabled); // toggle flipped the now-active cut off
    assert!(c.list[0].enabled); // the other untouched
}

// ── remove_cut renumbering — no script verb, so exercised as a pure fn (mirrors cuts.rs) ───────────
#[test]
fn remove_cut_renumbers_connectors() {
    let mut cuts = Cuts {
        list: vec![x_cut(-10.0), x_cut(0.0), x_cut(10.0)],
        active: 2,
    };
    let mk = |cut| PlacedConn {
        cut,
        pos: [0.0, 0.0],
        size: 6.0,
        kind: fab::ConnKind::Onion,
        screw: Screw::M3,
    };
    let mut conns = Conns {
        list: vec![mk(0), mk(1), mk(2)],
    };
    remove_cut(&mut cuts, &mut conns, 1);
    assert_eq!(cuts.list.len(), 2);
    assert_eq!(cuts.active, 1); // clamped down from 2
    let cut_of: Vec<usize> = conns.list.iter().map(|c| c.cut).collect();
    assert_eq!(cut_of, vec![0, 1]); // cut-1 conn dropped; cut-2 conn → cut-1
}

// ── orbit wheel-gating (U.3.12) — a mouse-wheel over the egui panel must NOT zoom the camera ──────
//
// Regression guard for the scroll-zoom fix: `seam.over_ui` was structurally unreliable over the
// background-layer panels (egui's `is_pointer_over_egui()` reports false there), so `orbit` now gates
// the wheel on the panel RECT (physical-px seam bands + the window cursor). These drive `orbit`
// directly — spawn a window with a set physical cursor, a camera+Orbit, send one wheel notch, read
// the radius.

/// A panel seam: left panel `width` wide, a 40px top bar, a 20px bottom bar, egui not actively used.
fn seam(width: f32) -> PanelSeam {
    PanelSeam {
        over_ui: false,
        width_px: width,
        top_px: 40.0,
        bottom_px: 20.0,
    }
}

/// A headless app with just `orbit`, a window whose PHYSICAL cursor is at `cursor` (physical px,
/// top-left origin; `None` = off-window), and `seam`. Returns the app + the camera entity.
fn orbit_app(cursor: Option<(f32, f32)>, seam: PanelSeam) -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_message::<MouseMotion>()
        .add_message::<MouseWheel>()
        .init_resource::<ButtonInput<MouseButton>>()
        .init_resource::<DraggingCut>()
        .init_resource::<EditCut>()
        .insert_resource(seam)
        .add_systems(Update, orbit);
    let mut window = Window::default(); // default physical res is well larger than the test coords
    if let Some((x, y)) = cursor {
        window.set_physical_cursor_position(Some(DVec2::new(x as f64, y as f64)));
    }
    app.world_mut().spawn(window);
    let cam = app
        .world_mut()
        .spawn((
            Transform::default(),
            Orbit {
                yaw: 0.0,
                pitch: 0.0,
                radius: 100.0,
                target: Vec3::ZERO,
            },
        ))
        .id();
    (app, cam)
}

/// Send one line-wheel notch, step a frame, and return the camera's `Orbit.radius`.
fn wheel_then_radius(app: &mut App, cam: Entity) -> f32 {
    app.world_mut().write_message(MouseWheel {
        unit: MouseScrollUnit::Line,
        x: 0.0,
        y: 1.0,
        window: Entity::PLACEHOLDER, // orbit ignores the source window
        phase: TouchPhase::Moved,    // a real mouse wheel is always Moved
    });
    app.update();
    app.world().entity(cam).get::<Orbit>().unwrap().radius
}

#[test]
fn wheel_over_panel_does_not_zoom() {
    let (mut app, cam) = orbit_app(Some((10.0, 400.0)), seam(200.0)); // cursor in the left panel
    assert_eq!(wheel_then_radius(&mut app, cam), 100.0); // gated → radius untouched
}

#[test]
fn wheel_over_viewport_zooms() {
    let (mut app, cam) = orbit_app(Some((500.0, 400.0)), seam(200.0)); // cursor over the 3D view
    let r = wheel_then_radius(&mut app, cam);
    assert!(
        r < 100.0,
        "wheel over the 3D view should zoom in (radius {r} should be < 100)"
    );
}

#[test]
fn wheel_over_top_bar_does_not_zoom() {
    let (mut app, cam) = orbit_app(Some((500.0, 10.0)), seam(200.0)); // cursor over the top tab bar
    assert_eq!(wheel_then_radius(&mut app, cam), 100.0);
}

#[test]
fn active_egui_drag_gates_the_wheel_anywhere() {
    // `over_ui = true` models an ACTIVE egui drag (scrollbar/text) — gate even out over the 3D view.
    let s = PanelSeam {
        over_ui: true,
        width_px: 200.0,
        top_px: 40.0,
        bottom_px: 20.0,
    };
    let (mut app, cam) = orbit_app(Some((500.0, 400.0)), s);
    assert_eq!(wheel_then_radius(&mut app, cam), 100.0);
}

// ── Reset-to-auto (U.3.15) — wipes the active part's cuts + connectors and re-arms the derive ──────
#[test]
fn reset_to_auto_wipes_cuts_conns_and_rearms_derive() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_message::<PanelCmd>()
        .init_resource::<ActivePart>()
        .insert_resource(Status("x".into()))
        .insert_resource(Parts(vec![{
            let mut p = seeded_part(vec![x_cut(-10.0), x_cut(10.0)]);
            p.conns.list.push(PlacedConn {
                cut: 0,
                pos: [1.0, 1.0],
                size: 6.0,
                kind: fab::ConnKind::Onion,
                screw: Screw::M3,
            });
            p.auto_planned.0 = Some(std::path::PathBuf::from("m.scad")); // pretend already derived
            p
        }]))
        .add_systems(Update, auto_slice_action);
    app.world_mut().write_message(PanelCmd::AutoSlice); // the "Reset to auto" button
    app.update();
    let p = &app.world().resource::<Parts>().0[0];
    assert!(p.cuts.list.is_empty(), "reset clears cuts");
    assert!(p.conns.list.is_empty(), "reset clears connectors");
    assert!(
        p.auto_planned.0.is_none(),
        "reset re-arms kick_auto_plan to re-derive"
    );
}

// ── refresh_bounds_on_reload — a RESIZED part re-decides slicing; a CUT part stays frozen ───────────
#[test]
fn reload_refreshes_bounds_and_rearms_a_cutless_part() {
    // A presliced/whole part (no cuts) that the editor RESIZED: on reload it takes the fresh (bigger)
    // bbox and re-arms auto-plan, so the bed-overflow check judges the NEW solid — not the stale first
    // render. This is the "removed the pre-slices, won't re-slice" dogfood bug: the bbox used to freeze.
    let mut p = seeded_part(vec![]); // cutless (presliced or whole)
    p.auto_planned.0 = Some(std::path::PathBuf::from("m.scad")); // already derived once
    p.pieces = 3; // stale presliced component count
    refresh_bounds_on_reload(&mut p, [-200.0, -50.0, -50.0], [200.0, 50.0, 50.0]);
    assert_eq!(
        p.bounds.0,
        Some((
            Vec3::new(-200.0, -50.0, -50.0),
            Vec3::new(200.0, 50.0, 50.0)
        )),
        "a cutless reload takes the fresh bbox"
    );
    assert!(
        p.auto_planned.0.is_none(),
        "a cutless reload re-arms auto-plan against the new geometry"
    );
    assert_eq!(p.pieces, 0, "the stale presliced count is dropped");
}

#[test]
fn reload_freezes_bounds_of_a_part_with_user_cuts() {
    // A part the user already SLICED keeps its frozen bbox + plan on reload — the cut coords are
    // absolute in that frame, so re-seating the bbox would desync the cut planes from the geometry.
    let mut p = seeded_part(vec![x_cut(0.0)]); // has a user cut
    p.auto_planned.0 = Some(std::path::PathBuf::from("m.scad"));
    refresh_bounds_on_reload(&mut p, [-200.0, -50.0, -50.0], [200.0, 50.0, 50.0]);
    assert_eq!(
        p.bounds.0,
        Some((LO, HI)),
        "a part with cuts keeps its frozen bbox"
    );
    assert!(
        p.auto_planned.0.is_some(),
        "a part with cuts is NOT re-armed — the user owns those cuts"
    );
}

// ── revert_on_edit (U.3.15 flicker fix) — spurious Parts change must NOT collapse an explode ───────
#[test]
fn revert_on_edit_ignores_spurious_change_but_reverts_a_real_edit() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<ActivePart>()
        .insert_resource(Parts(vec![{
            let mut p = seeded_part(vec![x_cut(0.0)]);
            p.whole = Some(Handle::default());
            p.spread = 10.0; // exploded
            p
        }]))
        .add_systems(Update, revert_on_edit);
    app.world_mut()
        .spawn((Mesh3d(Handle::default()), PartId(0), Model));

    app.update(); // establish the slice-input baseline
    // Spuriously mark Parts changed (exactly what panel_ui / kick_auto_plan do every frame) — no edit.
    let _ = app.world_mut().resource_mut::<Parts>();
    app.update();
    assert_eq!(
        app.world().resource::<Parts>().0[0].spread,
        10.0,
        "a spurious Parts change must NOT collapse the explode (the flicker bug)"
    );

    // A real edit — add a cut, moving the slice-input hash.
    app.world_mut().resource_mut::<Parts>().0[0]
        .cuts
        .list
        .push(x_cut(20.0));
    app.update();
    assert_eq!(
        app.world().resource::<Parts>().0[0].spread,
        0.0,
        "a real cut edit reverts the explode to the intact model"
    );
}
