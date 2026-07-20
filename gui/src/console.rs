//! The in-app CONSOLE (W.3.16) — a bottom-panel expander that surfaces output there's nowhere else to
//! see on WEB (no terminal). Two feeds land in ONE capped ring buffer, tagged by kind:
//!   - [`Kind::Scad`]: the model's own `echo(...)` + warnings + render errors — plumbed back from the
//!     evaluator in the geom `Response` (works native AND web; the worker returns them).
//!   - [`Kind::Log`]: the app's `tracing` stream (a `LogPlugin` custom layer) — the "Full" extra.
//!
//! The panel's [Full | SCAD] toggle filters between the two.
//!
//! The buffer is a process GLOBAL (`OnceLock<Arc<Mutex<…>>>`), not a Bevy resource, because the tracing
//! layer runs OUTSIDE the ECS and off arbitrary threads (the native geom pool logs too) — it needs a
//! home the layer and the UI can both reach. Only the UI STATE ([`ConsoleUi`]) is a resource.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use bevy::log::BoxedLayer;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing_subscriber::Layer;
use bevy::prelude::*;
use bevy_egui::egui;

/// Ring-buffer cap — old lines drop off the front. A console, not a log file; a few thousand is plenty.
const CAP: usize = 4000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `echo` / warnings / render errors from the model — the OpenSCAD-side console.
    Scad,
    /// The app's `tracing` events — shown only in "Full" mode.
    Log,
}

struct Line {
    kind: Kind,
    /// Severity for Log lines; Scad + worker lines are pushed at INFO (always visible). The console's
    /// level dropdown (W.3.23) hides Log lines more verbose than the picked level.
    level: bevy::log::Level,
    text: String,
}

static CONSOLE: OnceLock<Arc<Mutex<VecDeque<Line>>>> = OnceLock::new();

fn buffer() -> &'static Arc<Mutex<VecDeque<Line>>> {
    CONSOLE.get_or_init(|| Arc::new(Mutex::new(VecDeque::new())))
}

/// Append a line at `level`, dropping the oldest past [`CAP`]. Poison-tolerant (a panicked writer must
/// not wedge the console for the rest of the session).
fn push_line(kind: Kind, level: bevy::log::Level, text: String) {
    let mut b = buffer().lock().unwrap_or_else(|e| e.into_inner());
    b.push_back(Line { kind, level, text });
    while b.len() > CAP {
        b.pop_front();
    }
}

/// Append a line at INFO severity — the SCAD echo/warning feed + the worker's forwarded logs, always
/// visible regardless of the level dropdown.
pub(crate) fn push(kind: Kind, text: impl Into<String>) {
    push_line(kind, bevy::log::Level::INFO, text.into());
}

/// Push each rendered eval message (already `ECHO: …` / `WARNING: …`) as a SCAD line.
pub(crate) fn push_scad_messages(messages: &[String]) {
    for m in messages {
        push(Kind::Scad, m.clone());
    }
}

