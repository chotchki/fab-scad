//! AST → source pretty-printer — the inverse of the parser.
//!
//! Output is FULLY PARENTHESIZED + canonical: every composite expression wraps in `(…)`, so
//! `parse(print(ast))` reconstructs `ast` (modulo spans) with no precedence ambiguity — the H.5.2
//! roundtrip property. Not pretty, deliberately: correctness over cosmetics. Comprehensions print
//! parenthesized (`(for …)`), which re-parses via the `list_comprehension_elements_p` paren form when
//! it sits in a vector element.
//!
//! SCOPE: recursive, for the BOUNDED asts the roundtrip generator + the customizer produce. It is NOT
//! hardened against an adversarial deep-left-chain ast (a 200k-deep `Binary` spine would overflow) —
//! that path is the PARSER's (guarded) and `Drop`'s (non-recursive) to own; the printer only ever
//! prints asts we built. If the customizer ever needs to emit deep exprs, make this iterative then.

use super::ast::{
    Arg, BinOp, Expr, ExprKind, ModuleInstantiation, Parameter, Program, Stmt, StmtKind, UnOp,
};

/// Print a whole [`Program`] to canonical OpenSCAD source (one statement per line).
#[must_use]
pub fn print(program: &Program) -> String {
    let mut out = String::new();
    for stmt in &program.stmts {
        write_stmt(&mut out, stmt);
        out.push('\n');
    }
    out
}

/// Print a single [`Expr`] to canonical source (fully parenthesized).
#[must_use]
pub fn print_expr(e: &Expr) -> String {
    let mut out = String::new();
    write_expr(&mut out, e);
    out
}

fn write_stmt(out: &mut String, s: &Stmt) {
    match &s.kind {
        StmtKind::Empty => out.push(';'),
        StmtKind::Assignment { name, value } => {
            out.push_str(name);
            out.push_str(" = ");
            write_expr(out, value);
            out.push(';');
        }
        StmtKind::Block(stmts) => write_block(out, stmts),
        StmtKind::Module(mi) => write_module_inst(out, mi),
        StmtKind::ModuleDef { name, params, body } => {
            out.push_str("module ");
            out.push_str(name);
            out.push('(');
            write_params(out, params);
            out.push_str(") ");
            write_stmt(out, body);
        }
        StmtKind::FunctionDef { name, params, body } => {
            out.push_str("function ");
            out.push_str(name);
            out.push('(');
            write_params(out, params);
            out.push_str(") = ");
            write_expr(out, body);
            out.push(';');
        }
        StmtKind::If { cond, then, els } => {
            out.push_str("if (");
            write_expr(out, cond);
            out.push_str(") ");
            write_block(out, then);
            if !els.is_empty() {
                out.push_str(" else ");
                write_block(out, els);
            }
        }
        StmtKind::Use(path) => {
            out.push_str("use <");
            out.push_str(path);
            out.push('>');
        }
        StmtKind::Include(path) => {
            out.push_str("include <");
            out.push_str(path);
            out.push('>');
        }
    }
}

/// A `{ … }` statement block / child list. ALWAYS braces (never the single-child shorthand): the
/// shorthand `translate() a();` and `translate() { a(); }` both parse to the same children, but a
/// SINGLE nested-block child (`translate() { { … } }`) only round-trips through the brace form.
fn write_block(out: &mut String, stmts: &[Stmt]) {
    out.push('{');
    for s in stmts {
        write_stmt(out, s);
    }
    out.push('}');
}

