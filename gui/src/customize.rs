//! X.2 — the OpenSCAD-style Customizer tab.
//!
//! fab-lang already extracts the metadata ([`fab_lang::customize`] → params + `[…]` widget hints +
//! `/* [Group] */` sections + each value's byte span). This module is the GUI half: render a widget per
//! param, and on change SOURCE-SPLICE the new value into the editor buffer at its `value_span`. The
//! existing debounced `preview_edited_buffer` re-renders from there — so a slider is just a structured
//! editor gesture, and the code editor + the 3D model stay coherent (X.2, decided over a `-D` override
//! seam). Native + web share this verbatim: `customize` is pure fab-lang, and the re-render inherits the
//! editor's native-temp-file / wasm-bytes paths.

use bevy_egui::egui;
use fab_lang::{Constraint, CustomParam, customize};

use crate::state::EditorBuf;

/// Extract the customizer params from the current editor buffer. A parse error just yields no params
/// (no tab) — a half-typed buffer shouldn't blow up. Cheap: `customize` parses only the buffer's
/// top-level statements (syntactic, no include resolution), so calling it per frame is fine.
pub(crate) fn extract(source: &str) -> Vec<CustomParam> {
    customize(source).map(|c| c.params).unwrap_or_default()
}

/// Render the Customize tab: a widget per param grouped by `/* [Group] */`, splicing at most one edit
/// per frame back into `editor.text` at the param's `value_span` (a slider drag emits one change per
/// frame, applied sequentially — spans stay fresh because `sync_customizer` re-parses after each edit).
pub(crate) fn customize_panel(
    ui: &mut egui::Ui,
    params: &[CustomParam],
    editor: &mut EditorBuf,
    now: f64,
) {
    // Snapshot each param's current value text UP FRONT (owned) so the widget closures don't borrow
    // `editor` — the splice below needs it mutably.
    let curs: Vec<String> = params
        .iter()
        .map(|p| editor.text.get(p.value_span.clone()).unwrap_or("").trim().to_string())
        .collect();

    // Cluster params into groups in source order (group = the last `/* [Group] */` before them).
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for (i, p) in params.iter().enumerate() {
        match groups.last_mut() {
            Some((g, idxs)) if *g == p.group => idxs.push(i),
            _ => groups.push((p.group.clone(), vec![i])),
        }
    }

    // (param index, new value source text) — at most one applied per frame.
    let mut pending: Option<(usize, String)> = None;
    let render = |ui: &mut egui::Ui, idxs: &[usize], pending: &mut Option<(usize, String)>| {
        for &i in idxs {
            if let Some(newv) = param_widget(ui, &params[i], &curs[i]) {
                *pending = Some((i, newv));
            }
        }
    };

    ui.add_space(4.0);
    for (group, idxs) in &groups {
        match group {
            Some(name) => {
                egui::CollapsingHeader::new(name)
                    .default_open(true)
                    .show(ui, |ui| render(ui, idxs, &mut pending));
            }
            None => render(ui, idxs, &mut pending),
        }
    }

    // Apply the splice: rewrite just the value slice, then arm the debounced re-render (mirrors the
    // Model-tab editor's dirty/edited_at). `sync_customizer` re-parses next frame → fresh spans.
    if let Some((i, newv)) = pending {
        let span = params[i].value_span.clone();
        if editor.text.get(span.clone()).is_some() {
            editor.text.replace_range(span, &newv);
            editor.dirty = true;
            editor.edited_at = Some(now);
        }
    }
}

