use super::shape::is_vector;
use super::v_is_list;
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, ops};
use crate::parser::BinOp;

/// BOSL2 `_get_ear(poly, ind, eps, _i=0)` — the ear-cut driver's per-candidate scan: the first `_i` whose
/// fan triangle is convex and empty ([`tri_class_val`] + the native [`none_inside`], with [`select`]'s
/// slice for the exclusion window), else the whisker fallback. Tail recursion → loop with the
/// [`no_progress`] guard; the whisker lane's `idx(ind)` runs the real native (its assert raises on a
/// non-list `ind` exactly like the reference).
#[allow(
    clippy::similar_names,
    reason = "`ind`/`lind` ARE the reference's own parameter and let names"
)]
/// The [`PINS`]' `is_vnf(x)` as [`vnf_centroid`]'s assert needs it, composed from the band's own natives
/// (`is_vector(x[0][0], 3)` / `is_vector(x[1][0])`).
pub(super) fn is_vnf_check(fx: &dyn crate::surface::FnCtx, x: &Value) -> crate::Result<bool> {
    if !v_is_list(x) {
        return Ok(false);
    }
    let ll = builtins::apply("len", std::slice::from_ref(x));
    if !ops::apply_binary(BinOp::Eq, ll, Value::Num(2.0)).is_truthy() {
        return Ok(false);
    }
    let x0 = ops::index(x.clone(), &Value::Num(0.0));
    let x1 = ops::index(x.clone(), &Value::Num(1.0));
    if !(v_is_list(&x0) && v_is_list(&x1)) {
        return Ok(false);
    }
    let empty = build_vector(Vec::new());
    let verts_ok = ops::apply_binary(BinOp::Eq, x0.clone(), empty.clone()).is_truthy()
        || (ops::apply_binary(
            BinOp::Ge,
            builtins::apply("len", std::slice::from_ref(&x0)),
            Value::Num(3.0),
        )
        .is_truthy()
            && is_vector(
                fx,
                &[ops::index(x0.clone(), &Value::Num(0.0)), Value::Num(3.0)],
            )?
            .is_truthy());
    if !verts_ok {
        return Ok(false);
    }
    Ok(ops::apply_binary(BinOp::Eq, x1.clone(), empty).is_truthy()
        || is_vector(fx, std::slice::from_ref(&ops::index(x1, &Value::Num(0.0))))?.is_truthy())
}

/// BOSL2 `point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]]` — pad/truncate a
/// point to 3 coords. A non-list RAISES (the inline assert; the message is a locator, so the harness matches
/// on "both errored", not the text). Each coord replicates the reference ternary through the REAL `==`
/// (`undef==undef` is true → an out-of-range slot takes `fill`) and `is_truthy`, then `build_vector` coalesces
/// exactly as the interpreter does (all-numeric → `NumList`, else `List`). `fill` defaults to `0` (1-arg call).
pub(super) fn point3d(args: &[Value]) -> crate::Result<Value> {
    let p = args.first().cloned().unwrap_or(Value::Undef);
    if !matches!(p, Value::List(_) | Value::NumList(_)) {
        // Error::Assert (not Eval): this mirrors the interpreted BOSL2 `assert(is_list(p))`, so it must
        // halt-and-export like a user assert (L.5.8), identical to the function it replaces.
        return Err(crate::Error::Assert(
            "assertion failed [assert(is_list(p))]".to_string(),
        ));
    }
    let fill = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let coords = (0..3)
        .map(|i| {
            let pi = ops::index(p.clone(), &Value::Num(f64::from(i)));
            if ops::apply_binary(BinOp::Eq, pi.clone(), Value::Undef).is_truthy() {
                fill.clone()
            } else {
                pi
            }
        })
        .collect();
    Ok(build_vector(coords))
}
