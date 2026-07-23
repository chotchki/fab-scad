//! The iterative expression spine (AA.4.2) — the whole `expr` grammar as an explicit-stack machine.
//!
//! Replaces the recursive cascade in [`expr.rs`](super::expr) for PARSING (the cascade survives as
//! the test-only differential oracle). One driver loop over two heap stacks — operands ([`Expr`])
//! and continuation [`Frame`]s — so expression nesting depth costs HEAP, never host stack: the
//! 144-deep issue4172 vector and a 100k-deep one parse alike. Statement nesting keeps its own
//! `MAX_DEPTH` (a legitimately-bounded shape); the spine's only ceiling is [`MAX_FRAMES`], an
//! adversarial-input sanity bound orders of magnitude past any real program.
//!
//! Equivalence contract: byte-identical ASTs (kinds AND spans) to the recursive cascade — pinned by
//! the `spine_matches_recursive_oracle` differential over the conformance corpus + generated
//! programs. Span conventions reproduced: a postfix chain's nodes all share the chain's start; a
//! `let`/`assert`/`echo` chain's folded nodes share one end; parens return the inner expr unchanged.

use winnow::error::ModalResult;
use winnow::stream::Location;

use super::ast::{Arg, BinOp, Expr, ExprKind, Parameter, UnOp};
use super::expr::starts_expr;
use super::{Tokens, bail, bump, expect, peek_kind, peek_kind2};
use crate::lexer::{TokenKind, decode_str, num_value};

/// Frame-stack sanity ceiling. Not a grammar bound — a guard against adversarial megabyte inputs
/// (the eval step budget's parser cousin). ~100k frames ≈ a >30k-deep nesting; issue4172 is 144.
const MAX_FRAMES: usize = 100_000;

/// What the driver is doing next.
enum Mode {
    /// Expecting an expression operand (an atom, a prefix, a bracket, …). `vec_elem` widens the
    /// grammar to vector-element position: comprehension generators (`for`/`each`/`if`/`let`, or
    /// parenthesized ones) are legal here (parser.y:640). `top` marks the TOP of an `expr` — the
    /// only position where the prefix forms (`function`/`let`/`assert`/`echo`) are legal
    /// (parser.y:336-359: they sit ABOVE the cascade; `1 + function(x) x` is a syntax error).
    Operand { vec_elem: bool, top: bool },
    /// Just completed the operand `expr` — look for postfix/operators, else fold outward.
    /// `start` is the operand's TOKEN start — for a parenthesized operand that's the `(`, while the
    /// expr's own span excludes it; the cascade's chain/binary/ternary spans use the token start.
    Operator { e: Expr, start: usize },
    /// Just completed a comprehension GENERATOR node (`LcFor`/`LcEach`/`LcIf`/`LcLet`) — a finished
    /// vector ELEMENT: generators take no postfix/operators, so this folds straight into the
    /// enclosing container (or an enclosing generator's body slot).
    Element(Expr),
}

/// A suspended continuation. Each variant carries exactly what its fold needs — including the span
/// starts the cascade's conventions require.
enum Frame {
    /// `- + ! ~` applied when the operand below completes. Folds on operator-fold (binds tighter
    /// than every binary tier, LOOSER than `^` — `-2^2` is `-(2^2)`, so `^` does NOT fold it).
    Unary { op: UnOp, start: usize },
    /// A pending left-assoc binary op: `lhs op <operand>`. Folds when an incoming operator's
    /// binding power is ≤ `bp` (left-assoc), or on any expression end. `start` = the lhs's TOKEN
    /// start (the cascade's `binary()` entry position).
    Binary {
        op: BinOp,
        bp: u8,
        lhs: Expr,
        start: usize,
    },
    /// A pending `lhs ^ <operand>` (right-assoc; rhs is unary-tier, so an incoming `^` does NOT
    /// fold it and neither does a prefix op below it). `start` = the base's TOKEN start.
    Pow { lhs: Expr, start: usize },
    /// `cond ? <operand>` — awaiting the `:`. `start` = the condition's TOKEN start.
    TernaryThen { cond: Expr, start: usize },
    /// `cond ? then : <operand>` — right-assoc, folds on expression end.
    TernaryEls {
        cond: Expr,
        then: Expr,
        start: usize,
    },
    /// `( <expr> )` grouping — folds to the inner expr UNCHANGED (no node, inner span; the cascade's
    /// paren behavior), then CONTINUES in operator mode with the `(` as the operand's token start
    /// (postfix/binary after it span from the paren: `(f)(x)` is a Call spanning from `(`).
    Paren { start: usize },
    /// A postfix call's argument collection: `callee ( <args…> )`. `chain_start` is the whole
    /// postfix chain's span start; `pending` is the current arg's `name =` half + its span start.
    CallArgs {
        callee: Expr,
        chain_start: usize,
        args: Vec<Arg>,
        pending: PendingArg,
    },
    /// A postfix index: `base [ <expr> ]`.
    Index { base: Expr, chain_start: usize },
    /// `[` seen, first element pending — could become a vector, a range, or (empty handled inline).
    BracketFirst { start: usize },
    /// `[first : <expr>` — a range's second slot (step-or-end).
    RangeSecond { start: usize, first: Expr },
    /// `[first : second : <expr>` — a range's third slot (the end; middle was the step).
    RangeThird {
        start: usize,
        first: Expr,
        second: Expr,
    },
    /// A comma vector: collected elements + the current one pending.
    VectorElems { start: usize, elems: Vec<Expr> },
    /// `function ( params… ) <body>` — the body pending (params already collected iteratively).
    FnLitBody {
        start: usize,
        params: Vec<Parameter>,
    },
    /// A function-literal parameter list mid-collection, suspended for a `name = <default>` expr.
    FnLitParam {
        start: usize,
        params: Vec<Parameter>,
        pending_name: std::rc::Rc<str>,
        pending_start: usize,
    },
    /// A `let`/`assert`/`echo` chain: completed steps + the current step's args mid-collection.
    ChainArgs {
        steps: Vec<(ChainStep, usize)>,
        step: ChainStep,
        step_start: usize,
        pending: PendingArg,
    },
    /// The chain's final body pending — folds the whole chain outward on completion.
    ChainBody { steps: Vec<(ChainStep, usize)> },
    /// `for ( <bindings…> )` (comprehension) mid-collection — or, after a `;`, the C-style variants.
    LcForBindings {
        start: usize,
        bindings: Vec<Arg>,
        pending: PendingArg,
    },
    /// C-style `for(init; <cond> ; update)` — the condition pending.
    LcForCCond { start: usize, init: Vec<Arg> },
    /// C-style `for(init; cond; <update…>)` mid-collection.
    LcForCUpdate {
        start: usize,
        init: Vec<Arg>,
        cond: Expr,
        update: Vec<Arg>,
        pending: PendingArg,
    },
    /// A plain comprehension `for (bindings) <element>` — the body pending.
    LcForBody { start: usize, bindings: Vec<Arg> },
    /// C-style `for (…;…;…) <element>` — the body pending.
    LcForCBody {
        start: usize,
        init: Vec<Arg>,
        cond: Expr,
        update: Vec<Arg>,
    },
    /// `each <element>` pending.
    LcEach { start: usize },
    /// A comprehension `if ( <cond> )` pending.
    LcIfCond { start: usize },
    /// `if (cond) <element>` — the then-element pending.
    LcIfThen { start: usize, cond: Expr },
    /// `if (cond) then else <element>` — the else-element pending.
    LcIfElse {
        start: usize,
        cond: Expr,
        then: Expr,
    },
    /// A comprehension `let ( <bindings…> )` mid-collection.
    LcLetBindings {
        start: usize,
        bindings: Vec<Arg>,
        pending: PendingArg,
    },
    /// `let (bindings) <element>` (comprehension position) — the body pending.
    LcLetBody { start: usize, bindings: Vec<Arg> },
    /// `( <element> )` around a comprehension generator (parser.y:616) — grouping only.
    LcParen,
}

