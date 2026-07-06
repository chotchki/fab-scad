//! The scad-rs evaluator (v0 skeleton).
//!
//! Expression evaluation runs on an EXPLICIT STACK — no host recursion, so evaluation depth is
//! bounded by the heap (the task/value `Vec`s), not the call stack. This is where the SPEC's "the
//! Safari class of failure becomes structurally impossible" actually lands, and it's the sibling of
//! the parser's non-recursive `Drop`. (I.7's Kani proofs target this machine's push/pop discipline.)
//!
//! v0 scope: the expression subset producing [`Value`] v0 (`Num`/`Bool`/`Str`/`NumList`/`Undef`),
//! plus `$fn`/`$fa`/`$fs` → fragment resolution. Functions, indexing, member access, ranges, and
//! heterogeneous/nested vectors fail LOUD ([`Error::Unimplemented`](crate::Error::Unimplemented)) —
//! I.1/I.4. Arithmetic/undef semantics are bug-for-bug OpenSCAD (`ops`).

mod builtins;
mod fmt;
mod fragments;
mod geo;
mod geometry;
mod loader;
mod message;
mod module;
mod ops;
mod scope;
mod trig;
mod value;

pub use fragments::fragments;
pub use geo::GeoNode;
pub use message::{Evaluation, Message};
pub use scope::Scope;
pub use value::{RANGE_MAX, RangeIter, Value, range_iter, range_len};

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use crate::Mesh;
use crate::parser::{
    Arg, BinOp, Expr, ExprKind, ModuleInstantiation, Parameter, Program, Stmt, StmtKind, UnOp,
};

/// The evaluation context, borrowed from the `Program`:
/// - `functions`: the user-function store (name → params + body). Functions live in their OWN
///   namespace (separate from variables), so a call resolves by name — which is why recursion and
///   mutual recursion work regardless of scope. Built once per program (`build_ctx`).
/// - `closures`: function-literal VALUES registered as they evaluate (indexed by [`Value::Function`]'s
///   `closure_id`). `&'a` AST refs, so a [`Value`] holding a `closure_id` stays `'static`.
/// - `messages`: `echo`/warning console output, accumulated in EMISSION order (I.5) — a shared buffer
///   because echo can fire deep in an expression, not just at a statement. Extracted into
///   [`Evaluation`] at the end; the mesh-only `evaluate*` sugar drops it.
#[derive(Default)]
pub(super) struct Ctx<'a> {
    functions: BTreeMap<&'a str, (&'a [Parameter], &'a Expr)>,
    /// User MODULE definitions (their own namespace, whole-program visibility) — name → (params, body
    /// statement). A module CALL resolves here before the builtin-primitive fallthrough (I.2.4).
    modules: loader::ModStore<'a>,
    closures: RefCell<Vec<(&'a [Parameter], &'a Expr)>>,
    messages: RefCell<Vec<Message>>,
    /// Live user-module call depth — the Safari-cliff guard. Statement eval is HOST-recursive (a module
    /// body re-enters `eval_stmt`), so a self-recursive module could overflow; this bounds it, LOUD
    /// ([`MAX_MODULE_DEPTH`]), never a silent stack crash.
    module_depth: Cell<usize>,
    /// The children-frame STACK for `children()` (I.2.5): each active module call pushes its call-site
    /// children + the caller's scope, so a `children()` in the body renders them LATE-bound. A stack, so
    /// nested module calls each see their own children; `children()` pops during eval so a `children()`
    /// inside the rendered children refers to the ENCLOSING call, not this one.
    children_stack: RefCell<Vec<ChildrenFrame<'a>>>,
}

/// One active module call's children context: the call-site child statements (borrowed from the AST) +
/// the CALLER's scope they evaluate in (OpenSCAD renders `children()` in the instantiation context).
struct ChildrenFrame<'a> {
    stmts: &'a [Stmt],
    scope: Scope,
}

/// Max nested user-module call depth before we bail LOUD (OpenSCAD caps recursion similarly). Statement
/// recursion is host-stack-bound — unlike the EXPRESSION machine (explicit stack, memory-bound) — so
/// this guard is what keeps a `module m() { m(); }` from crashing the process.
const MAX_MODULE_DEPTH: usize = 256;