fn clear() {
    buffer().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

// ── tracing capture (the "Full" feed) ─────────────────────────────────────────────────────────────

/// Pulls the `message` field out of a `tracing` event (the text of `info!("…")`), ignoring structured
/// fields — a console line, not a structured record.
struct MessageGrab(String);
impl Visit for MessageGrab {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// A `tracing` layer that mirrors each event into the console buffer as a [`Kind::Log`] line —
/// `LEVEL target: message`. Added via Bevy's `LogPlugin::custom_layer`, so it sees exactly the events
/// the app's log filter already admits (i.e. what the terminal would show on native).
struct ConsoleLayer;
impl<S: bevy::log::tracing::Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(
        &self,
        event: &bevy::log::tracing::Event<'_>,
        _ctx: bevy::log::tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut grab = MessageGrab(String::new());
        event.record(&mut grab);
        if grab.0.is_empty() {
            return;
        }
        let meta = event.metadata();
        push_line(
            Kind::Log,
            *meta.level(),
            format!("{} {}: {}", meta.level(), meta.target(), grab.0),
        );
    }
}

/// The `LogPlugin::custom_layer` hook — installs [`ConsoleLayer`] alongside Bevy's own log layer.
pub(crate) fn log_layer(_app: &mut App) -> Option<BoxedLayer> {
    Some(Box::new(ConsoleLayer))
}

// ── UI ────────────────────────────────────────────────────────────────────────────────────────────

/// The console's UI STATE (a resource): collapsed by default; `full` = show `tracing` too, else SCAD
/// only; `level` = the least-severe tracing line shown in Full mode (W.3.23 — INFO by default, crank to
/// DEBUG to diagnose).
#[derive(Resource)]
pub(crate) struct ConsoleUi {
    pub(crate) expanded: bool,
    pub(crate) full: bool,
    pub(crate) level: bevy::log::Level,
}

impl Default for ConsoleUi {
    fn default() -> Self {
        Self {
            expanded: false,
            full: false,
            level: bevy::log::Level::INFO,
        }
    }
}

/// Short label for the level dropdown (tracing's `Display` is SHOUTY: "ERROR" etc).
fn level_label(level: bevy::log::Level) -> &'static str {
    match level {
        bevy::log::Level::ERROR => "Error",
        bevy::log::Level::WARN => "Warn",
        bevy::log::Level::INFO => "Info",
        bevy::log::Level::DEBUG => "Debug",
        bevy::log::Level::TRACE => "Trace",
    }
}

/// Right-aligned status-bar controls. Collapsed: a "Console" button. Expanded: the [Full|SCAD] toggle +
/// Clear + Collapse. ASCII labels only (the egui font stack tofus stray glyphs — see gui/CLAUDE.md).
pub(crate) fn controls(ui: &mut egui::Ui, state: &mut ConsoleUi) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if state.expanded {
            if ui.button("Collapse").clicked() {
                state.expanded = false;
            }
            if ui.button("Clear").clicked() {
                clear();
            }
            ui.separator();
            ui.selectable_value(&mut state.full, false, "SCAD");
            ui.selectable_value(&mut state.full, true, "Full");
            // W.3.23: in Full mode, pick the least-severe tracing line to show (INFO default → DEBUG).
            if state.full {
                egui::ComboBox::from_id_salt("console_level")
                    .selected_text(level_label(state.level))
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for lvl in [
                            bevy::log::Level::ERROR,
                            bevy::log::Level::WARN,
                            bevy::log::Level::INFO,
                            bevy::log::Level::DEBUG,
                        ] {
                            ui.selectable_value(&mut state.level, lvl, level_label(lvl));
                        }
                    });
                ui.separator();
            }
        } else if ui.button("Console").clicked() {
            state.expanded = true;
        }
    });
}

/// The scrolling line list (drawn below the status row when expanded). Sticks to the bottom so the
/// newest output is always in view; monospace; warnings + errors gold, `tracing` muted.
pub(crate) fn log_view(ui: &mut egui::Ui, full: bool, level: bevy::log::Level) {
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let b = buffer().lock().unwrap_or_else(|e| e.into_inner());
            let mut any = false;
            // SCAD echo/warnings always show; tracing lines only in Full mode, at/above the picked
            // severity (Level Ord is ERROR < WARN < INFO < DEBUG, so `<=` keeps this level + worse).
            for line in b.iter().filter(|l| match l.kind {
                Kind::Scad => true,
                Kind::Log => full && l.level <= level,
            }) {
                any = true;
                let color = match line.kind {
                    Kind::Log => crate::theme::TEXT_MUTED,
                    Kind::Scad
                        if line.text.starts_with("WARNING")
                            || line.text.starts_with("render error") =>
                    {
                        crate::theme::GOLD_DIM
                    }
                    Kind::Scad => ui.visuals().text_color(),
                };
                ui.label(egui::RichText::new(&line.text).monospace().color(color));
            }
            if !any {
                ui.label(
                    egui::RichText::new(if full {
                        "no output yet"
                    } else {
                        "no echo / warnings yet — echo(…) in your model prints here"
                    })
                    .italics()
                    .color(crate::theme::TEXT_MUTED),
                );
            }
        });
}
