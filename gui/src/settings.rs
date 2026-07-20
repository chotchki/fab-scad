//! W.3.27.2: the desktop Settings screen — an egui modal (the header gear) that reads/writes the
//! hotchkiss.io publish credential ([`fab_scad::credentials`]). It exists so a double-clicked `.app` can
//! be handed a key WITHOUT the terminal, env vars, or hand-editing a dotfile — the gap that made the
//! Publish button feel dead. Native only: the web publishes via the site session cookie, no key to set.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};
use fab_scad::credentials::{self, Credentials, KeySource};

use crate::PanelCmd;
use crate::theme;

/// The Settings modal's UI state. `open` is raised by the header gear (via a [`PanelCmd::OpenSettings`]
/// message); the field buffers are (re)loaded from the saved file each time it opens (`loaded` latches
/// that one-shot so typing isn't clobbered every frame).
#[derive(Resource, Default)]
pub(crate) struct SettingsUi {
    open: bool,
    loaded: bool,
    key_input: String,
    url_input: String,
    /// The last Save outcome — `Ok(path)` in navy, `Err(msg)` in gold — shown under the buttons.
    msg: Option<Result<String, String>>,
}

impl SettingsUi {
    /// Raise the modal from elsewhere — a keyless Publish attempt pops it open (the loud cue). Reloads
    /// from disk on the next draw (`loaded` cleared), so it shows the current saved state.
    pub(crate) fn request_open(&mut self) {
        self.open = true;
        self.loaded = false;
    }
}

/// Draw the Settings modal + service the open request. Prefills from [`credentials::load_file`] on the
/// open transition, saves on click, and reports the ACTIVE key source (env beats file — an env key can't
/// be edited here, so the field disables and says so). Registered desktop-only.
pub(crate) fn settings_modal(
    mut contexts: EguiContexts,
    mut ev: MessageReader<PanelCmd>,
    mut state: ResMut<SettingsUi>,
) {
    // The gear raises the modal; reloading from disk on the next draw (loaded=false).
    if ev.read().any(|c| *c == PanelCmd::OpenSettings) {
        state.open = true;
        state.loaded = false;
    }
    if !state.open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // One-shot prefill from the saved file when the modal opens (don't stomp in-progress typing).
    if !state.loaded {
        let file = credentials::load_file();
        state.key_input = file.hio_api_key.unwrap_or_default();
        state.url_input = file.hio_url.unwrap_or_default();
        state.msg = None;
        state.loaded = true;
    }

    // The live resolution — env WINS over the form, so an env key locks the field (editing it is moot).
    let resolved = credentials::resolve();
    let env_locked = resolved.key_source == KeySource::Env;

    let mut still_open = true;
    let modal = egui::Modal::new(egui::Id::new("settings_modal")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(theme::chrome("Settings", 18.0).color(theme::NAVY));
        ui.separator();
        ui.label(
            egui::RichText::new("hotchkiss.io publish")
                .strong()
                .color(theme::NAVY),
        );
        ui.add_space(2.0);

        // WHY publish can or can't proceed right now — the legibility the dead button lacked.
        let (src_txt, src_col) = match resolved.key_source {
            KeySource::Env => (
                "key: $HIO_API_KEY is set — the environment overrides this form".to_string(),
                theme::TEXT_MUTED,
            ),
            KeySource::File => ("key: saved on this machine".to_string(), theme::NAVY),
            KeySource::Unset => (
                "key: not set — Publish stays disabled until you save one".to_string(),
                theme::GOLD_DIM,
            ),
        };
        ui.label(egui::RichText::new(src_txt).small().color(src_col));
        ui.add_space(6.0);

        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                ui.label("API key");
                let key_field = egui::TextEdit::singleline(&mut state.key_input)
                    .password(true)
                    .hint_text("hio_…")
                    .desired_width(240.0);
                ui.add_enabled(!env_locked, key_field);
                ui.end_row();

                ui.label("Base URL");
                ui.add(
                    egui::TextEdit::singleline(&mut state.url_input)
                        .hint_text(credentials::DEFAULT_URL)
                        .desired_width(240.0),
                );
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let save = ui.add(
                egui::Button::new(theme::chrome("Save", 14.0).color(theme::NAVY)).fill(theme::GOLD),
            );
            if save.clicked() {
                let creds = Credentials {
                    hio_api_key: Some(state.key_input.clone()),
                    hio_url: Some(state.url_input.clone()),
                };
                state.msg = Some(match credentials::save_file(&creds) {
                    Ok(p) => Ok(format!("saved -> {}", p.display())),
                    Err(e) => Err(format!("{e:#}")),
                });
            }
            if ui.button("Close").clicked() {
                still_open = false;
            }
        });

        if let Some(msg) = &state.msg {
            ui.add_space(6.0);
            match msg {
                Ok(t) => ui.label(egui::RichText::new(t).small().color(theme::NAVY)),
                Err(e) => ui.label(
                    egui::RichText::new(format!("save failed: {e}"))
                        .small()
                        .color(theme::GOLD_DIM),
                ),
            };
        }
    });
    // A backdrop click or Esc closes it.
    if modal.should_close() {
        still_open = false;
    }
    state.open = still_open;
}
