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

use fab_lang::{Arg, Expr, ExprKind, Parameter, Stmt, StmtKind, parse};

/// What one function's reference source reaches, by name — the raw material for an `Entry`'s guard
/// sets (names only; the guard VALUES are resolved against the island at arm time, not here).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Analysis {
    /// The defined function's name.
    pub name: String,
    /// Parameter names, in declaration order.
    pub params: Vec<String>,
    /// Free VALUE-position identifier reads (non-`$`) — the names `consts`/`consts_v` must guard.
    pub consts: BTreeSet<String>,
    /// CALL-position names that are not builtins — user-function dependencies (`deps`).
    pub deps: BTreeSet<String>,
    /// CALL-position builtin names (`builtins`) — shadowable, hence guarded.
    pub builtins: BTreeSet<String>,
}

/// Analyze a single `function name(params) = body;` reference.
///
/// # Errors
/// The reference must parse and contain exactly one function definition (the `Entry::reference`
/// contract) — anything else is a malformed reference, not a valid analysis subject.
pub fn analyze_function(reference: &str) -> Result<Analysis, String> {
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

/// [`analyze_function`] for a MODULE reference — the same walk over a body that is a STATEMENT.
///
/// AR.20. Split rather than folded into `analyze_function` because the shapes differ where it
/// matters: a function's body is one expression, a module's is a statement tree whose leaves are
/// instantiations. Everything after that — parameters in scope for their own defaults, self-calls
/// not counting as deps — is identical and is written once here by delegating to the same `walk`.
///
/// # Errors
/// If the reference does not parse, or does not hold exactly one module definition.
pub fn analyze_module(reference: &str) -> Result<Analysis, String> {
    let prog = parse(reference).map_err(|e| format!("reference does not parse: {e:?}"))?;
    let mut defs = prog.stmts.iter().filter_map(|s| match &s.kind {
        StmtKind::ModuleDef { name, params, body } => Some((name, params, body)),
        _ => None,
    });
    let Some((name, params, body)) = defs.next() else {
        return Err("reference holds no module definition".into());
    };
    if defs.next().is_some() {
        return Err("reference holds more than one module definition".into());
    }

    let mut out = Analysis {
        name: name.clone(),
        params: params.iter().map(|p| p.name.to_string()).collect(),
        ..Analysis::default()
    };
    let mut scope: Vec<String> = out.params.clone();
    for p in params {
        if let Some(d) = &p.default {
            walk(d, &mut scope, &mut out);
        }
    }
    walk_stmt(body, &mut scope, &mut out);
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
/// A registry-backed resolver hands back `&'static str`; a [`crate::library::Library`]-backed one
/// hands back a slice of the file it read, and both satisfy this.
pub fn analyze_closed<'r>(
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

/// What the emitter knows about a sibling it may call directly: its name, its parameters in
/// declaration order, and each parameter's DEFAULT already emitted as a Rust expression.
///
/// AR.18 — the defaults are what let a call with a HOLE compile. `f(x, c=3)` against `f(a,b,c)`
/// fills slots 0 and 2 and leaves 1 empty, and the positional `&[Value]` ABI cannot say "slot 1 was
/// not supplied". Passing `Value::Undef` would be WRONG: `args.get(1)` then returns `Some`, the
/// callee's `unwrap_or(default)` never fires, and an explicit undef silently overrides a real
/// default — AN.3's bug, in compiled form. So the hole is filled with the callee's OWN default.
///
/// Sound because a default evaluates in the callee's LEXICAL BASE, never the caller's scope, and an
/// emitted default references only literals and baked constants — anything else and the callee
/// itself would have declined. So the expression means the same thing wherever it is written.
#[derive(Debug, Clone)]
pub struct Sibling {
    pub name: String,
    pub params: Vec<String>,
    /// Per parameter: the emitted default, or `None` where the emitter could not produce one. A
    /// hole landing on a `None` declines the CALL rather than guessing.
    pub defaults: Vec<Option<String>>,
}

/// AR.20.4 — generate the Rust native for one `module name(params) body`.
///
/// The statement half of the emitter. A module returns GEOMETRY rather than a value, so the body
/// emits statements that push into a parts list which the ctx then groups — exactly the implicit
/// union a `{ … }` block means in OpenSCAD.
///
/// Everything a module can do that a function cannot goes through [`fab_lang::surface::ModuleCtx`]:
/// children render by re-entering interpretation (they are the USER's source and can never be
/// compiled), `$`-vars read through the inherited chain rather than a snapshot, and a module call
/// DISPATCHES. See that trait for why each is shaped the way it is.
///
/// # Errors
/// A construct outside the subset declines LOUDLY with the construct named — a partial native would
/// be a wrong native, and for a module that means silently missing geometry.
pub fn generate_module_native(
    reference: &str,
    baked: &[(&str, Baked)],
    siblings: &[Sibling],
) -> Result<String, String> {
    let prog =
        fab_lang::parse(reference).map_err(|e| format!("reference does not parse: {e:?}"))?;
    let Some(fab_lang::StmtKind::ModuleDef { name, params, body }) =
        prog.stmts.first().map(|s| &s.kind)
    else {
        return Err("reference holds no module definition".into());
    };

    let mut em = Emitter {
        baked,
        siblings,
        locals: Vec::new(),
        fresh: 0,
        in_module: true,
        registered_defs: Vec::new(),
        def_name: None,
        def_root: None,
        fn_locals: Vec::new(),
    };
    let fn_ident = rust_fn_ident(name)?;
    let mut out = String::new();
    let _ = write!(
        out,
        "/// Generated native for module `{name}` — geometry through the interpreter's own\n\
         /// construction, so a generated module is what interpreting its reference builds.\n\
         pub(super) fn {fn_ident}(fx: &dyn rt::ModuleCtx) -> rt::Result<rt::Geo> {{\n"
    );
    // Parameters bind from the ctx's already-matched args — the evaluator applied OpenSCAD's
    // two-phase rule (all defaults, then arguments over them) and the AN.6 duplicate precedence, so
    // a native that re-derived them would be reimplementing exactly the semantics AN documents
    // getting wrong.
    for (i, p) in params.iter().enumerate() {
        // A `$`-named parameter (AR.22: `attachable`'s whole family declares `$fn=…` defaults)
        // needs NO lexical slot: `bind_values` bound it into the call frame's specials before
        // dispatch — caller arg or declared default, the same two-phase as any param — and every
        // body read of a `$`-name already rides `fx.dollar`, which reads exactly that frame. `$`
        // is not a Rust ident char anyway (the text census counted `let p_$tag` as emitting for
        // months while rustc had never seen one).
        if p.name.starts_with('$') {
            continue;
        }
        if params[..i].iter().any(|q| q.name == p.name) {
            return Err(format!(
                "{name}: duplicate parameter `{}` — AN.6 two-phase binding has no native shape",
                p.name
            ));
        }
        let _ = writeln!(
            out,
            "    let p_{} = fx.args().get({i}).cloned().unwrap_or(rt::Value::Undef);",
            p.name
        );
    }
    for p in params {
        if p.name.starts_with('$') {
            continue; // no lexical slot — body reads ride `fx.dollar` (see the prologue note)
        }
        em.locals
            .push((p.name.to_string(), format!("p_{}", p.name)));
    }
    let _ = writeln!(out, "    let mut parts: Vec<rt::Geo> = Vec::new();");
    // The body's top-level nested MODULE defs (AR.14.4.5), collected over the same flattened
    // list the runtime's `collect_module_defs` walks — bare `{}` blocks inline, nothing deeper.
    // Duplicates keep the last def (both sides are last-wins BTreeMaps), and every top def stmt
    // becomes a no-op in the statement walk: the one registration call below covers them all.
    let mut mod_def_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    {
        let mut stack: Vec<&fab_lang::Stmt> = vec![body];
        while let Some(s) = stack.pop() {
            match &s.kind {
                fab_lang::StmtKind::Block(inner) => stack.extend(inner.iter().rev()),
                fab_lang::StmtKind::ModuleDef { name, .. } => {
                    mod_def_names.insert(name);
                    em.registered_defs.push((s.span.start, s.span.end));
                }
                _ => {}
            }
        }
    }
    let locals_mark = em.locals.len();
    // The module BODY is the outermost hoist scope: bind its whole-scope assignments (blocks
    // flattened, last-wins) before any statement runs — see `Emitter::hoist_prelude`.
    let (hoist, _top_epilogue) = em
        .hoist_prelude(std::slice::from_ref(body), true)
        .map_err(|e| format!("{name}: {e}"))?;
    out.push_str(&hoist);
    if !mod_def_names.is_empty() {
        // AFTER the whole prelude, matching the interpreter's capture of the FULLY-hoisted block
        // scope: a def textually above a reassignment still sees the final value (cuboid's
        // `size`/`teardrop`). The frame is every local the top hoist bound; params and `$`-sets
        // already live in the call frame the runtime builds the capture from.
        let names = mod_def_names
            .iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let frame = em.locals[locals_mark..]
            .iter()
            .map(|(n, id)| format!("({n:?}, {id}.clone())"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "    fx.register_local_modules(&[{names}], &[{frame}])?;"
        );
    }
    let body_code = em.stmt(body).map_err(|e| format!("{name}: {e}"))?;
    out.push_str(&body_code);
    let _ = writeln!(out, "    Ok(fx.group(parts))\n}}");
    Ok(out)
}

/// What KIND of expression sits in call position, for the coverage histogram. A `computed callee`
/// decline covers several unrelated shapes — indexing a table of function literals, an immediately
/// applied lambda, a ternary picking between two functions — and they do not all cost the same to
/// support, so the histogram has to tell them apart.
fn callee_shape(callee: &Expr) -> &'static str {
    match &callee.kind {
        ExprKind::Index { .. } => "index (a table of function values)",
        ExprKind::FunctionLiteral { .. } => "an immediately applied literal",
        ExprKind::Ternary { .. } => "a ternary picking a function",
        ExprKind::Call { .. } => "the result of another call",
        ExprKind::Member { .. } => "a member access",
        ExprKind::Let { .. } => "a let binding a function",
        other => {
            // Deliberately not a catch-all label: an unnamed shape here is one nobody has looked
            // at, and it should read that way in the report.
            let _ = other;
            "some other expression"
        }
    }
}

/// A constant a generated native BAKES: the VALUE it compiles in, and the SCAD that binds the same
/// thing in the AR.10 fallback island.
///
/// The two are carried separately rather than one derived from the other, and that is AR.16's key
/// correction. Re-rendering a value back to scad has a hole with a name: `INF` is `1/0`, and `{:?}`
/// on infinity prints `inf`, which LEXES AS AN IDENTIFIER in scad — so the island would silently
/// bind `undef` where the native baked real bits. 27 BOSL2 functions read `INF`.
///
/// Carrying the library's VERBATIM source instead sidesteps that entirely and is more faithful
/// besides: the island binds exactly what the library wrote, so the two cannot disagree about what
/// the constant means. The bootstrap path, whose registry entries carry values rather than source,
/// still re-renders — and still refuses non-finite, because there it genuinely has nothing better.
#[derive(Debug, Clone, PartialEq)]
pub struct Baked {
    /// The value the native compiles in.
    pub value: fab_lang::Value,
    /// What the fallback island binds. The library's own source where there is one.
    pub scad: String,
}

impl Baked {
    /// From a library constant: the folded value, and the verbatim source that produced it.
    #[must_use]
    pub fn from_source(value: fab_lang::Value, scad: impl Into<String>) -> Self {
        Self {
            value,
            scad: scad.into(),
        }
    }

    /// From a bare value, re-rendering the scad. The bootstrap path only.
    ///
    /// # Errors
    /// A non-finite number, or a value with no scad literal form — see the type doc.
    pub fn from_value(value: fab_lang::Value) -> Result<Self, String> {
        let scad = value_to_scad(&value)?;
        Ok(Self { value, scad })
    }

    /// The Rust expression constructing this value, bit-exact.
    fn emit(&self) -> Result<String, String> {
        emit_value(&self.value)
    }
}

/// A value as a Rust expression that rebuilds it exactly. Floats go through `from_bits` — never a
/// decimal round-trip — so a baked constant is the same bits the interpreter folded.
///
/// # Errors
/// A value the emitter has no constructor for. Declines by NAME rather than emitting something
/// approximate, because a native holding a subtly different constant is a wrong answer.
fn emit_value(v: &fab_lang::Value) -> Result<String, String> {
    Ok(match v {
        fab_lang::Value::Num(n) => emit_num(*n),
        fab_lang::Value::Bool(b) => format!("rt::Value::Bool({b})"),
        fab_lang::Value::Str(s) => format!("rt::Value::string({:?})", &**s),
        fab_lang::Value::Undef => "rt::Value::Undef".to_string(),
        fab_lang::Value::NumList(xs) => format!(
            "rt::Value::num_list(vec![{}])",
            xs.iter()
                .map(|x| format!("f64::from_bits({:#x}_u64)", x.to_bits()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        fab_lang::Value::List(items) => format!(
            "rt::Value::list(vec![{}])",
            items
                .iter()
                .map(emit_value)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        other => return Err(format!("no bake form for {other:?}")),
    })
}

/// A value as SCAD source — the bootstrap path's re-render. See [`Baked`] for why the library path
/// does not use this.
///
/// # Errors
/// A non-finite number: `{:?}` prints `inf`/`NaN`, which lex as IDENTIFIERS in scad, so the island
/// would silently bind `undef` where the native baked real bits. Every NaN payload also formats
/// alike, which would blind the cross-batch conflict check.
fn value_to_scad(v: &fab_lang::Value) -> Result<String, String> {
    Ok(match v {
        fab_lang::Value::Num(n) if n.is_finite() => format!("{n:?}"),
        fab_lang::Value::Num(n) => return Err(format!("bakes non-finite {n}")),
        fab_lang::Value::Bool(b) => b.to_string(),
        fab_lang::Value::Str(s) => format!("{:?}", &**s),
        fab_lang::Value::NumList(xs) => {
            if let Some(bad) = xs.iter().find(|x| !x.is_finite()) {
                return Err(format!("bakes non-finite element {bad}"));
            }
            format!(
                "[{}]",
                xs.iter()
                    .map(|x| format!("{x:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        fab_lang::Value::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(value_to_scad)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        other => return Err(format!("no scad form for {other:?}")),
    })
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
pub fn generate_native(
    reference: &str,
    baked: &[(&str, Baked)],
    siblings: &[Sibling],
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
        in_module: false,
        registered_defs: Vec::new(),
        def_name: Some(name),
        def_root: Some(body),
        fn_locals: Vec::new(),
    };
    // The Rust fn NAME is keyword-escaped (BOSL2 has `function while(...)`); every STRING —
    // the fallback lookup, the mint def — stays the scad name, which is the runtime's key.
    let fn_ident = rust_fn_ident(name)?;
    let mut out = String::new();
    let _ = write!(
        out,
        "/// Generated native for `{name}` — semantics route through the interpreter's own value\n\
         /// algebra (`ops::`/`builtins::`), bit-identical to the interpreted reference by construction.\n\
         pub(super) fn {fn_ident}(fx: &dyn rt::FnCtx, args: &[rt::Value]) -> rt::Result<rt::Value> {{\n\
         \x20   // AR.10/AR.24: past the depth budget, DECLINE — the LIVE evaluator re-interprets\n\
         \x20   // the proven reference on one machine's explicit stack (ctx-less callers get the\n\
         \x20   // throwaway oracle instead); recursion cannot ride the Rust stack unbounded.\n\
         \x20   let Some(_depth) = rt::DepthGuard::enter() else {{\n\
         \x20       return fx.reinterpret(\"{name}\", FALLBACK_SOURCES, args);\n\
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
        // A `$`-named PARAMETER is no more a Rust local than a `$`-named `let` binder is — and it
        // matters MORE here, because `function_band`'s fixpoint uses `generate_native` returning
        // `Ok` as its liveness probe: an emitted `let p_$fn = …` stays LIVE in the batch and takes
        // the whole generated crate down with it. AR.27 closed the binder half and left this one;
        // the module emitter has declined `$`-params since AR.22. No BOSL2 function declares one
        // today, which is exactly the kind of latency that makes it worth a check rather than luck.
        if p.name.starts_with('$') {
            return Err(format!(
                "{name}: a `$`-named parameter (`{}`), which is dynamic rather than a local",
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
        //
        // A FALLIBLE default gets a `match` rather than `unwrap_or_else`, because a `?` inside a
        // closure returning `Value` does not type-check — the closure would have to return
        // `Result`, and `unwrap_or_else` does not want one. BOSL2's `binsearch(…, cmp=f_cmp())`
        // is the live case: its default CALLS a sibling. Found by a skeptic compiling the whole
        // 866-function band, which is a thing no gate could do until AR.26.2 could emit one.
        let bind = if default == "rt::Value::Undef" {
            format!("{getter}.cloned().unwrap_or(rt::Value::Undef)")
        } else if default.contains('?') {
            format!(
                "match {getter}.cloned() {{ Some(v) => v, None => {default} }}"
            )
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

/// How a subtree's `! # % *` prefixes change its emission — see [`Emitter::modifier_plan`].
#[derive(Clone, Copy)]
enum ModPlan {
    /// `*` — emit nothing, and do not walk the subtree at all.
    Skip,
    /// No modifier, or `#` (preview-only, which no evaluation arm reads).
    Plain,
    /// `%` — emit the subtree, then drop the geometry it produced.
    Background,
}

/// Emission state: the baked constants and callable siblings (name + DECLARED PARAMS, self
/// included — that is how self- and mutual recursion resolve, and how named sibling arguments bind
/// at COMPILE time), plus the LEXICAL SCOPE — scad name to Rust ident, innermost last, so `let`
/// shadowing resolves exactly as the interpreter's scope does.
struct Emitter<'a, 'e> {
    baked: &'a [(&'a str, Baked)],
    siblings: &'a [Sibling],
    locals: Vec<(String, String)>,
    fresh: usize,
    /// AR.17.2 — the def this emission compiles: scad NAME and parsed body ROOT, the two things
    /// a mint call needs (the name is baked into `fx.mint_fn` because siblings share one `fx`;
    /// the root is where literal paths are computed from). `None` outside a function-def
    /// emission — module bodies and the sibling default probe — where a literal declines.
    def_name: Option<&'e str>,
    def_root: Option<&'e Expr>,
    /// Rust idents whose binding RHS was SYNTACTICALLY a function literal. A call through one
    /// emits `call_value` UNCONDITIONALLY — the value is a `Function` by construction — where
    /// the general AN.10 shape needs a static else-half (which the letrec workers, bound to
    /// names that are no builtin or sibling, do not have). Never truncated: `fresh_ident`
    /// makes idents unique, so a stale entry cannot collide.
    fn_locals: Vec<String>,
    /// Emitting a MODULE body (so `fx` is a `ModuleCtx` in scope) rather than a function.
    ///
    /// It gates exactly one thing today — whether a `$`-read can be answered by `fx.dollar` — and
    /// it has to, because a generated FUNCTION has no ctx to ask (AR.17's `FnCtx` is what would
    /// give it one). Without the flag the function emitter would emit a call to a name that is not
    /// in scope, which is a build break rather than a decline, and only for the functions that
    /// happen to read a `$`-var.
    in_module: bool,
    /// Spans of the body-top-level nested DEFS this emission registered through the ctx
    /// (AR.14.4.5) — matched by span in `stmt`, where a registered def is a no-op and any other
    /// def still declines. Span, not name: a same-named def in a deeper scope is a DIFFERENT
    /// binding (the interpreter registers per block) and must not ride the top registration.
    registered_defs: Vec<(usize, usize)>,
}

impl Emitter<'_, '_> {
    /// A collision-proof Rust ident for a `let`-bound scad name (shadowing gets a new number).
    ///
    /// # Errors
    /// A `$`-NAMED binder, which is not a local at all — see [`Emitter::fresh_ident`]'s caller.
    fn fresh_ident(&mut self, name: &str) -> Result<String, String> {
        // A `$`-binding is DYNAMIC: `let($fn = 8) …` reaches every callee of the body, not just
        // the lexical children, so binding it as a Rust local would be wrong even if `$` were a
        // legal ident character. It is not, so the emitter produced `let l5_$attach_to = …` and
        // the file simply did not build — which nothing noticed until AR.27 widened the band
        // enough to include such a function. The MODULE side declined this at AR.20.4 for the
        // semantic reason; the function side never had the check at all. Third time this phase
        // has found a `$` counted as coverage it could not compile (AR.22 is the other two).
        if name.starts_with('$') {
            return Err(format!(
                "a `$`-binding (`{name}`), which is dynamic rather than a local"
            ));
        }
        let id = self.fresh;
        self.fresh += 1;
        // scad names may lead with `_` (idx's `_s`) — `l1__s` trips non_snake_case, and the
        // counter already guarantees uniqueness, so the underscores add nothing.
        Ok(format!("l{id}_{}", name.trim_start_matches('_')))
    }

    fn expr(&mut self, e: &Expr) -> Result<String, String> {
        use fab_lang::BinOp;
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
                    b.emit()
                } else if name.starts_with('$') && self.in_module {
                    // AR.20.3 — a `$`-read is DYNAMIC, so it is answered at run time off the
                    // inherited chain rather than baked. That is not an optimisation detail: the
                    // value depends on the CALLER, so baking one would freeze whatever it happened
                    // to be when the library was transpiled.
                    //
                    // `$children` needs no special case even though it is not a library variable —
                    // the evaluator binds it into every call frame alongside `$parent_modules`
                    // (`bind_call_bookkeeping`), so reading it through the chain gets the call
                    // site's real child count.
                    //
                    // MODULES ONLY. A generated FUNCTION has no ctx to ask (that is AR.17's
                    // `FnCtx`), so it keeps declining rather than silently reading a different
                    // variable.
                    Ok(format!("fx.dollar({name:?})"))
                } else {
                    Err(format!("free read `{name}` has no baked value"))
                }
            }
            ExprKind::Unary { op, operand } => Ok(format!(
                "rt::unary(fx, rt::UnOp::{op:?}, {})",
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
                "rt::binary(fx, rt::BinOp::{op:?}, {}, {})",
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
                Ok(format!("rt::member({}, {field:?})", self.expr(base)?))
            }
            ExprKind::Let { bindings, body } => self.let_expr(bindings, body),
            // Expression `assert`: raise where the interpreter raises, with the INTERPRETER'S
            // OWN verdict (AR.17.2 skeptic finding — `bosl_assert("generated")` was a fatal
            // `Eval` with the text lost, where the twin raises the soft-caught `Assert` carrying
            // the pretty-printed condition; outcome, geometry and console all diverged, `posmod`
            // included since band 3). The condition source is baked from OUR parse — fingerprint
            // identity makes it byte-equal to the twin's `print_expr`. A MESSAGE arg evaluates
            // in the failure branch only: the twin evaluates it eagerly-with-rollback even on a
            // pass, but the value is discarded and no registry reference carries a side-effectful
            // message, so the observable behavior is identical where anything arms.
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
                // A fired assert DECLINES in a module body (the interpreted re-run carries the
                // real message + non-fatality); in a function native it raises the twin verdict.
                let raise = if self.in_module {
                    "rt::assert_decline()".to_string()
                } else {
                    let msg = match args.get(1) {
                        None => "None".to_string(),
                        Some(m) if m.name.is_none() => format!("Some(&{})", self.expr(&m.value)?),
                        Some(_) => return Err("named assert message".into()),
                    };
                    let cond_src = fab_lang::print_expr(&cond.value);
                    format!("rt::assert_failed({msg}, {cond_src:?})")
                };
                Ok(format!(
                    "{{ if !({c}).is_truthy() {{ return Err({raise}); }} {b} }}"
                ))
            }
            // `echo(args) body` in EXPRESSION position — the AR.22 census found `cyl`, the most
            // instantiated shape module in the library, declining on THIS alone (its bool-radius
            // warning). The console side effect fires when the expression evaluates, then the
            // body yields (`undef` when absent).
            //
            // FUNCTIONS TOO since AR.27: `echo` moved onto the `Console` supertrait both ctx
            // shapes carry, so the reason this was module-only — "a function native has no console
            // to reach" — stopped being true the moment warnings needed one. 29 functions, and no
            // new semantics: the emission is identical on both sides, which is the point of the
            // capability living in one place.
            ExprKind::Echo { args, body } => {
                let mut pairs: Vec<String> = Vec::with_capacity(args.len());
                for a in args {
                    let v = self.expr(&a.value)?;
                    let n = match &a.name {
                        Some(n) => format!("Some({:?})", &**n),
                        None => "None".to_string(),
                    };
                    pairs.push(format!("({n}, {v})"));
                }
                let b = match body {
                    Some(b) => self.expr(b)?,
                    None => "rt::Value::Undef".to_string(),
                };
                Ok(format!("{{ fx.echo(&[{}])?; {b} }}", pairs.join(", ")))
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
                    // AR.17 stage C — a COMPUTED callee (`f(a)(b)` curried application, `fns[i]`
                    // indexing a table of function values) is a VALUE in call position, and
                    // invoking a value is exactly the one capability `FnCtx` declares. The
                    // interpreter's `CallValue` machinery runs it; a non-function value answers
                    // `undef` there, which `call_value`'s decline reproduces by re-interpreting.
                    // A function LITERAL in call position still declines below — the value must
                    // come from somewhere the emitter can already express (a call, an index, a
                    // parameter), not be MINTED here, which needs the closure table.
                    if matches!(callee.kind, ExprKind::FunctionLiteral { .. }) {
                        return Err(format!("computed callee: {}", callee_shape(callee)));
                    }
                    let cal = self.expr(callee)?;
                    let mut pairs: Vec<String> = Vec::with_capacity(args.len());
                    for a in args {
                        let v = self.expr(&a.value)?;
                        let n = match &a.name {
                            Some(n) => format!("Some({:?})", &**n),
                            None => "None".to_string(),
                        };
                        pairs.push(format!("({n}, {v})"));
                    }
                    return Ok(format!("fx.call_value(&{cal}, &[{}])?", pairs.join(", ")));
                };
                self.call(name, args)
            }
            // AR.17.2 — a literal is MINTED by the evaluator from the fingerprint-proven def,
            // never compiled: its body stays interpreted AST, which is what makes nested
            // literals, echo and `$`-reads inside it cost nothing.
            ExprKind::FunctionLiteral { .. } => self.function_literal(e, None),
            other => Err(format!("construct outside the v0 subset: {other:?}")
                .chars()
                .take(120)
                .collect()),
        }
    }

    /// AR.17.2 — a function-literal expression becomes `fx.mint_fn(def, path, self_name,
    /// captures)`. The PATH is child indices from the def's body root under the parser's
    /// canonical `expr_children` ordering — the fingerprint proves it addresses the same node in
    /// the program's loaded definition. The CAPTURES are the literal subtree's free names that
    /// are emitter locals, in VALUE and CALL position both — a call-position miss is a silent
    /// `undef` where the interpreted twin resolves a param-held function through rung 1 (the
    /// f_* battery's whole shape). Names that are no local resolve at invoke through the island
    /// base or dispatch, identically in both tiers, so they need no capture — and no bake.
    /// `binder` mirrors `name_closure`: the let-binder's name when the literal is a binding
    /// RHS, so the 11 letrec self-recursions re-inject.
    fn function_literal(&mut self, e: &Expr, binder: Option<&str>) -> Result<String, String> {
        if self.in_module {
            // ModuleCtx carries no mint capability (and no armed module contains a literal
            // today) — the module-body first-class band is its own box, not a silent gap.
            return Err("a function literal in a module body".into());
        }
        let (Some(def), Some(root)) = (self.def_name, self.def_root) else {
            return Err("a function literal outside a compiled def body".into());
        };
        // The path space is the BODY: a literal in the def's own parameter defaults sits
        // outside it and declines (zero occurrences in BOSL2 — defensive, not load-bearing).
        let path =
            fab_lang::find_expr_path(root, e).ok_or("a function literal in a parameter default")?;
        // A `$`-read in THIS literal's param defaults evaluates against the minted env's base
        // chain — severed from the creation site's `$`-context, which the twin's whole-scope
        // capture reaches (skeptic finding: q = 42 interpreted vs 0 minted). Body `$`-reads
        // ride the INVOKE site's chain identically in both tiers, and nested literals are
        // minted by the interpreter itself — only the top literal's own defaults are the hole.
        // Zero occurrences in BOSL2; decline loudly rather than arm wrong.
        if let ExprKind::FunctionLiteral { params, .. } = &e.kind {
            for p in params {
                if let Some(d) = &p.default
                    && subtree_reads_dollar(d)
                {
                    return Err("a `$`-read in a function-literal parameter default".into());
                }
            }
        }
        let mut free = std::collections::BTreeSet::new();
        free_reads(e, &mut Vec::new(), &mut free);
        let caps: Vec<String> = free
            .iter()
            .filter_map(|n| {
                self.locals
                    .iter()
                    .rev()
                    .find(|(s, _)| s == n)
                    .map(|(_, ident)| format!("({n:?}, {ident}.clone())"))
            })
            .collect();
        let self_name = binder.map_or_else(|| "None".to_string(), |b| format!("Some({b:?})"));
        let path_s = path
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "fx.mint_fn({def:?}, &[{path_s}], {self_name}, &[{}])?",
            caps.join(", ")
        ))
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
            // A literal RHS gets the binder as `self_name` (`name_closure`'s stamp — letrec
            // self-recursion rides it) and marks the ident as definitely-a-Function, which
            // collapses the AN.10 conditional at its call sites. The RHS is emitted BEFORE the
            // binding lands in `locals`, so a self-reference inside the literal is NOT a
            // capture — exactly the interpreter's capture-at-bind order.
            let literal_rhs = matches!(b.value.kind, ExprKind::FunctionLiteral { .. });
            let val = if literal_rhs {
                self.function_literal(&b.value, Some(bn))?
            } else {
                self.expr(&b.value)?
            };
            let ident = self.fresh_ident(bn)?;
            let _ = write!(block, "let {ident} = {val}; ");
            if literal_rhs {
                self.fn_locals.push(ident.clone());
            }
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
        let acc = self.fresh_ident("acc")?;
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
    /// function is exactly this. In a MODULE body everything DISPATCHES (`fx.call_fn`,
    /// AR.14.4.3, whose rung 1 re-checks the call frame at runtime); in a FUNCTION native a
    /// lexically-shadowed call emits the interpreter's rung-1 rule INLINE (AR.17 stage C): a
    /// function VALUE in the binding is invoked through `fx.call_value`, anything else does not
    /// shadow (AD.1/p8) and the static resolution — builtins (names decorative, AR.3) then
    /// generated siblings with the full compile-time binding rules — runs instead. Sound because
    /// a function native's lexical world is EXACTLY the bindings the emitter saw: params and its
    /// own emitted `let`s, with the globals fenced off by `lookup_local_function`'s parentless
    /// stop.
    fn call(&mut self, name: &str, args: &[Arg]) -> Result<String, String> {
        let shadow = self
            .locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, ident)| ident.clone());
        if let Some(ident) = &shadow {
            // A MODULE body's param shadow is re-checked by `call_fn` rung 1 at runtime — but a
            // HOISTED local is a Rust `let` the frame never sees, so rung 1 cannot guard it:
            // decline. The `p_` prefix is the param marker (`fresh_ident` never produces it).
            if self.in_module && !ident.starts_with("p_") {
                return Err(format!(
                    "call through the local binding `{name}` (the AN.10 shape) resolves at runtime"
                ));
            }
        }
        if !self.in_module
            && let Some(ident) = &shadow
        {
            let cal = self.fresh_ident("cal")?;
            let mut pairs: Vec<String> = Vec::with_capacity(args.len());
            for a in args {
                let v = self.expr(&a.value)?;
                let n = match &a.name {
                    Some(n) => format!("Some({:?})", &**n),
                    None => "None".to_string(),
                };
                pairs.push(format!("({n}, {v})"));
            }
            // AR.17.2 — a binding whose RHS was a LITERAL is a `Function` by construction, so
            // the rung-1 conditional collapses to an unconditional invoke. This is what
            // un-declines the letrec workers (`a = function(…) … a(…)`): their names are no
            // builtin or sibling, so the general shape's static else-half does not exist.
            if self.fn_locals.iter().any(|f| f == ident) {
                return Ok(format!("fx.call_value(&{ident}, &[{}])?", pairs.join(", ")));
            }
            let static_call = self.static_call(name, args)?;
            // Both branches evaluate their OWN argument emissions; exactly one branch runs, so
            // side effects fire once either way.
            return Ok(format!(
                "{{ let {cal} = &{ident}; if matches!({cal}, rt::Value::Function {{ .. }}) {{ \
                 fx.call_value({cal}, &[{}])? }} else {{ {static_call} }} }}",
                pairs.join(", ")
            ));
        }
        // AR.14.4.3 — a MODULE body's function calls dispatch at RUNTIME through
        // `ModuleCtx::call_fn`, the same AN.10-safe design module calls ride: the callee is
        // whatever the program defines, so a user shadow of a builtin resolves correctly (the
        // fix `rt::bi` could never carry) and a sibling function needs no co-generated table.
        // Emit-time declines remain only for shapes `call_fn` hands straight back to the
        // interpreter — arming a module that declines on its every call would be noise.
        if self.in_module {
            let mut pairs: Vec<String> = Vec::with_capacity(args.len());
            for a in args {
                let v = self.expr(&a.value)?;
                let n = match &a.name {
                    Some(n) => format!("Some({:?})", &**n),
                    None => "None".to_string(),
                };
                pairs.push(format!("({n}, {v})"));
            }
            return Ok(format!(
                "fx.call_fn(&rt::FnCall {{ name: {name:?}, args: &[{}] }})?",
                pairs.join(", ")
            ));
        }
        self.static_call(name, args)
    }

    /// The STATIC half of a function native's call resolution: a pure builtin by direct `rt::bi`
    /// call, or a generated sibling with the compile-time binding rules. What runs when no local
    /// binding shadows the name — or when one does and holds a non-function at runtime (AD.1/p8).
    fn static_call(&mut self, name: &str, args: &[Arg]) -> Result<String, String> {
        // The FILE-VALUE builtins resolve paths off the calling FILE's directory binding and are
        // NOT in `BUILTIN_SURFACE` (they are expression forms of statements), so `is_builtin` is
        // false for them and they fall past every builtin arm below. Before AR.27 they then hit the
        // no-sibling DECLINE; after it they hit dispatch, and `call_named_outward` has no rung for
        // them — the interpreter resolves these BEFORE builtins — so a compiled `import(f)` answered
        // warn-and-`undef` where the interpreter read the file. A regression from missed speedup to
        // wrong ANSWER, and the missing-import-is-silent class besides.
        //
        // HERE rather than at the top of `call`, which is where it was first put and one branch too
        // late: the AN.10 lexical-shadow arm calls `static_call` for its else-half, so a body with a
        // local literally named `import` would have walked straight past a guard placed earlier.
        // `static_call` is the single funnel. (The module emitter keeps its own copy: it declines
        // before reaching here at all.)
        if matches!(name, "import" | "dxf_dim" | "dxf_cross") {
            return Err(format!("a call to the file-reading builtin `{name}`"));
        }
        if fab_lang::is_builtin(name) {
            // The capability lives in the DECLARATION (AS.2/AS.4): only a `Pure` builtin has an
            // `rt::bi` function to call. A context builtin (argument names, the rand stream, the
            // module stack) has NO function there, so this decline is belt — even a missed check
            // would emit a path that does not COMPILE, not a call that silently answers `undef`
            // (the AR.20.10 shape the old CONTEXT_BUILTINS hand list existed to patch).
            let pure = fab_lang::surface::BUILTIN_SURFACE.iter().any(|b| {
                b.decl.name == name && b.capability == fab_lang::surface::BuiltinCapability::Pure
            });
            if !pure {
                return Err(format!(
                    "a call to `{name}`, which needs evaluator context (a non-Pure builtin)"
                ));
            }
            let emitted: Vec<String> = args
                .iter()
                .map(|a| self.expr(&a.value))
                .collect::<Result<_, _>>()?;
            return Ok(format!("rt::bi::{name}(&[{}])", emitted.join(", ")));
        }
        // A `$`-ARGUMENT sets a DYNAMIC binding for the callee, which a static Rust call has no
        // frame to carry — so route the whole call through dispatch instead of declining it. The
        // runtime's `fill_slots` already separates `$`-args from positional slots and `bind_values`
        // binds them into the call frame's dynamic parent, exactly as the interpreter does. Costs a
        // dispatch where a static call would have done, buys the calls back.
        let dynamic_arg = args
            .iter()
            .any(|a| a.name.as_deref().is_some_and(|n| n.starts_with('$')));
        let Some(sib) = self.siblings.iter().find(|s| s.name == name).filter(|_| !dynamic_arg)
        else {
            // AR.27 — the callee is not in this batch, so DISPATCH instead of declining. Measured:
            // this is the difference between 866 and 1162 of BOSL2's 1329 functions, because a
            // static sibling call means a decliner takes every one of its callers with it and only
            // ~88 of the 463 dropped had a reason of their own.
            //
            // ARGUMENTS GO OVER AS WRITTEN, names attached, and the runtime matches slots against
            // whatever the name actually resolved to. Positionalising here would mean matching
            // against the LIBRARY's parameter list for a callee that resolves against the USER's
            // program — AN.10 wearing a function's hat. AR.20.8 found and removed exactly that
            // hazard on the module side, and the fix is the same: make no claim about the callee
            // and there is nothing left to violate. It also costs nothing in coverage, where
            // declining named args cost ~60 functions.
            // ARGUMENTS EVALUATE EAGERLY HERE, and the interpreter does not always evaluate
            // them at all: an UNKNOWN callee's args are never scheduled (`Task::Const(Undef)`
            // straight away), and `fill_arg_slots` drops a duplicated name or a surplus positional
            // UNEVALUATED — a rule this codebase states at the intrinsic gate for exactly this
            // reason. Where the argument is pure that difference is unobservable; where it ECHOES
            // or ASSERTS it is a console line the interpreter never prints, or a run-killing error
            // where the twin merely warns. Refuse those rather than fire them.
            //
            // The residue, stated rather than hidden: an arithmetic type error inside a dropped
            // argument still warns compiled and not interpreted. It needs an unknown-or-overflowing
            // callee AND a mistyped argument, and closing it properly means handing the runtime
            // THUNKS instead of values — a real ABI change, not a patch.
            for a in args {
                if has_side_effect(&a.value) {
                    return Err(format!(
                        "a side-effecting argument to the dispatched callee `{name}` (the \
                         interpreter may never evaluate it)"
                    ));
                }
            }
            let emitted: Vec<String> = args
                .iter()
                .map(|a| {
                    let v = self.expr(&a.value)?;
                    Ok(match &a.name {
                        Some(n) => format!("(Some({:?}), {v})", &**n),
                        None => format!("(None, {v})"),
                    })
                })
                .collect::<Result<_, String>>()?;
            // The CALLING def's own name goes over too: the runtime resolves a bound callee in
            // THIS function's lexical base, which is the only chain the interpreted twin walks.
            let def = self
                .def_name
                .ok_or("an outward call from a body with no definition name")?;
            return Ok(format!(
                "fx.call_named({def:?}, {name:?}, &[{}])?",
                emitted.join(", ")
            ));
        };
        // A generated sibling: everything is static, so the FULL binding rules run at COMPILE
        // time — a positional takes the lowest unfilled slot (AN.2), a named arg its declared
        // slot. The flat-slice ABI can only express a contiguous PREFIX of filled slots
        // (trailing unfilled fall to the callee's defaults); anything else declines.
        let params = sib.params.clone();
        let defaults = sib.defaults.clone();
        let mut slots: Vec<Option<String>> = vec![None; params.len()];
        for a in args {
            let v = self.expr(&a.value)?;
            let slot = if let Some(n) = &a.name {
                if n.starts_with('$') {
                    // Unreachable: `dynamic_arg` above routes such a call to dispatch. Belt, and
                    // it would be a DECLINE rather than a wrong answer if the guard ever moved.
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
        // AR.18 — a HOLE (an unfilled slot with a filled one after it) gets the callee's OWN
        // default, which is exactly what the callee would bind for itself. `Value::Undef` would
        // NOT do: `args.get(i)` would then return `Some`, the callee's `unwrap_or(default)` would
        // never fire, and an explicit undef would silently override a real default — AN.3's bug in
        // compiled form. See `Sibling` for why inlining the default here is position-independent.
        let last_filled = slots.iter().rposition(Option::is_some);
        let sib_ident = rust_fn_ident(name)?;
        let Some(last) = last_filled else {
            return Ok(format!("{sib_ident}(fx, &[])?"));
        };
        let mut vals: Vec<String> = Vec::with_capacity(last + 1);
        for (i, slot) in slots.into_iter().enumerate().take(last + 1) {
            match slot {
                Some(v) => vals.push(v),
                // A defaultless param binds `undef` when unfilled (AN.3), which the callee's own
                // `unwrap_or(rt::Value::Undef)` already does — so passing it explicitly agrees.
                None => match defaults.get(i) {
                    Some(Some(d)) => vals.push(d.clone()),
                    Some(None) => vals.push("rt::Value::Undef".to_string()),
                    None => {
                        return Err(format!(
                            "hole at slot {i} of sibling `{name}` with no declared parameter"
                        ));
                    }
                },
            }
        }
        Ok(format!("{sib_ident}(fx, &[{}])?", vals.join(", ")))
    }

    /// AR.20.4 — one STATEMENT as Rust that pushes any geometry it makes into `parts`.
    ///
    /// The statement mirror of [`Emitter::expr`]. Declines name the construct, because a module
    /// that silently skipped a statement would render MISSING GEOMETRY while still succeeding —
    /// the failure shape this phase keeps finding.
    /// The hoisted-assignment PRELUDE of one scope — the emitter's mirror of the evaluator's
    /// `hoisted_assignments` + `hoist_scope` (OpenSCAD's whole-scope, last-assignment-wins rule):
    /// nested BLOCKS flatten into the enclosing scope, names dedupe to their FIRST-occurrence
    /// position carrying the LAST expression, and each binding's expression is emitted with only
    /// the bindings hoisted BEFORE it in scope — a self- or forward-reference therefore reads the
    /// OUTER binding (a param, a bake), exactly as `hoist_scope`'s sequential bind resolves it.
    ///
    /// Emitting assignments at their statement POSITION instead (the first design) was a silent
    /// tier divergence, not a decline: `module m(x) { cube(x); x = 5; }` rendered `cube(x-arg)`
    /// compiled where the interpreter renders `cube(5)`, and `x = 1; cube(x); x = 5;` evaluated
    /// an expression the interpreter's dedupe never runs. Every scope the interpreter hoists
    /// (module body, `if` branches, `for` iteration bodies, `let`/`assert`/`echo` children) calls
    /// this before walking statements; the `Assignment` arm below is then a no-op.
    /// Returns `(prelude, epilogue)`: the epilogue RESTORES nested `$`-sets at scope exit and
    /// must be emitted after the scope's statements — empty for the top scope (a top-of-body
    /// `$`-set persists for the whole call; the frame dies with it) and whenever no `$`-set
    /// occurred. Error paths that skip it are fine: an `Err` aborts or declines the whole call,
    /// and the frame's state dies unread.
    fn hoist_prelude(&mut self, stmts: &[Stmt], top: bool) -> Result<(String, String), String> {
        // flatten_blocks + dedupe, mirroring eval's `hoisted_bindings` byte for byte: a `{ }`
        // is NOT an assignment scope upstream — its assignments belong to the enclosing scope.
        // Nested `function` DEFS share the hoist's variable namespace (the interpreter binds a
        // closure VALUE by the fn's name), so they walk in the SAME first-occurrence order — at
        // the body's top scope one becomes a `register_local_fn` at its position (AR.14.4.5),
        // carrying the locals hoisted BEFORE it, which is the interpreter's capture-at-bind view.
        enum Hoisted<'s> {
            Assign(&'s fab_lang::Expr),
            Fn((usize, usize)),
        }
        let mut flat: Vec<&Stmt> = Vec::new();
        let mut stack: Vec<&Stmt> = stmts.iter().rev().collect();
        while let Some(s) = stack.pop() {
            if let StmtKind::Block(inner) = &s.kind {
                stack.extend(inner.iter().rev());
            } else {
                flat.push(s);
            }
        }
        let mut order: Vec<(&str, Hoisted<'_>)> = Vec::new();
        let mut index: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for s in flat {
            match &s.kind {
                StmtKind::Assignment { name, value } => {
                    if let Some(&i) = index.get(&**name) {
                        // seen: last expr wins, first-occurrence position kept — unless the slot
                        // is a nested FN, whose cross-kind last-wins the emitter does not mirror.
                        if matches!(order[i].1, Hoisted::Fn(_)) {
                            return Err(format!(
                                "a nested function definition sharing the hoist slot of `{name}`"
                            ));
                        }
                        order[i].1 = Hoisted::Assign(value);
                    } else {
                        index.insert(name, order.len());
                        order.push((name, Hoisted::Assign(value)));
                    }
                }
                StmtKind::FunctionDef { name, .. } => {
                    if !top {
                        return Err(
                            "a nested function definition below the module body's top level".into(),
                        );
                    }
                    // The interpreter's whole-scope last-wins would rebind the SLOT (an earlier
                    // assignment or an earlier def of the same name) — a per-position
                    // registration cannot reproduce that, so any name sharing declines. A
                    // parameter collision is the same hazard one frame up: the hoisted closure
                    // shadows the param for the whole body, which the emitted `p_` reads
                    // would miss.
                    if index.contains_key(name.as_str())
                        || self.locals.iter().any(|(n, _)| n == name.as_str())
                    {
                        return Err(format!(
                            "a nested function definition sharing the hoist slot of `{name}`"
                        ));
                    }
                    index.insert(name, order.len());
                    order.push((name, Hoisted::Fn((s.span.start, s.span.end))));
                }
                _ => {}
            }
        }
        let mut out = String::new();
        let mut epilogue = String::new();
        // The locals THIS hoist bound so far — what a nested fn at this position captures.
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (name, entry) in order {
            let expr = match entry {
                Hoisted::Assign(expr) => expr,
                Hoisted::Fn(span) => {
                    let frame = pairs
                        .iter()
                        .map(|(n, id)| format!("({n:?}, {id}.clone())"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = writeln!(out, "    fx.register_local_fn({name:?}, &[{frame}])?;");
                    self.registered_defs.push(span);
                    continue;
                }
            };
            let v = self.expr(expr)?;
            if name.starts_with('$') {
                // The dynamic bind, in the same first-occurrence/last-wins slot a lexical one
                // takes — a later prelude binding whose expression reads this `$`-name sees the
                // NEW value (`fx.dollar` reads the frame just written), matching `hoist_scope`'s
                // sequential bind. The self-reference rule holds too: THIS binding's own
                // expression was emitted before the set, so `$x = $x * m` reads the inherited
                // `$x`. A NESTED scope's set is branch-scoped in the interpreter (its hoist binds
                // a child scope), which the frame write reproduces as SAVE + SET + scope-exit
                // RESTORE: everything inside the scope — callees, rendered children — reads the
                // frame while set, nothing outside reads between set and restore, and a saved
                // `undef` restoring as bound-undef is observationally the unbound read it was.
                if top {
                    let _ = writeln!(out, "    fx.set_dollar({name:?}, {v});");
                } else {
                    let save = self.fresh_ident("sd")?;
                    let _ = writeln!(
                        out,
                        "    let {save} = fx.dollar({name:?});\n    fx.set_dollar({name:?}, {v});"
                    );
                    // Restores unwind in REVERSE set order.
                    epilogue = format!("    fx.set_dollar({name:?}, {save});\n{epilogue}");
                }
                continue; // NOT a Rust local — body reads ride `fx.dollar`, the existing path.
            }
            let id = self.fresh_ident(name)?;
            let _ = writeln!(out, "    let {id} = {v};");
            pairs.push((name.to_string(), id.clone()));
            self.locals.push((name.to_string(), id));
        }
        Ok((out, epilogue))
    }

    fn stmt(&mut self, s: &Stmt) -> Result<String, String> {
        match &s.kind {
            // A lone `;` is not a child and contributes nothing (L.5.2).
            StmtKind::Empty => Ok(String::new()),
            // A block is an implicit GROUP: its own statements union, and that union is one part.
            StmtKind::Block(kids) => {
                // A block is an implicit GROUP: its statements union into ONE part of the
                // enclosing list. Its own list gets a fresh name so it cannot shadow the parent's.
                // NO locals boundary here — a block is not an assignment scope (see
                // `hoist_prelude`: its assignments were hoisted by the ENCLOSING scope), and
                // nothing else inside pushes locals that outlive its own arm.
                let inner = self.fresh_ident("blk")?;
                let mut out = format!(
                    "    let {inner} = {{\n        let mut parts: Vec<rt::Geo> = Vec::new();\n"
                );
                for k in kids {
                    out.push_str(&self.stmt(k)?);
                }
                let _ = write!(
                    out,
                    "        fx.group(parts)\n    }};\n    parts.push({inner});\n"
                );
                Ok(out)
            }
            // Bound by the scope's `hoist_prelude` before any statement ran (whole-scope
            // last-wins); in statement position there is nothing left to emit.
            StmtKind::Assignment { .. } => Ok(String::new()),
            StmtKind::If {
                modifiers,
                cond,
                then,
                els,
                ..
            } => {
                // A statement `if` carries the SAME `! # % *` prefixes a module call does
                // (`StmtKind::If.modifiers`), and this arm used to destructure them away with `..`.
                // That is a silent WRONG RENDER, not a missed optimisation: `*if (c) cube(1);`
                // draws nothing in the interpreter — `*` returns before the condition is even
                // evaluated (geo_stack.rs) — while the emitted code drew the cube. Verified by
                // probe: `*if`, `%if` and a bare `if` all produced byte-identical output.
                let bg = match Self::modifier_plan(*modifiers)? {
                    ModPlan::Skip => return Ok(String::new()),
                    ModPlan::Background => Some(self.fresh_ident("bg")?),
                    ModPlan::Plain => None,
                };
                let c = self.expr(cond)?;
                // Each branch is its own hoist scope (the interpreter runs a branch as a fresh
                // EvalNodes child scope): its assignments bind for the branch only.
                let mut out = format!("    if ({c}).is_truthy() {{\n");
                let mark = self.locals.len();
                let (prelude, epilogue) = self.hoist_prelude(then, false)?;
                out.push_str(&prelude);
                for k in then {
                    out.push_str(&self.stmt(k)?);
                }
                out.push_str(&epilogue);
                self.locals.truncate(mark);
                out.push_str("    } else {\n");
                let (prelude, epilogue) = self.hoist_prelude(els, false)?;
                out.push_str(&prelude);
                for k in els {
                    out.push_str(&self.stmt(k)?);
                }
                out.push_str(&epilogue);
                self.locals.truncate(mark);
                out.push_str("    }\n");
                if let Some(mark) = bg {
                    // `%` on an `if`: the branch still runs, its geometry does not survive.
                    out = format!("    let {mark} = parts.len();\n{out}");
                    let _ = writeln!(out, "    parts.truncate({mark});");
                }
                Ok(out)
            }
            StmtKind::Module(mi) => self.module_call(mi),
            // A body-top-level def was REGISTERED (AR.14.4.5) — by `register_local_modules`
            // after the prelude or `register_local_fn` at its hoist position — so in statement
            // position there is nothing left to emit. Matched by SPAN: a same-named def in a
            // deeper scope is a different binding and still declines below.
            StmtKind::ModuleDef { .. } | StmtKind::FunctionDef { .. }
                if self.registered_defs.contains(&(s.span.start, s.span.end)) =>
            {
                Ok(String::new())
            }
            StmtKind::ModuleDef { .. } => {
                Err("a nested module definition below the body's top level".into())
            }
            StmtKind::FunctionDef { .. } => {
                Err("a nested function definition below the body's top level".into())
            }
            StmtKind::Use(_) | StmtKind::Include(_) => {
                Err("a use/include inside a module body".into())
            }
        }
    }

    /// What OpenSCAD's `! # % *` prefixes mean for emission. They are NOT symmetric, and this
    /// mirrors `geo_stack::dispatch_module` in its order.
    ///
    /// A DECISION rather than a wrapper taking a closure, because `*` must not merely discard the
    /// body — it must never WALK it. A disabled subtree cannot be got wrong, so an unsupported
    /// construct inside one must not decline the module around it.
    ///
    /// # Errors
    /// Naming the modifier, for the one this emitter cannot reproduce.
    fn modifier_plan(md: fab_lang::Modifiers) -> Result<ModPlan, String> {
        // `*` DISABLES: no geometry AND no side effects. The interpreter returns before it
        // evaluates a call's arguments, or an `if`'s condition.
        if md.disable {
            return Ok(ModPlan::Skip);
        }
        // `!` diverts the subtree into the program-global ROOT override, which no `ModuleCtx`
        // method reaches. Declined under its own name rather than folded into a four-way bucket,
        // so the histogram can price it.
        if md.root {
            return Err("a `!` root modifier".into());
        }
        // `%` BACKGROUND still EVALUATES the subtree — echoes fire, asserts fire, `rands` draws
        // advance the stream (AK.3, oracle-probed) — and only its GEOMETRY leaves the output.
        // Treating it like `*` was assumed once before and refuted against the oracle, because
        // skipping the draws shifts the random stream for everything downstream, so the geometry
        // that goes wrong ends up somewhere else entirely.
        if md.background {
            return Ok(ModPlan::Background);
        }
        // `#` HIGHLIGHT is preview-only: no evaluation arm reads it, so it emits as if unwritten.
        Ok(ModPlan::Plain)
    }

    /// A statement's CHILDREN as one implicit-union part, in their own hoist scope — the shape the
    /// interpreter's A3 arm pushes for `echo`/`assert` children (one `Combinator::Union` node, iff
    /// any children exist). Inlining them into the enclosing parts list instead renders the same
    /// GEOMETRY but a different TREE, and the tiers must be indistinguishable structurally too —
    /// the CSG memo keys on the tree.
    fn union_part(&mut self, children: &[Stmt]) -> Result<String, String> {
        if children.is_empty() {
            return Ok(String::new());
        }
        let inner = self.fresh_ident("un")?;
        let mark = self.locals.len();
        let mut out =
            format!("    let {inner} = {{\n        let mut parts: Vec<rt::Geo> = Vec::new();\n");
        let (prelude, epilogue) = self.hoist_prelude(children, false)?;
        out.push_str(&prelude);
        for k in children {
            out.push_str(&self.stmt(k)?);
        }
        out.push_str(&epilogue);
        let _ = write!(
            out,
            "        fx.group(parts)\n    }};\n    parts.push({inner});\n"
        );
        self.locals.truncate(mark);
        Ok(out)
    }

    /// A module INSTANTIATION in statement position.
    fn module_call(&mut self, mi: &fab_lang::ModuleInstantiation) -> Result<String, String> {
        match Self::modifier_plan(mi.modifiers)? {
            ModPlan::Skip => return Ok(String::new()),
            ModPlan::Background => {
                let mark = self.fresh_ident("bg")?;
                let mut out = format!("    let {mark} = parts.len();\n");
                out.push_str(&self.module_call_body(mi)?);
                // AFTER the body, never a scope guard: the interpreter's `DiscardAbove` is a WORK
                // task, so its error DRAIN throws it away unrun. A `?` inside the emitted body
                // jumps past this line, which is the same behaviour for free.
                let _ = writeln!(out, "    parts.truncate({mark});");
                return Ok(out);
            }
            ModPlan::Plain => {}
        }
        self.module_call_body(mi)
    }

    /// [`module_call`](Self::module_call) with the modifiers already decided.
    fn module_call_body(&mut self, mi: &fab_lang::ModuleInstantiation) -> Result<String, String> {
        match mi.name.as_str() {
            "children" => {
                // The interpreter evaluates EVERY argument but selects with the first POSITIONAL
                // one only (`module::eval_args` + `positional.first()`): `children(c=0)` renders
                // ALL children. Named args still evaluate — into discarded bindings — for
                // eval-order parity. Taking `args.first()` regardless of its name selected child
                // 0 compiled where the interpreter rendered everything.
                let mut out = String::new();
                let mut sel: Option<String> = None;
                for a in &mi.args {
                    let v = self.expr(&a.value)?;
                    if a.name.is_none() && sel.is_none() {
                        let id = self.fresh_ident("sel")?;
                        let _ = writeln!(out, "    let {id} = {v};");
                        sel = Some(id);
                    } else {
                        let id = self.fresh_ident("arg")?;
                        let _ = writeln!(out, "    let _{id} = {v};");
                    }
                }
                match sel {
                    Some(id) => {
                        let _ = writeln!(out, "    parts.push(fx.child_at(&{id})?);");
                    }
                    None => {
                        let _ = writeln!(out, "    parts.push(fx.children()?);");
                    }
                }
                Ok(out)
            }
            // Statement `for` — the same nesting the comprehension side already emits, but each
            // iteration's body contributes GEOMETRY rather than elements. 69 of 416 modules.
            "for" => {
                let mark = self.locals.len();
                let mut out = String::new();
                let mut depth = 0;
                let mut restores = String::new();
                for b in &mi.args {
                    let Some(bn) = &b.name else {
                        return Err("an unnamed `for` binding".into());
                    };
                    // Each iterable is emitted INSIDE the enclosing loops, so a later binding sees
                    // the earlier binders — the interpreter's nesting order.
                    let iter = self.expr(&b.value)?;
                    if bn.starts_with('$') {
                        // A `$`-named BINDER (AR.22: the copies family iterates `$idx` so the
                        // children it renders can read it): per-iteration SET into the frame,
                        // saved before the loop opens and restored after every loop closes —
                        // reads inside the body ride `fx.dollar`, so no lexical slot exists.
                        let save = self.fresh_ident("sd")?;
                        let _ = writeln!(out, "    let {save} = fx.dollar({bn:?});");
                        let ident = self.fresh_ident("dv")?;
                        let _ =
                            writeln!(out, "    for {ident} in rt::iter_values_warned(fx, &{iter}) {{");
                        let _ = writeln!(out, "    fx.set_dollar({bn:?}, {ident}.clone());");
                        restores = format!("    fx.set_dollar({bn:?}, {save});\n{restores}");
                    } else {
                        let ident = self.fresh_ident(bn)?;
                        let _ =
                            writeln!(out, "    for {ident} in rt::iter_values_warned(fx, &{iter}) {{");
                        self.locals.push((bn.to_string(), ident));
                    }
                    depth += 1;
                }
                // The body is a fresh hoist scope PER ITERATION (the interpreter pushes one
                // EvalNodes per iteration) — the prelude sits inside the innermost loop.
                let (prelude, epilogue) = self.hoist_prelude(&mi.children, false)?;
                out.push_str(&prelude);
                for k in &mi.children {
                    out.push_str(&self.stmt(k)?);
                }
                out.push_str(&epilogue);
                for _ in 0..depth {
                    out.push_str("    }\n");
                }
                out.push_str(&restores);
                self.locals.truncate(mark);
                Ok(out)
            }
            // Statement `let`: binds for its CHILDREN only, then the bindings leave scope. 8 of 416.
            "let" => {
                let mark = self.locals.len();
                let mut out = String::new();
                let mut restores = String::new();
                for b in &mi.args {
                    let Some(bn) = &b.name else {
                        return Err("an unnamed `let` binding".into());
                    };
                    let v = self.expr(&b.value)?;
                    if bn.starts_with('$') {
                        // A `$`-binding is DYNAMIC — it reaches every callee and rendered child,
                        // not just the lexical children — so it is a frame SET scoped to the
                        // `let`'s children: save, set, restore after them (AR.22).
                        let save = self.fresh_ident("sd")?;
                        let _ = writeln!(
                            out,
                            "    let {save} = fx.dollar({bn:?});\n    fx.set_dollar({bn:?}, {v});"
                        );
                        restores = format!("    fx.set_dollar({bn:?}, {save});\n{restores}");
                    } else {
                        let ident = self.fresh_ident(bn)?;
                        let _ = writeln!(out, "    let {ident} = {v};");
                        self.locals.push((bn.to_string(), ident));
                    }
                }
                // The children form their own hoist scope under the `let` bindings.
                let (prelude, epilogue) = self.hoist_prelude(&mi.children, false)?;
                out.push_str(&prelude);
                for k in &mi.children {
                    out.push_str(&self.stmt(k)?);
                }
                out.push_str(&epilogue);
                out.push_str(&restores);
                self.locals.truncate(mark);
                Ok(out)
            }
            // `assert` in statement position: the condition gates the CHILDREN, and a FAILURE
            // DECLINES (AR.14.4.3) — upstream's statement-assert failure is a NON-fatal console
            // error carrying the assert's source text, which only the interpreted re-run can
            // reproduce. The passing path (every real render) stays native.
            "assert" => {
                let Some(cond) = mi.args.first() else {
                    return Err("an `assert` with no condition".into());
                };
                let c = self.expr(&cond.value)?;
                let mut out =
                    format!("    if !({c}).is_truthy() {{ return Err(rt::assert_decline()); }}\n");
                out.push_str(&self.union_part(&mi.children)?);
                Ok(out)
            }
            // Statement `echo` — the console side effect FIRST, then the children (the
            // interpreter's A3 order). Before this arm existed echo fell to the generic dispatch
            // arm below and reached the runtime's unknown-module path: the line was silently
            // DROPPED, a spurious warning appeared, and the children never rendered — a silent
            // wrong answer in the 11 echo-carrying BOSL2 modules the coverage floor counts.
            // AR.20.10 recorded a DECLINE verdict but never wired it; its stated hazard — an echo
            // reading a value assigned below it — died with the hoisting fix, so echo now EMITS.
            "echo" => {
                let mut args = String::new();
                for a in &mi.args {
                    let v = self.expr(&a.value)?;
                    match &a.name {
                        // `$`-named echo args are formatted like any named arg — no special
                        // channel, matching `format_echo_pairs`.
                        Some(n) => {
                            let _ = write!(args, "(Some({:?}), {v}), ", n.as_ref());
                        }
                        None => {
                            let _ = write!(args, "(None, {v}), ");
                        }
                    }
                }
                let mut out = format!("    fx.echo(&[{args}])?;\n");
                out.push_str(&self.union_part(&mi.children)?);
                Ok(out)
            }
            // `intersection_for` INTERSECTS its iterations — a different combinator than the
            // `for` arm's union, and zero BOSL2 modules use it. Decline by name until it earns an
            // emission; falling to the dispatch arm below would reach the runtime's
            // unknown-module path instead (the statement-echo bug class).
            "intersection_for" => Err("an `intersection_for` statement".into()),
            // AR.20.8 — a call to another MODULE, which is 404 of BOSL2's 416 and the arm that
            // makes AR.20.5/AR.20.6's dispatch reachable at all.
            //
            // The arguments go out IN SOURCE ORDER carrying the names they were written with, and
            // nothing is decided here: which slot each one fills is matched at RUNTIME against
            // whatever the callee resolved to. That is deliberate and it is the second design —
            // positionalising at compile time meant assuming the callee's parameter list, which
            // `resolve_module` is free to contradict (AN.10). See `rt::ModuleCall`.
            other => {
                let mut args = String::new();
                for a in &mi.args {
                    let v = self.expr(&a.value)?;
                    match &a.name {
                        // A `$`-arg needs no special channel: it is an argument whose name starts
                        // with `$`, and the runtime splits it out exactly as the interpreter does.
                        Some(n) => {
                            let _ = write!(args, "(Some({:?}), {v}), ", n.as_ref());
                        }
                        None => {
                            let _ = write!(args, "(None, {v}), ");
                        }
                    }
                }
                let children = self.child_block(mi)?;
                Ok(format!(
                    "    parts.push(fx.call(&rt::ModuleCall {{ name: {other:?}, args: &[{args}], children: {children} }})?);\n"
                ))
            }
        }
    }

    /// The `children:` field of an emitted `rt::ModuleCall` — one THUNK per geometry child.
    ///
    /// A thunk rather than built geometry because a callee may instantiate its children zero times
    /// or many, and each instantiation happens fresh. Each opens its own `parts` vec and groups it,
    /// which is what a child block means.
    ///
    /// The thunk parameter is NAMED `fx`, shadowing the enclosing module's ctx, and that is correct
    /// rather than convenient: a thunk is invoked with the CALLER's ctx, and a `children()` written
    /// inside a child block refers to the enclosing module's children anyway. So the ordinary
    /// statement emission works unchanged inside it.
    fn child_block(&mut self, mi: &fab_lang::ModuleInstantiation) -> Result<String, String> {
        // L.5.2 — neither an empty statement nor an assignment is a CHILD (counting either would
        // misalign `$children` and `children(i)`), but an assignment's bindings ARE in scope for
        // every geometry child: the interpreter PREPENDS the assigns to every render. Each thunk
        // mirrors that with its own prelude — a plain name becomes a sequential `let` (source
        // order, a later duplicate shadows = the interpreter's last-write-wins), a `$`-name a
        // SAVE + SET (visible to the thunk's own children renders and dispatches through the
        // frame — `attachable`'s `$parent_*` block is exactly this) with a restore so one
        // thunk's set cannot leak into the next over the shared render ctx. HONEST LIMIT: the
        // interpreter evaluates the assigns once per children()-render of the whole block, a
        // thunk once per CHILD — a side-effecting assign expression (an echo, a rands draw)
        // would fire more often compiled. No library child-block assign has one; the OpenSCAD
        // differential and the examples ratchet police the claim.
        let mut kids = Vec::new();
        let mut assigns: Vec<(&str, &fab_lang::Expr)> = Vec::new();
        for k in &mi.children {
            match &k.kind {
                StmtKind::Empty => {}
                StmtKind::Assignment { name, value } => assigns.push((name, value)),
                _ => kids.push(k),
            }
        }
        if kids.is_empty() {
            return Ok("rt::Children::None".to_string());
        }
        let mark = self.locals.len();
        let mut prelude = String::new();
        let mut epilogue = String::new();
        for (name, value) in assigns {
            let v = self.expr(value)?;
            if name.starts_with('$') {
                let save = self.fresh_ident("sd")?;
                let _ = write!(
                    prelude,
                    "let {save} = fx.dollar({name:?}); fx.set_dollar({name:?}, {v}); "
                );
                epilogue = format!("fx.set_dollar({name:?}, {save}); {epilogue}");
            } else {
                let id = self.fresh_ident(name)?;
                let _ = write!(prelude, "let {id} = {v}; ");
                self.locals.push((name.to_string(), id));
            }
        }
        let mut out = String::from("rt::Children::Compiled(&[");
        for k in kids {
            let body = self.stmt(k)?;
            let _ = write!(
                out,
                "&|fx: &dyn rt::ModuleCtx| {{ let mut parts: Vec<rt::Geo> = Vec::new(); {prelude}{body} {epilogue}Ok(fx.group(parts)) }}, "
            );
        }
        out.push_str("])");
        self.locals.truncate(mark);
        Ok(out)
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
                    let ident = self.fresh_ident(bn)?;
                    let _ = write!(open, "for {ident} in rt::iter_values_warned(fx, &{iter}) {{ ");
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
                let each = self.fresh_ident("each")?;
                Ok(format!(
                    "for {each} in rt::iter_values_warned(fx, &{v}) {{ {acc}.push({each}); }} "
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
                    // The SAME binder threading as `let_expr` (skeptic finding: this arm went
                    // through bare `expr`, so an element-position letrec minted with NO
                    // self_name and no fn-locals mark — a binder colliding with a builtin
                    // recursed into `rt::bi::*` instead of itself, silently).
                    let literal_rhs = matches!(b.value.kind, ExprKind::FunctionLiteral { .. });
                    let val = if literal_rhs {
                        self.function_literal(&b.value, Some(bn))?
                    } else {
                        self.expr(&b.value)?
                    };
                    let ident = self.fresh_ident(bn)?;
                    let _ = write!(out, "let {ident} = {val}; ");
                    if literal_rhs {
                        self.fn_locals.push(ident.clone());
                    }
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

/// A bootstrap subject's baked constants, merged into the batch's fallback-program constant table.
/// A name baking DIFFERENTLY across entries is a hard error, not last-wins — the fallback is ONE
/// island, so a conflict means two entries disagree about the same binding.
fn bake_bootstrap<'a>(
    subject: &'a fab_lang::BootstrapSubject,
    fallback_consts: &mut std::collections::BTreeMap<String, String>,
) -> Result<Vec<(&'a str, Baked)>, String> {
    let name = subject.name;
    let mut baked: Vec<(&str, Baked)> = subject
        .nums
        .iter()
        .map(|&(n, v)| (n, Baked::from_value(fab_lang::Value::Num(v))))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(n, r)| r.map(|b| (n, b)))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|e| format!("{name}: {e}"))?
        .into_iter()
        .collect();
    for (n, xs) in &subject.lists {
        baked.push((
            *n,
            Baked::from_value(fab_lang::Value::num_list(xs.clone()))
                .map_err(|e| format!("{name}: const `{n}` {e}"))?,
        ));
    }
    for (n, b) in &baked {
        let scad = b.scad.clone();
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

/// Describe one subject as a callable SIBLING: its parameters, and each parameter's default already
/// emitted as a Rust expression against that subject's OWN bakes.
///
/// The bakes matter and are the subtle part. A default is evaluated in the callee's lexical base,
/// so `is_vector(v, l, zero, all_nonzero=false, eps=_EPSILON)` resolves `_EPSILON` against
/// `is_vector`'s island — AN.11's exact lesson. Emitting it here with the SUBJECT's bakes preserves
/// that; emitting it with the caller's would be the bug that family fixed.
///
/// # Errors
/// A source that does not parse to a single function definition. A default the emitter cannot
/// produce is NOT an error — it records `None`, and only a call that actually leaves a hole there
/// declines.
fn sibling_of(subject: &Subject<'_>) -> Result<Sibling, String> {
    let a = analyze_function(subject.source)?;
    let prog = fab_lang::parse(subject.source)
        .map_err(|e| format!("{}: does not parse: {e:?}", subject.name))?;
    let Some(fab_lang::StmtKind::FunctionDef { params, .. }) = prog.stmts.first().map(|s| &s.kind)
    else {
        return Err(format!("{}: holds no function definition", subject.name));
    };
    let mut em = Emitter {
        baked: &subject.baked,
        siblings: &[],
        locals: Vec::new(),
        fresh: 0,
        in_module: false,
        registered_defs: Vec::new(),
        def_name: None,
        def_root: None,
        fn_locals: Vec::new(),
    };
    let defaults = params
        .iter()
        .map(|p| p.default.as_ref().and_then(|d| em.expr(d).ok()))
        .collect();
    Ok(Sibling {
        name: a.name,
        params: a.params,
        defaults,
    })
}

/// The generated file for a set of REGISTRY entry names — the bootstrap path, and what the regen
/// gate still drives. Its subjects carry the hand-maintained `consts`/`consts_v` bakes.
///
/// # Errors
/// An unknown entry name, or anything [`generate_batch`] declines.
pub fn generate_module(entry_names: &[&str]) -> Result<String, String> {
    let bootstrap = fab_lang::bootstrap_subjects(entry_names)
        .ok_or_else(|| format!("not all of {entry_names:?} are registry entries"))?;
    let mut sink = std::collections::BTreeMap::new();
    let mut subjects = Vec::with_capacity(bootstrap.len());
    for b in &bootstrap {
        subjects.push(Subject {
            name: b.name,
            source: b.source,
            // `bake_bootstrap` also fills a fallback-const map, which `generate_batch` rebuilds
            // from the subjects; the throwaway `sink` keeps that side effect out of the way.
            baked: bake_bootstrap(b, &mut sink)?,
        });
    }
    // The bootstrap path's LIBRARY is the hand registry, so that is what declares the surface.
    // `bootstrap_all` is the only remaining reader of the hand table from here, and AR.26.3 deletes
    // it along with the path.
    let (all, _pins) = fab_lang::bootstrap_all();
    let declared: Vec<&str> = all.iter().map(|b| b.source).collect();
    generate_batch(&subjects, &declared)
}

/// AR.20.8 — the generated file of MODULE natives, from their verbatim references.
///
/// A SEPARATE file from the function batch rather than a section of it, because the two are
/// genuinely different artifacts: a function batch shares a `FALLBACK_SOURCES` blob and a baked
/// constant table (AR.10/AR.16), while a module native carries neither yet. Merging them would mean
/// one regen path deciding both, and the module half is the one still moving.
///
/// THE POINT OF THIS FUNCTION IS THAT ITS OUTPUT IS COMPILED. Module emission had been exercised
/// only as generated TEXT — asserting that a string contains `fx.call(` proves the emitter ran, not
/// that rustc would accept what it wrote. Checking the file in puts the emitted text through the
/// compiler on every build, and through the tier differential on every test run.
///
/// # Errors
/// If any reference declines, naming the construct — the same contract as the function batch.
/// AR.14.4, the first ARMED band: the STANDALONE modules — every BOSL2 module that emits against
/// EMPTY bake and sibling tables, i.e. needs no baked constants and calls no library functions.
/// Exactly those are armable under the existing fingerprint-only gate: their outward calls are
/// builtin or user MODULES, both resolved at RUNTIME through dispatch (the AN.10-safe design), so
/// there is no guard machinery to get wrong. Returns `(file name, contents)` pairs — one file per
/// BOSL2 SOURCE file (AR.23, so a regen diff stays local to the .scad that moved) plus a `mod.rs`
/// spine carrying the registry table (name, VERBATIM reference for the fingerprint gate, fn) and
/// the bake expectations, generated together so they cannot drift. Deterministic: modules iterate
/// in name order, and the band membership is recomputed each regen — a module that gains a
/// constant read falls out of the band instead of arming unguarded.
pub fn generate_standalone_modules(
    root: &std::path::Path,
) -> Result<Vec<(String, String)>, String> {
    let lib = crate::library::Library::read(root).map_err(|e| format!("library read: {e:?}"))?;
    let folded = lib.fold_constants();
    // Band membership, recomputed every regen. Band 1: emits against EMPTY tables. Band 2: the
    // body's constant reads bake (so the registry row grows a const GUARD). A module that gains a
    // sibling-function call falls out of both rather than arming unguarded, and a band-1 module
    // that gains a constant read moves to band 2 with the guard it now needs.
    let mut band: Vec<(&crate::library::LibMod, Vec<(&str, Baked)>)> = Vec::new();
    for m in lib.modules.values() {
        if generate_module_native(&m.source, &[], &[]).is_ok() {
            band.push((m, Vec::new()));
        } else if let Ok(baked) = bake_reads(&m.source, &lib, &folded)
            && !baked.is_empty()
            && generate_module_native(&m.source, &baked, &[]).is_ok()
        {
            band.push((m, baked));
        }
    }
    // AR.23: group by the library's own `file` provenance — one generated file per source, so a
    // regen diff stays local to the .scad that moved. Ident collisions are LOUD: two source names
    // sanitizing to one Rust module would silently merge their files, and the regen gate's stale
    // sweep would see nothing wrong.
    let mut idents: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    for (m, _) in &band {
        if !idents.contains_key(m.file.as_str()) {
            idents.insert(&m.file, scad_mod_ident(&m.file)?);
        }
    }
    let mut seen: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for (file, ident) in &idents {
        if let Some(prev) = seen.insert(ident.as_str(), file) {
            return Err(format!(
                "sources `{prev}` and `{file}` both want generated module `{ident}`"
            ));
        }
    }
    // ident -> band indices; name-ordered within each file because `band` already is.
    let mut grouped: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, (m, _)) in band.iter().enumerate() {
        grouped
            .entry(idents[m.file.as_str()].as_str())
            .or_default()
            .push(i);
    }

    let mut files: Vec<(String, String)> = Vec::new();
    for (ident, members) in &grouped {
        let scad = &band[members[0]].0.file;
        let mut out = format!(
            "// GENERATED by `fab_lib::emit::generate_standalone_modules` (AR.14.4 bands 1+2) — DO NOT EDIT.\n\
             // Transpiled at BUILD time into fab-bosl2's OUT_DIR — nothing to refresh, and no\n\
             // checked-in copy to go stale. A BOSL2 bump moves the submodule and the next build\n\
             // re-emits this file.\n\
             //\n\
             // Natives for BOSL2 `{scad}` — one generated file per library SOURCE file (AR.23), so\n\
             // a regen diff stays local to the .scad that moved. The registry naming these lives in\n\
             // `mod.rs`.\n\
             //\n\
             {MODULE_FILE_DOC_AND_TAIL}"
        );
        for &i in members {
            let (m, baked) = &band[i];
            out.push('\n');
            out.push_str(&generate_module_native(&m.source, baked, &[])?);
            out.push('\n');
        }
        files.push((format!("{ident}.rs"), out));
    }

    // The SPINE. Registry rows and bake expectations are band-WIDE (a constant baked by two
    // modules must agree), so they stay in one place — a diff here means band membership itself
    // moved, which is exactly the diff worth reading whole.
    let mut out = String::from(
        "// GENERATED by `fab_lib::emit::generate_standalone_modules` (AR.14.4 bands 1+2) — DO NOT EDIT.\n\
         // Transpiled at BUILD time into fab-bosl2's OUT_DIR — nothing to refresh, and no\n\
         // checked-in copy to go stale.\n\
         //\n\
         // The band's SPINE (AR.23): the natives live one generated file per BOSL2 source file;\n\
         // this file declares them and carries what is genuinely band-wide — the bake\n\
         // expectations and the registry.\n\
         #![cfg_attr(rustfmt, rustfmt::skip)]\n\
         #![allow(\n\
         \x20   non_snake_case,\n\
         \x20   clippy::unreadable_literal,\n\
         \x20   clippy::used_underscore_items,\n\
         \x20   reason = \"generated code: module and __bake_* names carried verbatim from the \\\n\
         \x20             library, and bake expectations pin bit-exact from_bits literals\"\n\
         )]\n\
         \n",
    );
    for ident in grouped.keys() {
        let _ = writeln!(out, "mod {ident};");
    }
    // The bake EXPECTATIONS (band 2): one fn per baked constant, deduped across the band with
    // conflicts LOUD — a name baked to two different values would make the guard ambiguous, the
    // same cross-batch rule `generate_batch` enforces on the function side.
    let mut expectations: std::collections::BTreeMap<&str, &Baked> =
        std::collections::BTreeMap::new();
    for (_, baked) in &band {
        for (n, b) in baked {
            if lib.modules.contains_key(&format!("__bake_{n}")) {
                return Err(format!("module `__bake_{n}` collides with a bake builder"));
            }
            if let Some(prev) = expectations.insert(n, b)
                && prev != b
            {
                return Err(format!(
                    "const `{n}` baked differently across the module band"
                ));
            }
        }
    }
    if !expectations.is_empty() {
        out.push_str(
            "\n// AR.13: `rt` is the ONLY thing generated code names.\n\
             use fab_lang::rt;\n\
             \n/// Bake EXPECTATIONS (band 2) — what each guarded registry row hands the resolve-time\n\
             /// const guard. The guard compares the program's own top-level binding against this value\n\
             /// BIT-exactly before the native may wire; a rebound constant interprets instead.\n",
        );
        for (n, b) in &expectations {
            let _ = write!(
                out,
                "fn __bake_{n}() -> rt::Value {{\n    {}\n}}\n",
                b.emit()?
            );
        }
    }
    let _ = write!(
        out,
        "\n/// The band's registry — chained into `module_table` after the POC entries. Verbatim\n\
         /// references are the fingerprint gate's ground truth: a program whose definition drifts\n\
         /// from the pinned library interprets instead of wiring. A non-empty `consts` row is the\n\
         /// band-2 const guard (see the bake expectations above).\n\
         pub(super) static REGISTRY: &[super::ModuleEntry] = &[\n"
    );
    for (m, baked) in &band {
        let consts = if baked.is_empty() {
            "&[]".to_string()
        } else {
            format!(
                "&[{}]",
                baked
                    .iter()
                    .map(|(n, _)| format!("({n:?}, __bake_{n})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let _ = write!(
            out,
            "    super::ModuleEntry {{\n        name: {:?},\n        reference: {:?},\n        func: {}::{},\n        consts: {},\n    }},\n",
            m.name,
            m.source,
            idents[m.file.as_str()],
            rust_fn_ident(&m.name)?,
            consts
        );
    }
    out.push_str("];\n");
    files.push(("mod.rs".to_string(), out));
    Ok(files)
}

/// One module the emitter has been asked to compile — the module twin of [`Subject`]. No sibling
/// FUNCTION table (AR.14.4.3): a module's outward calls — modules AND functions — resolve at
/// runtime through dispatch.
struct ModuleSubject<'a> {
    source: &'a str,
    baked: Vec<(&'a str, Baked)>,
}

/// The generated fn ident for a library name: raw-escaped when the name is a Rust KEYWORD (BOSL2
/// has `module move()` and `function typeof()`), declined for the few names a raw identifier
/// cannot spell. The fingerprint gate keys on the scad NAME, so the Rust ident is free to differ.
///
/// BOTH keyword classes, and the second one is not pedantry: RESERVED words are not usable as
/// identifiers either, and BOSL2 declares `typeof`. It emitted bare in three positions — the fn
/// definition, the registry row's `func`, and every sibling call site — and none of them parse.
/// Found by compiling the whole band; no substring assertion could have.
///
/// # Errors
/// `crate`/`self`/`super`/`Self`, which `r#` cannot escape.
fn rust_fn_ident(name: &str) -> Result<String, String> {
    match name {
        "crate" | "self" | "super" | "Self" => {
            Err(format!("name `{name}` cannot be a Rust identifier"))
        }
        // Strict keywords.
        "as" | "async" | "await" | "break" | "const" | "continue" | "dyn" | "else" | "enum"
        | "extern" | "false" | "fn" | "for" | "gen" | "if" | "impl" | "in" | "let" | "loop"
        | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "static" | "struct"
        | "trait" | "true" | "try" | "type" | "unsafe" | "use" | "where" | "while"
        // Reserved for future use — equally unusable as a bare identifier.
        | "abstract" | "become" | "box" | "do" | "final" | "macro" | "override" | "priv"
        | "typeof" | "unsized" | "virtual" | "yield" => Ok(format!("r#{name}")),
        _ => Ok(name.to_string()),
    }
}

/// The Rust module ident for a library SOURCE file (AR.23): the stem, non-ident chars mapped to
/// `_`, keyword-escaped by [`rust_fn_ident`]. BOSL2's names all pass through untouched; the
/// mapping exists so an upstream rename cannot produce an unbuildable regen.
///
/// # Errors
/// One of the four names `r#` cannot escape.
fn scad_mod_ident(file: &str) -> Result<String, String> {
    let stem = file.strip_suffix(".scad").unwrap_or(file);
    let mut s: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if s.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    rust_fn_ident(&s)
}

pub fn generate_modules(references: &[&str]) -> Result<String, String> {
    let mut subjects = Vec::with_capacity(references.len());
    for r in references {
        subjects.push(ModuleSubject {
            source: r,
            baked: poc_module_bakes(r)?,
        });
    }
    generate_module_file(&subjects)
}

/// The doc-and-tail every generated MODULE file shares: what a module native IS, the rustfmt
/// escape, the lint carve-outs a mechanical transcription legitimately needs, and the AR.13
/// `rt` import. One literal so the POC file and the per-source band files (AR.23) cannot drift.
const MODULE_FILE_DOC_AND_TAIL: &str = "// A module native builds GEOMETRY through the interpreter's own construction (`ModuleCtx`),\n\
     // so what it renders is what interpreting its reference renders — the win is the deleted\n\
     // interpretation overhead, not different geometry.\n\
     \n\
     // rustfmt CANNOT format this file: the emitter's one-line expression bodies blow up its\n\
     // layout search (measured: 5 CPU-minutes without terminating at 402 modules), and the\n\
     // bytes are regenerated verbatim anyway — reformatting would only fail the currency gate.\n\
     #![cfg_attr(rustfmt, rustfmt::skip)]\n\
     #![allow(\n\
     \x20   unused_variables,\n\
     \x20   unused_mut,\n\
     \x20   non_snake_case,\n\
     \x20   clippy::get_first,\n\
     \x20   clippy::vec_init_then_push,\n\
     \x20   clippy::unreadable_literal,\n\
     \x20   clippy::cloned_ref_to_slice_refs,\n\
     \x20   clippy::used_underscore_items,\n\
     \x20   clippy::possible_missing_else,\n\
     \x20   clippy::collapsible_else_if,\n\
     \x20   clippy::similar_names,\n\
     \x20   clippy::needless_else,\n\
     \x20   clippy::if_same_then_else,\n\
     \x20   clippy::too_many_lines,\n\
     \x20   clippy::collapsible_if,\n\
     \x20   reason = \"generated code: a module need not READ every parameter it declares, \\\n\
     \x20             parameter slots are indexed uniformly (so slot 0 is `get(0)`, not \\\n\
     \x20             `first()`), and a parts vec grows CONDITIONALLY even when the first \\\n\
     \x20             push happens to be unconditional — plus bit-exact from_bits literals, \\\n\
     \x20             mechanical clones, upstream's underscore-prefixed and camelCase names \\\n\
     \x20             carried verbatim, fresh idents that differ by counter (`l1_r`/`l2_r`), \\\n\
     \x20             upstream's own empty/identical branches, and bodies as long as the \\\n\
     \x20             module they transcribe\"\n\
     )]\n\
     \n\
     // AR.13: `rt` is the ONLY thing generated code names.\n\
     use fab_lang::rt;\n";

/// The whole generated MODULE file for a batch of subjects — header + one native per subject.
/// The POC path ([`generate_modules`]); the BOSL2 band assembles its own per-source files from
/// the same doc-and-tail ([`generate_standalone_modules`], AR.23).
fn generate_module_file(subjects: &[ModuleSubject]) -> Result<String, String> {
    let mut out = format!(
        "// GENERATED by `fab_lib::emit::generate_modules` (AR.20.8) — DO NOT EDIT.\n\
         // Refresh: FAB_REGEN=1 cargo nextest run -p fab-lib -E 'test(generated_modules_are_current)'\n\
         //\n\
         {MODULE_FILE_DOC_AND_TAIL}"
    );
    for subject in subjects {
        out.push('\n');
        out.push_str(&generate_module_native(
            subject.source,
            &subject.baked,
            &[],
        )?);
        out.push('\n');
    }
    Ok(out)
}

/// AR.12.2 — the generated file for named functions read out of a LIBRARY, with no hand-maintained
/// anything. This is what a transpiled crate is actually built from, and the reason the emitter
/// stopped reading `intrinsics::REGISTRY`: a transpiler that depends on the table it exists to
/// delete cannot be moved out of fab-lang, and cannot describe a library it has no entry for.
///
/// Bakes are EMPTY for now, which bounds this to functions that read no library constant — the 742
/// of BOSL2's 1335 the coverage ratchet already measures, since it probes with empty bakes too.
/// AR.16 fills them from the library's own top level and the number moves.
///
/// # Errors
/// A name the library does not unambiguously declare (a collision counts as not declared), or
/// anything [`generate_batch`] declines.
pub fn generate_from_library(
    lib: &crate::library::Library,
    names: &[&str],
) -> Result<String, String> {
    let folded = lib.fold_constants();
    let mut subjects = Vec::with_capacity(names.len());
    for &name in names {
        let f = lib
            .functions
            .get(name)
            .ok_or_else(|| format!("`{name}` is not an unambiguous function in this library"))?;
        subjects.push(Subject {
            name: &f.name,
            source: &f.source,
            baked: bake_reads(&f.source, lib, &folded)?,
        });
    }
    // The whole library DECLARES; only `names` compile. Two lists, two lengths, on purpose.
    let declared: Vec<&str> = lib.functions.values().map(|f| f.source.as_str()).collect();
    generate_batch(&subjects, &declared)
}

/// How many `#` a raw string literal needs to hold `content` — one more than the longest run of
/// `#` that follows a `"` inside it, and at least one.
///
/// `r#"…"#` cannot contain `"#`; `r##"…"##` cannot contain `"##`. Scanning for the worst case is
/// three lines and removes a whole class of "this library happens to contain the wrong bytes"
/// failure, which at library scale is not hypothetical: BOSL2 has exactly one such function and it
/// took the other 865 down with it.
fn raw_string_hashes(content: &str) -> usize {
    let mut worst = 0;
    let bytes = content.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'"' {
            continue;
        }
        let run = bytes[i + 1..].iter().take_while(|&&c| c == b'#').count();
        worst = worst.max(run);
    }
    worst + 1
}

/// Does this expression OBSERVABLY do something when evaluated, beyond producing a value?
///
/// `echo` and `assert` only — the two constructs whose evaluation a user can SEE (a console line, a
/// raised error). Used where the compiled tier would evaluate an argument the interpreter might
/// drop; a pure argument evaluated needlessly costs nothing anybody can detect.
fn has_side_effect(e: &Expr) -> bool {
    if matches!(e.kind, ExprKind::Echo { .. } | ExprKind::Assert { .. }) {
        return true;
    }
    // Explicit work stack rather than recursion: house doctrine, and an argument expression is
    // user-supplied depth.
    let mut stack = vec![e];
    while let Some(n) = stack.pop() {
        if matches!(n.kind, ExprKind::Echo { .. } | ExprKind::Assert { .. }) {
            return true;
        }
        stack.extend(fab_lang::expr_children(n));
    }
    false
}

/// A batch's guard lists: name → (transitive deps, transitive builtins). See [`guard_closures`].
type GuardClosures<'a> = std::collections::BTreeMap<&'a str, (Vec<String>, Vec<String>)>;

/// The TRANSITIVE dep and builtin closures over an emitted batch (AR.26.2) — what each row's
/// `deps`/`builtins` guard lists have to carry.
///
/// TRANSITIVE, not one hop, and that is forced rather than chosen. A generated native calls its
/// siblings with STATIC Rust calls, so it INLINES them: it bakes the callee's semantics, the
/// callee's callee's semantics, and so on down. `guard_veto` checks each listed dep once and does
/// not recurse, so anything the list omits is a piece of baked semantics nobody proved. The
/// hand-written lists say the same thing in their doc — "TRANSITIVELY CLOSED by the author" — and
/// this is that sentence made mechanical.
///
/// TWO CONSEQUENCES WORTH KNOWING. It is STRICTER than the hand lists, never weaker: an author
/// could prune a branch no accepted argument shape reaches, and the closure cannot, so a generated
/// row guards on deps a hand row would have dropped. More guards means more declines, which is the
/// safe direction. And it closes the documented ONE HOP hole in the const guard for free —
/// `needs_post_hoist` and `dep_const_veto` walk `entry.deps` exactly one level, so a list that is
/// already the full closure makes that one level the whole graph.
///
/// Names outside the batch are excluded: by construction a live subject only calls live siblings
/// (that is what the [`function_band`] fixpoint establishes), so a name that is not a subject is a
/// BUILTIN or an emitter-internal call, neither of which belongs in `deps`.
fn guard_closures<'a>(subjects: &'a [Subject<'a>]) -> Result<GuardClosures<'a>, String> {
    use std::collections::{BTreeMap, BTreeSet};
    let in_batch: BTreeSet<&str> = subjects.iter().map(|s| s.name).collect();
    let mut one_hop: BTreeMap<&str, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for s in subjects {
        let a = analyze_function(s.source)?;
        let deps = a
            .deps
            .into_iter()
            .filter(|d| in_batch.contains(d.as_str()))
            .collect();
        one_hop.insert(s.name, (deps, a.builtins));
    }
    let mut deps: BTreeMap<&str, BTreeSet<String>> =
        one_hop.iter().map(|(k, (d, _))| (*k, d.clone())).collect();
    let mut builtins: BTreeMap<&str, BTreeSet<String>> =
        one_hop.iter().map(|(k, (_, b))| (*k, b.clone())).collect();
    // Fixpoint rather than a topological walk: the dep graph has real CYCLES (approx <-> idx <->
    // posmod is a 3-cycle in BOSL2), so no dependency order exists to resolve in. Bounded by the
    // batch size, which is the longest a chain can be.
    for _ in 0..=subjects.len() {
        let (dsnap, bsnap) = (deps.clone(), builtins.clone());
        let mut changed = false;
        for (name, (own, _)) in &one_hop {
            let mut d = dsnap[name].clone();
            let mut b = bsnap[name].clone();
            for dep in own {
                if let Some(x) = dsnap.get(dep.as_str()) {
                    d.extend(x.iter().cloned());
                }
                if let Some(x) = bsnap.get(dep.as_str()) {
                    b.extend(x.iter().cloned());
                }
            }
            if d != dsnap[name] {
                deps.insert(name, d);
                changed = true;
            }
            if b != bsnap[name] {
                builtins.insert(name, b);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(subjects
        .iter()
        .map(|s| {
            (
                s.name,
                (
                    // A row never pins ITSELF: `guard_veto` would compare the running function
                    // against its own anchor, which is the fingerprint check `resolve` already
                    // made. Harmless but noise, and it doubles a self-recursive row's list.
                    deps[s.name]
                        .iter()
                        .filter(|d| d.as_str() != s.name)
                        .cloned()
                        .collect(),
                    builtins[s.name].iter().cloned().collect(),
                ),
            )
        })
        .collect())
}

/// A library's functions that actually COMPILE TOGETHER, and why the rest do not.
///
/// The unit `generate_batch` should be handed at library scale. See [`function_band`].
pub struct FunctionBand<'a> {
    /// The survivors, name-ordered — every one emits against a sibling table containing only
    /// other survivors.
    pub subjects: Vec<Subject<'a>>,
    /// `(name, reason)` for every function that does not, name-ordered. The histogram's raw
    /// material, and the reason a coverage number can be a GATE rather than a vanity metric.
    pub declined: Vec<(&'a str, String)>,
    /// How many rounds the fixpoint took. Diagnostic: a jump means the emitted call graph got
    /// deeper, which is the shape of a regression that does not otherwise announce itself.
    pub rounds: usize,
}

/// THE FIXPOINT (AR.26.2): probe every function, DROP the decliners, REBUILD the sibling table from
/// the survivors only, repeat until nothing more falls out.
///
/// WHY IT CANNOT BE ONE PASS, which is the whole finding. A generated function calls a sibling with
/// a STATIC Rust call — a bare ident naming another generated fn. Probe a function against a sibling
/// table built from the WHOLE library and it happily emits a call to a function that itself declined
/// and therefore does not exist in the output. That is not a coverage overcount, it is a file that
/// does not compile; and because a decliner takes its callers with it, the loss cascades.
///
/// MEASURED on the pinned BOSL2: the single-pass probe reported 1197 of 1329 and the fixpoint
/// converged at 866 after 9 rounds — 463 dropped, of which only ~88 declined for their own reasons
/// and the rest was CASCADE. AR.27 then pulled the lever that cascade pointed at (a call to a
/// non-emitting sibling DISPATCHES rather than declining, the way a module call always has), and
/// the fixpoint now converges at 1260 in 2 rounds. The loop still matters and always will: whatever
/// the emitter cannot compile still takes its static callers with it.
///
/// Monotone: the live set only ever shrinks, so it terminates in at most one round per function.
///
/// # Errors
/// Only a bake failure that is not a decline — a library whose constants cannot be folded at all.
pub fn function_band<'a>(
    lib: &'a crate::library::Library,
) -> Result<FunctionBand<'a>, String> {
    let folded = lib.fold_constants();
    let fns: Vec<&'a crate::library::LibFn> = lib.functions.values().collect();
    // Bakes are a property of the FUNCTION and its library, never of the live set, so they are
    // computed once rather than per round. A bake failure is a decline like any other.
    let baked: Vec<Vec<(&'a str, Baked)>> = fns
        .iter()
        .map(|f| bake_reads(&f.source, lib, &folded).unwrap_or_default())
        .collect();

    let mut live: Vec<bool> = vec![true; fns.len()];
    let mut rounds = 0_usize;
    let mut reasons: Vec<Option<String>> = vec![None; fns.len()];
    loop {
        rounds += 1;
        // The sibling table IS the live set — a function that cannot even produce a `Sibling`
        // (its signature does not analyze) is not live, so the two can never disagree about who
        // exists.
        let mut siblings: Vec<Sibling> = Vec::with_capacity(fns.len());
        for (i, f) in fns.iter().enumerate() {
            if !live[i] {
                continue;
            }
            match sibling_of(&Subject {
                name: &f.name,
                source: &f.source,
                baked: baked[i].clone(),
            }) {
                Ok(sib) => siblings.push(sib),
                Err(e) => {
                    live[i] = false;
                    reasons[i] = Some(e);
                }
            }
        }
        let mut next = live.clone();
        for (i, f) in fns.iter().enumerate() {
            if !live[i] {
                continue;
            }
            if let Err(e) = generate_native(&f.source, &baked[i], &siblings) {
                next[i] = false;
                reasons[i] = Some(e);
            }
        }
        if next == live {
            break;
        }
        live = next;
    }

    let mut subjects = Vec::new();
    let mut declined = Vec::new();
    for (i, f) in fns.iter().enumerate() {
        if live[i] {
            subjects.push(Subject {
                name: &f.name,
                source: &f.source,
                baked: baked[i].clone(),
            });
        } else {
            declined.push((
                f.name.as_str(),
                reasons[i].clone().unwrap_or_else(|| "declined".to_string()),
            ));
        }
    }
    Ok(FunctionBand {
        subjects,
        declined,
        rounds,
    })
}

/// AR.16 — the constants THIS function reads, folded and paired with the library's own source.
///
/// Derived from the same analysis walk the guard sets come from, so a native bakes exactly what its
/// body reaches and nothing else. A name the fold could not resolve, or one the library does not
/// declare at top level, is simply left out — the emitter then declines that function on the free
/// read, which is the outcome it had before and never a wrong one.
fn bake_reads<'a>(
    source: &str,
    lib: &'a crate::library::Library,
    folded: &std::collections::BTreeMap<String, fab_lang::Value>,
) -> Result<Vec<(&'a str, Baked)>, String> {
    // A reference is a function OR a module, and the analyzers differ only in the body's shape.
    // Trying both here rather than at every call site is what keeps a module's bakes from being
    // SILENTLY EMPTY: the module coverage census did exactly that through an `unwrap_or_default`,
    // and reported the library's own `UP`/`CENTER`/`PI` as unbakeable free reads in ~90 modules.
    let analysis = match analyze_function(source) {
        Ok(a) => a,
        Err(fn_err) => analyze_module(source)
            .map_err(|mod_err| format!("neither a function ({fn_err}) nor a module ({mod_err})"))?,
    };
    let mut out = Vec::new();
    for read in &analysis.consts {
        // `PI` is the LANGUAGE's seeded constant, not the library's — `Scope::new` binds
        // `std::f64::consts::PI` at the root, so it is never in `lib.constants` and the fold
        // cannot see it. Baked with the seed's exact bits; the module const guard still compares
        // against the real chain at resolve time, so a program shadowing `PI` vetoes the native.
        if read == "PI" {
            out.push((
                "PI",
                Baked::from_value(fab_lang::Value::Num(std::f64::consts::PI))?,
            ));
            continue;
        }
        let (Some(value), Some(decl)) = (folded.get(read), lib.constants.get(read)) else {
            continue;
        };
        // The bake VALUE comes from the fold; the island's binding comes from the library's OWN
        // source. See `Baked` for why those are deliberately not the same rendering.
        out.push((
            decl.name.as_str(),
            Baked::from_source(value.clone(), &decl.source),
        ));
    }
    Ok(out)
}

/// One function the emitter has been asked to compile: what it is called, its VERBATIM source, and
/// the constants it bakes.
///
/// AR.12.2 — the seam that decouples the emitter from `intrinsics::REGISTRY`. The transpiler used to
/// read the hand registry directly, which made it depend on the very thing it exists to delete, and
/// made moving it into its own crate impossible. A subject can come from a registry entry or
/// straight out of a [`crate::library::Library`]; the emitter cannot tell and must not care.
pub struct Subject<'a> {
    pub name: &'a str,
    pub source: &'a str,
    pub baked: Vec<(&'a str, Baked)>,
}

/// The whole generated FILE for a batch of subjects, header + imports included.
/// Deterministic: same inputs → same bytes (the regen test's contract).
///
/// `declared` is the library's DECLARATION — the verbatim reference of every function it hosts,
/// which the `SURFACE` table is derived from. Deliberately a separate list from `subjects` and
/// deliberately a different LENGTH: a library declares more than it compiles (BOSL2 declares 1329
/// and the emitter compiles 866), and `LibrarySurface` splits those two questions for exactly that
/// reason. Passing the subjects here instead would quietly assert they are the same set, which is
/// the drift this phase exists to kill.
///
/// # Errors
/// A subject whose source does not parse to a single function definition, or that uses a construct
/// outside the emitter's subset — declines LOUDLY with the construct named, because a partial native
/// would be a wrong native.
pub fn generate_batch(subjects: &[Subject<'_>], declared: &[&str]) -> Result<String, String> {
    // The sibling table FIRST, whole batch, self included: mutual recursion (approx ↔ idx ↔
    // posmod is a real 3-cycle) forward-references freely in Rust, and named sibling arguments
    // bind against these declared param lists at compile time.
    let mut siblings: Vec<Sibling> = Vec::new();
    for subject in subjects {
        siblings.push(sibling_of(subject)?);
    }
    // The AR.10 fallback program: every baked constant (deduped, conflicts LOUD) followed by the
    // verbatim references — one interpretable island whose bindings equal the bakes.
    let mut fallback_consts: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut fallback_refs = String::new();

    let mut fns = String::new();
    for subject in subjects {
        for (n, b) in &subject.baked {
            let scad = b.scad.clone();
            if let Some(prev) = fallback_consts.insert((*n).to_string(), scad.clone())
                && prev != scad
            {
                return Err(format!(
                    "const `{n}` baked differently across the batch ({prev} vs {scad})"
                ));
            }
        }
        fallback_refs.push_str(subject.source);
        fallback_refs.push('\n');

        fns.push_str(&generate_native(subject.source, &subject.baked, &siblings)?);
        fns.push('\n');
    }
    let mut header = String::from(
        "// GENERATED by `eval::transpile::generate_module` (AR.6) — DO NOT EDIT.\n\
         // Refresh: FAB_REGEN=1 cargo nextest run -p fab-lang -E 'test(generated_file_is_current)'\n\
         //\n\
         // Every operation routes through the interpreter's own value algebra, so a generated\n\
         // native is bit-identical to interpreting its reference BY CONSTRUCTION — the win is the\n\
         // deleted interpretation overhead, not different math.\n\n\
         // rustfmt CANNOT format this file: the emitter's one-line expression bodies blow up its\n\
         // layout search (measured: 5 CPU-minutes without terminating at 402 modules), and the\n\
         // bytes are regenerated verbatim anyway — reformatting would only fail the currency gate.\n\
         #![cfg_attr(rustfmt, rustfmt::skip)]\n\
         #![allow(\n\
         \x20   unused_variables,\n\
         \x20   non_snake_case,\n\
         \x20   clippy::unreadable_literal,\n\
         \x20   clippy::cloned_ref_to_slice_refs,\n\
         \x20   clippy::used_underscore_items,\n\
         \x20   clippy::possible_missing_else,\n\
         \x20   clippy::collapsible_else_if,\n\
         \x20   clippy::similar_names,\n\
         \x20   clippy::needless_else,\n\
         \x20   clippy::if_same_then_else,\n\
         \x20   clippy::too_many_lines,\n\
         \x20   clippy::collapsible_if,\n\
         \x20   clippy::vec_init_then_push,\n\
         \x20   reason = \"generated code: bit-exact from_bits literals, mechanical clones, \\\n\
         \x20             upstream's underscore-prefixed names (params too — `p__total`), unused \\\n\
         \x20             loop binders a body never reads, fresh idents differing by counter, and \\\n\
         \x20             one-line block emission are the emitter's idiom\"\n\
         )]\n\n\
         // AR.13: `rt` is the ONLY thing generated code names. `extern crate self as fab_lang`\n\
         // makes this path resolve inside fab-lang too, so moving this file into its own crate\n\
         // cannot change a byte of what follows.\n\
         use fab_lang::rt;\n\n",
    );
    // The AR.10 fallback program: baked constants then verbatim references, one island whose
    // bindings equal the bakes — what a declining native interprets instead of recursing.
    //
    // The raw-string HASH LEVEL is computed from the content, not fixed at one. BOSL2's
    // `phillips_depth` carries a `"#` in a string literal, and at library scale that one reference
    // aborted the entire batch — which is the wrong answer twice over: the function is perfectly
    // compilable, and refusing 866 functions over one delimiter is not a decline, it is an outage.
    let mut island = String::new();
    for (n, scad) in &fallback_consts {
        let _ = writeln!(island, "{n} = {scad};");
    }
    island.push_str(&fallback_refs);
    let hashes = "#".repeat(raw_string_hashes(&island));
    let _ = write!(
        header,
        "/// Every baked constant + the batch's verbatim references as ONE interpretable program —\n\
         /// the AR.10 depth fallback's target (see `native_rt`). Constants print via Rust's\n\
         /// roundtrip-exact float formatting, so the interpreted bindings equal the bakes bit-for-bit.\n\
         pub(super) const FALLBACK_SOURCES: &str = r{hashes}\"\n{island}\"{hashes};\n\n"
    );

    // AR.14.5 — the library's declared SURFACE as static data: the same derivation
    // `native_surface()` ran at every process start (parse each reference, walk its body for
    // type evidence, widest-wins per parameter), run ONCE here instead. The fuzzer's decl table
    // stops being derived-then-leaked and becomes bytes the regen gate pins; the equivalence
    // test in fab-lang keeps the runtime derivation as the oracle so the two cannot drift.
    // Sorted by name — order is part of the seed-stability contract, because the generator
    // INDEXES this table with the RNG and a reordering silently re-points every accumulated corpus.
    //
    // AR.26.2: from `declared`, NOT from `fab_lang::bootstrap_all()`. Reading the hand registry
    // here meant a library-sourced batch carried the HAND registry's surface no matter which
    // library it had actually been given — a generated crate describing somebody else's functions.
    let mut surface: Vec<(String, String)> = declared
        .iter()
        .filter_map(|src| fab_lang::SurfaceFn::from_reference(src))
        .map(|f| {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| {
                    format!(
                        "rt::Param {{ name: {:?}, domain: rt::Domain::{:?}, required: {} }}",
                        p.name,
                        fab_lang::widest_domain(&p.domains),
                        p.required
                    )
                })
                .collect();
            let row = format!(
                "    rt::Decl {{ name: {:?}, kind: rt::Kind::Function, ret: rt::Domain::Num, names_bind: true, params: &[{}] }},",
                f.name,
                params.join(", ")
            );
            (f.name, row)
        })
        .collect();
    surface.sort_by(|a, b| a.0.cmp(&b.0));
    // A library may declare a name twice (BOSL2 does); the surface is a SET, first-wins, matching
    // the registry's own collision rule. Without this a duplicate would emit two `Decl` rows and
    // the fuzzer would draw the same function under two indices.
    surface.dedup_by(|a, b| a.0 == b.0);
    let mut table = String::from(
        "/// The registry's declared surface (AR.14.5) — what `native_surface()` derived at every\n\
         /// process start, as STATIC data. `names_bind: true` throughout: these stand in for BOSL2\n\
         /// user functions, where parameter names really do bind.\n\
         pub(super) static SURFACE: &[rt::Decl] = &[\n",
    );
    for (_, row) in &surface {
        table.push_str(row);
        table.push('\n');
    }
    table.push_str("];\n\n");

    // AR.26.2 — the REGISTRY ROWS, emitted beside the natives they name. Until now the function
    // side's rows were hand-written in fab-lang while the module band generated its own, which put
    // the two halves of one contract in two places maintained by different means. A generated
    // library now hands over its own dispatch table, which is the only shape that works once the
    // library lives in a crate fab-lang cannot see.
    let closures = guard_closures(subjects)?;
    // Bake EXPECTATIONS, deduped batch-wide with conflicts LOUD — the same cross-batch rule the
    // fallback island enforces above, one level down. ALL bakes go through `consts_v` rather than
    // splitting numbers into `consts`: the two fields differ only in how the arm step spells the
    // comparison (`to_bits` vs `value_bits_eq`, which does `to_bits` on a `Num`), and one
    // mechanism is one thing to get right.
    let mut expectations: std::collections::BTreeMap<&str, &Baked> =
        std::collections::BTreeMap::new();
    for subject in subjects {
        for (n, b) in &subject.baked {
            if let Some(prev) = expectations.insert(n, b)
                && prev.value != b.value
            {
                return Err(format!("const `{n}` baked differently across the batch"));
            }
        }
    }
    let mut rows = String::new();
    if !expectations.is_empty() {
        rows.push_str(
            "/// Bake EXPECTATIONS — what each guarded registry row hands the arm-time const guard.\n\
             /// The guard compares the program's own top-level binding against this value BIT-exactly\n\
             /// before the native may wire, because the fingerprint proves the FUNCTION and never the\n\
             /// constants it names; a rebound constant interprets instead.\n",
        );
        for (n, b) in &expectations {
            let _ = write!(rows, "fn __bake_{n}() -> rt::Value {{\n    {}\n}}\n", b.emit()?);
        }
        rows.push('\n');
    }
    rows.push_str(
        "/// This batch's registry rows. The VERBATIM reference is the fingerprint gate's ground\n\
         /// truth — a program whose definition drifted from the pinned library interprets instead of\n\
         /// wiring — and `deps`/`builtins` are TRANSITIVE closures over the batch, because a static\n\
         /// sibling call inlines the callee's semantics and the callee's own gate is never consulted\n\
         /// at dispatch.\n\
         ///\n\
         /// WHETHER THESE ARE LIVE DEPENDS ON THE CRATE. A generated LIBRARY crate serves them\n\
         /// from its `LibrarySurface::rows`, and they are its dispatch table. fab-lang's own\n\
         /// checked-in copy is NOT wired: these rows and its hand table name the same functions,\n\
         /// and one name cannot have two owning libraries, so the swap waits for AR.21 to delete\n\
         /// the hand table. There they are held to it by\n\
         /// `the_emitted_rows_agree_with_the_hand_registry`, which is what makes that swap safe.\n\
         /// The emitter cannot tell which consumer it is writing for, hence the condition.\n\
         #[allow(dead_code, reason = \"live in a library crate; awaiting AR.21 in fab-lang\")]\n\
         pub(super) static REGISTRY: &[rt::Entry] = &[\n",
    );
    for subject in subjects {
        let (deps, builtins) = &closures[subject.name];
        let consts_v = if subject.baked.is_empty() {
            "&[]".to_string()
        } else {
            format!(
                "&[{}]",
                subject
                    .baked
                    .iter()
                    .map(|(n, _)| format!("({n:?}, __bake_{n})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let list = |xs: &[String]| {
            if xs.is_empty() {
                "&[]".to_string()
            } else {
                format!(
                    "&[{}]",
                    xs.iter()
                        .map(|x| format!("{x:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        };
        let _ = write!(
            rows,
            "    rt::Entry {{\n        name: {:?},\n        reference: {:?},\n        consts: &[],\n        consts_v: {},\n        deps: {},\n        builtins: {},\n        func: {},\n    }},\n",
            subject.name,
            subject.source,
            consts_v,
            list(deps),
            list(builtins),
            rust_fn_ident(subject.name)?,
        );
    }
    rows.push_str("];\n\n");

    Ok(header + &table + &rows + &fns)
}

/// The entries the generated module currently covers, in DEPENDENCY order (a sibling call may
/// only reach entries earlier in this list). Growing this list is how an intrinsic migrates from
/// hand-written to generated (AR.7).
/// AR.20.8 — the MODULE references whose natives are generated into
/// `lang/src/eval/intrinsics/generated_modules.rs` and wired into `MODULE_REGISTRY`.
///
/// Verbatim source, not names, because a module native's fingerprint gate checks THESE bytes: the
/// registry entry's `reference` and the generated body must come from one string or they can
/// describe different code.
pub const GENERATED_MODULES: &[&str] = &[
    // The ABI POCs, now GENERATED rather than hand-written — which is the AR.20.8 deliverable.
    // Between them they exercise every part of the module ABI: a bound parameter and `children()`,
    // a call to another user module forwarding its children, and a call to BUILTINS (a combinator
    // plus a primitive reached with named arguments).
    "module _fab_poc_mod(k=1) { children(); }",
    "module _fab_poc_wrap(k=1) { _fab_poc_mod(k) children(); }",
    "module _fab_poc_prim(s=1) { translate([s,0,0]) cube(size=s, center=true); }",
    // AR.20.3 — a `$`-read, answered off the inherited dynamic chain rather than baked. Reads
    // `$children` (which the evaluator binds into every call frame) so the differential covers the
    // `fx.dollar` path with a value that DEPENDS ON THE CALL SITE, which is the whole point: a
    // baked one would freeze whatever it was at transpile time.
    "module _fab_poc_dollar(k=1) { if ($children > 1) children(1); else children(); }",
    // The `% *` modifiers, compiled. `%` must RUN its subtree and drop only the geometry; `*` must
    // not run it at all — and, crucially, a `*`-disabled statement is still a CHILD for `$children`
    // and `children(i)` counting, so dropping it would shift every index in the callee.
    "module _fab_poc_bg(s=1) { %cube(s); sphere(r=s); }",
    "module _fab_poc_star(s=1) { _fab_poc_dollar(s) { *cube(s); sphere(r=s); cylinder(r=s,h=s); } }",
    // Whole-scope HOISTING, compiled: `cube(x)` must see the assignment BELOW it (last-wins), the
    // self-reference `x + 2` must read the OUTER x (the parameter), and `y` — assigned inside a
    // `{ }` — must reach the `sphere` OUTSIDE it, because a block is not an assignment scope
    // upstream. Statement-position emission rendered all three wrong without declining.
    "module _fab_poc_hoist(x=1) { cube(x); x = x + 2; { y = x; } sphere(r=y); }",
    // Statement ECHO, compiled (AR.20.4's last construct): a named arg renders `k = 2`, the
    // side effect lands BEFORE geometry, echo children render as ONE implicit-union part, and a
    // dispatch to another module AFTER the echo makes this the rollback probe too — arm it with
    // `_fab_poc_mod` DRIFTED and the decline must re-interpret with the echoes appearing ONCE.
    "module _fab_poc_echo(n=1) { echo(\"poc\", n, k=n+1); echo(n) sphere(r=n); _fab_poc_mod(n) cube(n); }",
    // RECURSIVE, childless: native-to-native dispatch rides the host stack until the
    // MAX_MODULE_NATIVE_DEPTH budget exhausts, then hands the REST of the tree to the interpreter
    // mid-recursion — the fractal_tree shape, and the depth-budget path no test exercised.
    "module _fab_poc_rec(n=1) { if (n > 0) _fab_poc_rec(n - 1); else cube(1); }",
    // BAKED constants, compiled (AR.14.4 band 2): `UP` (a vector) and `_EPSILON` (a number) are
    // free reads the emitter burns in as VALUES — see `poc_module_bakes` for the values, and the
    // hand `MODULE_REGISTRY` row for the const GUARD that keeps a program rebinding either name
    // from reaching a native with stale bits.
    "module _fab_poc_bake(s=1) { translate(UP*s) cube(_EPSILON*1e9); }",
    // FUNCTION DISPATCH, compiled (AR.14.4.3): `helper` is a USER function resolved at runtime
    // (no sibling table, no fingerprint on the callee — dispatch), and `max` is a builtin reached
    // through the SAME `call_fn` ladder, so a program's `function max(a,b)=…` shadow resolves
    // exactly as interpreting would. The named arg exercises `fill_slots` through the fn ABI.
    "module _fab_poc_fncall(x=1) { cube(helper(v=x)); sphere(r=max(x, 2)); }",
    // The `$`-SET, compiled (AR.22): a hoisted in-body `$fab_ds = …` WRITES the dynamic chain —
    // the self-reference reads the INHERITED value, the echo and the compiled child block read
    // the NEW one, and the forwarded call-site children read it too (the attachment mechanism:
    // a parent's `$`-set reaching the children it renders).
    "module _fab_poc_dollarset(k=1) { $fab_ds = $fab_ds + k; echo(ds=$fab_ds); _fab_poc_mod(k) { cube($fab_ds); children(); } }",
    // NESTED MODULE DEFS, compiled (AR.14.4.5): `inner` registers onto the interpreter's own
    // local-module stack with a frame materialized AFTER the whole prelude, so its body — always
    // INTERPRETED — reads `w` at its post-reassignment value even though the def sits textually
    // above the assignment (whole-scope last-wins, cuboid's sharpest case). Both calls dispatch
    // through the ordinary `fx.call`, which must find the LOCAL def before any global sibling.
    "module _fab_poc_localmod(k=1) { module inner(a) { cube([a, w, 1]); } w = k * 2; inner(3); inner(w); }",
    // NESTED FUNCTION DEFS, compiled (AR.14.4.5): `pre` calls `f` BEFORE its hoist position —
    // unknown in both tiers (warn + undef), pinning position-correct registration; `f` captures
    // the earlier local `b`, calls the sibling `g` defined BELOW it (the letrec group), and `g`
    // self-recurses. `c` invokes `f` from the PRELUDE, through `call_fn`'s rung-1 invoke.
    "module _fab_poc_localfn(k=1) { pre = f(k); b = k + 1; function f(x) = g(x) + b; function g(x) = x <= 0 ? 0 : g(x - 1) + 1; c = f(2); cube([c, b, is_undef(pre) ? 1 : 9]); }",
    // A LOCAL module taking CHILDREN from a compiled call site — half_of's shape. A local def can
    // never be armed, so the compiled child block meets an interpreted callee and the whole
    // native DECLINES (AR.20.7): the differential pins that the decline is rolled back and the
    // interpreted re-run answers identically.
    "module _fab_poc_localmodkids(k=1) { module wrap() { children(); translate([k*2,0,0]) children(); } wrap() cube(k); }",
    // A PARAMETER holding a function VALUE, invoked (AD.1): the interpreter's oracle-pinned rule
    // is that a local binding holding a closure shadows any like-named function in call position.
    // `call_fn`'s rung 1 used to DECLINE this shape; it now runs the interpreter's `CallValue`
    // machinery, so the native answers instead of re-interpreting the whole module.
    "module _fab_poc_callparam(f) { v = f(4); cube([v, 1, 1]); }",
    // A registered nested fn called from INSIDE a compiled child block (the adversarial finding
    // on AR.14.4.5's first cut): the thunk renders under ANOTHER module's ctx, and the AR.22
    // bridge replaced the whole scope with the render point's — dropping the creator's LEXICAL
    // frame, where the letrec closure lives. `h` answered warn-and-`undef` and the cube rendered
    // WRONG while both tiers returned Ok — edge_profile_asym's exact shape. The bridge now
    // splits lexical (creator) from dynamic (render point), as the interpreted arm always has.
    "module _fab_poc_localfnthunk(k=1) { function h(x) = x * 2; _fab_poc_mod(k) { cube(h(k)); } }",
];

/// Synthetic bakes for the POC references above — BOSL2's own values, so a tier test that binds
/// the same names at top level arms the native. Every reference other than `_fab_poc_bake` bakes
/// nothing (returns empty), which keeps the other nine POCs byte-identical to their pre-band-2
/// emission.
///
/// # Errors
/// A value with no bake form (can't happen for the fixed values here; the signature matches
/// [`Baked::from_value`]).
fn poc_module_bakes(reference: &str) -> Result<Vec<(&'static str, Baked)>, String> {
    if !reference.starts_with("module _fab_poc_bake(") {
        return Ok(Vec::new());
    }
    Ok(vec![
        ("_EPSILON", Baked::from_value(fab_lang::Value::Num(1e-9))?),
        (
            "UP",
            Baked::from_value(fab_lang::Value::num_list(vec![0.0, 0.0, 1.0]))?,
        ),
    ])
}

pub const GENERATED_ENTRIES: &[&str] = &[
    "_fab_poc_sq",
    "_fab_poc_near0",
    "_fab_poc_outer",
    // AR.27 — the OUTWARD call. `_fab_poc_absent` is deliberately NOT in this list, so the
    // emitter cannot compile it and the call has to dispatch through `fx.call_named` instead of
    // taking the caller down with it. A POC whose whole point is what is MISSING.
    "_fab_poc_outward",
    // AR.27 — an echo-EXPRESSION in a function body. The console side effect is half the answer
    // and no mesh comparison can see it, so it gets a differential of its own.
    "_fab_poc_echo",
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
    // AR.18 band 5 — a sibling call with a HOLE. The pair is the test: the caller fills slots 0
    // and 2, so slot 1 must come back as the callee's default (7) rather than undef.
    "_fab_poc_sib",
    "_fab_poc_hole",
    // AR.17 stage C — first-class functions through `FnCtx`. `_fab_poc_callshadow` is the AN.10
    // rung-1 rule inline: param `last` shadows the generated sibling of the same name, so a
    // function VALUE passed there is INVOKED and anything else falls through to the sibling —
    // both branches in one native, decided at runtime like the interpreter. `_fab_poc_curried2`
    // is the computed callee: a table of function values indexed then applied.
    "_fab_poc_callshadow",
    "_fab_poc_curried2",
    // AR.17.2 — the mint POCs: every literal position the census measured, through the real
    // emitter (path, captures, self_name, the fn-locals collapse). `_fab_poc_mint_id` precedes
    // `_fab_poc_mint_arg`, whose body calls it as a sibling.
    "_fab_poc_mint_ret",
    "_fab_poc_mint_letrec",
    "_fab_poc_mint_id",
    "_fab_poc_mint_arg",
    "_fab_poc_mint_list",
    // AR.17.2.3 — the first REAL mint slice: fnliterals' currying factories, the reduce letrec
    // worker, and f_gt (literal-as-argument + computed callee — the f_* battery's shape).
    // `f_2arg_simple` precedes `f_gt`, whose body calls it as a sibling.
    "reduce",
    "f_1arg",
    "f_2arg",
    "f_2arg_simple",
    "f_3arg",
    "f_gt",
    // AR.25 — the hand-native cone. `determinant` is the first SELF-recursive generated
    // native; `constrain` closes the anchor hole that silently guard-declined vector_angle
    // and affine3d_rot_from_to (whose migrations follow their cone here). `apply`/`rot` stay
    // hand-listed with their blockers: apply's callees (reverse, vnf_reverse_faces, str_join)
    // cascade into library-scale generation, rot is the `_NO_ARG` bootstrap bake.
    "flatten",
    "list_to_matrix",
    "det2",
    "det3",
    "det4",
    "determinant",
    "constrain",
    "vector_angle",
    "affine3d_rot_from_to",
    // AR.17 stage A — the migration-closed batch: every HAND native whose emitted code resolves
    // its sibling calls inside this list (measured by `hand_native_closed_batch`, not argued).
    // What stays hand traces almost entirely to `is_vector`, whose only decline is the AN.10
    // param-shadow (`all_nonzero`) — the exact shape the FnCtx ABI flip un-declines; the rest
    // rides it (is_matrix/sum/unit/v_abs/... call is_vector) or waits on a registry entry for a
    // callee (`is_range`, `constrain`, `is_vnf`, `list_wrap`) or a value-typed bootstrap bake
    // (`UP`, `_NO_ARG`).
    "_is_liststr",
    "point3d",
    "_list_pattern",
    "same_shape",
    "is_consistent",
    "num_defined",
    "force_list",
    "_tri_class",
    "_is_at_left",
    "_sum",
    "is_2d_transform",
    "_is_point_on_line",
    "ident",
    "affine3d_zrot",
    "affine3d_xrot",
    "affine3d_yrot",
    "in_list",
    "_group_sort_by_index",
    "point2d",
    "affine3d_identity",
    "affine3d_translate",
    // AR.17 stage C — the rung-1 conditional un-declined is_vector (the AN.10 poster child:
    // param `all_nonzero` shadowing the like-named function now emits the interpreter's own
    // rule inline), and its dependency cone rides. Still hand after AR.25's cone:
    // apply (its callees reverse/vnf_reverse_faces/str_join cascade — library-scale
    // generation's job) and rot (the `_NO_ARG` sentinel no bootstrap bake can express).
    "is_vector",
    "all_nonzero",
    "is_matrix",
    "sum",
    "unit",
    "_apply",
    "_bt_search",
    "is_path",
    "v_abs",
    "v_theta",
    "vector_axis",
    "affine3d_rot_by_axis",
    // AR.17 stage D — the clean missing callees (select's cone stood behind `is_range`,
    // apply/_vnf_centroid behind `is_vnf`), then the cone they unblock.
    "is_range",
    "is_vnf",
    "select",
    "_none_inside",
    "_point_dist",
    "_get_ear",
    "_vnf_centroid",
];

/// Record a CALL by name: builtin or user dep. Deliberately IGNORES the lexical scope — a name
/// that is also a binding is the AN.10 shape (a parameter shadowing a function in call position),
/// and it stays in the guard set anyway: over-approximating keeps the guard honest while the
/// dispatch-level veto handles the shadowing itself.
fn record_call(name: &str, out: &mut Analysis) {
    if name.starts_with('$') {
        return;
    }
    if fab_lang::is_builtin(name) {
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

/// The statement-tree half of [`walk`] — a module BODY's free reads, calls and `$`-uses.
///
/// AR.20. Scoping mirrors the emitter's own statement walk exactly, and has to: a `for`/`let`
/// binding is in scope for its CHILDREN and not after (so the scope is truncated on the way out),
/// while a bare `assignment` binds for the REST of its block, which is why it is not truncated
/// here. A walk that got that wrong would report a bound name as a free read and bake a library
/// constant over a local — the same value in the same slot, silently.
fn walk_stmt(s: &Stmt, scope: &mut Vec<String>, out: &mut Analysis) {
    match &s.kind {
        StmtKind::Empty | StmtKind::Use(_) | StmtKind::Include(_) => {}
        StmtKind::Block(kids) => {
            let mark = scope.len();
            for k in kids {
                walk_stmt(k, scope, out);
            }
            scope.truncate(mark);
        }
        // Binds for the rest of the enclosing block — deliberately NOT truncated.
        StmtKind::Assignment { name, value } => {
            walk(value, scope, out);
            scope.push(name.to_string());
        }
        StmtKind::If {
            cond, then, els, ..
        } => {
            walk(cond, scope, out);
            for branch in [then, els] {
                let mark = scope.len();
                for k in branch {
                    walk_stmt(k, scope, out);
                }
                scope.truncate(mark);
            }
        }
        StmtKind::Module(mi) => {
            let mark = scope.len();
            // `for`/`let` bind their arguments for the CHILDREN; every other module's arguments are
            // ordinary expressions and its name is a call.
            let binds = matches!(mi.name.as_str(), "for" | "intersection_for" | "let");
            for a in &mi.args {
                walk(&a.value, scope, out);
                if binds && let Some(n) = &a.name {
                    scope.push(n.to_string());
                }
            }
            if !binds && !matches!(mi.name.as_str(), "children" | "echo" | "assert") {
                out.deps.insert(mi.name.to_string());
            }
            for k in &mi.children {
                walk_stmt(k, scope, out);
            }
            scope.truncate(mark);
        }
        // A nested definition's body is not this module's reads: it registers at runtime
        // (AR.14.4.5) and is INTERPRETED against the live captured scope, so its free reads
        // resolve there — never through the enclosing native's bakes.
        StmtKind::ModuleDef { .. } | StmtKind::FunctionDef { .. } => {}
    }
}

/// Does any node of this subtree READ a `$`-name (value or call position)? The mint-decline
/// probe for literal param defaults — see `function_literal`.
fn subtree_reads_dollar(e: &Expr) -> bool {
    let mut stack = vec![e];
    while let Some(e) = stack.pop() {
        // `expr_children` includes call CALLEES, so the Ident arm covers both positions.
        if let ExprKind::Ident(n) = &e.kind
            && n.starts_with('$')
        {
            return true;
        }
        stack.extend(fab_lang::expr_children(e));
    }
    false
}

/// AR.17.2 capture analysis — the free names of a literal subtree under the literal's OWN scope
/// discipline: value-position idents AND call-position callee names, non-`$`, not bound within.
/// Call-position names matter because the minted body resolves them through dispatch rung 1's
/// scope walk (the f_* battery calls param-held function values); [`walk`]'s `record_call`
/// deliberately ignores scope for GUARD over-approximation, which is the wrong direction here —
/// a missed capture is a silent `undef` divergence — so this walk is scope-EXACT, copying
/// [`walk`]'s binding rules verbatim.
fn free_reads<'e>(
    e: &'e Expr,
    scope: &mut Vec<&'e str>,
    out: &mut std::collections::BTreeSet<&'e str>,
) {
    fn read<'e>(name: &'e str, scope: &[&'e str], out: &mut std::collections::BTreeSet<&'e str>) {
        if !name.starts_with('$') && !scope.contains(&name) {
            out.insert(name);
        }
    }
    match &e.kind {
        ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Undef => {}
        ExprKind::Ident(name) => read(name, scope, out),
        ExprKind::Unary { operand, .. } => free_reads(operand, scope, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            free_reads(lhs, scope, out);
            free_reads(rhs, scope, out);
        }
        ExprKind::Ternary { cond, then, els } => {
            free_reads(cond, scope, out);
            free_reads(then, scope, out);
            free_reads(els, scope, out);
        }
        ExprKind::Index { base, index } => {
            free_reads(base, scope, out);
            free_reads(index, scope, out);
        }
        ExprKind::Member { base, .. } => free_reads(base, scope, out),
        ExprKind::Call { callee, args } => {
            if let ExprKind::Ident(name) = &callee.kind {
                read(name, scope, out);
            } else {
                free_reads(callee, scope, out);
            }
            for a in args {
                free_reads(&a.value, scope, out);
            }
        }
        ExprKind::Vector(items) => {
            for i in items {
                free_reads(i, scope, out);
            }
        }
        ExprKind::Range { start, step, end } => {
            free_reads(start, scope, out);
            if let Some(s) = step {
                free_reads(s, scope, out);
            }
            free_reads(end, scope, out);
        }
        ExprKind::FunctionLiteral { params, body } => {
            // Defaults evaluate in the closure's BASE (bind_values phase 1), where the
            // literal's own params are NOT visible — walk them BEFORE the params enter scope
            // (skeptic finding: a default reading a name that collides with a param counted as
            // bound-within, so the enclosing binding went uncaptured and the mint answered
            // undef where the twin answered the captured value).
            let mark = scope.len();
            for p in params {
                if let Some(d) = &p.default {
                    free_reads(d, scope, out);
                }
            }
            scope.extend(params.iter().map(|p| &*p.name));
            free_reads(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::Let { bindings, body } | ExprKind::LcFor { bindings, body } => {
            let mark = scope.len();
            for b in bindings {
                free_reads(&b.value, scope, out);
                if let Some(n) = &b.name {
                    scope.push(n);
                }
            }
            free_reads(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::Assert { args, body } | ExprKind::Echo { args, body } => {
            for a in args {
                free_reads(&a.value, scope, out);
            }
            if let Some(b) = body {
                free_reads(b, scope, out);
            }
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            // Update BINDS before cond/body walk — the `walk` LcForC rule verbatim.
            let mark = scope.len();
            for b in init.iter().chain(update) {
                free_reads(&b.value, scope, out);
                if let Some(n) = &b.name {
                    scope.push(n);
                }
            }
            free_reads(cond, scope, out);
            free_reads(body, scope, out);
            scope.truncate(mark);
        }
        ExprKind::LcEach(inner) => free_reads(inner, scope, out),
        ExprKind::LcIf { cond, then, els } => {
            free_reads(cond, scope, out);
            free_reads(then, scope, out);
            if let Some(e2) = els {
                free_reads(e2, scope, out);
            }
        }
    }
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
            // Defaults BEFORE params enter scope — they evaluate in the closure's BASE where
            // the literal's own params are not visible (same rule as `free_reads`; the old
            // order under-approximated consts for a default shadowed by a sibling param).
            let mark = scope.len();
            for p in params {
                if let Some(d) = &p.default {
                    walk(d, scope, out);
                }
            }
            scope.extend(params.iter().map(|p: &Parameter| p.name.to_string()));
            // AR.17.2: a literal's body dispatches at INVOKE against the captured env, so a
            // call to a SCOPE-BOUND name (a capture, or the literal's own param) resolves
            // through rung 1 in both tiers — it is not a `ctx.functions` dep, and recording it
            // as one would demand a function the program need not define (`reduce`'s `func` is
            // a parameter; an existence check on "func" vetoes the whole entry). Free call
            // names — and every builtin — still record: the guard-safe over-approximation
            // stays for everything the scope cannot prove.
            let mut inner = Analysis::default();
            walk(body, scope, &mut inner);
            out.consts.extend(inner.consts);
            out.builtins.extend(inner.builtins);
            for d in inner.deps {
                if !scope.contains(&d) {
                    out.deps.insert(d);
                }
            }
            scope.truncate(mark);
        }
        // `let` and comprehension-`for` share binder semantics (sequential bindings, body in
        // the extended scope, binders gone after) — but `let` adds one rule the comprehension
        // form does not (AR.17.2): a binding whose RHS is a function LITERAL is scope-resolved
        // EVERYWHERE inside this let — its own body via `name_closure`'s self-reinjection, the
        // let body via dispatch rung 1 — never through `ctx.functions`, so it must not become a
        // dep/const (the letrec workers' names name nothing else, and a dep that must EXIST
        // would veto arming). `record_call` is deliberately scope-blind, so the binder's
        // records from THIS subtree are removed after the walk; a record that predates the
        // binder's push (an earlier sibling binding's forward reference, which really can reach
        // a like-named global) survives via the snapshot — the guard-safe direction.
        ExprKind::Let { bindings, body } => {
            let mark = scope.len();
            let mut letrec: Vec<(String, bool)> = Vec::new();
            for b in bindings {
                let literal_rhs = matches!(b.value.kind, ExprKind::FunctionLiteral { .. });
                if literal_rhs && let Some(n) = &b.name {
                    letrec.push((n.to_string(), out.deps.contains(&**n)));
                    scope.push(n.to_string());
                }
                walk(&b.value, scope, out);
                if !literal_rhs && let Some(n) = &b.name {
                    scope.push(n.to_string());
                }
            }
            walk(body, scope, out);
            for (n, had) in letrec {
                if !had {
                    out.deps.remove(&n);
                }
            }
            scope.truncate(mark);
        }
        ExprKind::LcFor { bindings, body } => {
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
    use super::analyze_function;

    /// AR.20 — the MODULE half of the coverage ratchet, and the roadmap for what modules need
    /// next. Same contract as the function ratchet: a floor that must not fall, plus a decline
    /// histogram that ranks the remaining work by how many modules each construct blocks.
    ///
    /// Separate from the function ratchet because the two move independently and a single number
    /// would hide which half regressed.
    ///
    /// Read the roadmap with:
    /// `cargo nextest run -p fab-lib -E 'test(bosl2_module_coverage)' --no-capture`
    #[test]
    fn bosl2_module_coverage_holds_its_floor() {
        use std::collections::BTreeMap;

        /// Modules the emitter compiles TODAY, out of BOSL2's 414 unambiguous ones. Raise it as
        /// bands land; lowering it is a deliberate act that needs a reason next to it.
        ///
        /// 342 when the ratchet landed, 347 once `# % *` compiled, then 346 — one module lost on
        /// purpose (`parent_module`, whose silent-Undef answer a decline beats). Then 291 when
        /// AR.14.4 band 1 put generated text in front of RUSTC for the first time: 55 modules
        /// "emitted" `$`-named parameters/assignments as `let p_$tag = …` — invalid Rust the
        /// text-only census could never see, and semantically wrong anyway (a `$` binding is
        /// DYNAMIC, it reaches every callee). 396 once AR.22 armed the `$`-set band. Then 402
        /// with AR.14.4.5's nested defs (cuboid, half_of, corner_profile, bounding_box,
        /// rabbit_clip, edge_profile_asym); the tail is first-class functions (AR.17), two
        /// honest AN.6 duplicate-param declines, and stroke's `widths` free read — an upstream
        /// bug (read on a path that never assigns it), so it stays declined on purpose.
        const FLOOR: usize = 402;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !root.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = crate::library::Library::read(&root).expect("BOSL2 reads");
        let folded = lib.fold_constants();
        // BAKES AND SIBLINGS FOR REAL, the same way `generate_batch` builds them. Probing with
        // empty ones measures a transpiler nobody runs: the first pass of this census reported
        // 69/414 and blamed `$children`, when most of its declines were library FUNCTIONS that do
        // emit — the module just had not been handed the sibling table. Same lesson AR.16's ratchet
        // learned, re-learned one layer up.
        let siblings: Vec<super::Sibling> = lib
            .functions
            .values()
            .filter_map(|f| {
                let baked = super::bake_reads(&f.source, &lib, &folded).unwrap_or_default();
                super::sibling_of(&super::Subject {
                    name: "",
                    source: &f.source,
                    baked,
                })
                .ok()
            })
            .collect();

        let mut emitted = 0_usize;
        let mut declines: BTreeMap<String, usize> = BTreeMap::new();
        for m in lib.modules.values() {
            // NOT `unwrap_or_default()`. Swallowing a bake failure is what made the first run of
            // this census report 69/414 and blame `$children`; an empty bake table is a measurement
            // of a transpiler nobody runs, so a failure here is a decline with its own name.
            let baked = match super::bake_reads(&m.source, &lib, &folded) {
                Ok(b) => b,
                Err(e) => {
                    *declines
                        .entry(format!("bakes unavailable: {e}"))
                        .or_default() += 1;
                    continue;
                }
            };
            match super::generate_module_native(&m.source, &baked, &siblings) {
                Ok(_) => emitted += 1,
                Err(e) => {
                    // The message is `name: reason`; the reason is the band.
                    let reason = e.split_once(": ").map_or(e.clone(), |(_, r)| r.to_string());
                    *declines.entry(reason).or_default() += 1;
                }
            }
        }
        let total = lib.modules.len();
        let mut ranked: Vec<_> = declines.iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        println!("\n=== BOSL2 MODULE codegen coverage ===");
        println!(
            "{emitted}/{total} modules emit ({:.1}%)",
            100.0 * emitted as f64 / total as f64
        );
        for (reason, n) in ranked {
            println!("  {n:>4}  {reason}");
        }
        assert!(
            emitted >= FLOOR,
            "module codegen coverage FELL to {emitted} (floor {FLOOR}) — a construct the emitter \
             used to handle now declines, which does not fail anything else because the decline \
             just falls back to interpretation"
        );
    }

    /// AR.17 stage A scoping: which HAND-WRITTEN registry natives could the emitter regenerate
    /// today? Per-entry emit/decline against the FULL registry batch (sibling table included, so
    /// a hand native calling another registry name is not a false decline). Run:
    /// `cargo test -p fab-lib hand_native_migration_census -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement probe, not a gate"]
    fn hand_native_migration_census() {
        let (subjects, _pins) = fab_lang::bootstrap_all();
        let generated: std::collections::BTreeSet<&str> =
            super::GENERATED_ENTRIES.iter().copied().collect();
        // Bakes + siblings the way `generate_module` builds them; a subject whose bake refuses
        // (a value-typed const the bootstrap path cannot re-render) is its own decline bucket.
        let mut sink = std::collections::BTreeMap::new();
        let mut sibs: Vec<super::Sibling> = Vec::new();
        let mut baked_by_name: std::collections::BTreeMap<&str, Vec<(&str, super::Baked)>> =
            std::collections::BTreeMap::new();
        for b in &subjects {
            let baked = super::bake_bootstrap(b, &mut sink).unwrap_or_default();
            if let Ok(s) = super::sibling_of(&super::Subject {
                name: b.name,
                source: b.source,
                baked: baked.clone(),
            }) {
                sibs.push(s);
            }
            baked_by_name.insert(b.name, baked);
        }
        let mut emit = Vec::new();
        let mut decline = Vec::new();
        for b in &subjects {
            if generated.contains(b.name) {
                continue;
            }
            let baked = baked_by_name.remove(b.name).unwrap_or_default();
            match super::generate_native(b.source, &baked, &sibs) {
                Ok(_) => emit.push(b.name),
                Err(e) => decline.push((b.name, e)),
            }
        }
        eprintln!("=== hand-native migration census ===");
        eprintln!("EMITS ({}):", emit.len());
        for n in &emit {
            eprintln!("  {n}");
        }
        eprintln!("DECLINES ({}):", decline.len());
        for (n, e) in &decline {
            eprintln!("  {n}: {e}");
        }
    }

    /// AR.17 stage A scoping, part 2: the largest MIGRATION-CLOSED batch — candidates whose
    /// emitted code resolves every sibling call INSIDE the batch (the regen constraint the
    /// per-entry census cannot see, because it hands out the full-registry sibling table). Run:
    /// `cargo test -p fab-lib hand_native_closed_batch -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement probe, not a gate"]
    fn hand_native_closed_batch() {
        let candidates = [
            "_is_liststr",
            "point3d",
            "_list_pattern",
            "same_shape",
            "is_consistent",
            "num_defined",
            "force_list",
            "all_nonzero",
            "is_matrix",
            "_tri_class",
            "_is_at_left",
            "_none_inside",
            "_sum",
            "sum",
            "unit",
            "is_2d_transform",
            "_apply",
            "_bt_search",
            "_point_dist",
            "_is_point_on_line",
            "ident",
            "affine3d_zrot",
            "affine3d_xrot",
            "affine3d_yrot",
            "_get_ear",
            "in_list",
            "is_path",
            "_group_sort_by_index",
            "v_abs",
            "v_theta",
            "point2d",
            "affine3d_identity",
            "affine3d_rot_from_to",
            "affine3d_translate",
            // Stage C candidates — the former decliners, re-eligible as bands land: is_vector
            // (AN.10, un-declined by the rung-1 conditional), and the callee/bake-blocked tail.
            "is_vector",
            "select",
            "vector_angle",
            "_vnf_centroid",
            "vector_axis",
            "apply",
            "affine3d_rot_by_axis",
            // NOT `rot`: its `_NO_ARG` consts_v is the heterogeneous sentinel no bootstrap bake
            // can express, and one unexpressible constant refuses the whole batch.
            "_region_region_intersections",
        ];
        let mut batch: Vec<&str> = super::GENERATED_ENTRIES.to_vec();
        for c in candidates {
            if !batch.contains(&c) {
                batch.push(c);
            }
        }
        loop {
            match super::generate_module(&batch) {
                Ok(_) => break,
                Err(e) => {
                    let name = e.split(':').next().unwrap_or("").trim().to_string();
                    let before = batch.len();
                    batch.retain(|n| *n != name);
                    assert!(
                        batch.len() < before,
                        "decline named no removable entry: {e}"
                    );
                    eprintln!("dropped {name}: {e}");
                }
            }
        }
        eprintln!("=== migration-closed batch ({} total) ===", batch.len());
        for n in &batch {
            if !super::GENERATED_ENTRIES.contains(n) {
                eprintln!("  {n}");
            }
        }
    }

    /// The MODULE decline census (AR.14.4 scoping): who is still outside the band, bucketed by
    /// decline reason — the roadmap, because each bucket names the next capability and the counts
    /// rank them. Run:
    /// `cargo test -p fab-lib module_decline_census -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement probe, not a gate"]
    fn module_decline_census() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !root.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = crate::library::Library::read(&root).expect("BOSL2 reads");
        let folded = lib.fold_constants();
        let mut histo: std::collections::BTreeMap<String, Vec<(String, &str)>> =
            std::collections::BTreeMap::new();
        let mut armed = 0usize;
        for m in lib.modules.values() {
            let outcome = super::bake_reads(&m.source, &lib, &folded)
                .and_then(|baked| super::generate_module_native(&m.source, &baked, &[]));
            match outcome {
                Ok(_) => armed += 1,
                Err(e) => {
                    let reason = e
                        .split_once(": ")
                        .map_or(e.as_str(), |(_, r)| r)
                        .to_string();
                    let bucket = reason.split(" `").next().unwrap_or(&reason).to_string();
                    histo.entry(bucket).or_default().push((reason, &m.name));
                }
            }
        }
        println!("armed: {armed} of {}", lib.modules.len());
        let mut rows: Vec<_> = histo.iter().collect();
        rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        for (bucket, entries) in rows {
            let _ = bucket;
            let sample: Vec<&&str> = entries.iter().map(|(_, n)| n).take(6).collect();
            println!("[{}x] {} - e.g. {:?}", entries.len(), entries[0].0, sample);
        }
    }

    /// AR.11 — how much of the pinned BOSL2 the emitter actually owns, as a RATCHET. Runs the
    /// codegen over every top-level function in the library and asserts the emit count never
    /// falls; the decline histogram it prints alongside is the phase roadmap, because each
    /// decline names the construct the emitter does not yet handle and the counts rank them.
    ///
    /// A FLOOR rather than a report for the AR.3.3 reason: an emitter that quietly stops handling
    /// a construct does not fail, it falls back to interpretation, and every other test still
    /// passes. Only a number that must not drop can see that.
    ///
    /// AR.26.4.4 — the STATIC call graph's shape, which is what decides how much host stack the
    /// compiled tier can consume.
    ///
    /// A generated native calls an in-batch sibling with a plain Rust call, so the emitted call
    /// graph IS a stack-depth profile. Two numbers come out of it and they want different fixes:
    /// the DAG's HEIGHT is a build-time constant and can simply be asserted, while a CYCLE is
    /// unbounded at runtime and has to stop being a static call at all.
    ///
    /// Reported rather than merely asserted, because the cycle list is the work list.
    #[test]
    fn the_static_call_graph_is_measured() {
        use std::collections::{BTreeMap, BTreeSet};

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !root.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = crate::library::Library::read(&root).expect("BOSL2 reads");
        let band = super::function_band(&lib).expect("band computes");
        let in_batch: BTreeSet<&str> = band.subjects.iter().map(|s| s.name).collect();

        // The edges a native turns into a STATIC Rust call: user-function deps that are also in the
        // batch, which is exactly the filter `guard_closures` applies. An OVERESTIMATE, deliberately
        // — AR.27 routes some of these through dispatch instead (a `$`-argument, an AN.10 shadow) —
        // and overestimating is the safe direction for a bound on stack depth.
        let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for s in &band.subjects {
            let a = super::analyze_function(s.source).expect("subject analyzes");
            let out = a
                .deps
                .iter()
                .filter_map(|d| in_batch.get(d.as_str()).copied())
                .collect();
            edges.insert(s.name, out);
        }

        // Longest path, memoized, with cycle detection folded in: a node re-entered while still on
        // the current path is a BACK-EDGE, and its height is capped rather than diverging.
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Open,
            Done(usize),
        }
        fn height<'a>(
            n: &'a str,
            edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
            marks: &mut BTreeMap<&'a str, Mark>,
            cyclic: &mut BTreeSet<&'a str>,
        ) -> usize {
            match marks.get(n) {
                Some(Mark::Done(h)) => return *h,
                Some(Mark::Open) => {
                    cyclic.insert(n);
                    return 0; // on the current path: a back-edge, not a longer path
                }
                None => {}
            }
            marks.insert(n, Mark::Open);
            let h = edges
                .get(n)
                .into_iter()
                .flatten()
                .map(|&c| 1 + height(c, edges, marks, cyclic))
                .max()
                .unwrap_or(0);
            marks.insert(n, Mark::Done(h));
            h
        }

        let mut marks = BTreeMap::new();
        let mut cyclic = BTreeSet::new();
        let mut tallest = ("", 0usize);
        for &n in &in_batch {
            let h = height(n, &edges, &mut marks, &mut cyclic);
            if h > tallest.1 {
                tallest = (n, h);
            }
        }

        println!("\n=== BOSL2 static call graph (AR.26.4.4) ===");
        println!(
            "subjects {}, edges {}",
            band.subjects.len(),
            edges.values().map(BTreeSet::len).sum::<usize>()
        );
        println!("DAG height {} (tallest: {})", tallest.1, tallest.0);
        println!("names on a cycle: {} {:?}", cyclic.len(), cyclic);

        // THE HEIGHT IS A CEILING, not a target. Measured at ~28 KiB of release stack per level on
        // `window_light_blocker.scad`, so this number times that is the compiled tier's static
        // worst case — the figure wasm's 1 MiB link-time stack has to hold, since a browser thread
        // cannot ask for more the way `EVAL_STACK` does. Raising it is a deliberate act.
        const MAX_HEIGHT: usize = 24;
        assert!(
            tallest.1 <= MAX_HEIGHT,
            "the static call DAG grew to {} (via {}) — every level is host stack the compiled tier \
             spends before the depth guard can see it",
            tallest.1,
            tallest.0
        );
    }

    /// Read the roadmap with:
    /// `cargo nextest run -p fab-lang -E 'test(bosl2_codegen_coverage)' --no-capture`
    #[test]
    fn bosl2_codegen_coverage_holds_its_floor() {
        use std::collections::BTreeMap;

        /// Functions the emitter compiles TODAY (2026-07-28), out of 1335 — 632 at AR.11, 742 once
        /// 632 at AR.11, 742 once AR.15 made `Value::Str`
        /// emittable, 936 once AR.16 baked the library's own constants, 1072 once AR.18 filled a
        /// sibling call's holes with the callee's defaults. Raise it as bands land — first-class
        /// functions (AR.17) are the biggest left, at 75 computed callees plus 34 literals plus 32
        /// AN.10 shapes. Lowering it is a deliberate act that needs a reason next to it, which is
        /// the whole point of the ratchet.
        ///
        /// LOWERED ONCE, 1072 -> 1066, and here is the reason. Six functions were compiling calls
        /// to `textmetrics`/`fontmetrics`/`object`/`rands`/`parent_module` into `rt::builtin(...)`,
        /// which CANNOT answer them — the evaluator intercepts all five in `run_builtin` before the
        /// pure table, so `rt::builtin` returned `Undef` with no error and no warning. Those six
        /// were not coverage, they were six silent wrong answers, and they now decline by name.
        /// See `CONTEXT_BUILTINS`.
        ///
        /// 1105 at AR.17 stage C: computed callees (`f(a)(b)`, `fns[i](x)`) and AN.10
        /// local-binding calls emit through `FnCtx` — invoking a VALUE is the one capability the
        /// flip bought.
        ///
        /// 1206 at AR.17.2: function LITERALS emit as `fx.mint_fn` calls — the evaluator
        /// constructs the closure from a path-addressed node of the fingerprint-proven def, so
        /// the whole 107-strong literal band collapsed in one move (fnliterals.scad essentially
        /// whole). The tail was free reads (36), echo-in-function (28), C-style comprehensions
        /// (11), `rands` (8), `$`-args in sibling calls (~15), and the param-held-callee shape
        /// (12) whose conditional emission has no static else-half.
        ///
        /// LOWERED 1206 -> 866 at AR.26.2, and this one is a MEASUREMENT CORRECTION rather than a
        /// regression — the third time this phase has found a ratchet counting a transpiler nobody
        /// runs (AR.16's empty bakes, AR.20.9's empty siblings, AR.22's $-mirage). Every number
        /// above was produced by probing each function against a sibling table built from the WHOLE
        /// library, including functions that decline. A generated sibling call is a STATIC Rust call
        /// to a bare ident, so those emissions named functions that do not exist in the output: the
        /// file does not compile, and a decliner takes its callers down with it. `function_band`
        /// runs the drop-and-rebuild to a FIXPOINT — 9 rounds — and 866 is what survives.
        ///
        /// WHERE THE 463 WENT, and it was the roadmap: only about 88 declined for their own reasons
        /// (free read 36, echo 28, C-style comprehension 11, `rands` 8, duplicate parameter 5). The
        /// rest was CASCADE — a decliner taking its callers with it — and the histogram named each
        /// victim's callee: `move` alone took 19, `segs` 19, `sort` 18.
        ///
        /// RAISED 866 -> 1162 at AR.27, which took the second of those two levers: a call to a
        /// non-emitting sibling now DISPATCHES through the evaluator instead of declining, exactly
        /// as a MODULE call has since AR.20.5 — which is why the module side never had a cascade.
        /// The fixpoint converges in 5 rounds instead of 9, and the tail is finally constructs
        /// rather than collateral: free reads, echo-in-function, `rands`, `$`-args in sibling
        /// calls, and named args to an out-of-batch callee (which cannot be positionalised without
        /// the callee's parameter list, so they decline honestly).
        ///
        /// Then 1162 -> 1139, a CORRECTION rather than a regression and the same shape as AR.22's:
        /// a `$`-named `let` binder emitted `let l5_$attach_to = …`, which is not a Rust
        /// identifier, so 23 of those 1162 could never have compiled. They were coverage on paper.
        /// The module side declined `$`-bindings at AR.20.4 for the SEMANTIC reason — a `$` binding
        /// is dynamic, it reaches every callee rather than the lexical children — and the function
        /// side simply never had the check. Found the only way this class ever gets found: by
        /// building the band.
        ///
        /// 1217 once `call_named` carried the argument NAMES over instead of positionalising them
        /// (91.5%). Declining named args to an out-of-batch callee was costing 78 functions for a
        /// caution that turned out to be unnecessary: matching slots at RUNTIME against whatever the
        /// name resolved to is not merely as safe as compile-time positionalising, it is safer —
        /// positionalising matches against the LIBRARY's parameter list for a callee that resolves
        /// against the USER's program, which is AN.10 wearing a function's hat. AR.20.8 found and
        /// removed exactly that hazard on the module side; this is the same fix.
        ///
        /// The tail is now constructs and nothing else: free reads 36, echo-in-function 29,
        /// C-style comprehension 11, `rands` 8, `$`-args in sibling calls ~11, duplicate parameter
        /// 5 (a CORRECT decline), range form 2.
        ///
        /// 1233 (92.7%) once a `$`-ARGUMENT routed a sibling call through dispatch too. A `$`-arg
        /// sets a DYNAMIC binding for the callee and a static Rust call has no frame to carry one,
        /// so the call declined — but `fill_slots` already separates `$`-args and `bind_values`
        /// already binds them into the call frame's dynamic parent, so the runtime path needed
        /// nothing new at all. Costs a dispatch where a static call would have done.
        ///
        /// 1260 (94.8%) once an echo-EXPRESSION compiled in a function body. It had been
        /// module-only for a stated reason — "a function native has no console to reach" — that
        /// stopped being true the moment WARNINGS needed one: `echo` moved onto the `Console`
        /// supertrait both ctx shapes carry, and the emission is now identical on both sides.
        ///
        /// The tail is 37 free reads, 11 C-style comprehensions, 8 `rands`, 5 duplicate parameters
        /// (a CORRECT decline), 3 range forms, and a handful of singletons.
        ///
        /// WHAT THIS NUMBER IS AND IS NOT, because four corrections came from getting it wrong.
        /// This test PROBES the emitter; it does not COMPILE what the emitter writes, and every
        /// overcount in the history above was a construct that emitted text rustc would refuse.
        /// The compile gate is `fab-bosl2`: it is a workspace member, so an ordinary
        /// `cargo check --workspace` builds all 1260 of these natives. THE TWO GATES ARE A PAIR —
        /// if fab-bosl2 ever leaves the workspace, this number silently goes back to being a
        /// measure of the emitter's optimism.
        const FLOOR: usize = 1260;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !root.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = crate::library::Library::read(&root).expect("BOSL2 reads");
        let band = super::function_band(&lib).expect("band computes");
        let total = lib.functions.len();
        let ok = band.subjects.len();

        let mut by_reason: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for (name, reason) in &band.declined {
            let slot = by_reason.entry(classify(reason)).or_insert((0, String::new()));
            slot.0 += 1;
            if slot.1.is_empty() {
                slot.1 = (*name).to_string();
            }
        }
        let mut hist: Vec<_> = by_reason.into_iter().collect();
        hist.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
        println!("\n=== BOSL2 codegen coverage ===");
        println!(
            "functions {total} ({} unparsed files), fixpoint rounds {}",
            lib.unparsed.len(),
            band.rounds
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
        // sound — clearing a band re-runs the survivors against whatever they hit next. And the
        // dominant bucket is now CASCADE ("call to X, not a builtin or generated sibling"), which
        // names the caller's victim rather than a construct: those functions are compilable and are
        // waiting on whatever their callee tripped on.
        for (reason, (n, ex)) in &hist {
            println!("  {n:5}  {reason}   e.g. {ex}");
        }

        // CASCADE ROOTS, ranked by blast radius. The histogram above names each victim's missing
        // CALLEE, which is the wrong half of the story on its own — the actionable question is what
        // that callee itself tripped on, because clearing ONE root recovers everyone waiting on it.
        // This is the roadmap the flat histogram cannot be.
        let own: BTreeMap<&str, &str> = band
            .declined
            .iter()
            .map(|(n, r)| (*n, r.as_str()))
            .collect();
        let mut roots: Vec<(usize, &str, String)> = Vec::new();
        for (reason, (n, _)) in &hist {
            let Some(rest) = reason.strip_prefix("other — call to `") else {
                continue;
            };
            let Some(callee) = rest.split('`').next() else {
                continue;
            };
            let Some(own_reason) = own.get(callee) else {
                continue; // not a declining library function — a name nothing declares
            };
            roots.push((*n, callee, classify(own_reason)));
        }
        roots.sort_by_key(|(n, name, _)| (std::cmp::Reverse(*n), *name));
        if !roots.is_empty() {
            println!("--- cascade ROOTS (clear one, recover its column) ---");
            for (n, callee, why) in roots.iter().take(15) {
                println!("  {n:5}  {callee} declines on: {why}");
            }
        }

        assert!(
            ok >= FLOOR,
            "codegen coverage REGRESSED: {ok} functions emit, floor is {FLOOR}. \
             The emitter lost a construct — see the histogram above. It does not fail loudly on \
             its own, it just falls back to interpreting, which is why this number is a gate."
        );
        assert!(
            lib.unparsed.is_empty(),
            "{} BOSL2 files no longer parse — that is a PARSER regression, not a codegen one, and \
             it silently shrinks every count in this report: {:?}",
            lib.unparsed.len(),
            lib.unparsed
        );
    }



    /// AR.26.2 — the three emitter bugs a library-scale batch exposed, pinned so they cannot
    /// reopen. All three were invisible for months for one reason: nothing had ever COMPILED a
    /// batch this wide, and every gate the emitter had asserted on substrings of its own output.
    /// Substrings prove the emitter ran; they do not prove what it wrote would build.
    #[test]
    fn the_library_scale_emitter_bugs_stay_closed() {
        // (1) RESERVED keywords, not just strict ones. BOSL2 declares `function typeof(...)`,
        // which emitted bare in three positions and parsed in none of them.
        for kw in [
            "move", "type", "fn", "loop", // strict
            "typeof", "become", "box", "priv", "yield", "macro", "abstract", "do", "final",
            "override", "unsized", "virtual", // reserved
        ] {
            assert_eq!(
                super::rust_fn_ident(kw).as_deref(),
                Ok(format!("r#{kw}").as_str()),
                "`{kw}` must be raw-escaped — a bare keyword is a parse error, not a warning"
            );
        }
        for bad in ["crate", "self", "super", "Self"] {
            assert!(
                super::rust_fn_ident(bad).is_err(),
                "`{bad}` cannot be spelled even raw, so it must decline"
            );
        }
        assert_eq!(super::rust_fn_ident("cuboid").as_deref(), Ok("cuboid"));

        // (2) A FALLIBLE parameter default cannot live in a closure returning `Value`. BOSL2's
        // `binsearch(key, list, idx, cmp=f_cmp())` defaults by CALLING a sibling.
        let sib = super::sibling_of(&super::Subject {
            name: "callee",
            source: "function callee() = 1;",
            baked: Vec::new(),
        })
        .expect("sibling analyzes");
        let text = super::generate_native(
            "function withdefault(a, b=callee()) = a + b;",
            &[],
            std::slice::from_ref(&sib),
        )
        .expect("emits");
        assert!(
            text.contains("match args.get(1).cloned()"),
            "a fallible default must bind through a `match`, not `unwrap_or_else` — got:\n{text}"
        );
        assert!(
            !text.contains("unwrap_or_else(|| callee"),
            "a `?` inside a closure returning Value does not type-check"
        );
        // and a CHEAP default still takes the eager path
        let plain = super::generate_native("function plain(a, b=2) = a + b;", &[], &[])
            .expect("emits");
        assert!(
            plain.contains("unwrap_or_else"),
            "a constant default should stay lazy-but-infallible"
        );

        // (3) The FALLBACK_SOURCES raw string widens to fit its content. BOSL2's `phillips_depth`
        // carries a `\"#`, and the old fixed `r#\"…\"#` aborted the whole 866-function batch over it.
        assert_eq!(super::raw_string_hashes("nothing special"), 1);
        assert_eq!(super::raw_string_hashes("a \"# b"), 2);
        assert_eq!(super::raw_string_hashes("a \"## b"), 3);
        assert_eq!(
            super::raw_string_hashes("a \" b # c"),
            1,
            "a quote and a hash that are not adjacent do not close anything"
        );
        let with_hash = super::generate_batch(
            &[super::Subject {
                name: "q",
                source: "function q() = \"#\";",
                baked: Vec::new(),
            }],
            &["function q() = \"#\";"],
        )
        .expect("a `\"#` in a reference must not abort the batch");
        assert!(
            with_hash.contains("r##\""),
            "the island must open with enough hashes to hold its content"
        );
    }


    /// AR.27 SKEPTIC ROUND — two shapes the emitter must keep DECLINING, because dispatching them
    /// would answer where the interpreter reads a file, or fire a side effect the interpreter never
    /// reaches. Both were regressions the cascade lever introduced, and both are emit-time refusals, so
    /// this pins the emitter rather than a tier comparison.
    #[test]
    fn the_outward_call_declines_what_it_must() {
        // FILE-VALUE BUILTINS. Not in `BUILTIN_SURFACE`, so `is_builtin` is false and they fall past
        // every builtin arm; the interpreter resolves them BEFORE builtins and `call_named` has no rung
        // for them, so dispatching answered `undef` where the interpreter read the file.
        for src in [
            "function _p(f) = import(f);",
            "function _p(f) = dxf_dim(file=f, name=\"d\");",
            "function _p(f) = dxf_cross(file=f, layer=\"l\");",
            // Through the AN.10 shadow arm too: its else-half funnels into the same call path, which
            // is why the guard sits in `static_call` rather than earlier.
            "function _p(import, f) = import(f);",
            "function _p(f) = let(import = 1) import(f);",
        ] {
            let out = super::generate_native(src, &[], &[]);
            assert!(
                out.is_err(),
                "a file-reading builtin must decline, not dispatch: {src} emitted {out:?}"
            );
        }
    
        // A SIDE-EFFECTING ARGUMENT to a dispatched callee. The interpreter never evaluates an unknown
        // callee's arguments, and drops a duplicated name or a surplus positional unevaluated — so an
        // echo would print where it should not, and an assert would KILL a render the interpreter
        // merely warns through.
        for src in [
            "function _p(x) = _absent(echo(\"fired\") x);",
            "function _p(x) = _absent(assert(false, \"boom\") x);",
        ] {
            let out = super::generate_native(src, &[], &[]);
            assert!(
                out.is_err(),
                "a side-effecting argument must decline: {src} emitted {out:?}"
            );
        }
        // A PURE argument still dispatches — the decline is about observability, not about arguments.
        assert!(
            super::generate_native("function _p(x) = _absent(x + 1);", &[], &[]).is_ok(),
            "a pure argument to an out-of-batch callee must still compile"
        );
    
        // A `$`-NAMED PARAMETER is not a Rust local, and `function_band`'s liveness probe is
        // `generate_native` returning Ok — so emitting one would keep the function in the batch and
        // break the whole generated crate rather than just itself.
        assert!(
            super::generate_native("function _p(x, $fn = 8) = x * 2;", &[], &[]).is_err(),
            "a `$`-named parameter must decline"
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
                // Empty = keep the emitter's OWN detail: the shapes behind this probe cost
                // different amounts to support, so one bucket does not schedule anything.
                "",
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
                if bucket.is_empty() {
                    // Keep the emitter's OWN detail — everything from the probe onward. Used where
                    // one probe covers several shapes that cost different amounts to support.
                    return e
                        .rfind(probe)
                        .map_or_else(|| probe.to_string(), |i| e[i..].to_string());
                }
                return bucket.to_string();
            }
        }
        let tail = e.rsplit_once(": ").map_or(e, |(_, t)| t);
        format!("other — {}", tail.chars().take(50).collect::<String>())
    }

    /// AR.6 — the checked-in `generated.rs` is CURRENT: regenerating it produces the same
    /// bytes. The emitter lives in fab-lib now, so this gate reaches ACROSS the crate boundary
    /// to the file fab-lang ships — which is exactly the relationship AR.14.4 formalises when
    /// the generated crate replaces the checked-in file.
    ///
    /// Refresh with `FAB_REGEN=1`.
    #[test]
    fn generated_file_is_current() {
        let want = super::generate_module(super::GENERATED_ENTRIES).expect("generates");
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lang/src/eval/intrinsics/generated.rs"
        );
        if std::env::var_os("FAB_REGEN").is_some() {
            std::fs::write(path, &want).expect("write generated.rs");
            return;
        }
        let have = include_str!("../../lang/src/eval/intrinsics/generated.rs");
        assert_eq!(
            have, want,
            "generated.rs is stale — refresh with FAB_REGEN=1 (see the file header)"
        );
    }


    /// AR.20.8 — the generated MODULE file is current, and (because it is checked in and compiled)
    /// the emitter's module output is known to build rather than merely to contain the right
    /// substrings.
    #[test]
    fn generated_modules_are_current() {
        let want = super::generate_modules(super::GENERATED_MODULES).expect("generates");
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lang/src/eval/intrinsics/generated_modules.rs"
        );
        if std::env::var_os("FAB_REGEN").is_some() {
            std::fs::write(path, &want).expect("write generated_modules.rs");
            return;
        }
        let have = include_str!("../../lang/src/eval/intrinsics/generated_modules.rs");
        assert_eq!(
            have, want,
            "generated_modules.rs is stale — refresh with FAB_REGEN=1 (see the file header)"
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

    /// AR.12.2 — the emitter produces the SAME natives whether its subjects come from the hand
    /// registry or straight out of the library read. That is the equivalence the whole crate split
    /// rests on: if the two inputs disagreed, moving the transpiler off `REGISTRY` would silently
    /// change what gets compiled.
    ///
    /// Compares the emitted FUNCTIONS, not the whole file, and the reason is itself a finding: the
    /// AR.10 fallback island embeds the VERBATIM source, so a hand reference that was transcribed
    /// with different whitespace produces a byte-different `FALLBACK_SOURCES` while the natives
    /// above it are identical. Formatting-independence is a property of the emitter, not of the
    /// file it writes.
    #[test]
    fn library_subjects_emit_the_same_natives_as_registry_subjects() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("libs/BOSL2");
        if !dir.join("std.scad").exists() {
            eprintln!("skipping: libs/BOSL2 submodule not checked out");
            return;
        }
        let lib = crate::library::Library::read(&dir).expect("BOSL2 reads");

        // Entries BOSL2 declares that the registry also carries, so both inputs can describe
        // them. `approx` is the one that matters since AR.16: it BAKES `_EPSILON`, so this now
        // proves the library fold and the hand-written `Entry.consts` agree on the value — a
        // disagreement there is a native answering with a different epsilon, which is a wrong
        // answer rather than a missed compilation.
        // approx/posmod/idx are a real 3-CYCLE and a sibling call may only bind inside the batch,
        // so they come as a set or not at all.
        let names = [
            "is_nan",
            "is_finite",
            "is_def",
            "is_str",
            "last",
            "approx",
            "posmod",
            "idx",
        ];
        let from_registry = super::generate_module(&names).expect("registry path generates");
        let from_library =
            super::generate_from_library(&lib, &names).expect("library path generates");

        // From the first native onward — past the fallback island (see the doc above) AND past the
        // SURFACE table, which the two paths now DELIBERATELY disagree about: AR.26.2 made the
        // surface the LIBRARY's declaration, so the registry path declares the hand table's 83
        // names and the library path declares BOSL2's 1329. That difference is the decoupling
        // working; the natives below it are what this test is about.
        let natives = |text: &str| {
            text.find("pub(super) fn ")
                .map_or_else(String::new, |i| text[i..].to_string())
        };
        assert_eq!(
            natives(&from_registry),
            natives(&from_library),
            "the same functions compiled differently depending on where the emitter read them"
        );
        assert!(
            natives(&from_library).contains("pub(super) fn is_nan"),
            "the comparison would hold on two empty strings"
        );
    }

    /// AR.17.2 skeptic regressions, emit-level — the three capture/self_name holes the
    /// adversarial pass found by inspecting emitted text, pinned here the same way.
    #[test]
    fn skeptic_literal_holes_stay_closed() {
        // (1) An ELEMENT-position letrec gets its binder as self_name and the fn-locals
        // collapse — the old arm went through bare `expr`, so a binder colliding with a
        // builtin (`norm`) recursed into `rt::bi::norm` instead of itself, silently.
        let code = super::generate_native(
            "function _sk6(n) = [for (i = [0 : n]) let(norm = function(k) k <= 0 ? 0 : norm(k - 1)) norm(i)];",
            &[],
            &[],
        )
        .expect("generates");
        assert!(
            code.contains("Some(\"norm\")"),
            "element-let binder must ride self_name:\n{code}"
        );
        assert!(
            !code.contains("rt::bi::norm"),
            "the letrec call must collapse to call_value, not fall to the builtin:\n{code}"
        );

        // (2) A literal-param DEFAULT evaluates in the closure's BASE — a default reading a
        // name that collides with a sibling param must still CAPTURE the enclosing binding.
        let code = super::generate_native("function _sk5(y) = function(y, x = y) x;", &[], &[])
            .expect("generates");
        assert!(
            code.contains("(\"y\", p_y.clone())"),
            "the shadowed default read must capture the enclosing param:\n{code}"
        );

        // (3) A `$`-read in a literal's param default has no faithful minted env — decline.
        let err = super::generate_native("function _sk8() = function(x = $fn) x;", &[], &[])
            .expect_err("must decline");
        assert!(err.contains("parameter default"), "got: {err}");
    }

    /// AR.17.2 skeptic regression — a fired expression assert emits the INTERPRETER'S verdict:
    /// `rt::assert_failed` with the pretty-printed condition baked and the message arg
    /// evaluated in the failure branch, never the old fatal `bosl_assert("generated")`.
    #[test]
    fn a_fired_assert_emits_the_twin_verdict() {
        let code = super::generate_native(
            "function _ska(x) = assert(is_num(x), \"boom\") x;",
            &[],
            &[],
        )
        .expect("generates");
        assert!(
            code.contains("rt::assert_failed(Some(") && code.contains("\"is_num(x)\""),
            "missing the twin verdict in:\n{code}"
        );
        assert!(
            !code.contains("bosl_assert"),
            "the placeholder verdict must be gone:\n{code}"
        );
    }

    /// AR.20.4 — the statement emitter produces a module native, and it is the SAME module the
    /// hand-written POC implements. Printed rather than only asserted: this is the first generated
    /// module in the project, and its shape is the thing to look at.
    #[test]
    fn a_module_body_generates() {
        let src = "module _fab_poc_mod(k=1) { children(); }";
        let code = super::generate_module_native(src, &[], &[]).expect("generates");
        println!("\n=== generated module native ===\n{code}");
        for want in [
            "fn _fab_poc_mod(fx: &dyn rt::ModuleCtx) -> rt::Result<rt::Geo>",
            "let p_k = fx.args().get(0)",
            "parts.push(fx.children()?)",
            "Ok(fx.group(parts))",
        ] {
            assert!(code.contains(want), "missing `{want}` in:\n{code}");
        }
    }

    /// `*` DISABLES a subtree: no geometry AND no side effects. It must emit nothing, and it must
    /// not even WALK the subtree — an unsupported construct under a `*` is unreachable, so
    /// declining the module around it would invert what the modifier means.
    ///
    /// Asserted as an ABSENCE, which is why it is not in the band table above: those check that a
    /// construct appears, and the whole point here is that none does.
    #[test]
    fn a_disabled_subtree_emits_nothing_and_is_not_walked() {
        let code = super::generate_module_native("module m(n) { *cube(n); }", &[], &[])
            .expect("generates");
        assert!(
            !code.contains("cube"),
            "`*cube(n)` was emitted — the program said not to draw it:\n{code}"
        );
        // A construct the emitter CANNOT handle, under a `*`. It never runs, so it must not
        // decline the module.
        let unreachable = "module m(n) { *if (n) { x = 1; foo(x) bar(); } }";
        assert!(
            super::generate_module_native(unreachable, &[], &[]).is_ok(),
            "a disabled subtree was walked and declined the whole module"
        );
    }

    /// The scope PRELUDE hoists assignments (whole-scope, last-wins, blocks flattened) — the
    /// text-level half of the razor `a_compiled_module_hoists_scope_assignments_like_the_interpreter`
    /// pins at tier level. Assertions are on binder NAMES and value bits, never the fresh-ident
    /// counter (the AR.20.4 brittleness lesson).
    #[test]
    fn the_scope_prelude_hoists_assignments() {
        let out = super::generate_module_native(
            "module m(x=1) { cube(x); x = x + 2; { y = x; } sphere(r=y); }",
            &[],
            &[],
        )
        .expect("generates");
        // The hoisted binding appears BEFORE the statement above it, and its self-reference reads
        // the PARAM (the outer binding), exactly as `hoist_scope`'s sequential bind resolves it.
        let bind = out
            .find("_x = rt::binary")
            .expect("hoisted x binds from an expression");
        let cube = out.find("name: \"cube\"").expect("cube call emitted");
        assert!(
            bind < cube,
            "the assignment must bind before the cube that reads it"
        );
        // `p_x` appears exactly twice: the parameter binding line and the hoist expression — the
        // cube must read the HOISTED value, not the param.
        assert_eq!(
            out.matches("p_x").count(),
            2,
            "a statement read the param through the hoist:\n{out}"
        );
        // Block-flattened `y` binds at scope level and reaches the sphere outside the block.
        let y = out.find("_y = ").expect("y hoists out of the block");
        let sphere = out.find("name: \"sphere\"").expect("sphere call emitted");
        assert!(y < sphere, "y must bind before the sphere that reads it");

        // Last-wins dedupe: ONE binding, carrying the LAST expression (5.0) — the first is never
        // evaluated, matching `hoisted_assignments`.
        let out2 = super::generate_module_native("module m() { x = 1; cube(x); x = 5; }", &[], &[])
            .expect("generates");
        assert_eq!(
            out2.matches("_x = ").count(),
            1,
            "last-wins dedupe must emit exactly one binding:\n{out2}"
        );
        assert!(
            out2.contains("0x4014000000000000"), // 5.0 — the LAST expression
            "the binding must carry the LAST assignment's expression:\n{out2}"
        );
    }

    /// The statement subset declines by NAME. A module that silently skipped a statement would
    /// render MISSING GEOMETRY while still succeeding, which is the failure shape this phase keeps
    /// finding — so every unsupported construct says which one it was.
    #[test]
    fn module_statements_outside_the_subset_decline_by_name() {
        for (src, want) in [
            // NOT here any more: a child-block assignment — it EMITS as a per-thunk prelude as
            // of AR.22 (attachable's `$parent_*` block). `!` is the one modifier that stays
            // declined, under its OWN name so the histogram can price it: it diverts the subtree
            // into the program-global root override, which no `ModuleCtx` method reaches.
            ("module m() { !cube(1); }", "`!` root modifier"),
            // Nested defs EMIT as of AR.14.4.5 — and the PARSER already confines them to the
            // body's top level ("not inside a child block"), so the below-top-level decline arms
            // are defense in depth, unreachable from parseable source. What still declines is a
            // nested fn whose NAME shares a hoist slot: the interpreter's whole-scope last-wins
            // would rebind the slot across kinds (or shadow the param for the whole body), which
            // a per-position registration cannot mirror.
            (
                "module m() { f = 1; function f(x) = x; cube(f(1)); }",
                "sharing the hoist slot of `f`",
            ),
            (
                "module m(f) { function f(x) = x; cube(f(1)); }",
                "sharing the hoist slot of `f`",
            ),
            (
                "module m() { function f(x) = x; function f(x) = x + 1; cube(f(1)); }",
                "sharing the hoist slot of `f`",
            ),
        ] {
            let err = super::generate_module_native(src, &[], &[])
                .expect_err(&format!("must decline: {src}"));
            assert!(err.contains(want), "expected `{want}` in `{err}` for {src}");
        }
    }

    /// AR.14.4.5 — nested defs at the body's top level REGISTER: a `function` def becomes a
    /// `register_local_fn` at its hoist POSITION (capturing exactly the locals bound before it),
    /// module defs become ONE `register_local_modules` AFTER the whole prelude (the interpreter
    /// captures the fully-hoisted scope, so a def above a reassignment sees the final value).
    #[test]
    fn nested_defs_register_in_hoist_order() {
        let out = super::generate_module_native(
            "module m(k) { module inner(a) { cube([a, w, 1]); } a = 1; function f(x) = x + a; b = f(2); w = b + k; inner(w); }",
            &[],
            &[],
        )
        .expect("generates");
        let bind_a = out.find("_a = ").expect("a hoists");
        let reg_f = out
            .find("fx.register_local_fn(\"f\", &[(\"a\", ")
            .expect("f registers with the locals bound before it — and only those");
        let bind_b = out.find("_b = ").expect("b hoists");
        let reg_mods = out
            .find("fx.register_local_modules(&[\"inner\"], ")
            .expect("module defs register in one call");
        let call = out
            .find("fx.call(&rt::ModuleCall { name: \"inner\"")
            .expect("the call to the nested module is an ORDINARY dispatch");
        assert!(
            bind_a < reg_f && reg_f < bind_b,
            "f must register at its hoist position — after `a`, before `b`:\n{out}"
        );
        assert!(
            bind_b < reg_mods && reg_mods < call,
            "module registration must follow the WHOLE prelude and precede the statements:\n{out}"
        );
        // `f`'s frame must NOT carry `b` or `w` (hoisted after it) — the interpreter's
        // capture-at-bind-position view.
        let f_line = &out[reg_f..out[reg_f..].find('\n').map_or(out.len(), |i| reg_f + i)];
        assert!(
            !f_line.contains("\"b\"") && !f_line.contains("\"w\""),
            "f's frame must stop at its bind position: {f_line}"
        );
        // The module frame DOES carry the later locals — `inner` reads `w` post-hoist.
        let m_line = &out[reg_mods
            ..out[reg_mods..]
                .find('\n')
                .map_or(out.len(), |i| reg_mods + i)];
        assert!(
            m_line.contains("\"w\"") && m_line.contains("\"a\""),
            "the module frame is the full hoisted scope: {m_line}"
        );

        // A def inside a BARE block registers with the top scope — a `{ }` is not a scope
        // upstream, and the runtime's `collect_module_defs` flattens it identically.
        let bare = super::generate_module_native(
            "module m(k) { { module inner() { cube(k); } } inner(); }",
            &[],
            &[],
        )
        .expect("generates");
        assert!(
            bare.contains("fx.register_local_modules(&[\"inner\"], "),
            "a bare-block def must ride the top registration:\n{bare}"
        );
    }

    /// AR.20.4 — the statement bands the census counted, each generating rather than declining.
    #[test]
    fn the_statement_bands_generate() {
        for (label, src, want) in [
            (
                "statement for (69 modules)",
                "module m(n) { for (i = [0:n]) children(i); }",
                "_i in rt::iter_values_warned(fx, ",
            ),
            (
                "statement let (8 modules)",
                "module m(n) { let (d = n * 2) children(); }",
                "_d =",
            ),
            (
                "statement assert (part of the 89 assert/echo)",
                "module m(n) { assert(n > 0) children(); }",
                "return Err(rt::assert_decline(",
            ),
            (
                "child-block assignment (L.5.2) — a per-thunk let",
                "module m() { translate([1,0,0]) { x = 2; cube(x); } }",
                "_x = ",
            ),
            (
                "child-block `$`-assignment (AR.22) — save/set/restore around the thunk",
                "module m(n) { translate([n,0,0]) { $t = n; sphere(1); } }",
                "fx.set_dollar(\"$t\"",
            ),
            (
                "nested for, inner iterable sees the outer binder",
                "module m(n) { for (i = [0:n], j = [0:i]) children(j); }",
                "_j in rt::iter_values_warned(fx, ",
            ),
            (
                "children(i) selects rather than rendering all",
                "module m(n) { for (i = [0:n]) children(i); }",
                "fx.child_at(",
            ),
            // AR.20.8 — the module-call arm. The args go out in SOURCE ORDER with their names
            // attached; nothing is positionalised here, because the callee's parameter list is a
            // runtime fact (AN.10).
            (
                "a module call dispatches",
                "module m(n) { cyl(h=n, r=2); }",
                "fx.call(&rt::ModuleCall { name: \"cyl\"",
            ),
            (
                "a named argument keeps its NAME rather than being positionalised",
                "module m(n) { cyl(h=n, r=2); }",
                "(Some(\"h\"),",
            ),
            (
                "a `$`-argument is just an argument whose name starts with `$`",
                "module m(n) { cyl(n, $fn=8); }",
                "(Some(\"$fn\"),",
            ),
            (
                "a childless call says so",
                "module m(n) { cyl(n); }",
                "children: rt::Children::None",
            ),
            (
                "a child block becomes one thunk per geometry child",
                "module m(n) { translate([n,0,0]) { cube(n); sphere(n); } }",
                "rt::Children::Compiled(&[",
            ),
            // The `# % *` band. `#` is preview-only so it emits as if unwritten; `%` runs the
            // subtree and then drops its geometry; `*` emits nothing at all.
            (
                "`#` emits as if it were not written",
                "module m(n) { #cube(n); }",
                "name: \"cube\"",
            ),
            (
                "`%` runs the subtree, then truncates its geometry off the parts list",
                "module m(n) { %cube(n); }",
                "parts.truncate(",
            ),
            (
                "`%` on a statement `if` too — the branch runs, its geometry does not survive",
                "module m(n) { %if (n > 0) children(); }",
                "parts.truncate(",
            ),
        ] {
            let code = super::generate_module_native(src, &[], &[])
                .unwrap_or_else(|e| panic!("{label}: declined: {e}"));
            assert!(code.contains(want), "{label}: missing `{want}` in:\n{code}");
        }
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
        for forbidden in [
            "crate::eval",
            "crate::parser",
            "super::",
            "fab_lang::Result",
        ] {
            assert!(
                !code.contains(forbidden),
                "generated code reaches `{forbidden}` — that resolves only while this file lives \
                 inside fab-lang. Route it through `fab_lang::rt` (adding to `rt` if it is not \
                 there yet, which is a deliberate act — see that module's note)."
            );
        }
        assert!(
            code.contains("rt::binary") && code.contains("rt::bi::"),
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
        let subject = fab_lang::BootstrapSubject {
            name: "t",
            source: "function t() = C;",
            nums: Vec::new(),
            lists: vec![("C", vec![1.0, f64::INFINITY])],
            const_names: vec!["C"],
            deps: &[],
            builtins: &[],
        };
        let mut consts = std::collections::BTreeMap::new();
        let err = super::bake_bootstrap(&subject, &mut consts).expect_err("declines");
        assert!(err.contains("non-finite element"), "{err}");
    }
}