fn write_module_inst(out: &mut String, mi: &ModuleInstantiation) {
    // Modifiers print in a fixed order; they're order-independent FLAGS, so any input order
    // reconstructs the same `Modifiers`.
    if mi.modifiers.root {
        out.push('!');
    }
    if mi.modifiers.highlight {
        out.push('#');
    }
    if mi.modifiers.background {
        out.push('%');
    }
    if mi.modifiers.disable {
        out.push('*');
    }
    out.push_str(&mi.name);
    out.push('(');
    write_args(out, &mi.args);
    out.push(')');
    write_block(out, &mi.children);
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per ExprKind variant — a flat dispatch reads better than splitting it"
)]
fn write_expr(out: &mut String, e: &Expr) {
    match &e.kind {
        ExprKind::Num(n) => out.push_str(&n.to_string()),
        ExprKind::Str(s) => {
            out.push('"');
            write_escaped(out, s);
            out.push('"');
        }
        ExprKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        ExprKind::Undef => out.push_str("undef"),
        ExprKind::Ident(n) => out.push_str(n),
        ExprKind::Unary { op, operand } => {
            out.push('(');
            out.push_str(unop_str(*op));
            write_expr(out, operand);
            out.push(')');
        }
        ExprKind::Binary { op, lhs, rhs } => {
            out.push('(');
            write_expr(out, lhs);
            out.push(' ');
            out.push_str(binop_str(*op));
            out.push(' ');
            write_expr(out, rhs);
            out.push(')');
        }
        ExprKind::Ternary { cond, then, els } => {
            out.push('(');
            write_expr(out, cond);
            out.push_str(" ? ");
            write_expr(out, then);
            out.push_str(" : ");
            write_expr(out, els);
            out.push(')');
        }
        ExprKind::Index { base, index } => {
            write_expr(out, base);
            out.push('[');
            write_expr(out, index);
            out.push(']');
        }
        ExprKind::Member { base, field } => {
            write_expr(out, base);
            out.push('.');
            out.push_str(field);
        }
        ExprKind::Call { callee, args } => {
            write_expr(out, callee);
            out.push('(');
            write_args(out, args);
            out.push(')');
        }
        ExprKind::Vector(elems) => {
            out.push('[');
            write_comma_exprs(out, elems);
            out.push(']');
        }
        ExprKind::Range { start, step, end } => {
            out.push('[');
            write_expr(out, start);
            out.push_str(" : ");
            if let Some(step) = step {
                write_expr(out, step);
                out.push_str(" : ");
            }
            write_expr(out, end);
            out.push(']');
        }
        ExprKind::FunctionLiteral { params, body } => {
            out.push_str("(function (");
            write_params(out, params);
            out.push_str(") ");
            write_expr(out, body);
            out.push(')');
        }
        ExprKind::Let { bindings, body } => {
            out.push_str("(let (");
            write_args(out, bindings);
            out.push_str(") ");
            write_expr(out, body);
            out.push(')');
        }
        ExprKind::Assert { args, body } => write_assert_echo(out, "assert", args, body.as_deref()),
        ExprKind::Echo { args, body } => write_assert_echo(out, "echo", args, body.as_deref()),
        ExprKind::LcFor { bindings, body } => {
            out.push_str("(for (");
            write_args(out, bindings);
            out.push_str(") ");
            write_expr(out, body);
            out.push(')');
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            out.push_str("(for (");
            write_args(out, init);
            out.push_str("; ");
            write_expr(out, cond);
            out.push_str("; ");
            write_args(out, update);
            out.push_str(") ");
            write_expr(out, body);
            out.push(')');
        }
        ExprKind::LcEach(body) => {
            out.push_str("(each ");
            write_expr(out, body);
            out.push(')');
        }
        ExprKind::LcIf { cond, then, els } => {
            out.push_str("(if (");
            write_expr(out, cond);
            out.push_str(") ");
            write_expr(out, then);
            if let Some(els) = els {
                out.push_str(" else ");
                write_expr(out, els);
            }
            out.push(')');
        }
    }
}

fn write_assert_echo(out: &mut String, kw: &str, args: &[Arg], body: Option<&Expr>) {
    out.push('(');
    out.push_str(kw);
    out.push_str(" (");
    write_args(out, args);
    out.push(')');
    if let Some(body) = body {
        out.push(' ');
        write_expr(out, body);
    }
    out.push(')');
}

fn write_comma_exprs(out: &mut String, exprs: &[Expr]) {
    for (i, e) in exprs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_expr(out, e);
    }
}

fn write_args(out: &mut String, args: &[Arg]) {
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if let Some(name) = &a.name {
            out.push_str(name);
            out.push_str(" = ");
        }
        write_expr(out, &a.value);
    }
}

fn write_params(out: &mut String, params: &[Parameter]) {
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&p.name);
        if let Some(default) = &p.default {
            out.push_str(" = ");
            write_expr(out, default);
        }
    }
}

/// Re-escape a decoded string body so it re-parses to the SAME value (inverse of `decode_str`). Only
/// the value must round-trip, not the exact source escape — so a decoded `\x41` prints as `A`.
fn write_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Pos => "+",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "||",
        BinOp::And => "&&",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::BitOr => "|",
        BinOp::BitAnd => "&",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Pow => "^",
    }
}
