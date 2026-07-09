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

use cranelift::codegen::ir::{FuncRef, Value};
use cranelift::jit::{JITBuilder, JITModule};
use cranelift::module::{FuncId, Linkage, Module, ModuleError, default_libcall_names};
use cranelift::prelude::{
    AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, MemFlagsData,
    settings, types,
};

use fab_lang::{BinOp, Expr, ExprKind, UnOp};

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
}

impl CompiledFn {
    /// The parameter count the compiled function expects.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.arity
    }

    /// Call the compiled function with `params` (its length must equal [`CompiledFn::arity`]).
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> f64 {
        assert_eq!(
            params.len(),
            self.arity,
            "CompiledFn::call arity mismatch: got {}, expected {}",
            params.len(),
            self.arity
        );
        // THE unsafe seam of the whole crate. SAFETY: `code` is a finalized Cranelift function of
        // signature `extern "C" fn(*const f64) -> f64` (built in `define_one`); the owning module keeps
        // it mapped for as long as `self` is reachable; the function only READS `arity` f64s from the
        // pointer, and `params` has exactly that many (asserted above), so the reads are in-bounds.
        let f: unsafe extern "C" fn(*const f64) -> f64 = unsafe { std::mem::transmute(self.code) };
        unsafe { f(params.as_ptr()) }
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

    /// Call the compiled function with `params` (length must equal [`JitFn::arity`]).
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> f64 {
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
        defs: impl IntoIterator<Item = (&'a str, &'a [&'a str], &'a Expr)>,
    ) -> Result<Self, JitError> {
        let mut module = new_module()?;
        let (fmod_id, powf_id) = declare_math(&mut module)?;
        // Declare + define each compilable function, remembering its FuncId to resolve the code pointer
        // AFTER the single finalize. A unique export symbol per function (by index) avoids collisions.
        let mut pending: Vec<(String, FuncId, usize)> = Vec::new();
        for (i, (name, param_names, body)) in defs.into_iter().enumerate() {
            let symbol = format!("scad_jit_{i}");
            match define_one(&mut module, &symbol, param_names, body, fmod_id, powf_id) {
                Ok(func_id) => pending.push((name.to_string(), func_id, param_names.len())),
                Err(JitError::Unsupported(_)) => {} // declined → interpreter handles it
                Err(e) => return Err(e), // a real codegen failure — surface it
            }
        }
        module
            .finalize_definitions()
            .map_err(|e| JitError::Cranelift(e.to_string()))?;
        let fns = pending
            .into_iter()
            .map(|(name, func_id, arity)| {
                let code = module.get_finalized_function(func_id);
                (name, CompiledFn { code, arity })
            })
            .collect();
        Ok(JitRegistry { _module: module, fns })
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
    let (fmod_id, powf_id) = declare_math(&mut module)?;
    let func_id = define_one(&mut module, "scad_jit_fn", param_names, body, fmod_id, powf_id)?;
    module
        .finalize_definitions()
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    let code = module.get_finalized_function(func_id);
    Ok(JitFn {
        _module: module,
        inner: CompiledFn { code, arity: param_names.len() },
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
    Ok(JITModule::new(jb))
}

/// Declare the two external math routines (`(f64, f64) -> f64`) as imports in `module` — done ONCE per
/// module, their `FuncId`s reused by every function compiled into it.
fn declare_math(module: &mut JITModule) -> Result<(FuncId, FuncId), JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    let mut math_sig = module.make_signature();
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let fmod_id = module.declare_function("jit_fmod", Linkage::Import, &math_sig).map_err(cl)?;
    let powf_id = module.declare_function("jit_powf", Linkage::Import, &math_sig).map_err(cl)?;
    Ok((fmod_id, powf_id))
}

/// Build the IR for one function and declare + define it in `module` under `symbol` (NOT finalized —
/// the caller finalizes the whole module once). Returns the `FuncId` to resolve the code pointer after
/// finalize. On [`JitError::Unsupported`] nothing is added to the module (the IR is built before the
/// declare/define), so a declined function leaves the module clean for the next one.
fn define_one(
    module: &mut JITModule,
    symbol: &str,
    param_names: &[&str],
    body: &Expr,
    fmod_id: FuncId,
    powf_id: FuncId,
) -> Result<FuncId, JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());
    let ptr_ty = module.target_config().pointer_type();
    let mut ctx = module.make_context();
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(types::F64));

    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let block = fb.create_block();
        fb.append_block_params_for_function_params(block);
        fb.switch_to_block(block);
        fb.seal_block(block);
        let params_ptr = fb.block_params(block)[0];

        let fmod_ref = module.declare_func_in_func(fmod_id, fb.func);
        let powf_ref = module.declare_func_in_func(powf_id, fb.func);
        let index: BTreeMap<&str, usize> =
            param_names.iter().enumerate().map(|(i, &n)| (n, i)).collect();

        // IR is built BEFORE declare/define — an Unsupported node returns here with the module untouched.
        let result = compile_expr(&mut fb, body, params_ptr, &index, fmod_ref, powf_ref)?;
        fb.ins().return_(&[result]);
        fb.finalize();
    }

    let func_id = module.declare_function(symbol, Linkage::Export, &ctx.func.signature).map_err(cl)?;
    module.define_function(func_id, &mut ctx).map_err(cl)?;
    module.clear_context(&mut ctx);
    Ok(func_id)
}

