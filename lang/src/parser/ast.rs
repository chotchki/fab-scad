//! The scad-rs abstract syntax tree.
//!
//! Owned (not source-borrowing) so the AST outlives the source and can feed the evaluator, the
//! content-addressed cache, and threads freely. Every node carries a byte [`Span`] into the
//! original source (from winnow's `.with_span()`), so diagnostics and the customizer can point back.
//!
//! Phase H completes the grammar: module/function defs, if/else, use/include, the function-literal
//! / `let` / `assert` / `echo` EXPRESSION forms, and list comprehensions. Anything not yet parsed
//! fails LOUD ([`Error::Unimplemented`](crate::Error::Unimplemented)) rather than silently dropping.
//! Conformance reference: OpenSCAD `src/core/parser.y`; the live production ledger (what's parsed vs
//! deferred) is `lang/docs/grammar-inventory.md`.

use core::ops::Range;
use std::rc::Rc;

/// A byte range into the original source.
pub type Span = Range<usize>;

/// A parsed program: the top-level statement sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// Statements in source order.
    pub stmts: Vec<Stmt>,
}

/// A statement plus its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    /// What this statement is.
    pub kind: StmtKind,
    /// Byte span into the source.
    pub span: Span,
}

/// The classification of a statement (the G.3.3 subset).
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// A lone `;`.
    Empty,
    /// `name = expr;` (parser.y:227).
    Assignment {
        /// The bound name — an `Rc<str>` so hoisting it into a scope is a refcount bump, not a copy (N.2b).
        name: Rc<str>,
        /// The value expression.
        value: Expr,
    },
    /// A module call as a statement (with its children / modifiers).
    Module(ModuleInstantiation),
    /// A `{ … }` block of statements (parser.y:187 / `inner_input`).
    Block(Vec<Stmt>),
    /// `module name(params) body` (parser.y:193). The body is a single statement (usually a block).
    ModuleDef {
        /// The module name.
        name: String,
        /// The formal parameters (positional order; each may carry a default).
        params: Vec<Parameter>,
        /// The body — one statement.
        body: Box<Stmt>,
    },
    /// `function name(params) = body;` (parser.y:207).
    FunctionDef {
        /// The function name.
        name: String,
        /// The formal parameters (positional order; each may carry a default).
        params: Vec<Parameter>,
        /// The body expression.
        body: Expr,
    },
    /// `use <path>` (parser.y:176 / lexer.l:153). Imports the file's modules + functions (NOT its
    /// variables). Parse-only here: the raw path is captured; RESOLUTION is I.2's loader. `use` is
    /// top-level-only in OpenSCAD — we accept it as a statement anywhere (a benign widening).
    Use(String),
    /// `include <path>` (lexer.l:139). OpenSCAD splices the file textually in the LEXER; we emit a
    /// node carrying the raw path and splice in I.2's loader (parse stays zero-IO).
    Include(String),
    /// `if (cond) then [else els]` (parser.y:271-298). Grammatically an `ifelse_statement`, itself a
    /// `module_instantiation` — so `if` is legal wherever a module call is (top level OR a child),
    /// AND the `! # % *` prefixes apply to it (AA.1 — the sustainment census caught `*if` rejected).
    /// `then`/`els` are child-statement lists (0/1/many, like module children); an empty `els` means
    /// no `else`. `else if` chains fall out naturally: `els` is `[If { … }]`.
    If {
        /// The `! # % *` prefixes (same semantics as on a module call; they stack).
        modifiers: Modifiers,
        /// The condition expression.
        cond: Expr,
        /// The then-branch children.
        then: Vec<Stmt>,
        /// The else-branch children (empty ⇒ no `else`).
        els: Vec<Stmt>,
    },
}

