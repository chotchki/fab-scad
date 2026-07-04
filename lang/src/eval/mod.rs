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
mod ops;
mod scope;
mod value;

pub use fragments::fragments;
pub use scope::Scope;
pub use value::Value;

use crate::parser::{BinOp, Expr, ExprKind, UnOp};

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
                values.push(build_vector(items)?);
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
        ExprKind::Index { .. } => {
            return Err(crate::Error::Unimplemented(
                "list indexing is not yet implemented (I.1)",
            ));
        }
        ExprKind::Member { .. } => {
            return Err(crate::Error::Unimplemented(
                "member access is not yet implemented (I.1)",
            ));
        }
        ExprKind::Range { .. } => {
            return Err(crate::Error::Unimplemented(
                "ranges are not yet implemented (I.1)",
            ));
        }
    }
    Ok(())
}

/// Build a vector value. v0 supports the all-numeric fast path only; heterogeneous/nested vectors
/// are I.1.
fn build_vector(items: Vec<Value>) -> crate::Result<Value> {
    let mut nums = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Num(n) => nums.push(n),
            _ => {
                return Err(crate::Error::Unimplemented(
                    "non-numeric / nested vectors are not yet implemented (I.1)",
                ));
            }
        }
    }
    Ok(Value::num_list(nums))
}
