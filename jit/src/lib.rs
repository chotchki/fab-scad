//! fab-jit — a Cranelift JIT for scad-rs numeric functions (I.8 spike → P.1 production).
//!
//! NATIVE-ONLY by design. The browser can't JIT in-sandbox, so scad-rs's ONE implementation
//! everywhere is the interpreter; this crate is a native accelerator whose only reason to exist is to
//! run a hot numeric function as native code that is BIT-IDENTICAL to the interpreter (`fast == JIT`,
//! the sibling of `fast == slow`). The float-discipline recipe (docs/jit-recipe.md) is what keeps the
//! bits identical: no auto-FMA, fixed evaluation order, and every op Cranelift has no deterministic
//! native instruction for routed to a CALL into our own Rust math.
//!
//! This crate is the ONE place `unsafe` lives outside the kernel FFI: calling a finalized code pointer.
//! It's confined to [`CompiledFn::call`] and documented there. fab-lang stays `unsafe_code = forbid`.
//!
//! Numeric subset (P.1.1): a function body over `f64` parameters using number literals, parameter reads,
//! unary `-`/`+`, and `+ - * / % ^`. Anything else ([`ExprKind::Call`], ternary, indexing, a free
//! variable) returns [`JitError::Unsupported`] — the compiler never emits a wrong answer, it declines,
//! and the caller falls back to the interpreter. [`JitRegistry`] compiles many such functions into ONE
//! module (the spike leaked a module per function — the doc's #1 production gap).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use cranelift::codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift::codegen::ir::{BlockArg, FuncRef, Value};
use cranelift::jit::{JITBuilder, JITModule};
use cranelift::module::{FuncId, Linkage, Module, ModuleError, default_libcall_names};
use cranelift::prelude::{
    AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, MemFlagsData,
    settings, types,
};

/// The static RETURN shape of a compiled function — the descriptor the dispatch re-tags the untyped native
/// return by (P.1.4e + P.1.6 rung C). The interpreter carries this in [`Value`] for free; it evaporates
/// crossing `extern "C"`, so the compiled function reports it and the dispatch reconstructs the [`Value`]:
/// - `Num` — the `f64` return IS the result.
/// - `Bool` — the JIT computed an `i8` 0/1, returned as `0.0`/`1.0`; the dispatch wraps `Value::Bool`.
/// - `Vec(n)` — a fixed-shape numeric vector (rung C); the function WROTE `n` `f64`s to the sink buffer and
///   the `f64` return is a dummy, so the dispatch reads the buffer into a `Value::NumList`.
///
/// The Num-vs-Bool distinction is load-bearing (`Num(1.0) != Bool(true)` would diverge); the internal
/// per-sub-expression type lives on [`Lowered`], not here — this is ONLY the whole function's return.
#[derive(Clone, PartialEq, Eq)]
enum Ret {
    Num,
    Bool,
    Vec(usize),
    /// A fixed-shape NESTED vector — a matrix / list-of-vectors (P.1.6 rung-D 2c.1). The body wrote its
    /// `leaves` flat leaf scalars to the sink in row-major order; the `shape` tree records the nesting so the
    /// dispatch rebuilds the interpreter's nested `Value`. `Rc`-shared so cloning a [`CompiledFn`] out of the
    /// cache is a refcount bump, not a tree copy. A body whose vector is FLAT stays [`Ret::Vec`] (the fast path).
    Nested {
        shape: Rc<VShape>,
        leaves: usize,
    },
    /// A DYNAMIC-length numeric list (P.1.6 rung-D piece 2): the compiled function materialized it into the
    /// `sink` (`*mut Vec<f64>`) via a loop; the `f64` return is a dummy and the dispatch reads the sink into a
    /// `Value::NumList`. Length isn't known at compile time (unlike `Vec(n)`), so there's no descriptor.
    DynVec,
    /// A DYNAMIC-length MATRIX (P.1.6 rung-D 2c.3): the body pushed `width` scalars per row (row-major) into the
    /// sink; the dispatch RESHAPES the flat buffer into a `List` of `width`-chunks (each a `NumList` row). Same
    /// sink mechanism as `DynVec`, plus the compile-time `width` to un-flatten by.
    DynMat {
        width: usize,
    },
}

/// The nesting SHAPE of a fixed-vector return (P.1.6 rung-D 2c.1) — a tree mirroring the `Lowered::Vec` the body
/// returned. A `Leaf` pulls the next f64 from the flat sink buffer (in the order the body stored them); a `Nest`
/// is a sublist. [`rebuild_nested`] walks it to reconstruct the interpreter's `Value`, applying `build_vector`'s
/// exact rule at each level (all-`Num` children → `NumList`, else `List`) so the result is BIT-IDENTICAL.
#[derive(Clone, PartialEq, Eq)]
enum VShape {
    Leaf,
    Nest(Vec<VShape>),
}

use fab_lang::{
    BinOp, Expr, ExprKind, JitConst, JitDef, JitOutcome, NumericJit, NumericJitFactory, Parameter,
    RandStream, UnOp, jit_math_id,
};
// Aliased: Cranelift's `Value` (an IR SSA value) is used pervasively below, so the scad-lang runtime value
// (what the dispatch hands us to scalarize) rides under `ScadValue` to avoid the name clash.
use fab_lang::Value as ScadValue;

/// The runtime SHAPE of one call argument — what an on-demand specialization is keyed on (P.1.6 rung B). A
/// `Scalar` is one `f64`; a `Vec(n)` is `n` contiguous `f64`s flattened into the call buffer. The tag is
/// load-bearing, not just a length: `f(1.0)` (scalar param, body `x*2`) and `f([1.0])` (vec-1 param, body
/// `x[0]`) are the SAME 1-element buffer but DIFFERENT bodies — a bare length couldn't tell them apart.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ArgShape {
    Scalar,
    /// A list arg — a TREE of element shapes (P.1.6 rung-D 2c.2). A flat numeric vector is `Vec([Scalar; n])`; a
    /// matrix is `Vec([Vec([Scalar; c]); r])`. The nesting is load-bearing in the cache key: `f([[1,2]])` and
    /// `f([1,2])` flatten to the SAME 2 f64s but bind DIFFERENT-shaped `Lowered::Vec`s, so the shape distinguishes
    /// which specialization to compile.
    Vec(Vec<ArgShape>),
}

impl ArgShape {
    /// How many `f64` LEAF slots this arg occupies in the flat call buffer (a matrix sums its rows).
    fn size(&self) -> usize {
        match self {
            ArgShape::Scalar => 1,
            ArgShape::Vec(children) => children.iter().map(ArgShape::size).sum(),
        }
    }
}

/// One argument list's full shape — the on-demand cache key (per function name).
type ShapeSig = Vec<ArgShape>;

/// The longest numeric vector rung B will SCALARIZE into a call. A vec2/3/4 point (or a flattened ≤4×4
/// matrix) fits; a longer list — a comprehension result, `gaussian_rands`' 300k — DECLINES, because unrolling
/// it into IR would explode compile time + code size, and that dynamic-length case is rung D's sink-return
/// ABI, not scalarization.
const MAX_VEC_ARG: usize = 16;

/// The total LEAF-scalar budget for one scalarized (possibly nested) arg (P.1.6 rung-D 2c.2). A matrix nests, so
/// a per-level [`MAX_VEC_ARG`] alone would allow `16^depth` leaves — this caps the whole tree so IR unroll stays
/// bounded. A 4×4 (16) or 8×8 (64) matrix fits; bigger DECLINES to the interpreter.
const MAX_FLAT_ARG: usize = 64;

/// Derive each arg's [`ArgShape`] and flatten its `f64` LEAVES into `flat` (cleared first), or `None` if any arg
/// isn't scalarizable: a too-deep/too-wide list, a non-numeric leaf (string / undef / range / function), or a
/// list level longer than [`MAX_VEC_ARG`] / a whole arg over [`MAX_FLAT_ARG`] leaves. A `Num` → one leaf +
/// `Scalar`; a `NumList` → its elements + `Vec([Scalar; n])`; a NESTED `List` (a matrix) → its rows recursively
/// (2c.2). `flat` is the registry's REUSED scratch, so the hot path pays no per-call flatten allocation.
fn shape_and_flatten(args: &[ScadValue], flat: &mut Vec<f64>) -> Option<ShapeSig> {
    flat.clear();
    let mut sig = ShapeSig::with_capacity(args.len());
    for a in args {
        let start = flat.len();
        let shape = shape_and_flatten_one(a, flat)?;
        // Bound the IR unroll: a too-large (nested) arg declines, rolling back its leaves.
        if shape.size() > MAX_FLAT_ARG {
            flat.truncate(start);
            return None;
        }
        sig.push(shape);
    }
    Some(sig)
}

/// One arg's [`ArgShape`], pushing its leaf `f64`s to `flat` in row-major order — recursing into a nested `List`
/// (a matrix) so `[[a,b],[c,d]]` flattens to `[a,b,c,d]` with shape `Vec([Vec([Scalar;2]);2])` (2c.2). A list
/// level over [`MAX_VEC_ARG`] or a non-numeric leaf → `None` (rolling back that sublist's leaves). The whole-arg
/// [`MAX_FLAT_ARG`] cap is enforced by the caller.
fn shape_and_flatten_one(value: &ScadValue, flat: &mut Vec<f64>) -> Option<ArgShape> {
    match value {
        ScadValue::Num(n) => {
            flat.push(*n);
            Some(ArgShape::Scalar)
        }
        ScadValue::NumList(xs) => {
            if xs.len() > MAX_VEC_ARG {
                return None;
            }
            flat.extend_from_slice(xs);
            Some(ArgShape::Vec(vec![ArgShape::Scalar; xs.len()]))
        }
        ScadValue::List(items) => {
            if items.len() > MAX_VEC_ARG {
                return None;
            }
            let start = flat.len();
            let mut children = Vec::with_capacity(items.len());
            for it in items.iter() {
                match shape_and_flatten_one(it, flat) {
                    Some(c) => children.push(c),
                    // A non-numeric / too-big nested element → roll back this sublist's leaves + decline.
                    None => {
                        flat.truncate(start);
                        return None;
                    }
                }
            }
            Some(ArgShape::Vec(children))
        }
        // A string / undef / range / function value can't be a numeric JIT arg → decline the whole call.
        _ => None,
    }
}

/// The `%` an OpenSCAD `%` compiles to — the EXACT op the interpreter runs (`ops.rs`: `x % y`, C
/// `fmod` semantics, sign of the dividend). Routed as a call so the bits match, since Cranelift has no
/// deterministic float-remainder instruction. `extern "C"` so Cranelift can call it by symbol.
extern "C" fn jit_fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// The `^` an OpenSCAD `^` compiles to — the interpreter's `x.powf(y)` (`ops.rs`), routed as a call
/// (pow is a library transcendental, never a native instruction) so `fast == JIT` holds bit-for-bit.
extern "C" fn jit_powf(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// `min(a, b)` — the interpreter's `a.min(b)` (`builtins::min_max`'s fold step). Routed as a CALL, NOT
/// Cranelift's `fmin`: `fmin` PROPAGATES NaN (returns NaN if either operand is NaN), but `f64::min` IGNORES
/// it (returns the non-NaN operand) — they diverge on NaN (and on signed zero). The call guarantees the
/// interpreter's exact IEEE-minNum semantics, so `fast == JIT` holds.
extern "C" fn jit_fmin(a: f64, b: f64) -> f64 {
    a.min(b)
}

/// `max(a, b)` — the interpreter's `a.max(b)`. Routed as a call for the same reason as [`jit_fmin`]
/// (`f64::max` is maxNum: ignores NaN; Cranelift `fmax` propagates it).
extern "C" fn jit_fmax(a: f64, b: f64) -> f64 {
    a.max(b)
}

/// The working-set BUDGET for a JIT'd comprehension (P.1.6 rung-D piece 2). If a loop's element count would
/// exceed this, the compiled function BAILS to the interpreter (via the `raised` flag) rather than materialize
/// a giant `Vec<f64>` — an 8 MB working set at 1e6 `f64`s. NOT a correctness gate: the interpreter caps ranges
/// at `RANGE_MAX` (1e7) but iterates lists UNCAPPED, and the over-cap warning is deferred (I.5), so a decline
/// is always safe. A named knob so it's tunable once real corpus data lands (chotchki).
const COMPREHENSION_BUDGET: i64 = 1_000_000;

/// The dynamic-list ARENA for one JIT call (P.1.6 rung-D piece 2): it OWNS every `DynList` a comprehension
/// materializes, so first-class dynamic lists (bind to a `let`, `len`, iterate, compose) have somewhere to
/// live and a single owner to free. The dispatch (`call_numeric`) makes one per call and drops it after —
/// freeing all lists — but FIRST takes the one flagged `result` out into the returned `NumList`.
///
/// `lists` boxes each `Vec<f64>` so a list HANDLE (`*mut Vec<f64>`) stays valid as `lists` grows (the outer
/// `Vec` reallocating moves the boxes, not the boxed `Vec<f64>`s the handles aim at). `result` is the handle a
/// `DynVec`-returning body sets via [`jit_set_result`]; null until then.
struct JitArena {
    #[allow(
        clippy::vec_box,
        reason = "the Box is LOAD-BEARING: a DynList handle is a `*mut Vec<f64>` into `lists`, and it must \
                  survive `lists` reallocating — the Box keeps the inner `Vec<f64>` put while the outer Vec moves"
    )]
    lists: Vec<Box<Vec<f64>>>,
    result: *mut Vec<f64>,
}

impl JitArena {
    fn new() -> Self {
        JitArena {
            lists: Vec::new(),
            result: core::ptr::null_mut(),
        }
    }
}

/// The crate's THIRD unsafe seam (after the fn-ptr call + [`jit_rand_next`]) — the dynamic-list helpers, all
/// confined here. Each takes a raw pointer the compiled function passes; the caller (dispatch → native code)
/// guarantees a live, exclusively-accessed target for the call's duration (single-threaded, native code the
/// sole accessor). A non-comprehension body calls none of them, so its unused arena pointer is never touched.
///
/// Allocate a fresh empty `DynList` in `arena` and return its handle. `# Safety`: `arena` is a live
/// `*mut JitArena`.
extern "C" fn jit_arena_new_list(arena: *mut JitArena) -> *mut Vec<f64> {
    // SAFETY: `arena` is a live, exclusively-accessible `JitArena` (caller's contract).
    let arena = unsafe { &mut *arena };
    arena.lists.push(Box::new(Vec::new()));
    // The boxed `Vec<f64>` never moves as `lists` grows, so this handle stays valid for the whole call.
    &mut **arena.lists.last_mut().expect("just pushed")
}

/// Push one value onto a `DynList`. `# Safety`: `list` is a live handle from [`jit_arena_new_list`].
extern "C" fn jit_vec_push(list: *mut Vec<f64>, v: f64) {
    // SAFETY: `list` is a live, exclusively-accessible `Vec<f64>` (caller's contract).
    unsafe { &mut *list }.push(v);
}

/// A `DynList`'s element count. `# Safety`: `list` is a live handle.
#[allow(
    clippy::cast_possible_wrap,
    reason = "a JIT'd list is budget-capped at COMPREHENSION_BUDGET (1e6), far below i64::MAX"
)]
extern "C" fn jit_vec_len(list: *mut Vec<f64>) -> i64 {
    // SAFETY: `list` is a live, exclusively-accessible `Vec<f64>` (caller's contract).
    unsafe { &*list }.len() as i64
}

/// A `DynList`'s element `i` — the JIT only calls this for `0 <= i < len` (iteration), so it's always in-range.
/// `# Safety`: `list` is a live handle AND `i` is in `0..list.len()` (the JIT's loop guarantees it).
#[allow(
    clippy::cast_sign_loss,
    reason = "the JIT passes only 0 <= i < len (the loop induction var), so the cast is non-negative"
)]
extern "C" fn jit_vec_get(list: *mut Vec<f64>, i: i64) -> f64 {
    // SAFETY: `list` is live and `i` is in-range (caller's contract) — an indexing bug is a JIT codegen bug.
    let list = unsafe { &*list };
    list[i as usize]
}

/// Bounds-resolve a DYNAMIC index into a DynList (P.1.6 rung-D 2b.2b), replicating `ops::index` EXACTLY: `i <
/// 0` or non-finite → out-of-range; else `i as usize` (Rust's saturating truncation, matching the interpreter),
/// in-range iff `< len`. Returns the floored index as `i64`, or `-1` for out-of-range — where the interpreter
/// yields `undef`, which the JIT can't represent, so the caller BAILS to the interpreter. `# Safety`: `list` is
/// a live handle.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded finite && >= 0; `i as usize` matches the interpreter's `as_index` truncation (saturating)"
)]
extern "C" fn jit_vec_bound(list: *mut Vec<f64>, i: f64) -> i64 {
    if i < 0.0 || !i.is_finite() {
        return -1; // the interpreter's `undef`
    }
    // SAFETY: `list` is a live, exclusively-accessible `Vec<f64>` (caller's contract).
    let len = unsafe { &*list }.len();
    let idx = i as usize;
    if idx < len { idx as i64 } else { -1 }
}

/// Flag `list` as the function's RETURN value; the dispatch reads it out into the `NumList`. `# Safety`:
/// `arena` + `list` are live.
extern "C" fn jit_set_result(arena: *mut JitArena, list: *mut Vec<f64>) {
    // SAFETY: `arena` is a live, exclusively-accessible `JitArena` (caller's contract).
    unsafe { &mut *arena }.result = list;
}

/// The element count of a range `[start:step:end]` (P.1.6 rung-D piece 2) — routed to the interpreter's EXACT
/// [`fab_lang::range_len`] (the `step==0`/non-finite/wrong-direction → 0 logic + the `RANGE_MAX` cap) so the
/// JIT's loop bound is bit-identical. Returned as `i64` (capped at `RANGE_MAX` = 1e7, which fits).
#[allow(
    clippy::cast_possible_wrap,
    reason = "range_len is capped at RANGE_MAX (1e7), far below i64::MAX — the cast never wraps"
)]
extern "C" fn jit_range_len(start: f64, step: f64, end: f64) -> i64 {
    fab_lang::range_len(start, step, end) as i64
}

/// The interpreter's `as_index` on a count/index `n` (P.1.6 rung-D 2b.3) — `n.is_finite() && n >= 0.0` →
/// `n as usize` (TRUNCATED, matching), else `-1` (the interpreter's `undef`). Used for a DYNAMIC-count
/// `rands(min, max, count)`: a valid count drives the draw loop; `-1` → the JIT bails (undef). A count over
/// [`COMPREHENSION_BUDGET`] is caught by the caller's budget check, so the `i64` doesn't need its own cap.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded finite && >= 0; `n as i64` saturates a huge count to i64::MAX (→ the budget bail)"
)]
extern "C" fn jit_as_index(n: f64) -> i64 {
    if n.is_finite() && n >= 0.0 {
        n as i64
    } else {
        -1
    }
}

/// The seedless-`rands` helper (P.1.6 rung-D piece 1) — advances the WOVEN [`RandStream`] by ONE draw, through
/// the SAME [`RandStream::next_one`] the interpreter's seedless path uses, so the draw sequence AND the `draws`
/// fence counter stay bit-identical. A JIT'd `rands(min, max, count)` calls this `count` times in source order;
/// the stream is `Ctx::rand_stream`, woven in by pointer through the ABI (a 2.5 KB MT19937 state can't ride a
/// register, and the doctrine forbids SEEDING inside the JIT — only advancing the caller's live stream).
///
/// This is the SECOND (and only other) unsafe seam of the crate, after the compiled-fn-pointer call — confined
/// and documented HERE. `stream` is a raw `*mut RandStream`; the caller (the eval dispatch → the compiled
/// function) guarantees it is a live stream with EXCLUSIVE single-threaded access for the call's duration (the
/// dispatch hands out `RefCell::as_ptr` and holds no `RefMut`, and native code is the sole accessor while it
/// runs). A non-rands body never calls this, so an unused stream pointer is never dereferenced.
extern "C" fn jit_rand_next(stream: *mut RandStream, min: f64, max: f64) -> f64 {
    // SAFETY: per the contract above, `stream` is a live, exclusively-accessible `RandStream` for this call.
    let stream = unsafe { &mut *stream };
    stream.next_one(min, max)
}

/// A scalar math builtin (`sin`/`sqrt`/`abs`/…) an OpenSCAD `Call` compiles to (P.1.4b). Routed to
/// [`fab_lang::jit_math`] — the SAME computation the interpreter's builtin does (OpenSCAD trig in degrees
/// via our `trig`, not raw libm), so `fast == JIT` holds. `id` selects the op; a unary op ignores `b`.
extern "C" fn jit_math_call(id: u32, a: f64, b: f64) -> f64 {
    fab_lang::jit_math(id as u16, a, b)
}

