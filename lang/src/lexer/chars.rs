//! ASCII character-class predicates for the lexer.
//!
//! OpenSCAD identifiers and numbers are ASCII-only (non-ASCII bytes are a lex error upstream), so
//! these stay ASCII — mirroring lexer.l's `IDSTART`/`IDREST` classes (lexer.l:130-131).

/// `IDSTART` = `[A-Za-z_$]` — a legal FIRST identifier char (`$` starts a `$`-var).
pub(crate) fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_ascii_alphabetic()
}

/// `IDREST` = `[A-Za-z0-9_]` — a legal non-first identifier char (note: NOT `$`, so `a$b` splits).
pub(crate) fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// The `[ \t\r\n]` run flex allows between `use`/`include` and its `<` (lexer.l:139,153).
pub(crate) fn is_use_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}
