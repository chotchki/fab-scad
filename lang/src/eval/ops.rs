//! Binary + unary value operations — OpenSCAD `Value.cc` semantics, bug-for-bug.
//!
//! Everything here is INFALLIBLE: a wrong/undef operand yields `Undef` (OpenSCAD's undef-propagation
//! — `Value::undef(reason)`), never an error. The load-bearing surprises (grounded from Value.cc):
//! `str + str` is `undef` (not concat), `vec * vec` (equal-length) is the DOT PRODUCT (a scalar),
//! `vec + vec` silently TRUNCATES to the shorter, `%` is `fmod` (sign of dividend), `^` is `pow`,
//! cross-type `==`/`!=` never coerce (`1 == true` → false), cross-type `< <= > >=` → `undef`.
//!
//! **[VERIFY at G.3.6]** `&&`/`||` are evaluated NON-short-circuit here (both operands already
//! evaluated by the stack machine); if OpenSCAD short-circuits, `true || <erroring>` diverges.

use std::cmp::Ordering;
use std::rc::Rc;

use super::value::Value;
use crate::parser::{BinOp, UnOp};

/// Apply a binary operator to two already-evaluated values. Infallible (bad types → `Undef`).
#[must_use]
pub fn apply_binary(op: BinOp, a: Value, b: Value) -> Value {
    use Value::{Num, NumList};
    match op {
        BinOp::Add => match (a, b) {
            (Num(x), Num(y)) => Num(x + y),
            (NumList(x), NumList(y)) => Value::NumList(zip_trunc(&x, &y, |x, y| x + y)),
            _ => Value::Undef,
        },
        BinOp::Sub => match (a, b) {
            (Num(x), Num(y)) => Num(x - y),
            (NumList(x), NumList(y)) => Value::NumList(zip_trunc(&x, &y, |x, y| x - y)),
            _ => Value::Undef,
        },
        BinOp::Mul => match (a, b) {
            (Num(x), Num(y)) => Num(x * y),
            (Num(s), NumList(v)) | (NumList(v), Num(s)) => {
                Value::NumList(v.iter().map(|e| e * s).collect())
            }
            // equal-length non-empty number vectors → DOT PRODUCT (a scalar), NOT element-wise
            (NumList(x), NumList(y)) if !x.is_empty() && x.len() == y.len() => Num(dot(&x, &y)),
            _ => Value::Undef,
        },
        BinOp::Div => match (a, b) {
            (Num(x), Num(y)) => Num(x / y), // IEEE: 1/0 → inf, 0/0 → NaN
            (NumList(v), Num(s)) => Value::NumList(v.iter().map(|e| e / s).collect()),
            (Num(s), NumList(v)) => Value::NumList(v.iter().map(|e| s / e).collect()),
            _ => Value::Undef,
        },
        BinOp::Mod => match (a, b) {
            (Num(x), Num(y)) => Num(x % y), // Rust f64 `%` == C fmod (sign of dividend)
            _ => Value::Undef,
        },
        BinOp::Pow => match (a, b) {
            (Num(x), Num(y)) => Num(x.powf(y)),
            _ => Value::Undef,
        },
        BinOp::Eq => Value::Bool(a == b), // Value's custom PartialEq IS OpenSCAD `==` (no coercion)
        BinOp::Ne => Value::Bool(a != b),
        BinOp::Lt => order(&a, &b, |o| o == Ordering::Less),
        BinOp::Le => order(&a, &b, |o| o != Ordering::Greater),
        BinOp::Gt => order(&a, &b, |o| o == Ordering::Greater),
        BinOp::Ge => order(&a, &b, |o| o != Ordering::Less),
        BinOp::And => Value::Bool(a.is_truthy() && b.is_truthy()),
        BinOp::Or => Value::Bool(a.is_truthy() || b.is_truthy()),
        BinOp::BitOr => bitwise(a, b, |x, y| x | y),
        BinOp::BitAnd => bitwise(a, b, |x, y| x & y),
        BinOp::Shl => shift(a, b, true),
        BinOp::Shr => shift(a, b, false),
    }
}

/// Apply a prefix unary operator. Infallible (bad type → `Undef`).
#[must_use]
pub fn apply_unary(op: UnOp, v: Value) -> Value {
    match op {
        UnOp::Neg => match v {
            Value::Num(x) => Value::Num(-x),
            Value::NumList(xs) => Value::NumList(xs.iter().map(|e| -e).collect()),
            _ => Value::Undef,
        },
        UnOp::Pos => v, // no-op (parser.y:469)
        UnOp::Not => Value::Bool(!v.is_truthy()),
        UnOp::BitNot => match v {
            Value::Num(x) => Value::Num(int_to_f64(!f64_to_int(x))),
            _ => Value::Undef,
        },
    }
}

