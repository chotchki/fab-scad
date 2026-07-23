//! Binary + unary value operations — OpenSCAD `Value.cc` semantics, bug-for-bug.
//!
//! Everything here is INFALLIBLE: a wrong/undef operand yields `Undef` (OpenSCAD's undef-propagation
//! — `Value::undef(reason)`), never an error. The load-bearing surprises (grounded from Value.cc):
//! `str + str` is `undef` (not concat), `vec * vec` (equal-length) is the DOT PRODUCT (a scalar),
//! `vec + vec` silently TRUNCATES to the shorter, `%` is `fmod` (sign of dividend), `^` is `pow`,
//! cross-type `==`/`!=` never coerce (`1 == true` → false), cross-type `< <= > >=` → `undef`.
//!
//! `&&`/`||` DO short-circuit (OpenSCAD does) — the stack machine's `ShortCircuit` task decides whether
//! the RHS runs; the `And`/`Or` arms here only combine the both-evaluated case. Vector/matrix `*` is the
//! full linear algebra (dot / matrix products) built on the lane-based `dot` (vectorizable, not
//! OpenSCAD's serial sum — the ~1-ULP difference is under the differential metric's float tolerance).

use std::cmp::Ordering;
use std::rc::Rc;

use super::value::Value;
use crate::parser::{BinOp, UnOp};

/// Apply a binary operator to two already-evaluated values. Infallible (bad types → `Undef`).
#[must_use]
pub fn apply_binary(op: BinOp, a: Value, b: Value) -> Value {
    use Value::{List, Num, NumList};
    match op {
        BinOp::Add => match (a, b) {
            (Num(x), Num(y)) => Num(x + y),
            // The SIMD kernel: two contiguous `f64` vectors, element-wise. `zip_reuse` recycles a
            // refcount-1 operand's buffer (N.2e) — a hot path in BOSL2 point loops.
            (NumList(x), NumList(y)) => Value::NumList(zip_reuse(x, y, |x, y| x + y)),
            // A nested/heterogeneous vector (a MATRIX, `[[..],[..]]`) recurses PER ROW down to the
            // NumList kernel above — so matrix `+` stays SIMD-friendly (rows are the hot loop, the outer
            // walk is just dispatch). OpenSCAD adds vectors element-wise regardless of nesting depth.
            (a, b) if list_len(&a).is_some() && list_len(&b).is_some() => {
                elementwise(BinOp::Add, &a, &b)
            }
            _ => Value::Undef,
        },
        BinOp::Sub => match (a, b) {
            (Num(x), Num(y)) => Num(x - y),
            (NumList(x), NumList(y)) => Value::NumList(zip_reuse(x, y, |x, y| x - y)),
            (a, b) if list_len(&a).is_some() && list_len(&b).is_some() => {
                elementwise(BinOp::Sub, &a, &b)
            }
            _ => Value::Undef,
        },
        BinOp::Mul => match (a, b) {
            (Num(x), Num(y)) => Num(x * y),
            (Num(s), NumList(v)) | (NumList(v), Num(s)) => Value::NumList(map_reuse(v, |e| e * s)),
            // scalar × a NESTED / heterogeneous list broadcasts element-wise, RECURSIVELY (OpenSCAD's
            // `multvecnum` multiplies each entry via `*`, so `0*[[..],[..]]` = `[0*[..], 0*[..]]`).
            (Num(s), List(v)) | (List(v), Num(s)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Mul, Num(s), e.clone()))
                    .collect::<Vec<_>>(),
            ),
            // equal-length non-empty number vectors → DOT PRODUCT (a scalar), NOT element-wise
            (NumList(x), NumList(y)) if !x.is_empty() && x.len() == y.len() => Num(dot(&x, &y)),
            // linear algebra on vectors (NumList) and matrices (List-of-NumList), per OpenSCAD's
            // `multiply_visitor`: vector×matrix, matrix×vector, matrix×matrix. Empty → undef.
            (NumList(v), List(m)) if !v.is_empty() && !m.is_empty() => vec_times_mat(&v, &m),
            (List(m), NumList(v)) if !m.is_empty() && !v.is_empty() => mat_times_vec(&m, &v),
            (List(x), List(y)) if !x.is_empty() && !y.is_empty() => mat_times_mat(&x, &y),
            _ => Value::Undef,
        },
        BinOp::Div => match (a, b) {
            (Num(x), Num(y)) => Num(x / y), // IEEE: 1/0 → inf, 0/0 → NaN
            (NumList(v), Num(s)) => Value::NumList(map_reuse(v, |e| e / s)),
            (Num(s), NumList(v)) => Value::NumList(map_reuse(v, |e| s / e)),
            // nested list ÷ scalar (and scalar ÷ nested list) recurse element-wise, like OpenSCAD's `/`.
            (List(v), Num(s)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Div, e.clone(), Num(s)))
                    .collect::<Vec<_>>(),
            ),
            (Num(s), List(v)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Div, Num(s), e.clone()))
                    .collect::<Vec<_>>(),
            ),
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
            // A heterogeneous/NESTED list (e.g. a matrix — a `List` of `NumList` rows) negates
            // element-wise, recursing: OpenSCAD's `-[[a,b],[c,d]]` = `[[-a,-b],[-c,-d]]`. Without this a
            // `-matrix` (e.g. `-rot(90)` in BOSL2's rot_inverse) collapsed to `undef` and poisoned the
            // downstream matrix math. Non-numeric leaves fall through to `undef`, matching `-"x"`.
            Value::List(items) => Value::list(
                items
                    .iter()
                    .map(|e| apply_unary(UnOp::Neg, e.clone()))
                    .collect::<Vec<_>>(),
            ),
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