/// One param's widget. Returns the new VALUE SOURCE TEXT if the user changed it (else `None`). The
/// widget shape follows the `[…]` constraint; an un-annotated or expression-valued param falls back to
/// a type-inferred widget (never clobber a `30/2`-style expression by forcing it onto a slider).
fn param_widget(ui: &mut egui::Ui, p: &CustomParam, current: &str) -> Option<String> {
    let label = p.description.clone().unwrap_or_else(|| p.name.clone());
    match &p.constraint {
        Some(Constraint::Range { min, step, max }) => {
            // Only a slider when the current value is a plain number — an expression value falls through
            // to editable text so a drag can't overwrite `width/2` with a literal.
            if let Ok(mut v) = current.parse::<f64>() {
                let mut slider = egui::Slider::new(&mut v, *min..=*max).text(&label);
                if let Some(step) = step {
                    slider = slider.step_by(*step);
                }
                return ui.add(slider).changed().then(|| fmt_num(v));
            }
            inferred_widget(ui, &label, current)
        }
        Some(Constraint::Dropdown(items)) => {
            let quoted = current.starts_with('"');
            let cur_val = current.trim_matches('"');
            let shown = items
                .iter()
                .find(|it| it.value == cur_val)
                .map_or(cur_val, |it| it.label.as_deref().unwrap_or(&it.value));
            let mut chosen: Option<String> = None;
            egui::ComboBox::from_label(&label)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for it in items {
                        let text = it.label.as_deref().unwrap_or(&it.value);
                        if ui.selectable_label(it.value == cur_val, text).clicked() {
                            // Preserve the value's syntactic kind: a quoted current ⇒ a quoted write.
                            chosen = Some(if quoted {
                                format!("\"{}\"", it.value)
                            } else {
                                it.value.clone()
                            });
                        }
                    }
                });
            chosen
        }
        Some(Constraint::MaxLength(n)) => {
            let mut s = current.trim_matches('"').to_string();
            let resp = ui
                .horizontal(|ui| {
                    ui.label(&label);
                    ui.add(egui::TextEdit::singleline(&mut s).char_limit(*n as usize))
                })
                .inner;
            resp.changed().then(|| format!("\"{s}\""))
        }
        None => inferred_widget(ui, &label, current),
    }
}

/// Widget inferred from the current value's syntactic kind — `true`/`false` → checkbox, a number →
/// drag field, a `"quoted"` string → text field, anything else (vectors, expressions) → raw text edit
/// of the value expression (so ANY param stays editable, just without a structured widget).
fn inferred_widget(ui: &mut egui::Ui, label: &str, current: &str) -> Option<String> {
    if current == "true" || current == "false" {
        let mut b = current == "true";
        return ui.checkbox(&mut b, label).changed().then(|| b.to_string());
    }
    if let Ok(mut v) = current.parse::<f64>() {
        let resp = ui
            .horizontal(|ui| {
                ui.label(label);
                ui.add(egui::DragValue::new(&mut v).speed(0.1))
            })
            .inner;
        return resp.changed().then(|| fmt_num(v));
    }
    if current.len() >= 2 && current.starts_with('"') && current.ends_with('"') {
        let mut s = current[1..current.len() - 1].to_string();
        let resp = ui
            .horizontal(|ui| {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(&mut s))
            })
            .inner;
        return resp.changed().then(|| format!("\"{s}\""));
    }
    // Fallback: edit the value expression as raw text (vectors, expressions, anything unrecognised).
    let mut s = current.to_string();
    let resp = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(&mut s))
        })
        .inner;
    resp.changed().then_some(s)
}

/// Format a slider/drag value as clean OpenSCAD source: round off float-accumulation noise (6 decimals)
/// then the shortest round-tripping form — so `25` stays `25`, never `25.00000001`.
fn fmt_num(v: f64) -> String {
    let r = (v * 1e6).round() / 1e6;
    format!("{r}")
}

#[cfg(test)]
mod tests {
    use super::fmt_num;

    #[test]
    fn fmt_num_is_clean() {
        assert_eq!(fmt_num(25.0), "25");
        assert_eq!(fmt_num(25.5), "25.5");
        assert_eq!(fmt_num(0.1 + 0.2), "0.3"); // the classic float-noise case
        assert_eq!(fmt_num(-3.0), "-3");
    }
}
