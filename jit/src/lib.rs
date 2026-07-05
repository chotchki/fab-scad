//! fab-jit — a Cranelift JIT spike for scad-rs numeric functions (I.8).
//!
//! NATIVE-ONLY by design. The browser can't JIT in-sandbox, so scad-rs's ONE implementation
//! everywhere is the interpreter; this crate is a native accelerator whose only reason to exist is to
//! PROVE two things ahead of the Phase-L JIT-vs-intrinsics decision: that a Cranelift-compiled numeric
//! function is BIT-IDENTICAL to the interpreter (`fast == JIT`, the sibling of `fast == slow`), and
//! that the float-discipline recipe holds — no auto-FMA, fixed evaluation order, and every op Cranelift
//! has no deterministic native instruction for routed to a CALL into our own Rust math.
//!
//! This crate is the ONE place `unsafe` lives outside the kernel FFI: calling a finalized code pointer.
//! It's confined to [`JitFn::call`] and documented there. fab-lang stays `unsafe_code = forbid`.
//!
//! Scope of the spike: a function body over `f64` parameters using number literals, parameter reads,
//! unary `-`/`+`, and `+ - * / % ^`. Anything else ([`ExprKind::Call`], ternary, indexing, a free
//! variable) returns [`JitError::Unsupported`] — the compiler never emits a wrong answer, it declines.

use std::collections::BTreeMap;

use cranelift::codegen::ir::{FuncRef, Value};
use cranelift::jit::{JITBuilder, JITModule};
use cranelift::module::{Linkage, Module, ModuleError, default_libcall_names};
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
    /// A construct outside the spike's numeric subset (a call, ternary, index, non-parameter
    /// identifier, or a non-arithmetic operator). Carries a short reason.
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

/// A JIT-compiled numeric function: `fn(params: &[f64]) -> f64`. Owns its [`JITModule`] so the
/// executable memory stays mapped for the function's lifetime.
pub struct JitFn {
    // Keeps the finalized code mapped. Never freed (the spike compiles a handful of functions); a real
    // integration would pool modules + free on drop. The `code` pointer below points into this memory,
    // which Cranelift places at a fixed address, so moving the struct doesn't invalidate it.
    _module: JITModule,
    code: *const u8,
    arity: usize,
}

impl JitFn {
    /// The parameter count the compiled function expects.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.arity
    }

    /// Call the JIT-compiled function with `params` (its length must equal [`JitFn::arity`]).
    ///
    /// # Panics
    /// If `params.len()` != the function's arity.
    #[must_use]
    pub fn call(&self, params: &[f64]) -> f64 {
        assert_eq!(
            params.len(),
            self.arity,
            "JitFn::call arity mismatch: got {}, expected {}",
            params.len(),
            self.arity
        );
        // THE unsafe seam of the whole crate. SAFETY: `code` is a finalized Cranelift function of
        // signature `extern "C" fn(*const f64) -> f64` (built in `compile_function`); `_module` keeps
        // it mapped; the function only READS `arity` f64s from the pointer, and `params` has exactly
        // that many, so the reads are in-bounds.
        let f: unsafe extern "C" fn(*const f64) -> f64 = unsafe { std::mem::transmute(self.code) };
        unsafe { f(params.as_ptr()) }
    }
}

/// Compile a numeric function body (over `param_names`, in order) to native code via Cranelift.
///
/// The generated function has signature `extern "C" fn(*const f64) -> f64`: parameter `i` is read from
/// `params[i]`. Evaluation order mirrors the interpreter (left operand then right), and `%`/`^` become
/// calls to [`jit_fmod`]/[`jit_powf`] — the same Rust ops the interpreter uses — so the result is
/// bit-identical.
///
/// # Errors
/// [`JitError::Unsupported`] for any node outside the numeric subset, [`JitError::Cranelift`] for a
/// codegen failure.
pub fn compile_function(param_names: &[&str], body: &Expr) -> Result<JitFn, JitError> {
    let cl = |e: ModuleError| JitError::Cranelift(e.to_string());

    // ISA + flags. `opt_level=speed` is fine for determinism: Cranelift never CONTRACTS fmul+fadd into
    // an fma (that's an LLVM fast-math behavior); it emits the instructions we ask for, in order.
    let mut flags = settings::builder();
    flags
        .set("opt_level", "speed")
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    flags
        .set("is_pic", "false")
        .map_err(|e| JitError::Cranelift(e.to_string()))?;
    let isa = cranelift::native::builder()
        .map_err(|e| JitError::Cranelift(e.to_string()))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| JitError::Cranelift(e.to_string()))?;

    let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
    jb.symbol("jit_fmod", jit_fmod as *const u8);
    jb.symbol("jit_powf", jit_powf as *const u8);
    let mut module = JITModule::new(jb);
    let ptr_ty = module.target_config().pointer_type();

    // The two external math fns: `(f64, f64) -> f64`.
    let mut math_sig = module.make_signature();
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let fmod_id = module
        .declare_function("jit_fmod", Linkage::Import, &math_sig)
        .map_err(cl)?;
    let powf_id = module
        .declare_function("jit_powf", Linkage::Import, &math_sig)
        .map_err(cl)?;

    // The JIT function: `(params: *const f64) -> f64`.
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
        let index: BTreeMap<&str, usize> = param_names
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, i))
            .collect();

        let result = compile_expr(&mut fb, body, params_ptr, &index, fmod_ref, powf_ref)?;
        fb.ins().return_(&[result]);
        fb.finalize();
    }

    let func_id = module
        .declare_function("scad_jit_fn", Linkage::Export, &ctx.func.signature)
        .map_err(cl)?;
    module.define_function(func_id, &mut ctx).map_err(cl)?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().map_err(cl)?;
    let code = module.get_finalized_function(func_id);

    Ok(JitFn {
        _module: module,
        code,
        arity: param_names.len(),
    })
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
