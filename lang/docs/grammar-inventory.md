# Grammar inventory — the OpenSCAD conformance checklist

Every production in OpenSCAD's `src/core/parser.y` and every rule in `src/core/lexer.l`, mapped to
the scad-rs node that implements it, its parse status, and the test that pins it. This is the
LEDGER for Phase H: a production is "accounted for" when it has a row here, a faithful AST node,
and a conformance anchor. The bison-derived conformance suite (H.5.3) is generated FROM this table
— one example per production, so a green suite IS this doc proven executable.

Provenance: the reference grammar is the installed nightly (2026.06.12 = master). `parser.y` has no
precedence table — precedence is STRUCTURAL, one nonterminal per tier — so our hand-written
recursive descent mirrors the rule cascade rather than a Pratt table. Line refs (`parser.y:NNN`)
point at that file. Anchors name the test that pins the row; `tests/parser_corpus.rs` and
`tests/lexer_corpus.rs` unless noted. `H.5.3` in the anchor column is a HOLE — a real row whose
dedicated conformance test the H.5.3 suite still owes.

## Status legend

| Mark | Meaning |
|------|---------|
| ✅ | Parsed today — landed in the G.3.3 tracer bullet (`lang/src/parser/`). |
| 🔨 | Built THIS phase — the task id (`H.2.2`) that lands the AST node + parser. |
| ⏳ | Parses to an AST node here; its SEMANTICS land in a later phase (`I` evaluator / `J` geometry). |
| 🚫 | Recognized-but-LOUD today — the parser bails with an `H.x`/`I.x` tag; the 🔨/⏳ row retires the bail. |
| ⚠️ | DELIBERATE divergence from OpenSCAD — documented in [Divergences](#deliberate-divergences), not a gap. |

The split that matters: Phase H owns PARSING (source → faithful AST). Whether the evaluator can RUN a
node is a separate axis — `use`/`include` parse in H (🔨) but resolve in I.2's loader (⏳), and the
whole geometry surface parses as module calls but lowers in J. "Parsed" ≠ "evaluated"; this table
tracks the first, and names the phase that owns the second.

## Parser grammar (`parser.y`)

### Top level + statements

| Production | `parser.y` | AST node / parser fn | Status | Anchor |
|------------|-----------|----------------------|--------|--------|
| `input : /*empty*/` | 174 | `Program{stmts:[]}` | ✅ | H.5.3 |
| `input : input TOK_USE` | 176 | `StmtKind::Use` | ✅ H.2.5 | `use_and_include_parse_to_nodes_with_the_raw_path` |
| `input : input statement` | 182 | `program()` loop | ✅ | `assignment_and_empty` |
| `statement : ';'` | 186 | `StmtKind::Empty` | ✅ | `assignment_and_empty` |
| `statement : '{' inner_input '}'` | 187 | `StmtKind::Block` | ✅ | `top_level_block` |
| `statement : module_instantiation` | 188 | `StmtKind::Module` | ✅ | `module_instantiation_forms` |
| `statement : assignment` | 192 | `StmtKind::Assignment` | ✅ | `assignment_and_empty` |
| `statement : TOK_MODULE TOK_ID '(' parameters ')' statement` | 193 | `StmtKind::ModuleDef` | ✅ H.2.2 | `module_and_function_defs_parse` |
| `statement : TOK_FUNCTION TOK_ID '(' parameters ')' '=' expr ';'` | 207 | `StmtKind::FunctionDef` | ✅ H.2.3 | `module_and_function_defs_parse` |
| `statement : TOK_EOT` | 215 | `StmtKind::Empty` (ETX sentinel) | ✅ | H.5.3 |
| `inner_input` | 221 | `block()` contents | ✅ | `top_level_block` |
| `assignment : TOK_ID '=' expr ';'` | 226 | `StmtKind::Assignment` | ✅ | `assignment_and_empty` |

### Module instantiation + if/else

| Production | `parser.y` | AST node / parser fn | Status | Anchor |
|------------|-----------|----------------------|--------|--------|
| `module_instantiation : '!'/'#'/'%'/'*' module_instantiation` | 235-254 | `Modifiers` (four flags) | ✅ | `all_modifiers_stack` |
| `module_instantiation : single_module_instantiation child_statement` | 255 | `ModuleInstantiation` | ✅ | `module_instantiation_forms` |
| `module_instantiation : ifelse_statement` | 265 | `StmtKind::If` (in the module-inst path) | ✅ H.2.4 | `if_else_parses_in_every_position` |
| `ifelse_statement : if_statement [TOK_ELSE child_statement]` | 271-285 | `StmtKind::If{els}` | ✅ H.2.4 | `if_else_parses_in_every_position` |
| `if_statement : TOK_IF '(' expr ')' child_statement` | 287 | `StmtKind::If{cond,then}` | ✅ H.2.4 | `if_else_parses_in_every_position` |
| `child_statements` (⊂ `inner_input`, no defs) | 300 | `block(allow_defs=false)` | ✅ H.2.6 | `defs_are_rejected_inside_child_blocks` |
| `child_statement : ';' / '{'…'}' / module_instantiation` | 306-313 | `child_statement()` | ✅ | `module_instantiation_forms` |
| `module_id : TOK_ID \| for \| let \| assert \| echo \| each` | 316-323 | `module_id()` | ✅ | `keyword_module_ids` |
| `single_module_instantiation : module_id '(' arguments ')'` | 325 | `ModuleInstantiation` | ✅ | `module_instantiation_forms` |

`for` / `intersection_for` / `let` / `each` / `assert` / `echo` as STATEMENTS need no dedicated
grammar — they are `module_id`s (or, for `intersection_for`, a plain `TOK_ID`), so they already
parse as `ModuleInstantiation` today. Their control-flow/echo SEMANTICS are I.2/I.3.

### Expressions — the tier cascade

Precedence is the rule order, loosest→tightest. Our `binary()` climb reproduces it with binding
powers; `exponent`/`unary`/`call` are their own fns. All ✅ (G.3.3).

| Tier (production) | `parser.y` | Operators | Our `binop` bp | Anchor |
|-------------------|-----------|-----------|----------------|--------|
| `expr : logic_or` | 335 | — | — | `arithmetic_precedence` |
| `logic_or` | 362 | `\|\|` | 2 | `unary_and_logical` |
| `logic_and` | 370 | `&&` | 3 | `unary_and_logical` |
| `equality` | 378 | `== !=` | 4 | `bitwise_sits_between_comparison_and_shift` |
| `comparison` | 390 | `> >= < <=` | 5 | `bitwise_sits_between_comparison_and_shift` |
| `binaryor` | 410 | `\|` | 6 | `bitwise_sits_between_comparison_and_shift` |
| `binaryand` | 418 | `&` | 7 | `bitwise_sits_between_comparison_and_shift` |
| `shift` | 426 | `<< >>` | 8 | `bitwise_sits_between_comparison_and_shift` |
| `addition` | 438 | `+ -` | 9 | `arithmetic_precedence` |
| `multiplication` | 450 | `* / %` | 10 | `arithmetic_precedence` |
| `unary : '+' / '-' / '!' / '~' unary` | 467-491 | prefix | `unary()` | `unary_and_logical` (⚠️ `-literal` fold) |
| `exponent : call ['^' unary]` | 494 | `^` right-assoc | `exponent()` | `power_is_right_assoc_between_unary_and_call` |
| `call : primary \| call '(' args ')' \| call '[' expr ']' \| call '.' TOK_ID` | 502-518 | postfix | `call()` | `postfix_chains` |

### Expressions — the non-cascade `expr` forms

| Production | `parser.y` | AST node | Status | Anchor |
|------------|-----------|----------|--------|--------|
| `expr : logic_or '?' expr ':' expr` | 341 | `ExprKind::Ternary` | ✅ | `ternary_is_right_assoc` |
| `expr : TOK_FUNCTION '(' parameters ')' expr` | 336 | `ExprKind::FunctionLiteral` | ✅ H.3.3 | `function_let_assert_echo_expressions_parse` |
| `expr : TOK_LET '(' arguments ')' expr` | 345 | `ExprKind::Let` | ✅ H.3.4 | `function_let_assert_echo_expressions_parse` |
| `expr : TOK_ASSERT '(' arguments ')' expr_or_empty` | 350 | `ExprKind::Assert{body:Option}` | ✅ H.3.5 | `function_let_assert_echo_expressions_parse` |
| `expr : TOK_ECHO '(' arguments ')' expr_or_empty` | 355 | `ExprKind::Echo{body:Option}` | ✅ H.3.5 | `function_let_assert_echo_expressions_parse` |
| `expr_or_empty : /*empty*/ \| expr` | 569 | `Option<Box<Expr>>` | ✅ H.3.5 | `function_let_assert_echo_expressions_parse` |

### Primary + collections

| Production | `parser.y` | AST node | Status | Anchor |
|------------|-----------|----------|--------|--------|
| `primary : TOK_TRUE / TOK_FALSE` | 521-527 | `ExprKind::Bool` | ✅ | `literals` |
| `primary : TOK_UNDEF` | 529 | `ExprKind::Undef` | ✅ | `literals` |
| `primary : TOK_NUMBER` | 533 | `ExprKind::Num` | ✅ | `literals` |
| `primary : TOK_STRING` | 537 | `ExprKind::Str` | ✅ | `literals` |
| `primary : TOK_ID` | 542 | `ExprKind::Ident` | ✅ | `literals` |
| `primary : '(' expr ')'` | 547 | inner expr (no paren node) | ✅ | `arithmetic_precedence` |
| `primary : '[' expr ':' expr ']'` | 551 | `ExprKind::Range{step:None}` | ✅ | `vectors_and_ranges` |
| `primary : '[' expr ':' expr ':' expr ']'` | 555 | `ExprKind::Range{step:Some}` | ✅ | `vectors_and_ranges` |
| `primary : '[' ']'` | 559 | `ExprKind::Vector([])` | ✅ | `vectors_and_ranges` |
| `primary : '[' vector_elements optional_trailing_comma ']'` | 563 | `ExprKind::Vector` | ✅ | `vectors_and_ranges` |
| `vector_elements` / `optional_trailing_comma` | 622-638 | `Vector` + trailing comma | ✅ | `vectors_and_ranges` |
| `vector_element : list_comprehension_elements_p \| expr` | 640 | `vector_element()` dispatch | ✅ H.3.2 | `list_comprehensions_parse_every_form` |

### List comprehensions (H.3.2) — ✅

All landed H.3.2, anchor `list_comprehensions_parse_every_form`. NOTE: the `let` comprehension reuses
[`ExprKind::Let`] rather than a distinct `LcLet` — a vector `let` is semantically the let-EXPRESSION
(bind, evaluate body), the only twist being its body is a vector element (may nest a comprehension).
The AST shape differs from OpenSCAD's `LcLet` there, but the VALUE is identical; a deliberate
simplification.

| Production | `parser.y` | AST node | Status |
|------------|-----------|----------|--------|
| `TOK_LET '(' arguments ')' lce_p` | 583 | `ExprKind::Let` (see note) | ✅ H.3.2 |
| `TOK_EACH vector_element` | 588 | `LcEach` | ✅ H.3.2 |
| `TOK_FOR '(' arguments ')' vector_element` | 592 | `LcFor` | ✅ H.3.2 |
| `TOK_FOR '(' arguments ';' expr ';' arguments ')' vector_element` | 597 | `LcForC` (C-style) | ✅ H.3.2 |
| `TOK_IF '(' expr ')' vector_element [TOK_ELSE vector_element]` | 603-607 | `LcIf{els:Option}` | ✅ H.3.2 |
| `list_comprehension_elements_p : lce \| '(' lce ')'` | 614 | paren-guard in `vector_element()` | ✅ H.3.2 |

### Parameters + arguments

| Production | `parser.y` | AST node | Status | Anchor |
|------------|-----------|----------|--------|--------|
| `parameters / parameter_list` | 645-664 | `Vec<Parameter>` | ✅ H.2.1 | `module_and_function_defs_parse` |
| `parameter : TOK_ID` | 666 | `Parameter{default:None}` | ✅ H.2.1 | `module_and_function_defs_parse` |
| `parameter : TOK_ID '=' expr` | 672 | `Parameter{default:Some}` | ✅ H.2.1 | `module_and_function_defs_parse` |
| `arguments / argument_list` | 679-698 | `Vec<Arg>` | ✅ | `module_instantiation_forms` |
| `argument : expr` | 700 | `Arg{name:None}` (positional) | ✅ | `module_instantiation_forms` |
| `argument : TOK_ID '=' expr` | 705 | `Arg{name:Some}` (named, incl. `$fn=`) | ✅ | `module_instantiation_forms` |

## Lexer grammar (`lexer.l`)

Our lexer is essentially COMPLETE for the whole grammar — the G-phase built it lossless (comments
kept for the customizer). Phase H's lexer work is H.1.2: audit-and-pin, not build.

| Rule | `lexer.l` | Our handling | Status | Anchor |
|------|-----------|--------------|--------|--------|
| `include <path>` | 139 | `TokenKind::Include` (raw path; splice is I.2) | ✅ ⚠️ | `use_include_filename_tokens` |
| `use <path>` | 153 | `TokenKind::Use` (raw path; import is I.2) | ✅ ⚠️ | `use_include_filename_tokens` |
| string body + `\n\t\r\\\"` | 176-187 | `lex_string` / `decode_str` | ✅ | `string_escapes_decode` |
| `\x[0-7]{H}` (`\x00`→space) | 188 | `decode_hex_byte` | ✅ | `string_escapes_decode` |
| `\u{H}{4}` / `\U{H}{6}` | 189 | `decode_unicode` | ✅ | `string_escapes_decode` |
| undefined escape → warn + literal | 190 | `decode_str` passthrough (warn is I.5) | ✅ | `decode_and_num_edge_paths` |
| line `//` / block `/* */` comments | 205-219 | `LineComment` / `BlockComment` (KEPT) | ✅ ⚠️ | `comments_kept_in_all_filtered_from_code` |
| block comment non-nesting | 213 | first `*/` closes | ✅ | `block_comments_do_not_nest` |
| `\x03` → `TOK_EOT` | 239 | `TokenKind::Eot` | ✅ | H.5.3 |
| keywords (module…undef) | 241-253 | whole-lexeme keyword match | ✅ | `every_keyword_lexes` / `keyword_only_as_whole_lexeme` |
| nbsp `\xc2\xa0` / BOM `\xef\xbb\xbf` skip | 260-271 | `skip_trivia_ws` (U+00A0, U+FEFF) | ✅ | `bom_and_nbsp_are_skipped` |
| other non-ASCII UTF-8 → `TOK_ERROR` | 277 | rejected at the `&str`/token boundary | ✅ ⚠️ | `non_ascii_identifier_is_a_hard_error` |
| `0x{H}+` hex int (lowercase) | 287 | `recognize_hex` / `num_value` | ✅ | `hex_lowercase_only` |
| float forms `{D}+{E}`, `.{D}+`, `{D}+.` | 301-303 | `recognize_float` | ✅ | `floats_leading_and_trailing_dot` |
| `{D}+` decimal | 309 | `recognize_decimal` | ✅ | `num_value_matches_openscad` |
| `{IDSTART}{IDREST}*` id (incl. `$`) | 324 | `lex_word` → `Ident` / `DollarIdent` | ✅ | `dollar_vars_and_id_splitting` |
| `{D}{IDREST}*` digit-leading id (deprecated) | 325 | `lex_digit_start` (warn is I.5) | ✅ | `number_vs_digit_leading_id_longest_match` |
| operators `<= >= == != && \|\| << >>` | 332-339 | multi-char `alt` (longest match) | ✅ | `multi_char_operators_beat_prefixes` |
| single-char catch-all `. return yytext[0]` | 341 | per-char `dispatch!` arm | ✅ | `unary_minus_is_its_own_token` |

## Deliberate divergences

Not gaps — choices, each with a reason. These are the ⚠️ rows above.

- **Comments are PRESERVED as tokens.** flex discards them (comment states emit nothing); we keep
  `LineComment`/`BlockComment` in `Lexed::all` because the customizer (H.4) binds trailing comments
  to parameters. The parser reads `Lexed::code`, which filters them out, so the grammar is unaffected.
- **The lexer does ZERO file I/O.** flex splices `include` files and opens `use` targets INSIDE the
  lexer (buffer-stack push). We emit `Include`/`Use` tokens carrying the raw path and resolve them in
  I.2's loader — the parser stays a pure, fuzzable `&str → tokens` function with no filesystem reach.
  This is the H.2.5 decision (2026-07-04): parse-only in H, resolve in a loader.
- **Unicode is handled at code-point granularity, rejected at the `&str` boundary.** flex scans
  BYTES and emits `TOK_ERROR` on stray non-ASCII UTF-8 mid-stream. We take `&str` (valid UTF-8 by
  construction — the caller rejects non-UTF-8), step by `char`, and keep the ASCII-only grammar for
  ids/keywords/numbers. Same observable result (non-ASCII outside strings/comments is an error);
  different mechanism.
- **`-literal` is NOT constant-folded at parse time.** flex/bison fold `- <numeric literal>` into a
  single negative `Literal` (parser.y:475-483). We emit `Unary{Neg, Num}` and let the evaluator fold.
  It evaluates identically (`-2^2 == -(2^2)` holds either way — `^` binds tighter than unary); the AST
  SHAPE differs, which only matters to a print-tree comparison, never to a value. Noted so the H.5
  roundtrip property doesn't chase it as a bug.
- **`child_statements ⊂ inner_input`** (enforced, H.2.6). In `parser.y`, a module-call/`if` child
  block (`child_statements`) admits only `child_statement`/`assignment` — NOT module/function defs —
  whereas the file top level + a module-def body (`inner_input`) admit everything. We thread an
  `allow_defs` context flag through `statement()`/`block()`: `true` at the top level and in a
  module body, `false` in every child subtree; a def in the `false` context is a parse error.
  RESIDUAL leniency: `use`/`include` are still accepted in a child context (OpenSCAD restricts `use`
  to the file's `input` level) — harmless while resolution is deferred, and the I.2 loader owns
  placement validation.

## How H.5.3 consumes this

The conformance suite is derived mechanically: each row with a concrete production contributes at
least one source snippet that must parse to the named AST node. A row whose anchor is `H.5.3` (or
`—` for a not-yet-built 🔨 row) is a HOLE the suite must fill before that box can tick. When every
row has a real anchor and the suite is green, "every production accounted for" (H.1) is proven, not
asserted.
