//! Value → string, the OpenSCAD way — the SHARED formatter behind `str()` (I.4.2) and echo (I.5).
//!
//! Numbers are the hard part: OpenSCAD's `str`/`echo` print with a specific precision (6 significant
//! digits, trailing zeros trimmed, scientific notation past a magnitude threshold, `nan`/`inf` spelled
//! lowercase). This is a FIRST CUT — integers and terminating decimals already match bit-for-bit; the
//! 6-sig-fig rounding + scientific crossover get nailed against the oracle at I.5 (the "echo/str text
//! string-equal vs oracle" gate). Everything routes through here so that gate has ONE place to fix.
//!
//! `str()` vs echo differ only at the TOP level: `str("ab")` is the raw `ab`, but a string NESTED in a
//! list — and every string echo touches — is quoted (`["ab"]`). So the quoting lives in [`format_value`]
//! (the nested/echo form) and `str`'s top-level raw-string case is handled by its caller.

use super::value::Value;

/// A value's OpenSCAD string form — the NESTED/echo representation: strings are QUOTED, a list is
/// `[a, b, c]`, a range is `[start : step : end]`. `str()`'s top-level raw-string case is its caller's.
pub(super) fn format_value(v: &Value) -> String {
    match v {
        Value::Undef => "undef".to_string(),
        Value::Bool(b) => b.to_string(), // "true" / "false"
        Value::Num(n) => format_number(*n),
        Value::Str(s) => format!("\"{s}\""), // quoted; internal-escape exactness is I.5
        Value::NumList(xs) => format_list(xs.iter().map(|n| format_number(*n))),
        Value::List(xs) => format_list(xs.iter().map(format_value)),
        Value::Range { start, step, end } => format!(
            "[{} : {} : {}]",
            format_number(*start),
            format_number(*step),
            format_number(*end)
        ),
        // A function's exact echo text needs the closure AST (via `Ctx`, which the formatter doesn't
        // hold) and is vanishingly rare in `str`/echo — a placeholder for now, pinned at I.5.
        Value::Function { .. } => "function ...".to_string(),
    }
}

/// A number's OpenSCAD string form. `-0` normalizes to `0`; `nan`/`inf`/`-inf` are lowercase. Finite
/// values use Rust's shortest round-trip — EXACT for integers and terminating decimals, first-cut for
/// long fractions / large magnitudes (6-sig-fig + scientific crossover is I.5).
fn format_number(n: f64) -> String {
    if n == 0.0 {
        "0".to_string() // collapses -0 → "0", matching OpenSCAD
    } else if n.is_nan() {
        "nan".to_string()
    } else if n.is_infinite() {
        if n < 0.0 { "-inf" } else { "inf" }.to_string()
    } else {
        format!("{n}")
    }
}

/// Join pre-formatted items as `[a, b, c]` (empty → `[]`).
fn format_list(items: impl Iterator<Item = String>) -> String {
    let mut s = String::from("[");
    for (i, item) in items.enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&item);
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::{Value, format_number, format_value};

    #[test]
    fn numbers_cover_the_special_cases() {
        assert_eq!(format_number(5.0), "5"); // integer-valued → no decimal
        assert_eq!(format_number(-0.0), "0"); // -0 normalizes
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(1.5), "1.5"); // terminating decimal
        assert_eq!(format_number(-2.25), "-2.25");
        assert_eq!(format_number(f64::NAN), "nan");
        assert_eq!(format_number(f64::INFINITY), "inf");
        assert_eq!(format_number(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn values_render_nested_openscad_form() {
        assert_eq!(format_value(&Value::Undef), "undef");
        assert_eq!(format_value(&Value::Bool(true)), "true");
        assert_eq!(format_value(&Value::Bool(false)), "false");
        assert_eq!(format_value(&Value::string("ab")), "\"ab\""); // quoted when nested/echoed
        assert_eq!(
            format_value(&Value::num_list(vec![1.0, 2.0, 3.0])),
            "[1, 2, 3]"
        );
        assert_eq!(format_value(&Value::num_list(Vec::new())), "[]"); // empty list
        assert_eq!(
            format_value(&Value::list(vec![
                Value::num_list(vec![1.0]),
                Value::string("a"),
            ])),
            "[[1], \"a\"]" // nested strings quoted
        );
        assert_eq!(
            format_value(&Value::Range {
                start: 0.0,
                step: 2.0,
                end: 6.0,
            }),
            "[0 : 2 : 6]"
        );
    }
}
