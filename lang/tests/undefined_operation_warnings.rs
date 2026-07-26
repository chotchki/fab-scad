//! SV — the `undefined operation` warning family, every line an ORACLE PROBE (2026.06.12).
//!
//! Upstream warns on every binop type error; we were silent on all of them (found while triaging
//! the AR.4 heavy-lane disagreements). The comparable channel is the BARE message: the harness's
//! oracle collector drops `\t` continuation lines and `differ::normalize_warning` strips the
//! ` in file …, line N` locator, so these strings ARE the parity surface. The rules that took
//! probing to find, pinned here so nobody re-derives them:
//!
//! - warnings fire at the TOP-LEVEL operation only — element-wise arithmetic is silent per element
//!   (`[1,2]+[3,"a"]` is `[4, undef]`, no warning), and there is NO dedup (a loop warns per pass);
//! - same-type UNORDERABLE comparison pairs (undef, object, function) get upstream's reversed
//!   wording `operation undefined (…)` with the SURFACE op; every other cell is
//!   `undefined operation (…)`, surface op + surface operand order;
//! - EXCEPT inside vector comparisons, which follow upstream's desugar — `<`/`>=` run `a<b`,
//!   `<=`/`>` run `b<a` — so the leaf pair prints in RUN order under the op `<`, plus one
//!   `in vector comparison at index N` frame per nesting level, innermost first;
//! - `*` has bespoke strings: dot length mismatch, empty vectors, the three matrix shape
//!   messages with the real counts, and `Vector must contain only numbers` (upstream prints that
//!   one TWICE — a two-stack-levels artifact we deliberately do not copy).

#![allow(clippy::expect_used, reason = "test harness: expect IS the assertion")]

use fab_lang::evaluate_geometry_full;

/// The warning strings `src` produces, bare (no prefix, no locator — the comparable form).
fn warns(src: &str) -> Vec<String> {
    let (_, messages) = evaluate_geometry_full(src).expect("evaluates");
    messages
        .iter()
        .filter_map(fab_lang::Message::warning)
        .map(str::to_string)
        .collect()
}

/// One expression → exactly one warning, matched in full.
#[track_caller]
fn warns_one(expr: &str, expected: &str) {
    assert_eq!(
        warns(&format!("echo({expr});")),
        vec![expected.to_string()],
        "for `{expr}`"
    );
}

/// One expression → zero warnings (the silent cells are as load-bearing as the loud ones).
#[track_caller]
fn silent(expr: &str) {
    assert_eq!(
        warns(&format!("echo({expr});")),
        Vec::<String>::new(),
        "for `{expr}`"
    );
}

#[test]
fn arithmetic_type_errors_warn_generically() {
    warns_one("1 + \"a\"", "undefined operation (number + string)");
    warns_one("\"a\" + \"b\"", "undefined operation (string + string)");
    warns_one("1 + undef", "undefined operation (number + undefined)");
    warns_one("1 + [1,2]", "undefined operation (number + vector)");
    warns_one("1 - \"a\"", "undefined operation (number - string)");
    warns_one("undef * 1", "undefined operation (undefined * number)");
    warns_one("1 / undef", "undefined operation (number / undefined)");
    warns_one("[6,4] / \"a\"", "undefined operation (vector / string)");
    warns_one("\"a\" / [1,2]", "undefined operation (string / vector)");
    warns_one("5 % undef", "undefined operation (number % undefined)");
    warns_one("\"a\" % 2", "undefined operation (string % number)");
    warns_one("[1,2] % 2", "undefined operation (vector % number)");
    warns_one("2 ^ \"a\"", "undefined operation (number ^ string)");
    warns_one("2 ^ [1,2]", "undefined operation (number ^ vector)");
    warns_one("true + 1", "undefined operation (bool + number)");
}

#[test]
fn elementwise_arithmetic_is_silent_per_element() {
    silent("[1,2] + [3,\"a\"]"); // [4, undef] — the element's undef does NOT warn
    silent("[1,2] + [3,4,5]"); // silent truncation to [4, 6]
    silent("2 * [1,\"a\"]"); // broadcast: [2, undef]
    silent("-[1,\"a\"]"); // unary recursion: [-1, undef]
}