/// Why a numeric function couldn't be JIT-compiled. The compiler DECLINES rather than guess — an
/// unsupported node means "fall back to the interpreter", never a divergent result.
#[derive(Debug)]
pub enum JitError {
    /// A construct outside the numeric subset (a call, ternary, index, non-parameter identifier, or a
    /// non-arithmetic operator). Carries a short reason.
    Unsupported(&'static str),
    /// A Cranelift codegen/module failure (setup, verify, define, or finalize).
    Cranelift(String),
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Unsupported(why) => write!(f, "cannot JIT: {why}"),
            JitError::Cranelift(e) => write!(f, "cranelift: {e}"),
        }
    }
}

impl std::error::Error for JitError {}

/// A finalized numeric function: `extern "C" fn(*const f64, *mut u8, *mut f64) -> f64` as a raw code pointer.
/// The executable memory it points into is owned by the [`JitFn`] or [`JitRegistry`] that produced it — a
/// `CompiledFn` is only valid for that owner's lifetime. Cheap `Clone` — a pointer, two words, and (for a
/// nested return, 2c.1) an `Rc<VShape>` bump — so the registry can lift one out of its cache and call it AFTER
/// releasing the `RefCell` borrow, the code staying mapped as long as the module (in the registry) is alive.
#[derive(Clone)]
pub struct CompiledFn {
    code: *const u8,
    /// The number of `f64` slots the compiled function reads from the params pointer — the FLAT element count
    /// of this specialization's args (P.1.6 rung B): a scalar param is 1 slot, a vec-`n` param is `n`. For an
    /// all-scalar function this equals the parameter count (the pre-rung-B behavior).
    arity: usize,
    /// The STATIC return descriptor (P.1.4e + rung C): `Num`/`Bool` come back in the `f64` return; `Vec(n)` is
    /// written to the sink out-buffer. The dispatch re-tags into the matching [`fab_lang::Value`].
    ret_ty: Ret,
}

impl CompiledFn {
    /// The parameter count the compiled function expects.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.arity
    }

    /// Whether this function returns a BOOLEAN (a predicate / comparison / bool literal) rather than a number
    /// (P.1.4e) — the dispatch wraps its `0.0`/`1.0` result in `Value::Bool`, and the differential compares
    /// type-aware. `false` for the numeric majority.
    #[must_use]
    pub fn returns_bool(&self) -> bool {
        matches!(self.ret_ty, Ret::Bool)
    }

    /// Call the compiled function with `params` (length == [`CompiledFn::arity`]), a sink `out` buffer for a
    /// vector return (P.1.6 rung C), and the woven `rand` [`RandStream`] pointer (P.1.6 rung-D piece 1). A
    /// `Vec(n)` function writes its `n` elements into `out` (which MUST hold ≥ `n` slots) and the returned
    /// `f64` is a dummy; a `Num`/`Bool` function ignores `out`. A seedless-`rands` body advances `*rand`; a
    /// non-rands body never dereferences it (pass `null` freely). Returns `None` when the body's inline
    /// `assert` FAILED — the JIT can't unwind, so it flags a status byte and the caller falls back.
    ///
    /// # Panics
    /// If `params.len()` != the function's arity, or `out` is too small for a `Vec(n)` return.
    ///
    /// # Safety
    /// `rand` must be either null (for a body known not to draw) or a live, exclusively-accessible
    /// `*mut RandStream` for the call's duration (see [`jit_rand_next`]); likewise `arena` must be either null
    /// (for a body with no comprehension) or a live, exclusively-accessible `*mut JitArena` (see [`JitArena`]).
    #[must_use]
    pub unsafe fn call(
        &self,
        params: &[f64],
        out: &mut [f64],
        rand: *mut core::ffi::c_void,
        arena: *mut core::ffi::c_void,
    ) -> Option<f64> {
        assert_eq!(
            params.len(),
            self.arity,
            "CompiledFn::call arity mismatch: got {}, expected {}",
            params.len(),
            self.arity
        );
        match &self.ret_ty {
            Ret::Vec(n) => {
                assert!(
                    out.len() >= *n,
                    "CompiledFn::call out buffer too small: {} < {n}",
                    out.len()
                );
            }
            // A nested (matrix) return writes its FLAT leaf count into the same sink (2c.1).
            Ret::Nested { leaves, .. } => {
                assert!(
                    out.len() >= *leaves,
                    "CompiledFn::call out buffer too small: {} < {leaves}",
                    out.len()
                );
            }
            _ => {}
        }
        // THE unsafe seam of the whole crate. SAFETY: `code` is a finalized Cranelift function of signature
        // `extern "C" fn(*const f64, *mut u8, *mut f64, *mut RandStream, *mut JitArena) -> f64` (built in
        // `define_one`); the owning module keeps it mapped as long as `self` is reachable. It READS `arity`
        // f64s from the first pointer (`params` has exactly that many, asserted), WRITES one byte through the
        // second (`raised`), WRITES `n` f64s through the third only for a `Vec(n)` return (`out`, asserted ≥ n),
        // passes the fourth to `jit_rand_next` only on a seedless-`rands` body, and the fifth to the dynamic-list
        // helpers only on a comprehension body — the caller's contract per the # Safety note. No unwinding
        // crosses the boundary.
        let f: unsafe extern "C" fn(
            *const f64,
            *mut u8,
            *mut f64,
            *mut core::ffi::c_void,
            *mut core::ffi::c_void,
        ) -> f64 = unsafe { std::mem::transmute(self.code) };
        let mut raised: u8 = 0;
        let result = unsafe {
            f(
                params.as_ptr(),
                &raw mut raised,
                out.as_mut_ptr(),
                rand,
                arena,
            )
        };
        if raised == 0 { Some(result) } else { None }
    }
}

/// A single JIT-compiled numeric function that OWNS its module (the standalone-compile API, used by the
/// fast==JIT differential). For compiling many functions, prefer [`JitRegistry`] — one module for all.
pub struct JitFn {
    // Keeps the finalized code mapped. Cranelift places code at a fixed address, so moving the struct
    // doesn't invalidate the pointer inside `inner`.
    _module: JITModule,
    inner: CompiledFn,
}

impl JitFn {
    /// The parameter count the compiled function expects.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.inner.arity()
    }

    /// Call the compiled function with `params` (length must equal [`JitFn::arity`]). `None` if the body's
    /// inline `assert` failed (see [`CompiledFn::call`]). The standalone API is `Num`-only (it declines a bool
    /// or vector return) AND declines a seedless-`rands` body (no stream to weave), so a 1-slot dummy
    /// out-buffer and a NULL rand pointer suffice — the compiled body never touches either.
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> Option<f64> {
        // SAFETY: `compile_function` declines a rands body AND is `Num`-only (no vec/comprehension), so this
        // body never dereferences the null rand/arena pointers nor writes the out-buffer.
        unsafe {
            self.inner.call(
                params,
                &mut [0.0],
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        }
    }
}

/// One user function's compile inputs, OWNED (P.1.6 rung B). The registry CLONES these at build so it can
/// RECOMPILE a function for a new arg SHAPE on demand — the interpreter's AST is borrowed only during
/// `build`, which keeps `Box<dyn NumericJit>` `'static` (Ctx.jit needs no lifetime). `Expr`/`Parameter` are
/// `Clone`; the clone is one-time per program.
struct OwnedDef {
    params: Vec<Parameter>,
    body: Expr,
}

/// The compiled-function store: a program's user functions compiled into ONE [`JITModule`], with a
/// per-`(name, arg-shape)` specialization cache (P.1.6 rung B). The all-SCALAR shape of every function is
/// pre-compiled at `build` (the hot path stays warm + feeds the EXPLAIN report); a VECTOR-arg shape is
/// compiled LAZILY on first sight during eval and memoized. Because [`JITModule`] finalizes incrementally, a
/// later on-demand `define`+`finalize` leaves every earlier code pointer valid.
///
/// The `RefCell`s carry the interior mutability the lazy path needs behind `NumericJit`'s `&self`: the module
/// (to define new specializations), the cache (to memoize them), and a reused flatten scratch. A JIT'd body
/// is pure native math and never re-enters the interpreter, so a compiled function can't recursively call
/// back into `call_numeric` — no `RefCell` is ever borrowed twice.
pub struct JitRegistry {
    /// The one module every specialization compiles into. `RefCell` because rung B defines new shapes through
    /// `&self` during eval.
    module: RefCell<JITModule>,
    /// The external math helpers, declared once at build; their `FuncId`s are module-stable and reused by
    /// every specialization (scalar and on-demand alike).
    helpers: Helpers,
    /// Every user function, OWNED, for on-demand recompile + inlining. Name-keyed.
    defs: BTreeMap<String, OwnedDef>,
    /// Top-level constants, OWNED (P.1.4 globals): a free variable resolves by compiling its value-expr.
    globals: BTreeMap<String, Expr>,
    /// name → (arg-shape → the compiled specialization, or `None` if that shape DECLINES). The all-scalar
    /// shape is pre-filled at build; a vector shape is compiled + memoized on first sight. `None` memoizes a
    /// decline, so a declining shape compiles at most once (never re-attempted per call).
    cache: RefCell<BTreeMap<String, BTreeMap<ShapeSig, Option<CompiledFn>>>>,
    /// Reused flatten scratch — the current call's arg `f64`s. Single-threaded, so one buffer avoids a
    /// per-call allocation on the hot path.
    scratch: RefCell<Vec<f64>>,
    /// Monotone counter for a unique export symbol per specialization (Cranelift needs distinct symbols).
    next_symbol: Cell<usize>,
    /// Build-time decline reasons for the ALL-SCALAR shape (name → first out-of-subset blocker) — the
    /// absorption-ceiling histogram for `FAB_JIT_EXPLAIN`. Vector shapes compile on demand and aren't here.
    declined: BTreeMap<String, &'static str>,
    /// How many functions compiled their all-scalar shape at build — the EXPLAIN coverage count.
    scalar_compiled: usize,
    /// Runtime activity counters (P.1.5): what the JIT actually DID this eval — offered calls, per-name
    /// fires, and the decline taxonomy. Reported at drop under `FAB_JIT_EXPLAIN`; the increments are cheap
    /// enough to keep unconditionally.
    stats: RefCell<JitStats>,
}

/// One eval's runtime JIT activity (P.1.5) — the "is it even firing?" numbers the coverage report can't
/// give. `offered` counts calls that passed the name gate (the flatten/lookup tax was paid); the rest
/// partition its outcomes.
#[derive(Default)]
struct JitStats {
    /// Calls to a name the registry owns (compiled OR declined) — each paid the shape/flatten scan.
    offered: u64,
    /// Args didn't scalarize (nested/oversized/non-numeric) — interpreted.
    shape_declined: u64,
    /// The (name, shape) is memoized as out-of-subset — interpreted.
    subset_declined: u64,
    /// An inline assert raised; the interpreter re-ran the call to raise the real error.
    assert_bailed: u64,
    /// Completed native calls, per function name.
    fired: BTreeMap<String, u64>,
}

impl JitRegistry {
    /// Own every function in `defs` + constant in `consts`, then PRE-COMPILE each function's all-SCALAR
    /// specialization into one module (the hot path + the EXPLAIN coverage signal). A function outside the
    /// numeric subset for all-scalar args is recorded as declined, not fatal — and MAY still compile later for
    /// a vector-arg shape (rung B, on demand). An empty `defs` is a valid, empty registry.
    ///
    /// # Errors
    /// [`JitError::Cranelift`] only for a module-level failure (ISA/module setup, or the single build-time
    /// `finalize_definitions`) — a per-function decline is swallowed, never surfaced as an error.
    pub fn build<'a>(
        defs: impl IntoIterator<Item = (&'a str, &'a [Parameter], &'a Expr)>,
        consts: impl IntoIterator<Item = (&'a str, &'a Expr)>,
    ) -> Result<Self, JitError> {
        // Own the AST up front so the registry can recompile any function for a new arg shape later.
        let owned_defs: BTreeMap<String, OwnedDef> = defs
            .into_iter()
            .map(|(n, p, b)| {
                (
                    n.to_string(),
                    OwnedDef {
                        params: p.to_vec(),
                        body: b.clone(),
                    },
                )
            })
            .collect();
        let owned_globals: BTreeMap<String, Expr> = consts
            .into_iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect();

        let mut module = new_module()?;
        let helpers = declare_helpers(&mut module)?;

        let mut cache: BTreeMap<String, BTreeMap<ShapeSig, Option<CompiledFn>>> = BTreeMap::new();
        let mut declined: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut scalar_compiled = 0usize;
        let mut next_symbol = 0usize;
        {
            // Borrow-maps over the OWNED AST for the compiler (every function visible to every other, so a
            // caller can inline any callee incl. a forward reference). Built once for the whole build pass.
            let fn_defs: FnDefs = owned_defs
                .iter()
                .map(|(n, d)| (n.as_str(), (d.params.as_slice(), &d.body)))
                .collect();
            let globals: Globals = owned_globals.iter().map(|(n, v)| (n.as_str(), v)).collect();
            // Declare + define each function's all-scalar shape, remembering its FuncId to resolve the code
            // pointer AFTER the single finalize. `Vec(name, FuncId, flatlen, ret_ty)`.
            let mut pending: Vec<(&str, FuncId, usize, Ret)> = Vec::new();
            for (name, d) in &owned_defs {
                let params: Vec<(&str, ArgShape)> = d
                    .params
                    .iter()
                    .map(|p| (p.name.as_ref(), ArgShape::Scalar))
                    .collect();
                let symbol = format!("scad_jit_{next_symbol}");
                next_symbol += 1;
                match define_one(
                    &mut module,
                    &symbol,
                    &params,
                    &d.body,
                    &fn_defs,
                    &globals,
                    &helpers,
                ) {
                    // all-scalar flatlen == parameter count.
                    Ok((func_id, ret_ty)) => pending.push((name, func_id, d.params.len(), ret_ty)),
                    Err(JitError::Unsupported(reason)) => {
                        declined.insert(name.clone(), reason);
                    }
                    Err(e) => return Err(e), // a real codegen failure — surface it
                }
            }
            module
                .finalize_definitions()
                .map_err(|e| JitError::Cranelift(e.to_string()))?;
            for (name, func_id, flatlen, ret_ty) in pending {
                let code = module.get_finalized_function(func_id);
                let sig = vec![ArgShape::Scalar; flatlen];
                cache.entry(name.to_string()).or_default().insert(
                    sig,
                    Some(CompiledFn {
                        code,
                        arity: flatlen,
                        ret_ty,
                    }),
                );
                scalar_compiled += 1;
            }
        }

        Ok(JitRegistry {
            module: RefCell::new(module),
            helpers,
            defs: owned_defs,
            globals: owned_globals,
            cache: RefCell::new(cache),
            scratch: RefCell::new(Vec::new()),
            next_symbol: Cell::new(next_symbol),
            declined,
            scalar_compiled,
            stats: RefCell::default(),
        })
    }

    /// The specialization for `(name, sig)` — from the cache, or COMPILED on demand and memoized (P.1.6 rung
    /// B). `None` if `name` is unknown OR this arg shape DECLINES (the body leaves the numeric subset for these
    /// shapes); a decline memoizes as `None`, so a shape compiles at most once. The returned [`CompiledFn`] is
    /// `Copy`, lifted out from behind the cache borrow — the code stays mapped for the registry's life.
    fn get_or_compile(&self, name: &str, sig: &ShapeSig) -> Option<CompiledFn> {
        // Fast path: already compiled, or a memoized decline.
        if let Some(entry) = self.cache.borrow().get(name).and_then(|m| m.get(sig)) {
            return entry.clone();
        }
        // Unknown function → nothing to compile (and nothing to memoize).
        let def = self.defs.get(name)?;
        // Compile this shape now. Build the param-shape list + the borrow-maps over the owned AST (only paid
        // on a cache MISS — a rare event, once per never-before-seen shape).
        let params: Vec<(&str, ArgShape)> = def
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    p.name.as_ref(),
                    sig.get(i).cloned().unwrap_or(ArgShape::Scalar),
                )
            })
            .collect();
        let flatlen: usize = sig.iter().map(ArgShape::size).sum();
        let symbol = {
            let n = self.next_symbol.get();
            self.next_symbol.set(n + 1);
            format!("scad_jit_{n}")
        };
        let fn_defs: FnDefs = self
            .defs
            .iter()
            .map(|(n, d)| (n.as_str(), (d.params.as_slice(), &d.body)))
            .collect();
        let globals: Globals = self.globals.iter().map(|(n, v)| (n.as_str(), v)).collect();
        let compiled = {
            let mut module = self.module.borrow_mut();
            match define_one(
                &mut module,
                &symbol,
                &params,
                &def.body,
                &fn_defs,
                &globals,
                &self.helpers,
            ) {
                Ok((func_id, ret_ty)) => match module.finalize_definitions() {
                    Ok(()) => Some(CompiledFn {
                        code: module.get_finalized_function(func_id),
                        arity: flatlen,
                        ret_ty,
                    }),
                    // A finalize failure is unexpected mid-eval; decline this shape (interpret) rather than panic.
                    Err(_) => None,
                },
                // Out of the numeric subset for this shape (or a codegen failure) → memoize the decline.
                Err(_) => None,
            }
        };
        self.cache
            .borrow_mut()
            .entry(name.to_string())
            .or_default()
            .insert(sig.clone(), compiled.clone());
        compiled
    }

    /// Per-function decline reasons for the ALL-SCALAR shape (name → the first out-of-subset node kind) — the
    /// build-time absorption ceiling. Vector-arg shapes compile on demand, so some names here still gain a
    /// specialization at runtime (rung B); this is the conservative scalar view the EXPLAIN report shows.
    #[must_use]
    pub fn declined(&self) -> &BTreeMap<String, &'static str> {
        &self.declined
    }

    /// The pre-compiled all-SCALAR specialization named `name`, if it compiled (else `None`). The stable
    /// name-keyed lookup the scalar tests + EXPLAIN use; vector shapes go through [`get_or_compile`].
    #[must_use]
    pub fn get(&self, name: &str) -> Option<CompiledFn> {
        let n = self.defs.get(name)?.params.len();
        let sig = vec![ArgShape::Scalar; n];
        let cache = self.cache.borrow();
        cache.get(name).and_then(|m| m.get(&sig)).cloned().flatten()
    }

    /// How many functions compiled their all-scalar shape at build — the coverage count (EXPLAIN report).
    #[must_use]
    pub fn len(&self) -> usize {
        self.scalar_compiled
    }

    /// Whether there are NO functions to JIT at all (an empty program). Unlike the pre-rung-B registry, a
    /// non-empty registry is kept even if nothing compiled all-scalar — a vector-arg shape may still compile
    /// on demand, so the hook stays installed whenever there's a function it could specialize.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The names whose all-SCALAR shape compiled, sorted — for the FAB_EXPLAIN coverage report.
    pub fn compiled_names(&self) -> impl Iterator<Item = &str> {
        self.defs
            .keys()
            .map(String::as_str)
            .filter(|n| self.get(n).is_some())
    }
}