/// A module/function formal parameter: `id` or `id = default` (parser.y:666-677). Shared by module
/// defs, function defs, and function-literal expressions. A `$`-prefixed name is a special-variable
/// parameter (dynamic-scope injection, e.g. `module m($fn = 8)`), so the name may begin with `$`.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    /// The parameter name (may begin with `$`). An `Rc<str>` so binding it per CALL is a refcount bump,
    /// not a fresh `String` copy (N.2b) — the call-heavy hot path.
    pub name: Rc<str>,
    /// The default-value expression, present iff the `id = expr` form was used.
    pub default: Option<Expr>,
    /// Byte span of the whole parameter.
    pub span: Span,
}

/// A module instantiation: `mods name(args) child` (parser.y:234-332).
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleInstantiation {
    /// The `! # % *` prefixes (they stack, parser.y:235-254).
    pub modifiers: Modifiers,
    /// The module name — a plain identifier, or one of the keyword module-ids
    /// `for`/`let`/`assert`/`echo`/`each` (parser.y:316-323).
    pub name: String,
    /// Call arguments (positional and/or named).
    pub args: Vec<Arg>,
    /// Children: empty for `;`, one for a single child, many for a `{ … }` block (parser.y:306-313).
    pub children: Vec<Stmt>,
}

/// The four module modifier prefixes (parser.y:235-254). They compose, so all four are flags.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the modifiers `! # % *` are four genuinely-independent flags that stack (parser.y:235-254)"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// `!` — render only this subtree (root).
    pub root: bool,
    /// `#` — highlight/debug.
    pub highlight: bool,
    /// `%` — background/transparent.
    pub background: bool,
    /// `*` — disable this subtree.
    pub disable: bool,
}

/// One call argument: positional (`name` = `None`) or named `name = expr` (parser.y:700-710).
/// `$`-args (`$fn = 8`) are just named args whose name begins with `$`.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// The parameter name for a named argument; `None` for a positional one. An `Rc<str>` so a `let`/`for`
    /// comprehension loop-var binding — the `lc_for` hot path — clones a refcount, not a `String` (N.2b).
    pub name: Option<Rc<str>>,
    /// The argument value.
    pub value: Expr,
    /// Byte span of the whole argument.
    pub span: Span,
}

/// An expression plus its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What this expression is.
    pub kind: ExprKind,
    /// Byte span into the source.
    pub span: Span,
}

impl Expr {
    /// Is this expression a compile-time LITERAL — upstream's `Expression::isLiteral()` virtual?
    ///
    /// A pure syntax question (no eval, no scope), which is why it lives on the AST. Only one caller
    /// today: the `Parameter "x" is overwritten with a literal` warning, which upstream fires only when
    /// the shadowing assignment can be decided statically. The rules are upstream's, each oracle-checked:
    /// a scalar literal is one; a UNARY op forwards to its operand (`-5` and `!true` warn, so does `--5`);
    /// a vector or range is one iff EVERY element is (`[1,2]` warns, `[1,y]` does not); and a BINARY op
    /// never is, however constant it looks (`1+1` does not warn). Everything else — a call, an index, a
    /// comprehension, an identifier, `$fn` — is not.
    /// Walks an explicit stack rather than the host one: an `Expr` tree is attacker-shaped (a vector of
    /// vectors nests as deep as the source says), and this crate's rule is that AST walks don't recurse.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        let mut stack = vec![self];
        while let Some(expr) = stack.pop() {
            match &expr.kind {
                ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
                ExprKind::Unary { operand, .. } => stack.push(operand),
                ExprKind::Vector(items) => stack.extend(items.iter()),
                ExprKind::Range { start, step, end } => {
                    stack.push(start);
                    stack.push(end);
                    stack.extend(step.as_deref());
                }
                // Anything with a runtime component — a call, an index, an identifier, a comprehension,
                // and notably a BINARY op however constant it looks — makes the whole tree non-literal.
                _ => return false,
            }
        }
        true
    }
}

