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

mod fragments;
mod geometry;
mod module;
mod ops;
mod scope;
mod trig;
mod value;

pub use fragments::fragments;
pub use scope::Scope;
pub use value::{RANGE_MAX, RangeIter, Value, range_iter, range_len};

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::Mesh;
use crate::parser::{Arg, BinOp, Expr, ExprKind, Parameter, Program, Stmt, StmtKind, UnOp};

/// The evaluation context, borrowed from the `Program`:
/// - `functions`: the user-function store (name → params + body). Functions live in their OWN
///   namespace (separate from variables), so a call resolves by name — which is why recursion and
///   mutual recursion work regardless of scope. Built once per program (`build_ctx`).
/// - `closures`: function-literal VALUES registered as they evaluate (indexed by [`Value::Function`]'s
///   `closure_id`). `&'a` AST refs, so a [`Value`] holding a `closure_id` stays `'static`.
#[derive(Default)]
pub(super) struct Ctx<'a> {
    functions: BTreeMap<&'a str, (&'a [Parameter], &'a Expr)>,
    closures: RefCell<Vec<(&'a [Parameter], &'a Expr)>>,
}

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
    /// Pop `n` values and build a vector from them.
    Vector(usize),
    /// Pop the index then the base, apply `base[index]`.
    Index,
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
    /// Push an `undef` — the value of an unfilled, defaultless parameter slot.
    PushUndef,
}

/// Evaluate an expression to a [`Value`] on the explicit stack.
///
/// # Errors
/// [`Error::Unimplemented`](crate::Error::Unimplemented) for constructs deferred past v0 (function
/// calls, indexing, member access, ranges, heterogeneous/nested vectors).
pub fn eval_expr(root: &Expr, scope: &Scope) -> crate::Result<Value> {
    eval_with_ctx(root, scope, &Ctx::default())
}

/// Evaluate an expression with a function-store [`Ctx`] in scope (so calls resolve). `scope` doubles
/// as the LEXICAL base for function bodies: a call's body evaluates in `global.child()` + its params,
/// NOT the caller's locals (OpenSCAD functions are lexically scoped; `$`-var dynamic override is I.2.2).
pub(super) fn eval_with_ctx<'a>(
    root: &'a Expr,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Value> {
    let global = scope.clone();
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
            Task::Vector(n) => {
                let items = values.split_off(values.len().saturating_sub(n));
                values.push(build_vector(items));
            }
            Task::Index => {
                // index was pushed after base, so it's on top.
                let index = values.pop().unwrap_or(Value::Undef);
                let base = values.pop().unwrap_or(Value::Undef);
                values.push(ops::index(base, &index));
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
            Task::PushUndef => values.push(Value::Undef),
        }
    }
    Ok(values.pop().unwrap_or(Value::Undef))
}