/// The dispatch hook the interpreter calls (P.1.2). The hook SCALARIZES the evaluated args itself (P.1.6 rung
/// B) — a `Num` is a scalar, a `NumList` a fixed-small vector — then runs (or on-demand compiles) the
/// specialization for that exact arg SHAPE. `None` means "not scalarizable / not compiled / declined / the
/// inline assert raised" — the interpreter takes over in every case, which is correct for all of them.
impl NumericJit for JitRegistry {
    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "the trait method stays SAFE because fab-lang (`unsafe_code = forbid`) must call it; `rand`'s \
                  validity is the dispatch's documented contract (it passes `Ctx::rand_stream`'s live cell \
                  pointer) and the actual deref is confined to `CompiledFn::call`'s unsafe seam"
    )]
    fn call_numeric(
        &self,
        name: &str,
        args: &[ScadValue],
        rand: *mut core::ffi::c_void,
    ) -> Option<JitOutcome> {
        // Cheap membership check before any flatten work — most eligible calls are to non-JIT functions.
        if !self.defs.contains_key(name) {
            return None;
        }
        self.stats.borrow_mut().offered += 1;
        let mut scratch = self.scratch.borrow_mut();
        // Derive the arg shape + flatten the f64s into scratch. `None` → a non-scalarizable arg (nested list,
        // string, over-long vector) → interpret. `get_or_compile` doesn't touch `scratch`, so holding this
        // borrow across it is safe (a different `RefCell`), and the compiled fn then reads `scratch` directly.
        let Some(sig) = shape_and_flatten(args, &mut scratch) else {
            self.stats.borrow_mut().shape_declined += 1;
            return None;
        };
        let Some(compiled) = self.get_or_compile(name, &sig) else {
            self.stats.borrow_mut().subset_declined += 1;
            return None;
        };
        // RE-TAG the untyped native return by the specialization's static shape (P.1.4e + rung C): a `Num` IS
        // the `f64`; a `Bool` predicate yields `0.0`/`1.0` → `Value::Bool`; a `Vec(n)` wrote its `n` elements to
        // a sink buffer → `Value::NumList`. `None` from `call` = the inline assert raised → interpret. The
        // scalar path keeps its stack dummy out — no heap allocation. `rand` is the eval's woven stream (P.1.6
        // rung-D piece 1) — a seedless-`rands` body advances it; the dispatch guarantees it's live + exclusive.
        // SAFETY: `rand` came from the dispatch's `Ctx::rand_stream` cell pointer, valid + single-threaded.
        // The dynamic-list ARENA (P.1.6 rung-D 2b.2), created ONCE per call — even a SCALAR body may
        // materialize an intermediate DynList (e.g. `len([for …])`), so every call gets a real (empty-until-
        // used) arena; it's freed when this fn returns, AFTER a `DynVec` result is taken out of it.
        let mut arena = JitArena::new();
        let arena_ptr = std::ptr::from_mut(&mut arena).cast::<core::ffi::c_void>();
        let out = match &compiled.ret_ty {
            Ret::Num => Some(JitOutcome::Num(unsafe {
                compiled.call(&scratch, &mut [0.0], rand, arena_ptr)
            }?)),
            Ret::Bool => Some(JitOutcome::Bool(
                unsafe { compiled.call(&scratch, &mut [0.0], rand, arena_ptr) }? != 0.0,
            )),
            Ret::Vec(n) => {
                let mut out = vec![0.0; *n];
                // the f64 return is a dummy; the elements are in `out`.
                unsafe { compiled.call(&scratch, &mut out, rand, arena_ptr) }?;
                Some(JitOutcome::Vec(out))
            }
            Ret::Nested { shape, leaves } => {
                let mut out = vec![0.0; *leaves];
                // the f64 return is a dummy; the FLAT leaves are in `out`, rebuilt into the nested value via the
                // shape tree (2c.1). `None` = an inline assert raised → interpret.
                unsafe { compiled.call(&scratch, &mut out, rand, arena_ptr) }?;
                let mut cursor = 0usize;
                Some(JitOutcome::Nested(rebuild_nested(shape, &out, &mut cursor)))
            }
            Ret::DynVec => {
                // `None` = the BUDGET bail (or an inline assert) flagged `raised` → interpret. On success the
                // body flagged `arena.result` (via `jit_set_result`) — TAKE its Vec out before the arena drops.
                unsafe { compiled.call(&scratch, &mut [0.0], rand, arena_ptr) }?;
                let result = if arena.result.is_null() {
                    Vec::new() // a DynVec body always sets a result; empty is a safe fallback
                } else {
                    // SAFETY: `arena.result` is a live arena list (a boxed `Vec<f64>` the body just filled);
                    // taking it leaves an empty `Vec`, and the arena (still alive) frees it on drop.
                    unsafe { std::mem::take(&mut *arena.result) }
                };
                Some(JitOutcome::Vec(result))
            }
            Ret::DynMat { width } => {
                // Like DynVec, but RESHAPE the flat sink into a `List` of `width`-chunks (2c.3). `None` = budget
                // bail / assert → interpret.
                unsafe { compiled.call(&scratch, &mut [0.0], rand, arena_ptr) }?;
                let flat = if arena.result.is_null() {
                    Vec::new()
                } else {
                    // SAFETY: as in `DynVec` — `arena.result` is the live boxed `Vec<f64>` the body filled.
                    unsafe { std::mem::take(&mut *arena.result) }
                };
                // `width` scalars were pushed per row, so `flat.len()` is a multiple of `width`. Reshape row-major
                // into a `Value::List` of `NumList` rows — the interpreter's nested matrix, bit-identical (its
                // `PartialEq` even makes an empty `List` equal an empty `NumList`).
                debug_assert!(
                    flat.len() % width == 0,
                    "DynMat flat len {} not a multiple of width {width}",
                    flat.len()
                );
                let rows: Vec<ScadValue> = flat
                    .chunks(*width)
                    .map(|row| ScadValue::num_list(row.to_vec()))
                    .collect();
                Some(JitOutcome::Nested(ScadValue::list(rows)))
            }
        };
        // Book the outcome (P.1.5): a `Some` is a completed native call; a `None` here can only be the
        // inline-assert/budget bail (both declines already returned above).
        let mut stats = self.stats.borrow_mut();
        if out.is_some() {
            if let Some(n) = stats.fired.get_mut(name) {
                *n += 1;
            } else {
                stats.fired.insert(name.to_string(), 1);
            }
        } else {
            stats.assert_bailed += 1;
        }
        out
    }
}

/// The runtime-activity report (P.1.5), printed when the registry drops (eval end) under `FAB_JIT_EXPLAIN`
/// — the "did it even fire?" numbers: how many eligible calls the JIT was OFFERED, how they partitioned
/// (fired / shape-declined / subset-declined / assert-bailed), and the per-function fire counts. The decline
/// counts price the flatten/lookup tax the hook pays on calls it ends up interpreting anyway.
impl Drop for JitRegistry {
    fn drop(&mut self) {
        if std::env::var_os("FAB_JIT_EXPLAIN").is_none() {
            return;
        }
        let stats = self.stats.borrow();
        if stats.offered == 0 {
            return; // a compile-for-coverage registry that never dispatched (EXPLAIN with the JIT off)
        }
        let fired_total: u64 = stats.fired.values().sum();
        eprintln!(
            "[jit-fires] offered {} → fired {fired_total}  shape-declined {}  subset-declined {}  assert-bailed {}",
            stats.offered, stats.shape_declined, stats.subset_declined, stats.assert_bailed
        );
        let mut by_count: Vec<(&String, &u64)> = stats.fired.iter().collect();
        by_count.sort_unstable_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (name, count) in by_count {
            eprintln!("[jit-fires]   {count:>9}  {name}");
        }
    }
}

/// The factory the native shell hands to the eval entry (P.1.2b): given a program's function defs, compile
/// the numeric-subset ones into a [`JitRegistry`].
///
/// OPT-IN under `FAB_JIT=1` for now — the interpreter is the bit-identical baseline and the doctrine is
/// never-silently-wrong, so a NEW eval path stays off by default until P.1.3's end-to-end fast==JIT
/// differential proves it byte-for-byte on the corpus/models; then the default flips ON. Unset / any other
/// value → `None` (pure interpreter). An empty registry (a program with NO user functions) also returns
/// `None`; a non-empty one keeps the hook even if nothing compiled all-scalar — a vector-arg shape may still
/// compile on demand (P.1.6 rung B).
pub struct JitFactory;

impl NumericJitFactory for JitFactory {
    fn compile(
        &self,
        defs: &[JitDef<'_>],
        consts: &[JitConst<'_>],
        enabled: bool,
    ) -> Option<Box<dyn NumericJit>> {
        // `enabled` is the caller's authoritative RUN gate (`Config::jit`, from `FAB_JIT` on the CLI path) —
        // no env sniff here. `FAB_JIT_EXPLAIN` stays a report-only probe: it may compile-for-coverage even when
        // the JIT won't run, but then returns `None` (interpret everything) below.
        let explain = std::env::var_os("FAB_JIT_EXPLAIN").is_some();
        if !enabled && !explain {
            return None; // neither running nor reporting → skip the compile entirely
        }
        let registry = JitRegistry::build(
            defs.iter().map(|d| (d.name, d.params, d.body)),
            consts.iter().map(|c| (c.name, c.value)),
        )
        .ok()?;
        if explain {
            explain_coverage(defs, &registry);
        }
        // EXPLAIN can run with the JIT OFF (report-only) — return the hook ONLY when actually enabled.
        if !enabled || registry.is_empty() {
            return None;
        }
        Some(Box::new(registry))
    }
}

/// The `FAB_JIT_EXPLAIN` coverage report (P.1.3) — the JIT sibling of the intrinsic tier's `FAB_EXPLAIN`.
/// Which of the program's functions the numeric subset COMPILED (native dispatch) vs DECLINED (interpreted),
/// to stderr. The declined count is the headroom `P.1.4` (ternary/comparisons/transcendental calls) reclaims.
#[allow(
    clippy::cast_precision_loss,
    reason = "coverage/histogram percentages in a dev-only stderr report; a 2^52-function program is unreachable"
)]
fn explain_coverage(defs: &[JitDef<'_>], registry: &JitRegistry) {
    let total = defs.len();
    let compiled = registry.len();
    let pct = 100.0 * compiled as f64 / total.max(1) as f64;
    eprintln!(
        "\n[jit-explain] === numeric-JIT coverage === {compiled}/{total} functions compiled ({pct:.1}%)"
    );
    for name in registry.compiled_names() {
        eprintln!("[jit-explain]   + compiled  {name}");
    }
    // The absorption ceiling: which node kind blocks each declined function, aggregated. The biggest bucket
    // is the subset feature (usually `call`) that would unlock the most WHOLE functions if added next.
    let mut histogram: BTreeMap<&'static str, usize> = BTreeMap::new();
    for reason in registry.declined().values() {
        *histogram.entry(reason).or_default() += 1;
    }
    let mut rows: Vec<(&&'static str, &usize)> = histogram.iter().collect();
    rows.sort_unstable_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let declined_total = registry.declined().len();
    eprintln!(
        "[jit-explain]   {declined_total} declined — first-blocker histogram (the absorption ceiling):"
    );
    for (reason, count) in rows {
        let share = 100.0 * *count as f64 / declined_total.max(1) as f64;
        eprintln!("[jit-explain]     {count:>5}  {share:5.1}%  {reason}");
    }
    // This is the ALL-SCALAR view: a function blocked by `index of a non-vector` / `member access on a
    // non-vector` here still gains a specialization when CALLED with a vector arg (P.1.6 rung B, compiled on
    // demand and not counted above). The scalar histogram is the conservative floor.
    eprintln!(
        "[jit-explain]   (vector-arg specializations compile on demand at runtime — not in the counts above)"
    );
    // `FAB_JIT_EXPLAIN=full`: the per-function decline listing behind the histogram — "why did fn X
    // decline" without a scratch probe (P.1.5). Grouped by reason so the biggest bucket reads as a worklist.
    if std::env::var_os("FAB_JIT_EXPLAIN").is_some_and(|v| v == "full") {
        let mut by_fn: Vec<(&str, &&'static str)> = registry
            .declined()
            .iter()
            .map(|(n, r)| (n.as_str(), r))
            .collect();
        by_fn.sort_unstable_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
        for (name, reason) in by_fn {
            eprintln!("[jit-explain]   - declined  {name}: {reason}");
        }
    }
}

/// Compile a single numeric function body (over `param_names`, in order) to native code, owning its own
/// module. The standalone API — [`JitRegistry`] is the multi-function form. Signature is
/// `extern "C" fn(*const f64) -> f64`: parameter `i` is read from `params[i]`, evaluation order mirrors
/// the interpreter, and `%`/`^` become calls to [`jit_fmod`]/[`jit_powf`] so the result is bit-identical.
///
/// # Errors
/// [`JitError::Unsupported`] for any node outside the numeric subset, [`JitError::Cranelift`] for a
/// codegen failure.
pub fn compile_function(param_names: &[&str], body: &Expr) -> Result<JitFn, JitError> {
    let mut module = new_module()?;
    let helpers = declare_helpers(&mut module)?;
    // The standalone API compiles ONE function with no peers to inline and no top-level constants (the
    // differential harness uses it for self-contained bodies); a user-fn call OR a free-variable reference to a
    // constant therefore declines. [`JitRegistry`] is the multi-function, globals-aware form.
    let no_defs = FnDefs::new();
    let no_globals = Globals::new();
    // The standalone API weaves NO RandStream (piece 1) and NO JitArena (piece 2) — both are registry features
    // — so a `rands` OR comprehension body must DECLINE here rather than have `JitFn::call`'s null stream/arena
    // pointer dereferenced (even a SCALAR body that only uses an intermediate list, e.g. `len([for …])`).
    if needs_runtime_env(body) {
        return Err(JitError::Unsupported(
            "rands/comprehension (standalone API is env-free; use JitRegistry)",
        ));
    }
    // The standalone differential passes plain `f64` args, so every parameter is a SCALAR shape.
    let params: Vec<(&str, ArgShape)> =
        param_names.iter().map(|&n| (n, ArgShape::Scalar)).collect();
    let (func_id, ret_ty) = define_one(
        &mut module,
        "scad_jit_fn",
        &params,
        body,
        &no_defs,
        &no_globals,
        &helpers,
    )?;
    // The standalone API is f64-only (the fast==JIT differential compares raw f64s); a bool- OR vector-returning
    // body is the registry path's job (it carries the tag + the sink buffer), so DECLINE it here.
    if !matches!(ret_ty, Ret::Num) {
        return Err(JitError::Unsupported(
            "non-numeric return (standalone API is f64-only; use JitRegistry)",
        ));
    }
    module
        .finalize_definitions()
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    let code = module.get_finalized_function(func_id);
    Ok(JitFn {
        _module: module,
        inner: CompiledFn {
            code,
            arity: param_names.len(),
            ret_ty,
        },
    })
}

/// A fresh JIT module with our two math helper symbols registered. `opt_level=speed` is safe for
/// determinism: Cranelift never CONTRACTS fmul+fadd into an fma (that's an LLVM fast-math behavior); it
/// emits the instructions we ask for, in order.
fn new_module() -> Result<JITModule, JitError> {
    let mut flags = settings::builder();
    let set = |flags: &mut settings::Builder, k, v| {
        flags
            .set(k, v)
            .map_err(|e| JitError::Cranelift(e.to_string()))
    };
    set(&mut flags, "opt_level", "speed")?;
    set(&mut flags, "use_colocated_libcalls", "false")?;
    set(&mut flags, "is_pic", "false")?;
    let isa = cranelift::native::builder()
        .map_err(|e| JitError::Cranelift(e.to_string()))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
    jb.symbol("jit_fmod", jit_fmod as *const u8);
    jb.symbol("jit_powf", jit_powf as *const u8);
    jb.symbol("jit_fmin", jit_fmin as *const u8);
    jb.symbol("jit_fmax", jit_fmax as *const u8);
    jb.symbol("jit_math_call", jit_math_call as *const u8);
    jb.symbol("jit_rand_next", jit_rand_next as *const u8);
    jb.symbol("jit_range_len", jit_range_len as *const u8);
    jb.symbol("jit_as_index", jit_as_index as *const u8);
    jb.symbol("jit_arena_new_list", jit_arena_new_list as *const u8);
    jb.symbol("jit_vec_push", jit_vec_push as *const u8);
    jb.symbol("jit_vec_len", jit_vec_len as *const u8);
    jb.symbol("jit_vec_get", jit_vec_get as *const u8);
    jb.symbol("jit_vec_bound", jit_vec_bound as *const u8);
    jb.symbol("jit_set_result", jit_set_result as *const u8);
    Ok(JITModule::new(jb))
}

/// The external helper routines declared as imports in `module` — done ONCE per module, their `FuncId`s
/// reused by every function compiled into it.
struct Helpers {
    /// `jit_fmod(f64, f64) -> f64` — the `%` operator.
    fmod: FuncId,
    /// `jit_powf(f64, f64) -> f64` — the `^` operator.
    powf: FuncId,
    /// `jit_fmin(f64, f64) -> f64` — the `min` builtin's fold step (NaN-ignoring, unlike Cranelift `fmin`).
    fmin: FuncId,
    /// `jit_fmax(f64, f64) -> f64` — the `max` builtin's fold step.
    fmax: FuncId,
    /// `jit_math_call(i32 id, f64, f64) -> f64` — a scalar math builtin dispatched by id (P.1.4b).
    math: FuncId,
    /// `jit_rand_next(*mut RandStream, f64, f64) -> f64` — one seedless `rands` draw (P.1.6 rung-D piece 1).
    rand_next: FuncId,
    /// `jit_range_len(f64, f64, f64) -> i64` — a range's element count, the loop bound (P.1.6 rung-D piece 2).
    range_len: FuncId,
    /// `jit_as_index(f64) -> i64` — a count/index validated (finite ≥ 0 → truncated, else -1) (2b.3).
    as_index: FuncId,
    /// `jit_arena_new_list(*mut JitArena) -> *mut Vec<f64>` — allocate a fresh DynList (P.1.6 rung-D 2b.2).
    arena_new_list: FuncId,
    /// `jit_vec_push(*mut Vec<f64>, f64)` — push one element onto a DynList.
    vec_push: FuncId,
    /// `jit_vec_len(*mut Vec<f64>) -> i64` — a DynList's length.
    vec_len: FuncId,
    /// `jit_vec_get(*mut Vec<f64>, i64) -> f64` — a DynList's element `i` (in-range by construction).
    vec_get: FuncId,
    /// `jit_vec_bound(*mut Vec<f64>, f64) -> i64` — a dynamic index resolved + bounds-checked (2b.2b); -1 = undef.
    vec_bound: FuncId,
    /// `jit_set_result(*mut JitArena, *mut Vec<f64>)` — flag the DynList that is the function's return.
    set_result: FuncId,
}

fn declare_helpers(module: &mut JITModule) -> Result<Helpers, JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    // `(f64, f64) -> f64` for fmod/powf/fmin/fmax.
    let mut op_sig = module.make_signature();
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.returns.push(AbiParam::new(types::F64));
    let fmod = module
        .declare_function("jit_fmod", Linkage::Import, &op_sig)
        .map_err(cl)?;
    let powf = module
        .declare_function("jit_powf", Linkage::Import, &op_sig)
        .map_err(cl)?;
    let fmin = module
        .declare_function("jit_fmin", Linkage::Import, &op_sig)
        .map_err(cl)?;
    let fmax = module
        .declare_function("jit_fmax", Linkage::Import, &op_sig)
        .map_err(cl)?;
    // `(i32 id, f64, f64) -> f64` for the math dispatcher.
    let mut math_sig = module.make_signature();
    math_sig.params.push(AbiParam::new(types::I32));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let math = module
        .declare_function("jit_math_call", Linkage::Import, &math_sig)
        .map_err(cl)?;
    // `(*mut RandStream, f64, f64) -> f64` — the stream pointer rides the target pointer type.
    let mut rand_sig = module.make_signature();
    rand_sig
        .params
        .push(AbiParam::new(module.target_config().pointer_type()));
    rand_sig.params.push(AbiParam::new(types::F64));
    rand_sig.params.push(AbiParam::new(types::F64));
    rand_sig.returns.push(AbiParam::new(types::F64));
    let rand_next = module
        .declare_function("jit_rand_next", Linkage::Import, &rand_sig)
        .map_err(cl)?;
    // `(f64, f64, f64) -> i64` — the range length / loop bound.
    let mut rlen_sig = module.make_signature();
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.returns.push(AbiParam::new(types::I64));
    let range_len = module
        .declare_function("jit_range_len", Linkage::Import, &rlen_sig)
        .map_err(cl)?;
    // `(f64) -> i64` — as_index (a count/index validated).
    let mut aidx_sig = module.make_signature();
    aidx_sig.params.push(AbiParam::new(types::F64));
    aidx_sig.returns.push(AbiParam::new(types::I64));
    let as_index = module
        .declare_function("jit_as_index", Linkage::Import, &aidx_sig)
        .map_err(cl)?;
    // The dynamic-list helpers (2b.2), each built explicitly (a signature-building closure would hold a
    // `&module` borrow across `declare_function`'s `&mut`). `p` = the target pointer type.
    let p = module.target_config().pointer_type();
    // `jit_arena_new_list(*mut JitArena) -> *mut Vec<f64>`.
    let mut new_sig = module.make_signature();
    new_sig.params.push(AbiParam::new(p));
    new_sig.returns.push(AbiParam::new(p));
    let arena_new_list = module
        .declare_function("jit_arena_new_list", Linkage::Import, &new_sig)
        .map_err(cl)?;
    // `jit_vec_push(*mut Vec<f64>, f64)`.
    let mut push_sig = module.make_signature();
    push_sig.params.push(AbiParam::new(p));
    push_sig.params.push(AbiParam::new(types::F64));
    let vec_push = module
        .declare_function("jit_vec_push", Linkage::Import, &push_sig)
        .map_err(cl)?;
    // `jit_vec_len(*mut Vec<f64>) -> i64`.
    let mut len_sig = module.make_signature();
    len_sig.params.push(AbiParam::new(p));
    len_sig.returns.push(AbiParam::new(types::I64));
    let vec_len = module
        .declare_function("jit_vec_len", Linkage::Import, &len_sig)
        .map_err(cl)?;
    // `jit_vec_get(*mut Vec<f64>, i64) -> f64`.
    let mut get_sig = module.make_signature();
    get_sig.params.push(AbiParam::new(p));
    get_sig.params.push(AbiParam::new(types::I64));
    get_sig.returns.push(AbiParam::new(types::F64));
    let vec_get = module
        .declare_function("jit_vec_get", Linkage::Import, &get_sig)
        .map_err(cl)?;
    // `jit_vec_bound(*mut Vec<f64>, f64) -> i64`.
    let mut bound_sig = module.make_signature();
    bound_sig.params.push(AbiParam::new(p));
    bound_sig.params.push(AbiParam::new(types::F64));
    bound_sig.returns.push(AbiParam::new(types::I64));
    let vec_bound = module
        .declare_function("jit_vec_bound", Linkage::Import, &bound_sig)
        .map_err(cl)?;
    // `jit_set_result(*mut JitArena, *mut Vec<f64>)`.
    let mut res_sig = module.make_signature();
    res_sig.params.push(AbiParam::new(p));
    res_sig.params.push(AbiParam::new(p));
    let set_result = module
        .declare_function("jit_set_result", Linkage::Import, &res_sig)
        .map_err(cl)?;
    Ok(Helpers {
        fmod,
        powf,
        fmin,
        fmax,
        math,
        rand_next,
        range_len,
        as_index,
        arena_new_list,
        vec_push,
        vec_len,
        vec_get,
        vec_bound,
        set_result,
    })
}

/// Build the IR for one function specialized to `params` (each param's name + arg [`ArgShape`]) and declare +
/// define it in `module` under `symbol` (NOT finalized — the caller finalizes once). Returns the `FuncId` (to
/// resolve the code pointer after finalize) AND the body's static return [`Ret`] — a bool-returning body is
/// returned as `0.0`/`1.0`, a vector body writes to the sink; the caller tags the function so the dispatch
/// reconstructs the matching `Value` (P.1.4e + rung C).
///
/// The native ABI is `(flat_params: *const f64, raised: *mut u8, out: *mut f64) -> f64`: params are FLATTENED
/// into the first pointer — a scalar param is one slot, a vec-`n` param is `n` contiguous slots (P.1.6 rung B).
/// A scalar is read lazily by its flat offset; a vector param is loaded up-front into a scalarized
/// [`Lowered::Vec`] and bound in the initial locals, so from there it's handled exactly like a rung-A internal
/// vector. A `Num`/`Bool` body returns via the `f64`; a fixed-shape VECTOR body (rung C) writes its elements to
/// the `out` sink and the `f64` is a dummy. On [`JitError::Unsupported`] nothing is added to the module (IR is
/// built before declare/define), so a declined function leaves the module clean for the next one.
fn define_one(
    module: &mut JITModule,
    symbol: &str,
    params: &[(&str, ArgShape)],
    body: &Expr,
    defs: &FnDefs,
    globals: &Globals,
    helpers: &Helpers,
) -> Result<(FuncId, Ret), JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    let ptr_ty = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    // Signature: `(params, raised, out, rand, sink) -> f64`, all pointers. `raised` is the assert-failure /
    // budget-bail out-param (P.1.4) — a falsy `assert` or an over-budget comprehension writes 1; the JIT can't
    // unwind. `out` is the rung-C fixed sink. `rand` is the woven RandStream (piece 1). `sink` is the rung-D
    // dynamic `*mut Vec<f64>` a comprehension pushes into (piece 2). Unused pointers are never dereferenced.
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(types::F64));

    let mut fbctx = FunctionBuilderContext::new();
    let ret_ty;
    {
        let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let block = fb.create_block();
        fb.append_block_params_for_function_params(block);
        fb.switch_to_block(block);
        fb.seal_block(block);
        let params_ptr = fb.block_params(block)[0];
        let raised_ptr = fb.block_params(block)[1];
        let out_ptr = fb.block_params(block)[2];
        let rand_ptr = fb.block_params(block)[3];
        let arena_ptr = fb.block_params(block)[4];

        let fmod_ref = module.declare_func_in_func(helpers.fmod, fb.func);
        let powf_ref = module.declare_func_in_func(helpers.powf, fb.func);
        let fmin_ref = module.declare_func_in_func(helpers.fmin, fb.func);
        let fmax_ref = module.declare_func_in_func(helpers.fmax, fb.func);
        let math_ref = module.declare_func_in_func(helpers.math, fb.func);
        let rand_next_ref = module.declare_func_in_func(helpers.rand_next, fb.func);
        let range_len_ref = module.declare_func_in_func(helpers.range_len, fb.func);
        let as_index_ref = module.declare_func_in_func(helpers.as_index, fb.func);
        let arena_new_list_ref = module.declare_func_in_func(helpers.arena_new_list, fb.func);
        let vec_push_ref = module.declare_func_in_func(helpers.vec_push, fb.func);
        let vec_len_ref = module.declare_func_in_func(helpers.vec_len, fb.func);
        let vec_get_ref = module.declare_func_in_func(helpers.vec_get, fb.func);
        let vec_bound_ref = module.declare_func_in_func(helpers.vec_bound, fb.func);
        let set_result_ref = module.declare_func_in_func(helpers.set_result, fb.func);
        // Flat param layout: walk params in order assigning f64 slots — a scalar takes 1, a vec-`n` takes `n`.
        // A scalar's flat ELEMENT offset goes in `index` (read lazily in the `Ident` arm as `offset * 8`
        // bytes); a vector's `n` slots are loaded now into a `Lowered::Vec` bound in `locals`, so the body sees
        // a scalarized vector. For an all-scalar function the offsets are 0,1,2,… — identical to the pre-rung-B
        // param-index layout, so nothing changes for the scalar majority.
        let mut index: BTreeMap<&str, usize> = BTreeMap::new();
        let mut locals = LetEnv::new(); // seeded below with the vector params; then let-bindings extend it
        let mut off = 0usize;
        for (name, shape) in params {
            match shape {
                ArgShape::Scalar => {
                    index.insert(name, off);
                    off += 1;
                }
                ArgShape::Vec(_) => {
                    // A vector/MATRIX param eagerly loads its leaves into a (possibly nested) `Lowered::Vec` so
                    // the body sees a scalarized value (2c.2 recurses for a matrix). `load_arg` threads `off`.
                    let lowered = load_arg(&mut fb, params_ptr, shape, &mut off)?;
                    locals.insert(name, lowered);
                }
            }
        }
        let inlining: [&str; 0] = []; // nothing being inlined at the top level
        let lower = Lower {
            params_ptr,
            raised_ptr,
            index: &index,
            locals: &locals,
            defs,
            globals,
            inlining: &inlining,
            depth: 0,
            rand_ptr,
            arena_ptr,
            fmod: fmod_ref,
            powf: powf_ref,
            fmin: fmin_ref,
            fmax: fmax_ref,
            math: math_ref,
            rand_next: rand_next_ref,
            range_len: range_len_ref,
            as_index: as_index_ref,
            arena_new_list: arena_new_list_ref,
            vec_push: vec_push_ref,
            vec_len: vec_len_ref,
            vec_get: vec_get_ref,
            vec_bound: vec_bound_ref,
            set_result: set_result_ref,
        };

        // IR is built BEFORE declare/define — an Unsupported node returns here with the module untouched. The
        // body compiles to a `Lowered`, which the return tags: a NUMERIC body returns its f64 directly; a BOOL
        // body (an i8 0/1) returns 0.0/1.0, tagged so the dispatch wraps `Value::Bool` (P.1.4e); a fixed-shape
        // VECTOR body (rung C) WRITES its elements to the `out` sink; a DYNAMIC-list body (rung-D 2b.2) flags
        // its arena handle as the result via `jit_set_result` — the dispatch reads it into a `NumList`. A NESTED
        // vector (matrix) declines. `compile_comprehension` emits its own budget-bail `return_`, but the normal
        // path always flows here for the final `return_`.
        let (ret, ty) = match compile_expr(&mut fb, body, &lower)? {
            Lowered::Num(v) => (v, Ret::Num),
            // A compile-time-const number RETURN (`function three() = 3;`, a folded body) → its `f64const`, tagged `Num`.
            Lowered::ConstNum(c) => (fb.ins().f64const(c), Ret::Num),
            Lowered::Bool(v) => {
                let one = fb.ins().f64const(1.0);
                let zero = fb.ins().f64const(0.0);
                (fb.ins().select(v, one, zero), Ret::Bool)
            }
            // A compile-time-const bool RETURN (a predicate body) → the literal `0.0`/`1.0`, tagged `Bool`.
            Lowered::ConstBool(b) => (fb.ins().f64const(if b { 1.0 } else { 0.0 }), Ret::Bool),
            Lowered::Vec(elems) => {
                // Flatten the (possibly NESTED) vector's leaf scalars into the sink in row-major order, building
                // the shape tree as we go (2c.1). A FLAT vector (every child a leaf) keeps the `Ret::Vec(n)` fast
                // path; a nested one (a matrix / list-of-vectors) carries its shape so the dispatch rebuilds it.
                let mut off = 0usize;
                let shape = store_return_vec(&mut fb, out_ptr, &elems, &mut off)?;
                let ret = match &shape {
                    VShape::Nest(children)
                        if children.iter().all(|c| matches!(c, VShape::Leaf)) =>
                    {
                        Ret::Vec(off)
                    }
                    _ => Ret::Nested {
                        shape: Rc::new(shape),
                        leaves: off,
                    },
                };
                (fb.ins().f64const(0.0), ret) // the f64 return is a dummy for a vector
            }
            Lowered::DynList(handle) => {
                fb.ins().call(lower.set_result, &[lower.arena_ptr, handle]);
                (fb.ins().f64const(0.0), Ret::DynVec) // the dispatch reads arena.result into a NumList
            }
            // A dynamic MATRIX (2c.3) — flag the same flat sink; the dispatch reshapes it into a `List` of
            // `width`-chunks (row-major). The `f64` return is a dummy, like `DynVec`.
            Lowered::DynMat { handle, width } => {
                fb.ins().call(lower.set_result, &[lower.arena_ptr, handle]);
                (fb.ins().f64const(0.0), Ret::DynMat { width })
            }
            // A body that evaluates to compile-time `undef` (a scalar-spec `len(scalar)`) — the JIT has no undef
            // return (`JitOutcome` is Num/Bool/Vec/Nested), so DECLINE; the interpreter returns the undef (2c.3).
            Lowered::ConstUndef => return Err(JitError::Unsupported("undef return")),
        };
        fb.ins().return_(&[ret]);
        fb.finalize();
        ret_ty = ty;
    }

    let func_id = module
        .declare_function(symbol, Linkage::Export, &ctx.func.signature)
        .map_err(cl)?;
    module.define_function(func_id, &mut ctx).map_err(cl)?;
    module.clear_context(&mut ctx);
    Ok((func_id, ret_ty))
}

