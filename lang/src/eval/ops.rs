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
        BinOp::Eq => Value::Bool(a == b), // derived PartialEq == OpenSCAD `==` for this subset
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

/// Dot product of two equal-length numeric vectors. Sequential (deterministic) sum; the fixed
/// 4-lane accumulation order (the fast==slow bitwise property) is I.1.
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

/// Ordering comparison: same-type → the type's order (NaN → `false`), cross-type → `Undef`.
fn order(a: &Value, b: &Value, want: impl Fn(Ordering) -> bool) -> Value {
    let ord = match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::NumList(x), Value::NumList(y)) => list_order(x, y),
        _ => return Value::Undef, // cross-type ordering is undef (a value)
    };
    Value::Bool(ord.is_some_and(want))
}

/// Lexicographic order of two numeric vectors; any `NaN` element makes it incomparable (`None`).
fn list_order(a: &[f64], b: &[f64]) -> Option<Ordering> {
    for (&x, &y) in a.iter().zip(b.iter()) {
        match x.partial_cmp(&y)? {
            Ordering::Equal => {}
            non_eq => return Some(non_eq),
        }
    }
    Some(a.len().cmp(&b.len()))
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
