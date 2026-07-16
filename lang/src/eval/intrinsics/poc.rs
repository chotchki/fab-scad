use crate::eval::value::Value;
use crate::eval::{builtins, ops};
use crate::parser::BinOp;

/// The POC intrinsic: `x * x`. Mirrors the interpreter's `Num * Num` (and `undef` for a non-number arg, as
/// `apply_binary` yields). Deliberately trivial — it exists to exercise the mechanism, not to be fast.
pub(super) fn poc_sq(args: &[Value]) -> crate::Result<Value> {
    Ok(match args {
        [Value::Num(x)] => Value::Num(x * x),
        _ => Value::Undef,
    })
}

/// The const-guard POC: `abs(x) < _EPSILON` with `_EPSILON` baked as 1e-9 (the guard proves the bake).
/// Routes through the REAL `abs` builtin + the interpreter's own `<`, so it can't diverge on exotic inputs
/// (`abs` of a list/undef, `undef < num`) — bit-identical by construction, like `select`.
pub(super) fn poc_near0(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let a = builtins::apply("abs", &[x]);
    Ok(ops::apply_binary(BinOp::Lt, a, Value::Num(1e-9)))
}

/// The Value-const guard POC's expected `UP` — built like the `[0,0,1]` literal would (a `NumList`).
pub(super) fn poc_up_value() -> Value {
    Value::num_list(vec![0.0, 0.0, 1.0])
}

/// The Value-const-guard POC: `v == UP` with `UP` baked as `[0,0,1]` (the `consts_v` guard proves the bake).
/// The `==` routes through the interpreter's own op — exotic `v` compares exactly as interpreted.
pub(super) fn poc_isup(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    Ok(ops::apply_binary(BinOp::Eq, v, poc_up_value()))
}
