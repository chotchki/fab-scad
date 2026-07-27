use super::shape::{is_consistent, is_matrix};
use super::{bosl_assert, is_vector_core, no_progress, non_terminating, v_is_finite};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// Value-level `approx` for native callers (regions) — a SHIM over the GENERATED native, so the
/// semantics live in exactly one place. The clones are the slice ABI's price at this seam.
pub(super) fn approx_val(a: &Value, b: &Value, eps: &Value) -> crate::Result<Value> {
    super::generated::approx(&[a.clone(), b.clone(), eps.clone()])
}

/// BOSL2 `sum(v, dflt=0)` — the numeric/vector fast lane is the reference's own trick: `[for(i=v) 1]*v`
/// (a ones-vector dot / vector-matrix product through the interpreter's `*`); anything else consistent
/// (matrices…) folds through [`sum_tail`] with a `v[0]*0` seed.
pub(super) fn sum(args: &[Value]) -> crate::Result<Value> {
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

/// The reachable slice of BOSL2 `constrain` for [`vector_angle`]'s clamp: a non-NaN number clamps through
/// the real `min`/`max` builtins; a vector clamps elementwise; everything the asserts let through that ISN'T
/// one of those (undef, NaN — `is_num(NaN)` is false) falls to the reference's `assert(false)`. The matrix
/// branch (`flatten`/`list_to_matrix`) is unreachable from `vector_angle`'s asserted shapes — LOUD error, not
/// a silent wrong answer, if that proof ever breaks.
pub(super) fn constrain_clamp(v: &Value, minval: f64, maxval: f64) -> crate::Result<Value> {
    let clamp1 = |f: &Value| {
        builtins::apply(
            "max",
            &[
                Value::Num(minval),
                builtins::apply("min", &[f.clone(), Value::Num(maxval)]),
            ],
        )
    };
    match v {
        Value::Num(n) if !n.is_nan() => Ok(clamp1(v)),
        _ if is_vector_core(v) => {
            let out: Vec<Value> = iter_values_raw(v).iter().map(clamp1).collect();
            Ok(build_vector(out))
        }
        _ if is_matrix(std::slice::from_ref(v))?.is_truthy() => Err(crate::Error::Eval(
            "constrain: matrix input unreachable from vector_angle (intrinsic guard)".to_string(),
        )),
        Value::List(_) | Value::NumList(_) => {
            let out: Vec<Value> = iter_values_raw(v)
                .iter()
                .map(|vec| {
                    let row: Vec<Value> = iter_values_raw(vec).iter().map(clamp1).collect();
                    build_vector(row)
                })
                .collect();
            Ok(build_vector(out))
        }
        _ => Err(bosl_assert("constrain: invalid input")),
    }
}