/// Flatten a fixed vector's leaf scalars into the return sink `out_ptr` in row-major order, returning its shape
/// tree (2c.1). Recurses into a nested `Lowered::Vec` (a matrix row); a leaf is a `Num`/`ConstNum` stored at the
/// next f64 slot. A `Bool`/`ConstBool`/`DynList` leaf DECLINES (it can't ride the f64 sink) — the interpreter
/// keeps those. `off` is the running leaf count (== the next slot index), threaded across the whole tree.
fn store_return_vec(
    fb: &mut FunctionBuilder,
    out_ptr: Value,
    elems: &[Lowered],
    off: &mut usize,
) -> Result<VShape, JitError> {
    let mut children = Vec::with_capacity(elems.len());
    for e in elems {
        children.push(store_return_elem(fb, out_ptr, e, off)?);
    }
    Ok(VShape::Nest(children))
}

/// One element of a return vector (2c.1): a nested `Lowered::Vec` recurses into a `Nest`; anything else is a
/// LEAF — `num` materializes a `Num`/`ConstNum` to an f64 and stores it (a `Bool`/`DynList` declines via `num`).
fn store_return_elem(
    fb: &mut FunctionBuilder,
    out_ptr: Value,
    e: &Lowered,
    off: &mut usize,
) -> Result<VShape, JitError> {
    if let Lowered::Vec(sub) = e {
        return store_return_vec(fb, out_ptr, sub, off);
    }
    let x = e.num(fb)?;
    let byte_off =
        i32::try_from(*off * 8).map_err(|_| JitError::Unsupported("return offset overflow"))?;
    fb.ins()
        .store(MemFlagsData::trusted(), x, out_ptr, byte_off);
    *off += 1;
    Ok(VShape::Leaf)
}

