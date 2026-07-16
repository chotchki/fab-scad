use super::lists::{force_list, in_list};
use super::{bosl_assert, is_vector_core, v_is_finite, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values, ops};
use crate::parser::BinOp;

/// BOSL2 `is_def(x) = !is_undef(x)` — true iff `x` is anything but `undef`. Only the first positional arg
/// binds to `x` (extras are ignored, per OpenSCAD); zero args → `x` is `undef` → `false`.
pub(super) fn is_def(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(!matches!(
        args.first(),
        None | Some(Value::Undef)
    )))
}

/// BOSL2 `is_str(x) = is_string(x)` — true iff `x` is a string.
pub(super) fn is_str(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::Str(_)))))
}

/// BOSL2 `is_nan(x) = (x!=x)` — a value equals itself EXCEPT `NaN`, so this is true iff `x` is `NaN`. The hot
/// scalar path is native (`f64::is_nan`); any other type routes through the interpreter's own `!=` so the
/// intrinsic can't diverge from `x!=x` on an exotic input (e.g. a `NaN` inside a list, where element-wise `!=`
/// makes `[nan]!=[nan]` TRUE — a case the native scalar check would miss, but the op reproduces exactly).
pub(super) fn is_nan(args: &[Value]) -> crate::Result<Value> {
    Ok(match args.first() {
        Some(Value::Num(n)) => Value::Bool(n.is_nan()),
        other => {
            let x = other.cloned().unwrap_or(Value::Undef);
            ops::apply_binary(BinOp::Ne, x.clone(), x)
        }
    })
}

/// BOSL2 `is_finite(x) = is_num(x) && !is_nan(0*x)` — true iff `x` is a finite number. `0*x` is `NaN` when `x`
/// is `±inf`/`NaN` and `0` when finite, so the whole expression collapses to `f64::is_finite` on a number and
/// `false` on any non-number (the `is_num` short-circuit). Computing it directly erases the reference's
/// `is_num`/`is_nan`/`*` sub-evaluation — the point of the intrinsic. Proven bit-identical by the harness,
/// which interprets the reference WITH `is_nan` defined (the dependency-aware oracle).
pub(super) fn is_finite(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(
        matches!(args.first(), Some(Value::Num(n)) if n.is_finite()),
    ))
}

/// BOSL2 `_is_liststr(s) = is_list(s) || is_str(s)` — true iff `s` is a list (either representation) or a
/// string. A pure leaf: `is_list` is true for `List`/`NumList`, `is_str` for `Str`.
pub(super) fn is_liststr(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(matches!(
        args.first(),
        Some(Value::List(_) | Value::NumList(_) | Value::Str(_))
    )))
}

/// BOSL2 `_list_pattern(list)` — the shape skeleton: every non-list leaf becomes `0`, lists recurse. Results
/// coalesce through the interpreter's own `build_vector`, so a flat numeric level becomes the same `NumList`
/// the comprehension would build — VARIANT identity matters, the callers compare patterns with `==`/`!=`.
pub(super) fn list_pattern(args: &[Value]) -> crate::Result<Value> {
    Ok(list_pattern_of(args.first().unwrap_or(&Value::Undef)))
}
pub(super) fn list_pattern_of(v: &Value) -> Value {
    if v_is_list(v) {
        let out: Vec<Value> = iter_values(v).iter().map(list_pattern_of).collect();
        build_vector(out)
    } else {
        Value::Num(0.0)
    }
}

/// BOSL2 `same_shape(a,b) = is_def(b) && _list_pattern(a) == b*0` — do `a` and `b` have the same nesting
/// skeleton? `b*0` and the `==` route through `apply_binary` (`0*"str"` is undef, list `==` is elementwise),
/// and a falsy `is_def(b)` short-circuits to `false` exactly like the interpreter's `&&`.
pub(super) fn same_shape(args: &[Value]) -> crate::Result<Value> {
    if matches!(args.get(1), None | Some(Value::Undef)) {
        return Ok(Value::Bool(false)); // is_def(b) is false → && yields false
    }
    let a = args.first().cloned().unwrap_or(Value::Undef);
    let b = args.get(1).cloned().unwrap_or(Value::Undef);
    let pattern = list_pattern_of(&a);
    let b0 = ops::apply_binary(BinOp::Mul, b, Value::Num(0.0));
    let eq = ops::apply_binary(BinOp::Eq, pattern, b0);
    Ok(Value::Bool(eq.is_truthy()))
}

