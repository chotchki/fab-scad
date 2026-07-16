use super::geometry::point3d;
use super::math::constrain_clamp;
use super::shape::{is_consistent, is_vector, same_shape};
use super::{bosl_assert, is_vector_core, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values, ops};
use crate::parser::BinOp;

/// BOSL2 `unit(v, error=[[["ASSERT"]]])` — `v/norm(v)`, raising on a non-vector and (by default) on a
/// near-zero one; a caller-provided `error` value is returned instead of raising. The near-zero compare and
/// division route through ops so a `List`-shaped vector (norm → undef) degrades exactly as interpreted.
pub(super) fn unit(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&v) {
        return Err(bosl_assert("unit: invalid vector"));
    }
    let norm_v = builtins::apply("norm", std::slice::from_ref(&v));
    if ops::apply_binary(BinOp::Lt, norm_v.clone(), Value::Num(1e-9)).is_truthy() {
        return match args.get(1) {
            // default error → the sentinel → the inner assert(norm(v)>=_EPSILON) fires
            None => Err(bosl_assert("unit: cannot normalize a zero vector")),
            Some(err) => {
                if ops::apply_binary(BinOp::Eq, err.clone(), unit_sentinel()).is_truthy() {
                    Err(bosl_assert("unit: cannot normalize a zero vector"))
                } else {
                    Ok(err.clone())
                }
            }
        };
    }
    Ok(ops::apply_binary(BinOp::Div, v, norm_v))
}

/// The `unit` error-sentinel `[[["ASSERT"]]]`, built the way the literal would (`build_vector` all the way
/// down — a one-string level is a `List`).
pub(super) fn unit_sentinel() -> Value {
    build_vector(vec![build_vector(vec![build_vector(vec![Value::string(
        "ASSERT",
    )])])])
}

/// BOSL2 `vector_angle(v1,v2,v3)` — the angle between two vectors (or three points, or a pre-paired list),
/// `acos`-clamped. Assert chain in reference order with short-circuits preserved; the trig goes through the
/// REAL `acos` builtin (the exact-degree snap lives there).
#[allow(
    clippy::float_cmp,
    reason = "the reference's len(v1)==3 IS an exact f64 equality on an integer length"
)]
pub(super) fn vector_angle(args: &[Value]) -> crate::Result<Value> {
    let v1 = args.first().cloned().unwrap_or(Value::Undef);
    let v2 = args.get(1).cloned().unwrap_or(Value::Undef);
    let v3 = args.get(2).cloned().unwrap_or(Value::Undef);
    let v2_undef = matches!(v2, Value::Undef);
    let v3_undef = matches!(v3, Value::Undef);
    let ok1 = (v3_undef && (v2_undef || same_shape(&[v1.clone(), v2.clone()])?.is_truthy()))
        || is_consistent(&[build_vector(vec![v1.clone(), v2.clone(), v3.clone()])])?.is_truthy();
    if !ok1 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let ok2 = is_vector(std::slice::from_ref(&v1))?.is_truthy()
        || is_consistent(std::slice::from_ref(&v1))?.is_truthy();
    if !ok2 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let vecs = if !v3_undef {
        build_vector(vec![
            ops::apply_binary(BinOp::Sub, v1, v2.clone()),
            ops::apply_binary(BinOp::Sub, v3, v2),
        ])
    } else if !v2_undef {
        build_vector(vec![v1, v2])
    } else if matches!(
        builtins::apply("len", std::slice::from_ref(&v1)),
        Value::Num(n) if n == 3.0
    ) {
        let p = |i: f64| ops::index(v1.clone(), &Value::Num(i));
        build_vector(vec![
            ops::apply_binary(BinOp::Sub, p(0.0), p(1.0)),
            ops::apply_binary(BinOp::Sub, p(2.0), p(1.0)),
        ])
    } else {
        v1
    };
    let vecs0 = ops::index(vecs.clone(), &Value::Num(0.0));
    let vecs1 = ops::index(vecs, &Value::Num(1.0));
    let ok3 = is_vector(&[vecs0.clone(), Value::Num(2.0)])?.is_truthy()
        || is_vector(&[vecs0.clone(), Value::Num(3.0)])?.is_truthy();
    if !ok3 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let norm0 = builtins::apply("norm", std::slice::from_ref(&vecs0));
    let norm1 = builtins::apply("norm", std::slice::from_ref(&vecs1));
    let pos = |n: &Value| ops::apply_binary(BinOp::Gt, n.clone(), Value::Num(0.0)).is_truthy();
    if !(pos(&norm0) && pos(&norm1)) {
        return Err(bosl_assert("vector_angle: zero length vector"));
    }
    let dot = ops::apply_binary(BinOp::Mul, vecs0, vecs1);
    let ratio = ops::apply_binary(BinOp::Div, dot, ops::apply_binary(BinOp::Mul, norm0, norm1));
    let clamped = constrain_clamp(&ratio, -1.0, 1.0)?;
    Ok(builtins::apply("acos", std::slice::from_ref(&clamped)))
}