/// Rebuild the interpreter's nested `Value` from the flat leaf buffer + the shape tree (2c.1). A `Leaf` consumes
/// the next f64 as a `Num`; a `Nest` collects its children and applies `build_vector`'s EXACT rule — all-`Num`
/// children → the `NumList` fast path, else a general `List`. The interpreter's `PartialEq` already makes the two
/// list representations equal element-for-element, but matching the rule keeps the reconstructed value identical.
fn rebuild_nested(shape: &VShape, flat: &[f64], cursor: &mut usize) -> ScadValue {
    match shape {
        VShape::Leaf => {
            let v = flat[*cursor];
            *cursor += 1;
            ScadValue::Num(v)
        }
        VShape::Nest(children) => {
            let items: Vec<ScadValue> = children
                .iter()
                .map(|c| rebuild_nested(c, flat, cursor))
                .collect();
            match items
                .iter()
                .map(|v| {
                    if let ScadValue::Num(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .collect::<Option<Vec<f64>>>()
            {
                Some(nums) => ScadValue::num_list(nums),
                None => ScadValue::list(items),
            }
        }
    }
}

/// Eagerly load a vector/MATRIX param's leaf scalars from `params_ptr` into a (possibly nested) `Lowered::Vec`,
/// threading the flat slot offset `off` (P.1.6 rung-D 2c.2). A `Scalar` loads one f64 at `*off * 8` bytes → a
/// `Num`; a `Vec(children)` recurses → a nested `Lowered::Vec`, in the SAME row-major order `shape_and_flatten`
/// wrote the leaves. Mirrors the arg's `ArgShape`, so `f([[1,2],[3,4]])` binds `[[Num,Num],[Num,Num]]`.
fn load_arg(
    fb: &mut FunctionBuilder,
    params_ptr: Value,
    shape: &ArgShape,
    off: &mut usize,
) -> Result<Lowered, JitError> {
    match shape {
        ArgShape::Scalar => {
            let byte_off = i32::try_from(*off * 8)
                .map_err(|_| JitError::Unsupported("param offset overflow"))?;
            let v = fb
                .ins()
                .load(types::F64, MemFlagsData::trusted(), params_ptr, byte_off);
            *off += 1;
            Ok(Lowered::Num(v))
        }
        ArgShape::Vec(children) => {
            let mut elems = Vec::with_capacity(children.len());
            for c in children {
                elems.push(load_arg(fb, params_ptr, c, off)?);
            }
            Ok(Lowered::Vec(elems))
        }
    }
}

/// The iterable of a comprehension binding, once analyzed (P.1.6 rung-D 2b.2): a RANGE (index-based value) or
/// a DYNLIST (element via `jit_vec_get`). Both yield the loop bound `len` separately.
enum CompIter {
    Range { start: Value, step: Value },
    Dyn { handle: Value },
}

/// Compile `[for (var = iterable) scalar_body]` to a LOOP that MATERIALIZES each element into a fresh DynList
/// in the call's [`JitArena`], returning the list's handle as [`Lowered::DynList`] (P.1.6 rung-D piece 2). Now
/// a SUB-EXPRESSION (2b.2) — control CONTINUES after the loop (no function return here), so a comprehension can
/// be a `let` value, an iterable of another comprehension (composed), or the function result. The BUDGET bail
/// is the one early `return_` (over-budget → `raised` → the dispatch's `None` → interpret).
///
/// Bit-identity vs `eval::lc_for` + `RangeIter`: the RANGE bound is `jit_range_len` (the interpreter's EXACT
/// `range_len`); element `i`'s value is `start + (i as f64)*step` (`fcvt_from_sint` exact for `i < RANGE_MAX`,
/// same operand order); a DYNLIST iterates `0..len` via `jit_vec_get` (in-range by construction). The loop
/// pushes in index order, matching `lc_for`'s `out.extend`. v1 (2b.2): a SINGLE binding, a RANGE or a DYNLIST
/// iterable, a SCALAR element — a fixed-vector iterable / multi-binding / non-scalar element / filter declines.
/// UNROLL a comprehension over a COMPILE-TIME-fixed vector — `[for(x = [e0, e1, …]) body]` (rung 2b.N). Bind `x`
/// to each element in source order, compile `body` per iteration, and collect a fixed `Lowered::Vec` (the length
/// is known, so no arena/loop — like the arg-scalarize, one level up). The body may itself be a vector → a MATRIX
/// (2c.1 handles the nested result); a `rands` in the body draws once per element, IN ORDER, matching the
/// interpreter. A FILTER / `each` body declines naturally (its `LcIf`/`LcEach` isn't a compiled `ExprKind`). The
/// iterable is capped at [`MAX_VEC_ARG`] so the unroll can't explode IR — a longer one falls back to the interpreter.
fn unroll_fixed_comprehension(
    fb: &mut FunctionBuilder,
    name: &str,
    elems: Vec<Lowered>,
    lc_body: &Expr,
    lower: &Lower,
) -> Result<Lowered, JitError> {
    if elems.len() > MAX_VEC_ARG {
        return Err(JitError::Unsupported(
            "comprehension over an over-long fixed vector",
        ));
    }
    let mut out = Vec::with_capacity(elems.len());
    for e in elems {
        // Bind the loop var to THIS element in a fresh scope (lexical: the body sees the caller's env + `x`).
        let mut locals = lower.locals.clone();
        locals.insert(name, e);
        let scoped = Lower {
            locals: &locals,
            ..*lower
        };
        out.push(compile_expr(fb, lc_body, &scoped)?);
    }
    Ok(Lowered::Vec(out))
}

fn compile_comprehension(
    fb: &mut FunctionBuilder,
    bindings: &[fab_lang::Arg],
    lc_body: &Expr,
    lower: &Lower,
) -> Result<Lowered, JitError> {
    let [binding] = bindings else {
        return Err(JitError::Unsupported("multi-binding comprehension")); // rung 2b.N
    };
    let name = binding.name.as_deref().ok_or(JitError::Unsupported(
        "comprehension binding without a name",
    ))?;

    // The iterable is compiled ONCE, in source order, before the loop — matching the interpreter evaluating the
    // range/list value before iterating. A RANGE gives an index-based value; a DYNLIST (a value that compiles to
    // a handle — a `let`-bound comprehension, a nested one) is read by `jit_vec_get`.
    let (iter, len) = match &binding.value.kind {
        ExprKind::Range { start, step, end } => {
            let start_v = compile_expr(fb, start, lower)?.num(fb)?;
            let step_v = match step {
                Some(s) => compile_expr(fb, s, lower)?.num(fb)?,
                None => fb.ins().f64const(1.0), // `[a:b]` defaults step to 1.0
            };
            let end_v = compile_expr(fb, end, lower)?.num(fb)?;
            let call = fb.ins().call(lower.range_len, &[start_v, step_v, end_v]);
            (
                CompIter::Range {
                    start: start_v,
                    step: step_v,
                },
                fb.inst_results(call)[0],
            )
        }
        _ => match compile_expr(fb, &binding.value, lower)? {
            Lowered::DynList(handle) => {
                let call = fb.ins().call(lower.vec_len, &[handle]);
                (CompIter::Dyn { handle }, fb.inst_results(call)[0])
            }
            // A FIXED scalarized vector iterable (`[for(x = [1,2,3]) …]`, `[for(x = vec_param) …]`) — rung 2b.N:
            // UNROLL at compile time (length is known), no arena/loop, returning a fixed `Lowered::Vec`. Returns
            // EARLY (the loop machinery below is only for the runtime-length Range/DynList cases).
            Lowered::Vec(elems) => {
                return unroll_fixed_comprehension(fb, name, elems, lc_body, lower);
            }
            // A scalar / bool / undef isn't iterable → decline.
            _ => {
                return Err(JitError::Unsupported(
                    "comprehension over a non-range/non-dynlist",
                ));
            }
        },
    };

    // The result DynList, allocated in the arena. (Before the budget check — a bail just leaves it empty; the
    // arena frees it. The handle dominates every block below.)
    let list = {
        let call = fb.ins().call(lower.arena_new_list, &[lower.arena_ptr]);
        fb.inst_results(call)[0]
    };

    // BUDGET bail: an over-budget count flags `raised` and returns → the dispatch's `None` → the interpreter
    // runs the whole body. Checked BEFORE the loop, so no elements / draws happen first (piece-1 order-safe).
    let budget = fb.ins().iconst(types::I64, COMPREHENSION_BUDGET);
    let over = fb.ins().icmp(IntCC::SignedGreaterThan, len, budget);
    let bail = fb.create_block();
    let setup = fb.create_block();
    fb.ins().brif(over, bail, &[], setup, &[]);
    fb.seal_block(bail);
    fb.seal_block(setup);
    fb.switch_to_block(bail);
    let one_flag = fb.ins().iconst(types::I8, 1);
    fb.ins()
        .store(MemFlagsData::trusted(), one_flag, lower.raised_ptr, 0);
    let bail_ret = fb.ins().f64const(0.0);
    fb.ins().return_(&[bail_ret]);

    // The loop: header(i) → body (push) → back-edge → header; exit when i >= len.
    fb.switch_to_block(setup);
    let header = fb.create_block();
    fb.append_block_param(header, types::I64); // the induction variable i
    let body_block = fb.create_block();
    let exit = fb.create_block();
    let zero = fb.ins().iconst(types::I64, 0);
    fb.ins().jump(header, &[BlockArg::Value(zero)]);

    // header: i < len ? body : exit. UNSEALED — the body's back-edge is a second predecessor.
    fb.switch_to_block(header);
    let i = fb.block_params(header)[0];
    let cond = fb.ins().icmp(IntCC::SignedLessThan, i, len);
    fb.ins().brif(cond, body_block, &[], exit, &[]);
    fb.seal_block(body_block); // its only predecessor (header) is now declared
    fb.seal_block(exit);

    // body: value = the iterable's element `i`; bind the loop var; compile the scalar element; push it.
    fb.switch_to_block(body_block);
    let value = match iter {
        CompIter::Range { start, step } => {
            let i_f = fb.ins().fcvt_from_sint(types::F64, i); // exact for 0 <= i < RANGE_MAX < 2^53
            let scaled = fb.ins().fmul(i_f, step);
            fb.ins().fadd(start, scaled) // `start + (i as f64)*step`, the interpreter's operand order
        }
        CompIter::Dyn { handle } => {
            let call = fb.ins().call(lower.vec_get, &[handle, i]); // 0 <= i < len → in-range
            fb.inst_results(call)[0]
        }
    };
    let mut locals = lower.locals.clone();
    locals.insert(name, Lowered::Num(value));
    let scoped = Lower {
        locals: &locals,
        ..*lower
    };
    // Compile the body ONCE (its IR runs every iteration). A SCALAR body pushes 1 element/row → a flat DynList
    // (2b.2). A FIXED-WIDTH numeric-vector body `[a, b, c]` pushes W elements/row in row-major order → a DynMat
    // (2c.3). Anything else (a nested/ragged vector, a dyn list per row) declines. `width` is compile-time-known
    // (the body's shape is structural, the same every iteration), so it decides the return type after the loop.
    let body = compile_expr(fb, lc_body, &scoped)?;
    // `Some(W)` → the body is a fixed-width VECTOR row (→ DynMat, even W == 1: `[for(i) [i]]` is `[[0],[1],…]`,
    // a matrix, NOT the flat `[0,1,…]`); `None` → a SCALAR row (→ flat DynList). The distinction is the body's
    // NESTING, not its element count.
    let row_width = match &body {
        Lowered::Vec(elems)
            if !elems.is_empty()
                && elems.len() <= MAX_VEC_ARG
                && elems
                    .iter()
                    .all(|e| matches!(e, Lowered::Num(_) | Lowered::ConstNum(_))) =>
        {
            // A matrix row: push each of the W scalars IN ORDER (e0..eW-1) — the row-major layout the dispatch
            // reshapes by, and the order a body-`rands` must draw in (piece 1 weave, one draw per element).
            for e in elems {
                let ev = e.num(fb)?;
                fb.ins().call(lower.vec_push, &[list, ev]);
            }
            Some(elems.len())
        }
        // A scalar row: push 1. A nested/ragged/over-wide vector, a bool/dynlist/undef body → `num` Err → decline.
        _ => {
            let elem = body.num(fb)?;
            fb.ins().call(lower.vec_push, &[list, elem]);
            None
        }
    };
    let one = fb.ins().iconst(types::I64, 1);
    let i_next = fb.ins().iadd(i, one);
    fb.ins().jump(header, &[BlockArg::Value(i_next)]);
    fb.seal_block(header); // both predecessors (setup + body) are now declared

    // exit: control CONTINUES; the comprehension's value is the materialized handle — a flat DynList (scalar
    // body) or a DynMat carrying its row width (a fixed-width vector body, 2c.3), which the RETURN reshapes.
    fb.switch_to_block(exit);
    match row_width {
        Some(width) => Ok(Lowered::DynMat {
            handle: list,
            width,
        }),
        None => Ok(Lowered::DynList(list)),
    }
}

/// A compiled sub-expression's value, SCALARIZED (P.1.6 rung A). `Num`/`Bool` are a single IR value; `Vec`
/// is a FIXED-size vector carried as its element `Lowered`s at COMPILE time — a `[a,b,c]` literal, a vector
/// argument, or the result of elementwise / dot / scale arithmetic. A scalarized vector never touches memory:
/// a STATIC index picks an element, a dot UNROLLS the lane reduction, so a vector that stays statically-shaped
/// and statically-indexed fully scalarizes. A runtime-shaped / runtime-indexed vector DECLINES (that's rung
/// D's dynamic-list ABI). Nested (a matrix — `Vec` of `Vec`) is representable but rung A's arithmetic requires
/// FLAT vectors (`Num` elements); a matrix operand declines.
#[derive(Clone)]
enum Lowered {
    Num(Value),
    Bool(Value),
    Vec(Vec<Lowered>),
    /// A DYNAMIC-length list (P.1.6 rung-D 2b.2) — a runtime handle (`*mut Vec<f64>`, an IR pointer value) to a
    /// `Vec<f64>` materialized in the call's [`JitArena`] by a comprehension. Unlike `Vec` (compile-time fixed
    /// shape), its length lives at runtime; it's consumed by `len`, iteration (`[for(x = dynlist) …]`), and the
    /// function RETURN (flagged as the result). Arbitrary dynamic INDEXING (`dynlist[i]`) is rung 2b.2b.
    DynList(Value),
    /// A COMPILE-TIME-constant boolean (P.1.6 rung-D 2b.4 const-folding) — a type predicate whose answer is known
    /// per specialization (`is_undef(x)` → `false`, `is_list(Vec)` → `true`, …). Kept unmaterialized so a ternary
    /// with a `ConstBool` condition PRUNES the un-taken branch (never compiles it — that's how a compile-time-dead
    /// `dim==1 ? scalar : matrix` skips the matrix path). Materialized to a `Bool` (`i8` 0/1) only when it feeds a
    /// runtime op (`&&` with a runtime bool, a `select`, the function return).
    ConstBool(bool),
    /// A COMPILE-TIME-constant number (P.1.6 rung-D 2b.4 const-folding) — a literal, `len` of a fixed vector, or
    /// the result of folding const arithmetic. Kept unmaterialized so a comparison (`len(v) == 3`) can fold to a
    /// [`Lowered::ConstBool`] that then PRUNES a ternary branch (length/dimension dispatch on a fixed vector).
    /// Materialized to a `Num` (`fb.ins().f64const`) only when it feeds a runtime op — the fold uses Rust `f64`
    /// ops that are bit-identical to the Cranelift IR ops / the `jit_fmod`/`jit_powf` helpers.
    ConstNum(f64),
    /// A COMPILE-TIME-known `undef` (P.1.6 rung-D 2c.3 const-folding) — chiefly `len` of a NON-list (in a scalar
    /// specialization `len(scalar)` is statically `undef`), the `len of a non-vector` blocker. Folds like its
    /// siblings: `is_undef` → `true`, the other type predicates → `false`, `==`/`!=` → a `ConstBool` (undef
    /// equals only undef), an ORDERED comparison → another `ConstUndef` (undef is non-orderable → undef, per
    /// `ops::order`), and `truthy` → `false` (undef is falsy) so `len(x) ? … : …` and `len(x)==N ? … : …`
    /// PRUNE. It has no numeric value, so feeding it a runtime numeric op DECLINES (`num` → `Err`, like a bool).
    ConstUndef,
    /// A DYNAMIC-length MATRIX (P.1.6 rung-D 2c.3) — a comprehension whose body is a FIXED-WIDTH numeric vector,
    /// `[for(i = …) [a, b, c]]`. Stored in the SAME flat arena `Vec<f64>` as [`Lowered::DynList`] (`handle`),
    /// `width` (W) scalars pushed per row in row-major order; `width` is compile-time-known (the body's shape is
    /// structural), the ROW COUNT runtime. Consumed by `len` (→ row count = flat_len / W), `is_list`, and the
    /// RETURN (the dispatch reshapes the flat buffer into a `List` of `width`-chunks). Indexing / arithmetic ON a
    /// DynMat is out of scope (declines) — it's a materialize-and-return value like `DynList`.
    DynMat {
        handle: Value,
        width: usize,
    },
}

impl Lowered {
    /// The single IR `f64` VALUE of a numeric `Lowered` — a `Num` already is one, a `ConstNum` MATERIALIZES to an
    /// `f64const` (hence `fb`). A bool / vector / dynamic list DECLINES: an arithmetic/comparison operand, a
    /// math-builtin arg, or a scalar return that turned out to be one of those isn't in the numeric subset.
    fn num(&self, fb: &mut FunctionBuilder) -> Result<Value, JitError> {
        match self {
            Lowered::Num(v) => Ok(*v),
            Lowered::ConstNum(c) => Ok(fb.ins().f64const(*c)),
            Lowered::Bool(_) | Lowered::ConstBool(_) => Err(JitError::Unsupported(
                "a boolean where a number is required",
            )),
            Lowered::Vec(_) => Err(JitError::Unsupported("a vector where a number is required")),
            Lowered::DynList(_) => Err(JitError::Unsupported(
                "a dynamic list where a number is required",
            )),
            Lowered::ConstUndef => Err(JitError::Unsupported("undef where a number is required")),
            Lowered::DynMat { .. } => Err(JitError::Unsupported(
                "a dynamic matrix where a number is required",
            )),
        }
    }

    /// The compile-time `f64` value if this is a [`Lowered::ConstNum`], else `None` — the fold gate (a numeric op
    /// with two `const_num` operands folds; anything else materializes and runs at runtime).
    fn const_num(&self) -> Option<f64> {
        match self {
            Lowered::ConstNum(c) => Some(*c),
            _ => None,
        }
    }

    /// MATERIALIZE a `ConstNum` to a runtime `Num` (an `f64const` IR value), leaving every other `Lowered`
    /// untouched — used before running the runtime numeric-op path, so that path only ever sees `Num`/`Vec`/…,
    /// never a `ConstNum`.
    fn materialize_num(self, fb: &mut FunctionBuilder) -> Lowered {
        match self {
            Lowered::ConstNum(c) => Lowered::Num(fb.ins().f64const(c)),
            other => other,
        }
    }
}

/// Materialize a possibly-const BOOLEAN to its `i8` (0/1) IR value — a `Bool` already is one, a `ConstBool`
/// becomes an `iconst` (P.1.6 rung-D 2b.4). For when a const bool must feed a runtime op (a `select`, a
/// short-circuit with a runtime operand, the function return). A non-bool `Lowered` → `None`.
fn bool_ir(fb: &mut FunctionBuilder, v: &Lowered) -> Option<Value> {
    match v {
        Lowered::Bool(b) => Some(*b),
        Lowered::ConstBool(b) => Some(fb.ins().iconst(types::I8, i64::from(*b))),
        _ => None,
    }
}

/// Reduce a compiled sub-expression to its TRUTHINESS as an `i8` (0/1) — the interpreter's
/// `Value::is_truthy`. A `Bool` already IS that. A `Num` is truthy iff `!= 0.0` with `NaN` TRUTHY and
/// `±0` falsy — exactly `fcmp NotEqual` (`une`: unordered, so `NaN != 0` is true; `-0.0 != 0.0` is
/// false). This is what a ternary condition and `&&`/`||`/`!` test. A `Vec` DECLINES (a list's truthiness is
/// its non-emptiness — a compile-time constant here, but rare enough as a condition to leave for later).
fn truthy(fb: &mut FunctionBuilder, v: &Lowered) -> Result<Value, JitError> {
    match v {
        Lowered::Bool(b) => Ok(*b),
        Lowered::ConstBool(b) => Ok(fb.ins().iconst(types::I8, i64::from(*b))),
        Lowered::Num(n) => {
            let zero = fb.ins().f64const(0.0);
            Ok(fb.ins().fcmp(FloatCC::NotEqual, *n, zero))
        }
        // A compile-time-const number folds its truthiness: `c != 0.0` in Rust IS the `une` semantics — `NaN`
        // truthy, `±0` falsy — so this matches the `Num` runtime `fcmp NotEqual` bit-for-bit.
        Lowered::ConstNum(c) => Ok(fb.ins().iconst(types::I8, i64::from(*c != 0.0))),
        // `undef` is FALSY (`Value::is_truthy`) → a const `0`. So `len(scalar) && x` / a runtime `undef ? …`
        // materializes to a false flag, matching the interpreter (2c.3).
        Lowered::ConstUndef => Ok(fb.ins().iconst(types::I8, 0)),
        Lowered::Vec(_) => Err(JitError::Unsupported("a vector as a truth condition")),
        Lowered::DynList(_) => Err(JitError::Unsupported("a dynamic list as a truth condition")),
        Lowered::DynMat { .. } => Err(JitError::Unsupported(
            "a dynamic matrix as a truth condition",
        )),
    }
}

/// The COMPILE-TIME truthiness of a `Lowered`, or `None` if it isn't compile-time-known — a `ConstBool` is its
/// own value, a `ConstNum` is `c != 0.0` (the same `une` fold as [`truthy`]). Used to const-fold `&&`/`||` so a
/// wholly-const logical expression stays a `ConstBool` and can still prune a wrapping ternary.
#[allow(
    clippy::float_cmp,
    reason = "the `!= 0.0` une truthiness test — the exact `Num` runtime semantics"
)]
fn const_truthy(v: &Lowered) -> Option<bool> {
    match v {
        Lowered::ConstBool(b) => Some(*b),
        Lowered::ConstNum(c) => Some(*c != 0.0),
        // `undef` is FALSY (2c.3) — so a `len(scalar) ? … : …` / `len(x)==N ? …` dispatch prunes to the ELSE.
        Lowered::ConstUndef => Some(false),
        _ => None,
    }
}

/// The Cranelift float condition for an ORDERED comparison operator, matching the interpreter EXACTLY:
/// `<`/`<=`/`>`/`>=`/`==` go through `partial_cmp` (`NaN` → false), i.e. the ORDERED predicates; `!=` is
/// the interpreter's `x != y` (`NaN != NaN` is TRUE), i.e. UNORDERED not-equal. Any non-comparison op → `None`.
fn float_cc(op: BinOp) -> Option<FloatCC> {
    Some(match op {
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Le => FloatCC::LessThanOrEqual,
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::Ge => FloatCC::GreaterThanOrEqual,
        BinOp::Eq => FloatCC::Equal,
        BinOp::Ne => FloatCC::NotEqual,
        _ => return None,
    })
}

/// CONST-FOLD a comparison of two compile-time numbers (P.1.6 rung-D 2b.4) — Rust's native f64 comparators map
/// bit-for-bit onto [`float_cc`]'s Cranelift predicates: `< <= > >= ==` are ORDERED (any `NaN` → false, matching
/// the ordered `FloatCC`s and the interpreter's `partial_cmp`), and `!=` is Rust's `!(x==y)` = UNORDERED not-equal
/// (`NaN != NaN` → true, matching `FloatCC::NotEqual`/`une`). So the folded `ConstBool` equals the runtime `fcmp`.
#[allow(
    clippy::float_cmp,
    reason = "exact IEEE compare IS the semantics — must match the interpreter's `==`"
)]
fn const_fcmp(op: BinOp, x: f64, y: f64) -> bool {
    match op {
        BinOp::Lt => x < y,
        BinOp::Le => x <= y,
        BinOp::Gt => x > y,
        BinOp::Ge => x >= y,
        BinOp::Eq => x == y,
        BinOp::Ne => x != y,
        _ => unreachable!("const_fcmp is only reached behind float_cc"),
    }
}

/// CONST-FOLD a comparison where at least one operand is a compile-time `undef` (2c.3). `==`/`!=` yield a
/// `ConstBool` — `undef` equals ONLY `undef` (`Value::eq`: `(Undef,Undef)` true, every cross-type pair false),
/// so `==` is true iff BOTH are undef; an ORDERED `<`/`<=`/`>`/`>=` yields `undef` again (undef is non-orderable,
/// so `ops::order` returns `Undef`), staying a `ConstUndef` a wrapping `truthy`/ternary then treats as falsy.
fn fold_undef_cmp(op: BinOp, a: &Lowered, b: &Lowered) -> Lowered {
    let both_undef = matches!(a, Lowered::ConstUndef) && matches!(b, Lowered::ConstUndef);
    match op {
        BinOp::Eq => Lowered::ConstBool(both_undef),
        BinOp::Ne => Lowered::ConstBool(!both_undef),
        _ => Lowered::ConstUndef, // an ordered comparison of undef is itself undef
    }
}

/// Recursively lower `expr` to a Cranelift value + its [`Lowered`] type. Left operand before right — but for pure
/// numeric ops the operand ORDER doesn't affect the result bits (the operation is the same `fadd(a, b)`
/// either way); what matters is that we emit the operation itself, never a fused or reordered variant.
/// The AST is `MAX_DEPTH`-bounded by the parser, so this recursion can't overflow.
///
/// Type discipline keeps it sound: arithmetic requires `Num` operands, a comparison yields `Bool`, a
/// ternary's branches must AGREE in type, and `&&`/`||`/`!` operate on truthiness. A construct outside the
/// subset (a call, an index, a free variable, a bitwise op, a mixed-type ternary) DECLINES.
/// In-scope LOCAL bindings — `let`-bound names and inlined-call params → their compiled [`Lowered`] value.
/// Checked before the parameter `index`. Carried by-reference in [`Lower`] so entering a `let` just makes a
/// fresh map + a copied `Lower` pointing at it (no signature threading); `Lower` is `Copy`.
type LetEnv<'a> = BTreeMap<&'a str, Lowered>;

/// The program's user functions the JIT can INLINE: name → (parameters, body). A call to one splices its
/// body into the caller (fresh env binding its params to the arg values, an unfilled param to its default) —
/// the whole-function absorption that makes the JIT reach BOSL2's call chains. Carries full `Parameter`s
/// (name + default), not just names, so a SHORT call can bind the missing params to their defaults.
type FnDefs<'a> = BTreeMap<&'a str, (&'a [Parameter], &'a Expr)>;

/// The program's top-level CONSTANTS the JIT can inline: name → value expr (P.1.4 globals). A body's free
/// variable that names one resolves by compiling the constant's value-expr IN AN EMPTY SCOPE — a
/// self-contained numeric constant (`_EPSILON = 1e-9`, `INF = 1/0`, `PHI = (1+sqrt(5))/2`) inlines; one that
/// references ANOTHER global declines (see the `Ident` arm). Every top-level assignment is here, numeric or
/// not — a non-numeric one (a vector/string constant) simply makes its referrer DECLINE when its value-expr
/// compiles, so no filtering is needed and the decline reason names the actual blocker.
type Globals<'a> = BTreeMap<&'a str, &'a Expr>;

/// Max inline nesting before a call DECLINES — a runaway guard for pathological non-recursive chains (and
/// the coarse bound until step-3 real recursion). Deep enough for real BOSL2 numeric call chains.
const INLINE_LIMIT: usize = 32;

/// Max AST-structural depth [`compile_expr`] recurses before a body DECLINES (Q.7) — the compile-complexity
/// guard that stops a pathological deep expression (a left-assoc chain `x+x+…`, the one shape the parser
/// DOESN'T depth-bound — it parses iteratively) from overflowing the recursion. Calibrated from measurement,
/// not guessed: the WHOLE BOSL2 corpus tops out at depth 21, and the recursion overflows a 2MB stack at
/// ~160 in a debug build (frames are fat there; ~1500 in release). 64 sits 3× ABOVE any real body and ~2.5×
/// BELOW the worst-case (debug) overflow — so it costs zero real coverage yet the crash is unreachable on
/// any build. A body deeper than 64 (absurd for numeric code) simply declines to the interpreter, which
/// evaluates it fine on its explicit stack.
const MAX_LOWER_DEPTH: u32 = 64;

/// The shared lowering context — everything [`compile_expr`] threads down besides the builder itself.
/// Carried by value (it's `Copy`) so a scope that adds bindings just spreads a new one: `Lower { locals:
/// &inner, ..*lower }`. Holds the params pointer, the assert-failure out-param, the parameter index, the
/// in-scope `let` locals, and the helper `FuncRef`s.
#[derive(Clone, Copy)]
struct Lower<'a> {
    params_ptr: Value,
    raised_ptr: Value,
    index: &'a BTreeMap<&'a str, usize>,
    locals: &'a LetEnv<'a>,
    /// Every user function available to inline (whole program). Immutable per compile.
    defs: &'a FnDefs<'a>,
    /// The program's top-level constants (P.1.4 globals): a free variable naming one resolves by compiling its
    /// value-expr. EMPTY when compiling a constant's own value-expr, so a constant referencing another global
    /// declines (the safe match for the interpreter's whole-scope forward-reference rule) — see the `Ident` arm.
    globals: &'a Globals<'a>,
    /// The function names currently being inlined, outermost first — recursion guard (a callee already here
    /// DECLINES) + depth bound. Grows one entry per inline.
    inlining: &'a [&'a str],
    /// AST-structural recursion depth of [`compile_expr`] (Q.7). The parser does NOT bound this — a
    /// left-assoc operator chain (`x+x+…`) or a unary run (`----x`) parses ITERATIVELY into an arbitrarily
    /// deep tree WITHOUT tripping its nesting `MAX_DEPTH`, so `compile_expr` (which recurses on the AST)
    /// would blow the stack (~1500-deep on a 2MB thread → SIGABRT) where the explicit-stack INTERPRETER
    /// survives. Bounded by [`MAX_LOWER_DEPTH`]: past it the body DECLINES (a clean `JitError`, interpreter
    /// owns it), never overflows. Incremented at each `compile_expr` entry.
    depth: u32,
    /// The woven `RandStream` pointer (the 4th ABI param) — a JIT'd seedless `rands()` passes it to
    /// `jit_rand_next`. Untouched by a body that never draws (P.1.6 rung-D piece 1).
    rand_ptr: Value,
    /// The dynamic-list ARENA pointer (the 5th ABI param, `*mut JitArena`) — a JIT'd comprehension allocates
    /// its `DynList`(s) here (P.1.6 rung-D 2b.2). Untouched by a body with no comprehension.
    arena_ptr: Value,
    fmod: FuncRef,
    powf: FuncRef,
    fmin: FuncRef,
    fmax: FuncRef,
    math: FuncRef,
    rand_next: FuncRef,
    range_len: FuncRef,
    as_index: FuncRef,
    arena_new_list: FuncRef,
    vec_push: FuncRef,
    vec_len: FuncRef,
    vec_get: FuncRef,
    vec_bound: FuncRef,
    set_result: FuncRef,
}