/// BOSL2 `is_consistent(list, pattern)` — is every element of `list` shaped like `pattern` (default: like
/// `list[0]`)? The reference compares each entry of `0*list` against the pattern with `!=`; both the zeroing
/// and the compare route through `apply_binary`, iteration through `iter_values` — so a heterogeneous list
/// (where `0*entry` is undef) answers exactly as interpreted.
pub(super) fn is_consistent(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    if !v_is_list(&list) {
        return Ok(Value::Bool(false));
    }
    let n = match &list {
        Value::List(xs) => xs.len(),
        Value::NumList(xs) => xs.len(),
        _ => 0, // unreachable: v_is_list above
    };
    if n == 0 {
        return Ok(Value::Bool(true));
    }
    let pattern = match args.get(1) {
        None | Some(Value::Undef) => list_pattern_of(&ops::index(list.clone(), &Value::Num(0.0))),
        Some(p) => list_pattern_of(p),
    };
    let zeroed = ops::apply_binary(BinOp::Mul, Value::Num(0.0), list);
    let ok = iter_values(&zeroed)
        .into_iter()
        .all(|entry| !ops::apply_binary(BinOp::Ne, entry, pattern.clone()).is_truthy());
    Ok(Value::Bool(ok))
}

/// BOSL2 `num_defined(v) = len([for(vi=v) if(!is_undef(vi)) 1])` — how many entries are defined? Iteration
/// via `iter_values` (the interpreter's own `for` expansion: a scalar iterates once, a range expands), count
/// as the `len` builtin would report it.
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
pub(super) fn num_defined(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let count = iter_values(&v)
        .iter()
        .filter(|vi| !matches!(vi, Value::Undef))
        .count();
    Ok(Value::Num(count as f64))
}

/// BOSL2 `is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON)` — THE type predicate (8.8s of self
/// time across the O.4 four). Core + the three optional clauses in reference order; the `length` assert is
/// the one raise-site; `zero` compares `norm(v) >= eps` against `!zero`; a truthy `all_nonzero` delegates to
/// the real [`all_nonzero`] with ITS default eps (the reference's inner call passes none).
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
#[allow(
    clippy::float_cmp,
    reason = "the reference's `len(v)==length` IS an exact f64 equality; a tolerance would diverge"
)]
pub(super) fn is_vector(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&v) {
        return Ok(Value::Bool(false));
    }
    let n = match &v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0, // unreachable: is_vector_core above
    } as f64;
    if let Some(length) = args.get(1)
        && !matches!(length, Value::Undef)
    {
        let Value::Num(l) = length else {
            return Err(bosl_assert("is_vector: length must be a number"));
        };
        if l.is_nan() {
            return Err(bosl_assert("is_vector: length must be a number")); // is_num(NaN) is false
        }
        if *l != n {
            return Ok(Value::Bool(false));
        }
    }
    if let Some(zero) = args.get(2)
        && !matches!(zero, Value::Undef)
    {
        let eps = args.get(4).cloned().unwrap_or(Value::Num(1e-9));
        let norm_v = builtins::apply("norm", std::slice::from_ref(&v));
        let cmp = match (&norm_v, &eps) {
            (Value::Num(nv), Value::Num(e)) => Value::Bool(nv >= e),
            _ => ops::apply_binary(BinOp::Ge, norm_v, eps),
        };
        let want = Value::Bool(!zero.is_truthy());
        if !ops::apply_binary(BinOp::Eq, cmp, want).is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    if let Some(anz) = args.get(3)
        && anz.is_truthy()
    {
        return all_nonzero(&[v]); // 1-arg → the reference's inner call takes all_nonzero's own default eps
    }
    Ok(Value::Bool(true))
}

