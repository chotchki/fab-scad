use std::hash::{Hash, Hasher};

use crate::parser::{Arg, Expr, ExprKind, Parameter};

/// A structural fingerprint of a function's `(params, body)`: a 64-bit hash over the AST SHAPE — variant
/// discriminants, operators, literal bits (`f64` by `to_bits`, so `NaN`/`±0` are exact), names, and nesting
/// — with SPANS EXCLUDED (a fingerprint is source-formatting-independent; only the structure counts). Two
/// functions fingerprinting equal are structurally identical. A fixed-seed hasher makes it run-reproducible.
///
/// Collision note: a 64-bit hash CAN alias in theory, but the registry pairs the fingerprint with the
/// function NAME and the fast==slow harness proves each registered intrinsic bit-matches its reference — so
/// a collision would have to hit a same-NAMED, harness-verified function to matter, which the harness would
/// itself catch. Fingerprint is a fast pre-filter, not the whole proof.
#[must_use]
pub(in crate::eval) fn fingerprint(params: &[Parameter], body: &Expr) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_params(params, &mut h);
    hash_expr(body, &mut h);
    h.finish()
}

/// Hash a parameter list: arity, then each name + whether/how it defaults. Names ARE part of the identity
/// (a renamed param is a different function to us — and a different intrinsic contract).
fn hash_params(params: &[Parameter], h: &mut impl Hasher) {
    params.len().hash(h);
    for p in params {
        p.name.hash(h);
        match &p.default {
            Some(default) => {
                1u8.hash(h);
                hash_expr(default, h);
            }
            None => 0u8.hash(h),
        }
    }
}

/// Hash an argument list (call args + comprehension bindings): each arg's optional name + its value expr.
fn hash_args(args: &[Arg], h: &mut impl Hasher) {
    args.len().hash(h);
    for arg in args {
        match &arg.name {
            Some(name) => {
                1u8.hash(h);
                name.hash(h);
            }
            None => 0u8.hash(h),
        }
        hash_expr(&arg.value, h);
    }
}

/// Hash an expression's STRUCTURE, recursively, span-free. The match is EXHAUSTIVE with NO wildcard arm on
/// purpose: adding an `ExprKind` variant is then a COMPILE error here, forcing the fingerprint to account
/// for it — a silently-unhashed field would let two different functions collide and mis-dispatch an
/// intrinsic. Each arm leads with a distinct discriminant byte so structurally-different shapes can't alias
/// by field coincidence.
#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive per-variant match IS the safety mechanism — one arm per ExprKind, no wildcard, \
    so a new AST variant is a compile error here rather than a silently-unhashed field that could collide"
)]
fn hash_expr(e: &Expr, h: &mut impl Hasher) {
    match &e.kind {
        ExprKind::Num(n) => {
            0u8.hash(h);
            n.to_bits().hash(h);
        }
        ExprKind::Str(s) => {
            1u8.hash(h);
            s.hash(h);
        }
        ExprKind::Bool(b) => {
            2u8.hash(h);
            b.hash(h);
        }
        ExprKind::Undef => 3u8.hash(h),
        ExprKind::Ident(name) => {
            4u8.hash(h);
            name.hash(h);
        }
        ExprKind::Unary { op, operand } => {
            5u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(operand, h);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            6u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(lhs, h);
            hash_expr(rhs, h);
        }
        ExprKind::Ternary { cond, then, els } => {
            7u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_expr(els, h);
        }
        ExprKind::Index { base, index } => {
            8u8.hash(h);
            hash_expr(base, h);
            hash_expr(index, h);
        }
        ExprKind::Member { base, field } => {
            9u8.hash(h);
            hash_expr(base, h);
            field.hash(h);
        }
        ExprKind::Call { callee, args } => {
            10u8.hash(h);
            hash_expr(callee, h);
            hash_args(args, h);
        }
        ExprKind::Vector(items) => {
            11u8.hash(h);
            items.len().hash(h);
            for item in items {
                hash_expr(item, h);
            }
        }
        ExprKind::Range { start, step, end } => {
            12u8.hash(h);
            hash_expr(start, h);
            match step {
                Some(step) => {
                    1u8.hash(h);
                    hash_expr(step, h);
                }
                None => 0u8.hash(h),
            }
            hash_expr(end, h);
        }
        ExprKind::FunctionLiteral { params, body } => {
            13u8.hash(h);
            hash_params(params, h);
            hash_expr(body, h);
        }
        ExprKind::Let { bindings, body } => {
            14u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::Assert { args, body } => {
            15u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::Echo { args, body } => {
            16u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::LcFor { bindings, body } => {
            17u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            18u8.hash(h);
            hash_args(init, h);
            hash_expr(cond, h);
            hash_args(update, h);
            hash_expr(body, h);
        }
        ExprKind::LcEach(body) => {
            19u8.hash(h);
            hash_expr(body, h);
        }
        ExprKind::LcIf { cond, then, els } => {
            20u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_opt(els.as_deref(), h);
        }
    }
}

/// Hash an optional sub-expression (a present/absent flag then the expr) — `assert`/`echo`/`LcIf` bodies.
fn hash_opt(e: Option<&Expr>, h: &mut impl Hasher) {
    match e {
        Some(e) => {
            1u8.hash(h);
            hash_expr(e, h);
        }
        None => 0u8.hash(h),
    }
}
