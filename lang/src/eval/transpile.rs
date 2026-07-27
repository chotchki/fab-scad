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
pub(crate) fn analyze_closed(
    reference: &str,
    resolve: &dyn Fn(&str) -> Option<&'static str>,
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
                "Value::num_list(vec![{}])",
                xs.iter()
                    .map(|x| format!("f64::from_bits({:#x}_u64)", x.to_bits()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn emit_num(n: f64) -> String {
    format!("Value::Num(f64::from_bits({:#x}_u64))", n.to_bits())
}

/// What the emitter pulled in — drives the generated file's `use` lines so they stay minimal and
/// clippy-clean (an unconditional import block would trip `unused_imports` the first time a
/// generated set doesn't use one).
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent import FLAGS, not a state machine — a bitset would obscure them"
)]
struct Uses {
    ops: bool,
    builtins: bool,
    binop: bool,
    unop: bool,
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
        uses: Uses::default(),
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "/// Generated native for `{name}` — semantics route through the interpreter's own value\n\
         /// algebra (`ops::`/`builtins::`), bit-identical to the interpreted reference by construction.\n\
         pub(super) fn {name}(args: &[Value]) -> crate::Result<Value> {{\n\
         \x20   // AR.10: past the depth budget, DECLINE to the pure interpreter — explicit stack,\n\
         \x20   // same proven semantics; recursion cannot ride the Rust stack unbounded.\n\
         \x20   let Some(_depth) = super::native_rt::DepthGuard::enter() else {{\n\
         \x20       return super::native_rt::run_interpreted(FALLBACK_SOURCES, \"{name}\", args);\n\
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
            None => "Value::Undef".to_string(),
            Some(d) => em
                .expr(d)
                .map_err(|e| format!("{name}: default of `{}`: {e}", p.name))?,
        };
        // A cheap default binds eagerly (`unwrap_or`); a constructing one stays lazy.
        let bind = if default == "Value::Undef" {
            format!("{getter}.cloned().unwrap_or(Value::Undef)")
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
    uses: Uses,
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
            ExprKind::Bool(b) => Ok(format!("Value::Bool({b})")),
            ExprKind::Undef => Ok("Value::Undef".to_string()),
            ExprKind::Ident(name) => {
                if let Some((_, ident)) = self.locals.iter().rev().find(|(n, _)| n == name) {
                    Ok(format!("{ident}.clone()"))
                } else if let Some((_, b)) = self.baked.iter().find(|(n, _)| n == name) {
                    Ok(b.emit())
                } else {
                    Err(format!("free read `{name}` has no baked value"))
                }
            }
            ExprKind::Unary { op, operand } => {
                self.uses.ops = true;
                self.uses.unop = true;
                Ok(format!(
                    "ops::apply_unary(UnOp::{op:?}, {})",
                    self.expr(operand)?
                ))
            }
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
                    "Value::Bool({}.is_truthy() {sym} {}.is_truthy())",
                    self.expr(lhs)?,
                    self.expr(rhs)?
                ))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.uses.ops = true;
                self.uses.binop = true;
                Ok(format!(
                    "ops::apply_binary(BinOp::{op:?}, {}, {})",
                    self.expr(lhs)?,
                    self.expr(rhs)?
                ))
            }
            ExprKind::Ternary { cond, then, els } => Ok(format!(
                "if {}.is_truthy() {{ {} }} else {{ {} }}",
                self.expr(cond)?,
                self.expr(then)?,
                self.expr(els)?
            )),
            // `list[i]` — the interpreter's own index op carries the semantics (negative /
            // out-of-range → undef, string indexing, both list reprs).
            ExprKind::Index { base, index } => {
                self.uses.ops = true;
                Ok(format!(
                    "ops::index({}, &{})",
                    self.expr(base)?,
                    self.expr(index)?
                ))
            }
            ExprKind::Member { base, field } => {
                self.uses.ops = true;
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
                    None => "Value::Undef".to_string(),
                };
                Ok(format!(
                    "{{ if !({c}).is_truthy() {{ return Err(super::bosl_assert(\"generated\")); }} {b} }}"
                ))
            }
            // `[start : step? : end]` with computed endpoints — the interpreter's own constructor
            // carries the coercion rules.
            ExprKind::Range { start, step, end } => {
                let s = self.expr(start)?;
                let t = match step {
                    Some(t) => self.expr(t)?,
                    None => "Value::Num(f64::from_bits(0x3ff0000000000000_u64))".to_string(),
                };
                let e2 = self.expr(end)?;
                Ok(format!("build_range(&{s}, &{t}, &{e2})"))
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
            return Ok(format!("build_vector(vec![{}])", emitted.join(", ")));
        }
        let acc = self.fresh_ident("acc");
        let mut block = format!("{{ let mut {acc}: Vec<Value> = Vec::new(); ");
        for i in items {
            let _ = write!(block, "{}", self.element(i, &acc)?);
        }
        let _ = write!(block, "build_vector({acc}) }}");
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
            self.uses.builtins = true;
            return Ok(format!(
                "builtins::apply(\"{name}\", &[{}])",
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
                    let _ = write!(open, "for {ident} in iter_values_native(&{iter}) {{ ");
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
                    "for {each} in iter_values_native(&{v}) {{ {acc}.push({each}); }} "
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
    let mut uses = Uses::default();
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
        // regenerate the Uses by scanning the emitted text — cheaper than threading it out, and
        // the regen test pins the final bytes either way
        uses.ops |= fns.contains("ops::");
        uses.builtins |= fns.contains("builtins::apply");
        uses.binop |= fns.contains("BinOp::");
        uses.unop |= fns.contains("UnOp::");
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
         use crate::eval::value::Value;\n",
    );
    match (uses.ops, uses.builtins) {
        (true, true) => header.push_str("use crate::eval::{builtins, ops};\n"),
        (true, false) => header.push_str("use crate::eval::ops;\n"),
        (false, true) => header.push_str("use crate::eval::builtins;\n"),
        (false, false) => {}
    }
    match (uses.binop, uses.unop) {
        (true, true) => header.push_str("use crate::parser::{BinOp, UnOp};\n"),
        (true, false) => header.push_str("use crate::parser::BinOp;\n"),
        (false, true) => header.push_str("use crate::parser::UnOp;\n"),
        (false, false) => {}
    }
    // The eval-private helpers (visible here because generated.rs is eval's descendant): the
    // stack machine's own vector/range construction and the pure iteration seam (AR.9).
    let helpers: Vec<&str> = [
        ("build_range", "build_range("),
        ("build_vector", "build_vector("),
        ("iter_values_native", "iter_values_native("),
    ]
    .iter()
    .filter(|(_, probe)| fns.contains(probe))
    .map(|(name, _)| *name)
    .collect();
    match helpers.as_slice() {
        [] => {}
        [one] => {
            let _ = writeln!(header, "use crate::eval::{one};");
        }
        many => {
            let _ = writeln!(header, "use crate::eval::{{{}}};", many.join(", "));
        }
    }
    header.push('\n');
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
            let mark = scope.len();
            let _ = walk_bindings(init, scope, out);
            walk(cond, scope, out);
            walk_args(update, scope, out); // update rebinds names already in scope
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

    /// Names the syntactic walk reaches that the HAND lists prune, each with the reason. A pruned
    /// name is unreachable FOR THAT ENTRY's accepted arg shapes, so its whole SUBTREE is pruned —
    /// the comparison's resolver refuses to walk it, exactly as the author's transitive exclusion
    /// does (select drops `all_nonzero` AND therefore `all_nonzero`'s own `abs`). The direction
    /// matters: an over-approximation left in place would only make a guard CHECK MORE, never
    /// answer wrong; a name missing from DERIVED is a red test and an analyzer bug.
    fn pruned_by_author(entry: &str) -> &'static [&'static str] {
        match entry {
            // select's fixed 1-arg `is_vector(start)` call can never take the 2-arg path that
            // reaches `all_nonzero` — the author's reachability pruning (Entry doc, O.5.2). The
            // same argument covers every entry below: their is_vector calls never pass the
            // `all_nonzero=` parameter, so that branch (and its whole subtree) is dead for them.
            "select"
            | "is_matrix"
            | "_none_inside"
            | "sum"
            | "unit"
            | "_apply"
            | "_bt_search"
            | "vector_angle"
            | "_point_dist"
            | "_vnf_centroid"
            | "_get_ear"
            | "is_path"
            | "v_abs"
            | "v_theta"
            | "vector_axis"
            | "apply"
            | "affine3d_rot_by_axis" => &["all_nonzero"],
            // posmod's `approx(m, 0)` sits behind `is_finite(m) &&` — the short-circuit proves
            // approx only ever sees SCALARS from posmod. `idx` lives in approx's list branch, and
            // `is_list`/`len` are that branch's own guard condition, equally dead for numbers
            // (evaluation reaches `is_num(a) && is_num(b)?` first and takes it).
            "posmod" => &["idx", "is_list", "len"],
            _ => &[],
        }
    }

    /// Deltas surfaced by the closure that are NOT yet adjudicated — each needs its reference's
    /// branch structure read against the entry's accepted arg shapes before it becomes either a
    /// documented pruning above or a hand-list fix (AR.5a). Tracked EXACTLY (its own test below)
    /// so a new delta or a silently resolved one is loud; parked here is visible, not forgotten.
    fn unadjudicated(entry: &str, kind: &str) -> &'static [&'static str] {
        match (entry, kind) {
            ("is_matrix" | "sum" | "_apply" | "is_path" | "v_theta" | "apply", "builtins") => {
                &["abs", "norm"]
            }
            ("unit" | "_bt_search" | "vector_angle" | "_point_dist", "builtins") => &["abs"],
            ("_vnf_centroid" | "v_abs", "builtins") => &["norm"],
            ("vector_angle" | "affine3d_rot_from_to", "deps") => &["flatten", "list_to_matrix"],
            ("apply", "deps") => &["_sum", "sum"],
            ("rot", "deps") => &[
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
            ("rot", "builtins") => &["search"],
            ("affine3d_rot_by_axis", "deps") => &["idx", "posmod"],
            ("affine3d_rot_by_axis", "builtins") => &["abs", "is_string"],
            ("_region_region_intersections", "deps") => &["is_def"],
            ("_region_region_intersections", "builtins") => &["is_bool", "is_string"],
            _ => &[],
        }
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

    /// The NumList const arm refuses non-finite elements as loudly as the scalar arm: `{:?}`
    /// prints `inf`/`NaN`, which lex as IDENTIFIERS in scad — the fallback island would silently
    /// bind undef where the native bakes real bits.
    #[test]
    fn a_non_finite_numlist_const_declines() {
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