/// One `let`/`assert`/`echo` chain step (the cascade's `Step`, hoisted out for the frame).
enum ChainStep {
    Let(Vec<Arg>),
    Assert(Vec<Arg>),
    Echo(Vec<Arg>),
}

/// The `name =` half of an argument mid-collection (`None` name = positional), plus its span start.
struct PendingArg {
    name: Option<std::rc::Rc<str>>,
    start: usize,
}

/// Parse one full expression iteratively. The public face matches the cascade's `expr(i, depth)`;
/// `depth` guards STATEMENT-level nesting at entry only — expression nesting is heap-framed here
/// and no longer consumes it.
pub(crate) fn expr(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Expr> {
    if depth >= super::MAX_DEPTH {
        return bail(i, "expression nested too deeply");
    }
    drive(i, false)
}

/// Parse one vector ELEMENT (comprehension generators legal) — `vector_or_range`'s element entry.
fn drive(i: &mut Tokens<'_, '_>, vec_elem: bool) -> ModalResult<Expr> {
    let mut frames: Vec<Frame> = Vec::new();
    let mut mode = Mode::Operand {
        vec_elem,
        top: true,
    };
    loop {
        if frames.len() > MAX_FRAMES {
            return bail(i, "expression nested beyond the parser's sanity ceiling");
        }
        mode = match mode {
            Mode::Operand { vec_elem, top } => operand(i, &mut frames, vec_elem, top)?,
            Mode::Operator { e, start } => operator(i, &mut frames, e, start)?,
            Mode::Element(e) => match fold_element(i, &mut frames, e)? {
                Folded::Continue(m) => m,
                Folded::Done(e) => return Ok(e),
            },
        };
    }
}

/// Operand mode: dispatch the next token(s) into an atom, a prefix frame, or a container frame.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per operand-position token class — the dispatch IS the grammar"
)]
fn operand(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    vec_elem: bool,
    top: bool,
) -> ModalResult<Mode> {
    let start = i.current_token_start();
    // Comprehension generators — legal ONLY in vector-element position.
    if vec_elem {
        match peek_kind(i) {
            Some(TokenKind::For) => {
                bump(i)?; // 'for'
                expect(i, TokenKind::LParen, "'(' after `for`")?;
                return begin_args_or_empty(
                    i,
                    frames,
                    |pending| Frame::LcForBindings {
                        start,
                        bindings: Vec::new(),
                        pending,
                    },
                    Frame::LcForBody {
                        start,
                        bindings: Vec::new(),
                    },
                );
            }
            Some(TokenKind::Each) => {
                bump(i)?; // 'each'
                frames.push(Frame::LcEach { start });
                return Ok(Mode::Operand {
                    vec_elem: true,
                    top: true,
                });
            }
            Some(TokenKind::If) => {
                bump(i)?; // 'if'
                expect(i, TokenKind::LParen, "'(' after `if` in a comprehension")?;
                frames.push(Frame::LcIfCond { start });
                return Ok(Mode::Operand {
                    vec_elem: false,
                    top: true,
                });
            }
            Some(TokenKind::Let) => {
                bump(i)?; // 'let'
                expect(i, TokenKind::LParen, "'(' after `let`")?;
                return begin_args_or_empty(
                    i,
                    frames,
                    |pending| Frame::LcLetBindings {
                        start,
                        bindings: Vec::new(),
                        pending,
                    },
                    Frame::LcLetBody {
                        start,
                        bindings: Vec::new(),
                    },
                );
            }
            // `( <generator> )` — grouping around a comprehension element (parser.y:616). Only when
            // a generator keyword follows the paren; a plain `(expr)` stays an ordinary operand.
            Some(TokenKind::LParen)
                if matches!(
                    peek_kind2(i),
                    Some(TokenKind::For | TokenKind::Each | TokenKind::If | TokenKind::Let)
                ) =>
            {
                bump(i)?; // '('
                frames.push(Frame::LcParen);
                return Ok(Mode::Operand {
                    vec_elem: true,
                    top: true,
                });
            }
            _ => {} // fall through to the ordinary expression grammar
        }
    }
    match peek_kind(i) {
        // The prefix forms sit at the TOP of `expr` (parser.y:336-359) — NOT inside the cascade:
        // after a unary/binary/pow operator they're a syntax error (`1 + function(x) x`), exactly
        // where the cascade's `primary` would bail.
        Some(TokenKind::Function | TokenKind::Let | TokenKind::Assert | TokenKind::Echo)
            if !top =>
        {
            bail(i, "an expression")
        }
        Some(TokenKind::Function) => {
            bump(i)?; // 'function'
            expect(i, TokenKind::LParen, "'(' after `function`")?;
            // Parameters collect iteratively; a `name = <default>` suspends into a frame.
            collect_params_then(i, frames, start, Vec::new())
        }
        Some(TokenKind::Let | TokenKind::Assert | TokenKind::Echo) => {
            chain_begin(i, frames, Vec::new())
        }
        Some(TokenKind::Minus) => prefix(i, frames, UnOp::Neg, start),
        Some(TokenKind::Plus) => prefix(i, frames, UnOp::Pos, start),
        Some(TokenKind::Bang) => prefix(i, frames, UnOp::Not, start),
        Some(TokenKind::Tilde) => prefix(i, frames, UnOp::BitNot, start),
        Some(TokenKind::LParen) => {
            bump(i)?;
            frames.push(Frame::Paren { start });
            Ok(Mode::Operand {
                vec_elem: false,
                top: true,
            })
        }
        Some(TokenKind::LBracket) => {
            bump(i)?; // '['
            if peek_kind(i) == Some(TokenKind::RBracket) {
                bump(i)?;
                return Ok(Mode::Operator {
                    e: Expr {
                        kind: ExprKind::Vector(Vec::new()),
                        span: start..i.previous_token_end(),
                    },
                    start,
                });
            }
            frames.push(Frame::BracketFirst { start });
            Ok(Mode::Operand {
                vec_elem: true,
                top: true,
            })
        }
        Some(TokenKind::Num(raw)) => atom(i, ExprKind::Num(num_value(raw)), start),
        Some(TokenKind::Str(raw)) => atom(i, ExprKind::Str(decode_str(raw)), start),
        Some(TokenKind::True) => atom(i, ExprKind::Bool(true), start),
        Some(TokenKind::False) => atom(i, ExprKind::Bool(false), start),
        Some(TokenKind::Undef) => atom(i, ExprKind::Undef, start),
        Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => {
            let kind = ExprKind::Ident(n.to_string());
            atom(i, kind, start)
        }
        _ => bail(i, "an expression"),
    }
}

