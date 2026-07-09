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

use std::collections::BTreeMap;

use cranelift::codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift::codegen::ir::{FuncRef, Value};
use cranelift::jit::{JITBuilder, JITModule};
use cranelift::module::{FuncId, Linkage, Module, ModuleError, default_libcall_names};
use cranelift::prelude::{
    AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, MemFlagsData,
    settings, types,
};

/// The runtime TYPE of a compiled sub-expression. Both are IR values, but the distinction is
/// load-bearing: the interpreter's comparisons and `&&`/`||`/`!` produce a BOOL (`Value::Bool`), not a
/// number, and a function that RETURNS a bool must NOT be JIT'd (the dispatch wraps the result in
/// `Value::Num`, so a bool return would diverge). `Num` is an `f64`; `Bool` is an `i8` (0/1, the shape
/// Cranelift's `fcmp`/`icmp` yield and `select` consumes). A bool only ever feeds a condition or another
/// logical op — it can't be the function's return, nor an operand to arithmetic.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Num,
    Bool,
}

use fab_lang::{
    BinOp, Expr, ExprKind, JitConst, JitDef, JitOutcome, NumericJit, NumericJitFactory, Parameter,
    UnOp, jit_math_id,
};

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

/// A finalized numeric function: `fn(params: &[f64]) -> f64` as a raw code pointer. The executable
/// memory it points into is owned by the [`JitFn`] or [`JitRegistry`] that produced it — a `CompiledFn`
/// is only valid for that owner's lifetime, which the borrow checker enforces (registry entries are
/// returned by reference).
pub struct CompiledFn {
    code: *const u8,
    arity: usize,
    /// The STATIC return type (P.1.4e) — the native ABI returns an untyped `f64`, so this says whether it's a
    /// number or a boolean (`0.0`/`1.0`). Derived once at compile time from the body's [`Ty`]; the dispatch
    /// re-tags the result into the matching [`fab_lang::Value`]. Extends to a vector descriptor with the ABI.
    ret_ty: Ty,
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
        matches!(self.ret_ty, Ty::Bool)
    }

    /// Call the compiled function with `params` (its length must equal [`CompiledFn::arity`]). Returns
    /// `None` when the body's inline `assert` FAILED — the JIT can't unwind, so it flags a status byte and
    /// the caller falls back to the interpreter, which re-runs and raises the exact error (with its real
    /// message). On the common path (no assert, or assert passes) it's `Some(result)`.
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> Option<f64> {
        assert_eq!(
            params.len(),
            self.arity,
            "CompiledFn::call arity mismatch: got {}, expected {}",
            params.len(),
            self.arity
        );
        // THE unsafe seam of the whole crate. SAFETY: `code` is a finalized Cranelift function of signature
        // `extern "C" fn(*const f64, *mut u8) -> f64` (built in `define_one`); the owning module keeps it
        // mapped for as long as `self` is reachable; the function READS `arity` f64s from the first pointer
        // (`params` has exactly that many, asserted above) and WRITES one byte through the second, which
        // points at our live local `raised`. No unwinding crosses the boundary — an assert sets the byte.
        let f: unsafe extern "C" fn(*const f64, *mut u8) -> f64 =
            unsafe { std::mem::transmute(self.code) };
        let mut raised: u8 = 0;
        let result = unsafe { f(params.as_ptr(), &raw mut raised) };
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
    /// inline `assert` failed (see [`CompiledFn::call`]).
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> Option<f64> {
        self.inner.call(params)
    }
}

/// A cache of many numeric functions compiled into ONE [`JITModule`] and finalized together — the
/// production form of the spike (which leaked a module per function). Built from a program's user
/// functions: each is TRIED, the numeric-subset ones are kept (keyed by name), the rest declined and
/// left to the interpreter. Lookup is by function name (a program's function store is name-keyed, like
/// the intrinsic registry). The module is kept mapped for the registry's lifetime.
pub struct JitRegistry {
    _module: JITModule,
    fns: BTreeMap<String, CompiledFn>,
    /// Functions that DIDN'T compile → the first out-of-subset node kind that blocked them ([`kind_name`]).
    /// The absorption ceiling: aggregated, it says which subset feature (calls, indexing, comprehensions)
    /// would unlock the most whole functions. Surfaced by the `FAB_JIT_EXPLAIN` coverage histogram.
    declined: BTreeMap<String, &'static str>,
}

