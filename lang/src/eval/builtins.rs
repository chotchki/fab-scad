//! OpenSCAD builtin FUNCTIONS (`func.cc`), applied to already-evaluated arguments.
//!
//! A builtin is a leaf operation: its arguments evaluate on the explicit stack, then this dispatches
//! by name. Ill-typed / missing args yield `undef` (OpenSCAD's undef-propagation), never an error.
//! Trig is in DEGREES and reuses `trig`'s exact-quadrant `sin`/`cos` so `sin(30)` etc. match the
//! geometry path bit-for-bit. `rands` (non-deterministic) is deliberately NOT here — it needs the
//! seeded-RNG discipline (I.4.3). Names here MUST match [`is_builtin`].

use std::collections::BTreeMap;

use super::trig;
use super::value::Value;

/// Is `name` a builtin we implement? Checked at a call site AFTER user functions, BEFORE "unknown"
/// (so a user function may shadow a builtin, per OpenSCAD).
pub(super) fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "sign"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "floor"
            | "ceil"
            | "round"
            | "ln"
            | "log"
            | "exp"
            | "pow"
            | "sqrt"
            | "min"
            | "max"
            | "norm"
            | "cross"
    )
}

/// Apply a builtin by name to its positional args (named args are unused by the math group).
pub(super) fn apply(name: &str, pos: &[Value], _named: &BTreeMap<String, Value>) -> Value {
    match name {
        "abs" => num1(pos, f64::abs),
        "sign" => num1(pos, sign),
        "sin" => num1(pos, trig::sin_degrees),
        "cos" => num1(pos, trig::cos_degrees),
        "tan" => num1(pos, |x| trig::sin_degrees(x) / trig::cos_degrees(x)),
        "asin" => num1(pos, |x| x.asin().to_degrees()),
        "acos" => num1(pos, |x| x.acos().to_degrees()),
        "atan" => num1(pos, |x| x.atan().to_degrees()),
        "atan2" => num2(pos, |y, x| y.atan2(x).to_degrees()),
        "floor" => num1(pos, f64::floor),
        "ceil" => num1(pos, f64::ceil),
        "round" => num1(pos, f64::round), // half AWAY from zero — same as OpenSCAD
        "ln" => num1(pos, f64::ln),
        "log" => num1(pos, f64::log10), // OpenSCAD `log` is base 10
        "exp" => num1(pos, f64::exp),
        "pow" => num2(pos, f64::powf),
        "sqrt" => num1(pos, f64::sqrt),
        "min" => min_max(pos, true),
        "max" => min_max(pos, false),
        "norm" => norm(pos),
        "cross" => cross(pos),
        _ => Value::Undef,
    }
}

/// OpenSCAD `sign`: `-1`/`0`/`1` (unlike Rust's `signum`, which is `±1` at zero and `NaN` at `NaN`).
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0 // includes ±0 and NaN (both comparisons false), matching func.cc
    }
}

/// Apply a unary numeric function to the first arg; non-number / missing → `undef`.
fn num1(pos: &[Value], f: impl Fn(f64) -> f64) -> Value {
    match pos.first() {
        Some(&Value::Num(x)) => Value::Num(f(x)),
        _ => Value::Undef,
    }
}

/// Apply a binary numeric function to the first two args; non-numbers / missing → `undef`.
fn num2(pos: &[Value], f: impl Fn(f64, f64) -> f64) -> Value {
    match (pos.first(), pos.get(1)) {
        (Some(&Value::Num(a)), Some(&Value::Num(b))) => Value::Num(f(a, b)),
        _ => Value::Undef,
    }
}

/// `min`/`max`: either several numeric args, or a single numeric-list arg. Empty / ill-typed → `undef`.
fn min_max(pos: &[Value], is_min: bool) -> Value {
    let nums: Vec<f64> = match pos {
        [Value::NumList(xs)] => xs.to_vec(),
        [Value::Num(x)] => vec![*x],
        multi => {
            let mut v = Vec::with_capacity(multi.len());
            for value in multi {
                match value {
                    Value::Num(x) => v.push(*x),
                    _ => return Value::Undef,
                }
            }
            v
        }
    };
    match nums.split_first() {
        Some((&head, rest)) => Value::Num(
            rest.iter()
                .fold(head, |acc, &x| if is_min { acc.min(x) } else { acc.max(x) }),
        ),
        None => Value::Undef, // min()/max() with no numbers
    }
}

/// `norm(v)` — the Euclidean length of a numeric vector (sequential sum of squares, matching `func.cc`).
fn norm(pos: &[Value]) -> Value {
    match pos.first() {
        Some(Value::NumList(xs)) => Value::Num(xs.iter().map(|x| x * x).sum::<f64>().sqrt()),
        _ => Value::Undef,
    }
}

/// `cross(a, b)` — the 3D cross product (a 3-vector), or the 2D cross (a scalar). Anything else → `undef`.
fn cross(pos: &[Value]) -> Value {
    match (pos.first(), pos.get(1)) {
        (Some(Value::NumList(a)), Some(Value::NumList(b))) => match (&a[..], &b[..]) {
            ([a0, a1, a2], [b0, b1, b2]) => Value::num_list(vec![
                a1 * b2 - a2 * b1,
                a2 * b0 - a0 * b2,
                a0 * b1 - a1 * b0,
            ]),
            ([a0, a1], [b0, b1]) => Value::Num(a0 * b1 - a1 * b0),
            _ => Value::Undef,
        },
        _ => Value::Undef,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Value, apply};

    #[test]
    fn unknown_name_is_undef() {
        // `apply` is gated by `is_builtin` at every call site, so this fallback is reachable only here.
        assert_eq!(apply("not_a_builtin", &[], &BTreeMap::new()), Value::Undef);
    }
}