#[test]
fn multiplication_has_its_own_vocabulary() {
    warns_one(
        "[1,2] * [3,4,5]",
        "vector*vector requires matching lengths (2 != 3)",
    );
    warns_one("[] * []", "Multiplication is undefined on empty vectors");
    // empties are checked FIRST — this is not a length mismatch upstream
    warns_one("[] * [1,2]", "Multiplication is undefined on empty vectors");
    // a flat MIXED operand is a VECTOR in upstream's dispatch: the dot's failing pair, left first
    warns_one("[1,\"a\"] * [1,2]", "undefined operation (string * number)");
    warns_one("[1,2] * [1,\"a\"]", "undefined operation (number * string)");
    warns_one(
        "[[1,2]] * [[1,2]]",
        "matrix*matrix requires left operand column count to match right operand row count (2 != 1)",
    );
    warns_one(
        "[1,2,3] * [[1],[2]]",
        "vector*matrix requires vector length to match matrix row count (3 != 2)",
    );
    warns_one(
        "[[1,2],[3,4]] * [1,2,3]",
        "matrix*vector requires matrix column count to match vector length (2 != 3)",
    );
    // upstream prints this line TWICE (same message, two stack levels); once is the deliberate choice
    warns_one(
        "[[1,\"a\"]] * [[1],[2]]",
        "Vector must contain only numbers. Problem at index 1",
    );
}

#[test]
fn unary_messages_are_spaceless() {
    warns_one("-\"a\"", "undefined operation (-string)");
    warns_one("-undef", "undefined operation (-undefined)");
    warns_one("~\"a\"", "undefined operation (~string)");
    warns_one("~undef", "undefined operation (~undefined)");
}

#[test]
fn bitwise_and_shift() {
    warns_one("undef | 1", "undefined operation (undefined | number)");
    warns_one("\"a\" | 1", "undefined operation (string | number)");
    warns_one("6 & \"a\"", "undefined operation (number & string)");
    warns_one("5 << undef", "undefined operation (number << undefined)");
    warns_one("1 << 64", "shift too large");
    warns_one("1 << -1", "negative shift");
}

#[test]
fn cross_type_comparisons_keep_the_surface_op() {
    warns_one("[1,2] <= 3", "undefined operation (vector <= number)");
    warns_one("3 < [1,2]", "undefined operation (number < vector)");
    warns_one("\"a\" < 1", "undefined operation (string < number)");
    warns_one("true < 1", "undefined operation (bool < number)");
    warns_one("[0:1] < 1", "undefined operation (range < number)");
    warns_one("undef < 1", "undefined operation (undefined < number)");
}

#[test]
fn same_type_unorderable_pairs_get_the_reversed_wording() {
    warns_one(
        "undef < undef",
        "operation undefined (undefined < undefined)",
    );
    warns_one(
        "undef <= undef",
        "operation undefined (undefined <= undefined)",
    );
    warns_one(
        "object(a=1) < object(a=1)",
        "operation undefined (object < object)",
    );
    warns_one(
        "object(a=1) >= object(a=1)",
        "operation undefined (object >= object)",
    );
    warns_one(
        "(function(x) x) < (function(y) y)",
        "operation undefined (function < function)",
    );
    // cross-type object stays on the normal wording — the quirk is the same-type pairs only
    warns_one("object(a=1) < 1", "undefined operation (object < number)");
}

#[test]
fn vector_comparison_mismatches_desugar_and_carry_frames() {
    // `<` and `>=` run a<b; `<=` and `>` run b<a — the leaf pair prints in RUN order, op always `<`
    warns_one(
        "[1,2] < [1,\"b\"]",
        "undefined operation (number < string)\n\tin vector comparison at index 1",
    );
    warns_one(
        "[1,2] >= [1,\"b\"]",
        "undefined operation (number < string)\n\tin vector comparison at index 1",
    );
    warns_one(
        "[1,2] <= [1,\"b\"]",
        "undefined operation (string < number)\n\tin vector comparison at index 1",
    );
    warns_one(
        "[1,2] > [1,\"b\"]",
        "undefined operation (string < number)\n\tin vector comparison at index 1",
    );
    warns_one(
        "[1,undef] < [1,2]",
        "undefined operation (undefined < number)\n\tin vector comparison at index 1",
    );
    // nested: one frame per level, INNERMOST first
    warns_one(
        "[[1,2],[\"a\",4]] < [[1,2],[9,9]]",
        "undefined operation (string < number)\n\tin vector comparison at index 0\n\tin vector comparison at index 1",
    );
}

#[test]
fn coercing_ops_never_warn() {
    silent("!undef");
    silent("undef && true");
    silent("undef || false");
    silent("![1,2]");
    // and a NaN comparison is IEEE-false, not a type error
    silent("(0/0) < 1");
    silent("[0/0] <= [0/0]");
}

#[test]
fn no_dedup_a_loop_warns_every_pass() {
    let w = warns("for (i=[0:2]) echo(1 + \"a\");");
    assert_eq!(
        w,
        vec!["undefined operation (number + string)".to_string(); 3],
        "upstream re-warns per evaluation — three passes, three warnings"
    );
}
