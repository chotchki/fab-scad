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
use crate::surface::{Children, ModuleCall, ModuleCtx};

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
    /// COMPLETED outer native-module runs (AR.14.4.3 diagnostics). "Armed" and "RAN" are different
    /// facts — the band-1 postmortem: every transform native resolved, then declined at its first
    /// child-forwarding call, and no tier test could see the difference. A tier test that proves
    /// equality reads this to prove the native actually answered.
    pub(crate) static NATIVE_MODULE_RUNS: Cell<u64> = const { Cell::new(0) };
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

/// Holds a name on the evaluator's instantiation stack for the length of a call.
///
/// AR.20.5. The interpreted path balances its frames with a scheduled `PopModuleFrame` task; the
/// compiled path is an ordinary Rust call, so it balances them with `Drop` instead — which also
/// covers the `?` returns, where a hand-written pop would be skipped exactly when the stack most
/// needs restoring.
pub(super) struct ModuleStackGuard<'a, 'c> {
    ctx: &'c super::Ctx<'a>,
}

impl<'a, 'c> ModuleStackGuard<'a, 'c> {
    pub(super) fn push(name: &'a str, ctx: &'c super::Ctx<'a>) -> Self {
        ctx.module_stack.borrow_mut().push(name);
        Self { ctx }
    }
}

impl Drop for ModuleStackGuard<'_, '_> {
    fn drop(&mut self) {
        self.ctx.module_stack.borrow_mut().pop();
    }
}

/// One level of the interpreter's `module_depth`, plus its high-water mark. Same reasoning as
/// [`ModuleStackGuard`]: the compiled path unwinds through `Drop`, not through a work-stack task.
struct ModuleDepthTicket<'a, 'c> {
    ctx: &'c super::Ctx<'a>,
}

impl<'a, 'c> ModuleDepthTicket<'a, 'c> {
    fn enter(ctx: &'c super::Ctx<'a>) -> Self {
        let next = ctx.module_depth.get() + 1;
        ctx.module_depth.set(next);
        if next > ctx.peak_module_depth.get() {
            ctx.peak_module_depth.set(next);
        }
        Self { ctx }
    }
}

impl Drop for ModuleDepthTicket<'_, '_> {
    fn drop(&mut self) {
        let d = self.ctx.module_depth.get();
        self.ctx.module_depth.set(d.saturating_sub(1));
    }
}

/// The callee's children frame, which is what makes its `children()` find the CALL SITE's children.
struct ChildrenFrameGuard<'a, 'c> {
    ctx: &'c super::Ctx<'a>,
}

impl<'a, 'c> ChildrenFrameGuard<'a, 'c> {
    fn push(frame: super::ChildrenFrame<'a>, ctx: &'c super::Ctx<'a>) -> Self {
        ctx.children_stack.borrow_mut().push(frame);
        Self { ctx }
    }
}

impl Drop for ChildrenFrameGuard<'_, '_> {
    fn drop(&mut self) {
        self.ctx.children_stack.borrow_mut().pop();
    }
}

/// Where one module call's children came from, and therefore how rendering one works.
///
/// TWO shapes because there are two kinds of call site, and neither can be expressed as the other.
/// An INTERPRETED site has source statements, held unevaluated so `children()` renders them late in
/// the scope they were written in. A COMPILED site has already been turned into Rust, so its
/// children are thunks, bridged at render time onto the RENDERER's dynamic scope (see `Compiled`).
/// `Clone` is the bridge's re-borrow: `Stmts` clones its ref-vecs, `Compiled` copies its borrows.
#[derive(Clone)]
pub(super) enum CallChildren<'a, 'k> {
    /// Children written at an interpreted call site.
    Stmts {
        /// The geometry children, UNEVALUATED. Empty statements and child-block assignments are
        /// already filtered out upstream: neither is a child, and counting either would misalign
        /// both `$children` and `children(i)`.
        stmts: Vec<&'a Stmt>,
        /// Child-block assignments — not children, but their bindings are in scope for every
        /// geometry child, so they are prepended when one is rendered.
        assigns: Vec<&'a Stmt>,
        /// The scope the child statements were WRITTEN in: where they render, per OpenSCAD's late
        /// binding. Forwarded children keep their original site's scope, not the forwarder's.
        scope: Scope,
        /// That same site's island, for the same reason.
        island: usize,
    },
    /// Children written at a compiled call site: one thunk per child, in source order.
    ///
    /// Carries the CREATOR's structural pieces rather than its whole ctx, because the thunks must
    /// NOT run against the creator's dynamic context (AR.22, the tag-family lesson): the renderer
    /// bridges these with ITS OWN scope — the render point's chain, which includes every `$`-set
    /// between creator and renderer — exactly as the interpreter renders children with the chain
    /// at the render point. The creator's args and stashed children keep `fx.args()` and a
    /// forwarding `children()` meaning what they meant at the site that wrote the thunk.
    Compiled {
        thunks: &'k [crate::surface::ChildThunk<'k>],
        /// The creator's bound args — the thunk's `fx.args()`.
        args: &'k [Value],
        /// The creator's own stashed children, one level up — what a thunk's `children()` renders.
        children: &'k CallChildren<'a, 'k>,
        /// The creator's dispatch island — a thunk's `cube()` resolves where the creator's would.
        home_island: usize,
    },
}

