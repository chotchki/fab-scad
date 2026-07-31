use super::shape::is_consistent;
use super::{bosl_assert, is_vector_core, no_progress, non_terminating, v_is_finite};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// Value-level `approx` for native callers (regions) — a SHIM over the GENERATED native, so the
/// semantics live in exactly one place. The clones are the slice ABI's price at this seam.
pub(super) fn approx_val(
    fx: &dyn crate::surface::FnCtx,
    a: &Value,
    b: &Value,
    eps: &Value,
) -> crate::Result<Value> {
    super::generated::approx(fx, &[a.clone(), b.clone(), eps.clone()])
}

/// BOSL2 `sum(v, dflt=0)` — the numeric/vector fast lane is the reference's own trick: `[for(i=v) 1]*v`
/// (a ones-vector dot / vector-matrix product through the interpreter's `*`); anything else consistent
/// (matrices…) folds through [`sum_tail`] with a `v[0]*0` seed.
pub(super) fn sum(_fx: &dyn crate::surface::FnCtx, args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let dflt = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    if ops::apply_binary(BinOp::Eq, v.clone(), build_vector(Vec::new())).is_truthy() {
        return Ok(dflt);
    }
    if !is_consistent(std::slice::from_ref(&v))?.is_truthy() {
        return Err(bosl_assert("sum: non-numeric or inconsistent input"));
    }
    let v0 = ops::index(v.clone(), &Value::Num(0.0));
    if v_is_finite(&v0) || is_vector_core(&v0) {
        let n = iter_values_raw(&v).len();
        let ones = build_vector(vec![Value::Num(1.0); n]);
        return Ok(ops::apply_binary(BinOp::Mul, ones, v));
    }
    let seed = ops::apply_binary(BinOp::Mul, v0, Value::Num(0.0));
    sum_tail(&[v, seed])
}

/// BOSL2 `_sum(v,_total,_i=0)` — the fold tail: `_total + v[_i]` per index, entirely through the
/// interpreter's `+`/index (so vector/matrix accumulation is elementwise exactly as interpreted). A stuck
/// `_i` (±inf) trips the [`no_progress`] guard instead of hanging.
pub(super) fn sum_tail(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let mut total = args.get(1).cloned().unwrap_or(Value::Undef);
    let mut i = args.get(2).cloned().unwrap_or(Value::Num(0.0));
    loop {
        let ll = builtins::apply("len", std::slice::from_ref(&v));
        if ops::apply_binary(BinOp::Ge, i.clone(), ll.clone()).is_truthy() {
            return Ok(total);
        }
        if !matches!(ll, Value::Num(_)) {
            // len(v) is undef (non-list v): `_i >= undef` is never true, so the reference recurses forever
            // — only the interpreter's step budget would stop it. LOUD instead of a native hang.
            return Err(non_terminating("_sum"));
        }
        total = ops::apply_binary(BinOp::Add, total, ops::index(v.clone(), &i));
        let next_i = ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
        if no_progress(&i, &next_i) {
            return Err(non_terminating("_sum"));
        }
        i = next_i;
    }
}
