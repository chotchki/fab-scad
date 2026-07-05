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

use crate::Mesh;
use crate::parser::{BinOp, Expr, ExprKind, Program, Stmt, StmtKind, UnOp};

/// One step on the evaluator's explicit work-stack.
enum Task<'a> {
    /// Evaluate this expression, pushing its result onto the value stack.
    Eval(&'a Expr),
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
    /// Pop the condition, then schedule the taken branch.
    Ternary { then: &'a Expr, els: &'a Expr },
}

/// Evaluate an expression to a [`Value`] on the explicit stack.
///
/// # Errors
/// [`Error::Unimplemented`](crate::Error::Unimplemented) for constructs deferred past v0 (function
/// calls, indexing, member access, ranges, heterogeneous/nested vectors).
pub fn eval_expr(root: &Expr, scope: &Scope) -> crate::Result<Value> {
    let mut tasks: Vec<Task<'_>> = vec![Task::Eval(root)];
    let mut values: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Eval(e) => eval_node(e, scope, &mut tasks, &mut values)?,
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
            Task::Ternary { then, els } => {
                let cond = values.pop().unwrap_or(Value::Undef);
                tasks.push(Task::Eval(if cond.is_truthy() { then } else { els }));
            }
        }
    }
    Ok(values.pop().unwrap_or(Value::Undef))
}

/// Dispatch one AST node: leaves push a value directly; composites push their sub-tasks (children
/// first, so they evaluate before the combining task).
fn eval_node<'a>(
    e: &'a Expr,
    scope: &Scope,
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
            tasks.push(Task::Eval(operand));
        }
        ExprKind::Binary { op, lhs, rhs } => {
            tasks.push(Task::Binary(*op));
            tasks.push(Task::Eval(rhs));
            tasks.push(Task::Eval(lhs)); // popped (and evaluated) first
        }
        ExprKind::Ternary { cond, then, els } => {
            tasks.push(Task::Ternary { then, els });
            tasks.push(Task::Eval(cond));
        }
        ExprKind::Vector(elems) => {
            tasks.push(Task::Vector(elems.len()));
            for el in elems.iter().rev() {
                tasks.push(Task::Eval(el)); // reversed pushes → forward evaluation order
            }
        }
        ExprKind::Call { .. } => {
            return Err(crate::Error::Unimplemented(
                "function calls are not yet implemented (I.4)",
            ));
        }
        ExprKind::Index { base, index } => {
            tasks.push(Task::Index);
            tasks.push(Task::Eval(index));
            tasks.push(Task::Eval(base)); // popped (and evaluated) first → base under index
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
            tasks.push(Task::Eval(end));
            if let Some(step) = step {
                tasks.push(Task::Eval(step));
            }
            tasks.push(Task::Eval(start));
        }
        ExprKind::FunctionLiteral { .. } | ExprKind::Let { .. } => {
            return Err(crate::Error::Unimplemented(
                "function-literal / let expressions are not yet implemented (I.2)",
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
    let mut scope = scope.clone();
    let mut meshes = Vec::new();
    for stmt in &program.stmts {
        eval_stmt(stmt, &mut scope, &mut meshes)?;
    }
    match meshes.len() {
        0 => Ok(Mesh::new()),
        1 => Ok(meshes.pop().unwrap_or_default()),
        _ => Err(crate::Error::Unimplemented(
            "multiple top-level objects (implicit union) are not yet implemented (J.2)",
        )),
    }
}

/// Statement recursion is bounded by the parser's `MAX_DEPTH`, so host recursion here can't overflow
/// (unlike unbounded EXPRESSION recursion, which the explicit stack handles).
fn eval_stmt(stmt: &Stmt, scope: &mut Scope, meshes: &mut Vec<Mesh>) -> crate::Result<()> {
    match &stmt.kind {
        StmtKind::Empty => {}
        StmtKind::Assignment { name, value } => {
            let value = eval_expr(value, scope)?;
            scope.bind(name.clone(), value);
        }
        StmtKind::Block(stmts) => {
            for stmt in stmts {
                eval_stmt(stmt, scope, meshes)?;
            }
        }
        StmtKind::Module(mi) => meshes.push(module::eval_module(mi, scope)?),
        StmtKind::ModuleDef { .. } | StmtKind::FunctionDef { .. } => {
            return Err(crate::Error::Unimplemented(
                "user-defined modules and functions are not yet implemented (I.2)",
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
