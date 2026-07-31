use super::{bosl_assert, is_vector_core, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// BOSL2 `unit(v, error=[[["ASSERT"]]])` — `v/norm(v)`, raising on a non-vector and (by default) on a
/// near-zero one; a caller-provided `error` value is returned instead of raising. The near-zero compare and
/// division route through ops so a `List`-shaped vector (norm → undef) degrades exactly as interpreted.
pub(super) fn unit(_fx: &dyn crate::surface::FnCtx, args: &[Value]) -> crate::Result<Value> {
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
pub(super) fn bt_search(_fx: &dyn crate::surface::FnCtx, args: &[Value]) -> crate::Result<Value> {
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
            for iv in iter_values_raw(&t0) {
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