/// One step on the evaluator's explicit work-stack. Each `Eval` carries the [`Scope`] it evaluates
/// in (an `Rc<Frame>` clone — cheap), so a call's body can evaluate in the callee's scope while the
/// caller's continuation waits on the same stack (I.2.3). Value-combining tasks need no scope.
enum Task<'a> {
    /// Evaluate this expression in this scope, pushing its result onto the value stack.
    Eval(&'a Expr, Scope),
    /// Pop two values, apply the binary op, push the result.
    Binary(BinOp),
    /// Pop one value, apply the unary op, push the result.
    Unary(UnOp),
    /// Pop one value per element and build a vector — a COMPREHENSION element's value is SPLICED (its
    /// list's elements appended), a plain element is appended as one.
    VectorSplice(&'a [Expr]),
    /// Pop the index then the base, apply `base[index]`.
    Index,
    /// Pop the base, apply member access `base.field` (`.x`/`.y`/`.z` → index 0/1/2).
    Member(&'a str),
    /// Pop end, (step if `stepped`), start; build a range value.
    Range { stepped: bool },
    /// Pop the condition, then schedule the taken branch (in `scope`).
    Ternary {
        then: &'a Expr,
        els: &'a Expr,
        scope: Scope,
    },
    /// Pop `names.len()` values and bind them (params, then `$`-args) into a fresh child of `base`,
    /// seeded first with the CALLER's dynamic `$`-context, then evaluate `body` in that call scope. The
    /// heart of a call — no host recursion, so recursion depth is bounded by the heap (`corner_brace`).
    Apply {
        names: Vec<&'a str>,
        body: &'a Expr,
        base: Scope,
        caller: Scope,
    },
    /// Pop an evaluated CALLEE; if it's a [`Value::Function`], invoke it (its body evaluates in the
    /// captured env). Anything else → `undef` (calling a non-function). The dynamic-callee path:
    /// `(expr)(args)`, or a variable holding a closure.
    CallValue { args: &'a [Arg], caller: Scope },
    /// Pop the builtin's argument values, split into positional/named, and apply the builtin `name`.
    Builtin { name: &'a str, args: &'a [Arg] },
    /// Pop the just-evaluated binding value, bind it as `name` in a child of `scope`, then either
    /// evaluate the next `let` binding in that scope or (no bindings left) evaluate `body`. `let`
    /// bindings are SEQUENTIAL — a later one sees the earlier ones.
    LetStep {
        name: &'a str,
        rest: &'a [Arg],
        body: &'a Expr,
        scope: Scope,
    },
    /// Push an `undef` — the value of an unfilled, defaultless parameter slot.
    PushUndef,
    /// Short-circuit a `&&`/`||`: the LHS is on the value stack. `||` on a TRUTHY LHS yields `true` and
    /// `&&` on a FALSY LHS yields `false` — the RHS is NEVER evaluated (so its asserts / recursion don't
    /// run). Otherwise the RHS is evaluated and combined with the LHS by the normal op. This is
    /// load-bearing for OpenSCAD: BOSL2 guards recursion base-cases + assertions behind `a || b` / `a &&
    /// b`, so eager evaluation makes guarded asserts fire and guarded recursion never terminate.
    ShortCircuit {
        op: BinOp,
        rhs: &'a Expr,
        scope: Scope,
    },
}

/// Evaluate an expression to a [`Value`] on the explicit stack.
///
/// # Errors
/// [`Error::Unimplemented`](crate::Error::Unimplemented) for constructs deferred past v0 (function
/// calls, indexing, member access, ranges, heterogeneous/nested vectors).
pub fn eval_expr(root: &Expr, scope: &Scope) -> crate::Result<Value> {
    eval_with_ctx(root, scope, &Ctx::default())
}

/// Evaluate an expression with a function-store [`Ctx`] in scope (so calls resolve). At the top level
/// the lexical `global` (the base for function bodies) IS the eval scope.
pub(super) fn eval_with_ctx<'a>(
    root: &'a Expr,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Value> {
    eval_with_global(root, scope, scope, ctx)
}

/// Evaluate `root` in `scope`, with `global` as the LEXICAL base for any function body called during
/// it (a call's body evaluates in `global.child()` + its params, NOT the caller's locals — OpenSCAD
/// functions are lexically scoped; `$`-var dynamic override is I.2.2). `global` is threaded (not
/// re-derived from `scope`) so a nested eval — a comprehension body carrying loop variables — still
/// resolves function bodies against the TOP-LEVEL globals, not the loop scope.
#[allow(
    clippy::too_many_lines,
    reason = "the explicit-stack work-loop: one match arm per Task variant — splitting it would just \
    scatter the machine across helpers that each need the shared tasks/values stacks"
)]
fn eval_with_global<'a>(
    root: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Value> {
    let global = global.clone();
    let mut tasks: Vec<Task<'a>> = vec![Task::Eval(root, scope.clone())];
    let mut values: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Eval(e, s) => eval_node(e, &s, &global, ctx, &mut tasks, &mut values)?,
            Task::Binary(op) => {
                // pop order: rhs was pushed after lhs, so it's on top.
                let rhs = values.pop().unwrap_or(Value::Undef);
                let lhs = values.pop().unwrap_or(Value::Undef);
                values.push(ops::apply_binary(op, lhs, rhs));
            }
            Task::Unary(op) => {
                let v = values.pop().unwrap_or(Value::Undef);
                values.push(ops::apply_unary(op, v));
            }
            Task::VectorSplice(elems) => {
                let vals = values.split_off(values.len().saturating_sub(elems.len()));
                let mut out = Vec::new();
                for (elem, val) in elems.iter().zip(vals) {
                    if is_comprehension(elem) {
                        splice_into(val, &mut out);
                    } else {
                        out.push(val);
                    }
                }
                values.push(build_vector(out));
            }
            Task::Index => {
                // index was pushed after base, so it's on top.
                let index = values.pop().unwrap_or(Value::Undef);
                let base = values.pop().unwrap_or(Value::Undef);
                values.push(ops::index(base, &index));
            }
            Task::Member(field) => {
                let base = values.pop().unwrap_or(Value::Undef);
                values.push(ops::member(base, field));
            }
            Task::Range { stepped } => {
                // pushed start, [step], end → pop end, [step], start.
                let end = values.pop().unwrap_or(Value::Undef);
                let step = if stepped {
                    values.pop().unwrap_or(Value::Undef)
                } else {
                    Value::Num(1.0)
                };
                let start = values.pop().unwrap_or(Value::Undef);
                values.push(build_range(&start, &step, &end));
            }
            Task::Ternary { then, els, scope } => {
                let cond = values.pop().unwrap_or(Value::Undef);
                let branch = if cond.is_truthy() { then } else { els };
                tasks.push(Task::Eval(branch, scope));
            }
            Task::Apply {
                names,
                body,
                base,
                caller,
            } => {
                let vals = values.split_off(values.len().saturating_sub(names.len()));
                let mut call = base.child();
                // dynamic $-context: inherit the caller's reaching $-vars BEFORE the call's own
                // bindings, so a call's $-args (bound below) override the inherited ones.
                for (name, value) in caller.specials() {
                    call.bind(name, value);
                }
                for (name, value) in names.iter().zip(vals) {
                    call.bind(*name, value);
                }
                tasks.push(Task::Eval(body, call));
            }
            Task::CallValue { args, caller } => {
                match values.pop().unwrap_or(Value::Undef) {
                    Value::Function { closure_id, env } => {
                        let (params, body) = ctx.closures.borrow()[closure_id];
                        // a closure's body is lexically scoped to its captured env, not the caller's.
                        push_call(params, body, args, &caller, &env, &mut tasks);
                    }
                    _ => values.push(Value::Undef), // calling a non-function → undef
                }
            }
            Task::LetStep {
                name,
                rest,
                body,
                scope,
            } => {
                let value = values.pop().unwrap_or(Value::Undef);
                let mut inner = scope.child();
                inner.bind(name, value);
                match rest.split_first() {
                    Some((next, remaining)) => {
                        tasks.push(Task::LetStep {
                            name: next.name.as_deref().unwrap_or("_"),
                            rest: remaining,
                            body,
                            scope: inner.clone(),
                        });
                        tasks.push(Task::Eval(&next.value, inner));
                    }
                    None => tasks.push(Task::Eval(body, inner)),
                }
            }
            Task::Builtin { name, args } => run_builtin(name, args, &mut values),
            Task::PushUndef => values.push(Value::Undef),
            Task::ShortCircuit { op, rhs, scope } => {
                let lhs = values.pop().unwrap_or(Value::Undef);
                let or = matches!(op, BinOp::Or);
                if lhs.is_truthy() == or {
                    values.push(Value::Bool(or)); // `||` on truthy → true; `&&` on falsy → false
                } else {
                    // Not short-circuited: evaluate the RHS and combine it with the kept LHS.
                    values.push(lhs);
                    tasks.push(Task::Binary(op));
                    tasks.push(Task::Eval(rhs, scope));
                }
            }
        }
    }
    Ok(values.pop().unwrap_or(Value::Undef))
}

/// Dispatch one AST node: leaves push a value directly; composites push their sub-tasks (children
/// first, so they evaluate before the combining task).
#[allow(
    clippy::too_many_lines,
    reason = "the expression-node dispatch: one match arm per ExprKind — a cohesive jump table, not \
    separable without threading the tasks stack through every helper"
)]
fn eval_node<'a>(
    e: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
    tasks: &mut Vec<Task<'a>>,
    values: &mut Vec<Value>,
) -> crate::Result<()> {
    match &e.kind {
        ExprKind::Num(n) => values.push(Value::Num(*n)),
        ExprKind::Bool(b) => values.push(Value::Bool(*b)),
        ExprKind::Undef => values.push(Value::Undef),
        ExprKind::Str(s) => values.push(Value::string(s.as_str())),
        ExprKind::Ident(name) => values.push(scope.lookup(name)),
        ExprKind::Unary { op, operand } => {
            tasks.push(Task::Unary(*op));
            tasks.push(Task::Eval(operand, scope.clone()));
        }
        // `&&` / `||` SHORT-CIRCUIT (OpenSCAD semantics): evaluate the LHS, then a `ShortCircuit` task
        // decides whether the RHS runs at all — so a guarded assert or recursion behind it stays guarded.
        ExprKind::Binary {
            op: op @ (BinOp::And | BinOp::Or),
            lhs,
            rhs,
        } => {
            tasks.push(Task::ShortCircuit {
                op: *op,
                rhs,
                scope: scope.clone(),
            });
            tasks.push(Task::Eval(lhs, scope.clone()));
        }
        ExprKind::Binary { op, lhs, rhs } => {
            tasks.push(Task::Binary(*op));
            tasks.push(Task::Eval(rhs, scope.clone()));
            tasks.push(Task::Eval(lhs, scope.clone())); // popped (and evaluated) first
        }
        ExprKind::Ternary { cond, then, els } => {
            tasks.push(Task::Ternary {
                then,
                els,
                scope: scope.clone(),
            });
            tasks.push(Task::Eval(cond, scope.clone()));
        }
        ExprKind::Vector(elems) => {
            tasks.push(Task::VectorSplice(elems));
            for el in elems.iter().rev() {
                tasks.push(Task::Eval(el, scope.clone())); // reversed pushes → forward eval order
            }
        }
        ExprKind::Call { callee, args } => dispatch_call(callee, args, scope, global, ctx, tasks)?,
        ExprKind::Index { base, index } => {
            tasks.push(Task::Index);
            tasks.push(Task::Eval(index, scope.clone()));
            tasks.push(Task::Eval(base, scope.clone())); // evaluated first → base under index
        }
        ExprKind::Member { base, field } => {
            tasks.push(Task::Member(field));
            tasks.push(Task::Eval(base, scope.clone())); // base evaluated first, then `.field`
        }
        ExprKind::Range { start, step, end } => {
            // pushed so start evaluates first (bottom of the value stack), end last (top).
            tasks.push(Task::Range {
                stepped: step.is_some(),
            });
            tasks.push(Task::Eval(end, scope.clone()));
            if let Some(step) = step {
                tasks.push(Task::Eval(step, scope.clone()));
            }
            tasks.push(Task::Eval(start, scope.clone()));
        }
        ExprKind::FunctionLiteral { params, body } => {
            // register the literal's &'a params + body in the closure table; the VALUE holds just the
            // index + the captured env, so it stays 'static.
            let closure_id = {
                let mut closures = ctx.closures.borrow_mut();
                closures.push((params.as_slice(), body.as_ref()));
                closures.len() - 1
            };
            values.push(Value::Function {
                closure_id,
                env: scope.clone(),
            });
        }
        ExprKind::Let { bindings, body } => match bindings.split_first() {
            Some((first, rest)) => {
                tasks.push(Task::LetStep {
                    name: first.name.as_deref().unwrap_or("_"),
                    rest,
                    body,
                    scope: scope.clone(),
                });
                tasks.push(Task::Eval(&first.value, scope.clone()));
            }
            None => tasks.push(Task::Eval(body, scope.clone())), // `let() body` → just the body
        },
        ExprKind::Echo { args, body } => {
            // `echo(args) body?` — emit the ECHO line (side effect), then yield `body` (or undef). The
            // args + body sub-evaluate off the stack (bounded, like comprehensions); echo is rare + cold.
            emit_echo(args, scope, global, ctx)?;
            let value = match body {
                Some(b) => eval_with_global(b, scope, global, ctx)?,
                None => Value::Undef,
            };
            values.push(value);
        }
        ExprKind::Assert { args, body } => {
            // `assert(cond, msg?) body?` — LOUD on a falsy condition, else yield `body` (or undef).
            check_assert(args, scope, global, ctx)?;
            let value = match body {
                Some(b) => eval_with_global(b, scope, global, ctx)?,
                None => Value::Undef,
            };
            values.push(value);
        }
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => {
            // a comprehension element evaluates to its CONTRIBUTION list (spliced by the enclosing
            // VectorSplice). Only reached as a vector element (parser invariant).
            let contribution = eval_comprehension(e, scope, global, ctx)?;
            values.push(build_vector(contribution));
        }
    }
    Ok(())
}

