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

/// Parse a full expression (parser.y:334). The prefix forms (`function`/`let`/`assert`/`echo`) sit at
/// the TOP of the `expr` grammar, ABOVE the ternary/binary cascade — so their body greedily consumes
/// a whole `expr` (`function(x) x + 1` is `function(x) (x + 1)`). They are NOT valid as an operand
/// inside the cascade (`1 + function(x) x` is a syntax error), which falls out because the cascade
/// enters at `ternary`, never re-entering `expr`.
pub(crate) fn expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= MAX_DEPTH {
        return bail(i, "expression nested too deeply");
    }
    match peek_kind(i) {
        Some(TokenKind::Function) => function_literal(i, depth),
        Some(TokenKind::Let) => let_expr(i, depth),
        Some(TokenKind::Assert) => assert_or_echo(i, depth, false),
        Some(TokenKind::Echo) => assert_or_echo(i, depth, true),
        _ => ternary(i, depth),
    }
}

/// A function-literal expression `function(params) body` (parser.y:336).
fn function_literal(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'function'
    expect(i, TokenKind::LParen, "'(' after `function`")?;
    let params = param_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the parameter list")?;
    let body = expr(i, depth + 1)?;
    Ok(Expr {
        kind: ExprKind::FunctionLiteral {
            params,
            body: Box::new(body),
        },
        span: start..i.previous_token_end(),
    })
}

/// A `let(bindings) body` expression (parser.y:345).
fn let_expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'let'
    expect(i, TokenKind::LParen, "'(' after `let`")?;
    let bindings = arg_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the `let` bindings")?;
    let body = expr(i, depth + 1)?;
    Ok(Expr {
        kind: ExprKind::Let {
            bindings,
            body: Box::new(body),
        },
        span: start..i.previous_token_end(),
    })
}

/// An `assert(args) body?` or `echo(args) body?` expression (parser.y:350-359). The trailing body is
/// OPTIONAL (`expr_or_empty`): present iff the next token can START an expression.
fn assert_or_echo(i: &mut Tokens<'_, '_>, depth: usize, is_echo: bool) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'assert' / 'echo'
    expect(i, TokenKind::LParen, "'(' after `assert`/`echo`")?;
    let args = arg_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the arguments")?;
    let body = if starts_expr(peek_kind(i)) {
        Some(Box::new(expr(i, depth + 1)?))
    } else {
        None
    };
    let kind = if is_echo {
        ExprKind::Echo { args, body }
    } else {
        ExprKind::Assert { args, body }
    };
    Ok(Expr {
        kind,
        span: start..i.previous_token_end(),
    })
}

/// Whether `k` can begin an expression — the lookahead for `expr_or_empty`. Every token an `expr` can
/// start with; anything else (`;`, `)`, `]`, `,`, `:`, `}`, EOF, …) means "no body".
fn starts_expr(k: Option<TokenKind<'_>>) -> bool {
    matches!(
        k,
        Some(
            TokenKind::Num(_)
                | TokenKind::Str(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Undef
                | TokenKind::Ident(_)
                | TokenKind::DollarIdent(_)
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Minus
                | TokenKind::Plus
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::Function
                | TokenKind::Let
                | TokenKind::Assert
                | TokenKind::Echo
        )
    )
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
        // `function`/`let`/`assert`/`echo` are handled at `expr` (the top of the grammar); reaching
        // them HERE means they were used as an operand inside the cascade (`1 + let(a=1) a`), which is
        // a syntax error — fall through to the generic "expected an expression".
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

/// A vector element (parser.y:640): a comprehension generator (`for`/`each`/`if`/`let`, or one
/// wrapped in parens) OR a plain expression. Comprehensions NEST — every `body` is itself a
/// vector element.
fn vector_element(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= MAX_DEPTH {
        return bail(i, "list comprehension nested too deeply");
    }
    match peek_kind(i) {
        Some(TokenKind::For) => lc_for(i, depth),
        Some(TokenKind::Each) => lc_each(i, depth),
        Some(TokenKind::If) => lc_if(i, depth),
        Some(TokenKind::Let) => lc_let(i, depth),
        // `( list_comprehension_elements )` — parens around a comprehension (parser.y:616), grouping
        // only. Guarded on a comprehension keyword AFTER the `(`, so a plain `(expr)` still routes to
        // `expr`. (`(let …)` converges: both paths build the same `Let` node.)
        Some(TokenKind::LParen)
            if matches!(
                peek_kind2(i),
                Some(TokenKind::For | TokenKind::Each | TokenKind::If | TokenKind::Let)
            ) =>
        {
            bump(i)?; // '('
            let inner = vector_element(i, depth + 1)?;
            expect(i, TokenKind::RParen, "closing ')' of a comprehension")?;
            Ok(inner)
        }
        _ => expr(i, depth),
    }
}

/// `for (bindings) body` or the C-style `for (init; cond; update) body` (parser.y:592-602).
fn lc_for(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'for'
    expect(i, TokenKind::LParen, "'(' after `for`")?;
    let init = arg_list(i, depth + 1)?;
    let kind = if peek_kind(i) == Some(TokenKind::Semi) {
        bump(i)?; // ';'  → C-style
        let cond = expr(i, depth + 1)?;
        expect(i, TokenKind::Semi, "';' between the C-style `for` clauses")?;
        let update = arg_list(i, depth + 1)?;
        expect(i, TokenKind::RParen, "closing ')' of the `for` clauses")?;
        let body = vector_element(i, depth + 1)?;
        ExprKind::LcForC {
            init,
            cond: Box::new(cond),
            update,
            body: Box::new(body),
        }
    } else {
        expect(i, TokenKind::RParen, "closing ')' of the `for` bindings")?;
        let body = vector_element(i, depth + 1)?;
        ExprKind::LcFor {
            bindings: init,
            body: Box::new(body),
        }
    };
    Ok(Expr {
        kind,
        span: start..i.previous_token_end(),
    })
}

/// `each body` — splice `body`'s list into the enclosing vector (parser.y:588).
fn lc_each(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'each'
    let body = vector_element(i, depth + 1)?;
    Ok(Expr {
        kind: ExprKind::LcEach(Box::new(body)),
        span: start..i.previous_token_end(),
    })
}

/// A comprehension `if (cond) then [else els]` (parser.y:603-607) — the else binds greedily, as with
/// the statement `if`.
fn lc_if(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'if'
    expect(i, TokenKind::LParen, "'(' after `if` in a comprehension")?;
    let cond = expr(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of a comprehension `if`")?;
    let then = vector_element(i, depth + 1)?;
    let els = if peek_kind(i) == Some(TokenKind::Else) {
        bump(i)?; // 'else'
        Some(Box::new(vector_element(i, depth + 1)?))
    } else {
        None
    };
    Ok(Expr {
        kind: ExprKind::LcIf {
            cond: Box::new(cond),
            then: Box::new(then),
            els,
        },
        span: start..i.previous_token_end(),
    })
}

/// `let (bindings) body` as a comprehension element (parser.y:583). Reuses [`ExprKind::Let`] — a
/// vector `let` is semantically the let-EXPRESSION (bind, then evaluate the body); the only twist is
/// its body is a vector element, so it may be a nested comprehension.
fn lc_let(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'let'
    expect(i, TokenKind::LParen, "'(' after `let`")?;
    let bindings = arg_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the `let` bindings")?;
    let body = vector_element(i, depth + 1)?;
    Ok(Expr {
        kind: ExprKind::Let {
            bindings,
            body: Box::new(body),
        },
        span: start..i.previous_token_end(),
    })
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