#[allow(
    clippy::too_many_lines,
    reason = "the per-ExprKind lowering — one arm per node kind; splitting scatters the shared builder"
)]
fn compile_expr(fb: &mut FunctionBuilder, expr: &Expr, lower: &Lower) -> Result<Lowered, JitError> {
    // Q.7 compile-complexity guard: the parser does NOT bound AST depth for an operator chain / unary run
    // (both parse iteratively), so a pathological body would overflow this very recursion. DECLINE past the
    // limit — a clean `JitError` the interpreter (explicit-stack) then owns — instead of a stack abort. The
    // `deeper` shadow makes every recursive `compile_expr` below (including the `..*lower` scoped copies for
    // `let`/inline/global) carry the incremented count for free.
    if lower.depth >= MAX_LOWER_DEPTH {
        return Err(JitError::Unsupported(
            "expression nested past the JIT depth limit",
        ));
    }
    let deeper = Lower {
        depth: lower.depth + 1,
        ..*lower
    };
    let lower = &deeper;
    match &expr.kind {
        // A numeric literal stays COMPILE-TIME-const (P.1.6 rung-D 2b.4) so a comparison over it can fold to a
        // `ConstBool` and PRUNE a ternary branch (`len(v) == 3 ? … : …`). Materialized to a `Num` the moment it
        // feeds a runtime op — the fold path (Binary, below) keeps the runtime match from ever seeing a `ConstNum`.
        ExprKind::Num(n) => Ok(Lowered::ConstNum(*n)),
        // A bool literal (`true`/`false`) → the `i8` 0/1 a `Bool` is (P.1.4e). Lets a predicate body like
        // `cond ? true : false` compile — the ternary's branches now agree as `Bool`.
        ExprKind::Bool(b) => Ok(Lowered::Bool(fb.ins().iconst(types::I8, i64::from(*b)))),
        ExprKind::Ident(name) => {
            // A `$`-variable read is DYNAMICALLY scoped — the interpreter resolves it up the runtime CALL
            // chain, so NO lexical resolution here (a param slot, a let-local, an inlined global) is
            // trustworthy; a body reading one declines outright and stays interpreted (task #51: a
            // top-level `$fn = 32;` arrived via the consts and a compiled `$fn`-reader inlined 32, wrong
            // under any dynamic override). Checked BEFORE the env/param lookups on purpose: an INLINED
            // callee's `$`-read must not quietly resolve against the outer function's like-named binding.
            // The dispatch gate separately declines calls with explicit `$`-args; this guards the
            // inherited-context route in.
            if name.starts_with('$') {
                return Err(JitError::Unsupported("dynamically-scoped $-variable"));
            }
            // A `let`-bound local (or inlined-call param) shadows a parameter — check the env first. It may be
            // a scalarized vector (a `let(v = [a,b,c])`), so clone the whole `Lowered`.
            if let Some(v) = lower.locals.get(name.as_str()) {
                return Ok(v.clone());
            }
            // Then a parameter (read from the params pointer). Shadows a like-named global.
            if let Some(&i) = lower.index.get(name.as_str()) {
                let offset = i32::try_from(i * 8)
                    .map_err(|_| JitError::Unsupported("param offset overflow"))?;
                let v = fb.ins().load(
                    types::F64,
                    MemFlagsData::trusted(),
                    lower.params_ptr,
                    offset,
                );
                // A parameter is always a number here — the dispatch gate only routes all-`Num` calls to the JIT.
                return Ok(Lowered::Num(v));
            }
            // A free variable may be a top-level CONSTANT (P.1.4 globals): resolve it by compiling its
            // value-expr in an EMPTY scope — no params, no locals, and NO other globals. A self-contained
            // numeric constant (`_EPSILON = 1e-9`, `INF = 1/0` → +inf, `PHI = (1+sqrt(5))/2`, `NAN = acos(2)`)
            // inlines; one that references ANOTHER global hits the empty globals below and DECLINES. That
            // decline is the safe match for the interpreter's whole-scope forward-reference rule (`A = B+1;
            // B = 2;` gives `A = undef+1`, which a re-compiled value-expr would NOT reproduce) — so we never
            // resolve a constant that reads another. Empty globals also make a cycle impossible, so no extra
            // guard is needed; `defs`/`inlining` ride through, so a constant defined by a user-fn call still
            // inlines.
            if let Some(&gvalue) = lower.globals.get(name.as_str()) {
                let empty_index = BTreeMap::new();
                let empty_locals = LetEnv::new();
                let empty_globals = Globals::new();
                let glower = Lower {
                    index: &empty_index,
                    locals: &empty_locals,
                    globals: &empty_globals,
                    ..*lower
                };
                return compile_expr(fb, gvalue, &glower);
            }
            Err(JitError::Unsupported("non-parameter identifier"))
        }
        ExprKind::Unary { op, operand } => {
            let v = compile_expr(fb, operand, lower)?;
            match op {
                // `-x` negates a number, or elementwise-negates a vector (the interpreter's `apply_unary`).
                UnOp::Neg => neg_lowered(fb, v),
                // Unary `+` is a Num-only passthrough (matches the prior scalar behavior).
                UnOp::Pos => Ok(Lowered::Num(v.num(fb)?)),
                // `!x` = `!is_truthy(x)` → a Bool. `(truthy == 0)` inverts the 0/1 flag.
                UnOp::Not => {
                    // Const-fold `!x` for ANY compile-time-truthy `x` — a `ConstBool`, `ConstNum`, or `ConstUndef`
                    // (`!undef` = `true`, undef being falsy) → a `ConstBool` (2b.4 + 2c.3), so `!is_undef(x)` /
                    // `!(len(x)==N)` stays compile-time for a wrapping ternary; a runtime bool inverts the 0/1 flag.
                    if let Some(t) = const_truthy(&v) {
                        return Ok(Lowered::ConstBool(!t));
                    }
                    let t = truthy(fb, &v)?;
                    Ok(Lowered::Bool(fb.ins().icmp_imm(IntCC::Equal, t, 0)))
                }
                UnOp::BitNot => Err(JitError::Unsupported("bitwise-not")),
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let a = compile_expr(fb, lhs, lower)?;
            let b = compile_expr(fb, rhs, lower)?;
            // `&&`/`||`: the interpreter returns `Bool(truthy(a) OP truthy(b))`. Both operands are
            // side-effect-free here, so eager evaluation equals short-circuit — same bool, no float rounding.
            if matches!(op, BinOp::And | BinOp::Or) {
                // Const-fold when BOTH operands have compile-time truthiness (a const bool OR a const number,
                // P.1.6 rung-D 2b.4) — keeps const-ness so a wrapping ternary can still prune. A mixed
                // const/runtime pair materializes to a runtime `Bool`.
                if let (Some(x), Some(y)) = (const_truthy(&a), const_truthy(&b)) {
                    let r = if matches!(op, BinOp::And) {
                        x && y
                    } else {
                        x || y
                    };
                    return Ok(Lowered::ConstBool(r));
                }
                let ta = truthy(fb, &a)?;
                let tb = truthy(fb, &b)?;
                let r = if matches!(op, BinOp::And) {
                    fb.ins().band(ta, tb)
                } else {
                    fb.ins().bor(ta, tb)
                };
                return Ok(Lowered::Bool(r));
            }
            // A comparison: both operands must be numbers (the interpreter's `<` etc. on numbers; a vector
            // comparison isn't in the subset).
            if let Some(cc) = float_cc(*op) {
                // An `undef` operand (2c.3, a `len(scalar)`): the interpreter's undef comparison — `==`/`!=` give
                // a BOOL (undef equals ONLY undef), an ORDERED `<`/`>`/… gives UNDEF (non-orderable). Fold both,
                // so `len(x)==N` (scalar `x`) prunes and `len(x)<N` propagates undef into a pruning ternary.
                if matches!(a, Lowered::ConstUndef) || matches!(b, Lowered::ConstUndef) {
                    return Ok(fold_undef_cmp(*op, &a, &b));
                }
                // Const-fold two compile-time numbers → a `ConstBool` (`len(v) == 3` dispatch) that can PRUNE a
                // wrapping ternary. `const_fcmp` is bit-identical to the runtime `fcmp` (same ordered/une split).
                if let (Some(x), Some(y)) = (a.const_num(), b.const_num()) {
                    return Ok(Lowered::ConstBool(const_fcmp(*op, x, y)));
                }
                let (a, b) = (a.num(fb)?, b.num(fb)?);
                return Ok(Lowered::Bool(fb.ins().fcmp(cc, a, b)));
            }
            // Const-fold scalar arithmetic of two compile-time numbers → a `ConstNum` (`2 * 3 + 1` collapses to a
            // literal). Rust's `+ - * /` are the exact IEEE ops Cranelift's `fadd`/… emit (no FMA fusion), and `%`/
            // `^` route through the SAME `jit_fmod`/`jit_powf` the runtime path calls — so the fold matches bit-for-bit.
            if let (Some(x), Some(y)) = (a.const_num(), b.const_num()) {
                let folded = match op {
                    BinOp::Add => Some(x + y),
                    BinOp::Sub => Some(x - y),
                    BinOp::Mul => Some(x * y),
                    BinOp::Div => Some(x / y),
                    BinOp::Mod => Some(jit_fmod(x, y)),
                    BinOp::Pow => Some(jit_powf(x, y)),
                    _ => None,
                };
                if let Some(r) = folded {
                    return Ok(Lowered::ConstNum(r));
                }
            }
            // Not fully const: MATERIALIZE any `ConstNum` operand to a runtime `Num` so the arithmetic match below
            // only ever sees `Num`/`Vec`/`Bool`/… — never a `ConstNum`.
            let (a, b) = (a.materialize_num(fb), b.materialize_num(fb));
            // Arithmetic: scalar ⊙ scalar, or scalarized VECTOR ops (P.1.6 rung A). The vector cases mirror
            // `ops::apply_binary` EXACTLY: `+`/`−` elementwise, scalar `×`/`÷` scale, `vec × vec` = the 4-lane
            // dot product. Anything else (a matrix, a `%`/`^` on vectors, mismatched lengths) DECLINES.
            match (op, &a, &b) {
                (_, Lowered::Num(x), Lowered::Num(y)) => {
                    let (x, y) = (*x, *y);
                    let v = match op {
                        BinOp::Add => fb.ins().fadd(x, y),
                        BinOp::Sub => fb.ins().fsub(x, y),
                        BinOp::Mul => fb.ins().fmul(x, y),
                        BinOp::Div => fb.ins().fdiv(x, y),
                        BinOp::Mod => {
                            let call = fb.ins().call(lower.fmod, &[x, y]);
                            fb.inst_results(call)[0]
                        }
                        BinOp::Pow => {
                            let call = fb.ins().call(lower.powf, &[x, y]);
                            fb.inst_results(call)[0]
                        }
                        _ => return Err(JitError::Unsupported("non-arithmetic binary op")),
                    };
                    Ok(Lowered::Num(v))
                }
                (BinOp::Add | BinOp::Sub, Lowered::Vec(x), Lowered::Vec(y))
                    if x.len() == y.len() =>
                {
                    Ok(Lowered::Vec(vec_elementwise(fb, *op, x, y)?))
                }
                (BinOp::Mul, Lowered::Num(s), Lowered::Vec(v))
                | (BinOp::Mul, Lowered::Vec(v), Lowered::Num(s)) => {
                    Ok(Lowered::Vec(vec_scale(fb, v, *s)?))
                }
                // `vec/mat * vec/mat` — the interpreter's LINEAR-ALGEBRA `*` (2c.2b): vec·vec dot, vec×mat,
                // mat×vec, mat×mat. Dispatched by compile-time shape; a non-rectangular / empty / dimension-
                // mismatched operand (the interpreter's `undef`) DECLINES inside `vec_mat_product`.
                (BinOp::Mul, Lowered::Vec(x), Lowered::Vec(y)) => vec_mat_product(fb, x, y),
                (BinOp::Div, Lowered::Vec(v), Lowered::Num(s)) => {
                    Ok(Lowered::Vec(vec_div(fb, v, *s, true)?))
                }
                (BinOp::Div, Lowered::Num(s), Lowered::Vec(v)) => {
                    Ok(Lowered::Vec(vec_div(fb, v, *s, false)?))
                }
                _ => Err(JitError::Unsupported(
                    "unsupported operand types for arithmetic",
                )),
            }
        }
        // `c ? then : els`. A COMPILE-TIME-known condition (P.1.6 rung-D 2b.4) PRUNES the un-taken branch —
        // compiling ONLY the taken one, so a compile-time-dead branch (an un-JIT-able `dim==1 ? scalar : matrix`
        // matrix path) is never touched. Otherwise EAGER `select`: the interpreter evaluates only the taken
        // branch, but both are side-effect-free arithmetic here, so eager select is bit-identical (the untaken
        // branch's discarded NaN/inf can't affect the chosen result). Branches must AGREE in shape.
        ExprKind::Ternary { cond, then, els } => {
            let cv = compile_expr(fb, cond, lower)?;
            // A COMPILE-TIME-known condition (a const bool OR a const number, P.1.6 rung-D 2b.4) prunes — compile
            // ONLY the taken branch, so a compile-time-dead un-JIT-able branch is never touched.
            if let Some(b) = const_truthy(&cv) {
                return compile_expr(fb, if b { then } else { els }, lower);
            }
            let c = truthy(fb, &cv)?;
            let tv = compile_expr(fb, then, lower)?;
            let ev = compile_expr(fb, els, lower)?;
            select_lowered(fb, c, tv, ev)
        }
        // `let(x=e1, y=e2) body` — SEQUENTIAL bindings (a later one sees earlier ones), then the body in the
        // extended env. A binding is single-assignment, so it's just a name → its compiled IR value/type;
        // no store, no slot. The body sees every binding. (Unlocks the 24.8% `let` blocker.)
        ExprKind::Let { bindings, body } => {
            let mut locals = lower.locals.clone();
            for b in bindings {
                let name = b
                    .name
                    .as_deref()
                    .ok_or(JitError::Unsupported("let binding without a name"))?;
                let scoped = Lower {
                    locals: &locals,
                    ..*lower
                };
                let v = compile_expr(fb, &b.value, &scoped)?;
                locals.insert(name, v);
            }
            let scoped = Lower {
                locals: &locals,
                ..*lower
            };
            compile_expr(fb, body, &scoped)
        }
        // A call resolves in four ways: (1) a scalar math builtin → a call into OUR math (P.1.4b), (2) a USER
        // function → INLINE its body, (2.5) a VECTOR builtin (norm/len/cross) over scalarized vectors (rung B),
        // (3) else DECLINE (a variadic/list builtin, a dynamic `(expr)()` callee, an undefined name, a named arg).
        ExprKind::Call { callee, args } => {
            let ExprKind::Ident(name) = &callee.kind else {
                return Err(JitError::Unsupported("call"));
            };
            // (1) scalar math builtin, bit-identical to the interpreter (degrees + snapping) via `jit_math`.
            if let Some((id, arity)) = jit_math_id(name) {
                if args.len() != usize::from(arity) || args.iter().any(|a| a.name.is_some()) {
                    return Err(JitError::Unsupported("call"));
                }
                let a = compile_expr(fb, &args[0].value, lower)?.num(fb)?;
                let b = if arity == 2 {
                    compile_expr(fb, &args[1].value, lower)?.num(fb)?
                } else {
                    fb.ins().f64const(0.0) // a unary op ignores the second helper argument
                };
                let id_v = fb.ins().iconst(types::I32, i64::from(id));
                let call = fb.ins().call(lower.math, &[id_v, a, b]);
                return Ok(Lowered::Num(fb.inst_results(call)[0]));
            }
            // (2) user function → INLINE. Its body compiles into the caller with its params bound to the arg
            // VALUES (compiled in the CALLER's env), in a FRESH env (lexical: the callee sees only its own
            // params + let-locals, not the caller's). Recursion (callee already on the inline stack) and a
            // runaway depth DECLINE — step-3 real recursion is the follow-on. Exact positional arity only for
            // now (defaults + a free-var/global reference are the next inlining steps).
            if let Some(&(cparams, cbody)) = lower.defs.get(name.as_str()) {
                if lower.inlining.contains(&name.as_str()) {
                    return Err(JitError::Unsupported("recursion"));
                }
                if lower.inlining.len() >= INLINE_LIMIT {
                    return Err(JitError::Unsupported("inline-depth-limit"));
                }
                // Bind ARG SLOTS exactly as the interpreter's `push_call`: a POSITIONAL arg fills the next slot
                // left-to-right; a NAMED arg binds by param name (overriding a positional at that slot). A
                // `$`-arg (a dynamic override), an EXTRA positional, or an UNMATCHED named arg → DECLINE — the
                // interpreter DROPS the latter two WITHOUT evaluating them, so declining sidesteps that eval-
                // order subtlety and is safe (the interpreter owns the call). Slots compile in PARAM order below
                // — the interpreter's eval order — so a nested `rands` draws the same sequence whatever the
                // call-site arg order (P.1.6: named args, the `call` blocker's cheap slice).
                let mut arg_slots: Vec<Option<&Expr>> = vec![None; cparams.len()];
                let mut positional = 0usize;
                for arg in args {
                    match &arg.name {
                        None => {
                            let slot = arg_slots
                                .get_mut(positional)
                                .ok_or(JitError::Unsupported("extra positional arg"))?;
                            *slot = Some(&arg.value);
                            positional += 1;
                        }
                        Some(n) if n.starts_with('$') => {
                            return Err(JitError::Unsupported("$-arg"));
                        }
                        Some(n) => {
                            let i = cparams
                                .iter()
                                .position(|p| p.name.as_ref() == n.as_ref())
                                .ok_or(JitError::Unsupported("unmatched named arg"))?;
                            arg_slots[i] = Some(&arg.value);
                        }
                    }
                }
                let empty_index = BTreeMap::new(); // callee params live in `callee_env`, not `params_ptr`
                let empty_locals = LetEnv::new();
                let mut callee_env = LetEnv::new();
                for (i, p) in cparams.iter().enumerate() {
                    let pname = p.name.as_ref();
                    if let Some(expr) = arg_slots[i] {
                        // A provided arg (positional OR named) is compiled in the CALLER's env (may be a vector).
                        let v = compile_expr(fb, expr, lower)?;
                        callee_env.insert(pname, v);
                    } else if let Some(default) = p.default.as_ref() {
                        // Unfilled → its DEFAULT, compiled in the DEFINITION scope (no caller locals, no
                        // sibling params) — matching the interpreter's documented default-eval simplification.
                        let def_lower = Lower {
                            index: &empty_index,
                            locals: &empty_locals,
                            ..*lower
                        };
                        let v = compile_expr(fb, default, &def_lower)?;
                        callee_env.insert(pname, v);
                    }
                    // else: no arg, no default → leave `pname` unbound; the body DECLINES if it uses it (the
                    // interpreter would see `undef` there, which the numeric JIT can't represent anyway).
                }
                let mut stack = lower.inlining.to_vec();
                stack.push(name.as_str());
                let callee_lower = Lower {
                    index: &empty_index,
                    locals: &callee_env,
                    inlining: &stack,
                    ..*lower
                };
                return compile_expr(fb, cbody, &callee_lower);
            }
            // (2.5) a LIST/VECTOR builtin (`norm`/`len`/`cross`/`min`/`max`) over scalarized args (P.1.6 rung
            // B). Checked AFTER the user-fn inline so a program's own redefinition WINS — matching the
            // interpreter's user-function-first resolution (the builtin only fires when no user def shadows it).
            if matches!(name.as_str(), "len" | "norm" | "cross" | "min" | "max") {
                return compile_vec_builtin(fb, name, args, lower);
            }
            // (2.6) seedless `rands(min, max, count)` with a LITERAL count → `count` draws from the woven
            // RandStream (P.1.6 rung-D piece 1). Also user-shadowable, so checked here after the inline.
            if name.as_str() == "rands" {
                return compile_seedless_rands(fb, args, lower);
            }
            // (2.7) a TYPE PREDICATE (`is_undef`/`is_num`/…) → mostly a COMPILE-TIME `ConstBool` from the arg's
            // Lowered TYPE (P.1.6 rung-D 2b.4 const-folding). Const-folding a predicate lets a ternary PRUNE the
            // un-taken branch — the point of the whole feature. User-shadowable, so after the inline.
            if matches!(
                name.as_str(),
                "is_undef" | "is_bool" | "is_num" | "is_string" | "is_list" | "is_function"
            ) {
                if args.len() != 1 || args.iter().any(|a| a.name.is_some()) {
                    return Err(JitError::Unsupported("call"));
                }
                let arg = compile_expr(fb, &args[0].value, lower)?;
                return Ok(compile_type_predicate(fb, name, &arg));
            }
            // (3) not inlinable.
            Err(JitError::Unsupported("call"))
        }
        // `assert(cond) body` — a passthrough guard. Compile the CONDITION; if it's falsy, OR a 1 into the
        // `raised` out-param, and the value is the guarded body. On failure the caller ignores our f64 and
        // re-interprets to raise the exact error (message + locator), so computing the body eagerly is
        // harmless. The condition is the first POSITIONAL arg; a named/absent condition, or a bodyless
        // assert, DECLINES. (In practice the condition is usually a predicate CALL, which itself declines
        // until P.1.4b — assert-raise is the prerequisite that unwraps the layer, not the coverage win.)
        ExprKind::Assert { args, body } => {
            let Some(body) = body.as_deref() else {
                return Err(JitError::Unsupported("assert without a body value"));
            };
            let cond = match args.first() {
                Some(arg) if arg.name.is_none() => &arg.value,
                _ => {
                    return Err(JitError::Unsupported(
                        "assert without a positional condition",
                    ));
                }
            };
            let cv = compile_expr(fb, cond, lower)?;
            let t = truthy(fb, &cv)?;
            let failed = fb.ins().icmp_imm(IntCC::Equal, t, 0); // 1 iff the condition is falsy
            let prev = fb
                .ins()
                .load(types::I8, MemFlagsData::trusted(), lower.raised_ptr, 0);
            let now = fb.ins().bor(prev, failed);
            fb.ins()
                .store(MemFlagsData::trusted(), now, lower.raised_ptr, 0);
            compile_expr(fb, body, lower)
        }
        // A single-element `[for (…) …]` → a dynamic-length COMPREHENSION (P.1.6 rung-D 2b.2): materialize it
        // into the arena, yielding a `Lowered::DynList`. Otherwise a vector LITERAL `[a, b, c]` → a scalarized
        // `Lowered::Vec` of its compiled elements (rung A). A MIXED vector (a literal + a comprehension) or a
        // multi-element comprehension list declines here (rung 2b.N).
        ExprKind::Vector(elems) => {
            if let [single] = elems.as_slice()
                && let ExprKind::LcFor {
                    bindings,
                    body: lc_body,
                } = &single.kind
            {
                return compile_comprehension(fb, bindings, lc_body, lower);
            }
            let lowered: Result<Vec<Lowered>, JitError> =
                elems.iter().map(|e| compile_expr(fb, e, lower)).collect();
            Ok(Lowered::Vec(lowered?))
        }
        // `base[index]`. A scalarized fixed vector with a STATIC (literal) index picks the element at compile
        // time (P.1.6 rung A). A DYNAMIC list with a RUNTIME index (2b.2b) bounds-checks via `jit_vec_bound`
        // and, on out-of-range (`undef` in the interpreter — unrepresentable here), BAILS to the interpreter.
        ExprKind::Index { base, index } => {
            match compile_expr(fb, base, lower)? {
                Lowered::Vec(elems) => {
                    // Static index into a scalarized vector (rung A). A non-literal / negative / non-finite /
                    // out-of-range index → `undef` there → DECLINE (a scalarized vector can't hold undef).
                    let ExprKind::Num(n) = &index.kind else {
                        return Err(JitError::Unsupported("dynamic index of a fixed vector"));
                    };
                    if !n.is_finite() || *n < 0.0 {
                        return Err(JitError::Unsupported("index out of range"));
                    }
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "guarded finite && >= 0; matches the interpreter's `i as usize`, and an \
                        out-of-range index falls through to the `nth` miss → decline"
                    )]
                    let idx = *n as usize;
                    elems
                        .into_iter()
                        .nth(idx)
                        .ok_or(JitError::Unsupported("index out of range"))
                }
                Lowered::DynList(handle) => {
                    // A runtime index `i` into a DynList (2b.2b) — the gaussian_rands `nums[i]` shape. Resolve +
                    // bounds-check with `jit_vec_bound` (== `ops::index`: `i<0`/non-finite → -1; else `i as
                    // usize`, in-range iff `< len`). An out-of-range index (-1) is the interpreter's `undef`;
                    // the JIT can't represent it, so it BAILS immediately (raised + return → the dispatch's
                    // `None` → interpret), like the budget bail. In-range → the element via `jit_vec_get`.
                    let i_f = compile_expr(fb, index, lower)?.num(fb)?;
                    let idx = {
                        let call = fb.ins().call(lower.vec_bound, &[handle, i_f]);
                        fb.inst_results(call)[0]
                    };
                    let zero_i = fb.ins().iconst(types::I64, 0);
                    let out_of_range = fb.ins().icmp(IntCC::SignedLessThan, idx, zero_i); // idx == -1
                    let bail = fb.create_block();
                    let cont = fb.create_block();
                    fb.ins().brif(out_of_range, bail, &[], cont, &[]);
                    fb.seal_block(bail);
                    fb.seal_block(cont);
                    fb.switch_to_block(bail);
                    let one_flag = fb.ins().iconst(types::I8, 1);
                    fb.ins()
                        .store(MemFlagsData::trusted(), one_flag, lower.raised_ptr, 0);
                    let bail_ret = fb.ins().f64const(0.0);
                    fb.ins().return_(&[bail_ret]);
                    fb.switch_to_block(cont);
                    let call = fb.ins().call(lower.vec_get, &[handle, idx]);
                    Ok(Lowered::Num(fb.inst_results(call)[0]))
                }
                // `x[i]` on a NON-list is `undef` (`ops::index`) regardless of `i` — the `index of a non-vector`
                // blocker (2c.3). Fold to a compile-time `ConstUndef`, but FIRST compile `index` for its eval-
                // order side effects: the interpreter evaluates it (eager operands), so a nested `rands` must
                // still advance the stream (piece 1) and an inline assert must still fire. The result is discarded.
                _ => {
                    compile_expr(fb, index, lower)?;
                    Ok(Lowered::ConstUndef)
                }
            }
        }
        // `v.x`/`v.y`/`v.z` on a scalarized vector → element 0/1/2 (P.1.6 rung B). `ops::member` maps ONLY
        // x/y/z to an index and EVERYTHING else to `undef`; a `.z` on a too-short vector is `undef` too. The
        // JIT can't represent `undef`, so a non-xyz field OR an out-of-range axis DECLINES — same element,
        // no float op, so bit-identical to the interpreter's `index(base, axis)`.
        ExprKind::Member { base, field } => {
            let Lowered::Vec(elems) = compile_expr(fb, base, lower)? else {
                // `.x`/`.y`/… on a NON-vector is `undef` (`ops::member` → `index(non-list, axis)` → undef) — a
                // compile-time `ConstUndef` (2c.3), so a `member(scalar) == N ? …` folds instead of declining.
                return Ok(Lowered::ConstUndef);
            };
            let idx = match field.as_str() {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                // A non-xyz field is `undef` (`ops::member` maps ONLY x/y/z) — fold.
                _ => return Ok(Lowered::ConstUndef),
            };
            // A `.z` on a too-short vector is `undef` too (the `nth` miss) — fold rather than decline.
            match elems.into_iter().nth(idx) {
                Some(e) => Ok(e),
                None => Ok(Lowered::ConstUndef),
            }
        }
        // Everything else DECLINES — named so the EXPLAIN coverage histogram (P.1.4) shows WHICH node kind
        // blocks each function, i.e. the absorption ceiling per subset feature we might add next.
        other => Err(JitError::Unsupported(kind_name(other))),
    }
}