/// The classification of an expression (parser.y:334-567).
///
/// `Default` (= [`ExprKind::Undef`]) exists only so the non-recursive [`Drop`] for [`Expr`] can
/// blank a node with `mem::take`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ExprKind {
    /// A number literal (already decoded to `f64`).
    Num(f64),
    /// A string literal (already escape-decoded).
    Str(String),
    /// `true` / `false`.
    Bool(bool),
    /// `undef`.
    #[default]
    Undef,
    /// A variable reference (a `Lookup`); the name includes a leading `$` for special vars.
    Ident(String),
    /// A prefix unary op: `- + ! ~` (parser.y:467-491).
    Unary {
        /// The operator.
        op: UnOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// A binary op (parser.y:362-464, 494-500).
    Binary {
        /// The operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// C-style ternary `cond ? then : els`, right-associative (parser.y:341).
    Ternary {
        /// The condition.
        cond: Box<Expr>,
        /// The value when true.
        then: Box<Expr>,
        /// The value when false.
        els: Box<Expr>,
    },
    /// Indexing `base[index]` (parser.y:509).
    Index {
        /// The base expression.
        base: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
    },
    /// Member access `base.field` (parser.y:513).
    Member {
        /// The base expression.
        base: Box<Expr>,
        /// The member name.
        field: String,
    },
    /// A function call `callee(args)` (parser.y:504).
    Call {
        /// The callee (usually an [`ExprKind::Ident`]).
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Arg>,
    },
    /// A vector/list literal `[a, b, c]` (parser.y:559-563).
    Vector(Vec<Expr>),
    /// A range `[start : end]` or `[start : step : end]` (parser.y:551-555; middle is the STEP).
    Range {
        /// The start value.
        start: Box<Expr>,
        /// The step, if the three-part form was used.
        step: Option<Box<Expr>>,
        /// The end value.
        end: Box<Expr>,
    },
    /// A function-literal expression `function(params) body` (parser.y:336). The body is a full
    /// `expr` (greedy), so `function(x) x + 1` binds as `function(x) (x + 1)`.
    FunctionLiteral {
        /// The formal parameters.
        params: Vec<Parameter>,
        /// The body expression.
        body: Box<Expr>,
    },
    /// A `let(bindings) body` expression (parser.y:345) — binds `name = value` args, then evaluates
    /// `body` in that scope. Distinct from the `let` MODULE call (statement position); this is the
    /// expression form.
    Let {
        /// The `name = value` bindings.
        bindings: Vec<Arg>,
        /// The body evaluated in the extended scope.
        body: Box<Expr>,
    },
    /// An `assert(args) body?` expression (parser.y:350). `body` is OPTIONAL (`expr_or_empty`):
    /// `assert(cond)` checks and passes through, `assert(cond) x` checks then yields `x`.
    Assert {
        /// The assertion arguments (condition, optional message).
        args: Vec<Arg>,
        /// The optional pass-through body.
        body: Option<Box<Expr>>,
    },
    /// An `echo(args) body?` expression (parser.y:355). `body` is OPTIONAL (`expr_or_empty`).
    Echo {
        /// The echo arguments.
        args: Vec<Arg>,
        /// The optional pass-through body.
        body: Option<Box<Expr>>,
    },
    /// A list-comprehension `for (bindings) body` (parser.y:592) — iterate `bindings`, contributing
    /// `body` each step. Only produced as a vector element ([`ExprKind::Vector`]). `body` is itself a
    /// vector element, so comprehensions NEST (`[for(i=r) for(j=r) [i,j]]`).
    LcFor {
        /// The `name = range/list` iteration bindings.
        bindings: Vec<Arg>,
        /// The per-step contribution (a vector element).
        body: Box<Expr>,
    },
    /// A C-style list-comprehension `for (init; cond; update) body` (parser.y:597).
    LcForC {
        /// The initializer bindings.
        init: Vec<Arg>,
        /// The loop condition.
        cond: Box<Expr>,
        /// The per-step update bindings.
        update: Vec<Arg>,
        /// The per-step contribution.
        body: Box<Expr>,
    },
    /// A list-comprehension `each body` (parser.y:588) — SPLICE `body`'s list into the enclosing
    /// vector (flatten one level) rather than nesting it.
    LcEach(Box<Expr>),
    /// A list-comprehension `if (cond) then [else els]` (parser.y:603-607) — conditionally contribute
    /// `then` (or `els`). Distinct from the STATEMENT [`StmtKind::If`]: this yields list elements.
    LcIf {
        /// The condition.
        cond: Box<Expr>,
        /// The contribution when true.
        then: Box<Expr>,
        /// The contribution when false (absent ⇒ nothing).
        els: Option<Box<Expr>>,
    },
}

/// A prefix unary operator (parser.y:467-491).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-` negate.
    Neg,
    /// `+` (a no-op in OpenSCAD, kept for fidelity).
    Pos,
    /// `!` logical not.
    Not,
    /// `~` bitwise not.
    BitNot,
}

