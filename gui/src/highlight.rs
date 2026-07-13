//! SCAD syntax highlighting for the Model-tab code editor (U.3.13) — an egui `TextEdit` layouter that
//! lexes the buffer with fab-lang's OWN lexer and paints each token by kind, so the editor's colors
//! track the real grammar (not a regex approximation). A buffer that doesn't lex yet (mid-edit) falls
//! back to plain text. Cheap: the lexer is allocation-free on the token path and SCAD files are small,
//! so re-lexing per layout is fine.

use crate::egui;
use egui::text::{LayoutJob, TextFormat};

/// A run's [`TextFormat`] in `font`/`color` — the only per-token allocation (a `FontId` clone).
fn fmt(font: &egui::FontId, color: egui::Color32) -> TextFormat {
    TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    }
}

/// Build a colored [`LayoutJob`] for `text` in `font`; `default` paints whitespace, operators, and
/// identifiers. Every byte of `text` is covered (gaps between tokens included) so the galley matches
/// the buffer exactly — off-by-one there desyncs the cursor.
pub(crate) fn scad_job(text: &str, font: egui::FontId, default: egui::Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY; // the code editor scrolls, never wraps
    let Ok(lexed) = fab_lang::lex(text) else {
        job.append(text, 0.0, fmt(&font, default)); // doesn't lex yet → plain
        return job;
    };
    let mut last = 0usize;
    for tok in &lexed.all {
        let start = tok.span.start.min(text.len());
        let end = tok.span.end.min(text.len());
        if start >= end {
            continue; // Eof (zero-width) or an out-of-range span
        }
        if start > last {
            job.append(&text[last..start], 0.0, fmt(&font, default)); // whitespace / gaps
        }
        job.append(
            &text[start..end],
            0.0,
            fmt(&font, color_for(&tok.kind, default)),
        );
        last = end;
    }
    if last < text.len() {
        job.append(&text[last..], 0.0, fmt(&font, default));
    }
    job
}

/// A token's editor color (GitHub-light, for the white editor well): keywords red, comments grey,
/// strings + numbers blue, special vars (`$fn`) orange, `use`/`include` purple; everything else
/// (idents, operators, punctuation) the `default` text color (navy).
fn color_for(kind: &fab_lang::TokenKind, default: egui::Color32) -> egui::Color32 {
    use egui::Color32;
    use fab_lang::TokenKind as T;
    // GitHub-light — the editor well is white now, so the old VS Code Dark+ palette would be
    // near-invisible. A functional-coding exception (like the gizmo colors), kept local to this module.
    match kind {
        T::LineComment(_) | T::BlockComment(_) => Color32::from_rgb(110, 119, 129), // #6e7781
        T::Str(_) => Color32::from_rgb(10, 48, 105),                                // #0a3069
        T::Num(_) => Color32::from_rgb(5, 80, 174),                                 // #0550ae
        T::DollarIdent(_) => Color32::from_rgb(149, 56, 0),                         // #953800
        T::Use(_) | T::Include(_) => Color32::from_rgb(130, 80, 223),               // #8250df
        T::Module
        | T::Function
        | T::If
        | T::Else
        | T::Let
        | T::Assert
        | T::Echo
        | T::For
        | T::Each
        | T::True
        | T::False
        | T::Undef => Color32::from_rgb(207, 34, 46), // #cf222e
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn color_at(job: &LayoutJob, byte: usize) -> Color32 {
        job.sections
            .iter()
            .find(|s| s.byte_range.start.0 <= byte && byte < s.byte_range.end.0)
            .unwrap_or_else(|| panic!("no section covers byte {byte}"))
            .format
            .color
    }

    #[test]
    fn colors_keyword_comment_string_and_leaves_idents_default() {
        let text = "module m() { echo(\"hi\"); } // note";
        let job = scad_job(text, egui::FontId::monospace(12.0), Color32::WHITE);
        assert_eq!(color_at(&job, 0), Color32::from_rgb(207, 34, 46)); // `module` → red
        assert_eq!(
            color_at(&job, text.find("echo").unwrap()),
            Color32::from_rgb(207, 34, 46) // `echo` keyword → red
        );
        assert_eq!(
            color_at(&job, text.find('"').unwrap() + 1),
            Color32::from_rgb(10, 48, 105) // inside "hi" → blue
        );
        assert_eq!(
            color_at(&job, text.find("//").unwrap()),
            Color32::from_rgb(110, 119, 129) // comment → grey
        );
        assert_eq!(
            color_at(&job, text.find(" m(").unwrap() + 1),
            Color32::WHITE
        ); // ident `m` → default
    }

    #[test]
    fn unlexable_text_falls_back_to_one_default_run() {
        // An unterminated string won't lex — the whole buffer still renders, plain.
        let text = "cube(\"oops";
        let job = scad_job(text, egui::FontId::monospace(12.0), Color32::WHITE);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].format.color, Color32::WHITE);
    }
}