/// Dispatch one AST node: leaves push a value directly; composites push their sub-tasks (children
/// first, so they evaluate before the combining task).
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
            tasks.push(Task::Vector(elems.len()));
            for el in elems.iter().rev() {
                tasks.push(Task::Eval(el, scope.clone())); // reversed pushes → forward eval order
            }
        }
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind {
                // a named user function (its own namespace) resolves first.
                if let Some(&(params, body)) = ctx.functions.get(name.as_str()) {
                    push_call(params, body, args, scope, global, tasks);
                    return Ok(());
                }
                // an UNBOUND identifier callee is a builtin (or genuinely unknown) → LOUD (I.4).
                if matches!(scope.lookup(name), Value::Undef) {
                    return Err(crate::Error::Unimplemented(
                        "call to a builtin or unknown function is not yet implemented (I.4)",
                    ));
                }
            }
            // otherwise the callee is a value — evaluate it, then apply (a closure in a var, `(e)(a)`).
            tasks.push(Task::CallValue {
                args,
                caller: scope.clone(),
            });
            tasks.push(Task::Eval(callee, scope.clone()));
        }
        ExprKind::Index { base, index } => {
            tasks.push(Task::Index);
            tasks.push(Task::Eval(index, scope.clone()));
            tasks.push(Task::Eval(base, scope.clone())); // evaluated first → base under index
        }
        ExprKind::Member { .. } => {
            return Err(crate::Error::Unimplemented(
                "member access is not yet implemented (I.1)",
            ));
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
        ExprKind::Let { .. } => {
            return Err(crate::Error::Unimplemented(
                "let expressions are not yet implemented (I.3)",
            ));
        }
        ExprKind::Assert { .. } | ExprKind::Echo { .. } => {
            return Err(crate::Error::Unimplemented(
                "assert / echo expressions are not yet implemented (I.5)",
            ));
        }
        ExprKind::LcFor { .. }
        | ExprKind::LcForC { .. }
        | ExprKind::LcEach(_)
        | ExprKind::LcIf { .. } => {
            return Err(crate::Error::Unimplemented(
                "list comprehensions are not yet implemented (I.3)",
            ));
        }
    }
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
    let ctx = build_ctx(program);
    let mut scope = scope.clone();
    let mut meshes = Vec::new();
    for stmt in &program.stmts {
        eval_stmt(stmt, &mut scope, &ctx, &mut meshes)?;
    }
    match meshes.len() {
        0 => Ok(Mesh::new()),
        1 => Ok(meshes.pop().unwrap_or_default()),
        _ => Err(crate::Error::Unimplemented(
            "multiple top-level objects (implicit union) are not yet implemented (J.2)",
        )),
    }
}

/// Collect user function definitions into the [`Ctx`] store (their own namespace). A pre-pass over the
/// whole program, so a call can resolve a function defined anywhere (whole-program visibility, like
/// OpenSCAD); a duplicate name — last definition wins (`BTreeMap::insert`).
fn build_ctx(program: &Program) -> Ctx<'_> {
    let mut functions = BTreeMap::new();
    for stmt in &program.stmts {
        if let StmtKind::FunctionDef { name, params, body } = &stmt.kind {
            functions.insert(name.as_str(), (params.as_slice(), body));
        }
    }
    Ctx {
        functions,
        closures: RefCell::default(),
    }
}

/// Statement recursion is bounded by the parser's `MAX_DEPTH`, so host recursion here can't overflow
/// (unlike unbounded EXPRESSION recursion, which the explicit stack handles).
fn eval_stmt<'a>(
    stmt: &'a Stmt,
    scope: &mut Scope,
    ctx: &Ctx<'a>,
    meshes: &mut Vec<Mesh>,
) -> crate::Result<()> {
    match &stmt.kind {
        // `Empty`: nothing. `FunctionDef`: already registered into `ctx` by `build_ctx` (function
        // definitions live in their own namespace) — nothing to evaluate here.
        StmtKind::Empty | StmtKind::FunctionDef { .. } => {}
        StmtKind::Assignment { name, value } => {
            let value = eval_with_ctx(value, scope, ctx)?;
            scope.bind(name.clone(), value);
        }
        StmtKind::Block(stmts) => {
            for stmt in stmts {
                eval_stmt(stmt, scope, ctx, meshes)?;
            }
        }
        StmtKind::Module(mi) => meshes.push(module::eval_module(mi, scope, ctx)?),
        StmtKind::ModuleDef { .. } => {
            return Err(crate::Error::Unimplemented(
                "user-defined modules are not yet implemented (I.2.4)",
            ));
        }
        StmtKind::If { .. } => {
            return Err(crate::Error::Unimplemented(
                "if/else evaluation is not yet implemented (I.3)",
            ));
        }
        StmtKind::Use(_) | StmtKind::Include(_) => {
            return Err(crate::Error::Unimplemented(
                "use/include resolution is not yet implemented (I.2 loader)",
            ));
        }
    }
    Ok(())
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
}