impl JitRegistry {
    /// Compile every numeric-subset function in `defs` into one module. Each entry is `(name,
    /// param_names, body)`; a function outside the subset (or a codegen failure) is SKIPPED, not fatal —
    /// the registry holds only what compiled, and the caller interprets the rest. An empty result (no
    /// function compiled) is a valid, empty registry.
    ///
    /// # Errors
    /// [`JitError::Cranelift`] only for a module-level failure (ISA/module setup, or the single
    /// `finalize_definitions`) — a per-function decline is swallowed, never surfaced as an error.
    pub fn build<'a>(
        defs: impl IntoIterator<Item = (&'a str, &'a [Parameter], &'a Expr)>,
        consts: impl IntoIterator<Item = (&'a str, &'a Expr)>,
    ) -> Result<Self, JitError> {
        // Materialize the input so every function is visible to every other (a caller can INLINE any callee,
        // including forward references). `fn_defs` maps name → (parameters, body) for the inliner; `globals`
        // maps a top-level constant's name → its value-expr for free-variable resolution (P.1.4 globals).
        let entries: Vec<(&str, &[Parameter], &Expr)> = defs.into_iter().collect();
        let fn_defs: FnDefs = entries.iter().map(|&(n, p, b)| (n, (p, b))).collect();
        let globals: Globals = consts.into_iter().collect();
        let mut module = new_module()?;
        let helpers = declare_helpers(&mut module)?;
        // Declare + define each compilable function, remembering its FuncId to resolve the code pointer
        // AFTER the single finalize. A unique export symbol per function (by index) avoids collisions.
        let mut pending: Vec<(String, FuncId, usize, Ty)> = Vec::new();
        let mut declined: BTreeMap<String, &'static str> = BTreeMap::new();
        for (i, &(name, params, body)) in entries.iter().enumerate() {
            let symbol = format!("scad_jit_{i}");
            // `define_one` indexes the top-level params by NAME (they're always fully applied via the
            // dispatch gate); defaults only matter for INLINED callees, which read them from `fn_defs`.
            let param_names: Vec<&str> = params.iter().map(|p| p.name.as_ref()).collect();
            match define_one(&mut module, &symbol, &param_names, body, &fn_defs, &globals, &helpers) {
                Ok((func_id, ret_ty)) => pending.push((name.to_string(), func_id, params.len(), ret_ty)),
                // Declined → the interpreter handles it; record the FIRST out-of-subset node that blocked it
                // (the absorption-ceiling signal for the EXPLAIN histogram).
                Err(JitError::Unsupported(reason)) => {
                    declined.insert(name.to_string(), reason);
                }
                Err(e) => return Err(e), // a real codegen failure — surface it
            }
        }
        module
            .finalize_definitions()
            .map_err(|e| JitError::Cranelift(e.to_string()))?;
        let fns = pending
            .into_iter()
            .map(|(name, func_id, arity, ret_ty)| {
                let code = module.get_finalized_function(func_id);
                (name, CompiledFn { code, arity, ret_ty })
            })
            .collect();
        Ok(JitRegistry { _module: module, fns, declined })
    }

    /// Per-function decline reasons (name → the first out-of-subset node kind) — the absorption ceiling.
    #[must_use]
    pub fn declined(&self) -> &BTreeMap<String, &'static str> {
        &self.declined
    }

    /// The compiled function named `name`, if one was compiled (else the caller interprets).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CompiledFn> {
        self.fns.get(name)
    }

    /// How many functions compiled — the coverage count (feeds the EXPLAIN report).
    #[must_use]
    pub fn len(&self) -> usize {
        self.fns.len()
    }

    /// Whether nothing compiled (a program with no numeric-subset functions).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }

    /// The names of the compiled functions, sorted — for the FAB_EXPLAIN coverage report.
    pub fn compiled_names(&self) -> impl Iterator<Item = &str> {
        self.fns.keys().map(String::as_str)
    }
}

