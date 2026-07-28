//! AR.5 — the transpiler's ANALYSIS pass: derive, from a function's verbatim reference source, what
//! `intrinsics::Entry` hand-maintains — the const / dep / builtin guard sets.
//!
//! Every AN finding was the hand-maintained half of an `Entry` disagreeing with what the reference
//! actually does at runtime. A transpiler generates the native FROM the reference, so it must also
//! generate the guards from the same read — one source, no drift. This module is that read.
//!
//! DERIVED, not proven: the walk is SYNTACTIC, so it over-approximates the hand lists, which are
//! transitively closed by the author over ACCEPTED ARG SHAPES (semantic reachability pruning —
//! `select` excludes `all_nonzero` because no accepted shape reaches it). Over-approximation is the
//! SAFE direction for a guard: more names checked means the entry wires less often, never that it
//! answers wrong. The registry comparison test pins both directions: hand ⊆ derived (a hand name
//! the walk misses is an analyzer BUG), and derived-minus-hand is an explicit, reasoned allowlist
//! rather than silent slack.

#![allow(
    dead_code,
    reason = "AR.5: the AR.6 codegen is the production consumer; the registry comparison test exercises it today"
)]

use std::collections::BTreeSet;
use std::fmt::Write;

use super::value::Value;
use crate::parser::{Arg, Expr, ExprKind, Parameter, StmtKind, parse};

/// What one function's reference source reaches, by name — the raw material for an `Entry`'s guard
/// sets (names only; the guard VALUES are resolved against the island at arm time, not here).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Analysis {
    /// The defined function's name.
    pub(crate) name: String,
    /// Parameter names, in declaration order.
    pub(crate) params: Vec<String>,
    /// Free VALUE-position identifier reads (non-`$`) — the names `consts`/`consts_v` must guard.
    pub(crate) consts: BTreeSet<String>,
    /// CALL-position names that are not builtins — user-function dependencies (`deps`).
    pub(crate) deps: BTreeSet<String>,
    /// CALL-position builtin names (`builtins`) — shadowable, hence guarded.
    pub(crate) builtins: BTreeSet<String>,
}

/// Analyze a single `function name(params) = body;` reference.
///
/// # Errors
/// The reference must parse and contain exactly one function definition (the `Entry::reference`
/// contract) — anything else is a malformed reference, not a valid analysis subject.
pub(crate) fn analyze_function(reference: &str) -> Result<Analysis, String> {
    let prog = parse(reference).map_err(|e| format!("reference does not parse: {e:?}"))?;
    let mut defs = prog.stmts.iter().filter_map(|s| match &s.kind {
        StmtKind::FunctionDef { name, params, body } => Some((name, params, body)),
        _ => None,
    });
    let Some((name, params, body)) = defs.next() else {
        return Err("reference holds no function definition".into());
    };
    if defs.next().is_some() {
        return Err("reference holds more than one function definition".into());
    }

    let mut out = Analysis {
        name: name.clone(),
        params: params.iter().map(|p| p.name.to_string()).collect(),
        ..Analysis::default()
    };
    // Defaults evaluate in the call's binding environment, where every parameter NAME is already
    // declared (`function f(a, b=a)` reads the param, not a global) — so params are in scope for
    // the default walks as well as the body.
    let mut scope: Vec<String> = out.params.clone();
    for p in params {
        if let Some(d) = &p.default {
            walk(d, &mut scope, &mut out);
        }
    }
    walk(body, &mut scope, &mut out);
    // Self-recursion is not a dependency: the entry's own fingerprint already proves that exact
    // definition, and the hand lists never carry it.
    let own = out.name.clone();
    out.deps.remove(&own);
    Ok(out)
}

/// One function's analysis, with `deps` and `builtins` closed TRANSITIVELY over a resolver that
/// maps a dep name to its reference source (the registry + pins, for the comparison test; the
/// library's own definitions, for a future whole-library transpile). An unresolvable dep stays a
/// dep — the fingerprint gate will veto it at arm time, which is the safe failure.
///
/// `consts` are deliberately NOT flattened across the closure. A dep's constant is guarded by the
/// DEP's own entry against the DEP's home island (AN.11 — checking it against the caller's island
/// is exactly the bug that family fixed), so `select` reaching `_EPSILON` through `is_vector` puts
/// the name on `is_vector`'s analysis, never on `select`'s. The registry comparison caught this
/// pass doing the flatten and the hand lists were RIGHT.
/// The `'r` is load-bearing: the resolved source must outlive the call, but it must NOT be tied to
/// the queried NAME's lifetime (elision would infer exactly that and reject every real resolver).
/// A registry-backed resolver hands back `&'static str`; a [`super::library::Library`]-backed one
/// hands back a slice of the file it read, and both satisfy this.
pub(crate) fn analyze_closed<'r>(
    reference: &str,
    resolve: &dyn Fn(&str) -> Option<&'r str>,
) -> Result<Analysis, String> {
    let mut out = analyze_function(reference)?;
    let mut queue: Vec<String> = out.deps.iter().cloned().collect();
    let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
    while let Some(dep) = queue.pop() {
        let Some(src) = resolve(&dep) else { continue };
        let sub = analyze_function(src)?;
        out.builtins.extend(sub.builtins);
        for d in sub.deps {
            if seen.insert(d.clone()) {
                queue.push(d.clone());
            }
            out.deps.insert(d);
        }
    }
    // MUTUAL recursion closes back onto the root (`approx` ↔ its dep) — the root's own name is no
    // more a dependency arriving via a dep than it was as a direct self-call.
    let own = out.name.clone();
    out.deps.remove(&own);
    Ok(out)
}

/// A constant value a generated native BAKES, in a form that can be emitted bit-exactly
/// (`f64::from_bits`) — never a decimal round-trip.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Baked {
    Num(f64),
    NumList(Vec<f64>),
}

