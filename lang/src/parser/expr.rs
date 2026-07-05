//! Expression parsing — OpenSCAD's `parser.y` cascade reproduced by hand.
//!
//! parser.y has NO precedence table; precedence is STRUCTURAL, one nonterminal per tier. We mirror
//! that: `expr → ternary → binary(climb) → unary → exponent → call → primary`. Left-associative
//! binary chains are ITERATIVE (the climb loop); right-assoc (`^`, ternary), unary prefixes, and
//! delimiter nesting recurse — all bounded by [`MAX_DEPTH`](super::MAX_DEPTH), so pathological input
//! errors LOUD instead of overflowing. `depth` is passed unchanged DOWN the single-descent cascade
//! and `depth + 1` at genuine nesting points (sub-expressions inside `()`/`[]`/operators).

use winnow::error::ModalResult;
use winnow::stream::Location;

use super::ast::{Arg, BinOp, Expr, ExprKind, Parameter, UnOp};
use super::{MAX_DEPTH, Tokens, bail, bump, expect, peek_kind, peek_kind2};
use crate::lexer::{TokenKind, decode_str, num_value};

/// Parse a full expression (parser.y:334).
pub(crate) fn expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= MAX_DEPTH {
        return bail(i, "expression nested too deeply");
    }
    ternary(i, depth)
}

/// C-style ternary `cond ? then : els`, right-associative; the condition is a binary-level
/// expression (parser.y:341).
fn ternary(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let cond = binary(i, 0, depth)?;
    if peek_kind(i) != Some(TokenKind::Question) {
        return Ok(cond);
    }
    bump(i)?; // '?'  — commit point
    let then = expr(i, depth + 1)?;
    expect(i, TokenKind::Colon, "':' of a ternary")?;
    let els = expr(i, depth + 1)?;
    let end = i.previous_token_end();
    Ok(Expr {
        kind: ExprKind::Ternary {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        },
        span: start..end,
    })
}

/// Precedence climbing over the left-associative binary tiers (parser.y:362-464). `min_bp` is the
/// minimum binding power to keep consuming; the loop is iterative, so `a-b-c-…` never recurses.
fn binary(i: &mut Tokens<'_, '_>, min_bp: u8, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let mut lhs = unary(i, depth)?;
    while let Some((op, bp)) = peek_kind(i).and_then(binop) {
        if bp < min_bp {
            break;
        }
        bump(i)?; // the operator token
        let rhs = binary(i, bp + 1, depth + 1)?; // left-assoc: right side binds tighter
        let end = i.previous_token_end();
        lhs = Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span: start..end,
        };
    }
    Ok(lhs)
}

/// Map a token kind to a left-associative binary operator + binding power (parser.y tier order:
/// bitwise `|`/`&` sit BETWEEN comparison and shift, not below comparison).
fn binop(k: TokenKind<'_>) -> Option<(BinOp, u8)> {
    let pair = match k {
        TokenKind::OrOr => (BinOp::Or, 2),
        TokenKind::AndAnd => (BinOp::And, 3),
        TokenKind::EqEq => (BinOp::Eq, 4),
        TokenKind::Ne => (BinOp::Ne, 4),
        TokenKind::Lt => (BinOp::Lt, 5),
        TokenKind::Le => (BinOp::Le, 5),
        TokenKind::Gt => (BinOp::Gt, 5),
        TokenKind::Ge => (BinOp::Ge, 5),
        TokenKind::Pipe => (BinOp::BitOr, 6),
        TokenKind::Amp => (BinOp::BitAnd, 7),
        TokenKind::Shl => (BinOp::Shl, 8),
        TokenKind::Shr => (BinOp::Shr, 8),
        TokenKind::Plus => (BinOp::Add, 9),
        TokenKind::Minus => (BinOp::Sub, 9),
        TokenKind::Star => (BinOp::Mul, 10),
        TokenKind::Slash => (BinOp::Div, 10),
        TokenKind::Percent => (BinOp::Mod, 10),
        _ => return None,
    };
    Some(pair)
}

/// Prefix unary `- + ! ~`, right-recursive (parser.y:467-491).
fn unary(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= MAX_DEPTH {
        return bail(i, "expression nested too deeply");
    }
    let op = match peek_kind(i) {
        Some(TokenKind::Minus) => UnOp::Neg,
        Some(TokenKind::Plus) => UnOp::Pos,
        Some(TokenKind::Bang) => UnOp::Not,
        Some(TokenKind::Tilde) => UnOp::BitNot,
        _ => return exponent(i, depth),
    };
    let start = i.current_token_start();
    bump(i)?; // the prefix operator
    let operand = unary(i, depth + 1)?;
    let end = i.previous_token_end();
    Ok(Expr {
        kind: ExprKind::Unary {
            op,
            operand: Box::new(operand),
        },
        span: start..end,
    })
}