/// Pop a builtin call's argument values, split them into positional/named, and push the builtin result.
fn run_builtin(name: &str, args: &[Arg], values: &mut Vec<Value>) {
    // A benchmark span per builtin application (I.6); `builtin` field lets a layer break cost down by
    // name. All the tracing spans sit at TRACE level — the "compile-out-like-a-logger" doctrine.
    let _span = tracing::trace_span!("builtin", builtin = name).entered();
    let vals = values.split_off(values.len().saturating_sub(args.len()));
    let mut positional = Vec::new();
    let mut named = BTreeMap::new();
    for (arg, value) in args.iter().zip(vals) {
        match &arg.name {
            Some(n) => {
                named.insert(n.clone(), value);
            }
            None => positional.push(value),
        }
    }
    values.push(builtins::apply(name, &positional, &named));
}

/// Dispatch a call `callee(args)`: a NAMED user function (own namespace) resolves first; an UNBOUND
/// identifier callee is a builtin or genuinely unknown → LOUD (I.4); otherwise the callee is a value —
/// evaluate it and apply it (a closure in a variable, or `(expr)(args)`).
fn dispatch_call<'a>(
    callee: &'a Expr,
    args: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
    tasks: &mut Vec<Task<'a>>,
) -> crate::Result<()> {
    if let ExprKind::Ident(name) = &callee.kind {
        // resolution order (OpenSCAD): a user function may shadow a builtin.
        if let Some(&(params, body)) = ctx.functions.get(name.as_str()) {
            // A call-path EVENT, not a span: the call's body evaluates across later loop iterations on
            // the explicit stack (no host recursion), so its subtree isn't scope-bounded here — the
            // event marks WHICH function was entered, the enclosing `eval_program` span times the whole.
            tracing::trace!(function = name.as_str(), "call");
            push_call(params, body, args, scope, global, tasks);
            return Ok(());
        }
        if builtins::is_builtin(name) {
            tasks.push(Task::Builtin { name, args });
            for arg in args.iter().rev() {
                tasks.push(Task::Eval(&arg.value, scope.clone()));
            }
            return Ok(());
        }
        if matches!(scope.lookup(name), Value::Undef) {
            // not a user fn, not a builtin, not a bound function-value → an unimplemented builtin or a
            // typo. LOUD for now (catches missing builtins); OpenSCAD's warn-and-undef is I.5.
            return Err(crate::Error::Unimplemented(
                "call to an unimplemented builtin or unknown function (I.4)",
            ));
        }
    }
    tasks.push(Task::CallValue {
        args,
        caller: scope.clone(),
    });
    tasks.push(Task::Eval(callee, scope.clone()));
    Ok(())
}

/// Push the tasks for a function call (a named user function OR a closure): one value-source per
/// parameter — an arg expr (in the CALLER scope), a default (in the lexical `base` scope), or `undef` —
/// then an [`Task::Apply`] that binds them and evaluates the body. `base` is the lexical base of the
/// body: the top-level `global` for a named function, the captured `env` for a closure. OpenSCAD
/// arg-matching: positional args fill params left-to-right, named args fill by name (extra/unknown args
/// are dropped). Two documented first-cut simplifications: `$`-arg injection is I.2.2, and defaults
/// evaluate in the definition scope, not the partially-bound call scope (so a default can't reference
/// an earlier param — rare; defaults are usually constants).
fn push_call<'a>(
    params: &'a [Parameter],
    body: &'a Expr,
    args: &'a [Arg],
    caller: &Scope,
    base: &Scope,
    tasks: &mut Vec<Task<'a>>,
) {
    let mut slots: Vec<Option<(&'a Expr, Scope)>> = vec![None; params.len()];
    let mut dollars: Vec<(&'a str, &'a Expr)> = Vec::new(); // $-args → dynamic $-var injections
    let mut positional = 0;
    for arg in args {
        match &arg.name {
            None => {
                if let Some(slot) = slots.get_mut(positional) {
                    *slot = Some((&arg.value, caller.clone()));
                }
                positional += 1;
            }
            // a $-arg is a per-call dynamic override — injected into the call scope, not param-matched.
            Some(name) if name.starts_with('$') => dollars.push((name.as_str(), &arg.value)),
            Some(name) => {
                if let Some(i) = params.iter().position(|p| &p.name == name)
                    && let Some(slot) = slots.get_mut(i)
                {
                    *slot = Some((&arg.value, caller.clone()));
                }
            }
        }
    }
    for (slot, param) in slots.iter_mut().zip(params) {
        if let (None, Some(default)) = (&slot, &param.default) {
            *slot = Some((default, base.clone()));
        }
    }
    // bind order: params first, then $-args (bound last → they override the inherited $-context).
    let mut names: Vec<&'a str> = params.iter().map(|p| p.name.as_str()).collect();
    names.extend(dollars.iter().map(|(name, _)| *name));
    tasks.push(Task::Apply {
        names,
        body,
        base: base.clone(),
        caller: caller.clone(),
    });
    // push evals so the popped run is [params.., dollars..]: dollars first (deeper → on top), then
    // params reversed (param 0 evaluates first, lands at the bottom of the run).
    for (_, expr) in dollars.iter().rev() {
        tasks.push(Task::Eval(expr, caller.clone()));
    }
    for slot in slots.into_iter().rev() {
        match slot {
            Some((expr, scope)) => tasks.push(Task::Eval(expr, scope)),
            None => tasks.push(Task::PushUndef),
        }
    }
}

/// Is this expression a list-comprehension element (spliced into the enclosing vector) rather than a
/// plain element (appended as one)? `let` in a vector is a comprehension-`let`.
fn is_comprehension(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::LcFor { .. }
            | ExprKind::LcForC { .. }
            | ExprKind::LcEach(_)
            | ExprKind::LcIf { .. }
            | ExprKind::Let { .. }
    )
}

