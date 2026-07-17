//! O.10 — the region-monster band's dependency tier: the small BOSL2 list/geometry helpers
//! `_region_region_intersections` reaches (`list_wrap`, `_general_line_intersection`, `flatten`,
//! `column`, `count`, `mean`, `min_index`/`max_index`, `transpose`, `pointlist_bounds`,
//! `_sort_vectors`). Every value-level helper
//! here replicates its PINNED reference exactly, routing arithmetic/indexing/builtins through the
//! interpreter's own primitives (`ops::*`, `builtins::apply`) so exotic inputs — undef, ragged rows,
//! NaN, `-0.0` — degrade bit-identically to interpreting the reference. The fast==slow batteries in
//! `tests.rs` are the proof; the registry entry lands with `_rri` itself (O.10c).

#![allow(
    dead_code,
    reason = "the band's consumer (`_region_region_intersections`, O.10c) lands next; the fast==slow \
              batteries exercise every helper meanwhile — drop this once the entry wires them"
)]

use super::math::{approx_val, sum};
use super::{bosl_assert, non_terminating, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, ops};
use crate::parser::BinOp;

/// BOSL2 `list_wrap(list, eps=_EPSILON)` — close an open path by repeating its first point, unless the
/// ends already match (`are_ends_equal`) or the list is shorter than 2.
pub(super) fn list_wrap_val(list: &Value, eps: &Value) -> crate::Result<Value> {
    let Value::Bool(true) = builtins::apply("is_list", std::slice::from_ref(list)) else {
        return Err(bosl_assert("list_wrap: not a list"));
    };
    // assert(is_finite(eps) && eps>=0)
    let finite = matches!(eps, Value::Num(e) if e.is_finite());
    if !finite || ops::apply_binary(BinOp::Lt, eps.clone(), Value::Num(0.0)).is_truthy() {
        return Err(bosl_assert("list_wrap: invalid eps"));
    }
    let n = match list {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0,
    };
    if n < 2 || are_ends_equal_val(list, eps)?.is_truthy() {
        return Ok(list.clone());
    }
    // [each list, list[0]] — element order preserved, variant re-coalesced like the interpreter's
    // `build_vector` (an all-Num wrap stays a NumList).
    let mut out: Vec<Value> = match list {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        // Can't happen (is_list held above); the input unchanged is the no-panic fallback.
        _ => return Ok(list.clone()),
    };
    out.push(ops::index(list.clone(), &Value::Num(0.0)));
    Ok(build_vector(out))
}

/// BOSL2 `are_ends_equal(list, eps=_EPSILON)` — `approx(list[0], list[len-1], eps)` behind a
/// nonempty-list assert.
pub(super) fn are_ends_equal_val(list: &Value, eps: &Value) -> crate::Result<Value> {
    let n = match list {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("are_ends_equal: must give a nonempty list")),
    };
    if n == 0 {
        return Err(bosl_assert("are_ends_equal: must give a nonempty list"));
    }
    let first = ops::index(list.clone(), &Value::Num(0.0));
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the interpreter indexes with the same f64"
    )]
    let final_elem = ops::index(list.clone(), &Value::Num((n - 1) as f64));
    approx_val(&first, &final_elem, eps)
}

/// BOSL2 `_general_line_intersection(s1, s2, eps=_EPSILON)` — the 2D segment/segment intersection:
/// `undef` on (near-)parallel, else `[point, t, u]`. Every cross/sub/mul routes through ops (`cross`
/// on 2D vectors is the scalar `a.x*b.y - a.y*b.x` builtin).
pub(super) fn gli_val(s1: &Value, s2: &Value, eps: &Value) -> crate::Result<Value> {
    let p = |v: &Value, i: f64| ops::index(v.clone(), &Value::Num(i));
    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    let denominator = builtins::apply(
        "cross",
        &[sub(p(s1, 0.0), p(s1, 1.0)), sub(p(s2, 0.0), p(s2, 1.0))],
    );
    if approx_val(&denominator, &Value::Num(0.0), eps)?.is_truthy() {
        return Ok(Value::Undef);
    }
    let t = ops::apply_binary(
        BinOp::Div,
        builtins::apply(
            "cross",
            &[sub(p(s1, 0.0), p(s2, 0.0)), sub(p(s2, 0.0), p(s2, 1.0))],
        ),
        denominator.clone(),
    );
    let u = ops::apply_binary(
        BinOp::Div,
        builtins::apply(
            "cross",
            &[sub(p(s1, 0.0), p(s2, 0.0)), sub(p(s1, 0.0), p(s1, 1.0))],
        ),
        denominator,
    );
    // s1[0] + t*(s1[1]-s1[0])
    let pt = ops::apply_binary(
        BinOp::Add,
        p(s1, 0.0),
        ops::apply_binary(BinOp::Mul, t.clone(), sub(p(s1, 1.0), p(s1, 0.0))),
    );
    Ok(build_vector(vec![pt, t, u]))
}