/// Element-wise combine, REUSING a refcount-1 operand's buffer in place instead of allocating a fresh one
/// (N.2e). This is the move/COW buffer reuse that keeps OpenSCAD's `VectorType` fast: in a BOSL2 path loop,
/// `p + [dx, dy]` has a temporary operand whose `Rc<[f64]>` is unique here (owned, popped off the value
/// stack), so we mutate it into the result rather than malloc. Bit-IDENTICAL to [`zip_trunc`] (same `f`,
/// same element order); reuse is gated on the operand already being the result length (`== n`, the shorter),
/// so a truncating op never leaves stale tail elements. Falls back to a fresh allocation when neither
/// operand is uniquely owned (both are live variables — refcount ≥ 2).
fn zip_reuse(mut a: Rc<[f64]>, mut b: Rc<[f64]>, f: impl Fn(f64, f64) -> f64) -> Rc<[f64]> {
    let n = a.len().min(b.len());
    // Reuse `a` (or `b`) only when it's ALREADY the result length `n` (the shorter), so a truncating op
    // leaves no stale tail; `sa.iter_mut().zip(b)` then truncates to n — matching `zip_trunc`.
    if a.len() == n
        && let Some(sa) = Rc::get_mut(&mut a)
    {
        for (x, &y) in sa.iter_mut().zip(b.iter()) {
            *x = f(*x, y);
        }
        return a;
    }
    if b.len() == n
        && let Some(sb) = Rc::get_mut(&mut b)
    {
        for (y, &x) in sb.iter_mut().zip(a.iter()) {
            *y = f(x, *y);
        }
        return b;
    }
    zip_trunc(&a, &b, f)
}

/// Map `f` over a vector, REUSING its buffer in place when uniquely owned (N.2e — `scalar * v`, `v / s`,
/// `-v`). Bit-identical to `v.iter().map(f).collect()`.
fn map_reuse(mut v: Rc<[f64]>, f: impl Fn(f64) -> f64) -> Rc<[f64]> {
    if let Some(s) = Rc::get_mut(&mut v) {
        for e in s.iter_mut() {
            *e = f(*e);
        }
        return v;
    }
    v.iter().map(|&e| f(e)).collect()
}