/// Splice a comprehension element's value into the vector accumulator: a list contributes its
/// elements; a scalar (e.g. `each 5`) contributes itself.
fn splice_into(val: Value, out: &mut Vec<Value>) {
    match val {
        Value::NumList(xs) => out.extend(xs.iter().map(|&x| Value::Num(x))),
        Value::List(xs) => out.extend(xs.iter().cloned()),
        other => out.push(other),
    }
}

/// The values a `for`/`each` iterable yields: a list's elements, a range's values (capped by
/// `range_iter`), a string's characters, or a scalar as a single value.
fn iter_values(v: &Value) -> Vec<Value> {
    match v {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        Value::Range { start, step, end } => {
            range_iter(*start, *step, *end).map(Value::Num).collect()
        }
        Value::Str(s) => s.chars().map(|c| Value::string(c.to_string())).collect(),
        other => vec![other.clone()],
    }
}

/// Evaluate a comprehension element to its CONTRIBUTION — the values it splices into the enclosing
/// vector. A plain expr contributes `[value]`; `for`/`each`/`if`/`let` flatmap/splice/filter/scope.
///
/// Comprehension NESTING is parse-bounded (`MAX_DEPTH`), so this bounded host recursion can't overflow;
/// iteration is capped (`RANGE_MAX`, list length). Each sub-expression re-enters the explicit-stack
/// evaluator carrying the TOP-LEVEL `global` (so a function called in a body resolves against globals,
/// not the loop scope) — a fresh stack per step; folding it onto one explicit stack is a deferred perf
/// optimization, and the element-cap WARNING text is I.5.
fn eval_comprehension<'a>(
    elem: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    match &elem.kind {
        ExprKind::LcFor { bindings, body } => lc_for(bindings, body, scope, global, ctx),
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => lc_for_c(init, cond, update, body, scope, global, ctx),
        ExprKind::LcEach(e) => Ok(iter_values(&eval_with_global(e, scope, global, ctx)?)),
        ExprKind::LcIf { cond, then, els } => {
            if eval_with_global(cond, scope, global, ctx)?.is_truthy() {
                eval_comprehension(then, scope, global, ctx)
            } else {
                match els {
                    Some(e) => eval_comprehension(e, scope, global, ctx),
                    None => Ok(Vec::new()),
                }
            }
        }
        ExprKind::Let { bindings, body } => {
            let inner = comprehension_let_scope(bindings, scope, global, ctx)?;
            eval_comprehension(body, &inner, global, ctx)
        }
        _ => Ok(vec![eval_with_global(elem, scope, global, ctx)?]), // a plain element → [value]
    }
}

/// `for (name = iterable, …) body` — iterate each binding (multiple bindings NEST), evaluate `body`'s
/// contribution per step, concatenate.
fn lc_for<'a>(
    bindings: &'a [Arg],
    body: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    match bindings.split_first() {
        None => eval_comprehension(body, scope, global, ctx),
        Some((binding, rest)) => {
            let var = binding.name.as_deref().unwrap_or("_");
            let iterable = eval_with_global(&binding.value, scope, global, ctx)?;
            let mut out = Vec::new();
            for value in iter_values(&iterable) {
                let mut inner = scope.child();
                inner.bind(var, value);
                out.extend(lc_for(rest, body, &inner, global, ctx)?);
            }
            Ok(out)
        }
    }
}

/// C-style `for (init; cond; update) body`: the loop variables live in a flat map (each iteration a
/// fresh `scope.child()`, so no chain accumulation), `cond`/`update` see the current values, and
/// `update` MERGES into them (unmentioned vars persist). Capped at `RANGE_MAX` iterations.
fn lc_for_c<'a>(
    init: &'a [Arg],
    cond: &'a Expr,
    update: &'a [Arg],
    body: &'a Expr,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<Value>> {
    let mut vars: Vec<(String, Value)> = Vec::new();
    for arg in init {
        let name = arg.name.as_deref().unwrap_or("_").to_string();
        let value = eval_with_global(&arg.value, scope, global, ctx)?;
        vars.push((name, value));
    }
    let mut out = Vec::new();
    let mut iterations = 0u64;
    loop {
        let mut loop_scope = scope.child();
        for (name, value) in &vars {
            loop_scope.bind(name.clone(), value.clone());
        }
        if !eval_with_global(cond, &loop_scope, global, ctx)?.is_truthy() {
            break;
        }
        out.extend(eval_comprehension(body, &loop_scope, global, ctx)?);
        for arg in update {
            let name = arg.name.as_deref().unwrap_or("_");
            let value = eval_with_global(&arg.value, &loop_scope, global, ctx)?;
            match vars.iter_mut().find(|(n, _)| n == name) {
                Some(entry) => entry.1 = value,
                None => vars.push((name.to_string(), value)),
            }
        }
        iterations += 1;
        if iterations >= RANGE_MAX {
            // The runaway-`for(i=0; 1; …)` guard. Reaching it needs RANGE_MAX (1e7) real iterations, so
            // it's the single line the corpus can't cover — a defensive limit, equivalent-mutant class.
            // (Eval isn't under the parser/lexer mandatory-100% rule; the warning TEXT is I.5.)
            break;
        }
    }
    Ok(out)
}

/// Bind a comprehension `let`'s bindings SEQUENTIALLY (a later one sees the earlier), returning the
/// extended scope in which the `let` body's contribution is then evaluated.
fn comprehension_let_scope<'a>(
    bindings: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Scope> {
    let mut s = scope.clone();
    for binding in bindings {
        let name = binding.name.as_deref().unwrap_or("_");
        let value = eval_with_global(&binding.value, &s, global, ctx)?;
        let mut next = s.child();
        next.bind(name, value);
        s = next;
    }
    Ok(s)
}

/// Build a vector value: the all-numeric `NumList` fast path when every element is a number, else the
/// general heterogeneous `List`. The two compare EQUAL element-for-element (see `Value`'s `PartialEq`).
fn build_vector(items: Vec<Value>) -> Value {
    match items.iter().map(as_num).collect::<Option<Vec<f64>>>() {
        Some(nums) => Value::num_list(nums),
        None => Value::list(items),
    }
}

/// A number's `f64`, else `None` — the all-numeric test for the `NumList` fast path.
fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(*n),
        _ => None,
    }
}

/// Build a range value from its (already-evaluated) bounds — non-numeric bounds make the whole range
/// `undef` (OpenSCAD requires numeric range bounds).
fn build_range(start: &Value, step: &Value, end: &Value) -> Value {
    match (start, step, end) {
        (&Value::Num(start), &Value::Num(step), &Value::Num(end)) => {
            Value::Range { start, step, end }
        }
        _ => Value::Undef,
    }
}

/// Evaluate a whole program to a [`Mesh`] — the tracer-bullet spine's tail. Assignments bind into
/// the scope; a single top-level object produces its mesh.
///
/// # Errors
/// Deferred constructs fail LOUD: unknown modules / transforms / booleans (module eval), and
/// multiple top-level objects (implicit union — J.2).
pub fn eval_program(program: &Program, scope: &Scope) -> crate::Result<Mesh> {
    // The top-of-tree benchmark span (I.6): its busy-time is the whole evaluation. Everything below
    // nests under it, so a subscriber can attribute cost to `builtin`/`module` children. TRACE level →
    // free with no subscriber, compiled out in release under `release_max_level_off`.
    let _span = tracing::trace_span!("eval_program").entered();
    let ctx = build_ctx(program);
    mesh_of(run_stmts(program.stmts.iter(), &ctx, scope)?)
}

