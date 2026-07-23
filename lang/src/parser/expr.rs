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

use super::ast::{Arg, BinOp, Expr, Parameter};
#[cfg(test)]
use super::ast::{ExprKind, UnOp};
#[cfg(test)]
use super::{MAX_DEPTH, expect};
use super::{Tokens, bail, bump, labeled, peek_kind, peek_kind2};
use crate::lexer::TokenKind;
#[cfg(test)]
use crate::lexer::{decode_str, num_value};

/// Parse a full expression (parser.y:334). The prefix forms (`function`/`let`/`assert`/`echo`) sit at
/// the TOP of the `expr` grammar, ABOVE the ternary/binary cascade — so their body greedily consumes
/// a whole `expr` (`function(x) x + 1` is `function(x) (x + 1)`). They are NOT valid as an operand
/// inside the cascade (`1 + function(x) x` is a syntax error), which falls out because the cascade
/// enters at `ternary`, never re-entering `expr`.
pub(crate) fn expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    super::spine::expr(i, depth)
}

/// The RECURSIVE cascade — retired from production by the AA.4 spine, retained as the test-only
/// differential ORACLE (the fast==slow doctrine, parser edition): the spine must produce
/// byte-identical ASTs (kinds AND spans) to this on every corpus program shallow enough for it.
#[cfg(test)]
pub(crate) fn expr_recursive(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= MAX_DEPTH {
        return bail(i, "expression nested too deeply");
    }
    match peek_kind(i) {
        Some(TokenKind::Function) => function_literal(i, depth),
        Some(TokenKind::Let | TokenKind::Assert | TokenKind::Echo) => chain_expr(i, depth),
        _ => ternary(i, depth),
    }
}

/// A function-literal expression `function(params) body` (parser.y:336).
#[cfg(test)]
fn function_literal(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'function'
    expect(i, TokenKind::LParen, "'(' after `function`")?;
    let params = param_list_rec(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the parameter list")?;
    let body = expr_recursive(i, depth + 1)?;
    Ok(Expr {
        kind: ExprKind::FunctionLiteral {
            params,
            body: Box::new(body),
        },
        span: start..i.previous_token_end(),
    })
}

/// A `let(bindings) body` expression (parser.y:345).
/// A `let`/`assert`/`echo` prefix CHAIN + its optional final body (parser.y:337-359), parsed
/// ITERATIVELY — a LOOP over the prefixes, never per-link recursion — then right-folded into the nested
/// AST. OpenSCAD functions are single-expression, so this chain is THE idiom for a series of statements:
/// `let` binds a local, `assert` checks, `echo` prints — each evaluated, then continuing to the body
/// (an `assert`/`echo` is just an anonymous step, evaluated-not-saved). The three prefixes aren't special
/// — they fold identically. BOSL2 writes them 50-100 deep to emulate locals, and recursing once per link
/// would blow the expression depth cap; the loop keeps parse depth O(1). Consecutive `let`s additionally
/// MERGE into one multi-binding node (`let(a) let(b)` == `let(a, b)`, same left-to-right binding).
#[cfg(test)]
fn chain_expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    enum Step {
        Let(Vec<Arg>),
        Assert(Vec<Arg>),
        Echo(Vec<Arg>),
    }
    let mut steps: Vec<(Step, usize)> = Vec::new(); // each step + its start byte, for the node span
    loop {
        let at = i.current_token_start();
        match peek_kind(i) {
            Some(TokenKind::Let) => {
                bump(i)?; // 'let'
                expect(i, TokenKind::LParen, "'(' after `let`")?;
                let bindings = arg_list_rec(i, depth + 1)?;
                expect(i, TokenKind::RParen, "closing ')' of the `let` bindings")?;
                // Each syntactic `let` stays its OWN node (no run-folding): a duplicate name in
                // ONE let is ignored first-wins (AH.2.3), while `let(a=1) let(a=2)` legitimately
                // shadows — folding the run flat would turn the shadow into an ignored duplicate.
                steps.push((Step::Let(bindings), at));
            }
            Some(k @ (TokenKind::Assert | TokenKind::Echo)) => {
                bump(i)?; // 'assert' / 'echo'
                expect(i, TokenKind::LParen, "'(' after `assert`/`echo`")?;
                let args = arg_list_rec(i, depth + 1)?;
                expect(i, TokenKind::RParen, "closing ')' of the arguments")?;
                let step = if k == TokenKind::Echo {
                    Step::Echo(args)
                } else {
                    Step::Assert(args)
                };
                steps.push((step, at));
            }
            _ => break,
        }
    }
    // The final body: an expression if one follows, else none — valid ONLY when the innermost step is an
    // `assert`/`echo` (whose body is optional); a `let` with no body errors when it's folded below.
    let mut body: Option<Expr> = if starts_expr(peek_kind(i)) {
        Some(labeled(i, "a `let`/`assert`/`echo` body", |i| {
            expr_recursive(i, depth + 1)
        })?)
    } else {
        None
    };
    // Right-fold the steps outward: `let(a) assert(b) x` → Let{a, Assert{b, x}}.
    for (step, at) in steps.into_iter().rev() {
        let end = i.previous_token_end();
        let kind = match step {
            Step::Let(bindings) => match body.take() {
                Some(b) => ExprKind::Let {
                    bindings,
                    body: Box::new(b),
                },
                None => return bail(i, "a `let` body"), // `let(a)` with nothing after is invalid
            },
            Step::Assert(args) => ExprKind::Assert {
                args,
                body: body.take().map(Box::new),
            },
            Step::Echo(args) => ExprKind::Echo {
                args,
                body: body.take().map(Box::new),
            },
        };
        body = Some(Expr {
            kind,
            span: at..end,
        });
    }
    // Dispatched on a let/assert/echo, so ≥1 step folded → `body` is always `Some` here.
    body.map_or_else(|| bail(i, "a let/assert/echo chain"), Ok)
}