impl CallChildren<'_, '_> {
    /// How many geometry children the call site supplied — `$children`.
    fn count(&self) -> usize {
        match self {
            Self::Stmts { stmts, .. } => stmts.len(),
            Self::Compiled { thunks, .. } => thunks.len(),
        }
    }
}

/// The evaluator state one compiled module call sees.
pub(super) struct NativeModuleCtx<'a, 'c, 'k> {
    /// Arguments already bound to parameters by the evaluator's own two-phase rule — the
    /// AN.1/AN.2/AN.6 semantics a native must not reimplement.
    pub(super) args: Vec<Value>,
    /// The call-site children and how to render them.
    pub(super) children: CallChildren<'a, 'k>,
    /// The module's own call scope — where its `$`-vars resolve.
    pub(super) call_scope: std::cell::RefCell<Scope>,
    /// The island this module's OWN calls resolve against: where it was DEFINED, not where it was
    /// called from. Separate from the children's island (which lives on [`CallChildren::Stmts`])
    /// because they are genuinely different questions — a library module calling `cube` must find
    /// the library's `cube`, while its caller's children still render at the caller's site. One
    /// field used to answer both, which happened to work only while every test defined everything
    /// in one file.
    pub(super) home_island: usize,
    pub(super) ctx: &'c super::Ctx<'a>,
}

impl<'a> NativeModuleCtx<'a, '_, '_> {
    /// Render the children at `picked`, unioned — the one place both child shapes resolve.
    fn render(&self, picked: &[usize]) -> crate::Result<Geo> {
        if picked.is_empty() {
            return Ok(Geo::D3(GeoNode::Empty));
        }
        match &self.children {
            CallChildren::Stmts {
                stmts,
                assigns,
                scope,
                island,
            } => {
                // The site's LEXICAL scope with the CURRENT dynamic `$`-context overlaid by
                // reference — `call_frame`, never a copy. L.2.7: copying the reaching `$`-context
                // per call is what cost 42 clones a call once BOSL2 sets its top-level vars, and a
                // native that reintroduced it would be that bug in compiled form.
                let child_scope = Scope::call_frame(scope, &self.call_scope.borrow());
                // Child-block assignments first: their bindings must reach every geometry child.
                let mut render: Vec<&'a Stmt> = assigns.clone();
                render.extend(picked.iter().filter_map(|&i| stmts.get(i).copied()));
                let global = self.ctx.island_globals.borrow()[*island].clone();
                let parts = super::geo_stack::eval_geometry_driver(
                    &render,
                    &child_scope,
                    &global,
                    *island,
                    self.ctx,
                )?;
                // Union, matching the implicit grouping a child block gets in the interpreter.
                Ok(super::union_of(parts, self.ctx))
            }
            CallChildren::Compiled {
                thunks,
                args,
                children,
                home_island,
            } => {
                // AR.22's tag-family lesson (found by the OpenSCAD differential, not a tier
                // test): the thunk must NOT run against the creator's own dynamic context. The
                // interpreter renders children with the chain AT THE RENDER POINT — `hide()`
                // setting `$tags_hidden` and then rendering the children `diff()` handed it means
                // the cuboid inside those children SEES the hidden-set. Running `thunk(*caller)`
                // read the CREATOR's chain instead, silently dropping every `$`-frame between
                // creator and renderer, and `diff()` unioned what it should have subtracted. So:
                // bridge — the creator's STRUCTURE (args, stashed children, home island) with
                // THIS ctx's dynamic scope, which is the render point's chain by construction.
                let bridged = NativeModuleCtx {
                    args: args.to_vec(),
                    children: (*children).clone(),
                    call_scope: std::cell::RefCell::new(self.call_scope.borrow().clone()),
                    home_island: *home_island,
                    ctx: self.ctx,
                };
                let mut parts = Vec::with_capacity(picked.len());
                for &i in picked {
                    if let Some(thunk) = thunks.get(i) {
                        parts.push(thunk(&bridged)?);
                    }
                }
                Ok(super::union_of(parts, self.ctx))
            }
        }
    }