/// Evaluate `source` with its `use`/`include` graph resolved — the pure-crate spine behind
/// [`evaluate_file`](crate::evaluate_file) / [`evaluate_with_base`](crate::evaluate_with_base). The
/// loader owns every reachable file (so the evaluator's `&'a`-into-the-AST borrows span all of them);
/// we evaluate the flattened statement stream against the merged, precedence-correct function store.
/// `root_path` is the root's own path when it's a file (for back-reference dedup + cycle-break).
///
/// # Errors
/// Loader failures ([`Error::Load`](crate::Error::Load)), parse errors, and any evaluation error from
/// the flattened program.
pub(crate) fn evaluate_source(
    source: &str,
    base_dir: &std::path::Path,
    root_path: Option<&std::path::Path>,
    library_paths: &[std::path::PathBuf],
) -> crate::Result<(GeoNode, Vec<Message>)> {
    let _span = tracing::trace_span!("eval_program").entered();
    let loaded = loader::load(source, base_dir, root_path, library_paths)?;
    let (exec, defs) = loader::flatten(&loaded)?;
    let ctx = Ctx {
        functions: defs.functions,
        modules: defs.modules,
        closures: RefCell::default(),
        messages: RefCell::default(),
        module_depth: Cell::default(),
        children_stack: RefCell::default(),
    };
    let tree = run_stmts(exec.into_iter(), &ctx, &Scope::new())?;
    Ok((tree, ctx.messages.into_inner()))
}

/// Evaluate a statement stream to a geometry TREE ([`GeoNode`]) — shared by [`eval_program`] and the
/// loader path. The result is the implicit union of the top-level objects. The tree keeps fab-lang
/// backend-agnostic: a single primitive is a `Leaf` [`mesh_of`] can flatten with no kernel; anything
/// with a transform or a boolean needs the downstream Manifold backend (J.2).
fn run_stmts<'a>(
    stmts: impl Iterator<Item = &'a Stmt>,
    ctx: &Ctx<'a>,
    scope: &Scope,
) -> crate::Result<GeoNode> {
    let stmts: Vec<&Stmt> = stmts.collect();
    // The top-level hoisted scope IS the GLOBAL base for module bodies (a user module evaluates in
    // `global.child()` + its params — OpenSCAD's lexical hygiene). Hoist ONCE (not a pre-hoist +
    // re-hoist — that would let a forward reference see the pre-bound value, breaking `a = b; b = 5` →
    // `a` is undef), then evaluate the geometry in that same scope.
    let global = hoist_scope(&stmts, scope, ctx)?;
    Ok(union_of(eval_geometry(&stmts, &global, &global, ctx)?))
}

/// The geometry nodes a statement list produces, in order. Pass 1 HOISTS every assignment BEFORE any
/// geometry (OpenSCAD's whole-scope, last-assignment-wins rule), evaluating them in first-occurrence
/// order so a forward or self-referential reference sees `undef` (`sphere(x); x = 5;` → sphere(5);
/// `n = 1; n = n + 1;` → undef — verified against the oracle). Pass 2 runs the geometry statements with
/// the fully-bound scope. Shared by the top level, bare blocks, and every transform/boolean's children
/// (each gets a fresh hoisted child scope). Recursion is bounded by the parser's `MAX_DEPTH`, so the
/// geometry tree can't be deep enough to overflow (unlike the expression stack, which is explicit).
fn eval_nodes<'a>(
    stmts: &[&'a Stmt],
    ctx: &Ctx<'a>,
    scope: &Scope,
    global: &Scope,
) -> crate::Result<Vec<GeoNode>> {
    let hoisted = hoist_scope(stmts, scope, ctx)?;
    eval_geometry(stmts, &hoisted, global, ctx)
}

/// Hoist a statement list's assignments into a fresh working scope (a clone of `scope`): OpenSCAD's
/// whole-scope, last-assignment-wins rule, evaluating them in first-occurrence order so a forward /
/// self-reference sees `undef`. Returns the bound scope — the pure prefix `eval_nodes` and `run_stmts`
/// share. Hoisting into a FRESH scope (nothing pre-bound) is what keeps `a = b; b = 5` → `a` undef.
fn hoist_scope<'a>(stmts: &[&'a Stmt], scope: &Scope, ctx: &Ctx<'a>) -> crate::Result<Scope> {
    let mut scope = scope.clone();
    for (name, expr) in hoisted_assignments(stmts) {
        let value = eval_with_ctx(expr, &scope, ctx)?;
        scope.bind(name.to_string(), value);
    }
    Ok(scope)
}

/// Evaluate the GEOMETRY statements of a list (assignments already hoisted into `scope`) → their nodes,
/// threading `global` unchanged for any module body's lexical base.
fn eval_geometry<'a>(
    stmts: &[&'a Stmt],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Vec<GeoNode>> {
    let mut scope = scope.clone();
    let mut nodes = Vec::new();
    for stmt in stmts {
        if !matches!(stmt.kind, StmtKind::Assignment { .. }) {
            eval_stmt(stmt, &mut scope, global, ctx, &mut nodes)?;
        }
    }
    Ok(nodes)
}

/// Wrap geometry nodes in the implicit union: none → `Empty`, one → itself, many → `Union` (OpenSCAD
/// unions multiple top-level objects + a block's children).
fn union_of(mut nodes: Vec<GeoNode>) -> GeoNode {
    match nodes.len() {
        0 => GeoNode::Empty,
        1 => nodes.pop().unwrap_or(GeoNode::Empty),
        _ => GeoNode::Union(nodes),
    }
}

/// Iterate a `for`/`intersection_for` over its loop-variable ARGS (a Cartesian PRODUCT for multiple
/// vars), evaluating the body's geometry once per binding tuple and pushing each iteration's node.
/// Recursion depth = the number of loop vars (parse-bounded), so it can't overflow.
fn for_product<'a>(
    args: &'a [Arg],
    children: &[&'a Stmt],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
    out: &mut Vec<GeoNode>,
) -> crate::Result<()> {
    match args.split_first() {
        // all vars bound → the body
        None => out.push(union_of(eval_nodes(children, ctx, scope, global)?)),
        Some((arg, rest)) => {
            let name = arg.name.as_deref().unwrap_or("");
            let iterable = eval_with_ctx(&arg.value, scope, ctx)?;
            for value in iterate_values(&iterable) {
                let mut child = scope.clone();
                child.bind(name, value);
                for_product(rest, children, &child, global, ctx, out)?;
            }
        }
    }
    Ok(())
}

/// Call a user MODULE (I.2.4): bind the call's args into a fresh child of `global` (OpenSCAD lexical
/// hygiene — the body sees globals + params, not the caller's locals), then evaluate the body statement
/// there → its geometry (implicit-unioned). Guarded against unbounded self-recursion ([`MAX_MODULE_DEPTH`])
/// because statement eval is HOST-recursive — LOUD on overflow, never a silent stack crash.
fn call_user_module<'a>(
    mi: &'a ModuleInstantiation,
    caller: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<GeoNode> {
    let (params, body) = ctx.modules[mi.name.as_str()]; // the arm guarded `contains_key`
    let depth = ctx.module_depth.get();
    if depth >= MAX_MODULE_DEPTH {
        return Err(crate::Error::Unimplemented(
            "user-module recursion too deep (the statement-eval depth guard — a runaway recursive module)",
        ));
    }
    ctx.module_depth.set(depth + 1);
    let mut call = bind_module_scope(params, &mi.args, caller, global, ctx)?;
    // `$children` = the call-site child count; the children themselves are stashed for `children()` to
    // render LATE, in the CALLER's scope (I.2.5).
    call.bind("$children", Value::Num(child_count(mi.children.len())));
    ctx.children_stack.borrow_mut().push(ChildrenFrame {
        stmts: mi.children.as_slice(),
        scope: caller.clone(),
    });
    let mut nodes = Vec::new();
    let result = eval_stmt(body, &mut call, global, ctx, &mut nodes);
    ctx.children_stack.borrow_mut().pop(); // restore even on error (no `?` before these)
    ctx.module_depth.set(depth);
    result?;
    Ok(union_of(nodes))
}