/// A single-token atom → operator mode.
fn atom(i: &mut Tokens<'_, '_>, kind: ExprKind, start: usize) -> ModalResult<Mode> {
    bump(i)?;
    Ok(Mode::Operator {
        e: Expr {
            kind,
            span: start..i.previous_token_end(),
        },
        start,
    })
}

/// A prefix operator: frame it, stay in operand mode.
fn prefix(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    op: UnOp,
    start: usize,
) -> ModalResult<Mode> {
    bump(i)?;
    frames.push(Frame::Unary { op, start });
    Ok(Mode::Operand {
        vec_elem: false,
        top: false,
    })
}

/// Begin an argument list that may be empty: on an immediate `)`, consume it and push `on_empty`
/// (the post-list frame); otherwise push `on_first(pending)` and parse the first value.
fn begin_args_or_empty(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    on_first: impl FnOnce(PendingArg) -> Frame,
    on_empty: Frame,
) -> ModalResult<Mode> {
    if peek_kind(i) == Some(TokenKind::RParen) {
        bump(i)?; // ')'
        frames.push(on_empty);
        return Ok(Mode::Operand {
            vec_elem: true,
            top: true,
        }); // generator bodies are vector elements
    }
    let pending = take_arg_name(i)?;
    frames.push(on_first(pending));
    Ok(Mode::Operand {
        vec_elem: false,
        top: true,
    })
}

/// Consume an optional `name =` argument head; returns the pending-arg record either way.
fn take_arg_name(i: &mut Tokens<'_, '_>) -> ModalResult<PendingArg> {
    let start = i.current_token_start();
    if let Some(TokenKind::Ident(name) | TokenKind::DollarIdent(name)) = peek_kind(i)
        && peek_kind2(i) == Some(TokenKind::Eq)
    {
        let name: std::rc::Rc<str> = name.into();
        bump(i)?; // name
        bump(i)?; // '='
        return Ok(PendingArg {
            name: Some(name),
            start,
        });
    }
    Ok(PendingArg { name: None, start })
}

/// Collect function-literal parameters iteratively; a defaulted parameter suspends into a frame for
/// its value expression. On the closing `)`, push the body frame.
fn collect_params_then(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    start: usize,
    mut params: Vec<Parameter>,
) -> ModalResult<Mode> {
    loop {
        if peek_kind(i) == Some(TokenKind::RParen) {
            bump(i)?; // ')'
            frames.push(Frame::FnLitBody { start, params });
            return Ok(Mode::Operand {
                vec_elem: false,
                top: true,
            });
        }
        let pstart = i.current_token_start();
        let name: std::rc::Rc<str> = match peek_kind(i) {
            Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => n.into(),
            _ => return bail(i, "a parameter name"),
        };
        bump(i)?; // the name
        if peek_kind(i) == Some(TokenKind::Eq) {
            bump(i)?; // '='
            frames.push(Frame::FnLitParam {
                start,
                params,
                pending_name: name,
                pending_start: pstart,
            });
            return Ok(Mode::Operand {
                vec_elem: false,
                top: true,
            });
        }
        params.push(Parameter {
            name,
            default: None,
            span: pstart..i.previous_token_end(),
        });
        if peek_kind(i) == Some(TokenKind::Comma) {
            bump(i)?; // ',' — on a trailing comma the next loop pass sees `)` and closes
        }
    }
}