/// BOSL2 `flatten(l)` — ONE level: a non-list passes through; list elements splice if they are lists,
/// else ride along.
pub(super) fn flatten_val(l: &Value) -> crate::Result<Value> {
    if !v_is_list(l) {
        return Ok(l.clone());
    }
    let mut out = Vec::new();
    match l {
        // A NumList's elements are Nums — never lists — so flatten is the identity on its elements.
        Value::NumList(xs) => out.extend(xs.iter().map(|&x| Value::Num(x))),
        Value::List(xs) => {
            for a in xs.iter() {
                match a {
                    Value::NumList(inner) => out.extend(inner.iter().map(|&x| Value::Num(x))),
                    Value::List(inner) => out.extend(inner.iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
        }
        // Can't happen (v_is_list held above); an unflattened passthrough is the no-panic fallback.
        _ => return Ok(l.clone()),
    }
    Ok(build_vector(out))
}

/// BOSL2 `column(M, i)` — `[for(row=M) row[i]]` behind list/index asserts. Row indexing routes through
/// `ops::index` so a short row yields `undef` exactly as interpreted.
pub(super) fn column_val(m: &Value, i: &Value) -> crate::Result<Value> {
    if !v_is_list(m) {
        return Err(bosl_assert("column: the input is not a list"));
    }
    let is_int = matches!(i, Value::Num(x) if x.fract() == 0.0 && x.is_finite());
    if !is_int || ops::apply_binary(BinOp::Lt, i.clone(), Value::Num(0.0)).is_truthy() {
        return Err(bosl_assert("column: invalid index"));
    }
    let rows: Vec<Value> = match m {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        // Can't happen (v_is_list held above); the assert is the no-panic fallback.
        _ => return Err(bosl_assert("column: the input is not a list")),
    };
    Ok(build_vector(
        rows.into_iter().map(|row| ops::index(row, i)).collect(),
    ))
}

/// BOSL2 `count(n, s=0, step=1, reverse=false)` — `[for(i=[0:1:n-1]) s+i*step]` (or the reversed
/// range). `n` may be a LIST (its len) or any number — fractional/negative `n` must produce exactly
/// the interpreter's range expansion, so the ranges go through [`crate::eval::value::range_iter`].
pub(super) fn count_val(
    n: &Value,
    s: &Value,
    step: &Value,
    reverse: &Value,
) -> crate::Result<Value> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the reference's len(n) is the same f64"
    )]
    let n = match n {
        Value::NumList(xs) => Value::Num(xs.len() as f64),
        Value::List(xs) => Value::Num(xs.len() as f64),
        other => other.clone(),
    };
    let one = Value::Num(1.0);
    let (start, rstep, end) = if reverse.is_truthy() {
        // [n-1 : -1 : 0]
        (
            ops::apply_binary(BinOp::Sub, n, one),
            Value::Num(-1.0),
            Value::Num(0.0),
        )
    } else {
        // [0 : 1 : n-1]
        (
            Value::Num(0.0),
            one.clone(),
            ops::apply_binary(BinOp::Sub, n, one),
        )
    };
    // Non-numeric bounds can't occur on the band's inputs (`n` is always a real length); a NaN bound
    // makes `range_iter` yield nothing, which is also what the interpreter's range does off-domain.
    let as_f = |v: &Value| match v {
        Value::Num(x) => *x,
        _ => f64::NAN,
    };
    let mut out = Vec::new();
    for i in crate::eval::value::range_iter(as_f(&start), as_f(&rstep), as_f(&end)) {
        out.push(ops::apply_binary(
            BinOp::Add,
            s.clone(),
            ops::apply_binary(BinOp::Mul, Value::Num(i), step.clone()),
        ));
    }
    Ok(build_vector(out))
}