/// The dispatch hook the interpreter calls (P.1.2). A compiled function named `name` with matching arity
/// runs as native code; anything else returns `None` and the interpreter runs the body. The arity filter
/// is defensive — the dispatch gate already guarantees `args.len()` equals the compiled arity, but a
/// mismatch declines rather than reading past the arg slice.
impl NumericJit for JitRegistry {
    fn call_numeric(&self, name: &str, args: &[f64]) -> Option<JitOutcome> {
        // A compiled function whose inline assert FAILED returns `None` → the interpreter runs the body and
        // raises the exact error. So `None` here means BOTH "not compiled" and "compiled but raised" — either
        // way the interpreter takes over, which is correct for both. On a value, RE-TAG the untyped `f64` by
        // the function's static return type (P.1.4e): a bool-returning predicate yields `0.0`/`1.0` that the
        // dispatch must wrap in `Value::Bool`, not `Value::Num`.
        let f = self.get(name).filter(|f| f.arity() == args.len())?;
        let raw = f.call(args)?;
        Some(match f.ret_ty {
            Ty::Num => JitOutcome::Num(raw),
            Ty::Bool => JitOutcome::Bool(raw != 0.0),
        })
    }
}

/// The factory the native shell hands to the eval entry (P.1.2b): given a program's function defs, compile
/// the numeric-subset ones into a [`JitRegistry`].
///
/// OPT-IN under `FAB_JIT=1` for now — the interpreter is the bit-identical baseline and the doctrine is
/// never-silently-wrong, so a NEW eval path stays off by default until P.1.3's end-to-end fast==JIT
/// differential proves it byte-for-byte on the corpus/models; then the default flips ON. Unset / any other
/// value → `None` (pure interpreter). An empty registry (nothing in the numeric subset compiled) also
/// returns `None`, so `Ctx.jit` carries a hook only when it can actually pay.
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
    let (func_id, ret_ty) =
        define_one(&mut module, "scad_jit_fn", param_names, body, &no_defs, &no_globals, &helpers)?;
    // The standalone API is f64-only (the fast==JIT differential compares raw f64s); a bool-returning body is
    // the registry path's job (it carries the type tag), so DECLINE it here to keep the contract.
    if matches!(ret_ty, Ty::Bool) {
        return Err(JitError::Unsupported("bool-returning body (standalone API is f64-only; use JitRegistry)"));
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
    jb.symbol("jit_math_call", jit_math_call as *const u8);
    Ok(JITModule::new(jb))
}

/// The external helper routines declared as imports in `module` — done ONCE per module, their `FuncId`s
/// reused by every function compiled into it.
struct Helpers {
    /// `jit_fmod(f64, f64) -> f64` — the `%` operator.
    fmod: FuncId,
    /// `jit_powf(f64, f64) -> f64` — the `^` operator.
    powf: FuncId,
    /// `jit_math_call(i32 id, f64, f64) -> f64` — a scalar math builtin dispatched by id (P.1.4b).
    math: FuncId,
}

fn declare_helpers(module: &mut JITModule) -> Result<Helpers, JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    // `(f64, f64) -> f64` for fmod/powf.
    let mut op_sig = module.make_signature();
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.params.push(AbiParam::new(types::F64));
    op_sig.returns.push(AbiParam::new(types::F64));
    let fmod = module.declare_function("jit_fmod", Linkage::Import, &op_sig).map_err(cl)?;
    let powf = module.declare_function("jit_powf", Linkage::Import, &op_sig).map_err(cl)?;
    // `(i32 id, f64, f64) -> f64` for the math dispatcher.
    let mut math_sig = module.make_signature();
    math_sig.params.push(AbiParam::new(types::I32));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let math = module.declare_function("jit_math_call", Linkage::Import, &math_sig).map_err(cl)?;
    Ok(Helpers { fmod, powf, math })
}

