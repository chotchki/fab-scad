//! The numeric-JIT's math ABI (P.1.4b). The desktop JIT ([`fab_jit`](../../../jit)) can't call the
//! interpreter's private builtin dispatch, but a JIT'd `sin(x)`/`sqrt(x)`/… MUST compute EXACTLY what the
//! interpreter's builtin does, or `fast == JIT` breaks — and OpenSCAD's trig is in DEGREES via [`super::trig`]
//! (with the exact-quadrant snapping), NOT raw libm. So this module is the shared seam: the scalar math
//! builtins the JIT can inline, id'd for a stable cross-crate ABI, each computing the SAME thing as
//! [`super::builtins::apply`]. The corpus differential (`jit/tests/corpus_diff.rs`) is the drift guard — a
//! divergence here from the interpreter fails the build.
//!
//! ONE source of truth: [`MATH`] is a table indexed by id, so [`jit_math`] (dispatch) and [`jit_math_id`]
//! (name → id) can never disagree. The JIT emits `jit_math(id, a, b)` (a unary op ignores `b`).

use super::trig;

/// A scalar math op in the uniform `(f64, f64) -> f64` shape — a unary op ignores its second argument. Each
/// MUST match the corresponding arm of [`super::builtins::apply`] bit-for-bit.
type MathFn = fn(f64, f64) -> f64;

/// `sign(x)` — `+1`/`-1`/`0`, with `±0` and `NaN` → `0` (both comparisons false), matching `func.cc` and
/// [`super::builtins`]'s `sign`.
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// The JIT-inlinable scalar math builtins: `(name, arity, impl)`. The INDEX is the ABI id — reorder only by
/// appending. Every `impl` mirrors `builtins::apply` exactly (trig in degrees via [`trig`]; `atan2(y, x)`;
/// `log` is base-10; `round` is half-away-from-zero).
static MATH: &[(&str, u8, MathFn)] = &[
    ("abs", 1, |a, _| a.abs()),
    ("sign", 1, |a, _| sign(a)),
    ("sin", 1, |a, _| trig::sin_degrees(a)),
    ("cos", 1, |a, _| trig::cos_degrees(a)),
    ("tan", 1, |a, _| trig::tan_degrees(a)),
    ("asin", 1, |a, _| trig::asin_degrees(a)),
    ("acos", 1, |a, _| trig::acos_degrees(a)),
    ("atan", 1, |a, _| trig::atan_degrees(a)),
    ("atan2", 2, |a, b| trig::atan2_degrees(a, b)),
    ("floor", 1, |a, _| a.floor()),
    ("ceil", 1, |a, _| a.ceil()),
    ("round", 1, |a, _| a.round()),
    ("ln", 1, |a, _| a.ln()),
    ("log", 1, |a, _| a.log10()),
    ("exp", 1, |a, _| a.exp()),
    ("pow", 2, |a, b| a.powf(b)),
    ("sqrt", 1, |a, _| a.sqrt()),
];

/// Compute the math op with ABI id `id` on `(a, b)` — the function the JIT's `jit_math_call` helper routes to.
/// A unary op ignores `b`. An unknown id (never emitted — the JIT only uses [`jit_math_id`]'s ids) → `NaN`.
#[must_use]
pub fn jit_math(id: u16, a: f64, b: f64) -> f64 {
    MATH.get(id as usize).map_or(f64::NAN, |&(_, _, f)| f(a, b))
}

/// The `(id, arity)` for a JIT-inlinable scalar math builtin `name`, else `None` (a call the JIT declines).
/// The JIT uses this at compile time to decide whether a `Call` node is an inlinable math builtin.
#[must_use]
pub fn jit_math_id(name: &str) -> Option<(u16, u8)> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "MATH has ~17 entries; the index fits u16 with room to spare"
    )]
    MATH.iter()
        .position(|&(n, _, _)| n == name)
        .map(|i| (i as u16, MATH[i].1))
}