/// BOSL2 `mean(v)` — `sum(v)/len(v)` behind a nonempty-list assert; `sum` is the existing native
/// (same reference the `sum` entry pins).
pub(super) fn mean_val(v: &Value) -> crate::Result<Value> {
    let n = match v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("mean: invalid list")),
    };
    if n == 0 {
        return Err(bosl_assert("mean: invalid list"));
    }
    let total = sum(std::slice::from_ref(v))?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the reference divides by the same f64 len"
    )]
    Ok(ops::apply_binary(BinOp::Div, total, Value::Num(n as f64)))
}

/// BOSL2 `min_index(vals)` / `max_index(vals)` (the 1-arg `all=false` shape the band reaches):
/// `search(min(vals), vals)[0]` — extremum + search through the real builtins, index through ops.
pub(super) fn min_index_val(vals: &Value) -> crate::Result<Value> {
    extremum_index(vals, "min")
}

pub(super) fn max_index_val(vals: &Value) -> crate::Result<Value> {
    extremum_index(vals, "max")
}

fn extremum_index(vals: &Value, which: &'static str) -> crate::Result<Value> {
    if !super::is_vector_core(vals) {
        return Err(bosl_assert("min_index/max_index: invalid list of numbers"));
    }
    // max_index additionally asserts len>0 (min_index's reference does not, but search on an empty
    // list yields [] and [0] indexes undef — identical either way for the shapes the band reaches).
    let ext = builtins::apply(which, std::slice::from_ref(vals));
    let found = builtins::apply("search", &[ext, vals.clone()]);
    Ok(ops::index(found, &Value::Num(0.0)))
}

/// BOSL2 `transpose(M)` (the 1-arg `reverse=false` shape): rows↔columns behind the rectangularity
/// assert; a plain vector passes through.
pub(super) fn transpose_val(m: &Value) -> crate::Result<Value> {
    let nonempty = match m {
        Value::NumList(xs) => !xs.is_empty(),
        Value::List(xs) => !xs.is_empty(),
        _ => false,
    };
    if !nonempty {
        return Err(bosl_assert("transpose: input must be a nonempty list"));
    }
    let first = ops::index(m.clone(), &Value::Num(0.0));
    if !v_is_list(&first) {
        if !super::is_vector_core(m) {
            return Err(bosl_assert(
                "transpose: input must be a vector or list of lists",
            ));
        }
        return Ok(m.clone());
    }
    let rows: Vec<Value> = match m {
        Value::List(xs) => xs.to_vec(),
        // A NumList's rows are Nums, but `first` was a list — unreachable; keep the exact fallback.
        _ => return Err(bosl_assert("transpose: input has inconsistent row lengths")),
    };
    let len0 = match &first {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        // Can't happen (`first` proved a list above); the assert is the no-panic fallback.
        _ => return Err(bosl_assert("transpose: input has inconsistent row lengths")),
    };
    for row in &rows {
        let ok = match row {
            Value::NumList(xs) => xs.len() == len0,
            Value::List(xs) => xs.len() == len0,
            _ => false,
        };
        if !ok {
            return Err(bosl_assert("transpose: input has inconsistent row lengths"));
        }
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "row/column indices are far below 2^52; the interpreter indexes with the same f64"
    )]
    let out: Vec<Value> = (0..len0)
        .map(|i| {
            build_vector(
                rows.iter()
                    .map(|row| ops::index(row.clone(), &Value::Num(i as f64)))
                    .collect(),
            )
        })
        .collect();
    Ok(build_vector(out))
}

/// BOSL2 `pointlist_bounds(pts)` — per-axis `[min, max]` spread, then transpose to `[mins, maxs]`.
/// The reference projects with `pts * ident(dim)[i]` — a matrix×basis-vector product whose per-row
/// dot is the interpreter's 4-LANED `ops::dot` (sign-of-zero differs from naive column extraction:
/// `-0.0` coordinates come out `+0.0` through the lane sum) — so the projection goes through
/// `ops::apply_binary(Mul, …)`, never a hand-rolled column read.
pub(super) fn pointlist_bounds_val(pts: &Value) -> crate::Result<Value> {
    let fast_path = super::shape::is_path(&[pts.clone(), Value::Undef, Value::Bool(true)])?;
    if !fast_path.is_truthy() {
        return Err(bosl_assert("pointlist_bounds: invalid pointlist"));
    }
    let first = ops::index(pts.clone(), &Value::Num(0.0));
    let dim = match &first {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("pointlist_bounds: invalid pointlist")),
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "point dimensions are tiny; the reference builds the same f64 identity"
    )]
    let ident_rows: Vec<Value> = (0..dim)
        .map(|i| {
            build_vector(
                (0..dim)
                    .map(|j| Value::Num(if i == j { 1.0 } else { 0.0 }))
                    .collect(),
            )
        })
        .collect();
    let spread: Vec<Value> = ident_rows
        .into_iter()
        .map(|basis| {
            let spreadi = ops::apply_binary(BinOp::Mul, pts.clone(), basis);
            build_vector(vec![
                builtins::apply("min", std::slice::from_ref(&spreadi)),
                builtins::apply("max", std::slice::from_ref(&spreadi)),
            ])
        })
        .collect();
    transpose_val(&build_vector(spread))
}