/// Power `^`, right-associative, sits between unary and call so `-2^2` == `-(2^2)` and `2^-3` works
/// (the right operand is a `unary`) (parser.y:494-500).
fn exponent(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let base = call(i, depth)?;
    if peek_kind(i) != Some(TokenKind::Caret) {
        return Ok(base);
    }
    bump(i)?; // '^'
    let rhs = unary(i, depth + 1)?; // right operand is a unary → right-assoc
    let end = i.previous_token_end();
    Ok(Expr {
        kind: ExprKind::Binary {
            op: BinOp::Pow,
            lhs: Box::new(base),
            rhs: Box::new(rhs),
        },
        span: start..end,
    })
}

/// Postfix chain: call `(args)`, index `[i]`, member `.field` — left-assoc, tightest (parser.y:502-518).
/// No depth guard: the chain LOOP is iterative, and its sub-expressions route through `expr` (guarded).
fn call(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let mut node = primary(i, depth)?;
    loop {
        let kind = match peek_kind(i) {
            Some(TokenKind::LParen) => {
                bump(i)?;
                let args = arg_list(i, depth + 1)?;
                expect(i, TokenKind::RParen, "closing ')' of a call")?;
                ExprKind::Call {
                    callee: Box::new(node),
                    args,
                }
            }
            Some(TokenKind::LBracket) => {
                bump(i)?;
                let index = expr(i, depth + 1)?;
                expect(i, TokenKind::RBracket, "closing ']' of an index")?;
                ExprKind::Index {
                    base: Box::new(node),
                    index: Box::new(index),
                }
            }
            Some(TokenKind::Dot) => {
                bump(i)?;
                let field = member_name(i)?;
                ExprKind::Member {
                    base: Box::new(node),
                    field,
                }
            }
            _ => break,
        };
        node = Expr {
            kind,
            span: start..i.previous_token_end(),
        };
    }
    Ok(node)
}

/// The identifier after a `.` (parser.y:513 `call '.' TOK_ID`).
fn member_name(i: &mut Tokens<'_, '_>) -> ModalResult<String> {
    match peek_kind(i) {
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => {
            let name = n.to_string();
            bump(i)?;
            Ok(name)
        }
        _ => bail(i, "a member name after '.'"),
    }
}

/// Atoms: literals, identifiers, `(expr)`, and `[…]` vectors/ranges (parser.y:520-567). The deferred
/// expression forms (`function`/`let`/`assert`/`echo`) fail LOUD here.
fn primary(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let kind = match peek_kind(i) {
        Some(TokenKind::Num(raw)) => ExprKind::Num(num_value(raw)),
        Some(TokenKind::Str(raw)) => ExprKind::Str(decode_str(raw)),
        Some(TokenKind::True) => ExprKind::Bool(true),
        Some(TokenKind::False) => ExprKind::Bool(false),
        Some(TokenKind::Undef) => ExprKind::Undef,
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => ExprKind::Ident(n.to_string()),
        Some(TokenKind::LParen) => {
            bump(i)?;
            let inner = expr(i, depth + 1)?;
            expect(i, TokenKind::RParen, "closing ')'")?;
            return Ok(inner); // OpenSCAD returns the inner expr; no paren node
        }
        Some(TokenKind::LBracket) => return vector_or_range(i, depth),
        Some(TokenKind::Function) => {
            return bail(
                i,
                "function-literal expressions are not yet implemented (H.2)",
            );
        }
        Some(TokenKind::Let) => return bail(i, "let-expressions are not yet implemented (H.2)"),
        Some(TokenKind::Assert) => {
            return bail(i, "assert-expressions are not yet implemented (H.2)");
        }
        Some(TokenKind::Echo) => return bail(i, "echo-expressions are not yet implemented (H.2)"),
        _ => return bail(i, "an expression"),
    };
    bump(i)?; // the single-token atom
    Ok(Expr {
        kind,
        span: start..i.previous_token_end(),
    })
}

