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
                hash_expr(default, h); // top-level entry: the default drains its own work stack
            }
            None => 0u8.hash(h),
        }
    }
}

/// [`hash_params`]'s in-walk twin: inline bytes now, default exprs DEFERRED (reversed) — used from
/// [`hash_node`]'s function-literal arm where the shared work stack drives the traversal.
fn hash_params_deferred<'e>(
    params: &'e [Parameter],
    h: &mut impl Hasher,
    work: &mut Vec<&'e Expr>,
) {
    params.len().hash(h);
    for p in params {
        p.name.hash(h);
        match &p.default {
            Some(_) => 1u8.hash(h),
            None => 0u8.hash(h),
        }
    }
    for p in params.iter().rev() {
        if let Some(default) = &p.default {
            work.push(default);
        }
    }
}

/// Hash an argument list (call args + comprehension bindings): each arg's optional name + its value expr.
fn hash_args<'e>(args: &'e [Arg], h: &mut impl Hasher, work: &mut Vec<&'e Expr>) {
    args.len().hash(h);
    for arg in args {
        match &arg.name {
            Some(name) => {
                1u8.hash(h);
                name.hash(h);
            }
            None => 0u8.hash(h),
        }
    }
    for arg in args.iter().rev() {
        work.push(&arg.value);
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
    // ITERATIVE over the expression tree (AA.4.3): this runs on every function definition at ctx
    // build, and the AA.4 parser spine now accepts arbitrarily deep bodies — per-level recursion
    // here would just move the overflow from the parser into the wire gate. Children are pushed
    // REVERSED so the walk visits them in source order; a child's stream now follows ALL of its
    // parent's inline bytes instead of interleaving mid-arm, which changes the exact byte stream —
    // safe, because fingerprints are computed fresh each run and the registry references hash
    // through this same function, so both sides of the dispatch gate move together.
    let mut work = vec![e];
    while let Some(e) = work.pop() {
        hash_node(e, h, &mut work);
    }
}

/// Hash ONE node's discriminant + inline fields; defer child expressions onto `work` (reversed, so
/// they pop in source order). The exhaustive match is the safety mechanism — see [`hash_expr`].
#[allow(
    clippy::too_many_lines,
    reason = "one arm per ExprKind — the exhaustive match IS the safety mechanism, same as before AA.4"
)]
fn hash_node<'e>(e: &'e Expr, h: &mut impl Hasher, work: &mut Vec<&'e Expr>) {
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
            work.push(operand);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            6u8.hash(h);
            (*op as u8).hash(h);
            work.push(rhs);
            work.push(lhs);
        }
        ExprKind::Ternary { cond, then, els } => {
            7u8.hash(h);
            work.push(els);
            work.push(then);
            work.push(cond);
        }
        ExprKind::Index { base, index } => {
            8u8.hash(h);
            work.push(index);
            work.push(base);
        }
        ExprKind::Member { base, field } => {
            9u8.hash(h);
            field.hash(h);
            work.push(base);
        }
        ExprKind::Call { callee, args } => {
            10u8.hash(h);
            hash_args(args, h, work);
            work.push(callee);
        }
        ExprKind::Vector(items) => {
            11u8.hash(h);
            items.len().hash(h);
            for item in items.iter().rev() {
                work.push(item);
            }
        }
        ExprKind::Range { start, step, end } => {
            12u8.hash(h);
            if let Some(step) = step {
                1u8.hash(h);
                work.push(end);
                work.push(step);
            } else {
                0u8.hash(h);
                work.push(end);
            }
            work.push(start);
        }
        ExprKind::FunctionLiteral { params, body } => {
            13u8.hash(h);
            work.push(body);
            hash_params_deferred(params, h, work);
        }
        ExprKind::Let { bindings, body } => {
            14u8.hash(h);
            work.push(body);
            hash_args(bindings, h, work);
        }
        ExprKind::Assert { args, body } => {
            15u8.hash(h);
            hash_opt(body.as_deref(), h, work);
            hash_args(args, h, work);
        }
        ExprKind::Echo { args, body } => {
            16u8.hash(h);
            hash_opt(body.as_deref(), h, work);
            hash_args(args, h, work);
        }
        ExprKind::LcFor { bindings, body } => {
            17u8.hash(h);
            work.push(body);
            hash_args(bindings, h, work);
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            18u8.hash(h);
            work.push(body);
            hash_args(update, h, work);
            work.push(cond);
            hash_args(init, h, work);
        }
        ExprKind::LcEach(body) => {
            19u8.hash(h);
            work.push(body);
        }
        ExprKind::LcIf { cond, then, els } => {
            20u8.hash(h);
            hash_opt(els.as_deref(), h, work);
            work.push(then);
            work.push(cond);
        }
    }
}

/// Hash an optional sub-expression's present/absent flag inline; a present expr is DEFERRED.
fn hash_opt<'e>(e: Option<&'e Expr>, h: &mut impl Hasher, work: &mut Vec<&'e Expr>) {
    match e {
        Some(e) => {
            1u8.hash(h);
            work.push(e);
        }
        None => 0u8.hash(h),
    }
}
