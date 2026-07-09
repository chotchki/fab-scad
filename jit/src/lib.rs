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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ret {
    Num,
    Bool,
    Vec(usize),
    /// A DYNAMIC-length numeric list (P.1.6 rung-D piece 2): the compiled function materialized it into the
    /// `sink` (`*mut Vec<f64>`) via a loop; the `f64` return is a dummy and the dispatch reads the sink into a
    /// `Value::NumList`. Length isn't known at compile time (unlike `Vec(n)`), so there's no descriptor.
    DynVec,
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
    Vec(usize),
}

impl ArgShape {
    /// How many `f64` slots this arg occupies in the flat call buffer.
    fn size(&self) -> usize {
        match self {
            ArgShape::Scalar => 1,
            ArgShape::Vec(n) => *n,
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

/// Derive each arg's [`ArgShape`] and flatten its `f64`s into `flat` (cleared first), or `None` if any arg
/// isn't scalarizable: a nested/mixed list (a matrix), a string / undef / range / function value, or a vector
/// longer than [`MAX_VEC_ARG`]. `Num` → one element + `Scalar`; a `NumList` (or an all-`Num` `List`, which a
/// freshly-built `[a,b]` can surface as before the value model normalizes it) → its elements + `Vec(len)`.
/// `flat` is the registry's REUSED scratch, so the hot path pays no per-call flatten allocation.
fn shape_and_flatten(args: &[ScadValue], flat: &mut Vec<f64>) -> Option<ShapeSig> {
    flat.clear();
    let mut sig = ShapeSig::with_capacity(args.len());
    for a in args {
        match a {
            ScadValue::Num(n) => {
                flat.push(*n);
                sig.push(ArgShape::Scalar);
            }
            ScadValue::NumList(xs) => {
                if xs.len() > MAX_VEC_ARG {
                    return None;
                }
                flat.extend_from_slice(xs);
                sig.push(ArgShape::Vec(xs.len()));
            }
            ScadValue::List(items) => {
                if items.len() > MAX_VEC_ARG {
                    return None;
                }
                let start = flat.len();
                for it in items.iter() {
                    match it {
                        ScadValue::Num(n) => flat.push(*n),
                        // A nested or non-numeric element → not scalarizable; roll back this arg's f64s.
                        _ => {
                            flat.truncate(start);
                            return None;
                        }
                    }
                }
                sig.push(ArgShape::Vec(items.len()));
            }
            // A string / undef / range / function value can't be a numeric JIT arg → decline the whole call.
            _ => return None,
        }
    }
    Some(sig)
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

/// Push one value onto the JIT's dynamic-list SINK (P.1.6 rung-D piece 2) — the growable `Vec<f64>` a JIT'd
/// comprehension materializes into, which the dispatch reads back as a `NumList`. THE crate's THIRD unsafe seam
/// (after the fn-ptr call + [`jit_rand_next`]), confined + documented here.
///
/// # Safety
/// `sink` MUST be a live `*mut Vec<f64>` with EXCLUSIVE access for the call's duration — the dispatch owns it
/// (`call_numeric` allocates it) and native code is the sole single-threaded accessor while the loop runs. A
/// non-comprehension body never calls this, so an unused sink pointer is never dereferenced.
extern "C" fn jit_vec_push(sink: *mut Vec<f64>, v: f64) {
    // SAFETY: per the contract above, `sink` is a live, exclusively-accessible `Vec<f64>` for this call.
    let sink = unsafe { &mut *sink };
    sink.push(v);
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
/// `CompiledFn` is only valid for that owner's lifetime. `Copy` (a pointer + two words), so the registry can
/// lift one out of its cache and call it AFTER releasing the `RefCell` borrow — the code stays mapped as long
/// as the module (in the registry) is alive.
#[derive(Clone, Copy)]
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
    /// `*mut RandStream` for the call's duration (see [`jit_rand_next`]); likewise `sink` must be either null
    /// (for a non-comprehension body) or a live, exclusively-accessible `*mut Vec<f64>` (see [`jit_vec_push`]).
    #[must_use]
    pub unsafe fn call(
        &self,
        params: &[f64],
        out: &mut [f64],
        rand: *mut core::ffi::c_void,
        sink: *mut core::ffi::c_void,
    ) -> Option<f64> {
        assert_eq!(
            params.len(),
            self.arity,
            "CompiledFn::call arity mismatch: got {}, expected {}",
            params.len(),
            self.arity
        );
        if let Ret::Vec(n) = self.ret_ty {
            assert!(out.len() >= n, "CompiledFn::call out buffer too small: {} < {n}", out.len());
        }
        // THE unsafe seam of the whole crate. SAFETY: `code` is a finalized Cranelift function of signature
        // `extern "C" fn(*const f64, *mut u8, *mut f64, *mut RandStream, *mut Vec<f64>) -> f64` (built in
        // `define_one`); the owning module keeps it mapped as long as `self` is reachable. It READS `arity`
        // f64s from the first pointer (`params` has exactly that many, asserted), WRITES one byte through the
        // second (`raised`), WRITES `n` f64s through the third only for a `Vec(n)` return (`out`, asserted ≥ n),
        // passes the fourth to `jit_rand_next` only on a seedless-`rands` body, and pushes to the fifth
        // (`*mut Vec<f64>`) only on a `DynVec` comprehension body — the caller's contract per the # Safety note.
        // No unwinding crosses the boundary.
        let f: unsafe extern "C" fn(
            *const f64,
            *mut u8,
            *mut f64,
            *mut core::ffi::c_void,
            *mut core::ffi::c_void,
        ) -> f64 = unsafe { std::mem::transmute(self.code) };
        let mut raised: u8 = 0;
        let result = unsafe { f(params.as_ptr(), &raw mut raised, out.as_mut_ptr(), rand, sink) };
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
        // body never dereferences the null rand/sink pointers nor writes the out-buffer.
        unsafe { self.inner.call(params, &mut [0.0], core::ptr::null_mut(), core::ptr::null_mut()) }
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
            .map(|(n, p, b)| (n.to_string(), OwnedDef { params: p.to_vec(), body: b.clone() }))
            .collect();
        let owned_globals: BTreeMap<String, Expr> =
            consts.into_iter().map(|(n, v)| (n.to_string(), v.clone())).collect();

        let mut module = new_module()?;
        let helpers = declare_helpers(&mut module)?;

        let mut cache: BTreeMap<String, BTreeMap<ShapeSig, Option<CompiledFn>>> = BTreeMap::new();
        let mut declined: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut scalar_compiled = 0usize;
        let mut next_symbol = 0usize;
        {
            // Borrow-maps over the OWNED AST for the compiler (every function visible to every other, so a
            // caller can inline any callee incl. a forward reference). Built once for the whole build pass.
            let fn_defs: FnDefs =
                owned_defs.iter().map(|(n, d)| (n.as_str(), (d.params.as_slice(), &d.body))).collect();
            let globals: Globals = owned_globals.iter().map(|(n, v)| (n.as_str(), v)).collect();
            // Declare + define each function's all-scalar shape, remembering its FuncId to resolve the code
            // pointer AFTER the single finalize. `Vec(name, FuncId, flatlen, ret_ty)`.
            let mut pending: Vec<(&str, FuncId, usize, Ret)> = Vec::new();
            for (name, d) in &owned_defs {
                let params: Vec<(&str, ArgShape)> =
                    d.params.iter().map(|p| (p.name.as_ref(), ArgShape::Scalar)).collect();
                let symbol = format!("scad_jit_{next_symbol}");
                next_symbol += 1;
                match define_one(&mut module, &symbol, &params, &d.body, &fn_defs, &globals, &helpers) {
                    // all-scalar flatlen == parameter count.
                    Ok((func_id, ret_ty)) => pending.push((name, func_id, d.params.len(), ret_ty)),
                    Err(JitError::Unsupported(reason)) => {
                        declined.insert(name.clone(), reason);
                    }
                    Err(e) => return Err(e), // a real codegen failure — surface it
                }
            }
            module.finalize_definitions().map_err(|e| JitError::Cranelift(e.to_string()))?;
            for (name, func_id, flatlen, ret_ty) in pending {
                let code = module.get_finalized_function(func_id);
                let sig = vec![ArgShape::Scalar; flatlen];
                cache
                    .entry(name.to_string())
                    .or_default()
                    .insert(sig, Some(CompiledFn { code, arity: flatlen, ret_ty }));
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
        })
    }

    /// The specialization for `(name, sig)` — from the cache, or COMPILED on demand and memoized (P.1.6 rung
    /// B). `None` if `name` is unknown OR this arg shape DECLINES (the body leaves the numeric subset for these
    /// shapes); a decline memoizes as `None`, so a shape compiles at most once. The returned [`CompiledFn`] is
    /// `Copy`, lifted out from behind the cache borrow — the code stays mapped for the registry's life.
    fn get_or_compile(&self, name: &str, sig: &ShapeSig) -> Option<CompiledFn> {
        // Fast path: already compiled, or a memoized decline.
        if let Some(entry) = self.cache.borrow().get(name).and_then(|m| m.get(sig)) {
            return *entry;
        }
        // Unknown function → nothing to compile (and nothing to memoize).
        let def = self.defs.get(name)?;
        // Compile this shape now. Build the param-shape list + the borrow-maps over the owned AST (only paid
        // on a cache MISS — a rare event, once per never-before-seen shape).
        let params: Vec<(&str, ArgShape)> = def
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_ref(), sig.get(i).cloned().unwrap_or(ArgShape::Scalar)))
            .collect();
        let flatlen: usize = sig.iter().map(ArgShape::size).sum();
        let symbol = {
            let n = self.next_symbol.get();
            self.next_symbol.set(n + 1);
            format!("scad_jit_{n}")
        };
        let fn_defs: FnDefs =
            self.defs.iter().map(|(n, d)| (n.as_str(), (d.params.as_slice(), &d.body))).collect();
        let globals: Globals = self.globals.iter().map(|(n, v)| (n.as_str(), v)).collect();
        let compiled = {
            let mut module = self.module.borrow_mut();
            match define_one(&mut module, &symbol, &params, &def.body, &fn_defs, &globals, &self.helpers) {
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
        self.cache.borrow_mut().entry(name.to_string()).or_default().insert(sig.clone(), compiled);
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
        cache.get(name).and_then(|m| m.get(&sig)).copied().flatten()
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
        self.defs.keys().map(String::as_str).filter(|n| self.get(n).is_some())
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
        let mut scratch = self.scratch.borrow_mut();
        // Derive the arg shape + flatten the f64s into scratch. `None` → a non-scalarizable arg (nested list,
        // string, over-long vector) → interpret. `get_or_compile` doesn't touch `scratch`, so holding this
        // borrow across it is safe (a different `RefCell`), and the compiled fn then reads `scratch` directly.
        let sig = shape_and_flatten(args, &mut scratch)?;
        let compiled = self.get_or_compile(name, &sig)?;
        // RE-TAG the untyped native return by the specialization's static shape (P.1.4e + rung C): a `Num` IS
        // the `f64`; a `Bool` predicate yields `0.0`/`1.0` → `Value::Bool`; a `Vec(n)` wrote its `n` elements to
        // a sink buffer → `Value::NumList`. `None` from `call` = the inline assert raised → interpret. The
        // scalar path keeps its stack dummy out — no heap allocation. `rand` is the eval's woven stream (P.1.6
        // rung-D piece 1) — a seedless-`rands` body advances it; the dispatch guarantees it's live + exclusive.
        // SAFETY: `rand` came from the dispatch's `Ctx::rand_stream` cell pointer, valid + single-threaded.
        let null = core::ptr::null_mut();
        match compiled.ret_ty {
            Ret::Num => Some(JitOutcome::Num(unsafe { compiled.call(&scratch, &mut [0.0], rand, null) }?)),
            Ret::Bool => {
                Some(JitOutcome::Bool(unsafe { compiled.call(&scratch, &mut [0.0], rand, null) }? != 0.0))
            }
            Ret::Vec(n) => {
                let mut out = vec![0.0; n];
                // the f64 return is a dummy; the elements are in `out`.
                unsafe { compiled.call(&scratch, &mut out, rand, null) }?;
                Some(JitOutcome::Vec(out))
            }
            Ret::DynVec => {
                // A comprehension (P.1.6 rung-D piece 2) pushes into this growable sink; the f64 return is a
                // dummy. `None` here = the BUDGET bail (or an inline assert) flagged `raised` → interpret.
                let mut sink: Vec<f64> = Vec::new();
                let sink_ptr = std::ptr::from_mut(&mut sink).cast::<core::ffi::c_void>();
                unsafe { compiled.call(&scratch, &mut [0.0], rand, sink_ptr) }?;
                Some(JitOutcome::Vec(sink))
            }
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
    fn compile(&self, defs: &[JitDef<'_>], consts: &[JitConst<'_>]) -> Option<Box<dyn NumericJit>> {
        let enabled = std::env::var_os("FAB_JIT").as_deref() == Some(std::ffi::OsStr::new("1"));
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
    eprintln!("\n[jit-explain] === numeric-JIT coverage === {compiled}/{total} functions compiled ({pct:.1}%)");
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
    eprintln!("[jit-explain]   {declined_total} declined — first-blocker histogram (the absorption ceiling):");
    for (reason, count) in rows {
        let share = 100.0 * *count as f64 / declined_total.max(1) as f64;
        eprintln!("[jit-explain]     {count:>5}  {share:5.1}%  {reason}");
    }
    // This is the ALL-SCALAR view: a function blocked by `index of a non-vector` / `member access on a
    // non-vector` here still gains a specialization when CALLED with a vector arg (P.1.6 rung B, compiled on
    // demand and not counted above). The scalar histogram is the conservative floor.
    eprintln!("[jit-explain]   (vector-arg specializations compile on demand at runtime — not in the counts above)");
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
    // The standalone API has NO RandStream to weave (P.1.6 rung-D piece 1 is a registry feature), so a
    // seedless-`rands` body must DECLINE here rather than have `JitFn::call`'s null stream pointer dereferenced.
    if uses_rands(body) {
        return Err(JitError::Unsupported("rands (standalone API has no RandStream; use JitRegistry)"));
    }
    // The standalone differential passes plain `f64` args, so every parameter is a SCALAR shape.
    let params: Vec<(&str, ArgShape)> = param_names.iter().map(|&n| (n, ArgShape::Scalar)).collect();
    let (func_id, ret_ty) =
        define_one(&mut module, "scad_jit_fn", &params, body, &no_defs, &no_globals, &helpers)?;
    // The standalone API is f64-only (the fast==JIT differential compares raw f64s); a bool- OR vector-returning
    // body is the registry path's job (it carries the tag + the sink buffer), so DECLINE it here.
    if !matches!(ret_ty, Ret::Num) {
        return Err(JitError::Unsupported("non-numeric return (standalone API is f64-only; use JitRegistry)"));
    }
    module
        .finalize_definitions()
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    let code = module.get_finalized_function(func_id);
    Ok(JitFn {
        _module: module,
        inner: CompiledFn { code, arity: param_names.len(), ret_ty },
    })
}

/// A fresh JIT module with our two math helper symbols registered. `opt_level=speed` is safe for
/// determinism: Cranelift never CONTRACTS fmul+fadd into an fma (that's an LLVM fast-math behavior); it
/// emits the instructions we ask for, in order.
fn new_module() -> Result<JITModule, JitError> {
    let mut flags = settings::builder();
    let set = |flags: &mut settings::Builder, k, v| {
        flags.set(k, v).map_err(|e| JitError::Cranelift(e.to_string()))
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
    jb.symbol("jit_vec_push", jit_vec_push as *const u8);
    jb.symbol("jit_range_len", jit_range_len as *const u8);
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
    /// `jit_vec_push(*mut Vec<f64>, f64)` — push one element onto the comprehension sink (P.1.6 rung-D piece 2).
    vec_push: FuncId,
    /// `jit_range_len(f64, f64, f64) -> i64` — a range's element count, the loop bound (P.1.6 rung-D piece 2).
    range_len: FuncId,
}

fn declare_helpers(module: &mut JITModule) -> Result<Helpers, JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    // `(f64, f64) -> f64` for fmod/powf/fmin/fmax.
    let mut op_sig = module.make_signature();
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.returns.push(AbiParam::new(types::F64));
    let fmod = module.declare_function("jit_fmod", Linkage::Import, &op_sig).map_err(cl)?;
    let powf = module.declare_function("jit_powf", Linkage::Import, &op_sig).map_err(cl)?;
    let fmin = module.declare_function("jit_fmin", Linkage::Import, &op_sig).map_err(cl)?;
    let fmax = module.declare_function("jit_fmax", Linkage::Import, &op_sig).map_err(cl)?;
    // `(i32 id, f64, f64) -> f64` for the math dispatcher.
    let mut math_sig = module.make_signature();
    math_sig.params.push(AbiParam::new(types::I32));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let math = module.declare_function("jit_math_call", Linkage::Import, &math_sig).map_err(cl)?;
    // `(*mut RandStream, f64, f64) -> f64` — the stream pointer rides the target pointer type.
    let mut rand_sig = module.make_signature();
    rand_sig.params.push(AbiParam::new(module.target_config().pointer_type()));
    rand_sig.params.push(AbiParam::new(types::F64));
    rand_sig.params.push(AbiParam::new(types::F64));
    rand_sig.returns.push(AbiParam::new(types::F64));
    let rand_next = module.declare_function("jit_rand_next", Linkage::Import, &rand_sig).map_err(cl)?;
    // `(*mut Vec<f64>, f64)` → nothing — the comprehension sink push.
    let mut push_sig = module.make_signature();
    push_sig.params.push(AbiParam::new(module.target_config().pointer_type()));
    push_sig.params.push(AbiParam::new(types::F64));
    let vec_push = module.declare_function("jit_vec_push", Linkage::Import, &push_sig).map_err(cl)?;
    // `(f64, f64, f64) -> i64` — the range length / loop bound.
    let mut rlen_sig = module.make_signature();
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.params.push(AbiParam::new(types::F64));
    rlen_sig.returns.push(AbiParam::new(types::I64));
    let range_len = module.declare_function("jit_range_len", Linkage::Import, &rlen_sig).map_err(cl)?;
    Ok(Helpers { fmod, powf, fmin, fmax, math, rand_next, vec_push, range_len })
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
        let sink_ptr = fb.block_params(block)[4];

        let fmod_ref = module.declare_func_in_func(helpers.fmod, fb.func);
        let powf_ref = module.declare_func_in_func(helpers.powf, fb.func);
        let fmin_ref = module.declare_func_in_func(helpers.fmin, fb.func);
        let fmax_ref = module.declare_func_in_func(helpers.fmax, fb.func);
        let math_ref = module.declare_func_in_func(helpers.math, fb.func);
        let rand_next_ref = module.declare_func_in_func(helpers.rand_next, fb.func);
        let vec_push_ref = module.declare_func_in_func(helpers.vec_push, fb.func);
        let range_len_ref = module.declare_func_in_func(helpers.range_len, fb.func);
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
                ArgShape::Vec(n) => {
                    let mut elems = Vec::with_capacity(*n);
                    for k in 0..*n {
                        let byte_off = i32::try_from((off + k) * 8)
                            .map_err(|_| JitError::Unsupported("param offset overflow"))?;
                        let v =
                            fb.ins().load(types::F64, MemFlagsData::trusted(), params_ptr, byte_off);
                        elems.push(Lowered::Num(v));
                    }
                    locals.insert(name, Lowered::Vec(elems));
                    off += n;
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
            rand_ptr,
            sink_ptr,
            fmod: fmod_ref,
            powf: powf_ref,
            fmin: fmin_ref,
            fmax: fmax_ref,
            math: math_ref,
            rand_next: rand_next_ref,
            vec_push: vec_push_ref,
            range_len: range_len_ref,
        };

        // IR is built BEFORE declare/define — an Unsupported node returns here with the module untouched.
        // A body that IS a COMPREHENSION (P.1.6 rung-D piece 2) materializes into the `sink` via a LOOP — it
        // emits its OWN returns (a budget-bail + the loop exit), so no trailing return here. Otherwise: a
        // NUMERIC body returns its f64 directly; a BOOL body (an i8 0/1) is returned as 0.0/1.0 and tagged so
        // the dispatch wraps `Value::Bool` (P.1.4e); a fixed-shape VECTOR body (rung C) WRITES its elements to
        // the `out` sink and returns a dummy, read back into a `NumList`. A NESTED vector (matrix) declines.
        // `[for (…) …]` parses as a `Vector` holding ONE `LcFor` element (the comprehension IS the whole list).
        // A mixed vector (`[a, for(…) b]`) or multiple comprehensions decline here (rung 2b.N).
        let comprehension = match &body.kind {
            ExprKind::Vector(elems) => match elems.as_slice() {
                [single] => match &single.kind {
                    ExprKind::LcFor { bindings, body: lc_body } => Some((bindings, lc_body)),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        let ty = if let Some((bindings, lc_body)) = comprehension {
            compile_comprehension(&mut fb, bindings, lc_body, &lower)?
        } else {
            let (ret, ty) = match compile_expr(&mut fb, body, &lower)? {
                Lowered::Num(v) => (v, Ret::Num),
                Lowered::Bool(v) => {
                    let one = fb.ins().f64const(1.0);
                    let zero = fb.ins().f64const(0.0);
                    (fb.ins().select(v, one, zero), Ret::Bool)
                }
                Lowered::Vec(elems) => {
                    for (i, e) in elems.iter().enumerate() {
                        let x = e.num()?; // a nested element (matrix row) declines here — flat vectors only
                        let byte_off = i32::try_from(i * 8)
                            .map_err(|_| JitError::Unsupported("return offset overflow"))?;
                        fb.ins().store(MemFlagsData::trusted(), x, out_ptr, byte_off);
                    }
                    (fb.ins().f64const(0.0), Ret::Vec(elems.len())) // the f64 return is a dummy for a vec
                }
            };
            fb.ins().return_(&[ret]);
            ty
        };
        fb.finalize();
        ret_ty = ty;
    }

    let func_id = module.declare_function(symbol, Linkage::Export, &ctx.func.signature).map_err(cl)?;
    module.define_function(func_id, &mut ctx).map_err(cl)?;
    module.clear_context(&mut ctx);
    Ok((func_id, ret_ty))
}

/// Compile a top-level comprehension body `[for (var = range) scalar_body]` to a LOOP that materializes each
/// element into the `sink` (P.1.6 rung-D piece 2, rung 2b.1 — the first control flow in fab-jit). Emits the
/// function's OWN returns (a budget-bail path + the loop-exit path), so `define_one` adds none. Returns
/// [`Ret::DynVec`]; the dispatch reads the sink into a `Value::NumList`.
///
/// Bit-identity vs `eval::lc_for` + `RangeIter`: the loop bound is `jit_range_len` (the interpreter's EXACT
/// `range_len` — the `step==0`/non-finite/direction → 0 logic + the `RANGE_MAX` cap); element `i`'s value is
/// `start + (i as f64)*step` (`fcvt_from_sint` is exact for `i < RANGE_MAX < 2^53`, same operand order); the
/// loop runs `i = 0..len` pushing in index order, matching `lc_for`'s `out.extend`. A count over
/// [`COMPREHENSION_BUDGET`] BAILS to the interpreter (sets `raised` → the dispatch's `None`), which is always
/// safe (the interpreter computes the same list). v1 (2b.1): a SINGLE binding over a RANGE, a SCALAR body — a
/// list iterable / multi-binding / non-scalar body / filter declines (rung 2b.2+).
fn compile_comprehension(
    fb: &mut FunctionBuilder,
    bindings: &[fab_lang::Arg],
    lc_body: &Expr,
    lower: &Lower,
) -> Result<Ret, JitError> {
    // 2b.1: exactly one binding, over a RANGE literal, with a name.
    let [binding] = bindings else {
        return Err(JitError::Unsupported("multi-binding comprehension")); // rung 2b.2+
    };
    let name = binding.name.as_deref().ok_or(JitError::Unsupported("comprehension binding without a name"))?;
    let ExprKind::Range { start, step, end } = &binding.value.kind else {
        return Err(JitError::Unsupported("comprehension over a non-range")); // list iterable → 2b.2
    };
    // Range bounds — compiled ONCE, in source order (start, step, end), before the loop, matching the
    // interpreter evaluating the range value before iterating. `[a:b]` defaults step to 1.0.
    let start_v = compile_expr(fb, start, lower)?.num()?;
    let step_v = match step {
        Some(s) => compile_expr(fb, s, lower)?.num()?,
        None => fb.ins().f64const(1.0),
    };
    let end_v = compile_expr(fb, end, lower)?.num()?;

    // len = jit_range_len(start, step, end) — the interpreter's exact count (capped at RANGE_MAX).
    let len = {
        let call = fb.ins().call(lower.range_len, &[start_v, step_v, end_v]);
        fb.inst_results(call)[0]
    };
    // BUDGET bail: an over-budget count flags `raised` (like a failed assert) and returns → the dispatch's
    // `None` → the interpreter runs the whole body. Checked BEFORE the loop, so no elements / draws happen.
    let budget = fb.ins().iconst(types::I64, COMPREHENSION_BUDGET);
    let over = fb.ins().icmp(IntCC::SignedGreaterThan, len, budget);
    let bail = fb.create_block();
    let setup = fb.create_block();
    fb.ins().brif(over, bail, &[], setup, &[]);
    fb.seal_block(bail);
    fb.seal_block(setup);
    fb.switch_to_block(bail);
    let one_flag = fb.ins().iconst(types::I8, 1);
    fb.ins().store(MemFlagsData::trusted(), one_flag, lower.raised_ptr, 0);
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

    // body: value = start + (i as f64)*step; bind the loop var; compile the scalar element; push it.
    fb.switch_to_block(body_block);
    let i_f = fb.ins().fcvt_from_sint(types::F64, i); // exact for 0 <= i < RANGE_MAX < 2^53
    let scaled = fb.ins().fmul(i_f, step_v);
    let value = fb.ins().fadd(start_v, scaled); // `start + (i as f64)*step`, the interpreter's operand order
    let mut locals = lower.locals.clone();
    locals.insert(name, Lowered::Num(value));
    let scoped = Lower { locals: &locals, ..*lower };
    let elem = compile_expr(fb, lc_body, &scoped)?.num()?; // 2b.1: a SCALAR element (a vector/matrix declines)
    fb.ins().call(lower.vec_push, &[lower.sink_ptr, elem]);
    let one = fb.ins().iconst(types::I64, 1);
    let i_next = fb.ins().iadd(i, one);
    fb.ins().jump(header, &[BlockArg::Value(i_next)]);
    fb.seal_block(header); // both predecessors (setup + body) are now declared

    // exit: the sink holds the result; return a dummy (the DynVec descriptor is on `Ret`).
    fb.switch_to_block(exit);
    let exit_ret = fb.ins().f64const(0.0);
    fb.ins().return_(&[exit_ret]);
    Ok(Ret::DynVec)
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
}

impl Lowered {
    /// The single IR value of a `Num`, else DECLINE — an arithmetic/comparison operand, a math-builtin arg, or
    /// a scalar function return that turned out to be a bool or a vector isn't in the subset.
    fn num(&self) -> Result<Value, JitError> {
        match self {
            Lowered::Num(v) => Ok(*v),
            Lowered::Bool(_) => Err(JitError::Unsupported("a boolean where a number is required")),
            Lowered::Vec(_) => Err(JitError::Unsupported("a vector where a number is required")),
        }
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
        Lowered::Num(n) => {
            let zero = fb.ins().f64const(0.0);
            Ok(fb.ins().fcmp(FloatCC::NotEqual, *n, zero))
        }
        Lowered::Vec(_) => Err(JitError::Unsupported("a vector as a truth condition")),
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
    /// The woven `RandStream` pointer (the 4th ABI param) — a JIT'd seedless `rands()` passes it to
    /// `jit_rand_next`. Untouched by a body that never draws (P.1.6 rung-D piece 1).
    rand_ptr: Value,
    /// The dynamic-list SINK pointer (the 5th ABI param, `*mut Vec<f64>`) — a JIT'd comprehension pushes its
    /// materialized elements here via `jit_vec_push` (P.1.6 rung-D piece 2). Untouched by a non-comprehension body.
    sink_ptr: Value,
    fmod: FuncRef,
    powf: FuncRef,
    fmin: FuncRef,
    fmax: FuncRef,
    math: FuncRef,
    rand_next: FuncRef,
    vec_push: FuncRef,
    range_len: FuncRef,
}

#[allow(
    clippy::too_many_lines,
    reason = "the per-ExprKind lowering — one arm per node kind; splitting scatters the shared builder"
)]
fn compile_expr(fb: &mut FunctionBuilder, expr: &Expr, lower: &Lower) -> Result<Lowered, JitError> {
    match &expr.kind {
        ExprKind::Num(n) => Ok(Lowered::Num(fb.ins().f64const(*n))),
        // A bool literal (`true`/`false`) → the `i8` 0/1 a `Bool` is (P.1.4e). Lets a predicate body like
        // `cond ? true : false` compile — the ternary's branches now agree as `Bool`.
        ExprKind::Bool(b) => Ok(Lowered::Bool(fb.ins().iconst(types::I8, i64::from(*b)))),
        ExprKind::Ident(name) => {
            // A `let`-bound local (or inlined-call param) shadows a parameter — check the env first. It may be
            // a scalarized vector (a `let(v = [a,b,c])`), so clone the whole `Lowered`.
            if let Some(v) = lower.locals.get(name.as_str()) {
                return Ok(v.clone());
            }
            // Then a parameter (read from the params pointer). Shadows a like-named global.
            if let Some(&i) = lower.index.get(name.as_str()) {
                let offset = i32::try_from(i * 8)
                    .map_err(|_| JitError::Unsupported("param offset overflow"))?;
                let v = fb.ins().load(types::F64, MemFlagsData::trusted(), lower.params_ptr, offset);
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
                UnOp::Pos => Ok(Lowered::Num(v.num()?)),
                // `!x` = `!is_truthy(x)` → a Bool. `(truthy == 0)` inverts the 0/1 flag.
                UnOp::Not => {
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
                let (a, b) = (a.num()?, b.num()?);
                return Ok(Lowered::Bool(fb.ins().fcmp(cc, a, b)));
            }
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
                (BinOp::Add | BinOp::Sub, Lowered::Vec(x), Lowered::Vec(y)) if x.len() == y.len() => {
                    Ok(Lowered::Vec(vec_elementwise(fb, *op, x, y)?))
                }
                (BinOp::Mul, Lowered::Num(s), Lowered::Vec(v))
                | (BinOp::Mul, Lowered::Vec(v), Lowered::Num(s)) => {
                    Ok(Lowered::Vec(vec_scale(fb, v, *s)?))
                }
                (BinOp::Mul, Lowered::Vec(x), Lowered::Vec(y)) if !x.is_empty() && x.len() == y.len() => {
                    Ok(Lowered::Num(vec_dot(fb, x, y)?))
                }
                (BinOp::Div, Lowered::Vec(v), Lowered::Num(s)) => {
                    Ok(Lowered::Vec(vec_div(fb, v, *s, true)?))
                }
                (BinOp::Div, Lowered::Num(s), Lowered::Vec(v)) => {
                    Ok(Lowered::Vec(vec_div(fb, v, *s, false)?))
                }
                _ => Err(JitError::Unsupported("unsupported operand types for arithmetic")),
            }
        }
        // `c ? then : els` — the interpreter evaluates ONLY the taken branch, but both branches are
        // side-effect-free (pure arithmetic), so eager `select` is bit-identical: the untaken branch's
        // discarded NaN/inf can't affect the chosen result. Branches must agree in type.
        ExprKind::Ternary { cond, then, els } => {
            let cv = compile_expr(fb, cond, lower)?;
            let c = truthy(fb, &cv)?;
            let tv = compile_expr(fb, then, lower)?;
            let ev = compile_expr(fb, els, lower)?;
            // Branches must AGREE in shape (both Num, both Bool, or both same-length Vec) — a `select` per
            // element for a vector. A shape mismatch (incl. differing vector lengths) DECLINES: the interpreter
            // would pick one at runtime, which a scalarized value can't represent.
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
                let scoped = Lower { locals: &locals, ..*lower };
                let v = compile_expr(fb, &b.value, &scoped)?;
                locals.insert(name, v);
            }
            let scoped = Lower { locals: &locals, ..*lower };
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
                let a = compile_expr(fb, &args[0].value, lower)?.num()?;
                let b = if arity == 2 {
                    compile_expr(fb, &args[1].value, lower)?.num()?
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
                // Positional only, no more args than params (extra positional args are dropped by OpenSCAD,
                // but a JIT'd numeric callee never wants them — decline as unusual).
                if args.len() > cparams.len() || args.iter().any(|a| a.name.is_some()) {
                    return Err(JitError::Unsupported("call"));
                }
                let empty_index = BTreeMap::new(); // callee params live in `callee_env`, not `params_ptr`
                let empty_locals = LetEnv::new();
                let mut callee_env = LetEnv::new();
                for (i, p) in cparams.iter().enumerate() {
                    let pname = p.name.as_ref();
                    if let Some(arg) = args.get(i) {
                        // A provided arg is compiled in the CALLER's env (may be a scalarized vector).
                        let v = compile_expr(fb, &arg.value, lower)?;
                        callee_env.insert(pname, v);
                    } else if let Some(default) = p.default.as_ref() {
                        // Unfilled → its DEFAULT, compiled in the DEFINITION scope (no caller locals, no
                        // sibling params) — matching the interpreter's documented default-eval simplification.
                        let def_lower =
                            Lower { index: &empty_index, locals: &empty_locals, ..*lower };
                        let v = compile_expr(fb, default, &def_lower)?;
                        callee_env.insert(pname, v);
                    }
                    // else: no arg, no default → leave `pname` unbound; the body DECLINES if it uses it (the
                    // interpreter would see `undef` there, which the numeric JIT can't represent anyway).
                }
                let mut stack = lower.inlining.to_vec();
                stack.push(name.as_str());
                let callee_lower =
                    Lower { index: &empty_index, locals: &callee_env, inlining: &stack, ..*lower };
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
                _ => return Err(JitError::Unsupported("assert without a positional condition")),
            };
            let cv = compile_expr(fb, cond, lower)?;
            let t = truthy(fb, &cv)?;
            let failed = fb.ins().icmp_imm(IntCC::Equal, t, 0); // 1 iff the condition is falsy
            let prev = fb.ins().load(types::I8, MemFlagsData::trusted(), lower.raised_ptr, 0);
            let now = fb.ins().bor(prev, failed);
            fb.ins().store(MemFlagsData::trusted(), now, lower.raised_ptr, 0);
            compile_expr(fb, body, lower)
        }
        // A vector LITERAL `[a, b, c]` → a scalarized `Lowered::Vec` of its compiled elements (P.1.6 rung A). A
        // comprehension element (`[for …]`, `each …`) declines when its node compiles — a dynamic length the
        // scalarized value can't hold, so the whole vector declines with that element's reason.
        ExprKind::Vector(elems) => {
            let lowered: Result<Vec<Lowered>, JitError> =
                elems.iter().map(|e| compile_expr(fb, e, lower)).collect();
            Ok(Lowered::Vec(lowered?))
        }
        // `v[i]` with a STATIC (literal) index into a scalarized vector → the element (P.1.6 rung A). The
        // interpreter floors a finite non-negative index (`i as usize`); an out-of-range / negative /
        // non-finite / DYNAMIC index yields `undef` there, which the JIT can't represent → DECLINE.
        ExprKind::Index { base, index } => {
            let Lowered::Vec(elems) = compile_expr(fb, base, lower)? else {
                return Err(JitError::Unsupported("index of a non-vector"));
            };
            let ExprKind::Num(n) = &index.kind else {
                return Err(JitError::Unsupported("dynamic index"));
            };
            if !n.is_finite() || *n < 0.0 {
                return Err(JitError::Unsupported("index out of range"));
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "guarded finite && >= 0; matches the interpreter's `i as usize`, and an out-of-range \
                index falls through to the `nth` miss → decline"
            )]
            let idx = *n as usize;
            elems.into_iter().nth(idx).ok_or(JitError::Unsupported("index out of range"))
        }
        // `v.x`/`v.y`/`v.z` on a scalarized vector → element 0/1/2 (P.1.6 rung B). `ops::member` maps ONLY
        // x/y/z to an index and EVERYTHING else to `undef`; a `.z` on a too-short vector is `undef` too. The
        // JIT can't represent `undef`, so a non-xyz field OR an out-of-range axis DECLINES — same element,
        // no float op, so bit-identical to the interpreter's `index(base, axis)`.
        ExprKind::Member { base, field } => {
            let Lowered::Vec(elems) = compile_expr(fb, base, lower)? else {
                return Err(JitError::Unsupported("member access on a non-vector"));
            };
            let idx = match field.as_str() {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                _ => return Err(JitError::Unsupported("non-xyz member access")),
            };
            elems.into_iter().nth(idx).ok_or(JitError::Unsupported("member axis out of range"))
        }
        // Everything else DECLINES — named so the EXPLAIN coverage histogram (P.1.4) shows WHICH node kind
        // blocks each function, i.e. the absorption ceiling per subset feature we might add next.
        other => Err(JitError::Unsupported(kind_name(other))),
    }
}

/// Seedless `rands(min, max, count)` with a LITERAL count → `count` sequential draws from the woven
/// [`RandStream`] as a `Lowered::Vec` (P.1.6 rung-D piece 1). Mirrors `builtins::rands`'s SEEDLESS path: `min`
/// and `max` must be numbers, `count` is the interpreter's `as_index` (finite, ≥ 0, TRUNCATED). Each draw is a
/// [`jit_rand_next`] call advancing the shared stream, emitted in SOURCE order — so the stream advances exactly
/// as the interpreter's `(0..count)` loop would, bit-identical.
///
/// DECLINES (→ the interpreter draws instead, advancing the stream identically, so a decline is always safe):
/// a 4th `seed` arg (the seeded path is pure — a follow-on), a NON-literal count (dynamic length → rung-D
/// piece 2), a count over the scalarize cap [`MAX_VEC_ARG`] (would bloat the IR), a negative / non-finite
/// literal count (`as_index` → `undef`), a non-`Num` min|max, or any named arg.
fn compile_seedless_rands(
    fb: &mut FunctionBuilder,
    args: &[fab_lang::Arg],
    lower: &Lower,
) -> Result<Lowered, JitError> {
    // Seedless is EXACTLY 3 positional args; a 4th (seed) or a named arg declines.
    if args.len() != 3 || args.iter().any(|a| a.name.is_some()) {
        return Err(JitError::Unsupported("call"));
    }
    // count must be a compile-time literal the interpreter's `as_index` accepts (finite, ≥ 0), truncated.
    let ExprKind::Num(n) = &args[2].value.kind else {
        return Err(JitError::Unsupported("dynamic rands count")); // rung-D piece 2
    };
    if !n.is_finite() || *n < 0.0 {
        return Err(JitError::Unsupported("rands count out of range")); // as_index → undef → decline
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "guarded finite && >= 0; matches the interpreter's `as_index` truncation to usize"
    )]
    let count = *n as usize;
    if count > MAX_VEC_ARG {
        return Err(JitError::Unsupported("rands count over the scalarize cap")); // interpret / rung-D piece 2
    }
    // min, max compiled ONCE (their own side effects, incl. a nested rands, happen here — before the draws,
    // matching the interpreter evaluating the args before the builtin body). Non-`Num` → the interpreter's
    // `rands` returns undef → decline.
    let min = compile_expr(fb, &args[0].value, lower)?.num()?;
    let max = compile_expr(fb, &args[1].value, lower)?.num()?;
    let mut elems = Vec::with_capacity(count);
    for _ in 0..count {
        let call = fb.ins().call(lower.rand_next, &[lower.rand_ptr, min, max]);
        elems.push(Lowered::Num(fb.inst_results(call)[0]));
    }
    Ok(Lowered::Vec(elems))
}

/// Whether `expr` (recursively) calls `rands` — the standalone [`compile_function`] guard (P.1.6 rung-D piece
/// 1). That API weaves no `RandStream`, so a rands body must DECLINE rather than have `JitFn::call`'s null
/// stream pointer dereferenced. Over-conservative (a seeded / dynamic-count rands would decline in the compiler
/// anyway), but simple + safe. Comprehensions / ranges aren't walked — they decline before any draw is emitted.
fn uses_rands(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            matches!(&callee.kind, ExprKind::Ident(n) if n.as_str() == "rands")
                || uses_rands(callee)
                || args.iter().any(|a| uses_rands(&a.value))
        }
        ExprKind::Unary { operand, .. } => uses_rands(operand),
        ExprKind::Binary { lhs, rhs, .. } => uses_rands(lhs) || uses_rands(rhs),
        ExprKind::Ternary { cond, then, els } => {
            uses_rands(cond) || uses_rands(then) || uses_rands(els)
        }
        ExprKind::Index { base, index } => uses_rands(base) || uses_rands(index),
        ExprKind::Member { base, .. } => uses_rands(base),
        ExprKind::Vector(es) => es.iter().any(uses_rands),
        ExprKind::Let { bindings, body } => {
            bindings.iter().any(|b| uses_rands(&b.value)) || uses_rands(body)
        }
        ExprKind::Assert { args, body } => {
            args.iter().any(|a| uses_rands(&a.value)) || body.as_deref().is_some_and(uses_rands)
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
            let Lowered::Vec(elems) = compile_expr(fb, &args[0].value, lower)? else {
                return Err(JitError::Unsupported("len of a non-vector"));
            };
            #[allow(
                clippy::cast_precision_loss,
                reason = "a scalarized vector's length is tiny (<= MAX_VEC_ARG); f64 is exact"
            )]
            let n = elems.len() as f64;
            Ok(Lowered::Num(fb.ins().f64const(n)))
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
                let x = e.num()?;
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
            let (Lowered::Vec(a), Lowered::Vec(b)) =
                (compile_expr(fb, &args[0].value, lower)?, compile_expr(fb, &args[1].value, lower)?)
            else {
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
                    let (a0, a1, a2) = (a0.num()?, a1.num()?, a2.num()?);
                    let (b0, b1, b2) = (b0.num()?, b1.num()?, b2.num()?);
                    let x = sub_of_products(fb, a1, b2, a2, b1);
                    let y = sub_of_products(fb, a2, b0, a0, b2);
                    let z = sub_of_products(fb, a0, b1, a1, b0);
                    Ok(Lowered::Vec(vec![Lowered::Num(x), Lowered::Num(y), Lowered::Num(z)]))
                }
                // 2D cross → a SCALAR (`a0·b1 − a1·b0`).
                ([a0, a1], [b0, b1]) => {
                    let (a0, a1, b0, b1) = (a0.num()?, a1.num()?, b0.num()?, b1.num()?);
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
                    Lowered::Vec(elems) => {
                        elems.iter().map(Lowered::num).collect::<Result<Vec<_>, _>>()?
                    }
                    Lowered::Num(x) => vec![x],
                    Lowered::Bool(_) => return Err(JitError::Unsupported("min/max of a boolean")),
                }
            } else {
                // 0 or ≥2 args: the `multi` branch — every arg is a plain number (a vector/bool → undef → decline).
                args.iter()
                    .map(|a| compile_expr(fb, &a.value, lower)?.num())
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
/// order (order-independent per lane, so bit-identical to the interpreter's `zip_reuse`). A non-`Num` element
/// (a nested vector — a matrix row) DECLINES: rung A arithmetic is FLAT vectors.
fn vec_elementwise(
    fb: &mut FunctionBuilder,
    op: BinOp,
    a: &[Lowered],
    b: &[Lowered],
) -> Result<Vec<Lowered>, JitError> {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let (x, y) = (x.num()?, y.num()?);
            let v = match op {
                BinOp::Add => fb.ins().fadd(x, y),
                BinOp::Sub => fb.ins().fsub(x, y),
                _ => return Err(JitError::Unsupported("non-elementwise vector op")),
            };
            Ok(Lowered::Num(v))
        })
        .collect()
}

/// Scale a scalarized vector by a scalar: `v[i] * s` — matching `map_reuse(v, |e| e * s)` for both `v*s` and
/// `s*v` (float multiply is bit-commutative).
fn vec_scale(fb: &mut FunctionBuilder, v: &[Lowered], s: Value) -> Result<Vec<Lowered>, JitError> {
    v.iter().map(|e| Ok(Lowered::Num(fb.ins().fmul(e.num()?, s)))).collect()
}

/// `v / s` (element / scalar, `vec_over_scalar`) or `s / v` (scalar / element) — matching `map_reuse(v, |e| e
/// / s)` / `|e| s / e`. Division isn't commutative, so the operand order is load-bearing.
fn vec_div(
    fb: &mut FunctionBuilder,
    v: &[Lowered],
    s: Value,
    vec_over_scalar: bool,
) -> Result<Vec<Lowered>, JitError> {
    v.iter()
        .map(|e| {
            let e = e.num()?;
            let q = if vec_over_scalar { fb.ins().fdiv(e, s) } else { fb.ins().fdiv(s, e) };
            Ok(Lowered::Num(q))
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
        let p = fb.ins().fmul(a[i].num()?, b[i].num()?);
        lanes[i % 4] = fb.ins().fadd(lanes[i % 4], p);
    }
    let l01 = fb.ins().fadd(lanes[0], lanes[1]);
    let l23 = fb.ins().fadd(lanes[2], lanes[3]);
    Ok(fb.ins().fadd(l01, l23))
}

/// Negate a `Lowered` — a number `fneg`, a vector elementwise (recursing so a nested vector negates too),
/// matching `ops::apply_unary`. A bool DECLINES (`-true` isn't in the subset).
fn neg_lowered(fb: &mut FunctionBuilder, v: Lowered) -> Result<Lowered, JitError> {
    match v {
        Lowered::Num(n) => Ok(Lowered::Num(fb.ins().fneg(n))),
        Lowered::Vec(elems) => {
            let out: Result<Vec<Lowered>, JitError> =
                elems.into_iter().map(|e| neg_lowered(fb, e)).collect();
            Ok(Lowered::Vec(out?))
        }
        Lowered::Bool(_) => Err(JitError::Unsupported("negate a boolean")),
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
    match (t, e) {
        (Lowered::Num(t), Lowered::Num(e)) => Ok(Lowered::Num(fb.ins().select(c, t, e))),
        (Lowered::Bool(t), Lowered::Bool(e)) => Ok(Lowered::Bool(fb.ins().select(c, t, e))),
        (Lowered::Vec(t), Lowered::Vec(e)) if t.len() == e.len() => {
            let out: Result<Vec<Lowered>, JitError> =
                t.into_iter().zip(e).map(|(t, e)| select_lowered(fb, c, t, e)).collect();
            Ok(Lowered::Vec(out?))
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
        ExprKind::LcFor { .. } | ExprKind::LcForC { .. } | ExprKind::LcEach(_) | ExprKind::LcIf { .. } => {
            "comprehension"
        }
        ExprKind::Assert { .. } => "assert",
        ExprKind::Echo { .. } => "echo",
        ExprKind::Str(_) => "string-literal",
        ExprKind::Undef => "undef-literal",
        // The handled kinds don't reach here; name them defensively rather than wildcard. `Vector`/`Index`
        // (rung A) + `Member` (rung B) are handled — they decline with a SPECIFIC reason inside their arm,
        // never via this path.
        ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Ident(_) | ExprKind::Unary { .. }
        | ExprKind::Binary { .. } | ExprKind::Ternary { .. } | ExprKind::Vector(_)
        | ExprKind::Index { .. } | ExprKind::Member { .. } => "unhandled-in-subset",
    }
}