/// After `[`: an empty vector, a range (`[a:b]` / `[a:step:b]` — middle is the STEP), or a comma
/// vector. List-comprehension elements (`for`/`each`/`let`/`if`) are deferred LOUD (parser.y:551-563).
fn vector_or_range(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    // No depth guard: reached from `primary` at the same depth, so the `expr` guard already fired;
    // the elements route back through `expr` (guarded).
    let start = i.current_token_start();
    bump(i)?; // '['
    if peek_kind(i) == Some(TokenKind::RBracket) {
        bump(i)?;
        return Ok(Expr {
            kind: ExprKind::Vector(Vec::new()),
            span: start..i.previous_token_end(),
        });
    }
    let first = vector_element(i, depth + 1)?;
    if peek_kind(i) == Some(TokenKind::Colon) {
        bump(i)?; // ':'
        let second = expr(i, depth + 1)?;
        if peek_kind(i) == Some(TokenKind::Colon) {
            bump(i)?; // ':'  → [start : step : end]
            let third = expr(i, depth + 1)?;
            expect(i, TokenKind::RBracket, "closing ']' of a range")?;
            return Ok(Expr {
                kind: ExprKind::Range {
                    start: Box::new(first),
                    step: Some(Box::new(second)),
                    end: Box::new(third),
                },
                span: start..i.previous_token_end(),
            });
        }
        expect(i, TokenKind::RBracket, "closing ']' of a range")?; // [start : end]
        return Ok(Expr {
            kind: ExprKind::Range {
                start: Box::new(first),
                step: None,
                end: Box::new(second),
            },
            span: start..i.previous_token_end(),
        });
    }
    let mut elems = vec![first];
    while peek_kind(i) == Some(TokenKind::Comma) {
        bump(i)?; // ','
        if peek_kind(i) == Some(TokenKind::RBracket) {
            break; // trailing comma
        }
        elems.push(vector_element(i, depth + 1)?);
    }
    expect(i, TokenKind::RBracket, "closing ']' of a vector")?;
    Ok(Expr {
        kind: ExprKind::Vector(elems),
        span: start..i.previous_token_end(),
    })
}

/// A vector element — a plain expression; comprehension elements are deferred LOUD (H.3).
fn vector_element(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if matches!(
        peek_kind(i),
        Some(TokenKind::For | TokenKind::Each | TokenKind::Let | TokenKind::If)
    ) {
        return bail(i, "list comprehensions are not yet implemented (H.3)");
    }
    expr(i, depth)
}

/// A call argument list (positional and/or named), with an optional trailing comma (parser.y:679-710).
pub(crate) fn arg_list(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Arg>> {
    let mut args = Vec::new();
    if peek_kind(i) == Some(TokenKind::RParen) {
        return Ok(args); // empty ()
    }
    loop {
        args.push(argument(i, depth)?);
        if peek_kind(i) != Some(TokenKind::Comma) {
            break;
        }
        bump(i)?; // ','
        if peek_kind(i) == Some(TokenKind::RParen) {
            break; // trailing comma
        }
    }
    Ok(args)
}

/// A parameter list (module/function defs, function literals), optional trailing comma, possibly
/// empty (parser.y:645-664). Mirrors [`arg_list`], but each element is a `name`/`name = default`
/// [`Parameter`], not an [`Arg`]. The caller has consumed the opening `(`; this stops at the `)`.
pub(crate) fn param_list(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Parameter>> {
    let mut params = Vec::new();
    if peek_kind(i) == Some(TokenKind::RParen) {
        return Ok(params); // empty ()
    }
    loop {
        params.push(parameter(i, depth)?);
        if peek_kind(i) != Some(TokenKind::Comma) {
            break;
        }
        bump(i)?; // ','
        if peek_kind(i) == Some(TokenKind::RParen) {
            break; // trailing comma
        }
    }
    Ok(params)
}

/// One parameter: `id` or `id = default` (parser.y:666-677). A `$`-prefixed name is a
/// special-variable parameter, so both plain and `$`-idents are accepted.
fn parameter(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Parameter> {
    let start = i.current_token_start();
    let name = match peek_kind(i) {
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => n.to_string(),
        _ => return bail(i, "a parameter name"),
    };
    bump(i)?; // the name
    let default = if peek_kind(i) == Some(TokenKind::Eq) {
        bump(i)?; // '='
        Some(expr(i, depth + 1)?)
    } else {
        None
    };
    Ok(Parameter {
        name,
        default,
        span: start..i.previous_token_end(),
    })
}

/// One argument: `name = expr` (named, incl. `$fn = 8`) or a bare `expr` (positional) (parser.y:700-710).
fn argument(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Arg> {
    let start = i.current_token_start();
    if let Some(TokenKind::Ident(name) | TokenKind::DollarIdent(name)) = peek_kind(i)
        && peek_kind2(i) == Some(TokenKind::Eq)
    {
        bump(i)?; // name
        bump(i)?; // '='
        let value = expr(i, depth + 1)?;
        return Ok(Arg {
            name: Some(name.to_string()),
            value,
            span: start..i.previous_token_end(),
        });
    }
    let value = expr(i, depth + 1)?;
    Ok(Arg {
        name: None,
        value,
        span: start..i.previous_token_end(),
    })
}