/// Build the IR for one function and declare + define it in `module` under `symbol` (NOT finalized —
/// the caller finalizes the whole module once). Returns the `FuncId` (to resolve the code pointer after
/// finalize) AND the body's static return [`Ty`] — a bool-returning body is returned as `0.0`/`1.0` and the
/// caller tags the function so the dispatch wraps `Value::Bool` (P.1.4e). On [`JitError::Unsupported`] nothing
/// is added to the module (the IR is built before the declare/define), so a declined function leaves the
/// module clean for the next one.
fn define_one(
    module: &mut JITModule,
    symbol: &str,
    param_names: &[&str],
    body: &Expr,
    defs: &FnDefs,
    globals: &Globals,
    helpers: &Helpers,
) -> Result<(FuncId, Ty), JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    let ptr_ty = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    // Signature: `(params: *const f64, raised: *mut u8) -> f64`. `raised` is the assert-failure out-param
    // (P.1.4) — an inline `assert(cond)` whose condition is falsy writes 1 to it; the JIT can't unwind.
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

        let fmod_ref = module.declare_func_in_func(helpers.fmod, fb.func);
        let powf_ref = module.declare_func_in_func(helpers.powf, fb.func);
        let math_ref = module.declare_func_in_func(helpers.math, fb.func);
        let index: BTreeMap<&str, usize> =
            param_names.iter().enumerate().map(|(i, &n)| (n, i)).collect();
        let locals = LetEnv::new(); // no let-bindings in scope at the function's top level
        let inlining: [&str; 0] = []; // nothing being inlined at the top level
        let lower = Lower {
            params_ptr,
            raised_ptr,
            index: &index,
            locals: &locals,
            defs,
            globals,
            inlining: &inlining,
            fmod: fmod_ref,
            powf: powf_ref,
            math: math_ref,
        };

        // IR is built BEFORE declare/define — an Unsupported node returns here with the module untouched.
        // A NUMERIC body returns its f64 directly; a BOOL body (a predicate / comparison / bool literal — the
        // JIT computes it as an i8 0/1) is returned as 0.0/1.0, and the caller tags the function so the dispatch
        // wraps `Value::Bool` (P.1.4e). A VECTOR-returning body declines for now — the fixed-size out-buffer is
        // rung C; rung A only compiles functions that use vectors INTERNALLY and reduce to a scalar/bool.
        let (ret, ty) = match compile_expr(&mut fb, body, &lower)? {
            Lowered::Num(v) => (v, Ty::Num),
            Lowered::Bool(v) => {
                let one = fb.ins().f64const(1.0);
                let zero = fb.ins().f64const(0.0);
                (fb.ins().select(v, one, zero), Ty::Bool)
            }
            Lowered::Vec(_) => return Err(JitError::Unsupported("vector return (rung C)")),
        };
        fb.ins().return_(&[ret]);
        fb.finalize();
        ret_ty = ty;
    }

    let func_id = module.declare_function(symbol, Linkage::Export, &ctx.func.signature).map_err(cl)?;
    module.define_function(func_id, &mut ctx).map_err(cl)?;
    module.clear_context(&mut ctx);
    Ok((func_id, ret_ty))
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

/// Recursively lower `expr` to a Cranelift value + its [`Ty`]. Left operand before right — but for pure
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
    fmod: FuncRef,
    powf: FuncRef,
    math: FuncRef,
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
        // A call resolves in three ways: (1) a scalar math builtin → a call into OUR math (P.1.4b), (2) a USER
        // function → INLINE its body (step 2), (3) else DECLINE (a non-math/variadic builtin, a dynamic
        // `(expr)()` callee, an undefined name, a named arg).
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
        // Everything else DECLINES — named so the EXPLAIN coverage histogram (P.1.4) shows WHICH node kind
        // blocks each function, i.e. the absorption ceiling per subset feature we might add next.
        other => Err(JitError::Unsupported(kind_name(other))),
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
        ExprKind::Member { .. } => "member-access",
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
        // The handled kinds don't reach here; name them defensively rather than wildcard. `Vector`/`Index` are
        // handled (P.1.6 rung A) — they decline with a SPECIFIC reason inside their arm, never via this path.
        ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Ident(_) | ExprKind::Unary { .. }
        | ExprKind::Binary { .. } | ExprKind::Ternary { .. } | ExprKind::Vector(_)
        | ExprKind::Index { .. } => "unhandled-in-subset",
    }
}