/// Begin (or continue) a `let`/`assert`/`echo` chain: consume prefix steps whose arg-lists are
/// EMPTY inline; the first non-empty arg-list suspends into a `ChainArgs` frame. When no prefix
/// follows, decide the optional body.
fn chain_begin(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    mut steps: Vec<(ChainStep, usize)>,
) -> ModalResult<Mode> {
    loop {
        let at = i.current_token_start();
        let k = peek_kind(i);
        match k {
            Some(TokenKind::Let | TokenKind::Assert | TokenKind::Echo) => {
                bump(i)?; // the keyword
                expect(i, TokenKind::LParen, "'(' after `let`/`assert`/`echo`")?;
                let step = match k {
                    Some(TokenKind::Let) => ChainStep::Let(Vec::new()),
                    Some(TokenKind::Echo) => ChainStep::Echo(Vec::new()),
                    _ => ChainStep::Assert(Vec::new()),
                };
                if peek_kind(i) == Some(TokenKind::RParen) {
                    bump(i)?; // ')' — an empty step, no suspension needed
                    push_chain_step(&mut steps, step, at);
                    continue;
                }
                let pending = take_arg_name(i)?;
                frames.push(Frame::ChainArgs {
                    steps,
                    step,
                    step_start: at,
                    pending,
                });
                return Ok(Mode::Operand {
                    vec_elem: false,
                    top: true,
                });
            }
            _ => break,
        }
    }
    chain_body_or_fold(i, frames, steps)
}

/// A chain step completed (its `)` consumed). Consecutive `let`s stay SEPARATE nodes (the old
/// run-fold died with AH.2.3): a duplicate inside one `let` is ignored first-wins, while
/// `let(a=1) let(a=2)` legitimately shadows — flattening conflates the two.
fn push_chain_step(steps: &mut Vec<(ChainStep, usize)>, step: ChainStep, at: usize) {
    steps.push((step, at));
}

/// After the last chain prefix: an optional body (present iff an expression starts here) — suspend
/// for it, or fold the chain body-less immediately.
fn chain_body_or_fold(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    steps: Vec<(ChainStep, usize)>,
) -> ModalResult<Mode> {
    if starts_expr(peek_kind(i)) {
        frames.push(Frame::ChainBody { steps });
        return Ok(Mode::Operand {
            vec_elem: false,
            top: true,
        });
    }
    match fold_chain(i, steps, None)? {
        Some(e) => {
            let start = e.span.start;
            Ok(Mode::Operator { e, start })
        }
        None => bail(i, "a let/assert/echo chain"),
    }
}

/// Right-fold a completed chain outward (the cascade's exact shape: one shared END, per-step
/// starts; a body-less `let` is invalid).
fn fold_chain(
    i: &mut Tokens<'_, '_>,
    steps: Vec<(ChainStep, usize)>,
    mut body: Option<Expr>,
) -> ModalResult<Option<Expr>> {
    for (step, at) in steps.into_iter().rev() {
        let end = i.previous_token_end();
        let kind = match step {
            ChainStep::Let(bindings) => match body.take() {
                Some(b) => ExprKind::Let {
                    bindings,
                    body: Box::new(b),
                },
                None => return bail(i, "a `let` body"),
            },
            ChainStep::Assert(args) => ExprKind::Assert {
                args,
                body: body.take().map(Box::new),
            },
            ChainStep::Echo(args) => ExprKind::Echo {
                args,
                body: body.take().map(Box::new),
            },
        };
        body = Some(Expr {
            kind,
            span: at..end,
        });
    }
    Ok(body)
}

/// Operator mode: `e` just completed as a (sub)expression — postfix chains extend it, operators
/// frame it, anything else folds outward.
fn operator(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    mut e: Expr,
    start: usize,
) -> ModalResult<Mode> {
    // Postfix loop (call/index/member) — tightest tier, left-assoc; the chain's nodes all share the
    // chain start (= the operand's span start; parens/idents both satisfy the cascade's convention).
    loop {
        match peek_kind(i) {
            Some(TokenKind::LParen) => {
                bump(i)?; // '('
                let chain_start = start;
                if peek_kind(i) == Some(TokenKind::RParen) {
                    bump(i)?; // ')'
                    e = Expr {
                        span: chain_start..i.previous_token_end(),
                        kind: ExprKind::Call {
                            callee: Box::new(e),
                            args: Vec::new(),
                        },
                    };
                    continue;
                }
                let pending = take_arg_name(i)?;
                frames.push(Frame::CallArgs {
                    callee: e,
                    chain_start,
                    args: Vec::new(),
                    pending,
                });
                return Ok(Mode::Operand {
                    vec_elem: false,
                    top: true,
                });
            }
            Some(TokenKind::LBracket) => {
                bump(i)?; // '['
                frames.push(Frame::Index {
                    chain_start: start,
                    base: e,
                });
                return Ok(Mode::Operand {
                    vec_elem: false,
                    top: true,
                });
            }
            Some(TokenKind::Dot) => {
                bump(i)?; // '.'
                let field = match peek_kind(i) {
                    Some(TokenKind::Ident(n) | TokenKind::DollarIdent(n)) => {
                        let name = n.to_string();
                        bump(i)?;
                        name
                    }
                    _ => return bail(i, "a member name after '.'"),
                };
                e = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::Member {
                        base: Box::new(e),
                        field,
                    },
                };
            }
            _ => break,
        }
    }
    // `^` — binds tighter than unary/binary; right-assoc (an incoming `^` folds nothing).
    if peek_kind(i) == Some(TokenKind::Caret) {
        bump(i)?; // '^'
        frames.push(Frame::Pow { lhs: e, start });
        return Ok(Mode::Operand {
            vec_elem: false,
            top: false,
        });
    }
    // A binary operator: fold everything that binds at least as tight (unary + pow always; binary
    // frames with bp >= incoming, the left-assoc rule), then frame the rhs.
    if let Some((op, bp)) = peek_kind(i).and_then(super::expr::binop) {
        let (folded, fstart) = fold_tighter(i, frames, e, start, bp);
        bump(i)?; // the operator
        frames.push(Frame::Binary {
            op,
            bp,
            lhs: folded,
            start: fstart,
        });
        return Ok(Mode::Operand {
            vec_elem: false,
            top: false,
        });
    }
    // `?` — ternary, looser than every binary tier: fold them all, then await the `:`.
    if peek_kind(i) == Some(TokenKind::Question) {
        let (folded, fstart) = fold_tighter(i, frames, e, start, 0);
        bump(i)?; // '?'
        frames.push(Frame::TernaryThen {
            cond: folded,
            start: fstart,
        });
        return Ok(Mode::Operand {
            vec_elem: false,
            top: true,
        });
    }
    // Nothing extends the operand — hand it to the fold loop (drive's Element arm).
    let (folded, _) = fold_tighter(i, frames, e, start, 0);
    Ok(Mode::Element(folded))
}