/// BOSL2 `_sort_vectors(arr)` — the lexicographic 3-way quicksort: partition on column `_i`'s pivot,
/// recurse `(lesser, _i)`, `(equal, _i+1)`, `(greater, _i)`, concatenate in that order. Ported
/// ITERATIVELY (explicit work stack, output appended in-order) — BOSL2's recursion on a big
/// intersection set would ride the Rust stack otherwise (the `_group_sort_by_index` precedent). All
/// comparisons route through ops so ragged/exotic rows order exactly as interpreted.
pub(super) fn sort_vectors_val(arr: &Value) -> crate::Result<Value> {
    // Work item: sort THIS run starting at column `i`, appending its rows to the output in order.
    let mut out: Vec<Value> = Vec::new();
    let mut stack: Vec<(Vec<Value>, usize)> = vec![(value_rows(arr)?, 0)];
    let mut steps = 0usize;
    while let Some((rows, i)) = stack.pop() {
        // The interpreter stops runaway recursion only at its step budget; the native must not hang.
        steps += 1;
        if steps > 10_000_000 {
            return Err(non_terminating("_sort_vectors"));
        }
        // len(arr)<=1 → emit as-is. `_i >= len(arr[0])` on a NON-list first row is `>= undef` →
        // false in the interpreter → its recursion never terminates; LOUD here instead.
        if rows.len() <= 1 {
            out.extend(rows);
            continue;
        }
        let width = match rows.first() {
            Some(Value::NumList(xs)) => Some(xs.len()),
            Some(Value::List(xs)) => Some(xs.len()),
            _ => None,
        };
        match width {
            Some(w) if i >= w => {
                out.extend(rows);
                continue;
            }
            None => return Err(non_terminating("_sort_vectors: non-list row")),
            _ => {}
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "row counts/column indices are far below 2^52; the reference indexes the same"
        )]
        let pivot = ops::index(
            ops::index(
                build_vector(rows.clone()),
                &Value::Num((rows.len() as f64 / 2.0).floor()),
            ),
            &Value::Num(i as f64),
        );
        let (mut lesser, mut equal, mut greater) = (Vec::new(), Vec::new(), Vec::new());
        #[allow(
            clippy::cast_precision_loss,
            reason = "column index is tiny; the reference indexes entry[_i] with the same f64"
        )]
        for row in rows {
            let cell = ops::index(row.clone(), &Value::Num(i as f64));
            if ops::apply_binary(BinOp::Lt, cell.clone(), pivot.clone()).is_truthy() {
                lesser.push(row);
            } else if ops::apply_binary(BinOp::Eq, cell.clone(), pivot.clone()).is_truthy() {
                equal.push(row);
            } else if ops::apply_binary(BinOp::Gt, cell, pivot.clone()).is_truthy() {
                greater.push(row);
            }
            // A row incomparable to the pivot (NaN cell, type mismatch) lands in NO partition —
            // exactly the reference's three-filter behavior (the row is silently dropped).
        }
        // LIFO: push greater first so lesser pops (and emits) first — concat order preserved.
        stack.push((greater, i));
        stack.push((equal, i + 1));
        stack.push((lesser, i));
    }
    Ok(build_vector(out))
}

/// A list Value's elements as owned rows (the sort's working form).
fn value_rows(v: &Value) -> crate::Result<Vec<Value>> {
    match v {
        Value::NumList(xs) => Ok(xs.iter().map(|&x| Value::Num(x)).collect()),
        Value::List(xs) => Ok(xs.to_vec()),
        // The reference's len(arr) on a non-list is undef → `len(arr)<=1` false → non-terminating.
        _ => Err(non_terminating("_sort_vectors: non-list input")),
    }
}
