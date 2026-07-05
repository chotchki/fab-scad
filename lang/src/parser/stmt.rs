//! Statement + program parsing (parser.y:185-332).
//!
//! The keyword module-ids `for`/`let`/`assert`/`echo`/`each` are ordinary module calls
//! (parser.y:316-323), so they need no dedicated statement grammar. Two statement CONTEXTS are
//! threaded via `allow_defs` (H.2.6): `inner_input` (the file top level + a module-def body) admits
//! module/function DEFS; `child_statements` (a module-call or `if` child subtree) does NOT — a def
//! there is a parse error, matching `parser.y`'s split grammar. `use`/`include` stay lenient in both
//! contexts (their placement is validated later by the I.2 loader).

use winnow::error::ModalResult;
use winnow::stream::Location;

use super::ast::{Modifiers, ModuleInstantiation, Program, Stmt, StmtKind};
use super::expr::{arg_list, expr, param_list};
use super::{MAX_DEPTH, Tokens, bail, bump, expect, peek_kind, peek_kind2};
use crate::lexer::TokenKind;

/// The top-level program: statements up to the `Eof` sentinel (parser.y:174-183).
pub(crate) fn program(i: &mut Tokens<'_, '_>) -> ModalResult<Program> {
    let mut stmts = Vec::new();
    while peek_kind(i) != Some(TokenKind::Eof) {
        stmts.push(statement(i, 0, true)?); // the file top level is `inner_input`: defs allowed
    }
    bump(i)?; // consume Eof so `.parse()` sees the whole stream consumed
    Ok(Program { stmts })
}

/// One statement (parser.y:185-219). `allow_defs` selects the context: `true` = `inner_input`
/// (module/function defs legal), `false` = `child_statements` (a def here is a parse error, H.2.6).
fn statement(i: &mut Tokens<'_, '_>, depth: usize, allow_defs: bool) -> ModalResult<Stmt> {
    if depth >= MAX_DEPTH {
        return bail(i, "statements nested too deeply");
    }
    if !allow_defs && matches!(peek_kind(i), Some(TokenKind::Module | TokenKind::Function)) {
        return bail(
            i,
            "module/function definitions are only allowed at the top level or in a module body, not inside a child block",
        );
    }
    let start = i.current_token_start();
    match peek_kind(i) {
        Some(TokenKind::Semi | TokenKind::Eot) => {
            bump(i)?;
            Ok(Stmt {
                kind: StmtKind::Empty,
                span: start..i.previous_token_end(),
            })
        }
        Some(TokenKind::LBrace) => {
            // A nested block inherits the current context (`inner_input` stays `inner_input`).
            let stmts = block(i, depth, allow_defs)?;
            Ok(Stmt {
                kind: StmtKind::Block(stmts),
                span: start..i.previous_token_end(),
            })
        }
        Some(TokenKind::Module) => module_def(i, depth),
        Some(TokenKind::Function) => function_def(i, depth),
        Some(TokenKind::If) => ifelse_statement(i, depth),
        Some(TokenKind::Use(path)) => {
            let kind = StmtKind::Use(path.to_string());
            bump(i)?;
            Ok(Stmt {
                kind,
                span: start..i.previous_token_end(),
            })
        }
        Some(TokenKind::Include(path)) => {
            let kind = StmtKind::Include(path.to_string());
            bump(i)?;
            Ok(Stmt {
                kind,
                span: start..i.previous_token_end(),
            })
        }
        Some(TokenKind::Ident(name)) if peek_kind2(i) == Some(TokenKind::Eq) => {
            assignment(i, name, depth)
        }
        Some(
            TokenKind::Bang
            | TokenKind::Hash
            | TokenKind::Percent
            | TokenKind::Star
            | TokenKind::Ident(_)
            | TokenKind::For
            | TokenKind::Let
            | TokenKind::Assert
            | TokenKind::Echo
            | TokenKind::Each,
        ) => module_instantiation(i, depth),
        _ => bail(i, "a statement"),
    }
}

/// `name = expr;` (parser.y:227). The caller has verified `name` is an identifier followed by `=`.
fn assignment(i: &mut Tokens<'_, '_>, name: &str, depth: usize) -> ModalResult<Stmt> {
    let start = i.current_token_start();
    bump(i)?; // name
    bump(i)?; // '='
    let value = expr(i, depth + 1)?;
    expect(i, TokenKind::Semi, "';' after an assignment")?;
    Ok(Stmt {
        kind: StmtKind::Assignment {
            name: name.to_string(),
            value,
        },
        span: start..i.previous_token_end(),
    })
}

