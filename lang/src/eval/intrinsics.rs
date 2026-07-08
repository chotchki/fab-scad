//! The intrinsic tier (O.1) — replace a hot BOSL2 function's INTERPRETED body with a hand-written native
//! Rust implementation, selected by an AST FINGERPRINT so it's never silently wrong.
//!
//! The safety doctrine, stated once: an intrinsic is dispatched ONLY when the running function's
//! `(params, body)` AST fingerprints EXACTLY to the version the intrinsic was written and verified against.
//! A user on a different BOSL2 revision (a renamed local, a tweaked formula, an extra clamp) fingerprints
//! DIFFERENTLY → the registry misses → the interpreter runs the real body. So an intrinsic can never be
//! applied to a function it wasn't proven equivalent to; the worst case is a missed speedup, never a wrong
//! answer. The fast==slow harness ([`tests`]) is the other half: it runs the intrinsic AND the interpreted
//! reference on the same inputs and asserts BIT-IDENTICAL, so a divergent intrinsic fails the build.
//!
//! This module is O.1 — the MECHANISM (fingerprint + registry + the never-wrong gate). The intrinsics
//! themselves (the hand-written bodies for the profile's hot functions) are O.2.

use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use super::value::Value;
use crate::parser::{Arg, Expr, ExprKind, Parameter};

/// A hand-written native implementation of a specific user function. Receives the call's POSITIONAL argument
/// VALUES (already evaluated, in source order) and returns the result — the same `Value` the interpreted body
/// would. PURE: a function of its args only (no scope, no `$`-vars); the dispatch gate ([`super`]) only
/// routes all-positional calls here, so the ABI stays a flat slice. An intrinsic implements the WHOLE
/// function for the arg shapes it accepts; it hardcodes the reference's parameter defaults (it matches that
/// exact source), so a short positional call still gets the right answer.
pub(super) type Intrinsic = fn(&[Value]) -> Value;

/// One registered intrinsic: the exact function it stands in for. `reference` is the VERBATIM source of that
/// function (one `function name(params) = body;`) — the single source of truth: its fingerprint gates
/// dispatch, and the fast==slow harness runs its interpreted body as the oracle the `func` must bit-match.
struct Entry {
    /// The function name the intrinsic implements (registry bucket key).
    name: &'static str,
    /// The verbatim reference source of that function — fingerprinted + run as the harness oracle.
    reference: &'static str,
    /// The native implementation.
    func: Intrinsic,
}

/// The intrinsic registry. `_fab_poc_sq` is the O.1 mechanism POC (a synthetic, collision-proof name); the
/// rest are O.2 — the profile's hot BOSL2 predicates. Each entry's `reference` is the VERBATIM BOSL2 source
/// (from `libs/BOSL2`); the fast==slow harness proves the native `func` bit-matches interpreting it, and
/// `FAB_EXPLAIN` confirms it WIREs (vs DRIFTs) against the user's actual library.
///
/// O.2 targets so far are LEAF predicates — bodies that call only OpenSCAD BUILTINS (`is_undef`,
/// `is_string`), so the harness can interpret the reference with a default `Ctx`. Predicates that call OTHER
/// BOSL2 functions (`is_finite` → `is_nan`, `is_vector` → `_EPSILON`) need a dependency-aware harness — the
/// next O.2 step.
static REGISTRY: &[Entry] = &[
    Entry {
        name: "_fab_poc_sq",
        reference: "function _fab_poc_sq(x) = x * x;",
        func: poc_sq,
    },
    // BOSL2 `is_def`/`is_str` — the two hottest LEAF predicates (called in nearly every optional-arg check
    // and string guard). Verbatim from libs/BOSL2/builtins.scad.
    Entry {
        name: "is_def",
        reference: "function is_def(x) = !is_undef(x);",
        func: is_def,
    },
    Entry {
        name: "is_str",
        reference: "function is_str(x) = is_string(x);",
        func: is_str,
    },
];

