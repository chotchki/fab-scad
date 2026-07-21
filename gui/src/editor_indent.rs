//! W.3.35: Tab / Shift+Tab re-indents the Model-tab editor's selection. The PURE text transform lives here
//! (unit-tested in char space, no egui); `panel.rs` wires it to the `TextEdit`'s cursor state — it reads the
//! selection as egui `CCursor` CHAR offsets, calls [`reindent`], and writes the returned selection back.

/// One indent level: 4 spaces. The models + scad-lib are space-indented, so egui's `.code_editor()` default
/// (`\t`) reads inconsistent against them — Tab in fab-gui inserts spaces to match the source it edits.
pub(crate) const INDENT: &str = "    ";

/// Re-indent the block of lines a `[start, end)` CHAR selection touches, returning `(new_text, new_start,
/// new_end)` — all CHAR offsets (egui's `CCursor` counts characters, not bytes).
///
/// - A NON-empty selection is a block op: every line it spans gains one [`INDENT`] (`dedent=false`) or loses
///   up to one leading indent — a `\t` or up to `INDENT.len()` spaces (`dedent=true`). Blank lines are left
///   alone on indent. The returned selection covers the whole re-indented block, so repeated Tab keeps
///   working on it (the common editor behaviour).
/// - An EMPTY selection is a caret op: Tab inserts one [`INDENT`] at the caret; Shift+Tab strips the current
///   line's leading indent and pulls the caret left by what it removed.
pub(crate) fn reindent(
    text: &str,
    start: usize,
    end: usize,
    dedent: bool,
) -> (String, usize, usize) {
    let (lo, hi) = (start.min(end), start.max(end));
    if lo == hi {
        reindent_caret(text, lo, dedent)
    } else {
        reindent_block(text, lo, hi, dedent)
    }
}

/// Byte offset of the `char_idx`-th character (clamped to the end). `pub(crate)` so the bracket matcher
/// (W.3.38) shares this one char→byte crossing — egui hands CHAR carets, the lexer + text are byte-native.
pub(crate) fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Leading indent to strip from `line` on a dedent: one `\t`, else up to `INDENT.len()` spaces. Leading
/// whitespace is ASCII, so this count is both the char AND byte length of the prefix.
fn dedent_len(line: &str) -> usize {
    if line.starts_with('\t') {
        1
    } else {
        line.bytes()
            .take(INDENT.len())
            .take_while(|&b| b == b' ')
            .count()
    }
}

/// Caret op (empty selection): Tab inserts an indent, Shift+Tab strips the current line's leading indent.
fn reindent_caret(text: &str, caret: usize, dedent: bool) -> (String, usize, usize) {
    let cb = char_to_byte(text, caret);
    if !dedent {
        let mut out = String::with_capacity(text.len() + INDENT.len());
        out.push_str(&text[..cb]);
        out.push_str(INDENT);
        out.push_str(&text[cb..]);
        let c = caret + INDENT.chars().count();
        return (out, c, c);
    }
    // Dedent: strip the current line's leading indent; the caret slides left by whatever sat before it.
    let line_start = text[..cb].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[cb..].find('\n').map(|i| cb + i).unwrap_or(text.len());
    let strip = dedent_len(&text[line_start..line_end]);
    if strip == 0 {
        return (text.to_string(), caret, caret);
    }
    let mut out = String::with_capacity(text.len() - strip);
    out.push_str(&text[..line_start]);
    out.push_str(&text[line_start + strip..]);
    // The stripped chars are ASCII, so `strip` is a char count too. Pull the caret left by however many
    // of them sat before it (a caret inside the indent lands at the new line start).
    let caret_in_line = caret - text[..line_start].chars().count();
    let removed_before = caret_in_line.min(strip);
    (out, caret - removed_before, caret - removed_before)
}

/// Block op (non-empty selection): re-indent every full line the selection spans; return the whole block
/// selected.
fn reindent_block(text: &str, lo: usize, hi: usize, dedent: bool) -> (String, usize, usize) {
    let lo_b = char_to_byte(text, lo);
    let hi_b = char_to_byte(text, hi);
    let block_start = text[..lo_b].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // A selection ending exactly at a line start (right after a `\n`) shouldn't drag in the next line.
    let scan_from = if hi_b > 0 && text.as_bytes()[hi_b - 1] == b'\n' {
        hi_b - 1
    } else {
        hi_b
    };
    let block_end = text[scan_from..]
        .find('\n')
        .map(|i| scan_from + i)
        .unwrap_or(text.len());

    let block = &text[block_start..block_end];
    let mut new_block = String::with_capacity(block.len() + 16);
    for (i, line) in block.split('\n').enumerate() {
        if i > 0 {
            new_block.push('\n');
        }
        if dedent {
            new_block.push_str(&line[dedent_len(line)..]);
        } else if line.is_empty() {
            // leave blank lines blank — no trailing-whitespace noise
        } else {
            new_block.push_str(INDENT);
            new_block.push_str(line);
        }
    }

    let mut out = String::with_capacity(text.len() + 16);
    out.push_str(&text[..block_start]);
    out.push_str(&new_block);
    out.push_str(&text[block_end..]);

    let block_start_char = text[..block_start].chars().count();
    let new_sel_end = block_start_char + new_block.chars().count();
    (out, block_start_char, new_sel_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_multi_line_selection_prepends_to_each_line() {
        let text = "a();\nb();\nc();\n";
        // select all of line 0 + line 1 (chars 0..9 = "a();\nb();")
        let (out, s, e) = reindent(text, 0, 9, false);
        assert_eq!(out, "    a();\n    b();\nc();\n");
        // whole re-indented block selected: "    a();\n    b();" = 17 chars from 0
        assert_eq!((s, e), (0, 17));
    }

    #[test]
    fn dedent_multi_line_strips_one_level() {
        let text = "    a();\n    b();\nc();\n";
        let (out, ..) = reindent(text, 0, 13, true);
        assert_eq!(out, "a();\nb();\nc();\n");
    }

    #[test]
    fn dedent_strips_partial_and_tab() {
        assert_eq!(reindent("  x\n", 0, 3, true).0, "x\n"); // 2 spaces -> gone
        assert_eq!(reindent("\tx\n", 0, 2, true).0, "x\n"); // a leading tab -> gone
        assert_eq!(reindent("      x\n", 0, 7, true).0, "  x\n"); // 6 spaces -> strip 4, keep 2
    }

    #[test]
    fn empty_selection_tab_inserts_indent_at_caret() {
        let (out, s, e) = reindent("ab\n", 1, 1, false);
        assert_eq!(out, "a    b\n");
        assert_eq!((s, e), (5, 5)); // caret after the 4 inserted spaces
    }

    #[test]
    fn empty_selection_shift_tab_strips_current_line() {
        // caret at char 6 (on the 'x'), line is "    x"
        let (out, s, e) = reindent("    x\ny\n", 5, 5, true);
        assert_eq!(out, "x\ny\n");
        assert_eq!((s, e), (1, 1)); // caret slid left by the 4 removed spaces
    }

    #[test]
    fn selection_ending_at_line_start_does_not_grab_next_line() {
        // "a();\n" then select 0..5 (through the newline). Only line 0 re-indents.
        let (out, ..) = reindent("a();\nb();\n", 0, 5, false);
        assert_eq!(out, "    a();\nb();\n");
    }

    #[test]
    fn indent_leaves_blank_lines_blank() {
        let (out, ..) = reindent("a();\n\nb();\n", 0, 10, false);
        assert_eq!(out, "    a();\n\n    b();\n");
    }
}
