//! AR.13 — the RUNTIME ABI generated code is allowed to call, and nothing else.
//!
//! A transpiled native is a Rust function that reimplements a `.scad` function by routing every
//! operation through the interpreter's own value algebra. That is the correctness argument for the
//! whole transpiler: composition is bit-identical BY CONSTRUCTION because the generated code does
//! not do its own arithmetic, it calls the same primitives the interpreter calls. What it may call
//! is exactly this module.
//!
//! Until now that list was implicit. Generated code lived inside `eval` and could therefore reach
//! any `pub(crate)` item in fab-lang, so "the emission target" was "whatever compiles". AR.14 moves
//! generated code into its own crate, which forces the list to be written down — and writing it
//! down is the point, not the tax. A surface of ten functions is one somebody can review; a surface
//! of "everything in the crate" is not.
//!
//! NOT A USER API. These are the interpreter's internals under a stable name, exposed so a sibling
//! crate can be generated against them. They are `#[doc(hidden)]` and carry no compatibility
//! promise to anyone but the transpiler that emits calls to them — the two ship together.
//!
//! Adding to this module is a DELIBERATE act. Every addition widens what a generated native can do
//! without going through the value algebra, which is the one thing the bit-identity argument rests
//! on. If the emitter wants something that is not here, the question to answer first is whether the
//! interpreter reaches the same result the same way.

pub use crate::error::Result;
/// The builtin bridge. Renamed from `apply` on the way out: `rt::apply` next to `rt::apply_binary`
/// and `rt::apply_unary` reads like a third member of that family, and it is not one — it is the
/// name-dispatched call into OpenSCAD's builtin table.
pub use crate::eval::builtins::apply as builtin;
pub use crate::eval::intrinsics::bosl_assert;
pub use crate::eval::intrinsics::native_rt::{DepthGuard, run_interpreted};
pub use crate::eval::ops::{apply_binary, apply_unary, index};
pub use crate::eval::value::Value;
pub use crate::eval::{build_range, build_vector, iter_values_native};
pub use crate::parser::{BinOp, UnOp};