/// Whether `k` can begin an expression — the lookahead for `expr_or_empty`. Every token an `expr` can
/// start with; anything else (`;`, `)`, `]`, `,`, `:`, `}`, EOF, …) means "no body".
pub(super) fn starts_expr(k: Option<TokenKind<'_>>) -> bool {
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
#[cfg(test)]
fn ternary(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let cond = binary(i, 0, depth)?;
    if peek_kind(i) != Some(TokenKind::Question) {
        return Ok(cond);
    }
    bump(i)?; // '?'  — commit point
    let then = expr_recursive(i, depth + 1)?;
    expect(i, TokenKind::Colon, "':' of a ternary")?;
    let els = expr_recursive(i, depth + 1)?;
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
#[cfg(test)]
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
pub(super) fn binop(k: TokenKind<'_>) -> Option<(BinOp, u8)> {
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
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
fn call(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    let mut node = primary(i, depth)?;
    loop {
        let kind = match peek_kind(i) {
            Some(TokenKind::LParen) => {
                bump(i)?;
                let args = arg_list_rec(i, depth + 1)?;
                expect(i, TokenKind::RParen, "closing ')' of a call")?;
                ExprKind::Call {
                    callee: Box::new(node),
                    args,
                }
            }
            Some(TokenKind::LBracket) => {
                bump(i)?;
                let index = expr_recursive(i, depth + 1)?;
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
#[cfg(test)]
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
#[cfg(test)]
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
            let inner = expr_recursive(i, depth + 1)?;
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
#[cfg(test)]
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
        let second = expr_recursive(i, depth + 1)?;
        if peek_kind(i) == Some(TokenKind::Colon) {
            bump(i)?; // ':'  → [start : step : end]
            let third = expr_recursive(i, depth + 1)?;
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
#[cfg(test)]
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
        _ => expr_recursive(i, depth),
    }
}

/// `for (bindings) body` or the C-style `for (init; cond; update) body` (parser.y:592-602).
#[cfg(test)]
fn lc_for(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'for'
    expect(i, TokenKind::LParen, "'(' after `for`")?;
    let init = arg_list_rec(i, depth + 1)?;
    let kind = if peek_kind(i) == Some(TokenKind::Semi) {
        bump(i)?; // ';'  → C-style
        let cond = expr_recursive(i, depth + 1)?;
        expect(i, TokenKind::Semi, "';' between the C-style `for` clauses")?;
        let update = arg_list_rec(i, depth + 1)?;
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
#[cfg(test)]
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
#[cfg(test)]
fn lc_if(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'if'
    expect(i, TokenKind::LParen, "'(' after `if` in a comprehension")?;
    let cond = expr_recursive(i, depth + 1)?;
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
#[cfg(test)]
fn lc_let(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    let start = i.current_token_start();
    bump(i)?; // 'let'
    expect(i, TokenKind::LParen, "'(' after `let`")?;
    let bindings = arg_list_rec(i, depth + 1)?;
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
        args.push(labeled(i, "a call argument", |i| argument(i, depth))?);
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
    let name: std::rc::Rc<str> = match peek_kind(i) {
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => n.into(),
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
            name: Some(name.into()),
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

// ─── Recursive twins of the argument/parameter helpers — the ORACLE's half (cfg(test)). The
// production `arg_list`/`param_list` above route through the spine via `expr`; these route through
// `expr_recursive`, so the oracle is recursive end-to-end. Bodies mirror their production twins.

#[cfg(test)]
fn arg_list_rec(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Arg>> {
    let mut args = Vec::new();
    if peek_kind(i) == Some(TokenKind::RParen) {
        return Ok(args);
    }
    loop {
        args.push(labeled(i, "a call argument", |i| argument_rec(i, depth))?);
        if peek_kind(i) != Some(TokenKind::Comma) {
            break;
        }
        bump(i)?;
        if peek_kind(i) == Some(TokenKind::RParen) {
            break;
        }
    }
    Ok(args)
}

#[cfg(test)]
fn param_list_rec(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Parameter>> {
    let mut params = Vec::new();
    if peek_kind(i) == Some(TokenKind::RParen) {
        return Ok(params);
    }
    loop {
        params.push(parameter_rec(i, depth)?);
        if peek_kind(i) != Some(TokenKind::Comma) {
            break;
        }
        bump(i)?;
        if peek_kind(i) == Some(TokenKind::RParen) {
            break;
        }
    }
    Ok(params)
}

#[cfg(test)]
fn parameter_rec(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Parameter> {
    let start = i.current_token_start();
    let name: std::rc::Rc<str> = match peek_kind(i) {
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => n.into(),
        _ => return bail(i, "a parameter name"),
    };
    bump(i)?;
    let default = if peek_kind(i) == Some(TokenKind::Eq) {
        bump(i)?;
        Some(expr_recursive(i, depth + 1)?)
    } else {
        None
    };
    Ok(Parameter {
        name,
        default,
        span: start..i.previous_token_end(),
    })
}

#[cfg(test)]
fn argument_rec(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Arg> {
    let start = i.current_token_start();
    if let Some(TokenKind::Ident(name) | TokenKind::DollarIdent(name)) = peek_kind(i)
        && peek_kind2(i) == Some(TokenKind::Eq)
    {
        bump(i)?;
        bump(i)?;
        let value = expr_recursive(i, depth + 1)?;
        return Ok(Arg {
            name: Some(name.into()),
            value,
            span: start..i.previous_token_end(),
        });
    }
    let value = expr_recursive(i, depth + 1)?;
    Ok(Arg {
        name: None,
        value,
        span: start..i.previous_token_end(),
    })
}