/// Render `children()` / `children(i)` (I.2.5): the current module call's stashed call-site children,
/// evaluated LATE in the CALLER's scope. `children()` → all; `children(i)` → the i-th; `children([i,j])`
/// → those (out-of-range / negative indices drop). Outside any module call the stash is empty → no
/// geometry. The current frame is POPPED for the duration so a `children()` INSIDE the rendered children
/// refers to the ENCLOSING call (OpenSCAD's late-binding), then restored for the caller's continuation.
fn eval_children<'a>(
    mi: &'a ModuleInstantiation,
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<GeoNode> {
    let (positional, _, _) = module::eval_args(mi, scope, ctx)?;
    let Some(frame) = ctx.children_stack.borrow_mut().pop() else {
        return Ok(GeoNode::Empty); // children() outside a module call → nothing
    };
    let selected: Vec<&Stmt> = match positional.first() {
        None => frame.stmts.iter().collect(), // children() → all
        Some(Value::Num(i)) => child_at(*i)
            .and_then(|i| frame.stmts.get(i))
            .into_iter()
            .collect(),
        Some(Value::NumList(xs)) => xs
            .iter()
            .filter_map(|&i| child_at(i).and_then(|i| frame.stmts.get(i)))
            .collect(),
        _ => Vec::new(),
    };
    let result = eval_nodes(&selected, ctx, &frame.scope, global);
    ctx.children_stack.borrow_mut().push(frame); // restore for the caller's continuation
    Ok(union_of(result?))
}

/// A child count as a `Num` — the child list is tiny, so the `usize → f64` widening is exact.
#[allow(
    clippy::cast_precision_loss,
    reason = "a call's child count is small; the widening is exact"
)]
fn child_count(n: usize) -> f64 {
    n as f64
}

/// A `children(i)` index: a non-negative WHOLE number → its `usize`, else `None` (dropped).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: only a non-negative integer-valued f64 converts; everything else is None"
)]
fn child_at(i: f64) -> Option<usize> {
    (i >= 0.0 && i.fract() == 0.0).then_some(i as usize)
}

/// Build a user module's call scope: match `args` to `params` (positional fill left-to-right, named by
/// name, defaults for the rest), then bind them + the `$`-args into a fresh child of `global`. Mirrors
/// the function-call arg-match ([`push_call`]) but EAGER (statement level, no `Task` machine): arg exprs
/// evaluate in the CALLER scope, defaults in `global` (the definition scope), `$`-args bind LAST so they
/// override the inherited dynamic `$`-context.
fn bind_module_scope<'a>(
    params: &'a [Parameter],
    args: &'a [Arg],
    caller: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Scope> {
    let mut slots: Vec<Option<(&'a Expr, Scope)>> = vec![None; params.len()];
    let mut dollars: Vec<(&'a str, &'a Expr)> = Vec::new();
    let mut positional = 0;
    for arg in args {
        match &arg.name {
            None => {
                if let Some(slot) = slots.get_mut(positional) {
                    *slot = Some((&arg.value, caller.clone()));
                }
                positional += 1;
            }
            Some(name) if name.starts_with('$') => dollars.push((name.as_str(), &arg.value)),
            Some(name) => {
                if let Some(i) = params.iter().position(|p| &p.name == name)
                    && let Some(slot) = slots.get_mut(i)
                {
                    *slot = Some((&arg.value, caller.clone()));
                }
            }
        }
    }
    for (slot, param) in slots.iter_mut().zip(params) {
        if let (None, Some(default)) = (&slot, &param.default) {
            *slot = Some((default, global.clone()));
        }
    }
    let mut call = global.child();
    for (name, value) in caller.specials() {
        call.bind(name, value); // inherit the caller's reaching $-context first
    }
    for (param, slot) in params.iter().zip(&slots) {
        let value = match slot {
            Some((expr, s)) => eval_with_ctx(expr, s, ctx)?,
            None => Value::Undef,
        };
        call.bind(param.name.as_str(), value);
    }
    for (name, expr) in dollars {
        let value = eval_with_ctx(expr, caller, ctx)?;
        call.bind(name, value); // $-args last → override the inherited $-context
    }
    Ok(call)
}

/// The values a `for` binding iterates: a range → its (capped) values, a vector → its elements, a
/// scalar → a single iteration (OpenSCAD's `for(i = 5)`).
fn iterate_values(v: &Value) -> Vec<Value> {
    match v {
        Value::Range { start, step, end } => {
            range_iter(*start, *step, *end).map(Value::Num).collect()
        }
        Value::NumList(xs) => xs.iter().map(|&n| Value::Num(n)).collect(),
        Value::List(xs) => xs.to_vec(),
        other => vec![other.clone()],
    }
}

/// Flatten a geometry tree WITHOUT a backend: `Empty` → an empty mesh, a single `Leaf` → its mesh.
/// Anything with a transform or a boolean needs the Manifold backend (fab-scad), so it errors LOUD —
/// callers reach for [`evaluate_geometry`](crate::evaluate_geometry) + a backend instead.
pub(crate) fn mesh_of(tree: GeoNode) -> crate::Result<Mesh> {
    match tree {
        GeoNode::Empty => Ok(Mesh::new()),
        GeoNode::Leaf(mesh) => Ok(mesh),
        // Color is a display property, not geometry — a colored PRIMITIVE still flattens with no backend.
        GeoNode::Color { child, .. } => mesh_of(*child),
        _ => Err(crate::Error::Unimplemented(
            "geometry with transforms or booleans needs a backend — use evaluate_geometry (J.2)",
        )),
    }
}

/// The hoisted assignment order of a scope, as a PURE function (statements in → ordered `(name, expr)`
/// out, no evaluation, no side effects): a scope's assignments deduped by name in FIRST-occurrence
/// order, each carrying the LAST assignment's expr. Mirrors OpenSCAD's parser (`handle_assignment`
/// overwrites a duplicate's expr in place, keeping its position) feeding `ScopeContext::init`, which
/// evaluates them in that order. The caller evaluates + binds; keeping the ORDER pure makes the
/// last-assignment-wins + forward-ref-is-undef rules unit-testable without a scope.
fn hoisted_assignments<'a>(stmts: &[&'a Stmt]) -> Vec<(&'a str, &'a Expr)> {
    let mut order: Vec<(&'a str, &'a Expr)> = Vec::new();
    let mut index: BTreeMap<&'a str, usize> = BTreeMap::new();
    for stmt in stmts {
        if let StmtKind::Assignment { name, value } = &stmt.kind {
            if let Some(&i) = index.get(name.as_str()) {
                order[i].1 = value; // seen: last expr wins, first-occurrence position kept
            } else {
                index.insert(name.as_str(), order.len());
                order.push((name.as_str(), value));
            }
        }
    }
    order
}

/// Evaluate an `echo`'s arguments and push the formatted `ECHO:` content onto the message log — named
/// args render `name = value`, positional just `value`, joined by `, ` (OpenSCAD's echo order). The
/// value form is the shared [`fmt::format_value`] (strings QUOTED), so it's bug-for-bug with the oracle.
fn emit_echo<'a>(
    args: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        let value = eval_with_global(&arg.value, scope, global, ctx)?;
        parts.push(match &arg.name {
            Some(name) => format!("{name} = {}", fmt::format_value(&value)),
            None => fmt::format_value(&value),
        });
    }
    ctx.messages
        .borrow_mut()
        .push(Message::Echo(parts.join(", ")));
    Ok(())
}