    /// A call whose callee is a BUILTIN, not a user module — AR.20.6.
    ///
    /// Two shapes, and the split is the interpreter's own: a name that COMBINES its children
    /// (transforms, booleans, hull, the extrudes, offset, projection, resize, color, the no-op
    /// groupings) resolves to a `Combinator` and applies it to the rendered children; everything
    /// else is a PRIMITIVE, which takes no children at all. Both destinations are reached through
    /// the same functions `dispatch_module` uses — `combinator_for` and `eval_primitive` — because
    /// two name tables that agree until somebody edits one is exactly how a compiled `translate`
    /// would quietly become a union and still RENDER.
    ///
    /// A primitive's children are dropped WITHOUT being rendered, matching the interpreter: it never
    /// schedules them, so `cube(1) { sphere(); }` is a cube and the sphere costs nothing. Running the
    /// thunks here would invent side effects the interpreted program does not have.
    fn call_builtin(
        &self,
        name: &str,
        args: &[(Option<&'static str>, Value)],
        children: Children<'_>,
    ) -> crate::Result<Geo> {
        // The statement-form names `dispatch_module` owns arms for can never be dispatched as
        // builtin MODULES — the emitter intercepts every one — so a name that leaks through (a
        // future emitter hole; statement `echo` was exactly this before its arm existed, found by
        // the AR.20 recon) must DECLINE to the interpreter rather than reach `eval_primitive`'s
        // unknown-module arm and mis-render with a spurious warning.
        if matches!(
            name,
            "echo" | "assert" | "let" | "for" | "intersection_for" | "children"
        ) {
            return Err(crate::Error::Unimplemented(
                "a statement-form module name reached compiled dispatch",
            ));
        }
        // The same partition `module::eval_args` makes: positional, named, and `$`-args into a child
        // scope. No parameter matching, because a builtin has no declared parameter list to match
        // against — its binding tables are the evaluator's.
        let mut child_scope = self.call_scope.borrow().child();
        let mut positional = Vec::new();
        let mut named = std::collections::BTreeMap::new();
        for (n, v) in args {
            match n {
                Some(n) if n.starts_with('$') => child_scope.bind(*n, v.clone()),
                Some(n) => {
                    named.insert((*n).to_string(), v.clone());
                }
                None => positional.push(v.clone()),
            }
        }

        if super::geo_stack::combinator_name(name) {
            let parts = match children {
                Children::None => Vec::new(),
                Children::Compiled(thunks) => thunks
                    .iter()
                    .map(|thunk| thunk(self))
                    .collect::<crate::Result<Vec<Geo>>>()?,
            };
            let comb = super::geo_stack::combinator_for(name, &positional, &named, &child_scope);
            return Ok(comb.apply(parts, self.ctx));
        }
        Ok(super::module::eval_primitive(
            name,
            &positional,
            &named,
            &child_scope,
            self.ctx,
        ))
    }

    /// The child indices a `children(i)` / `children([i:j])` / `children([a,b])` selects.
    ///
    /// The evaluator's own index rules, not a reimplementation: a number picks one, a list picks
    /// several, a range picks a span, and anything out of range picks NOTHING rather than erroring —
    /// which is what upstream does and what `children()` in the interpreter does.
    fn indices(&self, selector: &Value) -> Vec<usize> {
        let n = self.child_count();
        let keep = |i: usize| (i < n).then_some(i);
        match selector {
            Value::Num(i) => super::child_at(*i).and_then(keep).into_iter().collect(),
            Value::NumList(xs) => xs
                .iter()
                .filter_map(|&i| super::child_at(i).and_then(keep))
                .collect(),
            Value::Range { start, step, end } => super::value::range_iter(*start, *step, *end)
                .filter_map(|i| super::child_at(i).and_then(keep))
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl ModuleCtx for NativeModuleCtx<'_, '_, '_> {
    fn args(&self) -> &[Value] {
        &self.args
    }

    fn child_count(&self) -> usize {
        self.children.count()
    }

    fn child(&self, i: usize) -> crate::Result<Geo> {
        if i >= self.child_count() {
            // Out of range renders nothing, matching upstream — not an error.
            return Ok(Geo::D3(GeoNode::Empty));
        }
        self.render(&[i])
    }

    fn child_at(&self, selector: &Value) -> crate::Result<Geo> {
        self.render(&self.indices(selector))
    }

    fn children(&self) -> crate::Result<Geo> {
        let all: Vec<usize> = (0..self.child_count()).collect();
        self.render(&all)
    }

    fn group(&self, parts: Vec<Geo>) -> Geo {
        // The interpreter's OWN grouping, so 2D/3D partitioning and the mixing warning are
        // inherited rather than reimplemented.
        super::union_of(parts, self.ctx)
    }

    fn dollar(&self, name: &str) -> Value {
        // Read THROUGH the chain rather than handing one over: see the L.2.7 note on `render`.
        self.call_scope.borrow().lookup(name)
    }

    fn set_dollar(&self, name: &'static str, value: Value) {
        // AR.22 — the hoisted in-body `$x = …`, bound into THIS call's frame exactly where
        // `hoist_scope` binds it: below any enclosing capture's boundary (so the memo's read-set
        // stays caller-facing, structurally — the BU.8 shape), and on the dynamic chain every
        // later callee and rendered child inherits. `bind` routes the `$`-name to specials and
        // mints a fresh dyn_ctx node, so scopes cloned BEFORE this set keep their capture-time
        // view — the interpreter's own CoW rule.
        self.call_scope.borrow_mut().bind(name, value);
    }

    fn echo(&self, args: &[(Option<&'static str>, Value)]) -> crate::Result<()> {
        // The interpreter's own pair-shaped formatter core, pushed through the ONE ordered
        // message log — content only, no `ECHO: ` prefix (`Message::render` adds it), so the
        // echo/warning interleave the I.5 gate string-compares survives untouched.
        let pairs: Vec<(Option<&str>, &Value)> = args.iter().map(|(n, v)| (*n, v)).collect();
        let line = super::format_echo_pairs(&pairs)?;
        self.ctx
            .messages
            .borrow_mut()
            .push(super::Message::Echo(line));
        Ok(())
    }

    fn call(&self, call: &ModuleCall<'_>) -> crate::Result<Geo> {
        let &ModuleCall {
            name,
            args,
            children,
        } = call;
        // AR.20.5 — dispatch, which is what makes this a compiler rather than an interpreter with
        // extra steps. chotchki: "I really want dispatch, otherwise we're making an interpreter
        // with extra steps."
        //
        // The setup below MIRRORS `push_user_module` deliberately and in its order, because the two
        // tiers have to be indistinguishable: same recursion verdict, same argument matching, same
        // bookkeeping binds, same instantiation stack. Where a step is shared it is CALLED rather
        // than copied.
        //
        // DECLINING IS SAFE HERE, and that is load-bearing rather than incidental: `try_native_module`
        // treats `Unimplemented` out of a native as a decline and re-runs the whole call interpreted,
        // the module twin of AR.10. So the gaps below cost speed, never an answer.
        let Some((def, home, base)) = self.ctx.resolve_module(self.home_island, name) else {
            // No USER module by that name, so the call is a BUILTIN — `cube`, `translate`, `union`,
            // which is what every leaf module is made of.
            return self.call_builtin(name, args, children);
        };
        let (params, body) = def;

        // The interpreter's recursion bound, with its verdict class — this guard is a semantic
        // limit on runaway recursion (AD.5), not a missing feature, so it must not read as one.
        let depth = self.ctx.module_depth.get();
        if depth >= super::MAX_MODULE_DEPTH {
            return Err(crate::Error::Eval(format!(
                "Recursion detected calling module '{name}'"
            )));
        }

        // Which argument fills which parameter slot, decided HERE against the parameters the name
        // actually resolved to — never against what the emitter assumed. See `ModuleCall`.
        let owned: Vec<(Option<std::rc::Rc<str>>, Value)> = args
            .iter()
            .map(|(n, v)| (n.map(std::rc::Rc::from), v.clone()))
            .collect();
        let (slots, dollars, diagnostics) =
            super::fill_slots(params, owned.iter().map(|(n, v)| (n.as_ref(), v.clone())));
        // AN.14 — the arg diagnostics belong to whichever path BOUND the call, and this path did.
        for d in diagnostics {
            self.ctx.warn(d);
        }

        // Args bind in the CALLER's scope; the body's lexical base is the callee's HOME island
        // global (or its captured defining scope, for a scope-local def).
        let home_global = base.unwrap_or_else(|| self.ctx.island_globals.borrow()[home].clone());
        let mut call = bind_values(params, &slots, &dollars, &self.call_scope.borrow(), &home_global, self.ctx)?;
        super::geo_stack::warn_params_overwritten(params, body, self.ctx);
        let n_children = match children {
            Children::None => 0,
            Children::Compiled(thunks) => thunks.len(),
        };
        super::bind_call_bookkeeping(&mut call, n_children, self.ctx);

        let native = if self.ctx.config.intrinsics {
            super::intrinsics::resolve_module(name, params, body, &home_global)
        } else {
            None
        };

        // The fast path, and the reason this is dispatch rather than task emission: another native.
        // A compiled child block can only be handed over HERE, because a native carries its children
        // as thunks while the interpreter's frame is statement-shaped.
        //
        // NOT memoized: `push_user_module` consults the CSG cache at this point, and skipping a
        // cache can only cost time, never correctness. AR.20.5 leaves it out rather than porting the
        // eligibility fence to a second call site while the ABI is still moving.
        if let Some(native) = native
            && let Some(_ticket) = ModuleDepthGuard::enter()
        {
            let _frame = ModuleStackGuard::push(name, self.ctx);
            let inner = NativeModuleCtx {
                args: bound_args(params, &call),
                children: match children {
                    Children::None => CallChildren::Stmts {
                        stmts: Vec::new(),
                        assigns: Vec::new(),
                        scope: self.call_scope.borrow().clone(),
                        island: home,
                    },
                    Children::Compiled(thunks) => CallChildren::Compiled {
                        thunks,
                        args: &self.args,
                        children: &self.children,
                        home_island: self.home_island,
                    },
                },
                call_scope: std::cell::RefCell::new(call),
                home_island: home,
                ctx: self.ctx,
            };
            return native(&inner);
        }

        // INTERPRETED, which also covers a compiled callee that ran out of depth budget. The
        // interpreter's children frame holds `&Stmt` and its driver renders children by scheduling
        // work-stack tasks, so a non-empty thunk block has nowhere to go: decline (AR.20.7) rather
        // than drop it, because a wrapper's children ARE its output. Counted so the keep-the-
        // decline decision stays MEASURED on real armed models (`FAB_DEPTH=1` reports it).
        if let Children::Compiled(thunks) = children
            && !thunks.is_empty()
        {
            self.ctx
                .native_child_declines
                .set(self.ctx.native_child_declines.get() + 1);
            return Err(crate::Error::Unimplemented(
                "a compiled child block handed to an interpreted module (AR.20.7)",
            ));
        }
        // The same three frames `push_user_module` sets up, held by RAII so an error out of the body
        // cannot leave the stack unbalanced — the recursive shape here has no `PopModuleFrame` task
        // to lean on.
        let _depth = ModuleDepthTicket::enter(self.ctx);
        let _frame = ModuleStackGuard::push(name, self.ctx);
        let _kids = ChildrenFrameGuard::push(
            super::ChildrenFrame {
                stmts: Vec::new(),
                assigns: Vec::new(),
                scope: self.call_scope.borrow().clone(),
                island: home,
            },
            self.ctx,
        );
        let parts = super::geo_stack::eval_geometry_driver(
            std::slice::from_ref(&body),
            &call,
            &home_global,
            home,
            self.ctx,
        )?;
        // The body's statements union, which is what a module body means.
        Ok(super::union_of(parts, self.ctx))
    }

    // AR.14.4.3 — a compiled module's FUNCTION calls dispatch at runtime, mirroring
    // `dispatch_call`'s resolution ladder rung for rung. This is what makes the band shadow-proof
    // (a user `function sin(x)=0` resolves HERE, where `rt::bi::sin` would have baked the real
    // builtin) and what dissolves the sibling-function decline: the callee is whatever the
    // program defines, fingerprint-free, the same AN.10-safe design module dispatch uses.
    fn call_fn(&self, fcall: &crate::surface::FnCall<'_>) -> crate::Result<Value> {
        let ctx = self.ctx;
        let name = fcall.name;
        // Rung 1 (AD.1): an inner-scope binding holding a function VALUE shadows a named function
        // in call position. The emitter declines the statically-visible case (a param or local by
        // this name); a runtime injection — an unknown named arg bound into the call frame — can
        // still land one here, so DECLINE and let the whole call re-interpret.
        if self.call_scope.borrow().lookup_local_function(name).is_some() {
            return Err(crate::Error::Unimplemented(
                "a function value shadows a compiled body's callee (AD.1) — re-interpreting",
            ));
        }
        // Rung 2: the island's user functions — which SHADOW builtins, per OpenSCAD.
        if let Some(&((params, body), home)) = ctx.functions.get(name) {
            // The interpreter's AD.2 runaway guard, with its verdict class. In-flight count, not
            // host depth: the nested machine below keeps its own body evals heap-shaped, so this
            // counter is the only thing bounding native-driven recursion chains.
            let depth = ctx.live_calls.get() + 1;
            if depth > super::MAX_CALL_DEPTH {
                return Err(crate::Error::Eval(format!(
                    "Recursion detected calling function '{name}'"
                )));
            }
            ctx.live_calls.set(depth);
            let _live = LiveCallTicket(ctx);
            let owned: Vec<(Option<std::rc::Rc<str>>, Value)> = fcall
                .args
                .iter()
                .map(|(n, v)| (n.map(std::rc::Rc::from), v.clone()))
                .collect();
            let (slots, dollars, diagnostics) =
                super::fill_slots(params, owned.iter().map(|(n, v)| (n.as_ref(), v.clone())));
            for d in diagnostics {
                ctx.warn(d);
            }
            // Bind through the SAME two-phase binder the module ABI uses (`bind_values`): the
            // lexical base is the callee's home-island global, the dynamic parent this module's
            // call scope — so the body reads the caller's reaching `$`-context, as interpreted.
            let home_global = ctx.island_globals.borrow()[home].clone();
            let call = bind_values(params, &slots, &dollars, &self.call_scope.borrow(), &home_global, ctx)?;
            return super::eval_with_ctx(body, &call, ctx);
        }
        // Rung 3: the file-value builtins resolve paths off the calling FILE's directory binding —
        // rare in a module body, and the flat ABI has no story for it. Decline, loudly.
        if super::FileFnKind::from_name(name).is_some() {
            return Err(crate::Error::Unimplemented(
                "a file-reading builtin in a compiled body — re-interpreting",
            ));
        }
        // Rung 4: builtins, by DECLARED capability (AS.2) — the same partition `run_builtin`
        // dispatches on, `Named` included: the wrappers take bare name-slices, and a `FnCall`
        // carries every written name.
        if super::builtins::is_builtin(name) {
            let vals: Vec<Value> = fcall.args.iter().map(|(_, v)| v.clone()).collect();
            return match super::builtins::context_impl(name) {
                Some(super::builtins::ContextImpl::Named(f)) => {
                    let names: Vec<Option<&str>> =
                        fcall.args.iter().map(|(n, _)| *n).collect();
                    Ok(f(&names, &vals, &mut ctx.messages.borrow_mut()))
                }
                Some(super::builtins::ContextImpl::Stream(f)) => {
                    Ok(f(&vals, &mut ctx.rand_stream.borrow_mut()))
                }
                Some(super::builtins::ContextImpl::Stack(f)) => {
                    // The stack read is invisible to the eval-memo's key — mark impure, as
                    // `run_builtin` does, so an enclosing memoizable call declines to store.
                    ctx.impure_reads.set(ctx.impure_reads.get() + 1);
                    Ok(f(&vals, &ctx.module_stack.borrow()))
                }
                None => Ok(super::builtins::apply(name, &vals)),
            };
        }
        // Rung 5: not a function, not a builtin. An UNBOUND name warns and answers `undef`,
        // message-identical to the interpreter (L.5.7). A name bound to a VALUE (a top-level
        // closure — callable upstream — or a plain non-function) takes the interpreter's
        // `CallValue` machinery, which v1 does not mirror: decline.
        if matches!(self.call_scope.borrow().lookup(name), Value::Undef) {
            ctx.warn(format!("Ignoring unknown function '{name}'"));
            return Ok(Value::Undef);
        }
        Err(crate::Error::Unimplemented(
            "a value-typed callee in a compiled body — re-interpreting",
        ))
    }
}

/// RAII decrement for [`Ctx::live_calls`] — the balance an interpreted `Task::Apply` gets from
/// `Task::CallReturn`. Held across the body eval in [`ModuleCtx::call_fn`] so an `Err` out of the
/// body (an assert, the budget) cannot leave the in-flight count inflated.
struct LiveCallTicket<'c, 'a>(&'c super::Ctx<'a>);

impl Drop for LiveCallTicket<'_, '_> {
    fn drop(&mut self) {
        self.0.live_calls.set(self.0.live_calls.get() - 1);
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

/// Bind a callee's call scope from slot-matched, ALREADY-EVALUATED arguments.
///
/// AR.20.8. This is `bind_module_scope`'s rule with the evaluation already done, and it follows that
/// function step for step ON PURPOSE — the earlier version took a positional list and bound in ONE
/// interleaved pass, which is exactly the trap the interpreter carries a comment about falling into
/// once already.
///
/// OpenSCAD binds in TWO phases: every default first, in declaration order, then the passed
/// arguments over them. The ordering is load-bearing when a parameter NAME is DUPLICATED — BOSL2's
/// `rounding_edge_mask` lists `r` twice, once defaultless — because the unfilled second `r` writes
/// `undef` in phase 1 and the explicit `r=2` overwrites it in phase 2. Phase 1 also SKIPS a name an
/// argument will fill, or one an earlier duplicate already took, since defaults are
/// first-declared-wins: `module m(l, r, ang=90, d, r=0)` called as `m(l=1)` leaves `r` undef, not 0.
/// The default is still EVALUATED before being dropped, because its side effects (an `echo`, a
/// seedless `rands` draw) happen in declaration order and that is the one thing we cannot move.
///
/// # Errors
/// Whatever evaluating a default raises. Propagated rather than folded to `undef`: a default is
/// arbitrary library code, and swallowing its failure would bind a plausible wrong value.
fn bind_values<'a>(
    params: &'a [crate::parser::Parameter],
    slots: &[Option<Value>],
    dollars: &[(std::rc::Rc<str>, Value)],
    caller: &Scope,
    global: &Scope,
    ctx: &super::Ctx<'a>,
) -> crate::Result<Scope> {
    // Lexically a child of the callee's home global (hygiene), dynamically a child of the caller —
    // inheriting the `$`-context BY REFERENCE. L.2.7: copying it per call is the 42-clones bug.
    let mut call = Scope::call_frame(global, caller);
    let provided: std::collections::BTreeSet<&str> = params
        .iter()
        .zip(slots)
        .filter(|(_, slot)| slot.is_some())
        .map(|(p, _)| &*p.name)
        .collect();
    let mut set: Vec<&str> = Vec::with_capacity(params.len());
    // Phase 1 — defaults, evaluated in the callee's lexical BASE rather than the growing call scope.
    for (param, slot) in params.iter().zip(slots) {
        if slot.is_none() {
            let value = match &param.default {
                Some(default) => super::eval_with_ctx(default, global, ctx)?,
                // AN.3: an unfilled defaultless parameter is `undef` and must NOT fall through to a
                // like-named global.
                None => Value::Undef,
            };
            if !provided.contains(&*param.name) && !set.contains(&&*param.name) {
                set.push(&param.name);
                call.bind(std::rc::Rc::clone(&param.name), value);
            }
        }
    }
    // Phase 2 — the passed arguments override, in declaration order.
    for (param, slot) in params.iter().zip(slots) {
        if let Some(value) = slot {
            call.bind(std::rc::Rc::clone(&param.name), value.clone());
        }
    }
    // `$`-args bind LAST, so they shadow the inherited dynamic context.
    for (n, v) in dollars {
        call.bind(std::rc::Rc::clone(n), v.clone());
    }
    Ok(call)
}