/// BOSL2 `vector_axis(v1,v2,v3)` — the rotation axis between two vectors (or three points, or a paired
/// list): `unit(cross(w1,w3))` with the near-(anti)parallel fallback through `UP`/`RIGHT` (the O.8
/// Value-const guard proves the bakes). `is_vector(v, zero=false)` is the guarded-eps nonzero check —
/// reproduced by calling the real [`is_vector`] native with the same arg shape. Recursion is
/// depth-bounded (three-point → two-vector → done), so plain Rust recursion is safe.
pub(super) fn vector_axis(args: &[Value]) -> crate::Result<Value> {
    let v1 = args.first().cloned().unwrap_or(Value::Undef);
    let v2 = args.get(1).cloned().unwrap_or(Value::Undef);
    let v3 = args.get(2).cloned().unwrap_or(Value::Undef);
    if is_vector_core(&v3) {
        let trio = build_vector(vec![v3.clone(), v2.clone(), v1.clone()]);
        if !is_consistent(std::slice::from_ref(&trio))?.is_truthy() {
            return Err(bosl_assert("vector_axis: bad arguments"));
        }
        return vector_axis(&[
            ops::apply_binary(BinOp::Sub, v1, v2.clone()),
            ops::apply_binary(BinOp::Sub, v3, v2),
        ]);
    }
    if !matches!(v3, Value::Undef) {
        return Err(bosl_assert("vector_axis: bad arguments"));
    }
    if matches!(v2, Value::Undef) {
        if !v_is_list(&v1) {
            return Err(bosl_assert("vector_axis: bad arguments"));
        }
        let ll = builtins::apply("len", std::slice::from_ref(&v1));
        let e = |i: f64| ops::index(v1.clone(), &Value::Num(i));
        return if ops::apply_binary(BinOp::Eq, ll, Value::Num(2.0)).is_truthy() {
            vector_axis(&[e(0.0), e(1.0)])
        } else {
            vector_axis(&[e(0.0), e(1.0), e(2.0)])
        };
    }
    let nonzero = |v: &Value| -> crate::Result<bool> {
        Ok(is_vector(&[v.clone(), Value::Undef, Value::Bool(false)])?.is_truthy())
    };
    let pair = build_vector(vec![v1.clone(), v2.clone()]);
    if !(nonzero(&v1)? && nonzero(&v2)? && is_consistent(std::slice::from_ref(&pair))?.is_truthy())
    {
        return Err(bosl_assert("vector_axis: bad arguments"));
    }
    let unit_of = |v: Value| -> crate::Result<Value> {
        let n = builtins::apply("norm", std::slice::from_ref(&v));
        point3d(&[ops::apply_binary(BinOp::Div, v, n)])
    };
    let w1 = unit_of(v1)?;
    let w2 = unit_of(v2)?;
    let gt_eps = |v: Value| {
        ops::apply_binary(
            BinOp::Gt,
            builtins::apply("norm", std::slice::from_ref(&v)),
            Value::Num(1e-6),
        )
        .is_truthy()
    };
    let far = gt_eps(ops::apply_binary(BinOp::Sub, w1.clone(), w2.clone()))
        && gt_eps(ops::apply_binary(BinOp::Add, w1.clone(), w2.clone()));
    let w3 = if far {
        w2
    } else if gt_eps(ops::apply_binary(
        BinOp::Sub,
        v_abs(std::slice::from_ref(&w2))?,
        bosl_up(),
    )) {
        bosl_up()
    } else {
        bosl_right()
    };
    unit(&[builtins::apply("cross", &[w1, w3])])
}

/// BOSL2 `v_abs(v)` — element-wise absolute value of a vector; each element through the REAL `abs`.
pub(super) fn v_abs(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&v) {
        return Err(bosl_assert("v_abs: invalid vector"));
    }
    let out: Vec<Value> = iter_values(&v)
        .iter()
        .map(|x| builtins::apply("abs", std::slice::from_ref(x)))
        .collect();
    Ok(build_vector(out))
}

