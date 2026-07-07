//! Value → string, the OpenSCAD way — the SHARED formatter behind `str()` (I.4.2) and echo (I.5).
//!
//! Numbers are the hard part: OpenSCAD's `str`/`echo` print 6 SIGNIFICANT digits (trailing zeros
//! trimmed, scientific past a magnitude threshold, `nan`/`inf` lowercase) via a Google double-conversion
//! `ToPrecision`. [`format_number`] reproduces that exactly (I.5), verified against the oracle's `ECHO:`
//! output; string escaping matches `QuotedString`. Everything routes through here so echo/`str`/list
//! rendering share ONE implementation.
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
        Value::Str(s) => format!("\"{}\"", escape_string(s)), // quoted + escaped (QuotedString)
        Value::NumList(xs) => format_list(xs.iter().map(|n| format_number(*n))),
        Value::List(xs) => format_list(xs.iter().map(format_value)),
        Value::Range { start, step, end } => format!(
            "[{} : {} : {}]",
            format_number(*start),
            format_number(*step),
            format_number(*end)
        ),
        // A function value renders as its SOURCE, OpenSCAD-style (`function(x) target_func(x)`) — pre-computed
        // at closure creation (`print::function_value_repr`) since the formatter can't reach the closure AST.
        Value::Function { repr, .. } => repr.to_string(),
    }
}

/// A number's OpenSCAD string form, bug-for-bug against `DoubleConvert` (`Value.cc`): its
/// `DoubleToStringConverter::ToPrecision(n, 6)` prints 6 SIGNIFICANT digits, then trims trailing zeros.
/// The config (`max_leading=5`, `max_trailing=0`) makes it FIXED unless the exponent leaves too much
/// padding — scientific iff `E < -5` or `E > 5` (E = the rounded base-10 exponent) — with the exponent
/// sign-prefixed and its leading zero stripped (`1e+6`, `1e-6`). `-0` → `0` (`UNIQUE_ZERO`); `nan`/`inf`.
fn format_number(n: f64) -> String {
    if n == 0.0 {
        return "0".to_string(); // collapses -0 → "0"
    }
    if n.is_nan() {
        return "nan".to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    // Round to 6 sig figs FIRST (via scientific), so the fixed-vs-scientific decision uses the ROUNDED
    // exponent — a value like 999999.9 rounds up to 1e+6 and crosses the threshold, matching OpenSCAD.
    // `{:.5e}` always emits "mantissa e exponent", so the split + parse defaults are never-taken belts.
    let sci = format!("{n:.5e}");
    let (mantissa, exp_str) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    if (-5..=5).contains(&exp) {
        // FIXED: 6 sig figs → (5 - E) fractional digits, then trim.
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        trim_fraction(&format!("{n:.decimals$}"))
    } else {
        // SCIENTIFIC: trim the mantissa, re-emit the exponent as `e±N` (minimal digits, signed).
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{}", trim_fraction(mantissa), exp.abs())
    }
}

/// Trim trailing zeros from a fractional part (and a bare trailing `.`); integers are left as-is.
fn trim_fraction(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// Escape a string for the QUOTED echo form, matching OpenSCAD's `QuotedString` (`Value.cc`): the five
/// escapes `\t \n \r \" \\`; everything else (incl. non-ASCII) passes through verbatim.
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
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
    fn numbers_match_the_oracle() {
        // Every expected string is the actual `ECHO:` output of OpenSCAD 2026.06.12 (probed directly).
        // integers + short decimals:
        assert_eq!(format_number(9.0), "9");
        assert_eq!(format_number(9.5), "9.5");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(0.1), "0.1");
        assert_eq!(format_number(-42.0), "-42");
        assert_eq!(format_number(123.456), "123.456");
        // 6 significant digits (rounded):
        assert_eq!(format_number(1.0 / 3.0), "0.333333");
        assert_eq!(format_number(2.0 / 3.0), "0.666667");
        assert_eq!(format_number(10.0 / 3.0), "3.33333");
        assert_eq!(format_number(std::f64::consts::PI), "3.14159");
        // scientific crossover: |x| ≥ 1e6 or |x| < 1e-5 → e±N with the leading zero stripped:
        assert_eq!(format_number(1e6), "1e+6");
        assert_eq!(format_number(1e7), "1e+7");
        assert_eq!(format_number(1e21), "1e+21");
        assert_eq!(format_number(1e-6), "1e-6");
        assert_eq!(format_number(1e-5), "0.00001"); // 1e-5 stays FIXED
        assert_eq!(format_number(1e-4), "0.0001");
        assert_eq!(format_number(100_000.0), "100000"); // 1e5 stays FIXED
        // specials + unique zero:
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(f64::NAN), "nan");
        assert_eq!(format_number(f64::INFINITY), "inf");
        assert_eq!(format_number(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn strings_are_quoted_and_escaped() {
        assert_eq!(format_value(&Value::string("hi")), "\"hi\"");
        // QuotedString escapes: tab, newline, CR, quote, backslash.
        assert_eq!(
            format_value(&Value::string("a\tb\nc\rd\"e\\f")),
            "\"a\\tb\\nc\\rd\\\"e\\\\f\""
        );
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
