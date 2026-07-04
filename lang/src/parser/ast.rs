//! The scad-rs abstract syntax tree.
//!
//! Owned (not source-borrowing) so the AST outlives the source and can feed the evaluator, the
//! content-addressed cache, and threads freely. Every node carries a byte [`Span`] into the
//! original source (from winnow's `.with_span()`), so diagnostics and the customizer can point back.
//!
//! Scope is the G.3.3 tracer bullet: the full expression grammar + module instantiation +
//! assignment. Constructs beyond that (module/function defs, if/else, `use`, the function-literal /
//! `let` / `assert` / `echo` EXPRESSION forms, list comprehensions) are parsed to a LOUD
//! [`Error::Unimplemented`](crate::Error::Unimplemented), never silently dropped — they land in
//! H.2/H.3. Conformance reference: OpenSCAD `src/core/parser.y`.

use core::ops::Range;

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
        /// The bound name.
        name: String,
        /// The value expression.
        value: Expr,
    },
    /// A module call as a statement (with its children / modifiers).
    Module(ModuleInstantiation),
    /// A `{ … }` block of statements (parser.y:187 / `inner_input`).
    Block(Vec<Stmt>),
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
    /// The parameter name for a named argument; `None` for a positional one.
    pub name: Option<String>,
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