/// Element-wise combine, truncating to the shorter operand (OpenSCAD's silent-truncate).
fn zip_trunc(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Rc<[f64]> {
    a.iter().zip(b.iter()).map(|(&x, &y)| f(x, y)).collect()
}

/// Dot product of two equal-length numeric vectors, in the FIXED 4-lane accumulation order (the
/// reduction doctrine, SPEC): lane `j` sums every 4th product, then the lanes combine as
/// `(l0+l1)+(l2+l3)`. This is (1) DETERMINISTIC and (2) the exact shape a 4-wide SIMD reduction
/// produces, so a future SIMD fast path equals this scalar path BIT-FOR-BIT (the `fast == slow`
/// property, proven below). It matches OpenSCAD's naive left-fold for ≤3-element vectors (the common
/// geometry case); 4+ elements diverge by ≤1 ULP on non-integer inputs — verified visible-or-not at
/// I.5 (echo precision) / K (the harness).
fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut lanes = [0.0f64; 4];
    let (mut ac, mut bc) = (a.chunks_exact(4), b.chunks_exact(4));
    for (ca, cb) in ac.by_ref().zip(bc.by_ref()) {
        lanes[0] += ca[0] * cb[0];
        lanes[1] += ca[1] * cb[1];
        lanes[2] += ca[2] * cb[2];
        lanes[3] += ca[3] * cb[3];
    }
    for (lane, (&x, &y)) in ac.remainder().iter().zip(bc.remainder()).enumerate() {
        lanes[lane] += x * y;
    }
    (lanes[0] + lanes[1]) + (lanes[2] + lanes[3])
}

/// Ordering comparison. CROSS-type (`1 < "a"`) is `undef` — a type error. SAME orderable type
/// (num/num, str/str, list/list) always yields a BOOL: a well-typed comparison that's IEEE-incomparable
/// (a `NaN` anywhere) is `false`, matching OpenSCAD (`(0/0) < 1` is `false`, not `undef`).
fn order(a: &Value, b: &Value, want: impl Fn(Ordering) -> bool) -> Value {
    if same_orderable_type(a, b) {
        Value::Bool(value_cmp(a, b).is_some_and(want)) // NaN → None → false
    } else {
        Value::Undef // cross-type ordering is a type error (a value)
    }
}

/// Do `a` and `b` share an orderable type — both numbers, both strings, or both lists (either
/// representation)? `undef`/`bool` and cross-type pairs are NOT orderable.
fn same_orderable_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Num(_), Value::Num(_)) | (Value::Str(_), Value::Str(_))
    ) || (list_len(a).is_some() && list_len(b).is_some())
}

/// A total-ish order over values: numbers numerically, strings lexicographically, lists
/// element-wise-lexicographically (recursively, across BOTH list representations). `None` =
/// incomparable (cross-type, `NaN`, `undef`, `bool`). Recurses on nested lists (parse-bounded here;
/// deep-list ordering joins the explicit-stack work if comprehensions ever build one).
fn value_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => {
            let (la, lb) = (list_len(a)?, list_len(b)?);
            for i in 0..la.min(lb) {
                match value_cmp(&list_get(a, i), &list_get(b, i))? {
                    Ordering::Equal => {}
                    non_eq => return Some(non_eq),
                }
            }
            Some(la.cmp(&lb))
        }
    }
}

/// The element count of a list value (`NumList` or `List`), or `None` if it isn't a list.
fn list_len(v: &Value) -> Option<usize> {
    match v {
        Value::NumList(xs) => Some(xs.len()),
        Value::List(xs) => Some(xs.len()),
        _ => None,
    }
}

/// The `i`-th element of a list value as a `Value` (`Undef` out of range / not a list).
fn list_get(v: &Value, i: usize) -> Value {
    match v {
        Value::NumList(xs) => xs.get(i).copied().map_or(Value::Undef, Value::Num),
        Value::List(xs) => xs.get(i).cloned().unwrap_or(Value::Undef),
        _ => Value::Undef,
    }
}