/// A binary operator, in parser.y's precedence order (loosest [`BinOp::Or`] to tightest
/// [`BinOp::Pow`]). Note bitwise `|`/`&` sit BETWEEN comparison and shift, not below comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `||`.
    Or,
    /// `&&`.
    And,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `|` (bitwise or).
    BitOr,
    /// `&` (bitwise and).
    BitAnd,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%` (modulo).
    Mod,
    /// `^` (power), right-associative.
    Pow,
}

impl Drop for Expr {
    fn drop(&mut self) {
        // The parser builds a left-associative chain (`1+1+…`, `a.b.c…`, `a[0][0]…`) ITERATIVELY,
        // so it never overflows the stack while PARSING — but the resulting deep left-spine WOULD
        // overflow a naive recursive `Drop` when freed. Dismantle it via an explicit work-stack
        // instead: the AST's teardown mirror of the evaluator's explicit stack (no host recursion).
        // Every `Expr` that actually drops in here has had its children moved out first, so its own
        // `Drop` runs on an `Undef` and is O(1).
        let mut stack = vec![core::mem::take(&mut self.kind)];
        while let Some(kind) = stack.pop() {
            match kind {
                ExprKind::Unary { operand, .. } => stack.push(take_kind(*operand)),
                ExprKind::Binary { lhs, rhs, .. } => {
                    stack.push(take_kind(*lhs));
                    stack.push(take_kind(*rhs));
                }
                ExprKind::Ternary { cond, then, els } => {
                    stack.push(take_kind(*cond));
                    stack.push(take_kind(*then));
                    stack.push(take_kind(*els));
                }
                ExprKind::Index { base, index } => {
                    stack.push(take_kind(*base));
                    stack.push(take_kind(*index));
                }
                ExprKind::Member { base, .. } => stack.push(take_kind(*base)),
                ExprKind::Call { callee, args } => {
                    stack.push(take_kind(*callee));
                    stack.extend(args.into_iter().map(|a| take_kind(a.value)));
                }
                ExprKind::Vector(elems) => stack.extend(elems.into_iter().map(take_kind)),
                ExprKind::Range { start, step, end } => {
                    stack.push(take_kind(*start));
                    if let Some(step) = step {
                        stack.push(take_kind(*step));
                    }
                    stack.push(take_kind(*end));
                }
                ExprKind::FunctionLiteral { params, body } => {
                    stack.push(take_kind(*body));
                    stack.extend(params.into_iter().filter_map(|p| p.default).map(take_kind));
                }
                ExprKind::Let { bindings, body } => {
                    stack.push(take_kind(*body));
                    stack.extend(bindings.into_iter().map(|a| take_kind(a.value)));
                }
                ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
                    if let Some(body) = body {
                        stack.push(take_kind(*body));
                    }
                    stack.extend(args.into_iter().map(|a| take_kind(a.value)));
                }
                ExprKind::LcFor { bindings, body } => {
                    stack.push(take_kind(*body));
                    stack.extend(bindings.into_iter().map(|a| take_kind(a.value)));
                }
                ExprKind::LcForC {
                    init,
                    cond,
                    update,
                    body,
                } => {
                    stack.push(take_kind(*body));
                    stack.push(take_kind(*cond));
                    stack.extend(init.into_iter().map(|a| take_kind(a.value)));
                    stack.extend(update.into_iter().map(|a| take_kind(a.value)));
                }
                ExprKind::LcEach(body) => stack.push(take_kind(*body)),
                ExprKind::LcIf { cond, then, els } => {
                    stack.push(take_kind(*cond));
                    stack.push(take_kind(*then));
                    if let Some(els) = els {
                        stack.push(take_kind(*els));
                    }
                }
                ExprKind::Num(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::Undef
                | ExprKind::Ident(_) => {}
            }
        }
    }
}

/// Take an `Expr`'s kind, leaving it `Undef` so the `Expr`'s own `Drop` is a no-op as it falls here.
fn take_kind(mut e: Expr) -> ExprKind {
    core::mem::take(&mut e.kind)
}

/// AR.17.2 — the CANONICAL child enumeration for expression PATH-ADDRESSING, in source order.
/// The transpiler computes a child-index path to a `FunctionLiteral` against its parse of the
/// reference; the evaluator resolves the same path against the user's fingerprint-proven
/// definition. Fingerprint equality guarantees node-for-node structural identity (spans aside),
/// so a path transfers iff BOTH sides enumerate children identically — which is why this is the
/// ONE definition, public, and exhaustive: a new variant must decide its ordering here or
/// nothing compiles.
#[must_use]
pub fn expr_children(e: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    match &e.kind {
        ExprKind::Num(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Undef
        | ExprKind::Ident(_) => {}
        ExprKind::Unary { operand, .. } => out.push(&**operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            out.push(&**lhs);
            out.push(&**rhs);
        }
        ExprKind::Ternary { cond, then, els } => {
            out.push(&**cond);
            out.push(&**then);
            out.push(&**els);
        }
        ExprKind::Index { base, index } => {
            out.push(&**base);
            out.push(&**index);
        }
        ExprKind::Member { base, .. } => out.push(&**base),
        ExprKind::Call { callee, args } => {
            out.push(&**callee);
            out.extend(args.iter().map(|a| &a.value));
        }
        ExprKind::Vector(elems) => out.extend(elems.iter()),
        ExprKind::Range { start, step, end } => {
            out.push(&**start);
            if let Some(step) = step {
                out.push(&**step);
            }
            out.push(&**end);
        }
        ExprKind::FunctionLiteral { params, body } => {
            out.extend(params.iter().filter_map(|p| p.default.as_ref()));
            out.push(&**body);
        }
        ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
            out.extend(bindings.iter().map(|a| &a.value));
            out.push(&**body);
        }
        ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
            out.extend(args.iter().map(|a| &a.value));
            if let Some(body) = body {
                out.push(&**body);
            }
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            out.extend(init.iter().map(|a| &a.value));
            out.push(&**cond);
            out.extend(update.iter().map(|a| &a.value));
            out.push(&**body);
        }
        ExprKind::LcEach(inner) => out.push(&**inner),
        ExprKind::LcIf { cond, then, els } => {
            out.push(&**cond);
            out.push(&**then);
            if let Some(els) = els {
                out.push(&**els);
            }
        }
    }
    out
}

/// The `i`th child under [`expr_children`]'s ordering.
#[must_use]
pub fn expr_child(e: &Expr, i: usize) -> Option<&Expr> {
    expr_children(e).into_iter().nth(i)
}

/// The child-index path from `root` to `target` — by node IDENTITY (`ptr::eq`), not equality,
/// because two syntactically equal literals in one body are DIFFERENT mint targets. Depth-first
/// in [`expr_children`] order; recursion is bounded by the parser's own nesting cap.
#[must_use]
pub fn find_expr_path(root: &Expr, target: &Expr) -> Option<Vec<usize>> {
    if core::ptr::eq(root, target) {
        return Some(Vec::new());
    }
    for (i, child) in expr_children(root).into_iter().enumerate() {
        if let Some(mut path) = find_expr_path(child, target) {
            path.insert(0, i);
            return Some(path);
        }
    }
    None
}