/// BOSL2 `v_theta(v)` — the polar angle of a 2D/3D vector, through the REAL `atan2` and the same `.y`/`.x`
/// member reads the body does.
pub(super) fn v_theta(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let ok = is_vector(&[v.clone(), Value::Num(2.0)])?.is_truthy()
        || is_vector(&[v.clone(), Value::Num(3.0)])?.is_truthy();
    if !ok {
        return Err(bosl_assert("v_theta: invalid vector"));
    }
    let y = ops::member(v.clone(), "y");
    let x = ops::member(v, "x");
    Ok(builtins::apply("atan2", &[y, x]))
}

/// BOSL2 `UP` (= `TOP`) as the guard proves it bound: `[0,0,1]` as a `NumList`.
pub(super) fn bosl_up() -> Value {
    Value::num_list(vec![0.0, 0.0, 1.0])
}
/// BOSL2 `RIGHT`: `[1,0,0]` as a `NumList`.
pub(super) fn bosl_right() -> Value {
    Value::num_list(vec![1.0, 0.0, 0.0])
}

/// BOSL2 `_bt_search(query, r, points, tree)` — radius search over a ball tree. The reference's
/// `concat(root-hit, left, right)` tree recursion flattens to an ITERATIVE preorder DFS: the asserts force
/// every collected element to be a number, so a flat all-`Num` collection coalesces to the same `NumList`
/// the nested concats build — and an explicit stack can't blow the native stack on a crafted deep tree.
/// Assert/visit ORDER matches the interpreter (a raise in the left subtree fires before the right subtree
/// is looked at).
#[allow(
    clippy::float_cmp,
    reason = "the reference's len(tree)==1 / ==4 ARE exact f64 equalities on integer lengths"
)]
pub(super) fn bt_search(args: &[Value]) -> crate::Result<Value> {
    let query = args.first().cloned().unwrap_or(Value::Undef);
    let r = args.get(1).cloned().unwrap_or(Value::Undef);
    let points = args.get(2).cloned().unwrap_or(Value::Undef);
    let mut out: Vec<Value> = Vec::new();
    let mut stack = vec![args.get(3).cloned().unwrap_or(Value::Undef)];
    while let Some(tree) = stack.pop() {
        let ll = builtins::apply("len", std::slice::from_ref(&tree));
        let t0 = ops::index(tree.clone(), &Value::Num(0.0));
        let leaf = matches!(ll, Value::Num(n) if n == 1.0) && v_is_list(&t0);
        let node = matches!(ll, Value::Num(n) if n == 4.0)
            && matches!(&t0, Value::Num(n) if !n.is_nan())
            && matches!(ops::index(tree.clone(), &Value::Num(1.0)), Value::Num(n) if !n.is_nan());
        if !(v_is_list(&tree) && (leaf || node)) {
            return Err(bosl_assert("_bt_search: the tree is invalid"));
        }
        if leaf {
            let empty_ok =
                ops::apply_binary(BinOp::Eq, t0.clone(), build_vector(Vec::new())).is_truthy();
            if !(empty_ok || is_vector_core(&t0)) {
                return Err(bosl_assert("_bt_search: the tree is invalid"));
            }
            for iv in iter_values(&t0) {
                let d =
                    ops::apply_binary(BinOp::Sub, ops::index(points.clone(), &iv), query.clone());
                if ops::apply_binary(
                    BinOp::Le,
                    builtins::apply("norm", std::slice::from_ref(&d)),
                    r.clone(),
                )
                .is_truthy()
                {
                    out.push(iv);
                }
            }
        } else {
            let d = ops::apply_binary(BinOp::Sub, query.clone(), ops::index(points.clone(), &t0));
            let dist = builtins::apply("norm", std::slice::from_ref(&d));
            let radius = ops::apply_binary(
                BinOp::Add,
                r.clone(),
                ops::index(tree.clone(), &Value::Num(1.0)),
            );
            if ops::apply_binary(BinOp::Gt, dist.clone(), radius).is_truthy() {
                continue; // pruned subtree contributes `[]` — a no-op in the flat collection
            }
            if ops::apply_binary(BinOp::Le, dist, r.clone()).is_truthy() {
                out.push(t0);
            }
            stack.push(ops::index(tree.clone(), &Value::Num(3.0)));
            stack.push(ops::index(tree.clone(), &Value::Num(2.0)));
        }
    }
    Ok(build_vector(out))
}