/// The POC intrinsic: `x * x`. Mirrors the interpreter's `Num * Num` (and `undef` for a non-number arg, as
/// `apply_binary` yields). Deliberately trivial — it exists to exercise the mechanism, not to be fast.
fn poc_sq(args: &[Value]) -> Value {
    match args {
        [Value::Num(x)] => Value::Num(x * x),
        _ => Value::Undef,
    }
}

/// BOSL2 `is_def(x) = !is_undef(x)` — true iff `x` is anything but `undef`. Only the first positional arg
/// binds to `x` (extras are ignored, per OpenSCAD); zero args → `x` is `undef` → `false`.
fn is_def(args: &[Value]) -> Value {
    Value::Bool(!matches!(args.first(), None | Some(Value::Undef)))
}

/// BOSL2 `is_str(x) = is_string(x)` — true iff `x` is a string.
fn is_str(args: &[Value]) -> Value {
    Value::Bool(matches!(args.first(), Some(Value::Str(_))))
}

/// `name → (fingerprint, intrinsic)` for every registry entry, computed ONCE by parsing each `reference` and
/// fingerprinting its `(params, body)`. Lazy + cached: the parse cost is paid the first time an intrinsic is
/// looked up in the process, never per call. A `reference` that doesn't parse to a single `function` def is
/// a registry BUG — it's dropped with a debug assert rather than silently mis-registering.
fn table() -> &'static [(&'static str, u64, Intrinsic)] {
    static TABLE: OnceLock<Vec<(&'static str, u64, Intrinsic)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        REGISTRY
            .iter()
            .filter_map(|entry| {
                let fp = reference_fingerprint(entry.reference)?;
                Some((entry.name, fp, entry.func))
            })
            .collect()
    })
}

/// Parse a registry `reference` (one `function` def) and fingerprint it, or `None` if it isn't exactly that
/// (a registry authoring bug).
fn reference_fingerprint(reference: &str) -> Option<u64> {
    use crate::parser::{StmtKind, parse};
    let program = parse(reference).ok()?;
    let stmt = program.stmts.into_iter().next()?;
    if let StmtKind::FunctionDef { params, body, .. } = stmt.kind {
        Some(fingerprint(&params, &body))
    } else {
        debug_assert!(false, "intrinsic reference is not a single function def: {reference}");
        None
    }
}

/// Resolve a defined function to its intrinsic, if one is registered for EXACTLY this body. Called ONCE per
/// function at [`super::build_ctx`] time (never per call): fingerprint the running `(params, body)`, then
/// match on (name, fingerprint). A miss — no entry for the name, or the name matches but the body doesn't —
/// returns `None`, so the interpreter runs the real body. This is the never-silently-wrong gate.
#[must_use]
pub(super) fn lookup(name: &str, params: &[Parameter], body: &Expr) -> Option<Intrinsic> {
    let fp = fingerprint(params, body);
    table()
        .iter()
        .find(|(n, f, _)| *n == name && *f == fp)
        .map(|(_, _, func)| *func)
}

/// Test-only access to a registry entry's reference source, for the fast==slow harness.
#[cfg(test)]
pub(super) fn reference_of(name: &str) -> Option<&'static str> {
    REGISTRY.iter().find(|e| e.name == name).map(|e| e.reference)
}

/// How a defined function relates to the intrinsic registry — the EXPLAIN classification (O.3).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Plan {
    /// An intrinsic is registered for this name AND the body fingerprint matches → native dispatch will fire.
    Wired,
    /// An intrinsic is registered for this NAME, but the defined body fingerprints DIFFERENTLY (a BOSL2
    /// revision the intrinsic's reference doesn't match) → it silently INTERPRETS. The actionable case:
    /// either the user's library drifted, or the intrinsic's reference source is stale and needs updating.
    Drift,
    /// No intrinsic registered for this name — the ordinary interpreted function (the vast majority).
    NotRegistered,
}