/// Evaluate an `assert`'s arguments and fail LOUD if the condition is falsy: `assert(cond)`,
/// `assert(cond, msg)`, or the named `assert(condition = …, message = …)`. The failure text is NOT
/// matched to the oracle word-for-word (an agreed non-goal); it carries the user's message when given.
fn check_assert<'a>(
    args: &'a [Arg],
    scope: &Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<()> {
    let mut positional = Vec::new();
    let mut named_condition = None;
    let mut named_message = None;
    for arg in args {
        let value = eval_with_global(&arg.value, scope, global, ctx)?;
        match arg.name.as_deref() {
            None => positional.push(value),
            Some("condition") => named_condition = Some(value),
            Some("message") => named_message = Some(value),
            Some(_) => {} // unknown named arg — dropped, as OpenSCAD arg-matching does
        }
    }
    // A named `condition`/`message` beats the positional slot (params are `condition`, then `message`).
    let condition = named_condition.or_else(|| positional.first().cloned());
    let message = named_message.or_else(|| positional.get(1).cloned());
    if condition.is_some_and(|c| c.is_truthy()) {
        return Ok(());
    }
    Err(crate::Error::Eval(match message {
        Some(Value::Str(s)) => format!("assertion failed: {s}"),
        Some(other) => format!("assertion failed: {}", fmt::format_value(&other)),
        None => "assertion failed".to_string(),
    }))
}

/// Collect user function definitions into the [`Ctx`] store (their own namespace). A pre-pass over the
/// whole program, so a call can resolve a function defined anywhere (whole-program visibility, like
/// OpenSCAD); a duplicate name — last definition wins (`BTreeMap::insert`).
fn build_ctx(program: &Program) -> Ctx<'_> {
    let mut functions = BTreeMap::new();
    let mut modules = BTreeMap::new();
    for stmt in &program.stmts {
        match &stmt.kind {
            StmtKind::FunctionDef { name, params, body } => {
                functions.insert(name.as_str(), (params.as_slice(), body));
            }
            StmtKind::ModuleDef { name, params, body } => {
                modules.insert(name.as_str(), (params.as_slice(), &**body));
            }
            _ => {}
        }
    }
    Ctx {
        functions,
        modules,
        closures: RefCell::default(),
        messages: RefCell::default(),
        module_depth: Cell::default(),
        children_stack: RefCell::default(),
    }
}

/// Statement recursion is bounded by the parser's `MAX_DEPTH`, so host recursion here can't overflow
/// (unlike unbounded EXPRESSION recursion, which the explicit stack handles).
fn eval_stmt<'a>(
    stmt: &'a Stmt,
    scope: &mut Scope,
    global: &Scope,
    ctx: &Ctx<'a>,
    nodes: &mut Vec<GeoNode>,
) -> crate::Result<()> {
    match &stmt.kind {
        // Definitions + empties are no-ops at eval. `Empty`: nothing. `FunctionDef`: already registered
        // into `ctx` by `build_ctx` (its own namespace). `ModuleDef`: likewise a registration only —
        // defining an unused module IS nothing, and INSTANTIATING a user module still fails LOUD in
        // `module::eval_module`; that relaxation (from LOUD-on-def) is what lets `use`/`include` load
        // real files, which define modules everywhere (the call machinery is I.2.4 / Phase J).
        // `Assignment` is a no-op HERE: `eval_nodes` hoists every assignment (whole-scope, last-wins)
        // and skips it in the geometry pass, so a bound assignment never reaches `eval_stmt`.
        StmtKind::Empty
        | StmtKind::FunctionDef { .. }
        | StmtKind::ModuleDef { .. }
        | StmtKind::Assignment { .. } => {}
        // A bare `{ … }` block groups its children into ONE implicit-union node, in a fresh child scope
        // (its own hoisting).
        StmtKind::Block(stmts) => {
            let refs: Vec<&Stmt> = stmts.iter().collect();
            nodes.push(union_of(eval_nodes(&refs, ctx, scope, global)?));
        }
        // `echo`/`assert` at statement level are module instantiations, but they produce console
        // output, not geometry — handle them here (no node pushed) before the geometry dispatch. Their
        // geometry CHILDREN (`echo(x) cube();`) ride the module-children machinery (I.2.4 / J); rare.
        StmtKind::Module(mi) if mi.name == "echo" => emit_echo(&mi.args, scope, scope, ctx)?,
        StmtKind::Module(mi) if mi.name == "assert" => check_assert(&mi.args, scope, scope, ctx)?,
        // An affine TRANSFORM wraps the implicit union of its children (J.2). `$`-args don't reach a
        // transform, so its child scope is dropped.
        StmtKind::Module(mi) if geo::is_transform(&mi.name) => {
            let (positional, named, _) = module::eval_args(mi, scope, ctx)?;
            let matrix = geo::transform_matrix(&mi.name, &positional, &named);
            let refs: Vec<&Stmt> = mi.children.iter().collect();
            let child = union_of(eval_nodes(&refs, ctx, scope, global)?);
            nodes.push(GeoNode::Transform {
                matrix,
                child: Box::new(child),
            });
        }
        // A CSG BOOLEAN over its children — each geometry child is an operand (J.2). `difference` is the
        // first minus the rest, `intersection` is the common volume, `union` merges (also the default).
        StmtKind::Module(mi) if geo::is_boolean(&mi.name) => {
            let refs: Vec<&Stmt> = mi.children.iter().collect();
            let children = eval_nodes(&refs, ctx, scope, global)?;
            nodes.push(match mi.name.as_str() {
                "difference" => GeoNode::Difference(children),
                "intersection" => GeoNode::Intersection(children),
                _ => GeoNode::Union(children),
            });
        }
        // `color()` — set the subtree's display color (BOSL2-critical, J.2.8). An INVALID color (unknown
        // name, wrong arg type) INHERITS: no Color node, just the children (OpenSCAD's -1 sentinel).
        StmtKind::Module(mi) if mi.name == "color" => {
            let (positional, named, _) = module::eval_args(mi, scope, ctx)?;
            let refs: Vec<&Stmt> = mi.children.iter().collect();
            let child = union_of(eval_nodes(&refs, ctx, scope, global)?);
            match geo::resolve_color(&positional, &named) {
                Some(color) => nodes.push(GeoNode::Color {
                    color,
                    child: Box::new(child),
                }),
                None => nodes.push(child),
            }
        }
        // `children()` / `children(i)` (I.2.5) — render the enclosing module call's CALL-SITE children,
        // late-bound in the caller's scope. The BOSL2 currency: a wrapper module transforms `children()`.
        StmtKind::Module(mi) if mi.name == "children" => {
            nodes.push(eval_children(mi, scope, global, ctx)?);
        }
        // `for` / `intersection_for`: bind the loop variable(s) over a range/vector, evaluate the body
        // per iteration, and union (or intersect) the results (I.3.3 — the statement half of control
        // flow). Multiple loop vars nest as a product, like the comprehension `for`.
        StmtKind::Module(mi) if mi.name == "for" || mi.name == "intersection_for" => {
            let children: Vec<&Stmt> = mi.children.iter().collect();
            let mut iterations = Vec::new();
            for_product(&mi.args, &children, scope, global, ctx, &mut iterations)?;
            nodes.push(if mi.name == "intersection_for" {
                GeoNode::Intersection(iterations)
            } else {
                GeoNode::Union(iterations)
            });
        }
        // A USER MODULE call (I.2.4): resolve in the module store + bind args into a fresh child of the
        // GLOBAL scope (OpenSCAD hygiene — a module body sees globals + params, NOT the caller's locals),
        // then evaluate its body there. Checked before the builtin fallthrough; a name matching a builtin
        // transform/boolean/color was already dispatched above (so a user module can't shadow those — a
        // documented v1 simplification).
        StmtKind::Module(mi) if ctx.modules.contains_key(mi.name.as_str()) => {
            nodes.push(call_user_module(mi, scope, global, ctx)?);
        }
        // A PRIMITIVE → a `Leaf` (an unknown user module fails LOUD inside `eval_module`).
        StmtKind::Module(mi) => nodes.push(GeoNode::Leaf(module::eval_module(mi, scope, ctx)?)),
        // `if (cond) A else B` contributes the TAKEN branch's geometry (I.3.3).
        StmtKind::If { cond, then, els } => {
            let branch = if eval_with_ctx(cond, scope, ctx)?.is_truthy() {
                then
            } else {
                els
            };
            let refs: Vec<&Stmt> = branch.iter().collect();
            nodes.push(union_of(eval_nodes(&refs, ctx, scope, global)?));
        }
        // The loader resolves top-level `use`/`include` away (include → spliced, use → imported), so a
        // node reaching here is either a raw `eval_program` on an unloaded program or a `use`/`include`
        // NESTED inside a block/module body (not scanned — top-level is the OpenSCAD norm). LOUD.
        StmtKind::Use(_) | StmtKind::Include(_) => {
            return Err(crate::Error::Unimplemented(
                "unresolved use/include (nested, or eval_program on an unloaded program — use evaluate_file/evaluate_with_base)",
            ));
        }
    }
    Ok(())
}