/// Fold `Unary`/`Pow` frames (always) and `Binary` frames with `bp >= min_bp` — the precedence
/// climb made explicit. Ternary frames never fold here (they end only at `:`/expression end).
fn fold_tighter(
    i: &mut Tokens<'_, '_>,
    frames: &mut Vec<Frame>,
    mut e: Expr,
    mut start: usize,
    min_bp: u8,
) -> (Expr, usize) {
    loop {
        match frames.pop() {
            Some(Frame::Unary { op, start: at }) => {
                e = Expr {
                    span: at..i.previous_token_end(),
                    kind: ExprKind::Unary {
                        op,
                        operand: Box::new(e),
                    },
                };
                start = at;
            }
            Some(Frame::Pow { lhs, start: at }) => {
                e = Expr {
                    span: at..i.previous_token_end(),
                    kind: ExprKind::Binary {
                        op: BinOp::Pow,
                        lhs: Box::new(lhs),
                        rhs: Box::new(e),
                    },
                };
                start = at;
            }
            Some(Frame::Binary {
                op,
                bp,
                lhs,
                start: at,
            }) if bp >= min_bp => {
                e = Expr {
                    span: at..i.previous_token_end(),
                    kind: ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(e),
                    },
                };
                start = at;
            }
            // A looser-binding Binary (guard failed) or a container frame — not ours to fold.
            Some(other) => {
                frames.push(other);
                return (e, start);
            }
            None => return (e, start),
        }
    }
}

/// The outcome of folding a completed element into the frame below it.
enum Folded {
    /// Keep driving in this mode.
    Continue(Mode),
    /// The frame stack is empty — `e` is the whole expression.
    Done(Expr),
}