impl Baked {
    /// The Rust expression constructing this value, bit-exact.
    fn emit(&self) -> String {
        match self {
            Self::Num(n) => emit_num(*n),
            Self::NumList(xs) => format!(
                "rt::Value::num_list(vec![{}])",
                xs.iter()
                    .map(|x| format!("f64::from_bits({:#x}_u64)", x.to_bits()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn emit_num(n: f64) -> String {
    format!("rt::Value::Num(f64::from_bits({:#x}_u64))", n.to_bits())
}

/// AR.6 — generate the Rust native for one `function name(params) = body;` reference, in the
/// `poc.rs` IDIOM: every operation routes through `ops::apply_binary` / `builtins::apply` — the
/// interpreter's own value algebra — so composition is bit-identical BY CONSTRUCTION and what
/// compilation deletes is the interpretation overhead (scope maps become locals, dispatch becomes
/// direct calls). `baked` supplies the constant VALUES the entry's guards prove at arm time;
/// `dep_fns` names sibling GENERATED natives a dep call may bind to directly.
///
/// # Errors
/// A construct outside the v0 subset (strings, `let`, comprehensions, indexing, asserts …)
/// declines LOUDLY with the construct named — a partial native would be a wrong native.
pub(crate) fn generate_native(
    reference: &str,
    baked: &[(&str, Baked)],
    siblings: &[(String, Vec<String>)],
) -> Result<String, String> {
    let prog = parse(reference).map_err(|e| format!("reference does not parse: {e:?}"))?;
    let Some(StmtKind::FunctionDef { name, params, body }) = prog.stmts.first().map(|s| &s.kind)
    else {
        return Err("reference holds no function definition".into());
    };

    let mut em = Emitter {
        baked,
        siblings,
        locals: Vec::new(),
        fresh: 0,
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "/// Generated native for `{name}` — semantics route through the interpreter's own value\n\
         /// algebra (`ops::`/`builtins::`), bit-identical to the interpreted reference by construction.\n\
         pub(super) fn {name}(args: &[rt::Value]) -> rt::Result<rt::Value> {{\n\
         \x20   // AR.10: past the depth budget, DECLINE to the pure interpreter — explicit stack,\n\
         \x20   // same proven semantics; recursion cannot ride the Rust stack unbounded.\n\
         \x20   let Some(_depth) = rt::DepthGuard::enter() else {{\n\
         \x20       return rt::run_interpreted(FALLBACK_SOURCES, \"{name}\", args);\n\
         \x20   }};\n"
    );
    for (i, p) in params.iter().enumerate() {
        // A duplicate name has NO native shape: `Task::Apply` binds it two-phase (a provided arg
        // beats a later slot's default, AN.6), while Rust `let` shadowing would take the LAST
        // slot unconditionally. Decline; the entry stays interpreted.
        if params[..i].iter().any(|q| q.name == p.name) {
            return Err(format!(
                "{name}: duplicate parameter `{}` — AN.6 two-phase binding has no native shape",
                p.name
            ));
        }
        let getter = if i == 0 {
            "args.first()".to_string()
        } else {
            format!("args.get({i})")
        };
        // Defaults evaluate in the function's lexical BASE, never the growing call scope:
        // `push_call` evals an unfilled slot's default in `base` (the island globals), so `f(a,
        // b=a)` reads the GLOBAL `a` there — not the argument. `em.locals` stays empty until every
        // bind is emitted, which resolves exactly that scope: a baked const binds, a param name
        // declines LOUDLY as a free read.
        let default = match &p.default {
            None => "rt::Value::Undef".to_string(),
            Some(d) => em
                .expr(d)
                .map_err(|e| format!("{name}: default of `{}`: {e}", p.name))?,
        };
        // A cheap default binds eagerly (`unwrap_or`); a constructing one stays lazy.
        let bind = if default == "rt::Value::Undef" {
            format!("{getter}.cloned().unwrap_or(rt::Value::Undef)")
        } else {
            format!("{getter}.cloned().unwrap_or_else(|| {default})")
        };
        let _ = writeln!(out, "    let p_{} = {bind};", p.name);
    }
    for p in params {
        em.locals
            .push((p.name.to_string(), format!("p_{}", p.name)));
    }
    let body_expr = em.expr(body).map_err(|e| format!("{name}: {e}"))?;
    // The `let out` shape keeps clippy quiet when the whole body is a fallible sibling call
    // (`Ok(f(..)?)` would be needless_question_mark).
    let _ = writeln!(out, "    let out = {body_expr};\n    Ok(out)\n}}");
    Ok(out)
}

/// Emission state: the baked constants and callable siblings (name + DECLARED PARAMS, self
/// included — that is how self- and mutual recursion resolve, and how named sibling arguments bind
/// at COMPILE time), plus the LEXICAL SCOPE — scad name to Rust ident, innermost last, so `let`
/// shadowing resolves exactly as the interpreter's scope does.
struct Emitter<'a> {
    baked: &'a [(&'a str, Baked)],
    siblings: &'a [(String, Vec<String>)],
    locals: Vec<(String, String)>,
    fresh: usize,
}

impl Emitter<'_> {
    /// A collision-proof Rust ident for a `let`-bound scad name (shadowing gets a new number).
    fn fresh_ident(&mut self, name: &str) -> String {
        let id = self.fresh;
        self.fresh += 1;
        // scad names may lead with `_` (idx's `_s`) — `l1__s` trips non_snake_case, and the
        // counter already guarantees uniqueness, so the underscores add nothing.
        format!("l{id}_{}", name.trim_start_matches('_'))
    }

    fn expr(&mut self, e: &Expr) -> Result<String, String> {
        use crate::parser::BinOp;
        match &e.kind {
            ExprKind::Num(n) => Ok(emit_num(*n)),
            ExprKind::Bool(b) => Ok(format!("rt::Value::Bool({b})")),
            ExprKind::Undef => Ok("rt::Value::Undef".to_string()),
            // The AST string is already DECODED (the lexer resolved `\n`/`\u{…}` before it got
            // here — the interpreter does `Value::string(s.as_str())` with no further work), so
            // the emitter's job is purely to re-escape it as a Rust literal. `{:?}` is exactly
            // that and it round-trips: every escape `escape_debug` emits is also valid Rust and
            // decodes back to the same bytes.
            ExprKind::Str(s) => Ok(format!("rt::Value::string({s:?})")),
            ExprKind::Ident(name) => {
                if let Some((_, ident)) = self.locals.iter().rev().find(|(n, _)| n == name) {
                    Ok(format!("{ident}.clone()"))
                } else if let Some((_, b)) = self.baked.iter().find(|(n, _)| n == name) {
                    Ok(b.emit())
                } else {
                    Err(format!("free read `{name}` has no baked value"))
                }
            }
            ExprKind::Unary { op, operand } => Ok(format!(
                "rt::apply_unary(rt::UnOp::{op:?}, {})",
                self.expr(operand)?
            )),
            // `&&`/`||` SHORT-CIRCUIT in the interpreter (the stack machine's ShortCircuit task);
            // `apply_binary`'s And/Or arms are the both-evaluated case only. Rust's own `&&`/`||`
            // mirror the laziness exactly.
            ExprKind::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => {
                let sym = if matches!(op, BinOp::And) { "&&" } else { "||" };
                Ok(format!(
                    "rt::Value::Bool({}.is_truthy() {sym} {}.is_truthy())",
                    self.expr(lhs)?,
                    self.expr(rhs)?
                ))
            }
            ExprKind::Binary { op, lhs, rhs } => Ok(format!(
                "rt::apply_binary(rt::BinOp::{op:?}, {}, {})",
                self.expr(lhs)?,
                self.expr(rhs)?
            )),
            ExprKind::Ternary { cond, then, els } => Ok(format!(
                "if {}.is_truthy() {{ {} }} else {{ {} }}",
                self.expr(cond)?,
                self.expr(then)?,
                self.expr(els)?
            )),
            // `list[i]` — the interpreter's own index op carries the semantics (negative /
            // out-of-range → undef, string indexing, both list reprs).
            ExprKind::Index { base, index } => Ok(format!(
                "rt::index({}, &{})",
                self.expr(base)?,
                self.expr(index)?
            )),
            ExprKind::Member { base, field } => {
                Ok(format!("ops::member({}, {field:?})", self.expr(base)?))
            }
            ExprKind::Let { bindings, body } => self.let_expr(bindings, body),
            // Expression `assert`: the control-flow contract (Entry doc) — raise where the
            // interpreter raises; the diagnostic string is a locator, not output, and the message
            // args are NOT evaluated (matches the hand natives; upstream only evaluates them on
            // failure, and no registry reference carries a side-effectful message).
            ExprKind::Assert { args, body } => {
                let Some(cond) = args.first() else {
                    return Err("assert with no condition".into());
                };
                if cond.name.is_some() {
                    return Err("named assert condition".into());
                }
                let c = self.expr(&cond.value)?;
                let b = match body {
                    Some(b) => self.expr(b)?,
                    None => "rt::Value::Undef".to_string(),
                };
                Ok(format!(
                    "{{ if !({c}).is_truthy() {{ return Err(rt::bosl_assert(\"generated\")); }} {b} }}"
                ))
            }
            // `[start : step? : end]` with computed endpoints — the interpreter's own constructor
            // carries the coercion rules.
            ExprKind::Range { start, step, end } => {
                let s = self.expr(start)?;
                let t = match step {
                    Some(t) => self.expr(t)?,
                    None => "rt::Value::Num(f64::from_bits(0x3ff0000000000000_u64))".to_string(),
                };
                let e2 = self.expr(end)?;
                Ok(format!("rt::build_range(&{s}, &{t}, &{e2})"))
            }
            ExprKind::Vector(items) => self.vector(items),
            ExprKind::Call { callee, args } => {
                let ExprKind::Ident(name) = &callee.kind else {
                    return Err("computed callee".into());
                };
                self.call(name, args)
            }
            other => Err(format!("construct outside the v0 subset: {other:?}")
                .chars()
                .take(120)
                .collect()),
        }
    }

    /// Expression `let`: sequential bindings, each seeing the ones before it, gone after the
    /// body — a Rust block with shadow-proof idents is exactly that scope discipline. DUPLICATE
    /// names decline: upstream is first-wins-with-a-warning (AH.2.3) and the duplicate's RHS
    /// still evaluates — reproducing that faithfully buys nothing, since no registry reference
    /// contains one.
    fn let_expr(&mut self, bindings: &[Arg], body: &Expr) -> Result<String, String> {
        let mark = self.locals.len();
        let mut block = String::from("{ ");
        let mut seen: Vec<&str> = Vec::new();
        for b in bindings {
            let Some(bn) = &b.name else {
                return Err("unnamed let binding".into());
            };
            if seen.contains(&&**bn) {
                self.locals.truncate(mark);
                return Err(format!("duplicate let binding `{bn}`"));
            }
            seen.push(bn);
            let val = self.expr(&b.value)?;
            let ident = self.fresh_ident(bn);
            let _ = write!(block, "let {ident} = {val}; ");
            self.locals.push((bn.to_string(), ident));
        }
        let body_s = self.expr(body);
        self.locals.truncate(mark);
        let _ = write!(block, "{} }}", body_s?);
        Ok(block)
    }

    /// A vector literal. All-plain elements go straight through `build_vector` (the stack
    /// machine's repr normalization — all-numeric collapses to `NumList`); any comprehension
    /// element switches to the accumulator block the interpreter's `LcFor` walk mirrors.
    fn vector(&mut self, items: &[Expr]) -> Result<String, String> {
        let plain = items.iter().all(|i| {
            !matches!(
                i.kind,
                ExprKind::LcFor { .. }
                    | ExprKind::LcForC { .. }
                    | ExprKind::LcIf { .. }
                    | ExprKind::LcEach(_)
            )
        });
        if plain {
            let emitted: Vec<String> = items
                .iter()
                .map(|i| self.expr(i))
                .collect::<Result<_, _>>()?;
            return Ok(format!("rt::build_vector(vec![{}])", emitted.join(", ")));
        }
        let acc = self.fresh_ident("acc");
        let mut block = format!("{{ let mut {acc}: Vec<rt::Value> = Vec::new(); ");
        for i in items {
            let _ = write!(block, "{}", self.element(i, &acc)?);
        }
        let _ = write!(block, "rt::build_vector({acc}) }}");
        Ok(block)
    }

    /// A call by NAME. Resolution order is the AN.10 lesson made structural: a name that is
    /// lexically BOUND here (a parameter or `let` holding a function value) resolves to the
    /// BINDING at runtime — `is_vector`'s `all_nonzero` parameter shadowing the like-named
    /// function is exactly this — and a compiled sibling call would recreate the AN.10 bug, so
    /// it DECLINES. Then builtins (names decorative, AR.3 — arguments bind positionally in arg
    /// order), then generated siblings with the full compile-time binding rules.
    fn call(&mut self, name: &str, args: &[Arg]) -> Result<String, String> {
        if self.locals.iter().any(|(n, _)| n == name) {
            return Err(format!(
                "call through the local binding `{name}` (the AN.10 shape) resolves at runtime"
            ));
        }
        if super::builtins::is_builtin(name) {
            let emitted: Vec<String> = args
                .iter()
                .map(|a| self.expr(&a.value))
                .collect::<Result<_, _>>()?;
            return Ok(format!(
                "rt::builtin(\"{name}\", &[{}])",
                emitted.join(", ")
            ));
        }
        let Some((_, params)) = self.siblings.iter().find(|(n, _)| n == name) else {
            return Err(format!(
                "call to `{name}` (not a builtin or generated sibling)"
            ));
        };
        // A generated sibling: everything is static, so the FULL binding rules run at COMPILE
        // time — a positional takes the lowest unfilled slot (AN.2), a named arg its declared
        // slot. The flat-slice ABI can only express a contiguous PREFIX of filled slots
        // (trailing unfilled fall to the callee's defaults); anything else declines.
        let params = params.clone();
        let mut slots: Vec<Option<String>> = vec![None; params.len()];
        for a in args {
            let v = self.expr(&a.value)?;
            let slot = if let Some(n) = &a.name {
                if n.starts_with('$') {
                    return Err(format!("$-arg in sibling call `{name}`"));
                }
                let Some(i) = params.iter().position(|p| p == &**n) else {
                    return Err(format!("named arg `{n}` unknown to sibling `{name}`"));
                };
                if slots[i].is_some() {
                    return Err(format!("arg `{n}` supplied more than once to `{name}`"));
                }
                i
            } else {
                let Some(i) = slots.iter().position(Option::is_none) else {
                    return Err(format!("too many args to sibling `{name}`"));
                };
                i
            };
            slots[slot] = Some(v);
        }
        let filled = slots.iter().take_while(|s| s.is_some()).count();
        if slots[filled..].iter().any(Option::is_some) {
            return Err(format!(
                "non-contiguous arg fill for sibling `{name}` — the slice ABI can't hole"
            ));
        }
        let vals: Vec<String> = slots.into_iter().flatten().collect();
        Ok(format!("{name}(&[{}])?", vals.join(", ")))
    }

    /// One VECTOR ELEMENT as statements pushing into `acc` — the compiled mirror of the stack
    /// machine's `LcFor` walk: `for` nests per binding, `if` contributes conditionally, `each`
    /// splices through the same iteration seam, an element-position `let` binds and recurses.
    fn element(&mut self, e: &Expr, acc: &str) -> Result<String, String> {
        match &e.kind {
            ExprKind::LcFor { bindings, body } => {
                let mark = self.locals.len();
                let mut open = String::new();
                let mut depth = 0;
                for b in bindings {
                    let Some(bn) = &b.name else {
                        return Err("unnamed comprehension binding".into());
                    };
                    // Each iterable is emitted INSIDE the enclosing loops, so a later binding's
                    // iterable sees the earlier binders — the interpreter's nesting order.
                    let iter = self.expr(&b.value)?;
                    let ident = self.fresh_ident(bn);
                    let _ = write!(open, "for {ident} in rt::iter_values_native(&{iter}) {{ ");
                    self.locals.push((bn.to_string(), ident));
                    depth += 1;
                }
                let inner = self.element(body, acc);
                self.locals.truncate(mark);
                let mut out = open;
                let _ = write!(out, "{}", inner?);
                out.push_str(&"} ".repeat(depth));
                Ok(out)
            }
            ExprKind::LcIf { cond, then, els } => {
                let c = self.expr(cond)?;
                let t = self.element(then, acc)?;
                match els {
                    Some(e2) => {
                        let e2s = self.element(e2, acc)?;
                        Ok(format!("if ({c}).is_truthy() {{ {t} }} else {{ {e2s} }} "))
                    }
                    None => Ok(format!("if ({c}).is_truthy() {{ {t} }} ")),
                }
            }
            ExprKind::LcEach(inner) => {
                let v = self.expr(inner)?;
                let each = self.fresh_ident("each");
                Ok(format!(
                    "for {each} in rt::iter_values_native(&{v}) {{ {acc}.push({each}); }} "
                ))
            }
            // an element-position `let` (approx's `let(aa=…, bb=…) if(…) 1`)
            ExprKind::Let { bindings, body } => {
                let mark = self.locals.len();
                let mut out = String::new();
                let mut seen: Vec<&str> = Vec::new();
                for b in bindings {
                    let Some(bn) = &b.name else {
                        return Err("unnamed let binding".into());
                    };
                    if seen.contains(&&**bn) {
                        self.locals.truncate(mark);
                        return Err(format!("duplicate let binding `{bn}`"));
                    }
                    seen.push(bn);
                    let val = self.expr(&b.value)?;
                    let ident = self.fresh_ident(bn);
                    let _ = write!(out, "let {ident} = {val}; ");
                    self.locals.push((bn.to_string(), ident));
                }
                let body_s = self.element(body, acc);
                self.locals.truncate(mark);
                let _ = write!(out, "{}", body_s?);
                // Scope the binders to this element: a sibling element must not see them.
                Ok(format!("{{ {out} }} "))
            }
            ExprKind::LcForC { .. } => Err("C-style comprehension outside the subset".into()),
            _ => Ok(format!("{acc}.push({});\n        ", self.expr(e)?)),
        }
    }
}

/// One entry's baked constants (`consts` + `consts_v`), merged into the batch's fallback-program
/// constant table. A name baking DIFFERENTLY across entries is a hard error, not last-wins — the
/// fallback is ONE island, so a conflict means two entries disagree about the same binding.
fn bake_entry<'a>(
    entry: &'a super::intrinsics::Entry,
    fallback_consts: &mut std::collections::BTreeMap<String, String>,
) -> Result<Vec<(&'a str, Baked)>, String> {
    let name = entry.name;
    let mut baked: Vec<(&str, Baked)> = entry
        .consts
        .iter()
        .map(|&(n, v)| (n, Baked::Num(v)))
        .collect();
    for &(n, build) in entry.consts_v {
        match build() {
            Value::Num(x) => baked.push((n, Baked::Num(x))),
            Value::NumList(xs) => baked.push((n, Baked::NumList(xs.to_vec()))),
            other => {
                return Err(format!(
                    "{name}: consts_v `{n}` bakes {other:?} — not emittable in v0"
                ));
            }
        }
    }
    for (n, b) in &baked {
        let scad = match b {
            Baked::Num(v) if v.is_finite() => format!("{v:?}"),
            Baked::Num(v) => return Err(format!("{name}: const `{n}` bakes non-finite {v}")),
            // Elements need the same finiteness gate as scalars: `{:?}` prints `inf`/`NaN`, which
            // LEX AS IDENTIFIERS in scad — the fallback island would silently bind undef where
            // the native baked real bits (and every NaN payload formats alike, blinding the
            // cross-batch conflict check).
            Baked::NumList(xs) => match xs.iter().find(|x| !x.is_finite()) {
                Some(bad) => {
                    return Err(format!(
                        "{name}: const `{n}` bakes non-finite element {bad}"
                    ));
                }
                None => format!(
                    "[{}]",
                    xs.iter()
                        .map(|x| format!("{x:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        };
        if let Some(prev) = fallback_consts.insert((*n).to_string(), scad.clone())
            && prev != scad
        {
            return Err(format!(
                "const `{n}` baked differently across the batch ({prev} vs {scad})"
            ));
        }
    }
    Ok(baked)
}

/// The whole generated FILE for a set of registry entries, header + minimal imports included.
/// Deterministic: same registry state → same bytes (the regen test's contract).
pub(crate) fn generate_module(entry_names: &[&str]) -> Result<String, String> {
    // The sibling table FIRST, whole batch, self included: mutual recursion (approx ↔ idx ↔
    // posmod is a real 3-cycle) forward-references freely in Rust, and named sibling arguments
    // bind against these declared param lists at compile time.
    let mut siblings: Vec<(String, Vec<String>)> = Vec::new();
    for &name in entry_names {
        let entry = super::intrinsics::REGISTRY
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("`{name}` is not a registry entry"))?;
        let a = analyze_function(entry.reference)?;
        siblings.push((a.name, a.params));
    }
    // The AR.10 fallback program: every baked constant (deduped, conflicts LOUD) followed by the
    // verbatim references — one interpretable island whose bindings equal the bakes.
    let mut fallback_consts: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut fallback_refs = String::new();

    let mut fns = String::new();
    for &name in entry_names {
        let entry = super::intrinsics::REGISTRY
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("`{name}` is not a registry entry"))?;
        let baked = bake_entry(entry, &mut fallback_consts)?;
        if entry.reference.contains("\"#") {
            return Err(format!(
                "{name}: reference contains `\"#` — raw string emission breaks"
            ));
        }
        fallback_refs.push_str(entry.reference);
        fallback_refs.push('\n');

        fns.push_str(&generate_native(entry.reference, &baked, &siblings)?);
        fns.push('\n');
    }
    let mut header = String::from(
        "// GENERATED by `eval::transpile::generate_module` (AR.6) — DO NOT EDIT.\n\
         // Refresh: FAB_REGEN=1 cargo nextest run -p fab-lang -E 'test(generated_file_is_current)'\n\
         //\n\
         // Every operation routes through the interpreter's own value algebra, so a generated\n\
         // native is bit-identical to interpreting its reference BY CONSTRUCTION — the win is the\n\
         // deleted interpretation overhead, not different math.\n\n\
         #![allow(\n\
         \x20   clippy::unreadable_literal,\n\
         \x20   clippy::cloned_ref_to_slice_refs,\n\
         \x20   clippy::used_underscore_items,\n\
         \x20   clippy::possible_missing_else,\n\
         \x20   clippy::collapsible_else_if,\n\
         \x20   reason = \"generated code: bit-exact from_bits literals, mechanical clones, \\\n\
         \x20             upstream's underscore-prefixed names, and one-line block emission \\\n\
         \x20             are the emitter's idiom\"\n\
         )]\n\n\
         // AR.13: `rt` is the ONLY thing generated code names. `extern crate self as fab_lang`\n\
         // makes this path resolve inside fab-lang too, so moving this file into its own crate\n\
         // cannot change a byte of what follows.\n\
         use fab_lang::rt;\n\n",
    );
    // The AR.10 fallback program: baked constants then verbatim references, one island whose
    // bindings equal the bakes — what a declining native interprets instead of recursing.
    header.push_str(
        "/// Every baked constant + the batch's verbatim references as ONE interpretable program —\n\
         /// the AR.10 depth fallback's target (see `native_rt`). Constants print via Rust's\n\
         /// roundtrip-exact float formatting, so the interpreted bindings equal the bakes bit-for-bit.\n\
         pub(super) const FALLBACK_SOURCES: &str = r#\"\n",
    );
    for (n, scad) in &fallback_consts {
        let _ = writeln!(header, "{n} = {scad};");
    }
    header.push_str(&fallback_refs);
    header.push_str("\"#;\n\n");
    Ok(header + &fns)
}

/// The entries the generated module currently covers, in DEPENDENCY order (a sibling call may
/// only reach entries earlier in this list). Growing this list is how an intrinsic migrates from
/// hand-written to generated (AR.7).
pub(crate) const GENERATED_ENTRIES: &[&str] = &[
    "_fab_poc_sq",
    "_fab_poc_near0",
    "_fab_poc_outer",
    "_fab_poc_isup",
    // AR.7 — the first REAL BOSL2 intrinsics through the pipeline: the two hottest functions on
    // the model profile (56% of user-fn calls between them). `is_nan` first: `is_finite`'s
    // reference calls it, and a sibling call may only reach EARLIER entries.
    "is_nan",
    "is_finite",
    // AR.8 band 2 — `let`/`assert`/indexing live. The poc entry exercises every new construct;
    // the four real entries are the profile's next tier (last 9.6% of user-fn calls, default 2.5%,
    // is_def/is_str the hot optional-arg guards). posmod/idx wait on approx (comprehensions).
    "_fab_poc_band2",
    "is_def",
    "is_str",
    "default",
    "last",
    // AR.9 band 3 — comprehensions live. approx/posmod/idx are a real 3-CYCLE (approx's list
    // branch calls idx, idx wraps offsets through posmod, posmod's assert calls approx), which is
    // why siblings resolve batch-wide rather than earlier-only.
    "_fab_poc_band3",
    "approx",
    "posmod",
    "idx",
    // AR.15 band 4 — string literals live. Worth +110 BOSL2 functions on its own (47.3% → 55.5%
    // of the library emits), which is the best ratio of unlocked functions to thinking in the
    // phase: `Value::Str` was simply never emittable, so every `style="default"` parameter and
    // every anchor-name comparison declined on a construct with no semantics to get wrong.
    "_fab_poc_band4",
];

/// Record a CALL by name: builtin or user dep. Deliberately IGNORES the lexical scope — a name
/// that is also a binding is the AN.10 shape (a parameter shadowing a function in call position),
/// and it stays in the guard set anyway: over-approximating keeps the guard honest while the
/// dispatch-level veto handles the shadowing itself.
fn record_call(name: &str, out: &mut Analysis) {
    if name.starts_with('$') {
        return;
    }
    if super::builtins::is_builtin(name) {
        out.builtins.insert(name.to_string());
    } else {
        out.deps.insert(name.to_string());
    }
}

/// Walk `args` (named-arg NAMES are parameter labels, not reads — only values walk).
fn walk_args(args: &[Arg], scope: &mut Vec<String>, out: &mut Analysis) {
    for a in args {
        walk(&a.value, scope, out);
    }
}

/// Walk `bindings` SEQUENTIALLY (each value sees the names bound before it), pushing each name.
/// Returns how many names were pushed, for the caller to truncate.
fn walk_bindings(bindings: &[Arg], scope: &mut Vec<String>, out: &mut Analysis) -> usize {
    let mut pushed = 0;
    for b in bindings {
        walk(&b.value, scope, out);
        if let Some(n) = &b.name {
            scope.push(n.to_string());
            pushed += 1;
        }
    }
    pushed
}

fn walk(e: &Expr, scope: &mut Vec<String>, out: &mut Analysis) {
    match &e.kind {
        ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
        ExprKind::Ident(name) => {
            // `$`-vars are dynamic scope, guarded by dispatch (all-positional calls only), and the
            // hand `consts` never carry them.
            if !name.starts_with('$') && !scope.iter().any(|s| s == name) {
                out.consts.insert(name.clone());
            }
        }
        ExprKind::Unary { operand, .. } => walk(operand, scope, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk(lhs, scope, out);
            walk(rhs, scope, out);
        }
        ExprKind::Ternary { cond, then, els } => {
            walk(cond, scope, out);
            walk(then, scope, out);
            walk(els, scope, out);
        }
        ExprKind::Index { base, index } => {
            walk(base, scope, out);
            walk(index, scope, out);
        }
        ExprKind::Member { base, .. } => walk(base, scope, out),
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind {
                record_call(name, out);
            } else {
                // A computed callee (method value, applied literal) resolves at runtime — nothing
                // nameable to guard; its SUBEXPRESSIONS still walk.
                walk(callee, scope, out);
            }
            walk_args(args, scope, out);
        }
        ExprKind::Vector(items) => {
            for i in items {
                walk(i, scope, out);
            }
        }
        ExprKind::Range { start, step, end } => {
            walk(start, scope, out);
            if let Some(s) = step {
                walk(s, scope, out);
            }
            walk(end, scope, out);
        }
        ExprKind::FunctionLiteral { params, body } => {
            let mark = scope.len();
            scope.extend(params.iter().map(|p: &Parameter| p.name.to_string()));
            for p in params {
                if let Some(d) = &p.default {
                    walk(d, scope, out);
                }
            }
            walk(body, scope, out);
            scope.truncate(mark);
        }
        // `let` and comprehension-`for` share binder semantics exactly: sequential bindings, body
        // in the extended scope, binders gone after.
        ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
            let mark = scope.len();
            let _ = walk_bindings(bindings, scope, out);
            walk(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
            walk_args(args, scope, out);
            if let Some(b) = body {
                walk(b, scope, out);
            }
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            // The update clause BINDS, it does not merely rebind. `_dp_distance_array`
            // (skin.scad) introduces `newrow` in the update and then reads it from two later
            // update bindings — so walking update as plain args reported a loop variable as a
            // free global read. Update is walked BEFORE cond/body because everything the loop
            // binds, from either clause, is in scope for both; and `walk_bindings` pushes each
            // name only after walking its own value, which is the sequential binding L.2.8e
            // pinned for this exact form.
            let mark = scope.len();
            let _ = walk_bindings(init, scope, out);
            let _ = walk_bindings(update, scope, out);
            walk(cond, scope, out);
            walk(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::LcEach(inner) => walk(inner, scope, out),
        ExprKind::LcIf { cond, then, els } => {
            walk(cond, scope, out);
            walk(then, scope, out);
            if let Some(e2) = els {
                walk(e2, scope, out);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test harness: expect/panic ARE the assertions"
)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::intrinsics::{PINS, REGISTRY};
    use super::{analyze_closed, analyze_function};

    /// AR.11 — how much of the pinned BOSL2 the emitter actually owns, as a RATCHET. Runs the
    /// codegen over every top-level function in the library and asserts the emit count never
    /// falls; the decline histogram it prints alongside is the phase roadmap, because each
    /// decline names the construct the emitter does not yet handle and the counts rank them.
    ///
    /// A FLOOR rather than a report for the AR.3.3 reason: an emitter that quietly stops handling
    /// a construct does not fail, it falls back to interpretation, and every other test still
    /// passes. Only a number that must not drop can see that.
    ///
    /// Read the roadmap with:
    /// `cargo nextest run -p fab-lang -E 'test(bosl2_codegen_coverage)' --no-capture`
    #[test]
    fn bosl2_codegen_coverage_holds_its_floor() {
        use std::collections::BTreeMap;

        /// Functions the emitter compiles TODAY (2026-07-28), out of 1335 — 632 at AR.11, 742 once
        /// AR.15 made `Value::Str` emittable. Raise this as bands
        /// land — AR.16 constants, AR.17 first-class functions. Lowering it is a deliberate act
        /// that needs a reason next to it, which is the whole point of the ratchet.
        const FLOOR: usize = 742;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !root.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let mut files: Vec<_> = std::fs::read_dir(&root)
            .expect("BOSL2 checked out")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .collect();
        files.sort();

        // Pass 1: every function in the library, so a sibling call resolves library-wide.
        let mut sources: Vec<(String, String)> = Vec::new(); // (file, text)
        for f in &files {
            let text = std::fs::read_to_string(f).expect("readable");
            sources.push((f.file_name().expect("named").to_string_lossy().into(), text));
        }
        let mut siblings: Vec<(String, Vec<String>)> = Vec::new();
        let mut refs: Vec<(String, String, String)> = Vec::new(); // (file, name, source)
        let mut unparsed_files = 0_usize;
        for (file, text) in &sources {
            let Ok(prog) = crate::parser::parse(text) else {
                unparsed_files += 1;
                continue;
            };
            for stmt in &prog.stmts {
                if let crate::parser::StmtKind::FunctionDef {
                    name,
                    params,
                    body: _,
                } = &stmt.kind
                {
                    siblings.push((
                        name.clone(),
                        params.iter().map(|p| p.name.to_string()).collect(),
                    ));
                    refs.push((
                        file.clone(),
                        name.clone(),
                        text[stmt.span.clone()].to_string(),
                    ));
                }
            }
        }

        // Pass 2: try to emit each one.
        let mut ok = 0_usize;
        let mut by_reason: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for (file, name, src) in &refs {
            match super::generate_native(src, &[], &siblings) {
                Ok(_) => ok += 1,
                Err(e) => {
                    let reason = classify(&e);
                    let slot = by_reason.entry(reason).or_insert((0, String::new()));
                    slot.0 += 1;
                    if slot.1.is_empty() {
                        slot.1 = format!("{file}:{name}");
                    }
                }
            }
        }

        let total = refs.len();
        let mut hist: Vec<_> = by_reason.into_iter().collect();
        hist.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
        println!("\n=== BOSL2 codegen coverage ===");
        println!(
            "files {} ({unparsed_files} unparsed), functions {total}",
            files.len()
        );
        // Integer tenths-of-a-percent: no usize→f64 cast to justify for one printed number.
        let tenths = ok * 1000 / total.max(1);
        println!(
            "EMIT {ok}/{total} ({}.{}%), floor {FLOOR}",
            tenths / 10,
            tenths % 10
        );
        // FIRST-DECLINE-WINS, so every count is a LOWER bound on that construct's real prevalence:
        // a function that trips on a free read may hold three strings behind it. Ranking is still
        // sound — clearing a band re-runs the survivors against whatever they hit next.
        for (reason, (n, ex)) in &hist {
            println!("  {n:5}  {reason}   e.g. {ex}");
        }
        assert!(
            ok >= FLOOR,
            "codegen coverage REGRESSED: {ok} functions emit, floor is {FLOOR}. \
             The emitter lost a construct — see the histogram above. It does not fail loudly on \
             its own, it just falls back to interpreting, which is why this number is a gate."
        );
        assert!(
            unparsed_files == 0,
            "{unparsed_files} BOSL2 files no longer parse — that is a PARSER regression, not a \
             codegen one, and it silently shrinks every count in this report"
        );
    }

    /// Collapse a decline message to its CONSTRUCT — the emitter names what it hit, but embeds
    /// identifiers; the histogram wants the shape, not the instance.
    ///
    /// Order is load-bearing and the probes are NOT disjoint: a decline names the construct it hit
    /// by `{:?}`-printing the node, so an `echo("…")` message contains `Str(` too. Every enclosing
    /// CONSTRUCT is probed before any literal payload it might carry — otherwise the histogram
    /// credits the wrong band and the roadmap points at work already done.
    fn classify(e: &str) -> String {
        for (probe, bucket) in [
            (
                "computed callee",
                "computed callee (fn value in call position)",
            ),
            ("the AN.10 shape", "call through a local binding (AN.10)"),
            ("free read", "free read (an unbaked library constant)"),
            (
                "non-contiguous arg fill",
                "non-contiguous named args to a sibling",
            ),
            ("too many args", "too many args to a sibling"),
            ("C-style comprehension", "C-style comprehension"),
            ("Echo {", "echo"),
            (
                "duplicate parameter",
                "duplicate parameter (AN.6, correct decline)",
            ),
            ("does not parse", "reference does not parse"),
            ("Assert {", "assert form outside the subset"),
            ("Let {", "let form outside the subset"),
            ("Range {", "range form outside the subset"),
            ("Lookup", "lookup"),
            // Literal payloads LAST — see the ordering note above.
            ("FunctionLiteral", "function literal"),
            ("Str(", "string literal"),
        ] {
            if e.contains(probe) {
                return bucket.to_string();
            }
        }
        let tail = e.rsplit_once(": ").map_or(e, |(_, t)| t);
        format!("other — {}", tail.chars().take(50).collect::<String>())
    }

    /// Names the syntactic walk reaches that the HAND lists prune, each with the reason. A pruned
    /// name is unreachable FOR THAT ENTRY's accepted arg shapes, so its whole SUBTREE is pruned —
    /// the comparison's resolver refuses to walk it, exactly as the author's transitive exclusion
    /// does (select drops `all_nonzero` AND therefore `all_nonzero`'s own `abs`). The direction
    /// matters: an over-approximation left in place would only make a guard CHECK MORE, never
    /// answer wrong; a name missing from DERIVED is a red test and an analyzer bug.
    fn pruned_by_author(entry: &str) -> &'static [&'static str] {
        match entry {
            // ── the `is_vector` family ───────────────────────────────────────────────────────────
            // `is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON)` has two dead tails for
            // every entry here, because none of them passes anything past the SECOND parameter:
            //   * `zero` stays undef, so `is_undef(zero) ||` short-circuits before `norm(v)`;
            //   * `all_nonzero` keeps its `false` default, so `!all_nonzero ||` short-circuits before
            //     `all_nonzero(v)` — and with it that function's own `abs`.
            // A pruned name prunes its whole SUBTREE, which is why `abs`/`norm` come along. AR.5a
            // adjudicated each of these against the entry's accepted arg shapes; the per-entry arms
            // below are DELIBERATELY not collapsed into one, because the sets differ and a shared arm
            // would silently prune a name for an entry nobody checked.
            "select" | "_none_inside" | "_get_ear" | "vector_axis" => &["all_nonzero"],
            "unit" | "_bt_search" | "_point_dist" => &["all_nonzero", "abs"],
            "_vnf_centroid" | "v_abs" => &["all_nonzero", "norm"],
            "is_matrix" | "sum" | "_apply" | "is_path" | "v_theta" => {
                &["all_nonzero", "abs", "norm"]
            }
            // `apply` adds the sum family: its `is_matrix`/`is_vector` shape tests never reach the
            // point-list branch that would call `sum`/`_sum`.
            "apply" => &["all_nonzero", "abs", "norm", "sum", "_sum"],
            // `vector_angle`/`affine3d_rot_from_to` reach `flatten`/`list_to_matrix` only through a
            // `list_to_matrix` branch their fixed call shapes never take.
            "vector_angle" => &["all_nonzero", "abs", "flatten", "list_to_matrix"],
            "affine3d_rot_from_to" => &["flatten", "list_to_matrix"],
            // posmod's `approx(m, 0)` sits behind `is_finite(m) &&` — the short-circuit proves
            // approx only ever sees SCALARS from posmod. `idx` lives in approx's list branch, and
            // `is_list`/`len` are that branch's own guard condition, equally dead for numbers
            // (evaluation reaches `is_num(a) && is_num(b)?` first and takes it).
            "posmod" => &["idx", "is_list", "len"],
            // affine3d_rot_by_axis takes the SCALAR approx lane only (`assert(is_finite(ang))` pins
            // it), so approx's list branch — `idx`, and `posmod`/`is_string` under it — is dead.
            // NOTE `abs` is NOT here: that one is reachable, and AR.5a moved it into the hand list.
            "affine3d_rot_by_axis" => &["all_nonzero", "idx", "posmod", "is_string"],
            // `rot` is a DISPATCHER: its body picks one affine lane per call shape, and the
            // point-list lane (centroid/mean/pointlist_bounds/transpose/in_list/force_list/sum/_sum/
            // flatten/list_to_matrix/_all_func/is_path, plus the `search` builtin) belongs to the
            // `p=` argument the native declines.
            "rot" => &[
                "search",
                "_all_func",
                "_sum",
                "centroid",
                "flatten",
                "force_list",
                "in_list",
                "is_path",
                "list_to_matrix",
                "mean",
                "pointlist_bounds",
                "sum",
                "transpose",
            ],
            // `is_def` is reached only through a defaulted parameter this entry always supplies.
            "_region_region_intersections" => &["is_def"],
            _ => &[],
        }
    }

    /// AR.5a: this table is EMPTY, and that is the point. It parked derived-minus-hand deltas that
    /// nobody had reasoned about yet; every one has now been adjudicated into either a documented
    /// pruning above or a hand-list FIX (three were real missing guards — `affine3d_rot_by_axis`
    /// wanted `abs`, `_region_region_intersections` wanted `is_bool` + `is_string`).
    ///
    /// It stays as a named seam rather than being deleted, because the next widening of the codegen
    /// subset will surface new deltas and they should land HERE — visible and tracked — rather than
    /// in `pruned_by_author`, which is for names somebody proved unreachable. The two directions are
    /// not symmetric: a wrong pruning deletes a correctness guard and lets a native wire where the
    /// interpreter would diverge; a wrong hand-list entry only makes the guard check more.
    fn unadjudicated(_entry: &str, _kind: &str) -> &'static [&'static str] {
        &[]
    }

    /// The acceptance oracle for the whole pass: every hand-maintained guard list in the registry,
    /// closed over the same dep graph, must be CONTAINED in what the analyzer derives — and the
    /// over-approximation must be exactly the documented pruning, nothing silent in either
    /// direction. ~55 AN-audited entries is the strongest ground truth this analysis can get.
    #[test]
    fn derived_guards_contain_every_hand_list() {
        let mut deltas: Vec<String> = Vec::new();
        for entry in REGISTRY {
            let allow = pruned_by_author(entry.name);
            let resolve = |name: &str| -> Option<&'static str> {
                if allow.contains(&name) {
                    return None; // author-pruned: this entry never reaches it, subtree and all
                }
                REGISTRY
                    .iter()
                    .find(|e| e.name == name)
                    .map(|e| e.reference)
                    .or_else(|| PINS.iter().find(|(n, _)| *n == name).map(|(_, src)| *src))
            };
            let derived = analyze_closed(entry.reference, &resolve)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.name));

            let hand_consts: BTreeSet<&str> = entry
                .consts
                .iter()
                .map(|(n, _)| *n)
                .chain(entry.consts_v.iter().map(|(n, _)| *n))
                .collect();
            let hand_deps: BTreeSet<&str> = entry.deps.iter().copied().collect();
            let hand_builtins: BTreeSet<&str> = entry.builtins.iter().copied().collect();

            for (kind, hand, derived_set) in [
                ("consts", &hand_consts, &derived.consts),
                ("deps", &hand_deps, &derived.deps),
                ("builtins", &hand_builtins, &derived.builtins),
            ] {
                let missing: Vec<&&str> =
                    hand.iter().filter(|n| !derived_set.contains(**n)).collect();
                if !missing.is_empty() {
                    deltas.push(format!(
                        "{}: hand {kind} {missing:?} NOT DERIVED — the analyzer failed to reach \
                         a name the author proved matters (AN territory)",
                        entry.name
                    ));
                }
                let tracked = unadjudicated(entry.name, kind);
                let extra: Vec<&String> = derived_set
                    .iter()
                    .filter(|n| {
                        !hand.contains(n.as_str())
                            && !allow.contains(&n.as_str())
                            && !tracked.contains(&n.as_str())
                    })
                    .collect();
                if !extra.is_empty() {
                    deltas.push(format!(
                        "{}: derived {kind} {extra:?} beyond the hand list and undocumented — \
                         either the hand list is INCOMPLETE (fix the entry) or the walk needs a \
                         rule (document it in `pruned_by_author`)",
                        entry.name
                    ));
                }
            }
        }
        assert!(
            deltas.is_empty(),
            "{} guard-list deltas:\n{}",
            deltas.len(),
            deltas.join("\n")
        );
    }

    /// The generated file is BYTE-IDENTICAL to what the generator produces from today's registry —
    /// the checked-in-output contract that answers the design doc's build-cost kill risk (no
    /// build.rs; contributors pay nothing; drift is a red test, refreshed explicitly).
    #[test]
    fn generated_file_is_current() {
        let want = super::generate_module(super::GENERATED_ENTRIES).expect("generates");
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/eval/intrinsics/generated.rs"
        );
        if std::env::var_os("FAB_REGEN").is_some() {
            std::fs::write(path, &want).expect("write generated.rs");
            return;
        }
        let have = include_str!("intrinsics/generated.rs");
        assert_eq!(
            have, want,
            "generated.rs is stale — refresh with FAB_REGEN=1 (see the file header)"
        );
    }

    /// The walk's scope rules, on synthetic references where the answer is unambiguous.
    #[test]
    fn scope_rules_classify_reads_and_calls() {
        let a = analyze_function(
            "function t(a, b=_EPS) = let(c = a + PI) is_num(c) ? helper(b) : $fn + d;",
        )
        .expect("analyzes");
        assert_eq!(a.name, "t");
        assert_eq!(a.params, ["a", "b"]);
        // _EPS (a default), PI (a body read), d — free reads; a/b/c bound; $fn dynamic.
        assert_eq!(
            a.consts,
            ["PI", "_EPS", "d"].map(String::from).into_iter().collect()
        );
        assert_eq!(a.deps, ["helper"].map(String::from).into_iter().collect());
        assert_eq!(
            a.builtins,
            ["is_num"].map(String::from).into_iter().collect()
        );
    }

    /// AN.10's shape: a parameter shadowing a function name in CALL position still registers as a
    /// dep — over-approximation is the guard-safe direction.
    #[test]
    fn a_shadowed_call_name_is_still_a_dep() {
        let a = analyze_function("function t(f) = f(1);").expect("analyzes");
        assert_eq!(a.deps, ["f"].map(String::from).into_iter().collect());
    }

    /// AR.13 — generated code names `rt` and NOTHING else from fab-lang. This is the property that
    /// makes AR.14's crate move a move rather than a rewrite: a generated file that reaches
    /// `crate::eval::…` compiles today only because it happens to live inside `eval`, and would
    /// stop compiling the moment it did not.
    ///
    /// Checked against the emitted TEXT rather than by trusting the emitter, because the emitter
    /// has ~25 separate format strings that each decide a path independently. One of them
    /// reverting is a one-character diff that the compiler is perfectly happy with right up until
    /// the file moves.
    #[test]
    fn generated_code_names_only_the_rt_abi() {
        let text = super::generate_module(super::GENERATED_ENTRIES).expect("the batch generates");
        // Everything after the fallback-sources blob: the `r#"…"#` island is verbatim SCAD, and
        // scad identifiers are not Rust paths — `crate::` cannot appear there, but a name like
        // `super::` could in principle, so the scan starts past it.
        let code = text
            .rsplit_once("\"#;")
            .map_or(text.as_str(), |(_, tail)| tail);
        for forbidden in ["crate::eval", "crate::parser", "super::", "crate::Result"] {
            assert!(
                !code.contains(forbidden),
                "generated code reaches `{forbidden}` — that resolves only while this file lives \
                 inside fab-lang. Route it through `fab_lang::rt` (adding to `rt` if it is not \
                 there yet, which is a deliberate act — see that module's note)."
            );
        }
        assert!(
            code.contains("rt::apply_binary") && code.contains("rt::builtin"),
            "the scan found no rt calls at all, so it would pass on an empty file"
        );
    }

    /// A C-style comprehension's UPDATE clause introduces bindings, it does not only reassign
    /// them — `_dp_distance_array` in BOSL2's skin.scad binds `newrow` there and reads it from two
    /// later update bindings. Walking update as plain args reported that loop variable as a free
    /// global read, which put it on the guard set and would have declined the function for a
    /// constant that does not exist. Found by the AR.12 library census, not by any hand entry:
    /// none of the ~55 transcribed references uses this form.
    #[test]
    fn a_c_style_update_binding_is_not_a_free_read() {
        let a = analyze_function(
            "function t(n) = [for (i = 0; i < n; nxt = i * 2, i = nxt + 1) if (i > 2) i];",
        )
        .expect("analyzes");
        assert!(
            a.consts.is_empty(),
            "`nxt` is bound by the update clause, not read from the island: {:?}",
            a.consts
        );
    }

    /// Comprehension + function-literal binders leave scope when their expression ends.
    #[test]
    fn binders_do_not_leak() {
        let a = analyze_function(
            "function t(n) = [for (i = [0:n]) (function(j) j + i)(i)] * [for (i = [0:n]) i] + i;",
        )
        .expect("analyzes");
        // the trailing `+ i` is OUTSIDE both comprehensions: a free read.
        assert_eq!(a.consts, ["i"].map(String::from).into_iter().collect());
    }

    /// A default reading a PRIOR PARAM has no native shape: `push_call` evaluates an unfilled
    /// slot's default in the lexical BASE (island globals), so compiling `b=a` to the argument's
    /// value would read a different program than the interpreter — the AN family's exact failure.
    #[test]
    fn a_param_referencing_default_declines() {
        let err =
            super::generate_native("function t(a, b=a) = b;", &[], &[]).expect_err("declines");
        assert!(err.contains("free read `a`"), "{err}");
    }

    /// Duplicate parameter names bind two-phase in the machine (AN.6, arg-over-default); Rust
    /// `let` shadowing would take the LAST slot unconditionally. No native shape — decline.
    #[test]
    fn a_duplicate_parameter_declines() {
        let err =
            super::generate_native("function t(a, a=9) = a;", &[], &[]).expect_err("declines");
        assert!(err.contains("duplicate parameter `a`"), "{err}");
    }

    /// The `NumList` const arm refuses non-finite elements as loudly as the scalar arm: `{:?}`
    /// prints `inf`/`NaN`, which lex as IDENTIFIERS in scad — the fallback island would silently
    /// bind undef where the native bakes real bits.
    #[test]
    fn a_non_finite_numlist_const_declines() {
        #[allow(
            clippy::unnecessary_wraps,
            reason = "Entry.func's required Intrinsic signature — the wrap is the contract"
        )]
        fn noop(_: &[crate::Value]) -> crate::Result<crate::Value> {
            Ok(crate::Value::Undef)
        }
        fn inf_list() -> crate::Value {
            crate::Value::num_list(vec![1.0, f64::INFINITY])
        }
        let entry = super::super::intrinsics::Entry {
            name: "t",
            reference: "function t() = C;",
            consts: &[],
            consts_v: &[("C", inf_list)],
            deps: &[],
            builtins: &[],
            func: noop,
        };
        let mut consts = std::collections::BTreeMap::new();
        let err = super::bake_entry(&entry, &mut consts).expect_err("declines");
        assert!(err.contains("non-finite element"), "{err}");
    }
}