// I.7 — Kani proof of the stack machine's pop-N discipline (docs/testing-cards.md: "push/pop
// discipline", panic-freedom on the exact loop that runs untrusted SCAD). Compiled only under
// `cargo kani`.
#[cfg(kani)]
mod proofs {
    /// The multi-value pops — `VectorSplice` / `Apply` / `Builtin` all do
    /// `values.split_off(values.len().saturating_sub(n))` — can NEVER underflow the value stack: the
    /// split index is always `<= len` (saturating_sub can't wrap below 0), so `split_off` never panics,
    /// for ANY stack depth and ANY requested arity `n`. This is the push/pop discipline's safety core.
    #[kani::proof]
    fn stack_pop_n_never_underflows() {
        let depth: usize = kani::any();
        kani::assume(depth <= 8); // bounded model; the invariant is depth-independent (saturating_sub)
        let mut values: Vec<u8> = vec![0; depth];
        let n: usize = kani::any();
        let popped = values.split_off(values.len().saturating_sub(n)); // must not panic
        assert!(popped.len() <= depth);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "unit-test helpers: unwrap/expect/panic ARE the assertions; exact float asserts are deterministic"
)]
mod tests {
    use super::{Scope, Value, build_ctx, eval_with_ctx};
    use crate::parser::{StmtKind, parse};

    /// Evaluate a program's assignments in order (binding each), returning the LAST assignment's value
    /// — with the program's function store in scope. The end-to-end call test harness.
    fn eval_last(src: &str) -> Value {
        let prog = parse(src).expect("parses");
        let ctx = build_ctx(&prog);
        let mut scope = Scope::new();
        let mut last = Value::Undef;
        for stmt in &prog.stmts {
            if let StmtKind::Assignment { name, value } = &stmt.kind {
                last = eval_with_ctx(value, &scope, &ctx).expect("evaluates");
                scope.bind(name.clone(), last.clone());
            }
        }
        last
    }

    #[test]
    fn positional_named_and_default_args() {
        assert_eq!(
            eval_last("function f(x) = x + 1; y = f(2);"),
            Value::Num(3.0)
        );
        assert_eq!(
            eval_last("function f(x, y = 10) = x + y; a = f(5);"),
            Value::Num(15.0)
        ); // default
        assert_eq!(
            eval_last("function f(x, y = 10) = x + y; a = f(5, 20);"),
            Value::Num(25.0)
        ); // override
        assert_eq!(
            eval_last("function f(a, b) = a - b; y = f(b = 1, a = 10);"),
            Value::Num(9.0)
        ); // named, reordered
        assert_eq!(eval_last("function f(x, y) = y; a = f(1);"), Value::Undef); // unfilled, no default → undef
        assert_eq!(
            eval_last("function f(x) = x; y = f(1, 2, 3);"),
            Value::Num(1.0)
        ); // extra positional dropped
        assert_eq!(
            eval_last("function f(x) = x; y = f(x = 1, z = 9);"),
            Value::Num(1.0)
        ); // unknown named dropped
    }

    #[test]
    fn functions_are_lexically_scoped() {
        assert_eq!(
            eval_last("g = 7; function f() = g; y = f();"),
            Value::Num(7.0)
        ); // sees the global
        // a caller's LOCAL does NOT leak into the callee (lexical, not dynamic): inner sees no `x`.
        assert_eq!(
            eval_last("function inner() = x; function outer(x) = inner(); y = outer(99);"),
            Value::Undef
        );
    }

    #[test]
    fn recursion_and_mutual_recursion() {
        assert_eq!(
            eval_last("function fac(n) = n <= 1 ? 1 : n * fac(n - 1); y = fac(5);"),
            Value::Num(120.0)
        );
        let mutual = "function even(n) = n == 0 ? true : odd(n - 1); \
                      function odd(n) = n == 0 ? false : even(n - 1); \
                      y = even(10);";
        assert_eq!(eval_last(mutual), Value::Bool(true));
    }

    #[test]
    fn closures_capture_their_env_and_are_higher_order() {
        // a closure CAPTURES the scope at its definition (k = 100 is closed over).
        assert_eq!(
            eval_last("k = 100; g = function(x) x + k; y = g(1);"),
            Value::Num(101.0)
        );
        // a closure bound to a variable is called through the variable (the CallValue path).
        assert_eq!(
            eval_last("g = function(x) x * 2; y = g(21);"),
            Value::Num(42.0)
        );
        // higher-order: pass a closure as an argument, call it inside.
        assert_eq!(
            eval_last(
                "function apply(f, x) = f(x); double = function(n) n * 2; y = apply(double, 7);"
            ),
            Value::Num(14.0)
        );
        // calling a NON-function value → undef (not an error).
        assert_eq!(eval_last("g = 5; y = g(1);"), Value::Undef);
    }

    #[test]
    fn dollar_vars_are_dynamically_scoped() {
        // a $-arg injects into the call scope (per-call override), visible in the body.
        assert_eq!(
            eval_last("function f() = $fn; y = f($fn = 8);"),
            Value::Num(8.0)
        );
        // with no override, the callee sees the CALLER's reaching $-context (here the root $fn = 0).
        assert_eq!(eval_last("function f() = $fn; y = f();"), Value::Num(0.0));
        // DOWN the call tree: outer's injected $fn propagates to inner (dynamic, not lexical).
        assert_eq!(
            eval_last("function inner() = $fn; function outer() = inner(); y = outer($fn = 8);"),
            Value::Num(8.0)
        );
        // a nested per-call override WINS over the inherited $-context.
        assert_eq!(
            eval_last(
                "function inner() = $fn; function outer() = inner($fn = 3); y = outer($fn = 8);"
            ),
            Value::Num(3.0)
        );
    }

    #[test]
    fn deep_non_tail_recursion_is_heap_bounded() {
        // The corner_brace-class proof: 100k-deep NON-tail recursion — each level parks a pending `+`
        // on the stack — would blow a recursive tree-walker's HOST stack. On the explicit stack it's
        // just heap. sum(n) = n(n+1)/2, so sum(100000) = 5000050000 (exact in f64).
        let deep = "function sum(n) = n <= 0 ? 0 : n + sum(n - 1); y = sum(100000);";
        assert_eq!(eval_last(deep), Value::Num(5_000_050_000.0));
    }

    #[test]
    fn hoisted_assignments_dedup_first_occurrence_last_expr() {
        // The PURE override resolver: `a = 1; b = 2; a = 3;` → order [a, b] (FIRST-occurrence position),
        // and a carries the LAST expr (3, not 1). This is the whole rule the run_stmts hoist rides on.
        use crate::parser::{ExprKind, Stmt};
        let prog = parse("a = 1; b = 2; a = 3;").expect("parses");
        let stmts: Vec<&Stmt> = prog.stmts.iter().collect();
        let order = super::hoisted_assignments(&stmts);
        assert_eq!(
            order.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(matches!(order[0].1.kind, ExprKind::Num(n) if n == 3.0)); // a's expr is the last (3)
    }
}
