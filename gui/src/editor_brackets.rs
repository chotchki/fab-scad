//! W.3.38: bracket matching for the Model-tab editor. The PURE matcher lives here (unit-tested, no egui);
//! `panel.rs` reads the caret from the `TextEdit` state, calls [`match_bracket`], and `highlight.rs` paints
//! a background under the two matched bracket glyphs. Token-based off `fab_lang::lex`, so a bracket inside a
//! string or comment is never a bracket token → matching skips it for free.

/// Bracket family — `(`/`)`, `[`/`]`, `{`/`}` each pair only within their own family.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Paren,
    Bracket,
    Brace,
}

/// A bracket token reduced to what the matcher needs: its byte position, family, and open/close.
struct Br {
    pos: usize,
    family: Family,
    open: bool,
}

/// Classify a lexer token as a bracket (family + open/close), or `None` for anything else.
fn classify(kind: &fab_lang::TokenKind) -> Option<(Family, bool)> {
    use fab_lang::TokenKind::{LBrace, LBracket, LParen, RBrace, RBracket, RParen};
    match kind {
        LParen => Some((Family::Paren, true)),
        RParen => Some((Family::Paren, false)),
        LBracket => Some((Family::Bracket, true)),
        RBracket => Some((Family::Bracket, false)),
        LBrace => Some((Family::Brace, true)),
        RBrace => Some((Family::Brace, false)),
        _ => None,
    }
}

/// The two BYTE offsets of the bracket adjacent to `caret_byte` and its balanced partner, or `None` when the
/// caret isn't touching a bracket or the bracket is unbalanced. Prefers the bracket ENDING at the caret (the
/// one just to its left) over the one starting at it — the common editor convention. Brackets are single
/// ASCII bytes, so each returned offset is the exact position of one glyph.
pub(crate) fn match_bracket(text: &str, caret_byte: usize) -> Option<(usize, usize)> {
    let lexed = fab_lang::lex(text).ok()?;
    let brs: Vec<Br> = lexed
        .all
        .iter()
        .filter_map(|t| {
            classify(&t.kind).map(|(family, open)| Br {
                pos: t.span.start,
                family,
                open,
            })
        })
        .collect();

    // Pair the brackets with a stack (strict, type-matched nesting). A mismatched closer just stays unpaired
    // — no highlight, never a wrong one.
    let mut partner: Vec<Option<usize>> = vec![None; brs.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, b) in brs.iter().enumerate() {
        if b.open {
            stack.push(i);
        } else if let Some(&j) = stack.last()
            && brs[j].family == b.family
        {
            stack.pop();
            partner[i] = Some(j);
            partner[j] = Some(i);
        }
    }

    // The caret touches a bracket ENDING at it (pos+1 == caret, just left) or STARTING at it (pos == caret,
    // just right). Left wins.
    let hit = brs
        .iter()
        .position(|b| b.pos + 1 == caret_byte)
        .or_else(|| brs.iter().position(|b| b.pos == caret_byte))?;
    let mate = partner[hit]?;
    let (a, b) = (brs[hit].pos, brs[mate].pos);
    Some((a.min(b), a.max(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_after_open_matches_close() {
        // "(a)" — caret at byte 1 (just after `(`).
        assert_eq!(match_bracket("(a)", 1), Some((0, 2)));
    }

    #[test]
    fn caret_after_close_matches_open() {
        // "(a)" — caret at byte 3 (just after `)`).
        assert_eq!(match_bracket("(a)", 3), Some((0, 2)));
    }

    #[test]
    fn nested_families_skip_the_inner_balanced_pair() {
        // "f([1])": f=0 (=1 [=2 1=3 ]=4 )=5. Caret after the `(` (byte 2) matches its `)` at 5, over the
        // balanced `[...]`.
        assert_eq!(match_bracket("f([1])", 2), Some((1, 5)));
        // Caret after the `[` (byte 3) matches its `]` at 4.
        assert_eq!(match_bracket("f([1])", 3), Some((2, 4)));
    }

    #[test]
    fn brackets_inside_a_string_are_not_matched() {
        // The `(` inside the string is a string token, not a bracket — the trailing `)` has no partner.
        assert_eq!(match_bracket("\"(\" )", 5), None);
    }

    #[test]
    fn caret_not_touching_a_bracket_is_none() {
        assert_eq!(match_bracket("cube(10);", 2), None); // mid-identifier
    }

    #[test]
    fn unbalanced_bracket_is_none() {
        assert_eq!(match_bracket("(a", 1), None); // no closing paren
    }

    #[test]
    fn braces_match_across_lines() {
        // "{\n a();\n}" — caret after the `{` (byte 1) matches the `}` on the last line.
        let src = "{\n a();\n}";
        let close = src.find('}').unwrap();
        assert_eq!(match_bracket(src, 1), Some((0, close)));
    }
}
