//! The token types the lexer emits: [`Token`], [`TokenKind`], and the [`Lexed`] output.
//!
//! Plain data — no winnow coupling. Text-bearing kinds borrow the source (`'s`) so lexing
//! allocates nothing on the token path; numeric/string VALUES are decoded on demand
//! ([`num_value`](super::num_value) / [`decode_str`](super::decode_str)), because the customizer
//! (H.4) rewrites source and wants the RAW literal (`0x1F`, not `31`).

use core::ops::Range;

/// One lexed token: a classified [`TokenKind`] plus its byte span into the original source.
///
/// Borrows the source (`'s`) for every text-bearing kind — no allocation on the lex path.
/// `Clone` but not `Copy` ([`Range`] isn't `Copy`); a clone is two `usize`s plus a `Copy` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'s> {
    /// What this token is.
    pub kind: TokenKind<'s>,
    /// Byte range into the original source (from winnow's `.with_span()`).
    pub span: Range<usize>,
}

/// The classification of a lexeme.
///
/// `Copy` — every payload is a `&str` slice or unit. Fixed lexemes are unit variants (the G.3.3
/// parser matches them by discriminant); text-bearing lexemes carry a RAW source slice, decoded
/// on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind<'s> {
    // ── literals (raw, un-decoded) ──────────────────────────────────────────
    /// Numeric literal, RAW lexeme (`42`, `0x1F`, `.5e-3`, `5.`); decode with [`num_value`](super::num_value).
    Num(&'s str),
    /// String literal, RAW body between the quotes (escapes NOT applied); decode with [`decode_str`](super::decode_str).
    Str(&'s str),

    // ── identifiers ─────────────────────────────────────────────────────────
    /// `TOK_ID`, including deprecated digit-leading ids (`x`, `foo1`, `123abc`, `0X1F`).
    Ident(&'s str),
    /// A `$`-prefixed identifier (`$fn`, `$t`, or a lone `$`) — pre-classified from `Ident` so the
    /// evaluator can route special/dynamic args (same lexeme boundary as flex's single `TOK_ID`).
    DollarIdent(&'s str),

    // ── keywords (whole-lexeme match only: `modulefoo` is an `Ident`) ────────
    /// `module`.
    Module,
    /// `function`.
    Function,
    /// `if`.
    If,
    /// `else`.
    Else,
    /// `let`.
    Let,
    /// `assert`.
    Assert,
    /// `echo`.
    Echo,
    /// `for`.
    For,
    /// `each`.
    Each,
    /// `true`.
    True,
    /// `false`.
    False,
    /// `undef`.
    Undef,

    // ── context-sensitive file references (raw path slice, un-split) ─────────
    /// `use <path>` — `path` is the raw text inside `<…>` (resolution is H.2).
    Use(&'s str),
    /// `include <path>` — we EMIT a token; flex instead splices the file (splice is H.2).
    Include(&'s str),

    // ── comments (PRESERVED for the customizer; raw slice incl. markers) ─────
    /// `// …` up to (not including) the newline. Raw, includes the leading `//`.
    LineComment(&'s str),
    /// `/* … */` including both delimiters. Non-nesting: the first `*/` closes.
    BlockComment(&'s str),

    // ── multi-char operators (longest-match: these beat their prefixes) ──────
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `==`.
    EqEq,
    /// `!=`.
    Ne,
    /// `&&`.
    AndAnd,
    /// `||`.
    OrOr,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,

    // ── single-char operators / punctuation / modifiers ─────────────────────
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*` (also the module-disable modifier).
    Star,
    /// `/`.
    Slash,
    /// `%` (also the background modifier).
    Percent,
    /// `^`.
    Caret,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `=`.
    Eq,
    /// `!` (also the root/show-only modifier).
    Bang,
    /// `~`.
    Tilde,
    /// `&`.
    Amp,
    /// `|`.
    Pipe,
    /// `?`.
    Question,
    /// `:`.
    Colon,
    /// `.` (member access; a bare `.` with no adjacent digits).
    Dot,
    /// `,`.
    Comma,
    /// `;`.
    Semi,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `[`.
    LBracket,
    /// `]`.
    RBracket,
    /// `{`.
    LBrace,
    /// `}`.
    RBrace,
    /// `#` (the debug/highlight modifier).
    Hash,

    // ── special bytes ───────────────────────────────────────────────────────
    /// Literal ETX (0x03) — `TOK_EOT`, OpenSCAD's streamed-input end marker (lexer.l:239).
    Eot,
    /// Synthetic end-of-input sentinel (span = `len..len`), appended so G.3.3 can `expect(Eof)`.
    Eof,
}

impl TokenKind<'_> {
    /// Is this a comment kind? (Filtered out of the parser's [`Lexed::code`] view.)
    #[must_use]
    pub fn is_comment(&self) -> bool {
        matches!(self, TokenKind::LineComment(_) | TokenKind::BlockComment(_))
    }
}

/// The lexer's output: a lossless stream plus the parser's comment-free view.
///
/// Both borrow the source (`'s`). `all` is what the round-trip property and the customizer (H.4)
/// read; `code` is what G.3.3 wraps in `TokenSlice::new`.
#[derive(Debug, Clone)]
pub struct Lexed<'s> {
    /// EVERY token in source order INCLUDING comments — the lossless artifact. Ends with [`TokenKind::Eof`].
    pub all: Vec<Token<'s>>,
    /// The non-comment tokens (plus the trailing [`TokenKind::Eof`]) — the G.3.3 parser input.
    pub code: Vec<Token<'s>>,
}
