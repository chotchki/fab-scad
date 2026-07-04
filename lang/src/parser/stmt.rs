//! Statement + program parsing — the G.3.3 subset (parser.y:185-332).
//!
//! Module/function DEFS, `if`/`else`, and `use`/`include` resolution are recognized and fail LOUD
//! (H.2), never silently. The keyword module-ids `for`/`let`/`assert`/`echo`/`each` are ordinary
//! module calls (parser.y:316-323), so they need no dedicated statement grammar.

use winnow::error::ModalResult;
use winnow::stream::Location;

use super::ast::{Modifiers, ModuleInstantiation, Program, Stmt, StmtKind};
use super::expr::{arg_list, expr};
use super::{MAX_DEPTH, Tokens, bail, bump, expect, peek_kind, peek_kind2};
use crate::lexer::TokenKind;

/// The top-level program: statements up to the `Eof` sentinel (parser.y:174-183).
pub(crate) fn program(i: &mut Tokens<'_, '_>) -> ModalResult<Program> {
    let mut stmts = Vec::new();
    while peek_kind(i) != Some(TokenKind::Eof) {
        stmts.push(statement(i, 0)?);
    }
    bump(i)?; // consume Eof so `.parse()` sees the whole stream consumed
    Ok(Program { stmts })
}

/// One statement (parser.y:185-219). Deferred forms (defs, `if`, `use`/`include`) fail LOUD.
fn statement(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Stmt> {
    if depth >= MAX_DEPTH {
        return bail(i, "statements nested too deeply");
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
            let stmts = block(i, depth)?;
            Ok(Stmt {
                kind: StmtKind::Block(stmts),
                span: start..i.previous_token_end(),
            })
        }
        Some(TokenKind::Module) => bail(i, "module definitions are not yet implemented (H.2)"),
        Some(TokenKind::Function) => bail(i, "function definitions are not yet implemented (H.2)"),
        Some(TokenKind::If) => bail(i, "if/else statements are not yet implemented (H.2)"),
        Some(TokenKind::Use(_)) => bail(i, "use<> resolution is not yet implemented (H.2)"),
        Some(TokenKind::Include(_)) => bail(i, "include<> resolution is not yet implemented (H.2)"),
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

/// The child following a module head: `;` (none), `{ … }` (a block), or a single nested
/// instantiation (parser.y:306-313).
fn child_statement(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Stmt>> {
    match peek_kind(i) {
        Some(TokenKind::Semi) => {
            bump(i)?;
            Ok(Vec::new())
        }
        Some(TokenKind::LBrace) => block(i, depth),
        _ => {
            let child = module_instantiation(i, depth + 1)?;
            Ok(vec![child])
        }
    }
}

/// A `{ … }` block of statements (parser.y:187/300-308).
fn block(i: &mut Tokens<'_, '_>, depth: usize) -> ModalResult<Vec<Stmt>> {
    bump(i)?; // '{'
    let mut stmts = Vec::new();
    while !matches!(
        peek_kind(i),
        Some(TokenKind::RBrace | TokenKind::Eof) | None
    ) {
        stmts.push(statement(i, depth + 1)?);
    }
    expect(i, TokenKind::RBrace, "closing '}' of a block")?;
    Ok(stmts)
}
