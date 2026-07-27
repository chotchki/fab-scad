//! What remains of the O.1/O.8 hand-written proof-of-concept intrinsics. The four natives that
//! lived here (`poc_sq`, `poc_near0`, `poc_outer`, `poc_isup`) were the FIRST hand code the AR.6
//! transpiler deleted: `generated.rs` now carries mechanically-derived equivalents, and the swap
//! FIXED a latent fast_eq violation — the hand `poc_sq` answered `Undef` for an equal-length
//! numeric list where the interpreted reference (`x * x`) takes the DOT PRODUCT; the generated
//! native routes through `ops::apply_binary` and cannot make that class of mistake.

use crate::eval::value::Value;

/// The Value-const guard POC's expected `UP` — built like the `[0,0,1]` literal would (a `NumList`).
/// Stays hand-written: it is the `consts_v` EXPECTATION the arm-time guard compares against, not a
/// native implementation.
pub(super) fn poc_up_value() -> Value {
    Value::num_list(vec![0.0, 0.0, 1.0])
}