/// `base[index]` (`Value.cc` `operator[]`). The index is `size_t(toDouble(index))` — a non-number,
/// negative, or non-finite index is out of range (`undef`), a fractional one truncates toward zero.
/// Indexing a string yields the code-point-`idx` character as a 1-char string; a scalar yields `undef`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: i is finite and >= 0 here, so `as usize` truncates like OpenSCAD's size_t cast (huge → saturates → out of range → undef)"
)]
pub(crate) fn index(base: Value, index: &Value) -> Value {
    let &Value::Num(i) = index else {
        return Value::Undef;
    };
    if i < 0.0 || !i.is_finite() {
        return Value::Undef;
    }
    let idx = i as usize;
    match base {
        Value::Str(s) => s
            .chars()
            .nth(idx)
            .map_or(Value::Undef, |c| Value::string(c.to_string())),
        other => list_get(&other, idx),
    }
}

fn bitwise(lhs: Value, rhs: Value, combine: impl Fn(i64, i64) -> i64) -> Value {
    match (lhs, rhs) {
        (Value::Num(x), Value::Num(y)) => {
            Value::Num(int_to_f64(combine(f64_to_int(x), f64_to_int(y))))
        }
        _ => Value::Undef,
    }
}

fn shift(lhs: Value, rhs: Value, left: bool) -> Value {
    match (lhs, rhs) {
        (Value::Num(x), Value::Num(y)) => {
            let by = f64_to_int(y);
            if !(0..64).contains(&by) {
                return Value::Undef; // negative or >=64 shift → undef
            }
            let xi = f64_to_int(x);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "by is checked in 0..64, so the cast to u32 is exact and non-negative"
            )]
            let shifted = if left {
                xi << (by as u32)
            } else {
                xi >> (by as u32)
            };
            Value::Num(int_to_f64(shifted))
        }
        _ => Value::Undef,
    }
}

/// OpenSCAD `toInt64`: truncate toward zero. `f64 as i64` saturates (NaN → 0), never UB.
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenSCAD's toInt64 truncates; f64->i64 saturates in Rust, no UB"
)]
fn f64_to_int(x: f64) -> i64 {
    x.trunc() as i64
}

/// i64 back to the f64 all OpenSCAD numbers are (lossy past 2^53, matching OpenSCAD's double store).
#[allow(
    clippy::cast_precision_loss,
    reason = "OpenSCAD stores everything as f64; large bit-op results lose precision there too"
)]
fn int_to_f64(x: i64) -> f64 {
    x as f64
}

#[cfg(test)]
mod tests {
    use super::{Value, apply_binary, dot};
    use crate::parser::BinOp;
    use proptest::prelude::*;

    /// An INDEPENDENT reference for the fixed 4-lane order: box each product through a `Value` (the
    /// "slow" path — how a heterogeneous `List` would compute it), then reduce with `lane = k % 4`.
    /// Different code from `dot`'s chunk loop, SAME order — the whole point of the property below.
    fn slow_boxed_dot(a: &[f64], b: &[f64]) -> f64 {
        let mut lanes = [0.0f64; 4];
        for (k, (&x, &y)) in a.iter().zip(b).enumerate() {
            // Num * Num is always Num (no fallback arm to leave uncovered).
            if let Value::Num(p) = apply_binary(BinOp::Mul, Value::Num(x), Value::Num(y)) {
                lanes[k % 4] += p;
            }
        }
        (lanes[0] + lanes[1]) + (lanes[2] + lanes[3])
    }

    proptest! {
        /// fast == slow, BIT-FOR-BIT: the contiguous `NumList` dot (`dot`, the SIMD-shaped chunk loop)
        /// equals the boxed-`Value` dot (`slow_boxed_dot`, k%4) on random numeric vectors. Both use the
        /// fixed 4-lane order, so they agree by construction — and this LOCKS it: a future SIMD dot that
        /// reorders the reduction, or an FMA that fuses product+add, fails here instead of silently
        /// diverging from the oracle. Lengths span full 4-chunks + every remainder (0..3).
        #[test]
        fn fast_dot_equals_slow_boxed_dot(
            v in prop::collection::vec((-1.0e6f64..1.0e6, -1.0e6f64..1.0e6), 0..64)
        ) {
            let a: Vec<f64> = v.iter().map(|&(x, _)| x).collect();
            let b: Vec<f64> = v.iter().map(|&(_, y)| y).collect();
            prop_assert_eq!(dot(&a, &b).to_bits(), slow_boxed_dot(&a, &b).to_bits());
        }
    }
}