/// A type PREDICATE (`is_undef`/`is_bool`/`is_num`/`is_string`/`is_list`/`is_function`) on a compiled arg
/// (P.1.6 rung-D 2b.4). Mostly a COMPILE-TIME [`Lowered::ConstBool`] read off the arg's Lowered TYPE (known per
/// specialization) — the JIT never holds an `undef`/string/function value, so those are always `false`; a
/// `Vec`/`DynList` IS a list, a `Bool`/`ConstBool` IS a bool. The ONE runtime case is `is_num` on a `Num`: the
/// interpreter's `type==NUMBER && !isnan`, so `is_num(x) = x == x` (ORDERED equal — false for `NaN`, per L.2.8n).
fn compile_type_predicate(fb: &mut FunctionBuilder, name: &str, arg: &Lowered) -> Lowered {
    match name {
        // A compile-time `ConstUndef` (a `len(non-list)`, 2c.3) makes `is_undef` TRUE; every other `Lowered` is a
        // representable value → false. The JIT never represents strings / function values → those stay false.
        "is_undef" => Lowered::ConstBool(matches!(arg, Lowered::ConstUndef)),
        "is_string" | "is_function" => Lowered::ConstBool(false),
        "is_bool" => Lowered::ConstBool(matches!(arg, Lowered::Bool(_) | Lowered::ConstBool(_))),
        "is_list" => Lowered::ConstBool(matches!(
            arg,
            Lowered::Vec(_) | Lowered::DynList(_) | Lowered::DynMat { .. }
        )),
        "is_num" => match arg {
            // `is_num(NaN)` is FALSE (L.2.8n): `x == x` (ordered) is the runtime not-NaN test.
            Lowered::Num(x) => Lowered::Bool(fb.ins().fcmp(FloatCC::Equal, *x, *x)),
            // A const number folds it: `!c.is_nan()` IS the ordered `c == c` (false only for `NaN`).
            Lowered::ConstNum(c) => Lowered::ConstBool(!c.is_nan()),
            _ => Lowered::ConstBool(false),
        },
        _ => Lowered::ConstBool(false), // unreachable: the caller routes only the six predicates here
    }
}

/// Seedless `rands(min, max, count)` → `count` sequential draws from the woven [`RandStream`], each a
/// [`jit_rand_next`] call advancing the shared stream in SOURCE order (so the stream advances exactly as the
/// interpreter's `(0..count)` would, bit-identical). Mirrors `builtins::rands`'s SEEDLESS path: `min`/`max`
/// must be numbers, `count` is `as_index` (finite, ≥ 0, truncated). Two shapes by count:
/// - a SMALL LITERAL count (≤ [`MAX_VEC_ARG`]) → a scalarized [`Lowered::Vec`] (P.1.6 piece 1) — no memory.
/// - a DYNAMIC or large count → a draw LOOP materializing a [`Lowered::DynList`] (rung-D 2b.3) — the
///   `gaussian_rands` `nums = rands(0,1,dim*n*2)` shape. An `undef` count (`< 0` / non-finite) or one over
///   [`COMPREHENSION_BUDGET`] BAILS to the interpreter (raised → `None`), always safe.
///
/// DECLINES: a 4th `seed` arg (seeded rands is pure — a follow-on), a non-`Num` min|max, or a named arg.
fn compile_seedless_rands(
    fb: &mut FunctionBuilder,
    args: &[fab_lang::Arg],
    lower: &Lower,
) -> Result<Lowered, JitError> {
    // Seedless is EXACTLY 3 positional args; a 4th (seed) or a named arg declines.
    if args.len() != 3 || args.iter().any(|a| a.name.is_some()) {
        return Err(JitError::Unsupported("call"));
    }
    // min, max compiled in source order (their own side effects, incl. a nested rands, happen here — BEFORE the
    // count + draws, matching the interpreter evaluating the args before the builtin body).
    let min = compile_expr(fb, &args[0].value, lower)?.num(fb)?;
    let max = compile_expr(fb, &args[1].value, lower)?.num(fb)?;

    // A small NON-NEGATIVE literal count → the scalarized fixed vector (no arena). A big / invalid / dynamic
    // count falls through to the draw loop.
    if let ExprKind::Num(n) = &args[2].value.kind
        && n.is_finite()
        && *n >= 0.0
    {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "guarded finite && >= 0; matches the interpreter's `as_index` truncation to usize"
        )]
        let count = *n as usize;
        if count <= MAX_VEC_ARG {
            let mut elems = Vec::with_capacity(count);
            for _ in 0..count {
                let call = fb.ins().call(lower.rand_next, &[lower.rand_ptr, min, max]);
                elems.push(Lowered::Num(fb.inst_results(call)[0]));
            }
            return Ok(Lowered::Vec(elems));
        }
    }

    // DYNAMIC (or big / invalid) count → a draw loop into a DynList (2b.3). `count` is compiled ONCE, here,
    // after min/max (a literal recompiles to a const — harmless). `jit_as_index` validates it (undef → -1).
    let count_f = compile_expr(fb, &args[2].value, lower)?.num(fb)?;
    let count = {
        let call = fb.ins().call(lower.as_index, &[count_f]);
        fb.inst_results(call)[0]
    };
    // Bail on an undef count (-1) OR over-budget — both → the interpreter (which yields undef or the big list).
    let zero = fb.ins().iconst(types::I64, 0);
    let neg = fb.ins().icmp(IntCC::SignedLessThan, count, zero);
    let budget = fb.ins().iconst(types::I64, COMPREHENSION_BUDGET);
    let over = fb.ins().icmp(IntCC::SignedGreaterThan, count, budget);
    let bad = fb.ins().bor(neg, over);
    let bail = fb.create_block();
    let setup = fb.create_block();
    fb.ins().brif(bad, bail, &[], setup, &[]);
    fb.seal_block(bail);
    fb.seal_block(setup);
    fb.switch_to_block(bail);
    let one_flag = fb.ins().iconst(types::I8, 1);
    fb.ins()
        .store(MemFlagsData::trusted(), one_flag, lower.raised_ptr, 0);
    let bail_ret = fb.ins().f64const(0.0);
    fb.ins().return_(&[bail_ret]);

    // setup: the arena list + the draw loop (i = 0..count, each a jit_rand_next push).
    fb.switch_to_block(setup);
    let list = {
        let call = fb.ins().call(lower.arena_new_list, &[lower.arena_ptr]);
        fb.inst_results(call)[0]
    };
    let header = fb.create_block();
    fb.append_block_param(header, types::I64);
    let body_block = fb.create_block();
    let exit = fb.create_block();
    let zero_i = fb.ins().iconst(types::I64, 0);
    fb.ins().jump(header, &[BlockArg::Value(zero_i)]);
    fb.switch_to_block(header);
    let i = fb.block_params(header)[0];
    let cond = fb.ins().icmp(IntCC::SignedLessThan, i, count);
    fb.ins().brif(cond, body_block, &[], exit, &[]);
    fb.seal_block(body_block);
    fb.seal_block(exit);
    fb.switch_to_block(body_block);
    let draw = {
        let call = fb.ins().call(lower.rand_next, &[lower.rand_ptr, min, max]);
        fb.inst_results(call)[0]
    };
    fb.ins().call(lower.vec_push, &[list, draw]);
    let one = fb.ins().iconst(types::I64, 1);
    let i_next = fb.ins().iadd(i, one);
    fb.ins().jump(header, &[BlockArg::Value(i_next)]);
    fb.seal_block(header);
    fb.switch_to_block(exit);
    Ok(Lowered::DynList(list))
}

/// Whether `expr` (recursively) needs the RUNTIME environment the standalone [`compile_function`] doesn't
/// provide — a `rands` call (no woven `RandStream`, P.1.6 piece 1) OR a comprehension (no `JitArena`, piece 2).
/// Either would have `JitFn::call`'s NULL stream/arena pointer dereferenced, so the standalone API must
/// DECLINE such a body. Over-conservative (a seeded rands / an over-budget comprehension would decline in the
/// compiler anyway), but simple + safe. The registry path (which weaves both) has no such limit.
fn needs_runtime_env(expr: &Expr) -> bool {
    match &expr.kind {
        // Any comprehension node needs the arena.
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => true,
        ExprKind::Call { callee, args } => {
            matches!(&callee.kind, ExprKind::Ident(n) if n.as_str() == "rands")
                || needs_runtime_env(callee)
                || args.iter().any(|a| needs_runtime_env(&a.value))
        }
        ExprKind::Unary { operand, .. } => needs_runtime_env(operand),
        ExprKind::Binary { lhs, rhs, .. } => needs_runtime_env(lhs) || needs_runtime_env(rhs),
        ExprKind::Ternary { cond, then, els } => {
            needs_runtime_env(cond) || needs_runtime_env(then) || needs_runtime_env(els)
        }
        ExprKind::Index { base, index } => needs_runtime_env(base) || needs_runtime_env(index),
        ExprKind::Member { base, .. } => needs_runtime_env(base),
        ExprKind::Vector(es) => es.iter().any(needs_runtime_env),
        ExprKind::Let { bindings, body } => {
            bindings.iter().any(|b| needs_runtime_env(&b.value)) || needs_runtime_env(body)
        }
        ExprKind::Assert { args, body } => {
            args.iter().any(|a| needs_runtime_env(&a.value))
                || body.as_deref().is_some_and(needs_runtime_env)
        }
        _ => false,
    }
}