/// Element-wise recursive `op` (`+`/`-`) over two VECTOR values, OpenSCAD's nesting-agnostic vector
/// arithmetic: pair elements to the shorter length (matching `zip_trunc`'s truncation), each pair
/// combined by `op`. Called only when a heterogeneous [`Value::List`] is on at least one side — the flat
/// `NumList op NumList` case is the inline SIMD kernel. A matrix (`List` of `NumList`) tiles down to
/// per-ROW `NumList op NumList`, so the numeric hot loop stays the vectorizable `zip_trunc`; this outer
/// walk is just row DISPATCH (cheap `Rc`-clone element access), not a per-scalar path.
fn elementwise(op: BinOp, a: &Value, b: &Value) -> Value {
    // The caller's arm guard guarantees both are lists; `unwrap_or(0)` is a no-panic floor, not a branch.
    let n = list_len(a).unwrap_or(0).min(list_len(b).unwrap_or(0));
    Value::list(
        (0..n)
            .map(|i| apply_binary(op, list_get(a, i), list_get(b, i)))
            .collect::<Vec<_>>(),
    )
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

/// The `f64` slice of an all-number vector (a `NumList` row), or `None` if `v` isn't one. Matrix
/// multiplication is `undef` on a non-rectangular / non-numeric operand (OpenSCAD warns + returns undef),
/// and our repr invariant guarantees an all-number vector is always the `NumList` fast path.
fn num_row(v: &Value) -> Option<&[f64]> {
    match v {
        Value::NumList(xs) => Some(xs),
        _ => None,
    }
}

/// Matrix × vector: `out[i] = mat[i] · vec` (OpenSCAD `multmatvec`). Every row must be numeric and
/// `vec`-length (rectangular); otherwise `undef`. The per-row `dot` is the lane-based (vectorizable) one.
fn mat_times_vec(mat: &[Value], vec: &[f64]) -> Value {
    let mut out = Vec::with_capacity(mat.len());
    for row in mat {
        match num_row(row) {
            Some(r) if r.len() == vec.len() => out.push(dot(r, vec)),
            _ => return Value::Undef,
        }
    }
    Value::num_list(out)
}

/// Vector × matrix: `out[j] = Σ_i vec[i]·mat[i][j]` (OpenSCAD `multvecmat`). Requires `vec.len() ==
/// mat.len()` (vector length == matrix row count) and a rectangular, all-numeric matrix. Columns are
/// gathered so the reduction reuses the lane-based `dot`.
fn vec_times_mat(vec: &[f64], mat: &[Value]) -> Value {
    if vec.len() != mat.len() {
        return Value::Undef;
    }
    let Some(rows) = mat.iter().map(num_row).collect::<Option<Vec<&[f64]>>>() else {
        return Value::Undef;
    };
    let cols = rows[0].len();
    if rows.iter().any(|r| r.len() != cols) {
        return Value::Undef;
    }
    let mut col = vec![0.0; rows.len()];
    let mut out = Vec::with_capacity(cols);
    for j in 0..cols {
        for (i, r) in rows.iter().enumerate() {
            col[i] = r[j];
        }
        out.push(dot(vec, &col));
    }
    Value::num_list(out)
}

/// Matrix × matrix: each left ROW is a vector times the right matrix (OpenSCAD folds it exactly this
/// way). Left column count must match right row count; a non-numeric row → `undef`.
fn mat_times_mat(a: &[Value], b: &[Value]) -> Value {
    let mut out = Vec::with_capacity(a.len());
    for row in a {
        let Some(r) = num_row(row) else {
            return Value::Undef;
        };
        match vec_times_mat(r, b) {
            Value::Undef => return Value::Undef,
            v => out.push(v),
        }
    }
    Value::list(out)
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

/// Do `a` and `b` share an orderable type — both numbers, both strings, both bools, or both lists (either
/// representation)? `undef` and cross-type pairs are NOT orderable. Two BOOLs order `false < true` (they
/// coerce to `0`/`1`): BOSL2's `compare_vals(true, false) > 0` relies on it, and OpenSCAD's `<`/`>` return
/// a real bool for a bool pair (only CROSS-type — `bool` vs `num` — stays `undef`; `compare_vals` reaches
/// those through its own type-rank test, never through `<`).
fn same_orderable_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Num(_), Value::Num(_))
            | (Value::Str(_), Value::Str(_))
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Range { .. }, Value::Range { .. })
    ) || (list_len(a).is_some() && list_len(b).is_some())
}

/// A total-ish order over values: numbers numerically, strings lexicographically, bools `false < true`,
/// lists element-wise-lexicographically (recursively, across BOTH list representations). `None` =
/// incomparable (cross-type, `NaN`, `undef`). Recurses on nested lists (parse-bounded here;
/// deep-list ordering joins the explicit-stack work if comprehensions ever build one).
fn value_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)), // false < true
        // AH.2.1 (operators-tests golden): two RANGES order as the SEQUENCES they iterate —
        // `[0:1:3] >= [0:1:3]` is true, `[1:-1:3] < [1:-1:-1]` is true (empty < non-empty).
        (
            Value::Range { start, step, end },
            Value::Range {
                start: s2,
                step: t2,
                end: e2,
            },
        ) => super::value::range_seq_cmp((*start, *step, *end), (*s2, *t2, *e2)),

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
        // A RANGE indexes to its three fields — `r[0]=start`, `r[1]=step`, `r[2]=end` (OpenSCAD `RangeType`),
        // anything else `undef`. BOSL2's `is_range(x)` leans on exactly this (`is_finite(x[0..2])`).
        Value::Range { start, step, end } => match idx {
            0 => Value::Num(start),
            1 => Value::Num(step),
            2 => Value::Num(end),
            _ => Value::Undef,
        },
        other => list_get(&other, idx),
    }
}