/// Classify a defined function against the registry (O.3 EXPLAIN). Pure + testable; the `FAB_EXPLAIN`
/// stderr report ([`super::build_intrinsics`]) is just this plus a print.
#[must_use]
pub(super) fn classify(name: &str, params: &[Parameter], body: &Expr) -> Plan {
    if !REGISTRY.iter().any(|e| e.name == name) {
        return Plan::NotRegistered;
    }
    if lookup(name, params, body).is_some() {
        Plan::Wired
    } else {
        Plan::Drift
    }
}

/// The registered REFERENCE fingerprint for `name` — the hash a running function must match to WIRE — or
/// `None` if no intrinsic is registered under that name. Feeds the EXPLAIN DRIFT diagnostic, which prints it
/// next to the running function's own fingerprint so an author can SEE how the two differ (stale reference vs
/// a genuinely different library version). See [`fingerprint`].
#[must_use]
pub(super) fn reference_fp(name: &str) -> Option<u64> {
    table().iter().find(|(n, _, _)| *n == name).map(|(_, fp, _)| *fp)
}

/// Is the `FAB_EXPLAIN` intrinsic-plan report on? Cached once (env read per ctx build would be silly).
pub(super) fn explain_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FAB_EXPLAIN").is_some())
}

/// A structural fingerprint of a function's `(params, body)`: a 64-bit hash over the AST SHAPE — variant
/// discriminants, operators, literal bits (`f64` by `to_bits`, so `NaN`/`±0` are exact), names, and nesting
/// — with SPANS EXCLUDED (a fingerprint is source-formatting-independent; only the structure counts). Two
/// functions fingerprinting equal are structurally identical. A fixed-seed hasher makes it run-reproducible.
///
/// Collision note: a 64-bit hash CAN alias in theory, but the registry pairs the fingerprint with the
/// function NAME and the fast==slow harness proves each registered intrinsic bit-matches its reference — so
/// a collision would have to hit a same-NAMED, harness-verified function to matter, which the harness would
/// itself catch. Fingerprint is a fast pre-filter, not the whole proof.
#[must_use]
pub(super) fn fingerprint(params: &[Parameter], body: &Expr) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_params(params, &mut h);
    hash_expr(body, &mut h);
    h.finish()
}

/// Hash a parameter list: arity, then each name + whether/how it defaults. Names ARE part of the identity
/// (a renamed param is a different function to us — and a different intrinsic contract).
fn hash_params(params: &[Parameter], h: &mut impl Hasher) {
    params.len().hash(h);
    for p in params {
        p.name.hash(h);
        match &p.default {
            Some(default) => {
                1u8.hash(h);
                hash_expr(default, h);
            }
            None => 0u8.hash(h),
        }
    }
}

/// Hash an argument list (call args + comprehension bindings): each arg's optional name + its value expr.
fn hash_args(args: &[Arg], h: &mut impl Hasher) {
    args.len().hash(h);
    for arg in args {
        match &arg.name {
            Some(name) => {
                1u8.hash(h);
                name.hash(h);
            }
            None => 0u8.hash(h),
        }
        hash_expr(&arg.value, h);
    }
}