/// `module name(params) body` (parser.y:193). The body is a single `statement` (usually a block —
/// which is `inner_input`, so nested defs are legal there, unlike a module-call child block; see
/// H.2.6). The caller has verified the leading `module` keyword.
fn module_def(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Stmt> {
    let start = i.current_token_start();
    bump(i)?; // 'module'
    let name = def_name(i, "a module name after `module`")?;
    expect(i, TokenKind::LParen, "'(' after a module name")?;
    let params = param_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the parameter list")?;
    let body = statement(i, depth + 1, true)?; // a module body is `inner_input`: nested defs legal
    Ok(Stmt {
        kind: StmtKind::ModuleDef {
            name,
            params,
            body: Box::new(body),
        },
        span: start..i.previous_token_end(),
    })
}

/// `function name(params) = body;` (parser.y:207). The caller has verified the `function` keyword.
fn function_def(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Stmt> {
    let start = i.current_token_start();
    bump(i)?; // 'function'
    let name = def_name(i, "a function name after `function`")?;
    expect(i, TokenKind::LParen, "'(' after a function name")?;
    let params = param_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of the parameter list")?;
    expect(i, TokenKind::Eq, "'=' in a function definition")?;
    let body = expr(i, depth + 1)?;
    expect(i, TokenKind::Semi, "';' after a function definition")?;
    Ok(Stmt {
        kind: StmtKind::FunctionDef { name, params, body },
        span: start..i.previous_token_end(),
    })
}

/// The name in a `module`/`function` def — a plain identifier (parser.y's `TOK_ID`). Keyword and
/// `$`-prefixed names are rejected: a def can't be named `for` or `$x`.
fn def_name(i: &mut Tokens<'_, '_>, label: &'static str) -> ModalResult<String> {
    match peek_kind(i) {
        Some(TokenKind::Ident(n)) => {
            let name = n.to_string();
            bump(i)?;
            Ok(name)
        }
        _ => bail(i, label),
    }
}

/// `mods name(args) child` (parser.y:234-332). The `! # % *` prefixes stack into flags.
fn module_instantiation(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Stmt> {
    // Guards the single-child chain `a() a() … cube();`, which recurses here (not via `statement`).
    if depth >= MAX_DEPTH {
        return bail(i, "module calls nested too deeply");
    }
    let start = i.current_token_start();
    let mut modifiers = Modifiers::default();
    loop {
        match peek_kind(i) {
            Some(TokenKind::Bang) => modifiers.root = true,
            Some(TokenKind::Hash) => modifiers.highlight = true,
            Some(TokenKind::Percent) => modifiers.background = true,
            Some(TokenKind::Star) => modifiers.disable = true,
            _ => break,
        }
        bump(i)?;
    }
    let name = module_id(i)?;
    expect(i, TokenKind::LParen, "'(' after a module name")?;
    let args = arg_list(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of a module call")?;
    let children = child_statement(i, depth + 1)?;
    Ok(Stmt {
        kind: StmtKind::Module(ModuleInstantiation {
            modifiers,
            name,
            args,
            children,
        }),
        span: start..i.previous_token_end(),
    })
}

/// The module name — a plain identifier or a keyword module-id (parser.y:316-323).
fn module_id(i: &mut Tokens<'_, '_>) -> ModalResult<String> {
    let name = match peek_kind(i) {
        Some(TokenKind::Ident(n)) => n.to_string(),
        Some(TokenKind::For) => "for".to_string(),
        Some(TokenKind::Let) => "let".to_string(),
        Some(TokenKind::Assert) => "assert".to_string(),
        Some(TokenKind::Echo) => "echo".to_string(),
        Some(TokenKind::Each) => "each".to_string(),
        _ => return bail(i, "a module name"),
    };
    bump(i)?;
    Ok(name)
}

/// The child following a module head: `;` (none), `{ … }` (a block), an `if`/`else` (also a
/// `module_instantiation` in the grammar), or a single nested instantiation (parser.y:306-313).
fn child_statement(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Stmt>> {
    match peek_kind(i) {
        Some(TokenKind::Semi) => {
            bump(i)?;
            Ok(Vec::new())
        }
        Some(TokenKind::LBrace) => block(i, depth, false), // a child block is `child_statements`
        Some(TokenKind::If) => Ok(vec![ifelse_statement(i, depth + 1)?]),
        _ => {
            let child = module_instantiation(i, depth + 1)?;
            Ok(vec![child])
        }
    }
}

/// `if (cond) then [else els]` (parser.y:271-298). The `else` is bound greedily to the NEAREST `if`
/// — bison resolves the dangling-else the same way (`%prec NO_ELSE` shifts `else`). `else if` chains
/// recurse (each `else` re-enters via [`child_statement`]), so the depth guard is load-bearing.
fn ifelse_statement(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Stmt> {
    if depth >= MAX_DEPTH {
        return bail(i, "if/else nested too deeply");
    }
    let start = i.current_token_start();
    bump(i)?; // 'if'
    expect(i, TokenKind::LParen, "'(' after `if`")?;
    let cond = expr(i, depth + 1)?;
    expect(i, TokenKind::RParen, "closing ')' of an `if` condition")?;
    let then = child_statement(i, depth + 1)?;
    let els = if peek_kind(i) == Some(TokenKind::Else) {
        bump(i)?; // 'else'
        child_statement(i, depth + 1)?
    } else {
        Vec::new()
    };
    Ok(Stmt {
        kind: StmtKind::If { cond, then, els },
        span: start..i.previous_token_end(),
    })
}

/// A `{ … }` block of statements (parser.y:187/300-308). `allow_defs` carries the context: a
/// top-level or module-body block is `inner_input` (`true`); a module-call/`if` child block is
/// `child_statements` (`false`).
fn block(i: &mut Tokens<'_, '_>, depth: usize, allow_defs: bool) -> ModalResult<Vec<Stmt>> {
    bump(i)?; // '{'
    let mut stmts = Vec::new();
    while !matches!(
        peek_kind(i),
        Some(TokenKind::RBrace | TokenKind::Eof) | None
    ) {
        stmts.push(statement(i, depth + 1, allow_defs)?);
    }
    expect(i, TokenKind::RBrace, "closing '}' of a block")?;
    Ok(stmts)
}
