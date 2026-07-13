//! The fab-gui visual theme (W.1) — ported from the hotchkiss.io identity.
//!
//! hotchkiss.io is a warm, collegiate, *light* varsity-print look: navy + gold on a soft grey field,
//! FLAT and heavily BORDERED (never shadowed), Oswald uppercase for chrome, a serif accent voice. We
//! render that as a single light egui context (div-grey panel cards, white input/editor wells, heavy
//! navy borders) wrapping a near-black-navy 3D well where the gold model sits — the site's dark
//! "image-well" motif, in 3D.
//!
//! THE GOLD LAW: `GOLD` is load-bearing but NEVER text on a light surface — only a fill / pill / rule /
//! ring, always paired with navy (or navy text on a gold fill). Emphasis is color + rule, never weight:
//! Oswald ships 400 only, so chrome labels must not carry `.strong()`.
//!
//! Everything here is a tint/shade of the five site tokens (navy / gold / greys / white) plus the two
//! danger reds. One central [`install_theme`] system sets the egui `Visuals` + `Style` + fonts once; the
//! 3D constants are consumed by the Bevy scene/print/cuts modules.

use crate::*;
use egui::Color32;
use egui::style::WidgetVisuals;
use std::sync::Arc;

// ---- egui palette (Color32) --------------------------------------------------------------------

/// #14213d — primary text, borders, dark pills/fills.
pub(crate) const NAVY: Color32 = Color32::from_rgb(20, 33, 61);
/// #ffc935 — THE accent: selection, focus, CTA fills, active-tab text/rule. Never text on light.
pub(crate) const GOLD: Color32 = Color32::from_rgb(255, 201, 53);
/// #b8860b — the accent DIMMED enough to be legible AS text on light: stale / unsaved / auto / warn.
pub(crate) const GOLD_DIM: Color32 = Color32::from_rgb(184, 134, 11);
/// #f5f5f5 — the page grey (behind panels where one shows through).
#[allow(dead_code)] // palette completeness; the "page" in this layout is the 3D viewport, not an egui fill
pub(crate) const BODY_GREY: Color32 = Color32::from_rgb(245, 245, 245);
/// #e5e5e5 — the panel/bar card surface (also the light text color on navy).
pub(crate) const DIV_GREY: Color32 = Color32::from_rgb(229, 229, 229);
/// #ffffff — input / editor / menu wells.
pub(crate) const WHITE: Color32 = Color32::from_rgb(255, 255, 255);
/// striped / alt-row wash.
pub(crate) const FAINT_BG: Color32 = Color32::from_rgb(237, 238, 240);
/// #5b6377 — navy @ ~65%: muted / caption text.
pub(crate) const TEXT_MUTED: Color32 = Color32::from_rgb(91, 99, 119);
/// #14213d @ 20% — hairline separators / indent vline.
pub(crate) const BORDER_SUBTLE: Color32 = Color32::from_rgba_unmultiplied_const(20, 33, 61, 51);
/// #dc2626 — error / destructive text.
pub(crate) const DANGER: Color32 = Color32::from_rgb(220, 38, 38);
/// translucent gold (α120) — selection background (reads as a soft gold badge; lets text under it show).
pub(crate) const SEL_FILL: Color32 = Color32::from_rgba_unmultiplied_const(255, 201, 53, 120);

// ---- Bevy 3D viewport palette (srgb) -----------------------------------------------------------

/// #0f182d — near-black-navy viewport well (the site's dark image-well, in 3D).
pub(crate) const VIEWPORT: Color = Color::srgb(0.059, 0.094, 0.176);
/// #ffc935 — the brand-gold model/part material.
pub(crate) const MODEL_GOLD: Color = Color::srgb(1.0, 0.788, 0.208);
/// #2d364e — navy-slate bed/plate slab.
pub(crate) const BED_SLATE: Color = Color::srgb(0.176, 0.212, 0.306);
/// #e5e5e5 — div-grey bolt connector marker (amber vanished on the gold model; div-grey reads on both
/// the model and the navy well and stays distinct from the teal onion / red downgrade markers).
pub(crate) const BOLT_MARKER: Color = Color::srgb(0.898, 0.898, 0.898);

// ---- fonts (W.1.4) -----------------------------------------------------------------------------

/// An Oswald (condensed, chrome) [`egui::FontId`] at `size`.
pub(crate) fn oswald(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("oswald".into()))
}

/// A Quattrocento (serif accent voice) [`egui::FontId`] at `size`.
pub(crate) fn quattro(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("quattrocento".into()))
}

/// An UPPERCASE Oswald chrome label. egui has no `text-transform`, so we uppercase the string here —
/// the site's chrome (tabs, wordmark, headers, button captions) is always caps.
pub(crate) fn chrome(text: impl AsRef<str>, size: f32) -> egui::RichText {
    egui::RichText::new(text.as_ref().to_uppercase()).font(oswald(size))
}