/// Hash an expression's STRUCTURE, recursively, span-free. The match is EXHAUSTIVE with NO wildcard arm on
/// purpose: adding an `ExprKind` variant is then a COMPILE error here, forcing the fingerprint to account
/// for it — a silently-unhashed field would let two different functions collide and mis-dispatch an
/// intrinsic. Each arm leads with a distinct discriminant byte so structurally-different shapes can't alias
/// by field coincidence.
#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive per-variant match IS the safety mechanism — one arm per ExprKind, no wildcard, \
    so a new AST variant is a compile error here rather than a silently-unhashed field that could collide"
)]
fn hash_expr(e: &Expr, h: &mut impl Hasher) {
    match &e.kind {
        ExprKind::Num(n) => {
            0u8.hash(h);
            n.to_bits().hash(h);
        }
        ExprKind::Str(s) => {
            1u8.hash(h);
            s.hash(h);
        }
        ExprKind::Bool(b) => {
            2u8.hash(h);
            b.hash(h);
        }
        ExprKind::Undef => 3u8.hash(h),
        ExprKind::Ident(name) => {
            4u8.hash(h);
            name.hash(h);
        }
        ExprKind::Unary { op, operand } => {
            5u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(operand, h);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            6u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(lhs, h);
            hash_expr(rhs, h);
        }
        ExprKind::Ternary { cond, then, els } => {
            7u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_expr(els, h);
        }
        ExprKind::Index { base, index } => {
            8u8.hash(h);
            hash_expr(base, h);
            hash_expr(index, h);
        }
        ExprKind::Member { base, field } => {
            9u8.hash(h);
            hash_expr(base, h);
            field.hash(h);
        }
        ExprKind::Call { callee, args } => {
            10u8.hash(h);
            hash_expr(callee, h);
            hash_args(args, h);
        }
        ExprKind::Vector(items) => {
            11u8.hash(h);
            items.len().hash(h);
            for item in items {
                hash_expr(item, h);
            }
        }
        ExprKind::Range { start, step, end } => {
            12u8.hash(h);
            hash_expr(start, h);
            match step {
                Some(step) => {
                    1u8.hash(h);
                    hash_expr(step, h);
                }
                None => 0u8.hash(h),
            }
            hash_expr(end, h);
        }
        ExprKind::FunctionLiteral { params, body } => {
            13u8.hash(h);
            hash_params(params, h);
            hash_expr(body, h);
        }
        ExprKind::Let { bindings, body } => {
            14u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::Assert { args, body } => {
            15u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::Echo { args, body } => {
            16u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::LcFor { bindings, body } => {
            17u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::LcForC { init, cond, update, body } => {
            18u8.hash(h);
            hash_args(init, h);
            hash_expr(cond, h);
            hash_args(update, h);
            hash_expr(body, h);
        }
        ExprKind::LcEach(body) => {
            19u8.hash(h);
            hash_expr(body, h);
        }
        ExprKind::LcIf { cond, then, els } => {
            20u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_opt(els.as_deref(), h);
        }
    }
}

/// Hash an optional sub-expression (a present/absent flag then the expr) — `assert`/`echo`/`LcIf` bodies.
fn hash_opt(e: Option<&Expr>, h: &mut impl Hasher) {
    match e {
        Some(e) => {
            1u8.hash(h);
            hash_expr(e, h);
        }
        None => 0u8.hash(h),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "test harness: expect/panic ARE the assertions; intrinsics must bit-match, so == is exact"
)]
mod tests {
    use super::{fingerprint, lookup, poc_sq, reference_of};
    use crate::eval::build_ctx;
    use crate::parser::{Expr, Parameter, StmtKind, parse};
    use crate::{Scope, Value, eval_expr};

    /// Parse `src` (one `function` def) → its `(params, body)`.
    fn parse_fn(src: &str) -> (Vec<Parameter>, Expr) {
        let program = parse(src).expect("parses");
        let stmt = program.stmts.into_iter().next().expect("one stmt");
        match stmt.kind {
            StmtKind::FunctionDef { params, body, .. } => (params, body),
            other => panic!("expected a function def, got {other:?}"),
        }
    }

    /// `parse_fn` then fingerprint.
    fn fp(src: &str) -> u64 {
        let (params, body) = parse_fn(src);
        fingerprint(&params, &body)
    }

    /// The SLOW side of the harness: interpret a reference function's body with its params bound to
    /// `inputs`, via `eval_expr` (a default `Ctx` — NO intrinsics, so this is the pure interpreter).
    fn interpret(reference: &str, inputs: &[Value]) -> Value {
        let (params, body) = parse_fn(reference);
        let mut scope = Scope::new();
        for (p, v) in params.iter().zip(inputs) {
            scope.bind(p.name.clone(), v.clone());
        }
        eval_expr(&body, &scope).expect("reference body evaluates")
    }

    #[test]
    fn fingerprint_is_span_independent() {
        // Same STRUCTURE, different source formatting (whitespace/comments shift every span) → SAME
        // fingerprint. This is the property the registry relies on: it matches structure, not bytes.
        let a = fp("function f(x) = x + 1;");
        let b = fp("function f( x ) =\n   x  +  1 ; // trailing");
        assert_eq!(a, b, "whitespace/comments must not change the fingerprint");
    }

    #[test]
    fn a_changed_body_fingerprints_differently() {
        // The never-silently-wrong gate: a tweaked formula, a renamed param, or a changed literal is a
        // DIFFERENT function → different fingerprint → the intrinsic misses and the interpreter runs.
        let base = fp("function f(x) = x + 1;");
        assert_ne!(base, fp("function f(x) = x + 2;"), "literal change");
        assert_ne!(base, fp("function f(x) = x - 1;"), "operator change");
        assert_ne!(base, fp("function f(y) = y + 1;"), "param rename");
        assert_ne!(base, fp("function f(x, y) = x + 1;"), "arity change");
        assert_ne!(base, fp("function f(x) = x + 1.0000001;"), "epsilon literal change");
    }

    #[test]
    fn structurally_identical_functions_collide_by_design() {
        // Two DIFFERENTLY-NAMED functions with identical params+body fingerprint the SAME — the registry
        // pairs the fingerprint with the NAME, so this is fine (name disambiguates); the fingerprint only
        // certifies the BODY matches. Documents that fingerprint alone is body-identity, not full identity.
        assert_eq!(fp("function a(x) = x * x;"), fp("function b(x) = x * x;"));
    }

    #[test]
    fn deep_structural_features_are_captured() {
        // Comprehensions, lets, ternaries, ranges, calls — the shapes real BOSL2 functions are built from —
        // all feed the hash; a change deep inside flips the fingerprint (no shallow-only hashing).
        let a = fp("function g(n) = [for (i = [0:n]) let(j = i*2) [i, j > 3 ? j : 0]];");
        let b = fp("function g(n) = [for (i = [0:n]) let(j = i*2) [i, j > 4 ? j : 0]];");
        assert_ne!(a, b, "a literal buried in a nested comprehension must still register");
    }

    #[test]
    fn fast_equals_slow_bit_for_bit() {
        // THE correctness gate: every registered intrinsic must return EXACTLY what interpreting its
        // reference body returns, for every input. This is what makes an intrinsic safe to exist — it's
        // proven equivalent to the code it replaces. O.2 extends this per new intrinsic + its inputs.
        let reference = reference_of("_fab_poc_sq").expect("POC registered");
        for x in [0.0, 1.0, -3.5, 2.5, 1e9, std::f64::consts::PI, -0.0] {
            let input = [Value::Num(x)];
            let fast = poc_sq(&input);
            let slow = interpret(reference, &input);
            assert_eq!(fast, slow, "intrinsic vs interpreter diverged at x={x}: {fast:?} != {slow:?}");
        }
        // A non-number arg: the intrinsic must ALSO match the interpreter's undef (x*x on a string → undef).
        let bad = [Value::string("nope")];
        assert_eq!(poc_sq(&bad), interpret(reference, &bad), "undef path must match too");
    }

    #[test]
    fn the_fingerprint_gate_matches_only_the_exact_body() {
        // Never silently wrong: the intrinsic registers for the EXACT reference, and misses on any
        // perturbation (different body) or a name mismatch → the interpreter runs the real body instead.
        let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
        assert!(lookup("_fab_poc_sq", &p, &b).is_some(), "the exact reference must register");

        let (p2, b2) = parse_fn("function _fab_poc_sq(x) = x + x;");
        assert!(lookup("_fab_poc_sq", &p2, &b2).is_none(), "a changed body must NOT match");

        let (p3, b3) = parse_fn("function _fab_poc_sq(x, y) = x * x;");
        assert!(lookup("_fab_poc_sq", &p3, &b3).is_none(), "a changed arity must NOT match");

        assert!(lookup("some_other_name", &p, &b).is_none(), "same body, wrong name → no match");
    }

    #[test]
    fn build_ctx_wires_the_intrinsic_for_a_matching_program() {
        // The dispatch is authorized at ctx build: a program defining the exact reference function gets the
        // intrinsic in ctx.intrinsics (so `dispatch_call` will route its all-positional calls natively). A
        // program with a perturbed body does NOT — it stays interpreted.
        let matched = parse("function _fab_poc_sq(x) = x * x;").expect("parses");
        assert!(
            build_ctx(&matched).intrinsics.contains_key("_fab_poc_sq"),
            "the exact reference must be wired as an intrinsic"
        );
        let perturbed = parse("function _fab_poc_sq(x) = x * x + 1;").expect("parses");
        assert!(
            !build_ctx(&perturbed).intrinsics.contains_key("_fab_poc_sq"),
            "a perturbed body must fall back to the interpreter (no intrinsic wired)"
        );
    }

    #[test]
    fn a_matching_call_dispatches_through_the_intrinsic_task() {
        // End-to-end: exercise `Task::Intrinsic` through the real eval loop. A program defines the exact
        // reference; its call's RHS is evaluated with the built ctx, so `dispatch_call` routes the
        // all-positional call to the native `poc_sq` → 7*7 = 49. (The corpus proves the arm doesn't break
        // anything; this proves it RUNS — nothing in BOSL2 fingerprints to the POC, so only this hits it.)
        let program = parse("function _fab_poc_sq(x) = x * x; z = _fab_poc_sq(7);").expect("parses");
        let ctx = build_ctx(&program);
        let call = match &program.stmts[1].kind {
            StmtKind::Assignment { value, .. } => value,
            other => panic!("expected an assignment, got {other:?}"),
        };
        let result = crate::eval::eval_with_ctx(call, &Scope::new(), &ctx).expect("evaluates");
        assert_eq!(result, Value::Num(49.0), "the intrinsic-dispatched call returns x*x");
    }

    #[test]
    fn leaf_predicate_intrinsics_match_their_references_bit_for_bit() {
        // O.2: each real predicate intrinsic must equal interpreting its VERBATIM BOSL2 reference, across
        // every value type. (These references call only builtins — is_undef/is_string — so `interpret`'s
        // default Ctx can run them.)
        let cases = [
            Value::Undef,
            Value::Num(3.0),
            Value::Num(-0.0),
            Value::Bool(false),
            Value::string("hi"),
            Value::list(vec![Value::Num(1.0), Value::Num(2.0)]),
        ];
        for name in ["is_def", "is_str"] {
            let reference = reference_of(name).expect("registered");
            let (params, body) = parse_fn(reference);
            let func = lookup(name, &params, &body).expect("its own reference must register");
            for input in &cases {
                let one = [input.clone()];
                assert_eq!(
                    func(&one),
                    interpret(reference, &one),
                    "{name}({input:?}) diverged"
                );
            }
            // Zero args: the single param defaults to undef in both paths.
            assert_eq!(func(&[]), interpret(reference, &[]), "{name}() diverged");
        }
    }

    #[test]
    fn explain_classifies_wired_drift_and_unregistered() {
        use super::Plan;
        // WIRED: exact reference → will dispatch natively.
        let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
        assert_eq!(super::classify("_fab_poc_sq", &p, &b), Plan::Wired);
        // DRIFT: registered NAME, different body → interprets silently (the case EXPLAIN surfaces).
        let (pd, bd) = parse_fn("function _fab_poc_sq(x) = x * x + 1;");
        assert_eq!(super::classify("_fab_poc_sq", &pd, &bd), Plan::Drift);
        // NotRegistered: an ordinary function.
        let (pn, bn) = parse_fn("function ordinary(x) = x + 1;");
        assert_eq!(super::classify("ordinary", &pn, &bn), Plan::NotRegistered);
    }
}