/// Recursively lower `expr` to a Cranelift `f64` value. Left operand before right — but for pure
/// numeric ops the operand ORDER doesn't affect the result bits (the operation is the same
/// `fadd(a, b)` either way); what matters is that we emit the operation itself, never a fused or
/// reordered variant. The AST is `MAX_DEPTH`-bounded by the parser, so this recursion can't overflow.
fn compile_expr(
    fb: &mut FunctionBuilder,
    expr: &Expr,
    params_ptr: Value,
    index: &BTreeMap<&str, usize>,
    fmod: FuncRef,
    powf: FuncRef,
) -> Result<Value, JitError> {
    match &expr.kind {
        ExprKind::Num(n) => Ok(fb.ins().f64const(*n)),
        ExprKind::Ident(name) => {
            let i = index
                .get(name.as_str())
                .ok_or(JitError::Unsupported("non-parameter identifier"))?;
            let offset =
                i32::try_from(i * 8).map_err(|_| JitError::Unsupported("param offset overflow"))?;
            Ok(fb
                .ins()
                .load(types::F64, MemFlagsData::trusted(), params_ptr, offset))
        }
        ExprKind::Unary { op, operand } => {
            let v = compile_expr(fb, operand, params_ptr, index, fmod, powf)?;
            match op {
                UnOp::Neg => Ok(fb.ins().fneg(v)),
                UnOp::Pos => Ok(v),
                UnOp::Not | UnOp::BitNot => Err(JitError::Unsupported("non-arithmetic unary op")),
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let a = compile_expr(fb, lhs, params_ptr, index, fmod, powf)?;
            let b = compile_expr(fb, rhs, params_ptr, index, fmod, powf)?;
            match op {
                BinOp::Add => Ok(fb.ins().fadd(a, b)),
                BinOp::Sub => Ok(fb.ins().fsub(a, b)),
                BinOp::Mul => Ok(fb.ins().fmul(a, b)),
                BinOp::Div => Ok(fb.ins().fdiv(a, b)),
                BinOp::Mod => {
                    let call = fb.ins().call(fmod, &[a, b]);
                    Ok(fb.inst_results(call)[0])
                }
                BinOp::Pow => {
                    let call = fb.ins().call(powf, &[a, b]);
                    Ok(fb.inst_results(call)[0])
                }
                _ => Err(JitError::Unsupported("non-arithmetic binary op")),
            }
        }
        _ => Err(JitError::Unsupported("unsupported expression node")),
    }
}