/// Register the three committed subsets: Oswald + Quattrocento as named families, Material Symbols as a
/// lowest-priority fallback on every family (so icon glyphs resolve inside Oswald caps labels too).
fn install_ui_fonts(ctx: &egui::Context) {
    const MS: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/MaterialSymbols-subset.ttf"
    ));
    const OSWALD: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/Oswald-subset.ttf"
    ));
    const QUATTRO: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/Quattrocento-subset.ttf"
    ));
    let mut defs = egui::FontDefinitions::default(); // keeps Ubuntu-Light=Proportional, Hack=Monospace
    defs.font_data
        .insert("material-symbols".into(), Arc::new(egui::FontData::from_static(MS)));
    defs.font_data
        .insert("oswald".into(), Arc::new(egui::FontData::from_static(OSWALD)));
    defs.font_data
        .insert("quattrocento".into(), Arc::new(egui::FontData::from_static(QUATTRO)));

    // Icon subset: lowest-priority fallback on the built-in families.
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        defs.families.entry(fam).or_default().push("material-symbols".into());
    }
    // Oswald / Quattrocento: primary, then the full Proportional fallback chain (Ubuntu-Light + emoji +
    // the icon subset we just appended) so any glyph they lack still resolves rather than tofu-ing.
    let prop = defs.families[&egui::FontFamily::Proportional].clone();
    let mut oswald = vec!["oswald".to_string()];
    oswald.extend(prop.clone());
    defs.families.insert(egui::FontFamily::Name("oswald".into()), oswald);
    let mut quattro = vec!["quattrocento".to_string()];
    quattro.extend(prop);
    defs.families.insert(egui::FontFamily::Name("quattrocento".into()), quattro);

    ctx.set_fonts(defs);
}

/// Build the brand `Visuals` from the light base: light wells, heavy navy borders, gold accent, flat
/// (no shadows). Widget states carry the site's button language — inactive = white outline, hover fills
/// navy w/ gold ring, press fills gold.
fn brand_visuals() -> egui::Visuals {
    let r4 = egui::CornerRadius::same(4);
    let wv = |bg: Color32, weak: Color32, bs: egui::Stroke, fs: egui::Stroke, exp: f32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: weak,
        bg_stroke: bs,
        fg_stroke: fs,
        corner_radius: r4,
        expansion: exp,
    };
    let mut v = egui::Visuals::light();
    v.dark_mode = false;
    v.override_text_color = None; // per-widget fg_stroke carries navy, so hover-gold can differ
    v.window_fill = WHITE;
    v.panel_fill = DIV_GREY;
    v.faint_bg_color = FAINT_BG;
    v.extreme_bg_color = WHITE; // TextEdit / DragValue well
    v.code_bg_color = WHITE; // code editor well

    v.widgets.noninteractive = wv(
        DIV_GREY,
        DIV_GREY,
        egui::Stroke::new(1.0, BORDER_SUBTLE),
        egui::Stroke::new(1.0, NAVY),
        0.0,
    );
    v.widgets.inactive = wv(
        WHITE,
        WHITE,
        egui::Stroke::new(1.0, NAVY),
        egui::Stroke::new(1.0, NAVY),
        0.0,
    );
    v.widgets.hovered = wv(
        NAVY,
        NAVY,
        egui::Stroke::new(2.0, GOLD),
        egui::Stroke::new(1.0, GOLD),
        1.0,
    );
    v.widgets.active = wv(
        GOLD,
        GOLD,
        egui::Stroke::new(2.0, NAVY),
        egui::Stroke::new(1.0, NAVY),
        1.0,
    );
    v.widgets.open = wv(
        DIV_GREY,
        DIV_GREY,
        egui::Stroke::new(1.0, NAVY),
        egui::Stroke::new(1.0, NAVY),
        0.0,
    );

    v.selection.bg_fill = SEL_FILL;
    // Selected TEXT color (egui uses this for selectable_label / selected buttons — NONE renders the
    // text transparent). Navy reads on the gold selection pill and on the light panel.
    v.selection.stroke = egui::Stroke::new(1.0, NAVY);
    v.hyperlink_color = NAVY; // gold text on light is banned by the law
    v.warn_fg_color = GOLD_DIM;
    v.error_fg_color = DANGER;
    v.text_cursor.stroke = egui::Stroke::new(2.0, NAVY);

    v.window_stroke = egui::Stroke::new(2.0, NAVY);
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(6);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE; // shadows OFF entirely — depth is borders
    v.button_frame = true;
    v.collapsing_header_frame = false; // flat Parts headers (accent = the left-rule, not a frame)
    v.indent_has_left_vline = true;
    v.striped = false;
    v.slider_trailing_fill = true;
    v
}

/// Whether the theme is installed AND its fonts are live. `set_fonts` only takes effect at the START of
/// the NEXT egui pass, so laying out the named Oswald/Quattrocento families in the SAME pass that sets
/// them is a hard panic (not a fallback). `panel_ui` gates on this so it never draws too early.
#[derive(Resource, Default)]
pub(crate) struct ThemeReady(pub bool);

/// Run condition gating a system until [`ThemeReady`].
pub(crate) fn theme_ready(ready: Res<ThemeReady>) -> bool {
    ready.0
}

/// Install the theme (fonts + visuals + spacing). TWO-PHASE, because `set_fonts` applies at the start of
/// the next pass: phase 0 queues the fonts + style; phase 1 (next pass, fonts now bound) flips
/// [`ThemeReady`] to release `panel_ui`. The built style persists in the egui context. Replaces the old
/// icon-only `install_fonts`.
pub(crate) fn install_theme(
    mut contexts: EguiContexts,
    mut phase: Local<u8>,
    mut ready: ResMut<ThemeReady>,
) {
    if *phase >= 2 {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return; // context not up yet — retry next pass (panel_ui stays gated off until we're ready)
    };
    if *phase == 0 {
        install_ui_fonts(ctx);
        let visuals = brand_visuals();
        // Apply to BOTH theme variants so the system light/dark preference can't flip us off-brand.
        ctx.all_styles_mut(move |style| {
            style.visuals = visuals.clone();
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.window_margin = egui::Margin::same(8);
            style.spacing.menu_margin = egui::Margin::same(8);
            style.spacing.indent = 16.0;
        });
        *phase = 1;
    } else {
        ready.0 = true; // the queued fonts are bound this pass — release panel_ui
        *phase = 2;
    }
}