/// Fold a COMPLETED expression/element into the container/continuation frame below — the heart of
/// the machine. Every arm consumes the frame the cascade would have returned into.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per Frame variant — the fold table IS the grammar's continuation structure"
)]
fn fold_element(i: &mut Tokens<'_, '_>, frames: &mut Vec<Frame>, e: Expr) -> ModalResult<Folded> {
    // Expression-tier frames (unary/pow/binary) fold in `fold_tighter`; container frames each have
    // their continuation. The PURE folds (ternary-complete, fn-lit body, chain body) LOOP here
    // instead of recursing — a 10k-deep right-assoc chain folds on O(1) host stack.
    let mut e = e;
    loop {
        // Entry folds are no-ops here in practice (operator() pre-folds; Element completions sit on
        // container frames), so the carried start is the span start.
        let entry_start = e.span.start;
        (e, _) = fold_tighter(i, frames, e, entry_start, 0);
        let Some(frame) = frames.pop() else {
            return Ok(Folded::Done(e));
        };
        match frame {
            Frame::TernaryThen { cond, start } => {
                expect(i, TokenKind::Colon, "':' of a ternary")?;
                frames.push(Frame::TernaryEls {
                    cond,
                    then: e,
                    start,
                });
                return Ok(Folded::Continue(Mode::Operand {
                    vec_elem: false,
                    top: true,
                }));
            }
            Frame::TernaryEls { cond, then, start } => {
                let span = start..i.previous_token_end();
                e = Expr {
                    kind: ExprKind::Ternary {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Box::new(e),
                    },
                    span,
                };
                // continue the fold loop
            }
            Frame::Paren { start } => {
                expect(i, TokenKind::RParen, "closing ')'")?;
                // Paren returns the inner expr UNCHANGED — postfix/operators after it span from `(`.
                return Ok(Folded::Continue(Mode::Operator { e, start }));
            }
            Frame::CallArgs {
                callee,
                chain_start,
                mut args,
                pending,
            } => {
                args.push(Arg {
                    name: pending.name,
                    value: e,
                    span: pending.start..i.previous_token_end(),
                });
                if let Some(TokenKind::Comma) = peek_kind(i) {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RParen) {
                        bump(i)?; // trailing comma + ')'
                        let call = Expr {
                            span: chain_start..i.previous_token_end(),
                            kind: ExprKind::Call {
                                callee: Box::new(callee),
                                args,
                            },
                        };
                        let start = call.span.start;
                        return Ok(Folded::Continue(Mode::Operator { e: call, start }));
                    }
                    let pending = take_arg_name(i)?;
                    frames.push(Frame::CallArgs {
                        callee,
                        chain_start,
                        args,
                        pending,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                {
                    expect(i, TokenKind::RParen, "closing ')' of a call")?;
                    let call = Expr {
                        span: chain_start..i.previous_token_end(),
                        kind: ExprKind::Call {
                            callee: Box::new(callee),
                            args,
                        },
                    };
                    return {
                        let start = call.span.start;
                        Ok(Folded::Continue(Mode::Operator { e: call, start }))
                    };
                }
            }
            Frame::Index { base, chain_start } => {
                expect(i, TokenKind::RBracket, "closing ']' of an index")?;
                let idx = Expr {
                    span: chain_start..i.previous_token_end(),
                    kind: ExprKind::Index {
                        base: Box::new(base),
                        index: Box::new(e),
                    },
                };
                return Ok(Folded::Continue(Mode::Operator {
                    e: idx,
                    start: chain_start,
                }));
            }
            Frame::BracketFirst { start } => match peek_kind(i) {
                Some(TokenKind::Colon) => {
                    bump(i)?; // ':'
                    frames.push(Frame::RangeSecond { start, first: e });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                Some(TokenKind::Comma) => {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RBracket) {
                        bump(i)?; // trailing comma + ']'
                        return Ok(Folded::Continue(Mode::Operator {
                            e: Expr {
                                kind: ExprKind::Vector(vec![e]),
                                span: start..i.previous_token_end(),
                            },
                            start,
                        }));
                    }
                    frames.push(Frame::VectorElems {
                        start,
                        elems: vec![e],
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: true,
                        top: true,
                    }));
                }
                _ => {
                    expect(i, TokenKind::RBracket, "closing ']' of a vector")?;
                    return Ok(Folded::Continue(Mode::Operator {
                        e: Expr {
                            kind: ExprKind::Vector(vec![e]),
                            span: start..i.previous_token_end(),
                        },
                        start,
                    }));
                }
            },
            Frame::RangeSecond { start, first } => {
                if let Some(TokenKind::Colon) = peek_kind(i) {
                    bump(i)?; // ':'  → [start : step : end]
                    frames.push(Frame::RangeThird {
                        start,
                        first,
                        second: e,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                expect(i, TokenKind::RBracket, "closing ']' of a range")?;
                return Ok(Folded::Continue(Mode::Operator {
                    e: Expr {
                        kind: ExprKind::Range {
                            start: Box::new(first),
                            step: None,
                            end: Box::new(e),
                        },
                        span: start..i.previous_token_end(),
                    },
                    start,
                }));
            }
            Frame::RangeThird {
                start,
                first,
                second,
            } => {
                expect(i, TokenKind::RBracket, "closing ']' of a range")?;
                return Ok(Folded::Continue(Mode::Operator {
                    e: Expr {
                        kind: ExprKind::Range {
                            start: Box::new(first),
                            step: Some(Box::new(second)),
                            end: Box::new(e),
                        },
                        span: start..i.previous_token_end(),
                    },
                    start,
                }));
            }
            Frame::VectorElems { start, mut elems } => {
                elems.push(e);
                if let Some(TokenKind::Comma) = peek_kind(i) {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RBracket) {
                        bump(i)?; // trailing comma + ']'
                        return Ok(Folded::Continue(Mode::Operator {
                            e: Expr {
                                kind: ExprKind::Vector(elems),
                                span: start..i.previous_token_end(),
                            },
                            start,
                        }));
                    }
                    frames.push(Frame::VectorElems { start, elems });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: true,
                        top: true,
                    }));
                }
                expect(i, TokenKind::RBracket, "closing ']' of a vector")?;
                return Ok(Folded::Continue(Mode::Operator {
                    e: Expr {
                        kind: ExprKind::Vector(elems),
                        span: start..i.previous_token_end(),
                    },
                    start,
                }));
            }
            Frame::FnLitBody { start, params } => {
                e = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::FunctionLiteral {
                        params,
                        body: Box::new(e),
                    },
                };
                // continue the fold loop
            }
            Frame::FnLitParam {
                start,
                mut params,
                pending_name,
                pending_start,
            } => {
                params.push(Parameter {
                    name: pending_name,
                    default: Some(e),
                    span: pending_start..i.previous_token_end(),
                });
                if peek_kind(i) == Some(TokenKind::Comma) {
                    bump(i)?; // ','
                }
                return collect_params_then(i, frames, start, params).map(Folded::Continue);
            }
            Frame::ChainArgs {
                mut steps,
                mut step,
                step_start,
                pending,
            } => {
                let arg = Arg {
                    name: pending.name,
                    value: e,
                    span: pending.start..i.previous_token_end(),
                };
                match &mut step {
                    ChainStep::Let(v) | ChainStep::Assert(v) | ChainStep::Echo(v) => v.push(arg),
                }
                if let Some(TokenKind::Comma) = peek_kind(i) {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RParen) {
                        bump(i)?; // trailing comma + ')'
                        push_chain_step(&mut steps, step, step_start);
                        return chain_begin(i, frames, steps).map(Folded::Continue);
                    }
                    let pending = take_arg_name(i)?;
                    frames.push(Frame::ChainArgs {
                        steps,
                        step,
                        step_start,
                        pending,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                expect(i, TokenKind::RParen, "closing ')' of the arguments")?;
                push_chain_step(&mut steps, step, step_start);
                return chain_begin(i, frames, steps).map(Folded::Continue);
            }
            Frame::ChainBody { steps } => {
                match fold_chain(i, steps, Some(e))? {
                    Some(chain) => e = chain, // continue the fold loop
                    None => return bail(i, "a let/assert/echo chain"),
                }
            }
            Frame::LcForBindings {
                start,
                mut bindings,
                pending,
            } => {
                bindings.push(Arg {
                    name: pending.name,
                    value: e,
                    span: pending.start..i.previous_token_end(),
                });
                match peek_kind(i) {
                    Some(TokenKind::Comma) => {
                        bump(i)?; // ','
                        if peek_kind(i) == Some(TokenKind::RParen) {
                            bump(i)?; // trailing comma + ')'
                            frames.push(Frame::LcForBody { start, bindings });
                            return Ok(Folded::Continue(Mode::Operand {
                                vec_elem: true,
                                top: true,
                            }));
                        }
                        let pending = take_arg_name(i)?;
                        frames.push(Frame::LcForBindings {
                            start,
                            bindings,
                            pending,
                        });
                        return Ok(Folded::Continue(Mode::Operand {
                            vec_elem: false,
                            top: true,
                        }));
                    }
                    Some(TokenKind::Semi) => {
                        bump(i)?; // ';' → the C-style form; the collected bindings are the INIT
                        frames.push(Frame::LcForCCond {
                            start,
                            init: bindings,
                        });
                        return Ok(Folded::Continue(Mode::Operand {
                            vec_elem: false,
                            top: true,
                        }));
                    }
                    _ => {
                        expect(i, TokenKind::RParen, "closing ')' of the `for` bindings")?;
                        frames.push(Frame::LcForBody { start, bindings });
                        return Ok(Folded::Continue(Mode::Operand {
                            vec_elem: true,
                            top: true,
                        }));
                    }
                }
            }
            Frame::LcForCCond { start, init } => {
                expect(i, TokenKind::Semi, "';' between the C-style `for` clauses")?;
                if peek_kind(i) == Some(TokenKind::RParen) {
                    bump(i)?; // ')' — empty update list
                    frames.push(Frame::LcForCBody {
                        start,
                        init,
                        cond: e,
                        update: Vec::new(),
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: true,
                        top: true,
                    }));
                }
                let pending = take_arg_name(i)?;
                frames.push(Frame::LcForCUpdate {
                    start,
                    init,
                    cond: e,
                    update: Vec::new(),
                    pending,
                });
                return Ok(Folded::Continue(Mode::Operand {
                    vec_elem: false,
                    top: true,
                }));
            }
            Frame::LcForCUpdate {
                start,
                init,
                cond,
                mut update,
                pending,
            } => {
                update.push(Arg {
                    name: pending.name,
                    value: e,
                    span: pending.start..i.previous_token_end(),
                });
                if let Some(TokenKind::Comma) = peek_kind(i) {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RParen) {
                        bump(i)?; // trailing comma + ')'
                        frames.push(Frame::LcForCBody {
                            start,
                            init,
                            cond,
                            update,
                        });
                        return Ok(Folded::Continue(Mode::Operand {
                            vec_elem: true,
                            top: true,
                        }));
                    }
                    let pending = take_arg_name(i)?;
                    frames.push(Frame::LcForCUpdate {
                        start,
                        init,
                        cond,
                        update,
                        pending,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                expect(i, TokenKind::RParen, "closing ')' of the `for` clauses")?;
                frames.push(Frame::LcForCBody {
                    start,
                    init,
                    cond,
                    update,
                });
                return Ok(Folded::Continue(Mode::Operand {
                    vec_elem: true,
                    top: true,
                }));
            }
            Frame::LcForBody { start, bindings } => {
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::LcFor {
                        bindings,
                        body: Box::new(e),
                    },
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcForCBody {
                start,
                init,
                cond,
                update,
            } => {
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::LcForC {
                        init,
                        cond: Box::new(cond),
                        update,
                        body: Box::new(e),
                    },
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcEach { start } => {
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::LcEach(Box::new(e)),
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcIfCond { start } => {
                expect(i, TokenKind::RParen, "closing ')' of a comprehension `if`")?;
                frames.push(Frame::LcIfThen { start, cond: e });
                return Ok(Folded::Continue(Mode::Operand {
                    vec_elem: true,
                    top: true,
                }));
            }
            Frame::LcIfThen { start, cond } => {
                if peek_kind(i) == Some(TokenKind::Else) {
                    bump(i)?; // 'else'
                    frames.push(Frame::LcIfElse {
                        start,
                        cond,
                        then: e,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: true,
                        top: true,
                    }));
                }
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::LcIf {
                        cond: Box::new(cond),
                        then: Box::new(e),
                        els: None,
                    },
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcIfElse { start, cond, then } => {
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::LcIf {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Some(Box::new(e)),
                    },
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcLetBindings {
                start,
                mut bindings,
                pending,
            } => {
                bindings.push(Arg {
                    name: pending.name,
                    value: e,
                    span: pending.start..i.previous_token_end(),
                });
                if let Some(TokenKind::Comma) = peek_kind(i) {
                    bump(i)?; // ','
                    if peek_kind(i) == Some(TokenKind::RParen) {
                        bump(i)?; // trailing comma + ')'
                        frames.push(Frame::LcLetBody { start, bindings });
                        return Ok(Folded::Continue(Mode::Operand {
                            vec_elem: true,
                            top: true,
                        }));
                    }
                    let pending = take_arg_name(i)?;
                    frames.push(Frame::LcLetBindings {
                        start,
                        bindings,
                        pending,
                    });
                    return Ok(Folded::Continue(Mode::Operand {
                        vec_elem: false,
                        top: true,
                    }));
                }
                expect(i, TokenKind::RParen, "closing ')' of the `let` bindings")?;
                frames.push(Frame::LcLetBody { start, bindings });
                return Ok(Folded::Continue(Mode::Operand {
                    vec_elem: true,
                    top: true,
                }));
            }
            Frame::LcLetBody { start, bindings } => {
                let node = Expr {
                    span: start..i.previous_token_end(),
                    kind: ExprKind::Let {
                        bindings,
                        body: Box::new(e),
                    },
                };
                return Ok(Folded::Continue(Mode::Element(node)));
            }
            Frame::LcParen => {
                expect(i, TokenKind::RParen, "closing ')' of a comprehension")?;
                // Grouping only — the inner generator/element passes through as an ELEMENT.
                return Ok(Folded::Continue(Mode::Element(e)));
            }
            // Expression-tier frames were consumed by `fold_tighter` just above this pop, so one
            // here is a driver bug — fail the PARSE loudly rather than lie about the program.
            Frame::Unary { .. } | Frame::Binary { .. } | Frame::Pow { .. } => {
                debug_assert!(false, "expression-tier frame past fold_tighter");
                return bail(i, "an expression");
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::doc_markdown,
    reason = "differential-test harness: panic IS the assertion; the doc prose cites test names"
)]
mod tests {
    use winnow::stream::TokenSlice;

    /// The spine must produce BYTE-IDENTICAL ASTs (kinds AND spans — `Expr`'s PartialEq compares
    /// both) to the recursive cascade it replaced, across a corpus touching every expression
    /// production. The cascade is the ORACLE (fast==slow, parser edition); programs here stay
    /// shallow enough for its MAX_DEPTH.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the corpus IS the test — one row per grammar shape, splitting it hides coverage"
    )]
    fn spine_matches_recursive_oracle() {
        let corpus: &[&str] = &[
            // atoms + postfix
            "42",
            "0x1f",
            "\"hi\\n\"",
            "true",
            "false",
            "undef",
            "foo",
            "$fn",
            "f(1, x=2)",
            "a[0]",
            "a.field",
            "a.b[0](x)(y)[1].c",
            "f()",
            "f(1,)",
            "f($fn=8)",
            // parens (return inner unchanged; postfix after parens)
            "(1 + 2)",
            "((x))",
            "(f)(x)",
            "(a)[0]",
            "(a).b",
            "((f))(x)[1].m",
            // paren-led operands: the cascade's binary/ternary/pow spans start at the `(`
            "(a) + b",
            "(a == b) ? x : y",
            "(a) ^ 2",
            "-(a).b",
            "x + (y) * z",
            // unary / exponent interplay
            "-a",
            "+a",
            "!a",
            "~a",
            "----x",
            "-2^2",
            "2^-3",
            "2^3^4",
            "-a.b[0]",
            // binary tiers + associativity + comparison chains
            "a || b && c",
            "a == b != c",
            "a < b <= c > d >= e",
            "a | b & c",
            "a << b >> c",
            "a + b - c",
            "a * b / c % d",
            "1 + 2 * 3 ^ 2",
            "a + b < c || d",
            // ternary (right-assoc; cond is binary-tier)
            "c ? t : e",
            "a ? b : c ? d : e",
            "a == b ? c + 1 : d * 2",
            "c ? f(x) : [1, 2]",
            // vectors + ranges (incl. trailing commas, nesting)
            "[]",
            "[1]",
            "[1, 2, 3,]",
            "[[1, 2], [3, 4]]",
            "[0:5]",
            "[0:2:10]",
            "[a + 1 : b * 2]",
            "[[0:1], [1, 2]]",
            // comprehensions (every generator, nesting, parens, C-style for)
            "[for (i = [0:3]) i]",
            "[for (i = r, j = s) i + j]",
            "[for (i = 0; i < 5; i = i + 1) i]",
            "[for (i = 0; i < 5; i = i + 1, j = j - 1) i]",
            "[each list]",
            "[each [1, 2]]",
            "[for (i = r) if (i > 0) i]",
            "[for (i = r) if (i > 0) i else -i]",
            "[for (i = r) let (j = i) j]",
            "[(for (i = r) i)]",
            "[for (i = r) for (j = s) [i, j]]",
            "[for (i = r) each [i, i]]",
            "[if (x) 1]",
            "[let (a = 1) a]",
            "[1, for (i = r) i, 2]",
            // function literals (defaults are exprs; bodies greedy)
            "function(x) x + 1",
            "function(a, b = 2) a * b",
            "function() 0",
            "function(a = f(1), b = [1, 2]) a",
            "function(x) function(y) x + y",
            // let/assert/echo chains (merging, bodies, no-body forms)
            "let(a = 1) a",
            "let(a = 1, b = 2) a + b",
            "let(a = 1) let(b = 2) a",
            "assert(x > 0) y",
            "echo(\"hi\", x) y",
            "assert(x)",
            "echo(x)",
            "let(a = 1) assert(a) echo(a) a + 1",
            "let(a = 1) f(a) ? 1 : 2",
            // trailing commas + empty arg-lists (the fold arms the base corpus missed)
            "f(1, 2,)",
            "f(x = 1,)",
            "let(a = 1,) a",
            "echo(1,) 2",
            "assert(true,) 1",
            "[for (i = [0:1],) i]",
            "[for (i = 0; false;) 1]",
            "[for (i = 0; i < 2; i = i + 1,) i]",
            "[let (a = 1,) a]",
            "[for () 1]",
            "[let () 1]",
            "[each [1,]]",
            "function(a, b = 2,) a",
            "function(a,) a",
            // kitchen-sink composites
            "f([for (i = [0:2:10]) i * 2], x = a ? -b : c[1].d) + 2 ^ g(3)",
            "[for (i = r) if (i.x > f(i)[0]) let (v = -i) v else [i : 2 : j]]",
        ];
        for src in corpus {
            let lexed = crate::lex(src).expect("lexes");
            let spine = super::expr(&mut TokenSlice::new(&lexed.code), 0);
            let oracle = super::super::expr::expr_recursive(&mut TokenSlice::new(&lexed.code), 0);
            match (spine, oracle) {
                (Ok(s), Ok(o)) => assert_eq!(s, o, "AST divergence on {src:?}"),
                (s, o) => panic!("verdict divergence on {src:?}: spine={s:?} oracle={o:?}"),
            }
        }
    }

    /// The retired cascade's depth guards still fire (its own contract as the oracle — it may
    /// only run on inputs shallow enough for it, and these prove the guards that keep that honest).
    /// The spine parses all three shapes fine.
    #[test]
    fn oracle_depth_guards_still_fire_where_the_spine_parses() {
        let deep_parens = format!("{}1{}", "(".repeat(80), ")".repeat(80));
        let deep_unary = format!("{}1", "-".repeat(80));
        let deep_comp = format!("[{}1{}]", "each [".repeat(80), "]".repeat(80));
        for src in [deep_parens, deep_unary, deep_comp] {
            let lexed = crate::lex(&src).expect("lexes");
            let spine = super::expr(&mut TokenSlice::new(&lexed.code), 0);
            let oracle = super::super::expr::expr_recursive(&mut TokenSlice::new(&lexed.code), 0);
            assert!(spine.is_ok(), "spine parses {}…", &src[..20]);
            assert!(oracle.is_err(), "oracle guards {}…", &src[..20]);
        }
    }

    /// Both parsers must also agree on REJECTION for the operand-position prefix forms.
    #[test]
    fn spine_matches_oracle_on_rejections() {
        for src in [
            "1 + function(x) x",
            "1 + let(a = 1) a",
            "2 * assert(x)",
            "-echo(x)",
            "f(",
            "[1, 2",
            "a ? b",
            "[0:",
            "let(a = 1)",     // a `let` needs a body
            "function(1) 2",  // a parameter must be a name
            "a.(",            // a member must be a name
            "[for (i = r i]", // missing ')' of the for bindings
            "assert(",
        ] {
            let lexed = crate::lex(src).expect("lexes");
            let spine = super::expr(&mut TokenSlice::new(&lexed.code), 0);
            let oracle = super::super::expr::expr_recursive(&mut TokenSlice::new(&lexed.code), 0);
            assert!(
                spine.is_err() && oracle.is_err(),
                "both must reject {src:?}: spine={spine:?} oracle={oracle:?}"
            );
        }
    }
}
