//! AR.20.1 — the bridge a compiled MODULE reaches the evaluator through.
//!
//! [`crate::surface::ModuleCtx`] is the contract; this is the one implementation of it, adapting
//! the interpreter's own state (the `Ctx`, the call-site children frame, the caller's scope) into
//! the shape a generated module sees. Everything a native does to the world goes through here,
//! which is the point: the native gets no other handle on the evaluator, so the surface it can
//! disturb is exactly what this file exposes.
//!
//! CHILDREN RENDER SYNCHRONOUSLY, by calling back into `eval_geometry_driver`. That is what makes
//! `children()` mean what it means upstream — the child statements are the USER's source, held
//! UNEVALUATED, rendered in the CALLER's scope, and possibly rendered zero times or many. They can
//! never be compiled, so re-entering interpretation is the correct boundary rather than a
//! shortcoming.

use std::cell::Cell;

use super::geo::GeoNode;
use super::geo2d::Geo;
use super::scope::Scope;
use super::value::Value;
use crate::parser::Stmt;
use crate::surface::{Children, ModuleCtx};

/// How deep the compiled module path is allowed to nest before it hands back to the interpreter.
///
/// A generated module DISPATCHES — it calls the next module with a direct Rust call — so it rides
/// the host stack, which the interpreter's explicit stack deliberately does not (`MAX_MODULE_DEPTH`
/// is `100_000` precisely because that depth is heap).
///
/// MEASURED before being chosen, not guessed. Peak module nesting on real work: chotchki's models
/// run 1 to 41 (`ams_stackfix` is the deepest), BOSL2's own examples 0 to 39 — and `fractal_tree`
/// hits 139, because it is a genuine recursion. So 64 clears every non-recursive real model with
/// headroom, and recursive geometry declines, which it must: no host-stack budget worth having
/// covers an unbounded fractal. The decline is not a failure mode, it is the design.
pub(super) const MAX_MODULE_NATIVE_DEPTH: usize = 64;

thread_local! {
    /// Live compiled-module nesting. Thread-local for the same reason the function-native guard is:
    /// the budget bounds THIS thread's stack, and renders run per-thread.
    static NATIVE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII ticket on [`MAX_MODULE_NATIVE_DEPTH`]. `enter` refuses past the budget; `Drop` gives the
/// level back, so an early return through `?` cannot leak depth.
pub(super) struct ModuleDepthGuard;

impl ModuleDepthGuard {
    /// Take a level, or `None` when the budget is spent — in which case the caller must fall
    /// through to the interpreter rather than recurse anyway.
    pub(super) fn enter() -> Option<Self> {
        NATIVE_DEPTH.with(|d| {
            if d.get() >= MAX_MODULE_NATIVE_DEPTH {
                None
            } else {
                d.set(d.get() + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for ModuleDepthGuard {
    fn drop(&mut self) {
        NATIVE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// The evaluator state one compiled module call sees.
pub(super) struct NativeModuleCtx<'a, 'c> {
    /// Arguments already bound to parameters by the evaluator's own two-phase rule — the
    /// AN.1/AN.2/AN.6 semantics a native must not reimplement.
    pub(super) args: Vec<Value>,
    /// The call-site geometry children, UNEVALUATED. Empty statements and child-block assignments
    /// are already filtered out upstream: neither is a child, and counting either would misalign
    /// both `$children` and `children(i)`.
    pub(super) child_stmts: Vec<&'a Stmt>,
    /// Child-block assignments — not children, but their bindings are in scope for every geometry
    /// child, so they are prepended when one is rendered.
    pub(super) child_assigns: Vec<&'a Stmt>,
    /// The CALLER's scope: where a child renders, per OpenSCAD's late binding.
    pub(super) caller_scope: Scope,
    /// The caller's island, for the same reason.
    pub(super) caller_island: usize,
    /// The module's own call scope — where its `$`-vars resolve.
    pub(super) call_scope: Scope,
    pub(super) ctx: &'c super::Ctx<'a>,
}

impl<'a> NativeModuleCtx<'a, '_> {
    /// Render a selection of the call-site children, in the caller's scope, unioned.
    fn render(&mut self, selected: &[&'a Stmt]) -> crate::Result<Geo> {
        if selected.is_empty() {
            return Ok(Geo::D3(GeoNode::Empty));
        }
        // The caller's LEXICAL scope with the CURRENT dynamic `$`-context overlaid by reference —
        // `call_frame`, never a copy. L.2.7: copying the reaching `$`-context per call is what cost
        // 42 clones a call once BOSL2 sets its top-level vars, and a native that reintroduced it
        // would be that bug in compiled form.
        let child_scope = Scope::call_frame(&self.caller_scope, &self.call_scope);
        // Child-block assignments first: their bindings must reach every geometry child.
        let mut stmts: Vec<&'a Stmt> = self.child_assigns.clone();
        stmts.extend_from_slice(selected);
        let global = self.ctx.island_globals.borrow()[self.caller_island].clone();
        let parts = super::geo_stack::eval_geometry_driver(
            &stmts,
            &child_scope,
            &global,
            self.caller_island,
            self.ctx,
        )?;
        // Union, matching the implicit grouping a child block gets in the interpreter.
        Ok(super::union_of(parts, self.ctx))
    }
}

impl ModuleCtx for NativeModuleCtx<'_, '_> {
    fn args(&self) -> &[Value] {
        &self.args
    }

    fn child_count(&self) -> usize {
        self.child_stmts.len()
    }

    fn child(&mut self, i: usize) -> crate::Result<Geo> {
        let Some(&stmt) = self.child_stmts.get(i) else {
            // Out of range renders nothing, matching upstream — not an error.
            return Ok(Geo::D3(GeoNode::Empty));
        };
        self.render(&[stmt])
    }

    fn children(&mut self) -> crate::Result<Geo> {
        let all = self.child_stmts.clone();
        self.render(&all)
    }

    fn dollar(&self, name: &str) -> Value {
        // Read THROUGH the chain rather than handing one over: see the L.2.7 note on `render`.
        self.call_scope.lookup(name)
    }

    fn call(
        &mut self,
        _name: &str,
        _args: &[Value],
        _dollars: &[(&str, Value)],
        _children: Children<'_>,
    ) -> crate::Result<Geo> {
        // AR.20.5. Dispatch is the item that makes this a compiler rather than an interpreter with
        // extra steps, and it is deliberately NOT stubbed silently: 97% of BOSL2 modules call
        // another module, so a native that quietly rendered nothing here would be wrong for almost
        // the whole library while still producing geometry.
        Err(crate::Error::Unimplemented(
            "module-to-module dispatch from a compiled native (AR.20.5)",
        ))
    }
}

/// The module's parameters read out of its bound call scope, in DECLARATION order — the positional
/// slice a native indexes.
///
/// Read back from the scope rather than re-derived from the call site on purpose: `bind_module_scope`
/// has already applied OpenSCAD's two-phase rule (all defaults, then arguments over them), the
/// duplicate-parameter precedence (AN.6, first-declared wins), and the `$`-arg ordering. A native
/// that matched arguments itself would be reimplementing exactly the semantics the AN family
/// documents getting wrong.
pub(super) fn bound_args(params: &[crate::parser::Parameter], call: &Scope) -> Vec<Value> {
    params.iter().map(|p| call.lookup(&p.name)).collect()
}
