//! W.3.29.6: the Publish dialog — an egui modal that lets you NAME the model (title + description) before
//! it goes live, instead of silently inheriting the folder/filename. Cross-platform (the same modal will
//! drive the web Publish in W.3.29.4); the native Publish button opens it, `publish_native::publish_kick`
//! consumes the confirmed title/description.
//!
//! Flow: the Publish button writes `PanelCmd::Publish` → this system opens the modal (pre-filled from the
//! manifest/filename) → on "Publish" it sets `confirmed` → the publish flow fires off that flag.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};

use crate::PanelCmd;
use crate::state::SceneCfg;
use crate::theme;

/// The Publish dialog's state. `title`/`description` are edited in place; `confirmed` is a one-shot the
/// publish flow drains to start (so the modal owns the fields, the flow just reads them).
#[derive(Resource, Default)]
pub(crate) struct PublishDialog {
    open: bool,
    loaded: bool,
    pub(crate) title: String,
    pub(crate) description: String,
    /// Set for ONE frame when the user hits Publish; `publish_kick` takes it to start.
    pub(crate) confirmed: bool,
}

impl PublishDialog {
    fn request_open(&mut self) {
        self.open = true;
        self.loaded = false;
    }
}

/// Open on `PanelCmd::Publish`, draw the title/description modal (pre-filled once), and raise `confirmed`
/// when the user commits. A title is required (the page slug derives from it).
pub(crate) fn publish_dialog(
    mut contexts: EguiContexts,
    mut ev: MessageReader<PanelCmd>,
    mut dialog: ResMut<PublishDialog>,
    scene: Res<SceneCfg>,
) {
    if ev.read().any(|c| *c == PanelCmd::Publish) {
        dialog.request_open();
    }
    if !dialog.open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Pre-fill once from the manifest/filename (don't stomp in-progress typing).
    if !dialog.loaded {
        let (t, d) = default_meta(&scene);
        dialog.title = t;
        dialog.description = d;
        dialog.loaded = true;
    }

    let mut still_open = true;
    let mut commit = false;
    egui::Modal::new(egui::Id::new("publish_dialog")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.label(theme::chrome("Publish to hotchkiss.io", 18.0).color(theme::NAVY));
        ui.separator();
        egui::Grid::new("publish_dialog_grid")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                ui.label("Title");
                ui.add(
                    egui::TextEdit::singleline(&mut dialog.title)
                        .hint_text("model name")
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label("Description");
                ui.add(
                    egui::TextEdit::multiline(&mut dialog.description)
                        .hint_text("optional — markdown")
                        .desired_rows(3)
                        .desired_width(300.0),
                );
                ui.end_row();
            });
        ui.add_space(10.0);
        let has_title = !dialog.title.trim().is_empty();
        ui.horizontal(|ui| {
            let publish = ui.add_enabled(
                has_title,
                egui::Button::new(theme::chrome("Publish", 14.0).color(theme::NAVY))
                    .fill(theme::GOLD),
            );
            if publish.clicked() {
                commit = true;
            }
            if ui.button("Cancel").clicked() {
                still_open = false;
            }
        });
        if !has_title {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("a title is required — the page address derives from it")
                    .small()
                    .color(theme::GOLD_DIM),
            );
        }
    });
    if commit {
        dialog.confirmed = true;
        still_open = false;
    }
    dialog.open = still_open;
}

/// The pre-filled title + description: the nearest `project.toml` (title + publish.description), else the
/// file stem. On wasm there's no fs manifest — fall back to the source's stem (from `?model=`), blank desc.
fn default_meta(scene: &SceneCfg) -> (String, String) {
    let stem = || {
        scene
            .source
            .as_deref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(src) = scene.source.as_deref()
            && let Ok(m) = fab_scad::manifest::Manifest::load_near(src)
        {
            return (
                m.title().to_string(),
                m.publish.map(|p| p.description).unwrap_or_default(),
            );
        }
        (stem(), String::new())
    }
    #[cfg(target_arch = "wasm32")]
    {
        (stem(), String::new())
    }
}