/// A LIST/VECTOR builtin (`norm`/`len`/`cross`/`min`/`max`) over scalarized args (P.1.6 rung B). Each
/// replicates [`super::builtins`]'s implementation EXACTLY so `fast == JIT` holds:
/// - `len(v)` → the element count as an `f64` const (a scalarized vector's length is known at COMPILE time).
/// - `norm(v)` → the interpreter's `iter().map(|x| x*x).sum().sqrt()` — a SEQUENTIAL `fold(0.0, +)`, NOT the
///   4-lane `dot` (which would diverge ≥4 elements), then `sqrt` through the shared math helper (== `.sqrt()`).
/// - `cross(a,b)` → the 3D cross (a `Vec`, so it composes: `norm(cross(a,b))` fully scalarizes) or the 2D cross
///   (a scalar), each `fmul`/`fsub` in the interpreter's operand order.
/// - `min`/`max` → the interpreter's `min_max`: reduce one vector arg, one scalar arg, or several scalar args
///   with a SEQUENTIAL `fold(head, …)` through the `jit_fmin`/`jit_fmax` helper (NaN-ignoring, unlike native).
///
/// A non-numeric arg, a wrong arity, a named arg, a cross of non-2/3-vectors, or a `min`/`max` of nothing
/// DECLINES (the interpreter would yield `undef`, which the JIT can't represent).
fn compile_vec_builtin(
    fb: &mut FunctionBuilder,
    name: &str,
    args: &[fab_lang::Arg],
    lower: &Lower,
) -> Result<Lowered, JitError> {
    if args.iter().any(|a| a.name.is_some()) {
        return Err(JitError::Unsupported("call")); // builtins take positional args here
    }
    match name {
        "len" => {
            if args.len() != 1 {
                return Err(JitError::Unsupported("call"));
            }
            match compile_expr(fb, &args[0].value, lower)? {
                // A scalarized fixed vector's length is a COMPILE-TIME constant (rung B) — a `ConstNum`, so
                // `len(v) == N` dimension dispatch folds to a `ConstBool` and PRUNES the wrong-dimension branch
                // (P.1.6 rung-D 2b.4). That's the whole point: a `len(v)==3 ? … : …` compiles only the taken arm.
                Lowered::Vec(elems) => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "a scalarized vector's length is tiny (<= MAX_VEC_ARG); f64 is exact"
                    )]
                    let n = elems.len() as f64;
                    Ok(Lowered::ConstNum(n))
                }
                // A DYNAMIC list's length is a runtime `jit_vec_len` (P.1.6 rung-D 2b.2). The i64 count is
                // exact as f64 (budget-capped ≤ 1e6), converted with `fcvt_from_sint`.
                Lowered::DynList(handle) => {
                    let call = fb.ins().call(lower.vec_len, &[handle]);
                    let n_i = fb.inst_results(call)[0];
                    Ok(Lowered::Num(fb.ins().fcvt_from_sint(types::F64, n_i)))
                }
                // A dynamic MATRIX's length is its ROW COUNT (2c.3) — the flat element count / the row width. The
                // flat count is a multiple of `width` (W pushes/row), so the signed i64 divide is exact.
                Lowered::DynMat { handle, width } => {
                    let call = fb.ins().call(lower.vec_len, &[handle]);
                    let flat_i = fb.inst_results(call)[0];
                    #[allow(
                        clippy::cast_possible_wrap,
                        reason = "width ∈ [1, MAX_VEC_ARG] — tiny, never wraps i64"
                    )]
                    let w = fb.ins().iconst(types::I64, width as i64);
                    let rows_i = fb.ins().sdiv(flat_i, w);
                    Ok(Lowered::Num(fb.ins().fcvt_from_sint(types::F64, rows_i)))
                }
                // `len` of a NON-list (a scalar / bool — in a scalar spec `len(scalar_param)`) is `undef` (2c.3):
                // a compile-time `ConstUndef` so `len(x) == N` / `len(x) ? …` FOLD + prune instead of declining.
                // (A string has a real length in OpenSCAD, but a string never reaches the JIT — it declines at the
                // arg boundary — so every non-list `Lowered` here is genuinely undef-length.)
                _ => Ok(Lowered::ConstUndef),
            }
        }
        "norm" => {
            if args.len() != 1 {
                return Err(JitError::Unsupported("call"));
            }
            let Lowered::Vec(elems) = compile_expr(fb, &args[0].value, lower)? else {
                return Err(JitError::Unsupported("norm of a non-vector"));
            };
            // Sequential sum of squares — `fold(0.0, +)`, MATCHING `iter().map(|x| x*x).sum()` bit-for-bit.
            let mut acc = fb.ins().f64const(0.0);
            for e in &elems {
                let x = e.num(fb)?;
                let sq = fb.ins().fmul(x, x);
                acc = fb.ins().fadd(acc, sq);
            }
            // `sqrt` through the SAME helper the scalar `sqrt()` builtin uses, so it's the interpreter's `.sqrt()`.
            let (sqrt_id, _) = jit_math_id("sqrt").ok_or(JitError::Unsupported("call"))?;
            let id_v = fb.ins().iconst(types::I32, i64::from(sqrt_id));
            let zero = fb.ins().f64const(0.0); // the helper's unused second arg
            let call = fb.ins().call(lower.math, &[id_v, acc, zero]);
            Ok(Lowered::Num(fb.inst_results(call)[0]))
        }
        "cross" => {
            if args.len() != 2 {
                return Err(JitError::Unsupported("call"));
            }
            let (Lowered::Vec(a), Lowered::Vec(b)) = (
                compile_expr(fb, &args[0].value, lower)?,
                compile_expr(fb, &args[1].value, lower)?,
            ) else {
                return Err(JitError::Unsupported("cross of a non-vector"));
            };
            // `x*y - z*w` = `fsub(fmul(x,y), fmul(z,w))`, in the interpreter's exact operand order.
            let sub_of_products = |fb: &mut FunctionBuilder, x, y, z, w| {
                let p1 = fb.ins().fmul(x, y);
                let p2 = fb.ins().fmul(z, w);
                fb.ins().fsub(p1, p2)
            };
            match (a.as_slice(), b.as_slice()) {
                // 3D cross → a VECTOR (`ops::cross`'s [a1·b2−a2·b1, a2·b0−a0·b2, a0·b1−a1·b0]).
                ([a0, a1, a2], [b0, b1, b2]) => {
                    let (a0, a1, a2) = (a0.num(fb)?, a1.num(fb)?, a2.num(fb)?);
                    let (b0, b1, b2) = (b0.num(fb)?, b1.num(fb)?, b2.num(fb)?);
                    let x = sub_of_products(fb, a1, b2, a2, b1);
                    let y = sub_of_products(fb, a2, b0, a0, b2);
                    let z = sub_of_products(fb, a0, b1, a1, b0);
                    Ok(Lowered::Vec(vec![
                        Lowered::Num(x),
                        Lowered::Num(y),
                        Lowered::Num(z),
                    ]))
                }
                // 2D cross → a SCALAR (`a0·b1 − a1·b0`).
                ([a0, a1], [b0, b1]) => {
                    let (a0, a1, b0, b1) = (a0.num(fb)?, a1.num(fb)?, b0.num(fb)?, b1.num(fb)?);
                    Ok(Lowered::Num(sub_of_products(fb, a0, b1, a1, b0)))
                }
                _ => Err(JitError::Unsupported("cross of non-2/3-vectors")),
            }
        }
        "min" | "max" => {
            let is_min = name == "min";
            let helper = if is_min { lower.fmin } else { lower.fmax };
            // Gather the operands the interpreter's `min_max` would reduce (its arg handling, EXACTLY):
            //   `min(v)`      one vector arg → its elements;
            //   `min(x)`      one scalar arg → `[x]`;
            //   `min(a,b,…)`  the `multi` branch → each arg MUST be a number, else `undef` (→ decline).
            // A `min()` (no args) or an empty vector → `undef` (→ decline). A bool anywhere → non-number → decline.
            let nums: Vec<Value> = if args.len() == 1 {
                match compile_expr(fb, &args[0].value, lower)? {
                    Lowered::Vec(elems) => elems
                        .iter()
                        .map(|e| e.num(fb))
                        .collect::<Result<Vec<_>, _>>()?,
                    Lowered::Num(x) => vec![x],
                    // `min(literal)` — one const scalar → `[c]`, materialized (the fold reduction runs at runtime).
                    Lowered::ConstNum(c) => vec![fb.ins().f64const(c)],
                    Lowered::Bool(_) | Lowered::ConstBool(_) => {
                        return Err(JitError::Unsupported("min/max of a boolean"));
                    }
                    // min/max reducing a DYNAMIC list is a runtime fold (a loop) — a future rung; decline.
                    Lowered::DynList(_) => {
                        return Err(JitError::Unsupported("min/max of a dynamic list"));
                    }
                    // min/max of a dynamic matrix isn't a numeric reduction (2c.3) — decline.
                    Lowered::DynMat { .. } => {
                        return Err(JitError::Unsupported("min/max of a dynamic matrix"));
                    }
                    // `min(undef)` is `undef` (2c.3) — no numeric reduction to emit; decline.
                    Lowered::ConstUndef => return Err(JitError::Unsupported("min/max of undef")),
                }
            } else {
                // 0 or ≥2 args: the `multi` branch — every arg is a plain number (a vector/bool → undef → decline).
                args.iter()
                    .map(|a| compile_expr(fb, &a.value, lower)?.num(fb))
                    .collect::<Result<Vec<_>, _>>()?
            };
            let Some((&head, rest)) = nums.split_first() else {
                return Err(JitError::Unsupported("min/max of nothing")); // undef
            };
            // Sequential `fold(head, min/max)` — the interpreter's exact reduction order + operand order.
            let mut acc = head;
            for &x in rest {
                let call = fb.ins().call(helper, &[acc, x]);
                acc = fb.inst_results(call)[0];
            }
            Ok(Lowered::Num(acc))
        }
        _ => Err(JitError::Unsupported("call")), // unreachable: the caller only routes the handled names here
    }
}

/// Elementwise `a op b` on two equal-length scalarized vectors (`+`/`−`) — each lane an `fadd`/`fsub` in index
/// order (order-independent per lane, so bit-identical to the interpreter's `zip_reuse`). RECURSES for a matrix
/// (a nested `Vec` pair of equal length → elementwise on the rows, 2c.2); a Vec-vs-scalar or unequal-length
/// element pair DECLINES (a shape mismatch the interpreter handles differently — safer to interpret).
fn vec_elementwise(
    fb: &mut FunctionBuilder,
    op: BinOp,
    a: &[Lowered],
    b: &[Lowered],
) -> Result<Vec<Lowered>, JitError> {
    a.iter()
        .zip(b)
        .map(|(x, y)| match (x, y) {
            (Lowered::Vec(xs), Lowered::Vec(ys)) if xs.len() == ys.len() => {
                Ok(Lowered::Vec(vec_elementwise(fb, op, xs, ys)?))
            }
            (Lowered::Vec(_), _) | (_, Lowered::Vec(_)) => {
                Err(JitError::Unsupported("elementwise shape mismatch"))
            }
            _ => {
                let (x, y) = (x.num(fb)?, y.num(fb)?);
                let v = match op {
                    BinOp::Add => fb.ins().fadd(x, y),
                    BinOp::Sub => fb.ins().fsub(x, y),
                    _ => return Err(JitError::Unsupported("non-elementwise vector op")),
                };
                Ok(Lowered::Num(v))
            }
        })
        .collect()
}

/// Scale a scalarized vector by a scalar: `v[i] * s` — matching `map_reuse(v, |e| e * s)` for both `v*s` and
/// `s*v` (float multiply is bit-commutative). RECURSES into a nested `Vec` (a matrix row) so `mat * scalar`
/// scales every leaf (2c.2).
fn vec_scale(fb: &mut FunctionBuilder, v: &[Lowered], s: Value) -> Result<Vec<Lowered>, JitError> {
    v.iter()
        .map(|e| match e {
            Lowered::Vec(row) => Ok(Lowered::Vec(vec_scale(fb, row, s)?)),
            _ => {
                let ev = e.num(fb)?; // materialize BEFORE `fb.ins()` — a nested `fb.ins(e.num(fb))` double-borrows `fb`
                Ok(Lowered::Num(fb.ins().fmul(ev, s)))
            }
        })
        .collect()
}

/// `v / s` (element / scalar, `vec_over_scalar`) or `s / v` (scalar / element) — matching `map_reuse(v, |e| e
/// / s)` / `|e| s / e`. Division isn't commutative, so the operand order is load-bearing. RECURSES into a nested
/// `Vec` (a matrix row) so `mat / scalar` (and `scalar / mat`) divides every leaf (2c.2).
fn vec_div(
    fb: &mut FunctionBuilder,
    v: &[Lowered],
    s: Value,
    vec_over_scalar: bool,
) -> Result<Vec<Lowered>, JitError> {
    v.iter()
        .map(|e| match e {
            Lowered::Vec(row) => Ok(Lowered::Vec(vec_div(fb, row, s, vec_over_scalar)?)),
            _ => {
                let e = e.num(fb)?;
                let q = if vec_over_scalar {
                    fb.ins().fdiv(e, s)
                } else {
                    fb.ins().fdiv(s, e)
                };
                Ok(Lowered::Num(q))
            }
        })
        .collect()
}

/// Dot product of two equal-length scalarized vectors, replicating [`ops::dot`]'s 4-lane reduction EXACTLY:
/// `lane[i % 4] += a[i] * b[i]` in index order, then `(l0 + l1) + (l2 + l3)`. That fixed lane structure is
/// what makes `vec * vec` bit-identical (a naive left-fold would diverge ≥4 elements).
fn vec_dot(fb: &mut FunctionBuilder, a: &[Lowered], b: &[Lowered]) -> Result<Value, JitError> {
    let zero = fb.ins().f64const(0.0);
    let mut lanes = [zero; 4];
    for i in 0..a.len() {
        let (ai, bi) = (a[i].num(fb)?, b[i].num(fb)?); // materialize BEFORE `fb.ins()` (nested double-borrow)
        let p = fb.ins().fmul(ai, bi);
        lanes[i % 4] = fb.ins().fadd(lanes[i % 4], p);
    }
    let l01 = fb.ins().fadd(lanes[0], lanes[1]);
    let l23 = fb.ins().fadd(lanes[2], lanes[3]);
    Ok(fb.ins().fadd(l01, l23))
}

/// True if every element of a scalarized vector is a SCALAR (a `Num`/`ConstNum`) — a FLAT vector, not a matrix
/// row-list. `[]` is vacuously flat (an empty operand's product is `undef` anyway → declined by the length guards).
fn is_flat_vec(v: &[Lowered]) -> bool {
    v.iter()
        .all(|e| matches!(e, Lowered::Num(_) | Lowered::ConstNum(_)))
}

/// True if `v` is a non-empty MATRIX — every element is itself a `Lowered::Vec` (a row). Mixed (some scalar, some
/// vector) is neither flat nor a matrix and DECLINES (the interpreter treats such a ragged list as `undef` here).
fn is_matrix(v: &[Lowered]) -> bool {
    !v.is_empty() && v.iter().all(|e| matches!(e, Lowered::Vec(_)))
}

/// OpenSCAD `*` on two scalarized vectors/matrices (2c.2b), mirroring `ops::apply_binary`'s Mul cases EXACTLY:
/// vec·vec (equal non-empty length) → the lane `dot` (a SCALAR); vec×mat, mat×vec, mat×mat → linear-algebra
/// products, each reducing to inner `dot`s (so `vec_dot` == `ops::dot` keeps every product bit-identical). All
/// shapes are compile-time known, so a non-rectangular / empty / dimension-mismatched operand (the interpreter's
/// `undef`) DECLINES here rather than miscompute.
fn vec_mat_product(
    fb: &mut FunctionBuilder,
    x: &[Lowered],
    y: &[Lowered],
) -> Result<Lowered, JitError> {
    match (is_flat_vec(x), is_flat_vec(y), is_matrix(x), is_matrix(y)) {
        // vec · vec → dot (both non-empty + equal length; else `undef` → decline). Matches the interpreter's
        // `(NumList, NumList) if !x.is_empty() && x.len()==y.len()` guard.
        (true, true, _, _) => {
            if x.is_empty() || x.len() != y.len() {
                return Err(JitError::Unsupported(
                    "dot of empty or mismatched-length vectors",
                ));
            }
            Ok(Lowered::Num(vec_dot(fb, x, y)?))
        }
        (true, false, _, true) => vec_times_mat(fb, x, y), // vec × mat
        (false, true, true, _) => mat_times_vec(fb, x, y), // mat × vec
        (false, false, true, true) => mat_times_mat(fb, x, y), // mat × mat
        // ragged / mixed / non-conforming → the interpreter's `undef`; the JIT can't hold it, so decline.
        _ => Err(JitError::Unsupported("unsupported vector/matrix product")),
    }
}

/// Matrix × vector (2c.2b): `out[i] = dot(mat[i], vec)` — `ops::mat_times_vec`. Every row must be a numeric
/// vector of `vec`'s length (rectangular), checked at COMPILE time; a mismatch / non-numeric row is `undef` →
/// DECLINE. `vec_dot(row, vec)` keeps the arg order (a=row, b=vec) the interpreter's `dot(r, vec)` uses.
fn mat_times_vec(
    fb: &mut FunctionBuilder,
    mat: &[Lowered],
    vec: &[Lowered],
) -> Result<Lowered, JitError> {
    let mut out = Vec::with_capacity(mat.len());
    for row in mat {
        let Lowered::Vec(r) = row else {
            return Err(JitError::Unsupported("non-rectangular matrix in mat×vec"));
        };
        if r.len() != vec.len() {
            return Err(JitError::Unsupported("mat×vec dimension mismatch"));
        }
        out.push(Lowered::Num(vec_dot(fb, r, vec)?));
    }
    Ok(Lowered::Vec(out))
}

/// Vector × matrix (2c.2b): `out[j] = dot(vec, col_j)`, `col_j = [mat[0][j], …, mat[k][j]]` — `ops::vec_times_mat`.
/// Requires `vec.len() == mat.len()` (row count) and a rectangular numeric matrix; else `undef` → DECLINE. The
/// column is gathered so the reduction reuses `vec_dot` (a=vec, b=col), matching the interpreter's `dot(vec, col)`.
fn vec_times_mat(
    fb: &mut FunctionBuilder,
    vec: &[Lowered],
    mat: &[Lowered],
) -> Result<Lowered, JitError> {
    if vec.len() != mat.len() {
        return Err(JitError::Unsupported("vec×mat dimension mismatch"));
    }
    let mut rows: Vec<&[Lowered]> = Vec::with_capacity(mat.len());
    for row in mat {
        let Lowered::Vec(r) = row else {
            return Err(JitError::Unsupported("non-rectangular matrix in vec×mat"));
        };
        rows.push(r);
    }
    let cols = rows.first().map_or(0, |r| r.len());
    if cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return Err(JitError::Unsupported(
            "empty or non-rectangular matrix in vec×mat",
        ));
    }
    let mut out = Vec::with_capacity(cols);
    for j in 0..cols {
        // Column j = [rows[0][j], rows[1][j], …] — one leaf per row, so `col.len() == vec.len()`.
        let col: Vec<Lowered> = rows.iter().map(|r| r[j].clone()).collect();
        out.push(Lowered::Num(vec_dot(fb, vec, &col)?));
    }
    Ok(Lowered::Vec(out))
}

/// Matrix × matrix (2c.2b): each LEFT row times the right matrix — `ops::mat_times_mat` folds it exactly this
/// way (`vec_times_mat(a_row, b)`), so the result is a list of vectors (a matrix). A non-numeric / non-conforming
/// row is `undef` → DECLINE (propagated from `vec_times_mat`).
fn mat_times_mat(
    fb: &mut FunctionBuilder,
    a: &[Lowered],
    b: &[Lowered],
) -> Result<Lowered, JitError> {
    let mut out = Vec::with_capacity(a.len());
    for row in a {
        let Lowered::Vec(r) = row else {
            return Err(JitError::Unsupported(
                "non-rectangular left matrix in mat×mat",
            ));
        };
        out.push(vec_times_mat(fb, r, b)?);
    }
    Ok(Lowered::Vec(out))
}

/// Negate a `Lowered` — a number `fneg`, a vector elementwise (recursing so a nested vector negates too),
/// matching `ops::apply_unary`. A bool DECLINES (`-true` isn't in the subset).
fn neg_lowered(fb: &mut FunctionBuilder, v: Lowered) -> Result<Lowered, JitError> {
    match v {
        Lowered::Num(n) => Ok(Lowered::Num(fb.ins().fneg(n))),
        // A const number negates AT COMPILE TIME — Rust `-c` flips the sign bit exactly like Cranelift `fneg`
        // (NaN and ±0 included) — staying a `ConstNum` so a wrapping comparison/ternary can still fold.
        Lowered::ConstNum(c) => Ok(Lowered::ConstNum(-c)),
        Lowered::Vec(elems) => {
            let out: Result<Vec<Lowered>, JitError> =
                elems.into_iter().map(|e| neg_lowered(fb, e)).collect();
            Ok(Lowered::Vec(out?))
        }
        Lowered::Bool(_) | Lowered::ConstBool(_) => Err(JitError::Unsupported("negate a boolean")),
        // Elementwise-negating a DYNAMIC list is a runtime map (a loop) — a future rung (2b.4); decline.
        Lowered::DynList(_) => Err(JitError::Unsupported("negate a dynamic list")),
        // Negating a dynamic matrix is a runtime map too (2c.3) — decline.
        Lowered::DynMat { .. } => Err(JitError::Unsupported("negate a dynamic matrix")),
        // `-undef` is `undef` (`ops::apply_unary`) — stays a `ConstUndef` so a wrapping fold still sees it (2c.3).
        Lowered::ConstUndef => Ok(Lowered::ConstUndef),
    }
}

/// A `select` per the condition, SHAPE-matched: `Num`/`Bool` pick one IR value; two same-length vectors
/// select elementwise (recursing); any other pairing (differing kinds or vector lengths) DECLINES, since a
/// scalarized value can't hold a runtime-chosen shape.
fn select_lowered(
    fb: &mut FunctionBuilder,
    c: Value,
    t: Lowered,
    e: Lowered,
) -> Result<Lowered, JitError> {
    // The cond wasn't compile-time-known (else the ternary pruned), so the pick is a RUNTIME `select` — a const
    // branch has to materialize to an IR value. Do it up front so the `Num`/`Vec` pairings below don't grow a
    // `ConstNum` case (a `ConstNum ? : ConstNum` becomes `Num`/`Num` and selects normally).
    let (t, e) = (t.materialize_num(fb), e.materialize_num(fb));
    match (t, e) {
        (Lowered::Num(t), Lowered::Num(e)) => Ok(Lowered::Num(fb.ins().select(c, t, e))),
        (Lowered::Vec(t), Lowered::Vec(e)) if t.len() == e.len() => {
            let out: Result<Vec<Lowered>, JitError> = t
                .into_iter()
                .zip(e)
                .map(|(t, e)| select_lowered(fb, c, t, e))
                .collect();
            Ok(Lowered::Vec(out?))
        }
        // Two BOOLEAN branches (either `Bool` or a const-folded `ConstBool`, P.1.6 rung-D 2b.4) — materialize
        // both to `i8` and `select`. The runtime cond wasn't compile-time-known (else the ternary pruned), so a
        // runtime pick is correct.
        (
            t @ (Lowered::Bool(_) | Lowered::ConstBool(_)),
            e @ (Lowered::Bool(_) | Lowered::ConstBool(_)),
        ) => {
            let (tv, ev) = (
                bool_ir(fb, &t).expect("t is a bool"),
                bool_ir(fb, &e).expect("e is a bool"),
            );
            Ok(Lowered::Bool(fb.ins().select(c, tv, ev)))
        }
        _ => Err(JitError::Unsupported("ternary branches differ in type")),
    }
}

/// A short, stable name for an out-of-subset expression node — the DECLINE reason surfaced in the coverage
/// report. Grouped so the histogram reads as an absorption ceiling: `call` (the big one — builtin + user-fn
/// calls, P.1.4b), `index`/`member`, `comprehension`, `let`, and the non-numeric literals. Exhaustive so a
/// new `ExprKind` names itself here rather than hiding in a wildcard bucket.
fn kind_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Call { .. } => "call",
        ExprKind::Range { .. } => "range",
        ExprKind::Let { .. } => "let-binding",
        ExprKind::FunctionLiteral { .. } => "function-literal",
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => "comprehension",
        ExprKind::Assert { .. } => "assert",
        ExprKind::Echo { .. } => "echo",
        ExprKind::Str(_) => "string-literal",
        ExprKind::Undef => "undef-literal",
        // The handled kinds don't reach here; name them defensively rather than wildcard. `Vector`/`Index`
        // (rung A) + `Member` (rung B) are handled — they decline with a SPECIFIC reason inside their arm,
        // never via this path.
        ExprKind::Num(_)
        | ExprKind::Bool(_)
        | ExprKind::Ident(_)
        | ExprKind::Unary { .. }
        | ExprKind::Binary { .. }
        | ExprKind::Ternary { .. }
        | ExprKind::Vector(_)
        | ExprKind::Index { .. }
        | ExprKind::Member { .. } => "unhandled-in-subset",
    }
}