/// BOSL2 `all_nonzero(x, eps=_EPSILON)` — a finite scalar farther than `eps` from zero, or a vector of them.
/// Exotic `eps` routes the compares through the interpreter's ops (undef-propagation intact).
pub(super) fn all_nonzero(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    if v_is_finite(&x) {
        return Ok(match (&x, &eps) {
            (Value::Num(n), Value::Num(e)) => Value::Bool(n.abs() > *e),
            _ => ops::apply_binary(
                BinOp::Gt,
                builtins::apply("abs", std::slice::from_ref(&x)),
                eps.clone(),
            ),
        });
    }
    if !is_vector_core(&x) {
        return Ok(Value::Bool(false)); // is_vector(x) && … short-circuits
    }
    let near_zero = iter_values(&x).into_iter().any(|xx| match (&xx, &eps) {
        (Value::Num(n), Value::Num(e)) => n.abs() < *e,
        _ => ops::apply_binary(
            BinOp::Lt,
            builtins::apply("abs", std::slice::from_ref(&xx)),
            eps.clone(),
        )
        .is_truthy(),
    });
    Ok(Value::Bool(!near_zero)) // `[collected…] == []`
}

/// BOSL2 `is_matrix(A,m,n,square=false)` — rectangular numeric matrix, optionally shape-pinned. Composes the
/// band's own natives: `is_vector(A[0],n)` is the fixed 2-arg call (`zero`/`all_nonzero` branches unreachable —
/// which is why this entry needs NO `_EPSILON` guard even though `is_vector`'s does), `is_consistent(A)`
/// closes it. `len(A)` participates as a TRUTHINESS value in the `m`-undef clause (`0` rows → false).
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
pub(super) fn is_matrix(args: &[Value]) -> crate::Result<Value> {
    let a = args.first().cloned().unwrap_or(Value::Undef);
    if !v_is_list(&a) {
        return Ok(Value::Bool(false));
    }
    let la = match &a {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0, // unreachable: v_is_list above
    } as f64;
    let rows_ok = match args.get(1) {
        None | Some(Value::Undef) => la != 0.0, // (is_undef(m) && len(A)) — Num truthiness
        Some(m) => ops::apply_binary(BinOp::Eq, Value::Num(la), m.clone()).is_truthy(),
    };
    if !rows_ok {
        return Ok(Value::Bool(false));
    }
    let a0 = ops::index(a.clone(), &Value::Num(0.0));
    if let Some(square) = args.get(3)
        && square.is_truthy()
    {
        let l0 = builtins::apply("len", std::slice::from_ref(&a0));
        if !ops::apply_binary(BinOp::Eq, Value::Num(la), l0).is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    let n = args.get(2).cloned().unwrap_or(Value::Undef);
    if !is_vector(&[a0, n])?.is_truthy() {
        return Ok(Value::Bool(false));
    }
    is_consistent(&[a])
}

/// BOSL2 `is_path(list, dim=[2,3], fast=false)` — a matrix of ≥2 points whose width is in `dim`;
/// composes the band's own [`is_matrix`]/[`in_list`]/[`force_list`] natives.
pub(super) fn is_path(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    let dim = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Value::num_list(vec![2.0, 3.0]));
    let fast = args.get(2).cloned().unwrap_or(Value::Bool(false));
    if fast.is_truthy() {
        return Ok(Value::Bool(
            v_is_list(&list)
                && is_vector(std::slice::from_ref(&ops::index(
                    list.clone(),
                    &Value::Num(0.0),
                )))?
                .is_truthy(),
        ));
    }
    if !is_matrix(std::slice::from_ref(&list))?.is_truthy() {
        return Ok(Value::Bool(false));
    }
    let ll = builtins::apply("len", std::slice::from_ref(&list));
    if !ops::apply_binary(BinOp::Gt, ll, Value::Num(1.0)).is_truthy() {
        return Ok(Value::Bool(false));
    }
    let row0 = ops::index(list, &Value::Num(0.0));
    let l0 = builtins::apply("len", std::slice::from_ref(&row0));
    if !ops::apply_binary(BinOp::Gt, l0.clone(), Value::Num(0.0)).is_truthy() {
        return Ok(Value::Bool(false));
    }
    if matches!(dim, Value::Undef) {
        return Ok(Value::Bool(true));
    }
    let forced = force_list(std::slice::from_ref(&dim))?;
    in_list(&[l0, forced])
}