/// Member access `v.x` / `v.y` / `v.z` → index 0 / 1 / 2 — OpenSCAD's named vector components (the only
/// members it defines). Any other name → `undef`; the base rules (non-list, out-of-range → `undef`) are
/// [`index`]'s. BOSL2 reads coordinates this way everywhere (`corner.x`, `shift.y`, `v.z`).
pub(crate) fn member(base: Value, field: &str) -> Value {
    let axis = match field {
        "x" => 0.0,
        "y" => 1.0,
        "z" => 2.0,
        _ => return Value::Undef,
    };
    index(base, &Value::Num(axis))
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

// I.7 — Kani proofs of PANIC-FREEDOM on the arithmetic kernels that run on untrusted SCAD input
// (docs/testing-cards.md: "indices in bounds", panic-freedom on the exact loop). Symbolic primitives,
// so the guarantee is universal. Compiled only under `cargo kani`.
#[cfg(kani)]
mod proofs {
    /// `dot()`'s 4-lane tail indexes `lanes[lane]` (`lanes: [f64; 4]`) where `lane` enumerates the
    /// remainder of `chunks_exact(4)` — whose length is ALWAYS < 4 (the std guarantee: a remainder is
    /// shorter than the chunk size). So every `lane` is a valid index into the 4-lane accumulator. The
    /// invariant is modeled directly (`rem_len < 4`, a symbolic tail length) so CBMC proves the index
    /// bound without unwinding `Vec`/`chunks_exact` internals — this IS the "indices in bounds" proof.
    #[kani::proof]
    #[kani::unwind(4)]
    fn dot_tail_index_stays_in_bounds() {
        let rem_len: usize = kani::any();
        kani::assume(rem_len < 4); // chunks_exact(4).remainder().len() is always < 4
        let mut lanes = [0.0f64; 4];
        let mut lane = 0usize;
        while lane < rem_len {
            lanes[lane] += 1.0; // the tail op `lanes[lane] += x*y` — panics iff lane >= 4, proven safe
            lane += 1;
        }
    }

    /// `shift()` guards `by` into `0..64` BEFORE the shift, so `i64 << (by as u32)` / `>>` never
    /// overflow-panic (shift amount < bit width). Panic-freedom for the untrusted `<<`/`>>` path.
    #[kani::proof]
    fn guarded_shift_never_overflow_panics() {
        let by: i64 = kani::any();
        kani::assume((0..64).contains(&by)); // the exact guard in shift()
        let x: i64 = kani::any();
        let _l = x << (by as u32);
        let _r = x >> (by as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::dot;
    use proptest::prelude::*;

    /// An INDEPENDENT reference for the fixed 4-lane order: reduce products with `lane = k % 4`.
    /// Different code from `dot`'s SIMD-shaped chunk loop, SAME order — the whole point of the property
    /// below. (That boxed `Value` arithmetic matches raw `f64` is covered by the `eval_corpus` dot tests.)
    fn reference_dot(a: &[f64], b: &[f64]) -> f64 {
        let mut lanes = [0.0f64; 4];
        for (k, (&x, &y)) in a.iter().zip(b).enumerate() {
            lanes[k % 4] += x * y;
        }
        (lanes[0] + lanes[1]) + (lanes[2] + lanes[3])
    }

    proptest! {
        /// fast == slow, BIT-FOR-BIT: the contiguous `NumList` dot (`dot`, the SIMD-shaped chunk loop)
        /// equals the reference dot (`reference_dot`, k%4) on random numeric vectors. Both use the fixed
        /// 4-lane order, so they agree by construction — and this LOCKS it: a future SIMD dot that
        /// reorders the reduction, or an FMA that fuses product+add, fails here instead of silently
        /// diverging from the oracle. Lengths span full 4-chunks + every remainder (0..3).
        #[test]
        fn fast_dot_equals_the_fixed_order_reference(
            v in prop::collection::vec((-1.0e6f64..1.0e6, -1.0e6f64..1.0e6), 0..64)
        ) {
            let a: Vec<f64> = v.iter().map(|&(x, _)| x).collect();
            let b: Vec<f64> = v.iter().map(|&(_, y)| y).collect();
            prop_assert_eq!(dot(&a, &b).to_bits(), reference_dot(&a, &b).to_bits());
        }
    }
}
