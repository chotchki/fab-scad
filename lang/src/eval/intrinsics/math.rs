use super::shape::{is_consistent, is_matrix};
use super::{bosl_assert, is_vector_core, no_progress, non_terminating, v_is_finite, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// BOSL2 `approx(a,b,eps=_EPSILON)` — tolerant equality, recursing into lists. The num fast path requires
/// BOTH operands non-NaN (`is_num(NaN)` is false, so the interpreter routes NaN past that branch to the
/// list-check → `false`); an exotic (non-num) `eps` routes the compare through the interpreter's own op so
/// its undef-propagation survives. The list branch iterates pairwise (the reference's `idx(a)` is
/// `[0:1:len-1]` here — `posmod`'s assert can't fire, `len>0` when this branch differs from the `a==b` one).
pub(super) fn approx(args: &[Value]) -> crate::Result<Value> {
    let a = args.first().cloned().unwrap_or(Value::Undef);
    let b = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Num(1e-9));
    approx_val(&a, &b, &eps)
}
pub(super) fn approx_val(a: &Value, b: &Value, eps: &Value) -> crate::Result<Value> {
    use Value::{Bool, Num};
    if ops::apply_binary(BinOp::Eq, a.clone(), b.clone()).is_truthy() {
        return Ok(Bool(matches!(a, Bool(_)) == matches!(b, Bool(_))));
    }
    if let (Num(x), Num(y)) = (a, b)
        && !x.is_nan()
        && !y.is_nan()
    {
        return Ok(if let Num(e) = eps {
            Bool((x - y).abs() <= *e)
        } else {
            ops::apply_binary(
                BinOp::Le,
                builtins::apply("abs", &[Num(x - y)]),
                eps.clone(),
            )
        });
    }
    if v_is_list(a) && v_is_list(b) {
        let av = iter_values_raw(a);
        let bv = iter_values_raw(b);
        if av.len() == bv.len() {
            for (aa, bb) in av.iter().zip(bv.iter()) {
                let mismatch = if let (Num(x), Num(y)) = (aa, bb)
                    && !x.is_nan()
                    && !y.is_nan()
                {
                    if let Num(e) = eps {
                        (x - y).abs() > *e
                    } else {
                        ops::apply_binary(
                            BinOp::Gt,
                            builtins::apply("abs", &[Num(x - y)]),
                            eps.clone(),
                        )
                        .is_truthy()
                    }
                } else {
                    !approx_val(aa, bb, eps)?.is_truthy()
                };
                if mismatch {
                    return Ok(Bool(false)); // one collected entry → `[] == [..]` is false
                }
            }
            return Ok(Bool(true));
        }
    }
    Ok(Bool(false))
}

/// BOSL2 `posmod(x,m)` — the always-positive modulo. The assert passes iff both are finite numbers and
/// `approx(m,0)` (default eps) is false — i.e. `|m| > 1e-9`; then `(x%m+m)%m` routes through the
/// interpreter's own `%`/`+`.
pub(super) fn posmod(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let m = args.get(1).cloned().unwrap_or(Value::Undef);
    let ok = matches!(&x, Value::Num(n) if n.is_finite())
        && matches!(&m, Value::Num(n) if n.is_finite() && n.abs() > 1e-9);
    if !ok {
        return Err(bosl_assert(
            "posmod: input must be finite numbers, divisor nonzero",
        ));
    }
    let r = ops::apply_binary(BinOp::Mod, x, m.clone());
    let r = ops::apply_binary(BinOp::Add, r, m.clone());
    Ok(ops::apply_binary(BinOp::Mod, r, m))
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
