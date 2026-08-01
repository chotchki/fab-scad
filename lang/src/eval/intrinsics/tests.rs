#![allow(
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "fast==slow battery tests: near-twin bindings (native vs interpreted) and long input batteries are the design, not an accident"
)]
use super::{fingerprint, pin_reference_of, poc_sq, reference_of, resolve};
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
fn fp(src: &str) -> crate::surface::Fingerprint {
    let (params, body) = parse_fn(src);
    fingerprint(&params, &body)
}

/// The SLOW side of the harness: interpret a reference function's body with its params bound to
/// `inputs`, via `eval_expr` (a default `Ctx` — NO intrinsics, so this is the pure interpreter). Returns a
/// `Result` so an inline-`assert` reference (its failure IS the reference's behavior) compares against the
/// intrinsic's error, not a panic.
fn interpret(reference: &str, inputs: &[Value]) -> crate::Result<Value> {
    let (params, body) = parse_fn(reference);
    // Defaults evaluate in the lexical BASE (`push_call` rule) — never against the params bound so
    // far, so the oracle can't hand a param-referencing default the call value the machine denies.
    let base = Scope::new();
    let mut scope = base.clone();
    for (i, p) in params.iter().enumerate() {
        // A provided arg fills the slot; an unprovided one takes the param's DEFAULT (else undef) — the
        // real call path binds defaults, so an oracle that skipped them would run a short call with the
        // wrong values (e.g. `point3d(p)` with `fill` unbound instead of `fill=0`).
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => eval_expr(d, &base)?,
                None => Value::Undef,
            },
        };
        scope.bind(p.name.clone(), v);
    }
    eval_expr(&body, &scope)
}

/// Fast (intrinsic) and slow (interpreter) agree: both `Ok` with bit-identical values, or both `Err` (the
/// message is a diagnostic locator, not output — an intrinsic reproduces the assert's CONTROL FLOW, so
/// "both raised" is the match). A mixed `Ok`/`Err` is a real divergence.
fn same_result(fast: &crate::Result<Value>, slow: &crate::Result<Value>) -> bool {
    match (fast, slow) {
        (Ok(a), Ok(b)) => bit_eq(a, b),
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

/// The SLOW side for a reference that calls OTHER BOSL2 functions (the dependency-aware oracle). `deps` are
/// the verbatim source of those functions; they precede `target` in one program so its body can resolve
/// them. The built `Ctx` has its intrinsics table CLEARED, so the oracle is FULLY interpreted end-to-end
/// (a dep that happens to be a registered intrinsic doesn't shortcut — we're proving against the
/// interpreter, not against another intrinsic). `target` must be the LAST definition.
fn interpret_with_deps(target: &str, deps: &[&str], inputs: &[Value]) -> crate::Result<Value> {
    let src = format!("{}\n{target}", deps.join("\n"));
    let program = parse(&src).expect("deps+target parse");
    let mut ctx = build_ctx(&program, crate::Config::default());
    ctx.intrinsics.clear(); // force full interpretation — no intrinsic shortcut even for the deps
    let (params, body) = match &program.stmts.last().expect("has target").kind {
        StmtKind::FunctionDef { params, body, .. } => (params, body),
        other => panic!("target is not a function def: {other:?}"),
    };
    // Defaults evaluate in the lexical BASE (`push_call` rule), not the growing param scope.
    let base = Scope::new();
    let mut scope = base.clone();
    for (i, p) in params.iter().enumerate() {
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => crate::eval::eval_with_ctx(d, &base, &ctx)?,
                None => Value::Undef,
            },
        };
        scope.bind(p.name.clone(), v);
    }
    crate::eval::eval_with_ctx(body, &scope, &ctx)
}

/// Bit-level `Value` equality — the harness's notion of "bit-identical". `f64`s compare by `to_bits`, so
/// two `NaN`s (same bits) are EQUAL where `==` says `NaN != NaN`, and `0.0`/`-0.0` (different bits) are
/// DISTINCT where `==` says equal — exactly the determinism doctrine. Recurses into lists; other variants
/// fall back to `==` (they carry no float). Used wherever an intrinsic can RETURN a number (`last`/
/// `default`); the `Bool`-returning predicates are fine with plain `==`.
fn bit_eq(a: &Value, b: &Value) -> bool {
    use Value::{List, Num, NumList, Range};
    match (a, b) {
        (Num(x), Num(y)) => x.to_bits() == y.to_bits(),
        (NumList(x), NumList(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(p, q)| p.to_bits() == q.to_bits())
        }
        (List(x), List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| bit_eq(p, q))
        }
        (
            Range {
                start: s1,
                step: t1,
                end: e1,
            },
            Range {
                start: s2,
                step: t2,
                end: e2,
            },
        ) => {
            s1.to_bits() == s2.to_bits()
                && t1.to_bits() == t2.to_bits()
                && e1.to_bits() == e2.to_bits()
        }
        _ => a == b,
    }
}

/// The value battery the predicate intrinsics are proven against — one of every `Value` shape, with the
/// float edges (`±0`, `±inf`, `NaN`) that `is_nan`/`is_finite` turn on, plus a `NaN`/`inf` INSIDE a list
/// (the element-wise-`!=` corner that separates a naive scalar `is_nan` from the real `x!=x`).
fn value_battery() -> Vec<Value> {
    vec![
        Value::Undef,
        Value::Num(0.0),
        Value::Num(-0.0),
        Value::Num(3.5),
        Value::Num(-42.0),
        Value::Num(f64::INFINITY),
        Value::Num(f64::NEG_INFINITY),
        Value::Num(f64::NAN),
        Value::Bool(true),
        Value::Bool(false),
        Value::string("hi"),
        Value::string(""),
        Value::list(vec![Value::Num(1.0), Value::Num(2.0)]),
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::num_list(vec![f64::NAN]),
        Value::num_list(vec![f64::INFINITY]),
        Value::list(vec![]),
        Value::Range {
            start: 0.0,
            step: 1.0,
            end: 5.0,
        },
    ]
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
    assert_ne!(
        base,
        fp("function f(x) = x + 1.0000001;"),
        "epsilon literal change"
    );
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
    assert_ne!(
        a, b,
        "a literal buried in a nested comprehension must still register"
    );
}

#[test]
fn fast_equals_slow_bit_for_bit() {
    // THE correctness gate: every registered intrinsic must return EXACTLY what interpreting its
    // reference body returns, for every input. This is what makes an intrinsic safe to exist — it's
    // proven equivalent to the code it replaces. O.2 extends this per new intrinsic + its inputs.
    let reference = reference_of("_fab_poc_sq").expect("POC registered");
    for x in [0.0, 1.0, -3.5, 2.5, 1e9, std::f64::consts::PI, -0.0] {
        let input = [Value::Num(x)];
        assert!(
            same_result(
                &poc_sq(&crate::surface::NoClosures, &input),
                &interpret(reference, &input)
            ),
            "intrinsic vs interpreter diverged at x={x}"
        );
    }
    // A non-number arg: the intrinsic must ALSO match the interpreter's undef (x*x on a string → undef).
    let bad = [Value::string("nope")];
    assert!(
        same_result(
            &poc_sq(&crate::surface::NoClosures, &bad),
            &interpret(reference, &bad)
        ),
        "undef path must match too"
    );
}

/// AR.8 — the band-2 constructs (`let` with a shadow-capable scope, inline `assert`'s raise path,
/// indexing, a defaulted parameter), all in one synthetic entry, proven against the interpreter
/// over shapes that exercise each: a hit, an out-of-range index (undef → assert RAISES both
/// sides), a missing second arg (the default binds), a non-list base.
#[test]
fn generated_band2_matches_the_interpreter() {
    let reference = reference_of("_fab_poc_band2").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("_fab_poc_band2", &params, &body)
        .expect("its own reference must register")
        .func;
    let cases: &[&[Value]] = &[
        &[Value::num_list(vec![5.0, 7.0]), Value::Num(1.0)], // hit: 14
        &[Value::num_list(vec![5.0])],                       // default i=0: 10
        &[Value::num_list(vec![5.0]), Value::Num(9.0)],      // out of range → assert raises
        &[Value::string("nope")],                            // non-list base → assert raises
        &[],                                                 // no args at all
    ];
    for input in cases {
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, input),
                &interpret(reference, input)
            ),
            "band2 diverged on {input:?}"
        );
    }
}

/// AR.10 — the depth budget, end to end: `approx` over lists nested far past `MAX_NATIVE_DEPTH`
/// must DECLINE to the pure interpreter (explicit stack) and still bit-match it — never ride the
/// Rust stack unbounded. Depth 300 is ~5x the budget: without the guard this recursed 300 heavy
/// native frames; with it, level 64 falls back and the rest runs on the machine. The structures
/// differ at the INNERMOST leaf so the `a == b` fast path can't short-circuit the recursion.
#[test]
fn deep_nesting_declines_to_the_interpreter() {
    fn nest(depth: usize, leaf: f64) -> Value {
        let mut v = Value::num_list(vec![leaf]);
        for _ in 0..depth {
            v = Value::list(vec![v]);
        }
        v
    }
    let reference = reference_of("approx").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("approx", &params, &body)
        .expect("its own reference must register")
        .func;
    let a = nest(300, 1.0);
    let b = nest(300, 2.0);
    let deps = [
        reference_of("idx").expect("registered"),
        reference_of("posmod").expect("registered"),
        reference_of("is_finite").expect("registered"),
        reference_of("is_nan").expect("registered"),
    ];
    let input = [a, b];
    let fast = func(&crate::surface::NoClosures, &input);
    assert!(
        same_result(
            &fast,
            &interpret_with_deps_consts(
                reference,
                &deps,
                &[("_EPSILON", Value::Num(1e-9))],
                &input
            ),
        ),
        "deep nesting must decline to the interpreter and still agree; fast: {fast:?}"
    );
}

/// AR.9 — the comprehension constructs (a `for` over a RANGE literal, an element-position `let`,
/// a no-else `if`, an `each` splice mixed into the same vector), proven against the interpreter
/// over the shapes that exercise each arm — including empty and non-numeric iterable bounds.
#[test]
fn generated_band3_matches_the_interpreter() {
    let reference = reference_of("_fab_poc_band3").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("_fab_poc_band3", &params, &body)
        .expect("its own reference must register")
        .func;
    let cases: &[&[Value]] = &[
        &[Value::Num(4.0)],  // [4, 6, 8, 8, 9] — filter drops 0 and 2
        &[Value::Num(0.0)],  // filter drops everything → [8, 9]
        &[Value::Num(-1.0)], // empty range → [8, 9]
        &[Value::Undef],     // undef bound — the range constructor's coercion, both sides
        &[],
    ];
    for input in cases {
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, input),
                &interpret(reference, input)
            ),
            "band3 diverged on {input:?}"
        );
    }
}

/// AR.15 — string literals, proven against the interpreter rather than merely compiled. Two
/// escape layers stack in this reference (scad's lexer DECODES the source, then the emitter
/// RE-ENCODES the decoded text as a Rust literal), and a bug in either one produces a native that
/// builds fine and answers with subtly different bytes — so the cases below feed the default's
/// exact text back in, which only matches if both layers round-tripped.
#[test]
fn generated_band4_matches_the_interpreter() {
    let reference = reference_of("_fab_poc_band4").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("_fab_poc_band4", &params, &body)
        .expect("its own reference must register")
        .func;
    let cases: &[&[Value]] = &[
        // The default's DECODED text, byte for byte: an escaped quote, an escaped backslash and a
        // real newline. Equality here is the round-trip proof — either escape layer getting this
        // wrong makes `s == tag` false in the native and true in the interpreter.
        &[Value::string("q\"b\\c\nd")],
        &[Value::string("")],   // empty: falsy, and `str` of it is a no-op
        &[Value::string("αβ")], // non-ASCII in, non-ASCII out
        &[Value::Num(3.0)],     // cross-TYPE equality: a number never equals a string
        &[Value::Undef],
        &[Value::string("x"), Value::string("x")], // default overridden, both slots strings
        &[Value::string("x"), Value::Num(1.0)],    // a NUMBER where the default is a string
        &[],
    ];
    for input in cases {
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, input),
                &interpret(reference, input)
            ),
            "band4 diverged on {input:?}"
        );
    }
}

/// AR.6 — the LIST case the deleted hand `poc_sq` got WRONG: `x * x` with an equal-length numeric
/// list is the interpreter's DOT PRODUCT, and the hand native answered `Undef`. It sat unnoticed
/// because no battery input was a list. The generated native routes through `ops::apply_binary`,
/// so it cannot disagree with the interpreter on dispatch shape — the transpiler's first CORRECTED
/// divergence, pinned.
#[test]
fn generated_poc_sq_matches_the_interpreter_on_lists() {
    let reference = reference_of("_fab_poc_sq").expect("POC registered");
    for v in [
        Value::num_list(vec![1.0, 2.0, 3.0]), // dot with itself: 14
        Value::num_list(vec![]),              // empty: the empties rule
        Value::Undef,
    ] {
        let input = [v];
        assert!(
            same_result(
                &poc_sq(&crate::surface::NoClosures, &input),
                &interpret(reference, &input)
            ),
            "generated vs interpreter diverged on {:?}",
            input[0]
        );
    }
}

/// The SLOW side for a reference that reads a TOP-LEVEL CONSTANT (`_EPSILON`): like [`interpret`], plus
/// the named constants bound into the scope first — in a real program they'd resolve from the home-island
/// global, and the const GUARD (O.5.1) is what certifies the bound value matches the intrinsic's bake.
fn interpret_with_consts(
    reference: &str,
    consts: &[(&str, Value)],
    inputs: &[Value],
) -> crate::Result<Value> {
    let (params, body) = parse_fn(reference);
    let mut scope = Scope::new();
    for (name, v) in consts {
        scope.bind((*name).to_string(), v.clone());
    }
    for (i, p) in params.iter().enumerate() {
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => eval_expr(d, &scope)?,
                None => Value::Undef,
            },
        };
        scope.bind(p.name.clone(), v);
    }
    eval_expr(&body, &scope)
}

#[test]
fn fast_equals_slow_fab_poc_near0() {
    // The const-guard POC's correctness half: with `_EPSILON` bound to the guarded 1e-9 (the only state
    // the intrinsic ever arms under), native must bit-match the interpreter over the whole battery plus
    // the near-epsilon edges (strictly-less, exactly-equal, just-above).
    let reference = reference_of("_fab_poc_near0").expect("POC registered");
    let eps = [("_EPSILON", Value::Num(1e-9))];
    let mut inputs = value_battery();
    inputs.extend([5e-10, 1e-9, 2e-9, -5e-10, -1e-9].map(Value::Num));
    for v in inputs {
        let args = [v.clone()];
        assert!(
            same_result(
                &super::poc_near0(&crate::surface::NoClosures, &args),
                &interpret_with_consts(reference, &eps, &args)
            ),
            "intrinsic vs interpreter diverged at {v:?}"
        );
    }
}

/// The full oracle: deps AND top-level consts — a reference whose DEFAULT reads `_EPSILON` (approx,
/// `is_vector`…) needs the constant bound BEFORE params bind, exactly like the real definition scope (the
/// island global) provides it. Same clear-intrinsics contract as [`interpret_with_deps`].
fn interpret_with_deps_consts(
    target: &str,
    deps: &[&str],
    consts: &[(&str, Value)],
    inputs: &[Value],
) -> crate::Result<Value> {
    let src = format!("{}\n{target}", deps.join("\n"));
    let program = parse(&src).expect("deps+target parse");
    let mut ctx = build_ctx(&program, crate::Config::default());
    ctx.intrinsics.clear();
    let (params, body) = match &program.stmts.last().expect("has target").kind {
        StmtKind::FunctionDef { params, body, .. } => (params, body),
        other => panic!("target is not a function def: {other:?}"),
    };
    let mut scope = Scope::new();
    for (name, v) in consts {
        scope.bind((*name).to_string(), v.clone());
    }
    // PUBLISH the consts as island 0's global too — a DEP's defaults (approx's `eps=_EPSILON` when
    // posmod calls it) evaluate against the callee's home-island global, not the caller's scope. In a
    // real program both are the same hoisted global; the oracle must mirror that or a dep's default
    // silently reads undef (caught by the posmod battery).
    if let Some(slot) = ctx.island_globals.borrow_mut().first_mut() {
        *slot = scope.clone();
    }
    // The consts-only snapshot IS the lexical base: the target's own defaults evaluate there
    // (`push_call` rule), never against the params bound so far.
    let base = scope.clone();
    for (i, p) in params.iter().enumerate() {
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => crate::eval::eval_with_ctx(d, &base, &ctx)?,
                None => Value::Undef,
            },
        };
        scope.bind(p.name.clone(), v);
    }
    crate::eval::eval_with_ctx(body, &scope, &ctx)
}

/// The shape band's richer battery: everything in [`value_battery`] plus the nested/mixed/undef-bearing
/// shapes `_list_pattern`/`is_consistent`/`same_shape` actually discriminate on.
fn shape_battery() -> Vec<Value> {
    let mut b = value_battery();
    b.extend([
        Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![3.0, 4.0]),
        ]),
        Value::list(vec![
            Value::num_list(vec![1.0]),
            Value::list(vec![Value::Num(2.0), Value::string("a")]),
        ]),
        Value::list(vec![Value::Num(1.0), Value::num_list(vec![2.0])]),
        Value::list(vec![Value::Undef, Value::Num(1.0), Value::Undef]),
        Value::list(vec![Value::string("x"), Value::string("y")]),
        Value::list(vec![Value::list(vec![])]),
        Value::num_list(vec![0.0, -0.0]),
    ]);
    b
}

#[test]
fn fast_equals_slow_shape_band() {
    // The O.5.2 shape band, whole-battery: 1-arg fns over every battery value, 2-arg fns over every
    // PAIR (shape comparisons are about how two inputs relate). interpret_with_deps supplies the
    // recursive/dep definitions; deps=[] still resolves self-recursion (build_ctx sees the target).
    let battery = shape_battery();
    let lp_ref = reference_of("_list_pattern").unwrap();
    for v in &battery {
        let args = [v.clone()];
        assert!(
            same_result(
                &super::list_pattern(&crate::surface::NoClosures, &args),
                &interpret_with_deps(lp_ref, &[], &args)
            ),
            "_list_pattern diverged on {v:?}"
        );
        let nd_ref = reference_of("num_defined").unwrap();
        assert!(
            same_result(
                &super::num_defined(&crate::surface::NoClosures, &args),
                &interpret_with_deps(nd_ref, &[], &args)
            ),
            "num_defined diverged on {v:?}"
        );
    }
    let ss_ref = reference_of("same_shape").unwrap();
    let ss_deps = [reference_of("is_def").unwrap(), lp_ref];
    let ic_ref = reference_of("is_consistent").unwrap();
    for a in &battery {
        for b in &battery {
            let args = [a.clone(), b.clone()];
            assert!(
                same_result(
                    &super::same_shape(&crate::surface::NoClosures, &args),
                    &interpret_with_deps(ss_ref, &ss_deps, &args)
                ),
                "same_shape diverged on ({a:?}, {b:?})"
            );
            assert!(
                same_result(
                    &super::is_consistent(&args),
                    &interpret_with_deps(ic_ref, &[lp_ref], &args)
                ),
                "is_consistent diverged on ({a:?}, {b:?})"
            );
        }
        // the 1-arg form (pattern defaults to list[0]'s shape) — the overwhelmingly common call
        let args = [a.clone()];
        assert!(
            same_result(
                &super::is_consistent(&args),
                &interpret_with_deps(ic_ref, &[lp_ref], &args)
            ),
            "is_consistent/1 diverged on {a:?}"
        );
    }
    let fl_ref = reference_of("force_list").unwrap();
    let ns = [
        Value::Undef,
        Value::Num(0.0),
        Value::Num(1.0),
        Value::Num(3.0),
        Value::Num(-1.0),
        Value::Num(2.5),
        Value::string("x"),
    ];
    let fills = [Value::Undef, Value::Num(7.0), Value::string("f")];
    for v in &battery {
        for n in &ns {
            for fill in &fills {
                let args = [v.clone(), n.clone(), fill.clone()];
                assert!(
                    same_result(
                        &super::force_list(&args),
                        &interpret_with_deps(fl_ref, &[], &args)
                    ),
                    "force_list diverged on ({v:?}, {n:?}, {fill:?})"
                );
            }
        }
        let args = [v.clone()]; // defaults: n=1, fill undef
        assert!(
            same_result(
                &super::force_list(&args),
                &interpret_with_deps(fl_ref, &[], &args)
            ),
            "force_list/1 diverged on {v:?}"
        );
    }
}

/// The `_EPSILON` family's battery: numeric edges around the 1e-9 tolerance, vectors with NaN/inf
/// poison, near-zero vectors, plus every non-vector shape from the base battery.
fn eps_battery() -> Vec<Value> {
    let mut b = shape_battery();
    b.extend([1e-10, -1e-10, 1e-9, 2e-9, 1.0 + 1e-10, 0.5, -2.5, 1e12].map(Value::Num));
    b.extend([
        Value::num_list(vec![0.0, 0.0]),
        Value::num_list(vec![1e-10, 1.0]),
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::num_list(vec![1.0, f64::NAN]),
        Value::num_list(vec![1.0, f64::INFINITY]),
        Value::list(vec![Value::Num(1.0), Value::string("a")]),
    ]);
    b
}

#[test]
fn fast_equals_slow_epsilon_family() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let battery = eps_battery();
    let refs = |names: &[&str]| -> Vec<&'static str> {
        names.iter().map(|n| reference_of(n).expect(n)).collect()
    };
    let epses = [
        None,
        Some(Value::Num(1e-9)),
        Some(Value::Num(0.5)),
        Some(Value::Undef),
        Some(Value::string("x")),
    ];

    // approx(a,b[,eps]) — every pair × every eps shape (the recursion + NaN routing live here).
    let approx_ref = reference_of("approx").unwrap();
    let approx_deps = refs(&["idx", "posmod", "is_finite", "is_nan"]);
    for a in &battery {
        for b in &battery {
            for eps in &epses {
                let mut args = vec![a.clone(), b.clone()];
                if let Some(e) = eps {
                    args.push(e.clone());
                }
                assert!(
                    same_result(
                        &super::approx(&crate::surface::NoClosures, &args),
                        &interpret_with_deps_consts(approx_ref, &approx_deps, &consts, &args)
                    ),
                    "approx diverged on ({a:?}, {b:?}, eps {eps:?})"
                );
            }
        }
    }

    // posmod(x,m) — the assert-heavy one: both raise-sites and the wrap arithmetic.
    let posmod_ref = reference_of("posmod").unwrap();
    let posmod_deps = refs(&["is_finite", "is_nan", "approx", "idx"]);
    let nums = [
        Value::Num(0.0),
        Value::Num(-0.0),
        Value::Num(1e-10),
        Value::Num(-1e-10),
        Value::Num(5.0),
        Value::Num(-5.0),
        Value::Num(2.5),
        Value::Num(-7.25),
        Value::Num(f64::INFINITY),
        Value::Num(f64::NAN),
        Value::Undef,
        Value::string("m"),
        Value::num_list(vec![1.0]),
    ];
    for x in &nums {
        for m in &nums {
            let args = [x.clone(), m.clone()];
            assert!(
                same_result(
                    &super::posmod(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(posmod_ref, &posmod_deps, &consts, &args)
                ),
                "posmod diverged on ({x:?}, {m:?})"
            );
        }
    }

    // idx(list[,s,e,step]) — range identity (bit_eq compares Range fields) + the two raise-sites.
    let idx_ref = reference_of("idx").unwrap();
    let idx_deps = refs(&["posmod", "is_finite", "is_nan", "approx"]);
    let arg_sets: Vec<Vec<Value>> = vec![
        vec![],
        vec![Value::Num(1.0)],
        vec![Value::Num(1.0), Value::Num(-2.0)],
        vec![Value::Num(0.0), Value::Num(-1.0), Value::Num(2.0)],
        vec![Value::string("s")],
        vec![Value::Undef],
    ];
    for v in &battery {
        for tail in &arg_sets {
            let mut args = vec![v.clone()];
            args.extend(tail.iter().cloned());
            assert!(
                same_result(
                    &super::idx(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(idx_ref, &idx_deps, &consts, &args)
                ),
                "idx diverged on ({v:?}, tail {tail:?})"
            );
        }
    }

    // all_nonzero(x[,eps]).
    let anz_ref = reference_of("all_nonzero").unwrap();
    let anz_deps = refs(&["is_finite", "is_nan", "is_vector"]);
    for v in &battery {
        for eps in &epses {
            let mut args = vec![v.clone()];
            if let Some(e) = eps {
                args.push(e.clone());
            }
            assert!(
                same_result(
                    &super::all_nonzero(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(anz_ref, &anz_deps, &consts, &args)
                ),
                "all_nonzero diverged on ({v:?}, eps {eps:?})"
            );
        }
    }

    // is_vector(v[,length,zero,all_nonzero,eps]) — clause-by-clause arg shapes over the battery.
    let iv_ref = reference_of("is_vector").unwrap();
    let iv_deps = refs(&["is_finite", "is_nan", "all_nonzero"]);
    let lengths = [
        Value::Undef,
        Value::Num(2.0),
        Value::Num(3.0),
        Value::string("L"),
        Value::Num(f64::NAN),
    ];
    let zeros = [Value::Undef, Value::Bool(true), Value::Bool(false)];
    let anzs = [Value::Bool(false), Value::Bool(true)];
    for v in &battery {
        for length in &lengths {
            let args = [v.clone(), length.clone()];
            assert!(
                same_result(
                    &super::is_vector(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                ),
                "is_vector diverged on ({v:?}, length {length:?})"
            );
        }
        for zero in &zeros {
            for eps in [Value::Num(1e-9), Value::Num(0.5), Value::Undef] {
                let args = [
                    v.clone(),
                    Value::Undef,
                    zero.clone(),
                    Value::Bool(false),
                    eps.clone(),
                ];
                assert!(
                    same_result(
                        &super::is_vector(&crate::surface::NoClosures, &args),
                        &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                    ),
                    "is_vector diverged on ({v:?}, zero {zero:?}, eps {eps:?})"
                );
            }
        }
        for anz in &anzs {
            let args = [v.clone(), Value::Undef, Value::Undef, anz.clone()];
            assert!(
                same_result(
                    &super::is_vector(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                ),
                "is_vector diverged on ({v:?}, all_nonzero {anz:?})"
            );
        }
    }

    // is_matrix(A[,m,n,square]).
    let im_ref = reference_of("is_matrix").unwrap();
    let im_deps = refs(&[
        "is_vector",
        "is_finite",
        "is_nan",
        "is_consistent",
        "_list_pattern",
    ]);
    let mut mats = battery.clone();
    mats.extend([
        Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![3.0, 4.0]),
        ]),
        Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![3.0]),
        ]),
        Value::list(vec![
            Value::num_list(vec![1.0, 2.0, 5.0]),
            Value::num_list(vec![3.0, 4.0, 6.0]),
        ]),
    ]);
    let ms = [Value::Undef, Value::Num(2.0), Value::Num(3.0)];
    let ns = [Value::Undef, Value::Num(2.0), Value::string("n")];
    let squares = [Value::Bool(false), Value::Bool(true)];
    for a in &mats {
        for m in &ms {
            for n in &ns {
                for square in &squares {
                    let args = [a.clone(), m.clone(), n.clone(), square.clone()];
                    assert!(
                        same_result(
                            &super::is_matrix(&crate::surface::NoClosures, &args),
                            &interpret_with_deps_consts(im_ref, &im_deps, &consts, &args)
                        ),
                        "is_matrix diverged on ({a:?}, m {m:?}, n {n:?}, square {square:?})"
                    );
                }
            }
        }
    }
}

/// A 2D point as the interpreter builds it.
fn p2(x: f64, y: f64) -> Value {
    Value::num_list(vec![x, y])
}

#[test]
fn fast_equals_slow_earcut_band() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let tc_ref = reference_of("_tri_class").unwrap();
    let al_ref = reference_of("_is_at_left").unwrap();
    let ni_ref = reference_of("_none_inside").unwrap();
    let al_deps = [tc_ref];
    let ni_deps = [
        reference_of("select").unwrap(),
        tc_ref,
        al_ref,
        reference_of("is_vector").unwrap(),
        pin_reference_of("is_range").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
    ];

    // _tri_class: CW / CCW / collinear / near-collinear-within-eps triangles, 3D points (→ undef),
    // degenerate shapes, exotic eps.
    let tris = [
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(1.0, 0.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 1.0), p2(2.0, 2.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 1e-12), p2(2.0, 0.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 1e-3), p2(2.0, 0.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(0.0, 0.0), p2(1.0, 1.0)]),
        Value::list(vec![
            Value::num_list(vec![0.0, 0.0, 0.0]),
            Value::num_list(vec![1.0, 0.0, 0.0]),
            Value::num_list(vec![0.0, 1.0, 0.0]),
        ]),
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0)]),
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::Undef,
        Value::string("tri"),
        Value::list(vec![p2(f64::NAN, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
        Value::list(vec![p2(f64::INFINITY, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
    ];
    let epses = [
        None,
        Some(Value::Num(1e-9)),
        Some(Value::Num(0.1)),
        Some(Value::Undef),
        Some(Value::string("e")),
    ];
    for tri in &tris {
        for eps in &epses {
            let mut args = vec![tri.clone()];
            if let Some(e) = eps {
                args.push(e.clone());
            }
            assert!(
                same_result(
                    &super::tri_class(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(tc_ref, &[], &consts, &args)
                ),
                "_tri_class diverged on ({tri:?}, eps {eps:?})"
            );
        }
    }

    // _is_at_left: points against directed segments, incl. on-the-line and exotic shapes.
    let pts = [
        p2(0.0, 1.0),
        p2(0.0, -1.0),
        p2(0.5, 0.0),
        p2(f64::NAN, 0.0),
        Value::Undef,
        Value::Num(3.0),
    ];
    let lines = [
        Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0)]),
        Value::list(vec![p2(1.0, 0.0), p2(0.0, 0.0)]),
        Value::list(vec![p2(0.0, 0.0), p2(0.0, 0.0)]),
        Value::list(vec![p2(0.0, 0.0)]),
        Value::Undef,
    ];
    for pt in &pts {
        for line in &lines {
            for eps in &epses {
                let mut args = vec![pt.clone(), line.clone()];
                if let Some(e) = eps {
                    args.push(e.clone());
                }
                assert!(
                    same_result(
                        &super::is_at_left(&crate::surface::NoClosures, &args),
                        &interpret_with_deps_consts(al_ref, &al_deps, &consts, &args)
                    ),
                    "_is_at_left diverged on ({pt:?}, {line:?}, eps {eps:?})"
                );
            }
        }
    }

    // _none_inside: real ear-scan shapes over a CW L-polygon (concave), incl. an ear a reflex vertex
    // blocks, a duplicate-vertex polygon (the norm(vert-p1)<eps arm), the i-offset start, and the
    // exotic-input raise paths (non-list idxs / NaN i → select's asserts fire on BOTH sides).
    let lpoly = Value::list(vec![
        p2(0.0, 0.0),
        p2(0.0, 2.0),
        p2(1.0, 2.0),
        p2(1.0, 1.0),
        p2(2.0, 1.0),
        p2(2.0, 0.0),
    ]);
    let sq = Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(1.0, 1.0), p2(1.0, 0.0)]);
    let dup = Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(0.0, 1.0), p2(1.0, 0.0)]);
    let all6 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    let all4 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0]);
    let e9 = Value::Num(1e-9);
    let cases: Vec<Vec<Value>> = vec![
        // (idxs, poly, p0, p1, p2, eps[, i])
        vec![
            all6.clone(),
            lpoly.clone(),
            p2(0.0, 0.0),
            p2(0.0, 2.0),
            p2(1.0, 2.0),
            e9.clone(),
        ],
        vec![
            all6.clone(),
            lpoly.clone(),
            p2(1.0, 2.0),
            p2(1.0, 1.0),
            p2(2.0, 1.0),
            e9.clone(),
        ],
        vec![
            all6.clone(),
            lpoly.clone(),
            p2(2.0, 1.0),
            p2(2.0, 0.0),
            p2(0.0, 0.0),
            e9.clone(),
        ],
        vec![
            all4.clone(),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            e9.clone(),
        ],
        vec![
            all4.clone(),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            e9.clone(),
            Value::Num(2.0),
        ],
        vec![
            all4.clone(),
            dup.clone(),
            p2(0.0, 1.0),
            p2(0.0, 1.0),
            p2(1.0, 0.0),
            e9.clone(),
        ],
        vec![
            Value::num_list(vec![]),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            e9.clone(),
        ],
        // exotic: eps undef, idxs non-list (select raises), i NaN (select raises)
        vec![
            all4.clone(),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            Value::Undef,
        ],
        vec![
            Value::Num(7.0),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            e9.clone(),
        ],
        vec![
            all4.clone(),
            sq.clone(),
            p2(0.0, 0.0),
            p2(0.0, 1.0),
            p2(1.0, 1.0),
            e9.clone(),
            Value::Num(f64::NAN),
        ],
        // 3D polygon: every tri_class degrades to undef exactly as interpreted
        vec![
            Value::num_list(vec![0.0, 1.0, 2.0]),
            Value::list(vec![
                Value::num_list(vec![0.0, 0.0, 0.0]),
                Value::num_list(vec![1.0, 0.0, 0.0]),
                Value::num_list(vec![0.0, 1.0, 0.0]),
            ]),
            Value::num_list(vec![0.0, 0.0, 0.0]),
            Value::num_list(vec![1.0, 0.0, 0.0]),
            Value::num_list(vec![0.0, 1.0, 0.0]),
            e9.clone(),
        ],
    ];
    for args in &cases {
        assert!(
            same_result(
                &super::none_inside(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(ni_ref, &ni_deps, &consts, args)
            ),
            "_none_inside diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_aggregate_band() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let shape_deps = [
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("is_vector").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];

    // _sum / sum — scalars, vectors, matrices (the _sum lane), inconsistent (raise), empty (dflt).
    let sum_ref = reference_of("sum").unwrap();
    let sum_deps: Vec<&str> = shape_deps
        .iter()
        .copied()
        .chain([reference_of("_sum").unwrap()])
        .collect();
    let st_ref = reference_of("_sum").unwrap();
    let m22 = Value::list(vec![
        Value::num_list(vec![1.0, 2.0]),
        Value::num_list(vec![3.0, 4.0]),
    ]);
    let sums = [
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::num_list(vec![0.5]),
        Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![10.0, 20.0]),
        ]),
        Value::list(vec![m22.clone(), m22.clone()]),
        Value::list(vec![]),
        Value::list(vec![Value::Num(1.0), Value::string("x")]),
        Value::num_list(vec![f64::NAN, 1.0]),
        Value::Num(7.0),
        Value::Undef,
    ];
    for v in &sums {
        for dflt in [None, Some(Value::Num(9.0)), Some(Value::string("d"))] {
            let mut args = vec![v.clone()];
            if let Some(d) = &dflt {
                args.push(d.clone());
            }
            assert!(
                same_result(
                    &super::sum(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(sum_ref, &sum_deps, &consts, &args)
                ),
                "sum diverged on ({v:?}, dflt {dflt:?})"
            );
        }
        // a non-list v makes the reference recurse forever (len(v) is undef) — the oracle would HANG,
        // so those inputs are asserted native-side only below.
        if matches!(v, Value::List(_) | Value::NumList(_)) {
            let args = [v.clone(), Value::Num(0.0)];
            assert!(
                same_result(
                    &super::sum_tail(&args),
                    &interpret_with_deps_consts(st_ref, &[], &consts, &args)
                ),
                "_sum diverged on {v:?}"
            );
        }
    }
    // the non-terminating shapes: LOUD Err, never a hang (the interpreter only stops at its budget).
    assert!(super::sum_tail(&[Value::Num(7.0), Value::Num(0.0)]).is_err());
    assert!(super::sum_tail(&[Value::Undef, Value::Num(0.0)]).is_err());
    assert!(
        super::sum_tail(&[
            Value::num_list(vec![1.0]),
            Value::Num(0.0),
            Value::Num(f64::NEG_INFINITY)
        ])
        .is_err()
    );

    // unit — ordinary, near-zero (default raise vs custom error value), non-vector raise, List-shaped.
    let unit_ref = reference_of("unit").unwrap();
    let unit_deps = [
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];
    let units = [
        Value::num_list(vec![3.0, 4.0]),
        Value::num_list(vec![0.0, 0.0]),
        Value::num_list(vec![1e-10, 0.0]),
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::Num(5.0),
        Value::Undef,
        Value::list(vec![Value::Num(1.0), Value::string("x")]),
    ];
    for v in &units {
        for err in [None, Some(Value::Num(-7.0)), Some(Value::Undef)] {
            let mut args = vec![v.clone()];
            if let Some(e) = &err {
                args.push(e.clone());
            }
            assert!(
                same_result(
                    &super::unit(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(unit_ref, &unit_deps, &consts, &args)
                ),
                "unit diverged on ({v:?}, error {err:?})"
            );
        }
    }

    // is_2d_transform / _apply — real affine matrices (2D-in-3D, translation, scale, zscale), the
    // 2D-points-under-3D-transform lane, and the raise paths.
    let i2t_ref = reference_of("is_2d_transform").unwrap();
    let ap_ref = reference_of("_apply").unwrap();
    let ap_deps: Vec<&str> = shape_deps
        .iter()
        .copied()
        .chain([reference_of("is_matrix").unwrap(), i2t_ref])
        .collect();
    let mat4 = |rows: [[f64; 4]; 4]| {
        let rows: Vec<Value> = rows.iter().map(|r| Value::num_list(r.to_vec())).collect();
        Value::list(rows)
    };
    let ident = mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let translate = mat4([
        [1.0, 0.0, 0.0, 5.0],
        [0.0, 1.0, 0.0, -3.0],
        [0.0, 0.0, 1.0, 2.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let zscale = mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 4.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let scale2 = mat4([
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 3.0, 0.0, 0.0],
        [0.0, 0.0, 4.0, 0.0],
        [0.0, 0.0, 0.0, 2.0],
    ]);
    let rot2d = mat4([
        [0.0, -1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let mats = [
        ident.clone(),
        translate.clone(),
        zscale.clone(),
        scale2.clone(),
        rot2d.clone(),
        m22.clone(),
        Value::Undef,
    ];
    for t in &mats {
        let args = [t.clone()];
        assert!(
            same_result(
                &super::is_2d_transform(&args),
                &interpret_with_deps_consts(i2t_ref, &[], &consts, &args)
            ),
            "is_2d_transform diverged on {t:?}"
        );
    }
    let pts3 = Value::list(vec![
        Value::num_list(vec![1.0, 2.0, 3.0]),
        Value::num_list(vec![-1.0, 0.5, 0.0]),
    ]);
    let pts2 = Value::list(vec![
        Value::num_list(vec![1.0, 2.0]),
        Value::num_list(vec![-1.0, 0.5]),
    ]);
    for t in &mats {
        for p in [&pts3, &pts2, &m22, &Value::Undef] {
            let args = [t.clone(), p.clone()];
            assert!(
                same_result(
                    &super::apply_transform(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(ap_ref, &ap_deps, &consts, &args)
                ),
                "_apply diverged on ({t:?}, {p:?})"
            );
        }
    }

    // _bt_search — a real 2-level tree over five 2D points, radii that hit the prune / root-hit / leaf
    // lanes, plus the malformed-tree raises.
    let bt_ref = reference_of("_bt_search").unwrap();
    let bt_deps = [
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];
    let points = Value::list(vec![
        p2(0.0, 0.0),
        p2(1.0, 0.0),
        p2(0.0, 1.0),
        p2(5.0, 5.0),
        p2(5.2, 5.0),
    ]);
    // node: [pivot_idx, radius, left, right]; leaves carry index lists
    let leaf = |ids: &[f64]| Value::list(vec![Value::num_list(ids.to_vec())]);
    let tree = Value::list(vec![
        Value::Num(0.0),
        Value::Num(1.5),
        leaf(&[1.0, 2.0]),
        Value::list(vec![
            Value::Num(3.0),
            Value::Num(0.5),
            leaf(&[4.0]),
            leaf(&[]),
        ]),
    ]);
    let bt_cases: Vec<Vec<Value>> = vec![
        vec![p2(0.0, 0.0), Value::Num(1.1), points.clone(), tree.clone()],
        vec![p2(0.0, 0.0), Value::Num(0.1), points.clone(), tree.clone()],
        vec![p2(5.0, 5.0), Value::Num(0.5), points.clone(), tree.clone()],
        vec![p2(9.0, 9.0), Value::Num(0.1), points.clone(), tree.clone()],
        vec![
            p2(0.0, 0.0),
            Value::Num(1.1),
            points.clone(),
            leaf(&[0.0, 3.0]),
        ],
        vec![p2(0.0, 0.0), Value::Num(1.1), points.clone(), leaf(&[])],
        vec![
            p2(0.0, 0.0),
            Value::Num(1.1),
            points.clone(),
            Value::Num(7.0),
        ],
        vec![
            p2(0.0, 0.0),
            Value::Num(1.1),
            points.clone(),
            Value::list(vec![
                Value::Num(0.0),
                Value::Num(1.0),
                leaf(&[]),
                Value::Num(9.0),
            ]),
        ],
        vec![p2(0.0, 0.0), Value::Undef, points.clone(), tree.clone()],
    ];
    for args in &bt_cases {
        assert!(
            same_result(
                &super::bt_search(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(bt_ref, &bt_deps, &consts, args)
            ),
            "_bt_search diverged on {args:?}"
        );
    }

    // vector_angle — two-vector, three-point, paired-list, and the assert lanes (mismatched shapes,
    // zero-length, scalar input); the acos-domain clamp edge via antiparallel vectors.
    let va_ref = reference_of("vector_angle").unwrap();
    let va_deps: Vec<&str> = shape_deps
        .iter()
        .copied()
        .chain([
            reference_of("same_shape").unwrap(),
            reference_of("is_def").unwrap(),
            reference_of("is_matrix").unwrap(),
            pin_reference_of("constrain").unwrap(),
        ])
        .collect();
    let va_cases: Vec<Vec<Value>> = vec![
        vec![p2(1.0, 0.0), p2(0.0, 1.0)],
        vec![p2(1.0, 0.0), p2(-1.0, 0.0)],
        vec![p2(1.0, 0.0), p2(1.0, 0.0)],
        vec![
            Value::num_list(vec![1.0, 0.0, 0.0]),
            Value::num_list(vec![0.0, 0.0, 1.0]),
        ],
        vec![p2(1.0, 0.0), p2(0.0, 1.0), p2(1.0, 1.0)],
        vec![Value::list(vec![p2(1.0, 0.0), p2(0.0, 1.0)])],
        vec![Value::list(vec![p2(0.0, 2.0), p2(0.0, 0.0), p2(2.0, 0.0)])],
        vec![p2(1.0, 0.0), Value::num_list(vec![1.0, 0.0, 0.0])],
        vec![p2(0.0, 0.0), p2(1.0, 0.0)],
        vec![Value::Num(3.0)],
        vec![Value::Undef],
    ];
    for args in &va_cases {
        assert!(
            same_result(
                &super::generated::vector_angle(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(va_ref, &va_deps, &consts, args)
            ),
            "vector_angle diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_band5_batch1() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let select_knot = [
        reference_of("select").unwrap(),
        reference_of("is_vector").unwrap(),
        pin_reference_of("is_range").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];

    // _point_dist — a real segment chain (precomputed unit/len like offset() passes), the three
    // segdist lanes (behind / beyond / perpendicular), plus degenerate shapes.
    let pd_ref = reference_of("_point_dist").unwrap();
    let path = Value::list(vec![p2(0.0, 0.0), p2(2.0, 0.0), p2(2.0, 2.0)]);
    let units = Value::list(vec![p2(1.0, 0.0), p2(0.0, 1.0)]);
    let lens = Value::num_list(vec![2.0, 2.0]);
    let pd_cases: Vec<Vec<Value>> = vec![
        vec![path.clone(), units.clone(), lens.clone(), p2(1.0, 1.0)],
        vec![path.clone(), units.clone(), lens.clone(), p2(-1.0, -1.0)],
        vec![path.clone(), units.clone(), lens.clone(), p2(5.0, 5.0)],
        vec![path.clone(), units.clone(), lens.clone(), p2(2.0, 1.0)],
        vec![
            path.clone(),
            Value::list(vec![]),
            Value::num_list(vec![]),
            p2(0.0, 0.0),
        ],
        vec![Value::Undef, units.clone(), lens.clone(), p2(0.0, 0.0)],
        vec![path.clone(), units.clone(), lens.clone(), Value::Undef],
    ];
    for args in &pd_cases {
        assert!(
            same_result(
                &super::point_dist(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(pd_ref, &select_knot, &consts, args)
            ),
            "_point_dist diverged on {args:?}"
        );
    }

    // _is_point_on_line — on/off the line in 2D and 3D, each bounded mode, exotic shapes.
    let ipol_ref = reference_of("_is_point_on_line").unwrap();
    let ipol_deps = [reference_of("force_list").unwrap()];
    let line2 = Value::list(vec![p2(0.0, 0.0), p2(2.0, 0.0)]);
    let line3 = Value::list(vec![
        Value::num_list(vec![0.0, 0.0, 0.0]),
        Value::num_list(vec![0.0, 0.0, 2.0]),
    ]);
    let bounds = [
        None,
        Some(Value::Bool(true)),
        Some(Value::list(vec![Value::Bool(true), Value::Bool(false)])),
    ];
    let ipol_pts = [
        (p2(1.0, 0.0), line2.clone()),
        (p2(-1.0, 0.0), line2.clone()),
        (p2(3.0, 0.0), line2.clone()),
        (p2(1.0, 0.5), line2.clone()),
        (p2(1.0, 1e-12), line2.clone()),
        (Value::num_list(vec![0.0, 0.0, 1.0]), line3.clone()),
        (Value::num_list(vec![1.0, 0.0, 1.0]), line3.clone()),
        (Value::Undef, line2.clone()),
        (p2(1.0, 0.0), Value::Undef),
    ];
    for (pt, line) in &ipol_pts {
        for b in &bounds {
            let mut args = vec![pt.clone(), line.clone()];
            if let Some(b) = b {
                args.push(b.clone());
            }
            assert!(
                same_result(
                    &super::is_point_on_line(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(ipol_ref, &ipol_deps, &consts, &args)
                ),
                "_is_point_on_line diverged on ({pt:?}, {line:?}, {b:?})"
            );
        }
    }

    // _vnf_centroid — a unit cube VNF (quad faces exercise the fan j-loop), a tet, empty/invalid
    // raises, and a degenerate (zero-volume) self-intersection raise.
    let vc_ref = reference_of("_vnf_centroid").unwrap();
    let vc_deps = [
        pin_reference_of("is_vnf").unwrap(),
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
        reference_of("sum").unwrap(),
        reference_of("_sum").unwrap(),
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("approx").unwrap(),
        reference_of("idx").unwrap(),
        reference_of("posmod").unwrap(),
    ];
    let p3 = |x: f64, y: f64, z: f64| Value::num_list(vec![x, y, z]);
    let f = |ids: &[f64]| Value::num_list(ids.to_vec());
    let cube = Value::list(vec![
        Value::list(vec![
            p3(0.0, 0.0, 0.0),
            p3(1.0, 0.0, 0.0),
            p3(1.0, 1.0, 0.0),
            p3(0.0, 1.0, 0.0),
            p3(0.0, 0.0, 1.0),
            p3(1.0, 0.0, 1.0),
            p3(1.0, 1.0, 1.0),
            p3(0.0, 1.0, 1.0),
        ]),
        Value::list(vec![
            f(&[0.0, 3.0, 2.0, 1.0]),
            f(&[4.0, 5.0, 6.0, 7.0]),
            f(&[0.0, 1.0, 5.0, 4.0]),
            f(&[1.0, 2.0, 6.0, 5.0]),
            f(&[2.0, 3.0, 7.0, 6.0]),
            f(&[3.0, 0.0, 4.0, 7.0]),
        ]),
    ]);
    let tet = Value::list(vec![
        Value::list(vec![
            p3(0.0, 0.0, 0.0),
            p3(1.0, 0.0, 0.0),
            p3(0.0, 1.0, 0.0),
            p3(0.0, 0.0, 1.0),
        ]),
        Value::list(vec![
            f(&[0.0, 2.0, 1.0]),
            f(&[0.0, 1.0, 3.0]),
            f(&[1.0, 2.0, 3.0]),
            f(&[0.0, 3.0, 2.0]),
        ]),
    ]);
    // one open face only → summed signed volume ≈ 0 → the self-intersection assert raises
    let flat = Value::list(vec![
        Value::list(vec![
            p3(0.0, 0.0, 0.0),
            p3(1.0, 0.0, 0.0),
            p3(0.0, 1.0, 0.0),
        ]),
        Value::list(vec![f(&[0.0, 1.0, 2.0])]),
    ]);
    let vc_cases = [
        cube,
        tet,
        flat,
        Value::list(vec![Value::list(vec![]), Value::list(vec![])]),
        Value::Undef,
        Value::Num(3.0),
    ];
    for vnf in &vc_cases {
        let args = [vnf.clone()];
        assert!(
            same_result(
                &super::vnf_centroid(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(vc_ref, &vc_deps, &consts, &args)
            ),
            "_vnf_centroid diverged on {vnf:?}"
        );
    }

    // _group_sort_by_index — grouping, ordering, NaN/mixed-type key drops, empty/single/scalar.
    let gs_ref = reference_of("_group_sort_by_index").unwrap();
    let rows = |ks: &[f64]| {
        let v: Vec<Value> = ks
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                #[allow(clippy::cast_precision_loss, reason = "tiny test indices")]
                Value::list(vec![Value::Num(k), Value::Num(i as f64)])
            })
            .collect();
        Value::list(v)
    };
    let gs_cases: Vec<Vec<Value>> = vec![
        vec![rows(&[3.0, 1.0, 2.0, 1.0, 3.0]), Value::Num(0.0)],
        vec![rows(&[1.0, 1.0, 1.0]), Value::Num(0.0)],
        vec![rows(&[5.0, 4.0, 3.0, 2.0, 1.0]), Value::Num(0.0)],
        vec![rows(&[1.0, 2.0, 3.0, 4.0, 5.0]), Value::Num(0.0)],
        vec![rows(&[2.0, f64::NAN, 1.0]), Value::Num(0.0)],
        vec![rows(&[1.0]), Value::Num(0.0)],
        vec![Value::list(vec![]), Value::Num(0.0)],
        vec![
            Value::list(vec![
                Value::list(vec![Value::Num(1.0)]),
                Value::list(vec![Value::string("a")]),
                Value::list(vec![Value::Num(0.0)]),
            ]),
            Value::Num(0.0),
        ],
        vec![Value::Num(5.0), Value::Num(0.0)],
        vec![rows(&[2.0, 1.0]), Value::Undef],
    ];
    for args in &gs_cases {
        assert!(
            same_result(
                &super::group_sort_by_index(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(gs_ref, &[], &consts, args)
            ),
            "_group_sort_by_index diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_band5_batch2() {
    let consts = [("_EPSILON", Value::Num(1e-9))];

    // ident / the axis rotations — sizes, angle values incl. the snap-relevant right angles, raises.
    let id_ref = reference_of("ident").unwrap();
    for n in [
        Value::Num(0.0),
        Value::Num(1.0),
        Value::Num(3.0),
        Value::Num(4.0),
        Value::Num(2.5),
        Value::Undef,
        Value::string("n"),
    ] {
        let args = [n.clone()];
        assert!(
            same_result(
                &super::ident(&args),
                &interpret_with_deps_consts(id_ref, &[], &consts, &args)
            ),
            "ident diverged on {n:?}"
        );
    }
    let rot_deps = [
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
    ];
    let angles = [
        None,
        Some(Value::Num(0.0)),
        Some(Value::Num(90.0)),
        Some(Value::Num(-30.0)),
        Some(Value::Num(123.456)),
        Some(Value::Num(f64::NAN)),
        Some(Value::Undef),
    ];
    for (name, func) in [
        // The GENERATED versions — the hand ones are helpers on their way out (AR.17 stage A).
        (
            "affine3d_zrot",
            super::generated::affine3d_zrot as super::Intrinsic,
        ),
        ("affine3d_xrot", super::generated::affine3d_xrot),
        ("affine3d_yrot", super::generated::affine3d_yrot),
    ] {
        let r = reference_of(name).unwrap();
        for ang in &angles {
            let args: Vec<Value> = ang.iter().cloned().collect();
            assert!(
                same_result(
                    &func(&crate::surface::NoClosures, &args),
                    &interpret_with_deps_consts(r, &rot_deps, &consts, &args)
                ),
                "{name} diverged on {ang:?}"
            );
        }
    }

    // _get_ear — the concave L-polygon (has real ears at various _i), a triangle (immediate 0), a
    // whisker polygon (duplicate-adjacent vertices, no ears), and the raise/exotic lanes.
    let ge_ref = reference_of("_get_ear").unwrap();
    let ge_deps = [
        reference_of("_tri_class").unwrap(),
        reference_of("_none_inside").unwrap(),
        reference_of("_is_at_left").unwrap(),
        reference_of("select").unwrap(),
        reference_of("idx").unwrap(),
        reference_of("posmod").unwrap(),
        reference_of("approx").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("is_vector").unwrap(),
        pin_reference_of("is_range").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];
    // CW L-poly (BOSL2's earcut runs on CW): reversed order of the CCW L used in the earcut battery
    let lpoly_cw = Value::list(vec![
        p2(2.0, 0.0),
        p2(2.0, 1.0),
        p2(1.0, 1.0),
        p2(1.0, 2.0),
        p2(0.0, 2.0),
        p2(0.0, 0.0),
    ]);
    let tri_ind = Value::num_list(vec![0.0, 1.0, 2.0]);
    let all6 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    // a degenerate spike: b == d, so every candidate fails and the whisker lane fires
    let spike = Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(2.0, 0.0), p2(1.0, 0.0)]);
    let all4 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0]);
    let e9 = Value::Num(1e-9);
    let ge_cases: Vec<Vec<Value>> = vec![
        vec![lpoly_cw.clone(), all6.clone(), e9.clone()],
        vec![lpoly_cw.clone(), all6.clone(), e9.clone(), Value::Num(3.0)],
        vec![lpoly_cw.clone(), tri_ind.clone(), e9.clone()],
        vec![spike.clone(), all4.clone(), e9.clone()],
        vec![spike.clone(), all4.clone(), Value::Undef],
        vec![Value::Undef, all4.clone(), e9.clone()],
        vec![lpoly_cw.clone(), Value::Num(7.0), e9.clone()],
    ];
    for args in &ge_cases {
        assert!(
            same_result(
                &super::get_ear(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(ge_ref, &ge_deps, &consts, args)
            ),
            "_get_ear diverged on {args:?}"
        );
    }

    // in_list / is_path — hits, misses, idx-column lookups, the all-hits retry (a first hit that
    // doesn't match), raises, and is_path's dim/fast lanes.
    let il_ref = reference_of("in_list").unwrap();
    let il_deps = [
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("is_def").unwrap(),
    ];
    let nums = Value::num_list(vec![3.0, 5.0, 7.0]);
    let rows = Value::list(vec![
        Value::list(vec![Value::Num(1.0), Value::string("a")]),
        Value::list(vec![Value::Num(2.0), Value::string("b")]),
    ]);
    let il_cases: Vec<Vec<Value>> = vec![
        vec![Value::Num(5.0), nums.clone()],
        vec![Value::Num(4.0), nums.clone()],
        vec![Value::string("b"), rows.clone(), Value::Num(1.0)],
        vec![Value::string("c"), rows.clone(), Value::Num(1.0)],
        vec![Value::Num(2.0), rows.clone(), Value::Num(0.0)],
        vec![Value::string("a"), rows.clone()],
        vec![Value::Num(1.0), Value::Num(9.0)],
        vec![Value::Num(1.0), nums.clone(), Value::string("i")],
        vec![Value::Undef, nums.clone()],
    ];
    for args in &il_cases {
        assert!(
            same_result(
                &super::in_list(args),
                &interpret_with_deps_consts(il_ref, &il_deps, &consts, args)
            ),
            "in_list diverged on {args:?}"
        );
    }
    let ip_ref = reference_of("is_path").unwrap();
    let ip_deps: Vec<&str> = il_deps
        .iter()
        .copied()
        .chain([
            reference_of("is_matrix").unwrap(),
            reference_of("is_vector").unwrap(),
            reference_of("is_consistent").unwrap(),
            reference_of("_list_pattern").unwrap(),
            reference_of("in_list").unwrap(),
            reference_of("force_list").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ])
        .collect();
    let path2 = Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(1.0, 1.0)]);
    let path4 = Value::list(vec![
        Value::num_list(vec![0.0, 0.0, 0.0, 0.0]),
        Value::num_list(vec![1.0, 0.0, 0.0, 0.0]),
    ]);
    let ip_cases: Vec<Vec<Value>> = vec![
        vec![path2.clone()],
        vec![path4.clone()],
        vec![path4.clone(), Value::Num(4.0)],
        vec![path2.clone(), Value::Undef],
        vec![path2.clone(), Value::num_list(vec![3.0])],
        vec![
            path2.clone(),
            Value::num_list(vec![2.0, 3.0]),
            Value::Bool(true),
        ],
        vec![
            Value::Num(5.0),
            Value::num_list(vec![2.0, 3.0]),
            Value::Bool(true),
        ],
        vec![Value::list(vec![p2(0.0, 0.0)])],
        vec![Value::Undef],
    ];
    for args in &ip_cases {
        assert!(
            same_result(
                &super::is_path(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(ip_ref, &ip_deps, &consts, args)
            ),
            "is_path diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_fab_poc_isup() {
    // The Value-const POC's correctness half: with `UP` bound to the baked [0,0,1] (the only state the
    // intrinsic ever arms under), native must bit-match the interpreter over the battery plus the
    // exact/near-miss vectors.
    let reference = reference_of("_fab_poc_isup").unwrap();
    let consts = [("UP", Value::num_list(vec![0.0, 0.0, 1.0]))];
    let mut inputs = value_battery();
    inputs.extend([
        Value::num_list(vec![0.0, 0.0, 1.0]),
        Value::num_list(vec![0.0, 0.0, -1.0]),
        Value::num_list(vec![0.0, 0.0, 1.0 + 1e-15]),
        Value::list(vec![Value::Num(0.0), Value::Num(0.0), Value::Num(1.0)]),
    ]);
    for v in inputs {
        let args = [v.clone()];
        assert!(
            same_result(
                &super::poc_isup(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(reference, &[], &consts, &args)
            ),
            "_fab_poc_isup diverged on {v:?}"
        );
    }
}

#[test]
fn fast_equals_slow_o9_tree2a_apply() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let ap_ref = reference_of("apply").unwrap();
    let ap_deps = [
        reference_of("_apply").unwrap(),
        reference_of("is_matrix").unwrap(),
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("is_2d_transform").unwrap(),
        reference_of("is_def").unwrap(),
        pin_reference_of("is_vnf").unwrap(),
        pin_reference_of("determinant").unwrap(),
        pin_reference_of("det2").unwrap(),
        pin_reference_of("det3").unwrap(),
        pin_reference_of("det4").unwrap(),
        pin_reference_of("reverse").unwrap(),
        pin_reference_of("vnf_reverse_faces").unwrap(),
        pin_reference_of("str_join").unwrap(),
    ];
    let p3 = |x: f64, y: f64, z: f64| Value::num_list(vec![x, y, z]);
    let m4 = |rows: [[f64; 4]; 4]| {
        let rows: Vec<Value> = rows.iter().map(|r| Value::num_list(r.to_vec())).collect();
        Value::list(rows)
    };
    let translate = m4([
        [1.0, 0.0, 0.0, 5.0],
        [0.0, 1.0, 0.0, -3.0],
        [0.0, 0.0, 1.0, 2.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let mirror_x = m4([
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let tet = Value::list(vec![
        Value::list(vec![
            p3(0.0, 0.0, 0.0),
            p3(1.0, 0.0, 0.0),
            p3(0.0, 1.0, 0.0),
            p3(0.0, 0.0, 1.0),
        ]),
        Value::list(vec![
            Value::num_list(vec![0.0, 2.0, 1.0]),
            Value::num_list(vec![0.0, 1.0, 3.0]),
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::num_list(vec![0.0, 3.0, 2.0]),
        ]),
    ]);
    // a degenerate-but-is_vnf-passing VNF with a STRING face — the str_join lane under a mirror
    let stringy = Value::list(vec![
        Value::list(vec![
            p3(0.0, 0.0, 0.0),
            p3(1.0, 0.0, 0.0),
            p3(0.0, 1.0, 0.0),
        ]),
        Value::list(vec![
            Value::num_list(vec![0.0, 1.0, 2.0]),
            Value::string("abc"),
        ]),
    ]);
    let patch = Value::list(vec![
        Value::list(vec![p3(0.0, 0.0, 0.0), p3(1.0, 0.0, 0.0)]),
        Value::list(vec![p3(0.0, 1.0, 0.0), p3(1.0, 1.0, 0.0)]),
    ]);
    let pts = Value::list(vec![p3(1.0, 2.0, 3.0), p3(-1.0, 0.5, 0.0)]);
    let cases: Vec<Vec<Value>> = vec![
        vec![translate.clone(), Value::list(vec![])],
        vec![translate.clone(), p3(1.0, 2.0, 3.0)],
        vec![translate.clone(), tet.clone()],
        vec![mirror_x.clone(), tet.clone()],
        vec![mirror_x.clone(), stringy.clone()],
        vec![translate.clone(), patch.clone()],
        vec![translate.clone(), pts.clone()],
        vec![Value::Num(5.0), pts.clone()],
        vec![translate.clone(), Value::Num(7.0)],
    ];
    for args in &cases {
        assert!(
            same_result(
                &super::apply(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(ap_ref, &ap_deps, &consts, args)
            ),
            "apply diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_o9_tree2b_rot() {
    let no_arg = Value::list(vec![
        Value::Bool(true),
        Value::num_list(vec![123_232_345.0]),
        Value::Bool(false),
    ]);
    let consts = [
        ("_EPSILON", Value::Num(1e-9)),
        ("UP", Value::num_list(vec![0.0, 0.0, 1.0])),
        ("RIGHT", Value::num_list(vec![1.0, 0.0, 0.0])),
        ("_NO_ARG", no_arg.clone()),
    ];
    // rot's whole closure as the oracle program
    let deps: Vec<&str> = [
        "point3d",
        "affine3d_rot_from_to",
        "affine3d_rot_by_axis",
        "affine3d_zrot",
        "affine3d_yrot",
        "affine3d_xrot",
        "affine3d_translate",
        "affine3d_identity",
        "ident",
        "default",
        "apply",
        "_apply",
        "is_2d_transform",
        "vector_axis",
        "v_abs",
        "v_theta",
        "point2d",
        "vector_angle",
        "same_shape",
        "is_def",
        "is_matrix",
        "is_consistent",
        "_list_pattern",
        "unit",
        "approx",
        "idx",
        "posmod",
        "is_vector",
        "all_nonzero",
        "is_finite",
        "is_nan",
    ]
    .iter()
    .map(|n| reference_of(n).expect(n))
    .chain(
        [
            "move",
            "rot_inverse",
            "hstack",
            "all",
            "_all_bool",
            "is_func",
            "min_length",
            "max_length",
            "determinant",
            "det2",
            "det3",
            "det4",
            "is_vnf",
            "reverse",
            "vnf_reverse_faces",
            "str_join",
            "constrain",
        ]
        .iter()
        .map(|n| pin_reference_of(n).expect(n)),
    )
    .collect();
    let p3 = |x: f64, y: f64, z: f64| Value::num_list(vec![x, y, z]);
    let u = Value::Undef;
    let pts = Value::list(vec![p3(1.0, 2.0, 3.0), p3(-1.0, 0.5, 0.0)]);

    // translate / rot_by_axis smalls first
    let tr_ref = reference_of("affine3d_translate").unwrap();
    let tr_deps = [reference_of("default").unwrap()];
    for v in [
        Value::num_list(vec![1.0, -2.0, 3.0]),
        p2(4.0, 5.0),
        Value::list(vec![]),
        Value::Num(7.0),
    ] {
        let args = [v.clone()];
        assert!(
            same_result(
                &super::affine3d_translate(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(tr_ref, &tr_deps, &consts, &args)
            ),
            "affine3d_translate diverged on {v:?}"
        );
    }
    let ba_ref = reference_of("affine3d_rot_by_axis").unwrap();
    let ba_cases: Vec<Vec<Value>> = vec![
        vec![p3(0.0, 0.0, 1.0), Value::Num(45.0)],
        vec![p3(1.0, 1.0, 1.0), Value::Num(120.0)],
        vec![p3(1.0, 0.0, 0.0), Value::Num(0.0)],
        vec![p3(1.0, 0.0, 0.0), Value::Num(1e-12)],
        vec![p2(1.0, 0.0), Value::Num(30.0)],
        vec![p3(1.0, 0.0, 0.0), Value::Undef],
    ];
    for args in &ba_cases {
        assert!(
            same_result(
                &super::affine3d_rot_by_axis(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(ba_ref, &deps, &consts, args)
            ),
            "affine3d_rot_by_axis diverged on {args:?}"
        );
    }

    // rot — every lane: scalar, Euler vector, v-axis, from/to, cp conjugation, reverse (both parities of
    // matrix), p application, explicit-sentinel p, and the assert lanes.
    let rot_ref = reference_of("rot").unwrap();
    let cases: Vec<Vec<Value>> = vec![
        vec![Value::Num(37.0)],
        vec![p3(30.0, 40.0, 50.0)],
        vec![Value::Num(45.0), p3(1.0, 1.0, 0.0)],
        vec![Value::Num(30.0), u.clone(), p3(1.0, 2.0, 3.0)],
        vec![
            Value::Num(15.0),
            u.clone(),
            u.clone(),
            p3(0.0, 0.0, 1.0),
            p3(1.0, 0.0, 0.0),
        ],
        vec![
            Value::Num(0.0),
            u.clone(),
            u.clone(),
            p3(0.0, 0.0, 1.0),
            p3(0.0, 0.0, 2.0),
        ],
        vec![
            Value::Num(37.0),
            u.clone(),
            u.clone(),
            u.clone(),
            u.clone(),
            Value::Bool(true),
        ],
        vec![
            Value::Num(37.0),
            u.clone(),
            u.clone(),
            u.clone(),
            u.clone(),
            Value::Bool(false),
            pts.clone(),
        ],
        vec![
            Value::Num(37.0),
            u.clone(),
            u.clone(),
            u.clone(),
            u.clone(),
            Value::Bool(false),
            no_arg.clone(),
        ],
        vec![Value::Num(30.0), p3(0.0, 0.0, 0.0)],
        vec![Value::string("a")],
        vec![Value::Num(30.0), u.clone(), u.clone(), p3(1.0, 0.0, 0.0)],
        vec![
            p3(10.0, 20.0, 30.0),
            u.clone(),
            u.clone(),
            u.clone(),
            u.clone(),
            Value::Bool(true),
            pts.clone(),
        ],
    ];
    for args in &cases {
        assert!(
            same_result(
                &super::rot(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(rot_ref, &deps, &consts, args)
            ),
            "rot diverged on {args:?}"
        );
    }
}

#[test]
fn fast_equals_slow_o9_tree1() {
    let consts = [
        ("_EPSILON", Value::Num(1e-9)),
        ("UP", Value::num_list(vec![0.0, 0.0, 1.0])),
        ("RIGHT", Value::num_list(vec![1.0, 0.0, 0.0])),
    ];
    let p3v = |x: f64, y: f64, z: f64| Value::num_list(vec![x, y, z]);
    let iv_knot = [
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
    ];

    // v_abs / v_theta / point2d / affine3d_identity — smalls.
    let va_ref = reference_of("v_abs").unwrap();
    let vt_ref = reference_of("v_theta").unwrap();
    for v in [
        p3v(1.0, -2.0, 3.0),
        p3v(-0.0, 0.0, -1.5),
        p2(-1.0, 1.0),
        p2(3.0, -4.0),
        Value::num_list(vec![1.0]),
        Value::Num(2.0),
        Value::Undef,
    ] {
        let args = [v.clone()];
        assert!(
            same_result(
                &super::v_abs(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(va_ref, &iv_knot, &consts, &args)
            ),
            "v_abs diverged on {v:?}"
        );
        assert!(
            same_result(
                &super::v_theta(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(vt_ref, &iv_knot, &consts, &args)
            ),
            "v_theta diverged on {v:?}"
        );
    }
    let p2d_ref = reference_of("point2d").unwrap();
    for (p, fill) in [
        (Value::num_list(vec![1.0]), None),
        (p3v(1.0, 2.0, 3.0), None),
        (
            Value::list(vec![Value::Undef, Value::Num(2.0)]),
            Some(Value::Num(7.0)),
        ),
        (Value::Num(5.0), None),
    ] {
        let mut args = vec![p.clone()];
        if let Some(f) = &fill {
            args.push(f.clone());
        }
        assert!(
            same_result(
                &super::point2d(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(p2d_ref, &[], &consts, &args)
            ),
            "point2d diverged on ({p:?}, {fill:?})"
        );
    }
    let ai_ref = reference_of("affine3d_identity").unwrap();
    assert!(
        same_result(
            &super::affine3d_identity(&[]),
            &interpret_with_deps_consts(ai_ref, &[reference_of("ident").unwrap()], &consts, &[])
        ),
        "affine3d_identity diverged"
    );

    // vector_axis — the two-vector forms (perpendicular / parallel → UP fallback / UP-aligned → RIGHT
    // fallback / antiparallel), the three-point form, both paired-list arities, and the raise lanes.
    let vx_ref = reference_of("vector_axis").unwrap();
    let vx_deps: Vec<&str> = iv_knot
        .iter()
        .copied()
        .chain([
            reference_of("is_consistent").unwrap(),
            reference_of("_list_pattern").unwrap(),
            reference_of("point3d").unwrap(),
            reference_of("unit").unwrap(),
            reference_of("v_abs").unwrap(),
        ])
        .collect();
    let vx_cases: Vec<Vec<Value>> = vec![
        vec![p3v(1.0, 0.0, 0.0), p3v(0.0, 1.0, 0.0)],
        vec![p3v(1.0, 0.0, 0.0), p3v(2.0, 0.0, 0.0)],
        vec![p3v(0.0, 0.0, 1.0), p3v(0.0, 0.0, 2.0)],
        vec![p3v(1.0, 0.0, 0.0), p3v(-1.0, 0.0, 0.0)],
        vec![p2(1.0, 0.0), p2(0.0, 1.0)],
        vec![p3v(0.0, 0.0, 0.0), p3v(1.0, 0.0, 0.0)],
        vec![p3v(1.0, 2.0, 3.0), p2(1.0, 2.0)],
        vec![p3v(0.0, 0.0, 0.0), p3v(1.0, 1.0, 0.0), p3v(2.0, 0.0, 0.0)],
        vec![Value::list(vec![p3v(1.0, 0.0, 0.0), p3v(0.0, 1.0, 0.0)])],
        vec![Value::list(vec![
            p3v(0.0, 0.0, 0.0),
            p3v(1.0, 1.0, 0.0),
            p3v(2.0, 0.0, 0.0),
        ])],
        vec![Value::Num(5.0)],
        vec![p3v(1.0, 0.0, 0.0), p3v(0.0, 1.0, 0.0), Value::Num(9.0)],
    ];
    for args in &vx_cases {
        assert!(
            same_result(
                &super::vector_axis(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(vx_ref, &vx_deps, &consts, args)
            ),
            "vector_axis diverged on {args:?}"
        );
    }

    // affine3d_rot_from_to — aligned (identity), planar (zrot delta), general Rodrigues, 2D inputs,
    // antiparallel (the vector_axis fallback feeds Rodrigues), and the raise lanes.
    let rft_ref = reference_of("affine3d_rot_from_to").unwrap();
    let rft_deps: Vec<&str> = vx_deps
        .iter()
        .copied()
        .chain([
            reference_of("approx").unwrap(),
            reference_of("idx").unwrap(),
            reference_of("posmod").unwrap(),
            reference_of("affine3d_identity").unwrap(),
            reference_of("ident").unwrap(),
            reference_of("affine3d_zrot").unwrap(),
            reference_of("v_theta").unwrap(),
            reference_of("point2d").unwrap(),
            reference_of("vector_axis").unwrap(),
            reference_of("vector_angle").unwrap(),
            reference_of("same_shape").unwrap(),
            reference_of("is_def").unwrap(),
            reference_of("is_matrix").unwrap(),
            pin_reference_of("constrain").unwrap(),
        ])
        .collect();
    let rft_cases: Vec<Vec<Value>> = vec![
        vec![p3v(1.0, 0.0, 0.0), p3v(2.0, 0.0, 0.0)],
        vec![p3v(1.0, 0.0, 0.0), p3v(0.0, 1.0, 0.0)],
        vec![p3v(1.0, 0.0, 0.0), p3v(0.0, 0.0, 1.0)],
        vec![p3v(1.0, 2.0, 3.0), p3v(-3.0, 1.0, 0.5)],
        vec![p3v(1.0, 0.0, 0.0), p3v(-1.0, 0.0, 0.0)],
        vec![p2(1.0, 0.0), p2(0.0, 1.0)],
        vec![p3v(1.0, 0.0, 0.0), p2(0.0, 1.0)],
        vec![Value::Num(1.0), p3v(0.0, 0.0, 1.0)],
        vec![p3v(0.0, 0.0, 0.0), p3v(0.0, 0.0, 1.0)],
    ];
    for args in &rft_cases {
        assert!(
            same_result(
                &super::generated::affine3d_rot_from_to(&crate::surface::NoClosures, args),
                &interpret_with_deps_consts(rft_ref, &rft_deps, &consts, args)
            ),
            "affine3d_rot_from_to diverged on {args:?}"
        );
    }
}

#[test]
fn a_const_guarded_entry_resolves_with_its_guard_attached() {
    // The build-time gate reads `consts` off the resolved entry: non-empty means build_intrinsics skips
    // it (it arms post-hoist), and the guard travels with the entry for the arm step to verify.
    let (p, b) = parse_fn(reference_of("_fab_poc_near0").unwrap());
    let entry = resolve("_fab_poc_near0", &p, &b).expect("exact fingerprint resolves");
    assert_eq!(entry.consts, &[("_EPSILON", 1e-9)]);
    assert!(
        resolve("_fab_poc_sq", &p, &b).is_none(),
        "same body, different name → no entry"
    );
    // The pin anchors resolve too — a dep check needs their fingerprints. AR.26.1: an anchor is
    // scoped to the ASKING row's library, so the question is always "who is asking about what".
    assert!(
        super::anchor_fp("select", "is_range").is_some(),
        "PINS must anchor is_range"
    );
    assert!(
        super::anchor_fp("select", "no_such_fn").is_none(),
        "an unanchored name is a registry authoring bug the dep check declines over"
    );
    assert!(
        super::anchor_fp("no_such_fn", "is_range").is_none(),
        "a row nobody declared cannot anchor anything"
    );
}

#[test]
fn the_fingerprint_gate_matches_only_the_exact_body() {
    // Never silently wrong: the intrinsic registers for the EXACT reference, and misses on any
    // perturbation (different body) or a name mismatch → the interpreter runs the real body instead.
    let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
    assert!(
        resolve("_fab_poc_sq", &p, &b).is_some(),
        "the exact reference must register"
    );

    let (p2, b2) = parse_fn("function _fab_poc_sq(x) = x + x;");
    assert!(
        resolve("_fab_poc_sq", &p2, &b2).is_none(),
        "a changed body must NOT match"
    );

    let (p3, b3) = parse_fn("function _fab_poc_sq(x, y) = x * x;");
    assert!(
        resolve("_fab_poc_sq", &p3, &b3).is_none(),
        "a changed arity must NOT match"
    );

    assert!(
        resolve("some_other_name", &p, &b).is_none(),
        "same body, wrong name → no match"
    );
}

#[test]
fn build_ctx_wires_the_intrinsic_for_a_matching_program() {
    // The dispatch is authorized at ctx build: a program defining the exact reference function gets the
    // intrinsic in ctx.intrinsics (so `dispatch_call` will route its all-positional calls natively). A
    // program with a perturbed body does NOT — it stays interpreted.
    let matched = parse("function _fab_poc_sq(x) = x * x;").expect("parses");
    assert!(
        build_ctx(&matched, crate::Config::default())
            .intrinsics
            .contains_key("_fab_poc_sq"),
        "the exact reference must be wired as an intrinsic"
    );
    let perturbed = parse("function _fab_poc_sq(x) = x * x + 1;").expect("parses");
    assert!(
        !build_ctx(&perturbed, crate::Config::default())
            .intrinsics
            .contains_key("_fab_poc_sq"),
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
    let ctx = build_ctx(&program, crate::Config::default());
    let call = match &program.stmts[1].kind {
        StmtKind::Assignment { value, .. } => value,
        other => panic!("expected an assignment, got {other:?}"),
    };
    let result = crate::eval::eval_with_ctx(call, &Scope::new(), &ctx).expect("evaluates");
    assert_eq!(
        result,
        Value::Num(49.0),
        "the intrinsic-dispatched call returns x*x"
    );
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
        let func = resolve(name, &params, &body)
            .expect("its own reference must register")
            .func;
        for input in &cases {
            let one = [input.clone()];
            assert!(
                same_result(
                    &func(&crate::surface::NoClosures, &one),
                    &interpret(reference, &one)
                ),
                "{name}({input:?}) diverged"
            );
        }
        // Zero args: the single param defaults to undef in both paths.
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &[]),
                &interpret(reference, &[])
            ),
            "{name}() diverged"
        );
    }
}

#[test]
fn is_nan_matches_its_reference_bit_for_bit() {
    // `is_nan(x) = (x!=x)` — no deps, so the plain interpreter is the oracle. The list-with-NaN case is
    // the one that matters: `[nan]!=[nan]` is TRUE (element-wise), so a scalar-only intrinsic would be
    // wrong there — the intrinsic routes non-numbers through the real `!=`, and this proves it.
    let reference = reference_of("is_nan").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("is_nan", &params, &body)
        .expect("its own reference must register")
        .func;
    for input in value_battery() {
        let one = [input.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret(reference, &one)
            ),
            "is_nan({input:?}) diverged"
        );
    }
    assert!(
        same_result(
            &func(&crate::surface::NoClosures, &[]),
            &interpret(reference, &[])
        ),
        "is_nan() diverged"
    );
}

#[test]
fn is_finite_matches_its_reference_bit_for_bit() {
    // `is_finite(x) = is_num(x) && !is_nan(0*x)` calls `is_nan` — the dependency-aware oracle interprets
    // the reference WITH `is_nan` defined (and intrinsics cleared, so `is_nan` interprets too). Proves the
    // direct `f64::is_finite` collapse equals the full is_num/`0*x`/is_nan chain across every value shape.
    let reference = reference_of("is_finite").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("is_finite", &params, &body)
        .expect("its own reference must register")
        .func;
    let deps = ["function is_nan(x) = (x!=x);"];
    for input in value_battery() {
        let one = [input.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret_with_deps(reference, &deps, &one)
            ),
            "is_finite({input:?}) diverged"
        );
    }
    assert!(
        same_result(
            &func(&crate::surface::NoClosures, &[]),
            &interpret_with_deps(reference, &deps, &[])
        ),
        "is_finite() diverged"
    );
}

#[test]
fn last_matches_its_reference_bit_for_bit() {
    // `last(list) = list[len(list)-1]` calls only builtins (`len`, index) → plain interpreter oracle. The
    // battery hits every shape: a populated list/numlist (real last element), an EMPTY list (len 0 →
    // index -1 → undef), a string (last char), and non-indexables (num/range/undef → undef).
    let reference = reference_of("last").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("last", &params, &body)
        .expect("its own reference must register")
        .func;
    for input in value_battery() {
        let one = [input.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret(reference, &one)
            ),
            "last({input:?}) diverged"
        );
    }
    // A longer list, to prove it's the LAST element and not the first/second.
    let long = [Value::list(
        (0..7).map(|i| Value::Num(f64::from(i))).collect::<Vec<_>>(),
    )];
    assert!(
        same_result(
            &func(&crate::surface::NoClosures, &long),
            &interpret(reference, &long)
        ),
        "last(0..6) diverged"
    );
}

#[test]
fn default_matches_its_reference_bit_for_bit() {
    // `default(v, dflt=undef) = is_undef(v) ? dflt : v` — two params, so prove BOTH the 1-arg (dflt takes
    // its undef default) and 2-arg forms across the battery. `is_undef` is a builtin → plain oracle.
    let reference = reference_of("default").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("default", &params, &body)
        .expect("its own reference must register")
        .func;
    let battery = value_battery();
    for v in &battery {
        let one = [v.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret(reference, &one)
            ),
            "default({v:?}) diverged"
        );
        for d in &battery {
            let two = [v.clone(), d.clone()];
            assert!(
                same_result(
                    &func(&crate::surface::NoClosures, &two),
                    &interpret(reference, &two)
                ),
                "default({v:?}, {d:?}) diverged"
            );
        }
    }
}

#[test]
fn is_liststr_matches_its_reference_bit_for_bit() {
    // `_is_liststr(s) = is_list(s) || is_str(s)` calls the `is_str` BOSL2 fn → dependency-aware oracle
    // (is_list is a builtin). True for List/NumList/Str, false otherwise, across the whole battery.
    let reference = reference_of("_is_liststr").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("_is_liststr", &params, &body)
        .expect("its own reference must register")
        .func;
    let deps = ["function is_str(x) = is_string(x);"];
    for input in value_battery() {
        let one = [input.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret_with_deps(reference, &deps, &one)
            ),
            "_is_liststr({input:?}) diverged"
        );
    }
}

#[test]
fn point3d_matches_its_reference_bit_for_bit() {
    // `point3d` is the first asserting intrinsic: a non-list must ERROR on BOTH sides (same_result treats
    // any two errors as matching), a list pads/truncates to 3 coords with `fill`. Proves the 1-arg
    // (fill=0) and 2-arg forms, and the padding (short vector) / truncation (long) / out-of-range→fill
    // paths — including the NumList-vs-List coalescing of the result.
    let reference = reference_of("point3d").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("point3d", &params, &body)
        .expect("its own reference must register")
        .func;
    for input in value_battery() {
        let one = [input.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret(reference, &one)
            ),
            "point3d({input:?}) diverged"
        );
    }
    // Explicit shape cases: short (pad), exact, long (truncate), a heterogeneous list (List result), and a
    // custom 2-arg fill. Each proves value AND the assert-passes path.
    let shapes = [
        vec![Value::Num(5.0)],
        vec![Value::Num(1.0), Value::Num(2.0)],
        vec![Value::Num(1.0), Value::Num(2.0), Value::Num(3.0)],
        vec![
            Value::Num(1.0),
            Value::Num(2.0),
            Value::Num(3.0),
            Value::Num(4.0),
        ],
        vec![Value::Num(1.0), Value::string("x")],
    ];
    for s in shapes {
        let p = Value::list(s);
        let one = [p.clone()];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &one),
                &interpret(reference, &one)
            ),
            "point3d({p:?}) diverged"
        );
        let two = [p.clone(), Value::Num(-1.0)];
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, &two),
                &interpret(reference, &two)
            ),
            "point3d({p:?}, -1) diverged"
        );
    }
}

#[test]
fn select_matches_its_reference_bit_for_bit() {
    // `select` is the first MULTI-BRANCH intrinsic — scalar index / vector-or-range gather / two-index
    // slice, three assert raise-sites, list-OR-string input. The dependency-aware oracle interprets the
    // verbatim reference WITH the real BOSL2 predicate chain defined (is_vector → is_finite → is_nan,
    // is_range) and intrinsics cleared, so the native `func` is proven against the FULLY-interpreted body.
    // `_EPSILON`/`norm`/`all_nonzero` are inert at is_vector's default args (short-circuited), so they need
    // no definition — an unknown `_EPSILON` resolves to undef and is never read.
    let reference = reference_of("select").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("select", &params, &body)
        .expect("its own reference must register")
        .func;
    let deps = [
        "function is_nan(x) = (x!=x);",
        "function is_finite(x) = is_num(x) && !is_nan(0*x);",
        "function is_range(x) = !is_list(x) && is_finite(x[0]) && is_finite(x[1]) && is_finite(x[2]) ;",
        "function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) = \
            is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0] \
            && (is_undef(length) || (assert(is_num(length))len(v)==length)) \
            && (is_undef(zero) || ((norm(v) >= eps) == !zero)) \
            && (!all_nonzero || all_nonzero(v)) ;",
    ];

    let n = |xs: &[f64]| Value::num_list(xs.to_vec());
    let l7 = n(&[3., 4., 5., 6., 7., 8., 9.]); // the lists.scad doc example
    let hetero = Value::list(vec![
        Value::Num(1.0),
        Value::string("a"),
        Value::num_list(vec![2.0, 3.0]),
    ]);
    let s = Value::string("hello");
    let rng = |start: f64, step: f64, end: f64| Value::Range { start, step, end };

    let inf = f64::INFINITY;
    let nan = f64::NAN;
    let cases: Vec<Vec<Value>> = vec![
        // assert #1: a non-list/string `list` raises (both sides).
        vec![Value::Num(5.0), Value::Num(0.0)],
        vec![Value::Undef, Value::Num(0.0)],
        vec![rng(0., 1., 5.), Value::Num(0.0)],
        // l==0 → [] (list AND string), single- and two-arg.
        vec![n(&[]), Value::Num(2.0)],
        vec![Value::string(""), Value::Num(0.0)],
        vec![n(&[]), Value::Num(2.0), Value::Num(4.0)],
        // scalar start — wraparound, negatives, out-of-range, fractional (truncates), ±inf.
        vec![l7.clone(), Value::Num(5.0)],
        vec![l7.clone(), Value::Num(0.0)],
        vec![l7.clone(), Value::Num(6.0)],
        vec![l7.clone(), Value::Num(7.0)], // == l → wraps to 0
        vec![l7.clone(), Value::Num(-2.0)],
        vec![l7.clone(), Value::Num(-1.0)],
        vec![l7.clone(), Value::Num(100.0)],
        vec![l7.clone(), Value::Num(-100.0)],
        vec![l7.clone(), Value::Num(3.5)],
        vec![l7.clone(), Value::Num(inf)], // is_num TRUE (not NaN) → wrap→nan→index undef
        vec![l7.clone(), Value::Num(-inf)],
        // NaN start: is_num is FALSE for NaN → else branch → assert #2 raises.
        vec![l7.clone(), Value::Num(nan)],
        // vector start — gather with wraparound, and the empty vector → [].
        vec![l7.clone(), n(&[1., 3.])],
        vec![l7.clone(), n(&[3., 1.])],
        vec![l7.clone(), n(&[-1., -2.])],
        vec![l7.clone(), n(&[])],
        // range start.
        vec![l7.clone(), rng(1., 1., 3.)],
        vec![l7.clone(), rng(0., 2., 6.)],
        // BAD non-num start → assert #2 raises: non-num elem, nested, inf/nan elem, non-finite range,
        // string/bool/undef.
        vec![
            l7.clone(),
            Value::list(vec![Value::Num(1.0), Value::string("a")]),
        ],
        vec![
            l7.clone(),
            Value::list(vec![Value::num_list(vec![1.0, 2.0])]),
        ],
        vec![l7.clone(), n(&[1., inf])],
        vec![l7.clone(), n(&[nan, 2.])],
        vec![l7.clone(), rng(0., 1., inf)],
        vec![l7.clone(), Value::string("x")],
        vec![l7.clone(), Value::Bool(true)],
        vec![l7.clone(), Value::Undef],
        // two-index form — the doc examples + s>e wraparound + fractional bounds.
        vec![l7.clone(), Value::Num(5.0), Value::Num(6.0)],
        vec![l7.clone(), Value::Num(5.0), Value::Num(8.0)],
        vec![l7.clone(), Value::Num(5.0), Value::Num(2.0)],
        vec![l7.clone(), Value::Num(-3.0), Value::Num(-1.0)],
        vec![l7.clone(), Value::Num(3.0), Value::Num(3.0)],
        vec![l7.clone(), Value::Num(0.0), Value::Num(0.0)],
        vec![l7.clone(), Value::Num(6.0), Value::Num(0.0)],
        vec![l7.clone(), Value::Num(2.5), Value::Num(5.5)],
        // two-index non-finite → assert #3 raises (a non-num or inf/nan bound).
        vec![l7.clone(), Value::Num(inf), Value::Num(2.0)],
        vec![l7.clone(), Value::Num(2.0), Value::Num(nan)],
        vec![l7.clone(), Value::Num(2.0), Value::string("x")],
        vec![l7.clone(), Value::string("x"), Value::Num(2.0)],
        // heterogeneous List as `list` — element access, gather, slice (List result).
        vec![hetero.clone(), Value::Num(1.0)],
        vec![hetero.clone(), Value::Num(2.0)],
        vec![hetero.clone(), n(&[0., 2.])],
        vec![hetero.clone(), Value::Num(0.0), Value::Num(2.0)],
        // string as `list` — single char, gather + slice (List-of-Str result).
        vec![s.clone(), Value::Num(1.0)],
        vec![s.clone(), Value::Num(-1.0)],
        vec![s.clone(), n(&[0., 4.])],
        vec![s.clone(), Value::Num(1.0), Value::Num(3.0)],
        vec![s.clone(), Value::Num(3.0), Value::Num(1.0)],
    ];

    for inputs in &cases {
        assert!(
            same_result(
                &func(&crate::surface::NoClosures, inputs),
                &interpret_with_deps(reference, &deps, inputs)
            ),
            "select diverged on {inputs:?}"
        );
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

/// O.10a — the region-monster band's DEPENDENCY tier, each native vs interpreting its pinned
/// reference: list handling (`list_wrap`/`are_ends_equal`/`flatten`/`column`/`count`), stats
/// (`mean`/`min_index`/`max_index`), linalg (`transpose`/`pointlist_bounds`), the segment intersection
/// (`_general_line_intersection`), and the lexicographic `_sort_vectors`. Exotic shapes ride along
/// (ragged rows, NaN cells, `-0.0` — the 4-lane-dot sign case lives in `pointlist_bounds`).
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one battery per band tier, like its siblings"
)]
fn fast_equals_slow_o10_dep_tier() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let approx_deps = [
        reference_of("idx").unwrap(),
        reference_of("posmod").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
    ];
    let p = |xs: &[f64]| Value::num_list(xs.to_vec());

    // list_wrap / are_ends_equal — open, closed, near-closed (eps), short, exotic.
    let lw_ref = pin_reference_of("list_wrap").unwrap();
    let lw_deps: Vec<&str> = approx_deps
        .iter()
        .copied()
        .chain([
            pin_reference_of("are_ends_equal").unwrap(),
            reference_of("approx").unwrap(),
        ])
        .collect();
    let square_open = Value::list(vec![p(&[0.0, 0.0]), p(&[10.0, 0.0]), p(&[10.0, 10.0])]);
    let square_closed = Value::list(vec![p(&[0.0, 0.0]), p(&[10.0, 0.0]), p(&[0.0, 0.0])]);
    let near_closed = Value::list(vec![p(&[0.0, 0.0]), p(&[10.0, 0.0]), p(&[0.0, 5e-10])]);
    let one_pt = Value::list(vec![p(&[1.0, 2.0])]);
    let raw_nums = Value::num_list(vec![1.0, 2.0, 3.0]);
    for list in [
        &square_open,
        &square_closed,
        &near_closed,
        &one_pt,
        &raw_nums,
        &Value::Num(3.0),
    ] {
        let args = vec![list.clone(), Value::Num(1e-9)];
        assert!(
            same_result(
                &super::regions::list_wrap_val(
                    &crate::surface::NoClosures,
                    list,
                    &Value::Num(1e-9)
                ),
                &interpret_with_deps_consts(lw_ref, &lw_deps, &consts, &args)
            ),
            "list_wrap diverged on {list:?}"
        );
        let ae_ref = pin_reference_of("are_ends_equal").unwrap();
        let ae_deps: Vec<&str> = approx_deps
            .iter()
            .copied()
            .chain([reference_of("approx").unwrap()])
            .collect();
        assert!(
            same_result(
                &super::regions::are_ends_equal_val(
                    &crate::surface::NoClosures,
                    list,
                    &Value::Num(1e-9)
                ),
                &interpret_with_deps_consts(ae_ref, &ae_deps, &consts, &args)
            ),
            "are_ends_equal diverged on {list:?}"
        );
    }

    // _general_line_intersection — crossing, parallel, near-parallel, collinear, degenerate.
    let gli_ref = pin_reference_of("_general_line_intersection").unwrap();
    let gli_deps: Vec<&str> = approx_deps
        .iter()
        .copied()
        .chain([reference_of("approx").unwrap()])
        .collect();
    let seg = |a: [f64; 2], b: [f64; 2]| Value::list(vec![p(&a), p(&b)]);
    let cases = [
        (seg([0.0, 0.0], [10.0, 0.0]), seg([5.0, -5.0], [5.0, 5.0])),
        (seg([0.0, 0.0], [10.0, 0.0]), seg([0.0, 1.0], [10.0, 1.0])), // parallel
        (seg([0.0, 0.0], [10.0, 0.0]), seg([0.0, 0.0], [10.0, 1e-12])), // near-parallel
        (seg([0.0, 0.0], [10.0, 0.0]), seg([3.0, 0.0], [7.0, 0.0])),  // collinear
        (seg([2.0, 2.0], [2.0, 2.0]), seg([0.0, 0.0], [4.0, 4.0])),   // zero-length s1
        (seg([-0.0, 1.0], [4.0, -3.0]), seg([0.0, -1.0], [4.0, 3.0])), // -0.0 endpoint
    ];
    for (s1, s2) in &cases {
        let args = vec![s1.clone(), s2.clone(), Value::Num(1e-9)];
        assert!(
            same_result(
                &super::regions::gli_val(&crate::surface::NoClosures, s1, s2, &Value::Num(1e-9)),
                &interpret_with_deps_consts(gli_ref, &gli_deps, &consts, &args)
            ),
            "_general_line_intersection diverged on {s1:?} x {s2:?}"
        );
    }

    // flatten / column / count — plain, nested, ragged, exotic.
    let fl_ref = pin_reference_of("flatten").unwrap();
    let nested = Value::list(vec![
        Value::list(vec![Value::Num(1.0), Value::Num(2.0)]),
        p(&[3.0, 4.0]),
        Value::Num(5.0),
        Value::string("s"),
        Value::list(vec![Value::list(vec![Value::Num(6.0)])]),
    ]);
    for l in [&nested, &raw_nums, &Value::Num(7.0), &Value::Undef] {
        assert!(
            same_result(
                &super::regions::flatten_val(l),
                &interpret_with_deps_consts(fl_ref, &[], &consts, std::slice::from_ref(l))
            ),
            "flatten diverged on {l:?}"
        );
    }
    let col_ref = pin_reference_of("column").unwrap();
    let col_deps = [
        pin_reference_of("is_int").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
    ];
    let ragged = Value::list(vec![p(&[1.0, 2.0, 3.0]), p(&[4.0]), p(&[5.0, 6.0])]);
    for (m, i) in [
        (&square_open, Value::Num(0.0)),
        (&square_open, Value::Num(1.0)),
        (&ragged, Value::Num(1.0)),
        (&square_open, Value::Num(-1.0)),
        (&square_open, Value::Num(0.5)),
        (&Value::Num(1.0), Value::Num(0.0)),
    ] {
        let args = vec![m.clone(), i.clone()];
        assert!(
            same_result(
                &super::regions::column_val(m, &i),
                &interpret_with_deps_consts(col_ref, &col_deps, &consts, &args)
            ),
            "column diverged on {m:?}[{i:?}]"
        );
    }
    let cnt_ref = pin_reference_of("count").unwrap();
    for (n, s, step, rev) in [
        (
            Value::Num(4.0),
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Bool(false),
        ),
        (
            Value::Num(4.0),
            Value::Num(2.0),
            Value::Num(3.0),
            Value::Bool(true),
        ),
        (
            raw_nums.clone(),
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Bool(false),
        ),
        (
            Value::Num(0.0),
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Bool(false),
        ),
        (
            Value::Num(2.5),
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Bool(false),
        ),
        (
            Value::Num(2.5),
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Bool(true),
        ),
    ] {
        let args = vec![n.clone(), s.clone(), step.clone(), rev.clone()];
        assert!(
            same_result(
                &super::regions::count_val(&n, &s, &step, &rev),
                &interpret_with_deps_consts(cnt_ref, &[], &consts, &args)
            ),
            "count diverged on {args:?}"
        );
    }

    // mean — numbers, vectors (the vector-sum lane), empty (raise), inconsistent (raise).
    let mean_ref = pin_reference_of("mean").unwrap();
    let mean_deps = [
        reference_of("sum").unwrap(),
        reference_of("_sum").unwrap(),
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("same_shape").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("is_vector").unwrap(),
    ];
    let vecs = Value::list(vec![p(&[1.0, 2.0]), p(&[3.0, 4.0]), p(&[5.0, 6.0])]);
    let mixed = Value::list(vec![Value::Num(1.0), p(&[2.0, 3.0])]);
    for v in [
        &raw_nums,
        &vecs,
        &mixed,
        &Value::list(vec![]),
        &Value::Num(2.0),
    ] {
        assert!(
            same_result(
                &super::regions::mean_val(&crate::surface::NoClosures, v),
                &interpret_with_deps_consts(mean_ref, &mean_deps, &consts, std::slice::from_ref(v))
            ),
            "mean diverged on {v:?}"
        );
    }

    // min_index / max_index — plain, ties (first match), negatives, non-vector (raise).
    let iv_deps = [
        reference_of("is_vector").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("all_nonzero").unwrap(),
        reference_of("idx").unwrap(),
        reference_of("posmod").unwrap(),
    ];
    let mini_ref = pin_reference_of("min_index").unwrap();
    let maxi_ref = pin_reference_of("max_index").unwrap();
    for v in [
        &p(&[3.0, 1.0, 2.0]),
        &p(&[1.0, 1.0, 1.0]),
        &p(&[-5.0, 0.0, -5.0]),
        &raw_nums,
        &mixed,
        &Value::Num(4.0),
    ] {
        assert!(
            same_result(
                &super::regions::min_index_val(v),
                &interpret_with_deps_consts(mini_ref, &iv_deps, &consts, std::slice::from_ref(v))
            ),
            "min_index diverged on {v:?}"
        );
        assert!(
            same_result(
                &super::regions::max_index_val(v),
                &interpret_with_deps_consts(maxi_ref, &iv_deps, &consts, std::slice::from_ref(v))
            ),
            "max_index diverged on {v:?}"
        );
    }

    // transpose (1-arg shape) — matrix, vector pass-through, ragged (raise), empty (raise).
    let tr_ref = pin_reference_of("transpose").unwrap();
    let tr_deps: Vec<&str> = iv_deps.to_vec();
    for m in [
        &square_open,
        &vecs,
        &raw_nums,
        &ragged,
        &Value::list(vec![]),
        &Value::Num(1.0),
    ] {
        assert!(
            same_result(
                &super::regions::transpose_val(m),
                &interpret_with_deps_consts(tr_ref, &tr_deps, &consts, std::slice::from_ref(m))
            ),
            "transpose diverged on {m:?}"
        );
    }

    // pointlist_bounds — 2D/3D, -0.0 coords (the 4-lane dot sign-of-zero case), invalid (raise).
    let pb_ref = pin_reference_of("pointlist_bounds").unwrap();
    let pb_deps: Vec<&str> = iv_deps
        .iter()
        .copied()
        .chain([
            reference_of("is_path").unwrap(),
            reference_of("is_matrix").unwrap(),
            reference_of("is_consistent").unwrap(),
            reference_of("_list_pattern").unwrap(),
            reference_of("same_shape").unwrap(),
            reference_of("in_list").unwrap(),
            reference_of("force_list").unwrap(),
            reference_of("ident").unwrap(),
            pin_reference_of("transpose").unwrap(),
        ])
        .collect();
    let pts_2d = Value::list(vec![p(&[1.0, -2.0]), p(&[-3.0, 4.0]), p(&[0.5, 0.5])]);
    let pts_negz = Value::list(vec![p(&[-0.0, 1.0]), p(&[2.0, -0.0])]);
    let pts_3d = Value::list(vec![p(&[1.0, 2.0, 3.0]), p(&[-1.0, -2.0, -3.0])]);
    for pts in [&pts_2d, &pts_negz, &pts_3d, &raw_nums, &Value::Num(1.0)] {
        assert!(
            same_result(
                &super::regions::pointlist_bounds_val(&crate::surface::NoClosures, pts),
                &interpret_with_deps_consts(pb_ref, &pb_deps, &consts, std::slice::from_ref(pts))
            ),
            "pointlist_bounds diverged on {pts:?}"
        );
    }

    // _sort_vectors — shuffles, duplicate first columns (the _i+1 lane), -0.0/0.0 ties, NaN cells
    // (rows in NO partition — dropped), ragged rows, singletons.
    let sv_ref = pin_reference_of("_sort_vectors").unwrap();
    let shuffled = Value::list(vec![
        p(&[3.0, 1.0]),
        p(&[1.0, 9.0]),
        p(&[1.0, 2.0]),
        p(&[2.0, 0.0]),
        p(&[1.0, 2.0]),
    ]);
    let zero_ties = Value::list(vec![p(&[0.0, 2.0]), p(&[-0.0, 1.0]), p(&[0.0, 0.0])]);
    let with_nan = Value::list(vec![p(&[1.0, 2.0]), p(&[f64::NAN, 0.0]), p(&[0.5, 1.0])]);
    let ragged_rows = Value::list(vec![p(&[2.0, 1.0]), p(&[2.0]), p(&[1.0, 5.0, 9.0])]);
    for arr in [
        &shuffled,
        &zero_ties,
        &with_nan,
        &ragged_rows,
        &Value::list(vec![]),
        &one_pt,
    ] {
        for il in [
            &Value::Undef,
            &Value::num_list(vec![1.0, 0.0]),
            &Value::num_list(vec![1.0]),
            &Value::num_list(vec![]),
        ] {
            let args = vec![arr.clone(), (*il).clone()];
            assert!(
                same_result(
                    &super::regions::sort_vectors_val(arr, il),
                    &interpret_with_deps_consts(sv_ref, &[], &consts, &args)
                ),
                "_sort_vectors diverged on {arr:?} idxlist={il:?}"
            );
        }
    }
}

/// O.10b — `vector_search` + `_bt_tree`, native vs interpreted, BOTH branches: the ≤400-point
/// quadratic scan AND the >400-point ball tree (they return indices in DIFFERENT orders — tree
/// order is load-bearing for `_rri`'s downstream `search`/`select`), plus the pre-built
/// `[points, tree]` target form and the empty/multi-query shapes.
#[test]
fn fast_equals_slow_o10_vector_search() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let p = |xs: &[f64]| Value::num_list(xs.to_vec());
    let deps: Vec<&str> = vec![
        pin_reference_of("_bt_tree").unwrap(),
        reference_of("_bt_search").unwrap(),
        pin_reference_of("pointlist_bounds").unwrap(),
        pin_reference_of("max_index").unwrap(),
        pin_reference_of("min_index").unwrap(),
        pin_reference_of("mean").unwrap(),
        pin_reference_of("count").unwrap(),
        pin_reference_of("transpose").unwrap(),
        reference_of("ident").unwrap(),
        reference_of("select").unwrap(),
        reference_of("idx").unwrap(),
        reference_of("sum").unwrap(),
        reference_of("_sum").unwrap(),
        reference_of("is_path").unwrap(),
        reference_of("is_matrix").unwrap(),
        reference_of("is_vector").unwrap(),
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("same_shape").unwrap(),
        reference_of("in_list").unwrap(),
        reference_of("force_list").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        pin_reference_of("is_range").unwrap(),
        reference_of("all_nonzero").unwrap(),
        reference_of("posmod").unwrap(),
        reference_of("approx").unwrap(),
    ];
    let vs_ref = pin_reference_of("vector_search").unwrap();

    // A deterministic pseudo-random 2D cloud (fixed recurrence — no rand dep): 30 points for the
    // quadratic branch, 420 for the tree branch.
    let cloud = |n: usize| -> Value {
        let mut pts = Vec::new();
        let mut x: f64 = 3.7;
        for _ in 0..n {
            x = (x * 73.5 + 11.25) % 97.0;
            let y = (x * 31.5 + 5.125) % 89.0;
            pts.push(p(&[x, y]));
        }
        Value::list(pts)
    };
    let small = cloud(30);
    let big = cloud(420);
    let q1 = p(&[50.0, 40.0]);
    let qs = Value::list(vec![p(&[50.0, 40.0]), p(&[10.0, 10.0])]);
    let empty = Value::list(vec![]);

    let cases: Vec<(Value, Value, Value)> = vec![
        (q1.clone(), Value::Num(20.0), small.clone()),
        (qs.clone(), Value::Num(20.0), small.clone()),
        (q1.clone(), Value::Num(30.0), big.clone()), // the TREE branch
        (qs.clone(), Value::Num(30.0), big.clone()), // tree branch, multi-query
        (q1.clone(), Value::Num(0.0), small.clone()), // zero radius
        (empty.clone(), Value::Num(5.0), small.clone()),
        (q1.clone(), Value::Num(-1.0), small.clone()), // bad radius (raise)
        (q1.clone(), Value::Num(5.0), empty.clone()),  // empty target... query is a vector
        (qs.clone(), Value::Num(5.0), empty.clone()),  // empty target, matrix query
        (q1.clone(), Value::Num(5.0), Value::Num(3.0)), // invalid target (raise)
    ];
    for (q, r, target) in &cases {
        let args = vec![q.clone(), r.clone(), target.clone()];
        assert!(
            same_result(
                &super::regions::vector_search_val(&crate::surface::NoClosures, q, r, target),
                &interpret_with_deps_consts(vs_ref, &deps, &consts, &args)
            ),
            "vector_search diverged on q={q:?} r={r:?} target={target:?}"
        );
    }

    // The pre-built [points, tree] target form: build the tree NATIVELY (bt_tree_val is itself
    // battery-checked just below), search through both engines.
    let n_small = 30.0;
    let ind = super::regions::count_val(
        &Value::Num(n_small),
        &Value::Num(0.0),
        &Value::Num(1.0),
        &Value::Bool(false),
    )
    .unwrap();
    let tree =
        super::regions::bt_tree_val(&crate::surface::NoClosures, &small, &ind, &Value::Num(5.0))
            .unwrap();
    let prebuilt = Value::list(vec![small.clone(), tree.clone()]);
    let args = vec![q1.clone(), Value::Num(25.0), prebuilt.clone()];
    assert!(
        same_result(
            &super::regions::vector_search_val(
                &crate::surface::NoClosures,
                &q1,
                &Value::Num(25.0),
                &prebuilt
            ),
            &interpret_with_deps_consts(vs_ref, &deps, &consts, &args)
        ),
        "vector_search diverged on the pre-built tree target"
    );

    // _bt_tree itself, structurally: leaf collapse (n<=leafsize) and a real split, both vs the
    // interpreted reference.
    let bt_ref = pin_reference_of("_bt_tree").unwrap();
    for (pts, leafsize) in [(&small, 50.0), (&small, 5.0), (&big, 25.0)] {
        let n = match pts {
            Value::List(xs) => xs.len(),
            _ => 0,
        };
        #[allow(clippy::cast_precision_loss, reason = "tiny test sizes")]
        let ind = super::regions::count_val(
            &Value::Num(n as f64),
            &Value::Num(0.0),
            &Value::Num(1.0),
            &Value::Bool(false),
        )
        .unwrap();
        let args = vec![(*pts).clone(), ind.clone(), Value::Num(leafsize)];
        assert!(
            same_result(
                &super::regions::bt_tree_val(
                    &crate::surface::NoClosures,
                    pts,
                    &ind,
                    &Value::Num(leafsize)
                ),
                &interpret_with_deps_consts(bt_ref, &deps, &consts, &args)
            ),
            "_bt_tree diverged on n={n} leafsize={leafsize}"
        );
    }
}

/// O.10c — the region monster itself: `_region_region_intersections` native vs interpreting the
/// verbatim reference with its FULL dep closure. Crossing regions, multi-path regions, self-touching
/// corners (the `vector_search` duplicate lane), open paths, degenerate zero-length edges, collinear
/// non-crossings, and a >400-point region that flips the corner search onto the ball-tree branch.
#[test]
fn fast_equals_slow_o10_region_monster() {
    let consts = [("_EPSILON", Value::Num(1e-9))];
    let p = |xs: &[f64]| Value::num_list(xs.to_vec());
    let deps: Vec<&str> = vec![
        reference_of("idx").unwrap(),
        pin_reference_of("list_wrap").unwrap(),
        pin_reference_of("are_ends_equal").unwrap(),
        reference_of("approx").unwrap(),
        reference_of("is_finite").unwrap(),
        reference_of("is_nan").unwrap(),
        reference_of("posmod").unwrap(),
        pin_reference_of("_general_line_intersection").unwrap(),
        pin_reference_of("flatten").unwrap(),
        pin_reference_of("vector_search").unwrap(),
        pin_reference_of("_bt_tree").unwrap(),
        reference_of("_bt_search").unwrap(),
        pin_reference_of("pointlist_bounds").unwrap(),
        reference_of("ident").unwrap(),
        pin_reference_of("transpose").unwrap(),
        reference_of("is_path").unwrap(),
        reference_of("is_matrix").unwrap(),
        reference_of("is_vector").unwrap(),
        reference_of("is_consistent").unwrap(),
        reference_of("_list_pattern").unwrap(),
        reference_of("same_shape").unwrap(),
        reference_of("in_list").unwrap(),
        reference_of("force_list").unwrap(),
        reference_of("all_nonzero").unwrap(),
        pin_reference_of("is_range").unwrap(),
        pin_reference_of("max_index").unwrap(),
        pin_reference_of("min_index").unwrap(),
        pin_reference_of("mean").unwrap(),
        reference_of("sum").unwrap(),
        reference_of("_sum").unwrap(),
        pin_reference_of("column").unwrap(),
        pin_reference_of("is_int").unwrap(),
        pin_reference_of("count").unwrap(),
        reference_of("select").unwrap(),
        pin_reference_of("_sort_vectors").unwrap(),
    ];
    let rri_ref = reference_of("_region_region_intersections").unwrap();

    let square = |x0: f64, y0: f64, s: f64| {
        Value::list(vec![
            p(&[x0, y0]),
            p(&[x0 + s, y0]),
            p(&[x0 + s, y0 + s]),
            p(&[x0, y0 + s]),
        ])
    };
    let r_a = Value::list(vec![square(0.0, 0.0, 10.0)]);
    let r_b = Value::list(vec![square(5.0, 5.0, 10.0)]);
    let r_two = Value::list(vec![square(0.0, 0.0, 4.0), square(20.0, 0.0, 4.0)]);
    // Self-touching: a bowtie sharing its center point twice (the cornerpts lane).
    let bowtie = Value::list(vec![Value::list(vec![
        p(&[0.0, 0.0]),
        p(&[4.0, 4.0]),
        p(&[8.0, 0.0]),
        p(&[4.0, 4.0]),
        p(&[4.0, 8.0]),
    ])]);
    // Degenerate: a duplicate consecutive point (zero-length edge) + a collinear side.
    let degen = Value::list(vec![Value::list(vec![
        p(&[0.0, 0.0]),
        p(&[0.0, 0.0]),
        p(&[10.0, 0.0]),
        p(&[10.0, 10.0]),
    ])]);
    // >400 points total: a 420-vertex near-circle — the corner search's TREE branch inside _rri.
    let big_poly = {
        let mut pts = Vec::new();
        for k in 0..420 {
            let th = f64::from(k) * std::f64::consts::TAU / 420.0;
            pts.push(p(&[7.0 * th.cos(), 7.0 * th.sin()]));
        }
        Value::list(vec![Value::list(pts)])
    };

    let cases: Vec<(Value, Value, Value, Value, Value)> = vec![
        (
            r_a.clone(),
            r_b.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            r_b.clone(),
            r_a.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            r_two.clone(),
            r_a.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            bowtie.clone(),
            r_a.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            degen.clone(),
            r_b.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            r_a.clone(),
            r_b.clone(),
            Value::Bool(false),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
        (
            r_a.clone(),
            r_b.clone(),
            Value::Bool(true),
            Value::Bool(false),
            Value::Num(1e-9),
        ),
        (
            r_a.clone(),
            r_a.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ), // self
        (
            r_a.clone(),
            r_b.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(0.5),
        ), // fat eps
        (
            big_poly.clone(),
            r_a.clone(),
            Value::Bool(true),
            Value::Bool(true),
            Value::Num(1e-9),
        ),
    ];
    for (r1, r2, c1, c2, eps) in &cases {
        let args = vec![r1.clone(), r2.clone(), c1.clone(), c2.clone(), eps.clone()];
        assert!(
            same_result(
                &super::regions::rri_val(&crate::surface::NoClosures, &args),
                &interpret_with_deps_consts(rri_ref, &deps, &consts, &args)
            ),
            "_rri diverged on closed=({c1:?},{c2:?}) eps={eps:?} r1={r1:?}"
        );
    }
}

/// AR.14.2 — the index AGREES with the linear scan it replaced, name for name.
///
/// Six lookups changed from a scan over `REGISTRY`/`PINS` to a `BTreeMap` hit, and a map silently
/// answers `None` where a scan would have found a duplicate's other copy. So this re-derives every
/// answer the slow way and demands they match: an index that quietly disagrees with the array it
/// indexes is a native wiring against a function it does not implement.
#[test]
fn the_registry_index_agrees_with_a_linear_scan() {
    use super::{PINS, REGISTRY, anchor_fp, entry_by_name, reference_fp};

    for entry in REGISTRY {
        // `entry_by_name` — the scan found the FIRST entry of that name.
        let scanned = REGISTRY.iter().find(|e| e.name == entry.name);
        let indexed = entry_by_name(entry.name);
        assert_eq!(
            scanned.map(|e| e.name),
            indexed.map(|e| e.name),
            "entry_by_name disagrees for `{}`",
            entry.name
        );
        // `reference_fp` / `anchor_fp` — both resolve a name to its reference fingerprint, and
        // `anchor_fp` falls through to PINS. Every registry name must resolve through both.
        assert!(
            reference_fp(entry.name).is_some(),
            "`{}` is in the registry but its reference does not fingerprint",
            entry.name
        );
        assert_eq!(
            anchor_fp(entry.name, entry.name),
            reference_fp(entry.name),
            "anchor_fp must prefer the registry entry for `{}`",
            entry.name
        );
    }

    // A PIN resolves only when no registry entry shadows it — the `or_else` arm. Asked on behalf of
    // a registry entry, since AR.26.1 scopes an anchor to the asking row's own library and fab-lang
    // ships its rows and its pins as ONE library.
    let asker = REGISTRY[0].name;
    for &(name, _) in PINS {
        assert!(
            anchor_fp(asker, name).is_some(),
            "pinned dep `{name}` does not resolve"
        );
    }
}

/// A name must be UNIQUE across the registry, because the index is keyed by it: a second entry with
/// the same name replaces the first and dispatch loses it with no diagnostic. Checked in release
/// too, unlike the `debug_assert` in `table()`, since a duplicate is a source-level authoring bug
/// that should never reach a build of any profile.
#[test]
fn registry_and_pin_names_are_unique() {
    use std::collections::BTreeSet;

    use super::{PINS, REGISTRY};

    let mut seen = BTreeSet::new();
    for entry in REGISTRY {
        assert!(
            seen.insert(entry.name),
            "`{}` is declared twice in REGISTRY — the index keeps one and dispatch silently \
             loses the other",
            entry.name
        );
    }
    let mut pinned = BTreeSet::new();
    for &(name, _) in PINS {
        assert!(pinned.insert(name), "`{name}` is declared twice in PINS");
    }
}

/// The dep graph REFERENCES ITSELF and the cycles are real — `approx` ↔ `idx` ↔ `posmod` (approx's
/// list branch calls idx, idx wraps offsets through posmod, posmod's assert calls approx) and
/// `all_nonzero` ↔ `is_vector`.
///
/// The cycles are NOT why the cross-links are names, and an earlier version of this comment said
/// they were. Corrected: a cyclic graph of `&'static` references compiles fine in safe Rust, because
/// statics resolve to addresses at link time and there is no initialization order to get wrong —
/// `static A: Node = Node { next: &B }; static B: Node = Node { next: &A };` builds and walks.
/// (`Rc` specifically cannot do this job, but for unrelated reasons: it is neither `Send` nor
/// `Sync`, so it cannot live in the `static` the table is cached in, and an `Rc` cycle leaks
/// without `Weak`.)
///
/// The real reason a dep stays a NAME is that the name is not addressing anything of ours. Of its
/// three uses in `guard_veto`, two cannot be a pointer at all: it keys into the USER's function
/// table (`functions.get(dep)` — a program that does not exist until one is loaded), and it is
/// compared against the native's own PARAMETER names for the AN.10 shadow check. Only the third
/// use, fetching the expected fingerprint, addresses the registry.
///
/// So the cycles are pinned here for a different reason than first written: they say the dep graph
/// is a real graph rather than a DAG, which is what makes a topological "resolve deps first" scheme
/// impossible and forces the guard to work name-at-a-time. A direct `&'static` link alongside the
/// name is still worth adding — it would make a typo'd dep a COMPILE error rather than a permanent
/// silent veto — and that is AR.14.4's business, where the macro can emit it.
#[test]
fn the_dep_graph_really_does_contain_cycles() {
    use super::REGISTRY;

    fn reaches(from: &str, target: &str, depth: usize) -> bool {
        if depth == 0 {
            return false;
        }
        REGISTRY.iter().find(|e| e.name == from).is_some_and(|e| {
            e.deps
                .iter()
                .any(|&d| d == target || reaches(d, target, depth - 1))
        })
    }

    for (a, b) in [("approx", "idx"), ("approx", "posmod")] {
        assert!(
            reaches(a, b, 8) && reaches(b, a, 8),
            "`{a}` and `{b}` are supposed to be mutually reachable — if that is no longer true, \
             see this test's doc before changing how the registry links itself"
        );
    }
}

/// AR.20.1 — the module ABI, forced through the same fast==slow discipline bands 1-4 got.
///
/// The POC renders `children()` behind a bound parameter, which is the smallest module that
/// exercises everything the function side has no analogue for: an argument already matched by the
/// evaluator's two-phase rule, the call-site child COUNT, and children rendered LATE in the
/// caller's scope. Compared as MESHES rather than values, because a module's output is geometry.
#[test]
fn the_module_native_matches_the_interpreter() {
    // The SAME program twice, with the compiled tier ON and then OFF. That is the only honest
    // comparison: an earlier version of this test used two DIFFERENT sources and caught itself —
    // wrapping the body in `union()` changes the tree legitimately, so it was measuring a source
    // difference and calling it a tier difference.
    let src = "module _fab_poc_mod(k=1) { children(); }\n\
               _fab_poc_mod() { cube([2,3,4]); sphere(r=1); }";

    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, _) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{geo:?}")
    };

    let compiled = run(true);
    let interpreted = run(false);
    assert_eq!(
        compiled, interpreted,
        "the compiled module and the interpreted one built different geometry"
    );
    assert!(
        compiled.contains("Leaf"),
        "the POC produced no geometry — the comparison would hold on two empty trees: {compiled}"
    );
}

/// AR.20.5 — DISPATCH, held to the same fast==slow discipline: a compiled module calling another
/// module must render what interpreting the whole thing renders.
///
/// Two programs, because dispatch has two destinations and only one of them is the easy case. The
/// FIRST has both modules armed (compiled → compiled, the fast path the census cares about); the
/// SECOND drifts the callee's body so it cannot wire, which makes one render PART compiled and PART
/// interpreted. That mixed shape is not a curiosity — `fractal_tree` nests 139 deep, so the depth
/// budget guarantees real renders straddle both tiers, and it is exactly where a call frame set up
/// two different ways would disagree.
///
/// The console is compared alongside the mesh on purpose. Geometry alone would not catch a wrong
/// `$children` or `$parent_modules` here: the drifted callee ECHOES both, so a compiled caller that
/// skipped either bind (or pushed the instantiation stack only on the interpreted path) shows up as
/// a console diff instead of passing quietly.
#[test]
fn a_compiled_module_dispatching_matches_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    // Both armed: the wrapper's `call` finds a native for `_fab_poc_mod` and dispatches into it.
    let compiled_callee = "module _fab_poc_mod(k=1) { children(); }\n\
                           module _fab_poc_wrap(k=1) { _fab_poc_mod(k) children(); }\n\
                           _fab_poc_wrap() { cube([2,3,4]); sphere(r=1); }";
    // The callee's body DRIFTED (it echoes), so it does not wire and the compiled wrapper hands it
    // to the interpreter — while the wrapper itself still compiles.
    let interpreted_callee = "module _fab_poc_mod(k=1) { echo(pm=$parent_modules, ch=$children); children(); }\n\
         module _fab_poc_wrap(k=1) { _fab_poc_mod(k) children(); }\n\
         _fab_poc_wrap() { cube([2,3,4]); sphere(r=1); }";

    for (label, src) in [
        ("compiled callee", compiled_callee),
        ("interpreted callee", interpreted_callee),
    ] {
        let (geo_on, msgs_on) = run(src, true);
        let (geo_off, msgs_off) = run(src, false);
        assert_eq!(
            geo_on, geo_off,
            "{label}: dispatching from a compiled module built different geometry than interpreting"
        );
        assert_eq!(
            msgs_on, msgs_off,
            "{label}: dispatching from a compiled module wrote a different console — a `$children` \
             or `$parent_modules` bind that only the interpreted path performs"
        );
        assert!(
            geo_on.contains("Leaf"),
            "{label}: produced no geometry, so the comparison held on two empty trees: {geo_on}"
        );
    }

    // The mixed program must actually have ECHOED, or the console half of this test proved nothing.
    let (_, msgs) = run(interpreted_callee, true);
    assert!(
        msgs.contains("pm") && msgs.contains("ch"),
        "the drifted callee never ran its echo, so the $-var comparison was vacuous: {msgs}"
    );

    // NON-VACUITY, and this is the part that makes the equalities above mean something: an
    // equality holds just as well when nothing compiled at all. Prove the tier layout each program
    // claims, by asking the registry the same question dispatch asks.
    let wires = |src: &str, name: &str| {
        let program = crate::parser::parse(src).expect("parses");
        program.stmts.iter().any(|s| {
            matches!(&s.kind, crate::parser::StmtKind::ModuleDef { name: n, params, body }
                if &**n == name && super::resolve_module(name, params, body, &crate::Scope::new()).is_some())
        })
    };
    assert!(
        wires(compiled_callee, "_fab_poc_wrap") && wires(compiled_callee, "_fab_poc_mod"),
        "the compiled-callee program did not arm BOTH modules, so nothing dispatched"
    );
    assert!(
        wires(interpreted_callee, "_fab_poc_wrap"),
        "the wrapper did not arm, so the mixed program never entered compiled code"
    );
    assert!(
        !wires(interpreted_callee, "_fab_poc_mod"),
        "the callee was supposed to have DRIFTED out of the registry — the mixed case is not mixed"
    );
}

/// AR.20.6 — dispatch to BUILTINS, which is what makes the compiled tier able to build anything at
/// all: a leaf module is a transform wrapping a primitive, and until this worked every generated
/// module declined the moment it reached `cube`.
///
/// The POC is `translate([s,0,0]) cube(size=s, center=true)` — a `Combinator` chosen from evaluated
/// arguments, a primitive reached through the NAMED channel (the emitter cannot positionalise a
/// builtin), and the child handed over as a thunk. Compared tier-on against tier-off, mesh and
/// console both, exactly like the user-module case.
#[test]
fn a_compiled_module_calling_builtins_matches_the_interpreter() {
    let src = "module _fab_poc_prim(s=1) { translate([s,0,0]) cube(size=s, center=true); }\n\
               _fab_poc_prim(3);";

    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let (geo_on, msgs_on) = run(true);
    let (geo_off, msgs_off) = run(false);
    assert_eq!(
        geo_on, geo_off,
        "the compiled module built different geometry than interpreting it — a builtin reached \
         through `combinator_for`/`eval_primitive` disagreeing with `dispatch_module`"
    );
    assert_eq!(msgs_on, msgs_off, "the two tiers wrote different consoles");
    assert!(
        geo_on.contains("Transform") && geo_on.contains("Leaf"),
        "the POC did not produce a transformed primitive, so this compared two trivial trees: \
         {geo_on}"
    );

    // Non-vacuity: an equality holds just as well when nothing compiled. Prove the native wired.
    let program = crate::parser::parse(src).expect("parses");
    assert!(
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name, params, body }
                if &**name == "_fab_poc_prim"
                    && super::resolve_module("_fab_poc_prim", params, body, &crate::Scope::new()).is_some())),
        "`_fab_poc_prim` did not arm, so the builtin path was never entered"
    );
}

/// AR.20.8 — AN.10 on the MODULE path: a compiled caller must bind against the parameters the
/// callee ACTUALLY has, not the ones it was compiled against.
///
/// This is the test that killed the first design. `ModuleCall` used to carry arguments already
/// positionalised, with the callee's defaults BAKED IN to fill holes, plus the parameter names the
/// emitter assumed so the runtime could check them. Writing this case showed the check was
/// insufficient: `_fab_poc_mod`'s parameter is still named `k`, so a name comparison passes, while
/// the DEFAULT moved — and a baked default is exactly what a compiled caller would have been
/// carrying. Matching at runtime instead means there is no assumption left to violate.
///
/// The wrapper here calls `_fab_poc_mod(k)` with `k` supplied, so the shadowed default is reached
/// through the CALLEE's own body. Compiled and interpreted must agree on which default that is.
#[test]
fn a_compiled_caller_binds_against_the_callee_the_program_actually_has() {
    // `_fab_poc_mod` DRIFTED (it echoes), so it interprets while the wrapper still compiles — and
    // its `k` carries a default the wrapper was never built with.
    let src = "module _fab_poc_mod(k=99) { echo(k=k); children(); }\n\
               module _fab_poc_wrap(k=1) { _fab_poc_mod(k) children(); }\n\
               _fab_poc_wrap() { cube([2,3,4]); }";

    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let (geo_on, msgs_on) = run(true);
    let (geo_off, msgs_off) = run(false);
    assert_eq!(geo_on, geo_off, "the two tiers built different geometry");
    assert_eq!(
        msgs_on, msgs_off,
        "the compiled caller bound a different `k` than the interpreter — it matched arguments \
         against a parameter list the program does not have"
    );
    assert!(
        msgs_on.contains("k = 1"),
        "`k` should be the wrapper's 1, passed through: {msgs_on}"
    );

    // And the shadowed DEFAULT is reached when the wrapper does NOT supply it. Same source, but the
    // wrapper's own default is what flows in, so this pins that a default is evaluated from the
    // callee's real definition rather than from anything baked at compile time.
    let defaulted = "module _fab_poc_mod(k=99) { echo(k=k); children(); }\n\
                     module _fab_poc_wrap(k=1) { _fab_poc_mod() children(); }\n\
                     _fab_poc_wrap() { cube([2,3,4]); }";
    let run2 = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) = crate::evaluate_geometry_with_base_config(
            defaulted,
            std::path::Path::new("."),
            &[],
            config,
        )
        .expect("renders");
        format!("{msgs:?}")
    };
    assert_eq!(
        run2(true),
        run2(false),
        "an unsupplied parameter must take the CALLEE's default, whichever tier ran the call"
    );
}

/// AR.20.3 — a `$`-read is answered off the inherited dynamic chain, so it sees the CALL SITE's
/// value rather than one frozen when the library was transpiled.
///
/// `$children` is the sharpest probe available for that: the evaluator binds it into every call
/// frame, it differs per call site by construction, and a compiled module that baked it would
/// branch on the wrong count while still rendering geometry. The POC takes a DIFFERENT branch for
/// one child than for two, so a wrong read shows up as wrong geometry rather than as an error.
#[test]
fn a_compiled_dollar_read_sees_the_call_site() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, _) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{geo:?}")
    };
    let def = "module _fab_poc_dollar(k=1) { if ($children > 1) children(1); else children(); }\n";

    // ONE child takes the `else` branch, TWO takes the `children(1)` branch — two different
    // renders from one module, decided entirely by a `$`-read.
    let one = format!("{def}_fab_poc_dollar() {{ cube([2,3,4]); }}");
    let two = format!("{def}_fab_poc_dollar() {{ cube([2,3,4]); sphere(r=1); }}");
    for src in [&one, &two] {
        assert_eq!(
            run(src, true),
            run(src, false),
            "the compiled `$children` read disagreed with the interpreter"
        );
    }
    assert_ne!(
        run(&one, true),
        run(&two, true),
        "both call sites rendered the same thing, so the `$children` branch was never taken and \
         this test would pass with the read hard-coded"
    );

    let program = crate::parser::parse(&one).expect("parses");
    assert!(
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name, params, body }
                if &**name == "_fab_poc_dollar"
                    && super::resolve_module("_fab_poc_dollar", params, body, &crate::Scope::new()).is_some())),
        "`_fab_poc_dollar` did not arm, so the compiled path was never entered"
    );
}

/// AR.20 — the `% *` modifiers, compiled. They are the two that RENDER when got wrong, so both are
/// held to the tier differential rather than to a substring check on the emitted text.
///
/// `%` must RUN its subtree (echoes, asserts and `rands` draws all fire — treating it like `*`
/// shifts the random stream and the geometry that goes wrong ends up somewhere else) and drop only
/// the geometry. `*` must not run its subtree at all.
///
/// The `*` case is the sharper one, and it is a COUNTING hazard rather than a drawing one: a
/// `*`-disabled statement is still a CHILD, so `$children` and every `children(i)` index in the
/// callee depend on it staying in the list. `_fab_poc_star` passes three children with the FIRST
/// disabled, into a callee that selects `children(1)` — so dropping the disabled child instead of
/// emptying it silently selects the cylinder where the interpreter selects the sphere.
#[test]
fn the_compiled_modifiers_match_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let bg = "module _fab_poc_bg(s=1) { %cube(s); sphere(r=s); }\n_fab_poc_bg(3);";
    let star = "module _fab_poc_dollar(k=1) { if ($children > 1) children(1); else children(); }\n\
                module _fab_poc_star(s=1) { _fab_poc_dollar(s) { *cube(s); sphere(r=s); cylinder(r=s,h=s); } }\n\
                _fab_poc_star(3);";

    for (label, src) in [("%", bg), ("*", star)] {
        let (geo_on, msgs_on) = run(src, true);
        let (geo_off, msgs_off) = run(src, false);
        assert_eq!(
            geo_on, geo_off,
            "`{label}`: the compiled modifier built different geometry than interpreting it"
        );
        assert_eq!(msgs_on, msgs_off, "`{label}`: different console");
    }

    // NON-VACUITY. Both natives must actually arm, or these equalities hold on two interpreted runs.
    for (src, name) in [(bg, "_fab_poc_bg"), (star, "_fab_poc_star")] {
        let program = crate::parser::parse(src).expect("parses");
        assert!(
            program.stmts.iter().any(|s| matches!(&s.kind,
                crate::parser::StmtKind::ModuleDef { name: n, params, body }
                    if &**n == name && super::resolve_module(name, params, body, &crate::Scope::new()).is_some())),
            "`{name}` did not arm, so the compiled path was never entered"
        );
    }

    // `%` really discarded something: the same body WITHOUT the modifier renders more.
    let unmarked = "module _plain(s=1) { cube(s); sphere(r=s); }\n_plain(3);";
    assert_ne!(
        run(bg, true).0,
        run(unmarked, true).0,
        "`%` changed nothing, so the modifier was ignored rather than honoured"
    );
}

/// AR.20.4's hoisting razor, compiled: assignments bind WHOLE-SCOPE, last-wins, blocks flattened.
/// `cube(x)` sits ABOVE `x = x + 2` and must render the reassigned value (whose self-reference
/// reads the PARAM), and `{ y = x; }` must reach the `sphere` outside the block, because a block
/// is not an assignment scope upstream. The statement-position emission this replaced rendered
/// `cube(param)` without declining — the interpreter leg (oracle-pinned by
/// `whole_scope_hoisting_matches_the_oracle`) is what catches that here.
#[test]
fn a_compiled_module_hoists_scope_assignments_like_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let src = "module _fab_poc_hoist(x=1) { cube(x); x = x + 2; { y = x; } sphere(r=y); }\n\
               _fab_poc_hoist(3);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "hoisting: the compiled module built different geometry than interpreting it"
    );
    assert_eq!(msgs_on, msgs_off, "hoisting: different console");

    // NON-VACUITY: the native must actually arm, or the equalities hold on two interpreted runs.
    let program = crate::parser::parse(src).expect("parses");
    assert!(
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name: n, params, body }
                if &**n == "_fab_poc_hoist"
                    && super::resolve_module("_fab_poc_hoist", params, body, &crate::Scope::new()).is_some())),
        "`_fab_poc_hoist` did not arm, so the compiled path was never entered"
    );

    // The assignment is LOAD-BEARING: dropping it changes the interpreted render, so a native
    // that quietly skipped assignments could not pass the tier equality above.
    let without = "module _fab_poc_plainh(x=1) { cube(x); { y = x; } sphere(r=y); }\n\
                   _fab_poc_plainh(3);";
    assert_ne!(
        run(src, false).0,
        run(without, false).0,
        "the probe's assignment changes nothing, so the tier equality is vacuous"
    );
}

/// AR.20.4's last construct, compiled: statement `echo` pushes through the interpreter's OWN
/// formatter (a named arg renders `k = 4`), the side effect lands BEFORE geometry (the A3 order
/// the I.5 gate string-compares), and echo children render as one implicit-union part. Console
/// equality is the whole point — the old emission dropped the line, warned `Ignoring unknown
/// module 'echo'`, and never rendered the children.
#[test]
fn a_compiled_module_echoes_like_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let src = "module _fab_poc_mod(k=1) { children(); }\n\
               module _fab_poc_echo(n=1) { echo(\"poc\", n, k=n+1); echo(n) sphere(r=n); _fab_poc_mod(n) cube(n); }\n\
               _fab_poc_echo(3);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "echo: compiled geometry differs from interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "echo: different console");
    // The echoes are actually THERE — equality of two empty consoles proves nothing.
    assert!(
        msgs_on.contains("poc"),
        "the echo line is missing: {msgs_on}"
    );
    assert!(
        msgs_on.contains("k = 4"),
        "the named arg must render `k = 4`: {msgs_on}"
    );

    // NON-VACUITY: both modules arm, so the compiled path really ran end to end.
    let program = crate::parser::parse(src).expect("parses");
    for name in ["_fab_poc_echo", "_fab_poc_mod"] {
        assert!(
            program.stmts.iter().any(|s| matches!(&s.kind,
                crate::parser::StmtKind::ModuleDef { name: n, params, body }
                    if &**n == name && super::resolve_module(name, params, body, &crate::Scope::new()).is_some())),
            "`{name}` did not arm, so the compiled path was never entered"
        );
    }
}

/// The console ROLLBACK with a NON-EMPTY delta — the AR.20.5 residual every earlier decline test
/// exercised only trivially. `_fab_poc_echo` pushes TWO echoes, then its dispatch to a DRIFTED
/// (unarmed) `_fab_poc_mod` with a compiled child block hits the children-to-interpreted decline:
/// the truncate must remove the native's echoes and the interpreted retake must re-emit them —
/// exactly once, in the same order.
#[test]
fn a_declining_native_rolls_back_its_echoes_exactly_once() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    // `_fab_poc_mod` deliberately drifted from its pinned reference — it must NOT arm.
    let src = "module _fab_poc_mod(k=1) { union() { children(); } }\n\
               module _fab_poc_echo(n=1) { echo(\"poc\", n, k=n+1); echo(n) sphere(r=n); _fab_poc_mod(n) cube(n); }\n\
               _fab_poc_echo(3);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "rollback: compiled geometry differs from interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "rollback: different console");
    assert_eq!(
        msgs_on.matches("poc").count(),
        1,
        "the rolled-back echo must appear exactly once after the retake: {msgs_on}"
    );

    // Vacuity guards: the caller armed (so its echoes really were pushed and rolled back), and
    // the drifted callee did NOT (so the decline really fired).
    let program = crate::parser::parse(src).expect("parses");
    let armed = |name: &str| {
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name: n, params, body }
                if &**n == name && super::resolve_module(name, params, body, &crate::Scope::new()).is_some()))
    };
    assert!(armed("_fab_poc_echo"), "the echoing caller must arm");
    assert!(!armed("_fab_poc_mod"), "the drifted callee must NOT arm");
}

/// The CSG-memo capture must not LEAK when an armed native answers a cacheable call (the AR.20
/// recon's find): the capture used to open BEFORE the native attempt and close only in
/// `PopModuleFrame` — which a native success never reaches — so the stale capture tripped the
/// LIFO debug-assert when the ENCLOSING cacheable call closed its own. The capture now opens
/// after the native attempt; this pins the shape that fired it: an interpreted cacheable OUTER
/// call whose body makes a cacheable call an armed native answers.
#[test]
fn a_native_success_inside_a_cacheable_call_does_not_leak_the_capture() {
    let config = crate::Config {
        intrinsics: true,
        csg_cache: true,
        ..crate::Config::default()
    };
    let src = "module _fab_poc_hoist(x=1) { cube(x); x = x + 2; { y = x; } sphere(r=y); }\n\
               module _outercache(a=1) { _fab_poc_hoist(a); }\n\
               _outercache(3);";
    crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
        .expect("renders without tripping the capture LIFO assert");

    // NON-VACUITY: the inner native must arm, or no native success ever met an open capture.
    let program = crate::parser::parse(src).expect("parses");
    assert!(
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name: n, params, body }
                if &**n == "_fab_poc_hoist"
                    && super::resolve_module("_fab_poc_hoist", params, body, &crate::Scope::new()).is_some())),
        "`_fab_poc_hoist` did not arm"
    );
}

/// The depth-budget HANDOVER, previously untested on the compiled tier: a recursive armed native
/// dispatches native-to-native until `MAX_MODULE_NATIVE_DEPTH` (64) exhausts, then the REST of
/// the recursion interprets — childless, so the handover is silent mid-tree and the answer must
/// be identical either way (the `fractal_tree` shape AR.20.5 called load-bearing). 100 levels
/// forces the budget past its edge on every run.
#[test]
fn a_recursive_native_hands_over_to_the_interpreter_past_the_depth_budget() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };

    let src = "module _fab_poc_rec(n=1) { if (n > 0) _fab_poc_rec(n - 1); else cube(1); }\n\
               _fab_poc_rec(100);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "depth handover: compiled recursion built different geometry than interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "depth handover: different console");

    let program = crate::parser::parse(src).expect("parses");
    assert!(
        program.stmts.iter().any(|s| matches!(&s.kind,
            crate::parser::StmtKind::ModuleDef { name: n, params, body }
                if &**n == "_fab_poc_rec"
                    && super::resolve_module("_fab_poc_rec", params, body, &crate::Scope::new()).is_some())),
        "`_fab_poc_rec` did not arm, so no native recursion ever ran"
    );
}

/// AR.14.4 band 2 — a module whose emitted body BAKES top-level constants (`UP`, `_EPSILON`)
/// arms when the program's own bindings bit-match the baked expectations, and renders exactly as
/// interpreting it does. The guard's positive half; the veto half is the test below.
#[test]
fn a_bake_guarded_module_arms_when_the_constants_match() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "UP = [0,0,1];\n\
               _EPSILON = 1e-9;\n\
               module _fab_poc_bake(s=1) { translate(UP*s) cube(_EPSILON*1e9); }\n\
               _fab_poc_bake(3);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "bake guard: compiled module built different geometry than interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "bake guard: different console");

    // NON-VACUITY: with the right bindings in the base scope the entry WIRES...
    let program = crate::parser::parse(src).expect("parses");
    let (params, body) = program
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            crate::parser::StmtKind::ModuleDef { name, params, body }
                if &**name == "_fab_poc_bake" =>
            {
                Some((params, body))
            }
            _ => None,
        })
        .expect("has the def");
    let mut good = crate::Scope::new();
    good.bind("UP", Value::num_list(vec![0.0, 0.0, 1.0]));
    good.bind("_EPSILON", Value::Num(1e-9));
    assert!(
        super::resolve_module("_fab_poc_bake", params, body, &good).is_some(),
        "matching constants must wire — nothing above dispatched natively"
    );
    // ...and an EMPTY scope (no such bindings) must not: the guard is live, not decorative.
    assert!(
        super::resolve_module("_fab_poc_bake", params, body, &crate::Scope::new()).is_none(),
        "a scope without the baked constants must NOT wire"
    );
}

/// AR.14.4 band 2, the veto half: a program that REBINDS a baked constant must not reach the
/// native (the fingerprint cannot see the rebind; the const guard must). Both tiers still agree —
/// on the INTERPRETED answer, which honors the program's own `_EPSILON`.
#[test]
fn a_rebound_baked_constant_vetoes_the_module_native() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    // `_EPSILON` rebound to 0.5: interpreting renders cube(5e8) — if the stale-baked native
    // answered instead, it would render cube(1) and the tier equality below would catch it.
    let src = "UP = [0,0,1];\n\
               _EPSILON = 0.5;\n\
               module _fab_poc_bake(s=1) { translate(UP*s) cube(_EPSILON*1e9); }\n\
               _fab_poc_bake(3);";
    let (geo_on, msgs_on) = run(src, true);
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "a rebound constant leaked into the native: tiers diverged"
    );
    assert_eq!(msgs_on, msgs_off, "rebound constant: different console");

    let program = crate::parser::parse(src).expect("parses");
    let (params, body) = program
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            crate::parser::StmtKind::ModuleDef { name, params, body }
                if &**name == "_fab_poc_bake" =>
            {
                Some((params, body))
            }
            _ => None,
        })
        .expect("has the def");
    let mut rebound = crate::Scope::new();
    rebound.bind("UP", Value::num_list(vec![0.0, 0.0, 1.0]));
    rebound.bind("_EPSILON", Value::Num(0.5));
    assert!(
        super::resolve_module("_fab_poc_bake", params, body, &rebound).is_none(),
        "a rebound `_EPSILON` must veto the native"
    );
}

/// The first REAL BOSL2 modules through the compiled path (AR.14.4 band 1): `down`, `zrot` and
/// `hexagon` from the PINNED library — armed by fingerprint match against the include'd
/// definitions themselves, not transcribed copies — must render and echo exactly as interpreting
/// them does. Band 2 rides along: `top_half` and `upcube` bake direction vectors, so their rows
/// carry the const guard and arm only because `std.scad`'s own `TOP`/`BOT`/`CENTER` match.
/// Skips when the submodule is absent, like every libs/BOSL2 consumer.
#[test]
fn armed_bosl2_band_modules_match_the_interpreter() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2");
    if !root.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) = crate::evaluate_geometry_with_base_config(
            "include <std.scad>\ndown(3) cube(2);\nzrot(45) cube(1);\nhexagon(r=4);\n\
             top_half() sphere(r=3);\nupcube([2,3,4]);\n\
             arc_copies(d=40, n=6) sphere(1);\ncuboid([10,20,30]);\n\
             diff() cuboid([8,8,8]) { tag(\"remove\") cuboid([4,4,10]); };",
            &root,
            &[],
            config,
        )
        .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let (geo_on, msgs_on) = run(true);
    let (geo_off, msgs_off) = run(false);
    assert_eq!(
        geo_on, geo_off,
        "band: compiled BOSL2 modules built different geometry than interpreting them"
    );
    assert_eq!(msgs_on, msgs_off, "band: different console");

    // NON-VACUITY: the pinned library's OWN `down` definition fingerprint-matches its native.
    let transforms = std::fs::read_to_string(root.join("transforms.scad")).expect("reads");
    let program = crate::parser::parse(&transforms).expect("parses");
    let armed = program.stmts.iter().any(|s| matches!(&s.kind,
        crate::parser::StmtKind::ModuleDef { name, params, body }
            if &**name == "down" && super::resolve_module("down", params, body, &crate::Scope::new()).is_some()));
    assert!(armed, "`down` from the pinned library did not arm");

    // Band-2 non-vacuity: `top_half` (partitions.scad) is CONST-GUARDED on `BACK` + `TOP` (its
    // `planar` branch reads BACK) — it wires against a scope binding the library's own values,
    // and must NOT wire against a scope that rebinds one (the guard is live for real library
    // rows, not just the poc).
    let partitions = std::fs::read_to_string(root.join("partitions.scad")).expect("reads");
    let program = crate::parser::parse(&partitions).expect("parses");
    let (params, body) = program
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            crate::parser::StmtKind::ModuleDef { name, params, body } if &**name == "top_half" => {
                Some((params, body))
            }
            _ => None,
        })
        .expect("partitions.scad defines top_half");
    let mut good = crate::Scope::new();
    good.bind("BACK", Value::num_list(vec![0.0, 1.0, 0.0]));
    good.bind("TOP", Value::num_list(vec![0.0, 0.0, 1.0]));
    assert!(
        super::resolve_module("top_half", params, body, &good).is_some(),
        "`top_half` with the library's own `BACK`/`TOP` did not arm"
    );
    let mut rebound = crate::Scope::new();
    rebound.bind("BACK", Value::num_list(vec![0.0, 1.0, 0.0]));
    rebound.bind("TOP", Value::num_list(vec![0.0, 0.0, -1.0]));
    assert!(
        super::resolve_module("top_half", params, body, &rebound).is_none(),
        "`top_half` with a rebound `TOP` must not wire"
    );
}

/// AR.14.4.2 — a program that SHADOWS a builtin function must not have the armed module answer
/// with the REAL builtin. Dispatch resolves user functions first (BOSL2 itself shadows
/// `reverse`), so interpreting `down`'s `assert(is_undef(p), …)` reaches the program's
/// `is_undef` — here `false`, so the interpreted render ERRORS — while a native body that
/// compiled the call to `rt::bi::is_undef` sails past the assert and renders geometry. The
/// function-side natives veto shadows through their `builtins` guard; the module band must be
/// equally shadow-proof. Compared at the RESULT level because the divergence is error-vs-render.
#[test]
fn a_shadowed_builtin_function_does_not_leak_into_an_armed_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2");
    if !root.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        match crate::evaluate_geometry_with_base_config(
            "include <std.scad>\nfunction is_undef(x) = false;\ndown(3) cube(1);",
            &root,
            &[],
            config,
        ) {
            Ok((geo, msgs)) => format!("ok geo={geo:?} msgs={msgs:?}"),
            Err(e) => format!("err {e:?}"),
        }
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(
        on, off,
        "shadowed `is_undef`: the armed module answered with the REAL builtin"
    );
}

/// AR.14.4.3 — a compiled module's FUNCTION calls dispatch at runtime: `helper` resolves to the
/// PROGRAM's definition (no sibling table, no callee fingerprint) and `max` to the builtin, and
/// the rendered geometry matches interpreting exactly. Non-vacuity is the resolve check plus the
/// geometry itself: `helper(v=3)=4` drives cube(4), which interpretation reproduces only by
/// running the same resolution.
#[test]
fn a_compiled_module_dispatches_function_calls_like_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "function helper(v) = v + 1;\n\
               module _fab_poc_fncall(x=1) { cube(helper(v=x)); sphere(r=max(x, 2)); }\n\
               _fab_poc_fncall(3);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "fn dispatch: compiled module rendered different geometry than interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "fn dispatch: different console");
    // RAN, not merely armed — the band-1 postmortem's lesson: a native that resolves and then
    // declines mid-body leaves every equality above true and the dispatch path untested.
    assert!(
        ran > 0,
        "`_fab_poc_fncall`'s native never ran to completion"
    );
}

/// AR.14.4.2/.3 — the SHADOW half, and the reason function calls dispatch instead of baking
/// `rt::bi`: a program's `function max(a,b) = a - b;` must reach the SHADOW from a compiled body,
/// exactly as dispatch resolves it for the interpreter (user functions first). The module still
/// ARMS — correctness comes from resolution, not from refusing to compile — and the geometry
/// (sphere r = 3-2 = 1, not builtin max's 3) proves which function answered.
#[test]
fn a_shadowed_builtin_resolves_to_the_shadow_from_a_compiled_body() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let shadowed = "function helper(v) = v + 1;\n\
                    function max(a, b) = a - b;\n\
                    module _fab_poc_fncall(x=1) { cube(helper(v=x)); sphere(r=max(x, 2)); }\n\
                    _fab_poc_fncall(3);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(shadowed, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(shadowed, false);
    assert_eq!(
        geo_on, geo_off,
        "shadowed `max`: the compiled body answered with the REAL builtin"
    );
    assert_eq!(msgs_on, msgs_off, "shadowed `max`: different console");

    // The shadow changed the ANSWER (not just resolution bookkeeping): r = 3-2 = 1 vs builtin 3.
    let unshadowed = "function helper(v) = v + 1;\n\
                      module _fab_poc_fncall(x=1) { cube(helper(v=x)); sphere(r=max(x, 2)); }\n\
                      _fab_poc_fncall(3);";
    let (geo_plain, _) = run(unshadowed, true);
    assert_ne!(
        geo_on, geo_plain,
        "the shadow probe is vacuous — shadowed and unshadowed renders agree"
    );
    // And the native RAN in the shadowed program — the equality above wasn't decline-equality,
    // so the shadow really did resolve from inside compiled code.
    assert!(
        ran > 0,
        "`_fab_poc_fncall`'s native never ran in the shadowed program"
    );
}

/// AR.22 — a compiled module's `$`-SET writes the dynamic chain like the interpreter's hoisted
/// bind: the self-reference reads the INHERITED `$fab_ds` (10), the echo and the compiled child
/// block read the new value (12), and the FORWARDED call-site child (`sphere(r=$fab_ds)`) reads
/// it too — the attachment mechanism, where a parent's `$`-set reaches the children it renders.
/// Repeated calls with different arguments prove the set is per-CALL, not sticky.
#[test]
fn a_compiled_module_dollar_set_reaches_callees_and_children() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "$fab_ds = 10;\n\
               module _fab_poc_dollarset(k=1) { $fab_ds = $fab_ds + k; echo(ds=$fab_ds); _fab_poc_mod(k) { cube($fab_ds); children(); } }\n\
               _fab_poc_dollarset(2) sphere(r=$fab_ds);\n\
               _fab_poc_dollarset(5) sphere(r=$fab_ds);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "$-set: compiled module rendered different geometry than interpreting"
    );
    assert_eq!(msgs_on, msgs_off, "$-set: different console");
    assert!(
        msgs_on.contains("ds = 12") && msgs_on.contains("ds = 15"),
        "the echoes must carry the SET values (inherited 10 + k): {msgs_on}"
    );
    assert!(
        ran > 0,
        "`_fab_poc_dollarset`'s native never ran to completion"
    );
}

/// AR.22 — the TAG-FAMILY pipeline end to end, the attachment core's regression pin: `diff()`
/// hands its children to `hide()`/`show_only()`, each of which `$`-SETS and re-renders them, and
/// the `tag("remove")`ed cuboid must land in the SUBTRACTED half both tiers alike. This is the
/// program the OpenSCAD differential caught the render-point bug on: a compiled child thunk that
/// runs against its CREATOR's dynamic context drops every `$`-frame between creator and renderer
/// — `hide()`'s `$tags_hidden` never reached the cuboid, and `diff()` UNIONED what it should have
/// subtracted. The bridge (render-point scope over creator structure) is what this test pins.
#[test]
fn the_tag_family_renders_through_compiled_children_like_the_interpreter() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2");
    if !root.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) = crate::evaluate_geometry_with_base_config(
            "include <std.scad>\ndiff() cuboid([40, 25, 80]) { tag(\"remove\") left(5) cuboid([10, 10, 90]); };",
            &root,
            &[],
            config,
        )
        .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(false);
    assert_eq!(
        geo_on, geo_off,
        "tag family: a $-set was dropped between a thunk's creator and its renderer"
    );
    assert_eq!(msgs_on, msgs_off, "tag family: different console");
    assert!(
        ran > 0,
        "no native ran — the tag pipeline never exercised the bridge"
    );
}

/// The drift gate, on the MODULE path: a definition that does not match the reference the native
/// was generated from must NOT wire. Without this the native would answer for a module whose body
/// somebody changed, which is a wrong answer rather than a missed compilation.
#[test]
fn a_drifted_module_definition_does_not_wire() {
    use super::resolve_module;
    use crate::parser::{StmtKind, parse};

    let pinned = "module _fab_poc_mod(k=1) { children(); }";
    let moved = "module _fab_poc_mod(k=1) { union() { children(); } }";
    let renamed_param = "module _fab_poc_mod(j=1) { children(); }";
    // Reformatting is NOT drift: the fingerprint is over structure, spans excluded.
    let reformatted = "module _fab_poc_mod( k = 1 )\n{\n  children();\n}";

    let of = |src: &str| {
        let prog = parse(src).expect("parses");
        let stmt = prog.stmts.into_iter().next().expect("one stmt");
        match stmt.kind {
            StmtKind::ModuleDef { params, body, .. } => (params, *body),
            other => panic!("expected a module def, got {other:?}"),
        }
    };

    let (p, b) = of(pinned);
    assert!(
        resolve_module("_fab_poc_mod", &p, &b, &crate::Scope::new()).is_some(),
        "the pinned definition must wire"
    );
    let (p, b) = of(reformatted);
    assert!(
        resolve_module("_fab_poc_mod", &p, &b, &crate::Scope::new()).is_some(),
        "reformatting is not drift — the fingerprint excludes spans"
    );
    for (label, src) in [("body moved", moved), ("param renamed", renamed_param)] {
        let (p, b) = of(src);
        assert!(
            resolve_module("_fab_poc_mod", &p, &b, &crate::Scope::new()).is_none(),
            "{label}: a drifted definition must NOT wire"
        );
    }
    assert!(
        resolve_module("no_such_module", &p, &b, &crate::Scope::new()).is_none(),
        "an unregistered name never wires"
    );
}

/// AR.18 — a sibling call with a HOLE binds the callee's own default, not `undef`.
///
/// `_fab_poc_hole(x)` calls `_fab_poc_sib(x, c=3)`, filling slots 0 and 2 and leaving slot 1
/// empty. The positional `&[Value]` ABI cannot say "slot 1 was not supplied", so the emitter fills
/// it with `b`'s declared default. The distinction is the whole test: passing `Value::Undef`
/// instead would compile, run, and return `[x, undef, 3]` — a wrong ANSWER that looks like a
/// working native. That is AN.3's bug in compiled form.
/// AR.14.4.5 — nested MODULE defs in a native body register onto the interpreter's own
/// local-module stack and resolve through the ordinary dispatch.
///
/// Two programs. The FIRST pins the frame semantics: `inner` sits textually ABOVE the `w`
/// reassignment and must still see the final value (whole-scope last-wins — cuboid's sharpest
/// case; the registration is emitted AFTER the whole prelude for exactly this). The SECOND pins
/// RESOLUTION PRIORITY: the user defines a top-level `inner` that builds a sphere, and the local
/// def must win in both tiers — a compiled caller that reached the global sibling would render a
/// sphere while the interpreter renders cubes, which is a silent wrong answer, not a decline.
#[test]
fn nested_module_defs_register_and_match_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let plain = "module _fab_poc_localmod(k=1) { module inner(a) { cube([a, w, 1]); } w = k * 2; inner(3); inner(w); }\n\
                 _fab_poc_localmod(2);";
    let shadowing = "module inner(a) { sphere(r=a); }\n\
                     module _fab_poc_localmod(k=1) { module inner(a) { cube([a, w, 1]); } w = k * 2; inner(3); inner(w); }\n\
                     _fab_poc_localmod(2);\n\
                     inner(1);";
    for (label, src) in [("plain", plain), ("shadowing", shadowing)] {
        let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
        let (geo_on, msgs_on) = run(src, true);
        let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
        let (geo_off, msgs_off) = run(src, false);
        assert_eq!(geo_on, geo_off, "{label}: nested-def geometry diverged");
        assert_eq!(msgs_on, msgs_off, "{label}: different console");
        assert!(
            ran > 0,
            "{label}: `_fab_poc_localmod`'s native never ran to completion"
        );
        assert!(
            geo_on.contains("Leaf"),
            "{label}: no geometry — empty trees would agree"
        );
    }
}

/// AR.14.4.5 — nested FUNCTION defs bind as closures at their hoist positions, through
/// `register_local_fn` + `call_fn`'s rung-1 invoke.
///
/// One program, four pins riding the console/geometry equality: `pre` calls `f` BEFORE its hoist
/// position (unknown in both tiers — the warning is the position-correctness proof), `f` captures
/// the EARLIER local `b`, `f` calls the sibling `g` defined BELOW it (the letrec group), and `g`
/// self-recurses. `c = f(2)` runs in the PRELUDE, so the invoke path is exercised before any
/// geometry statement.
#[test]
fn nested_fn_defs_register_and_match_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "module _fab_poc_localfn(k=1) { pre = f(k); b = k + 1; function f(x) = g(x) + b; function g(x) = x <= 0 ? 0 : g(x - 1) + 1; c = f(2); cube([c, b, is_undef(pre) ? 1 : 9]); }\n\
               _fab_poc_localfn(1);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(geo_on, geo_off, "nested-fn geometry diverged");
    assert_eq!(msgs_on, msgs_off, "nested-fn console diverged");
    assert!(
        msgs_on.contains("Ignoring unknown function 'f'"),
        "the pre-position call must MISS the not-yet-bound local (both tiers warn): {msgs_on}"
    );
    assert!(
        ran > 0,
        "`_fab_poc_localfn`'s native never ran to completion"
    );
    assert!(
        geo_on.contains("Leaf"),
        "no geometry — empty trees would agree"
    );
}

/// AR.14.4.5 × AR.20.7 — a LOCAL module taking children from a compiled call site is the decline
/// path, permanently: a local def can never be armed, so the compiled child block always meets an
/// interpreted callee. The native must decline (NOT complete — `ran == 0` distinguishes the
/// decline from a fingerprint miss, which the direct `resolve_module` probe rules out), the
/// rollback must leave the console clean, and the interpreted re-run must answer identically.
/// This is `half_of`'s exact shape, so the cost of the decline is measured content, not theory.
#[test]
fn a_local_module_taking_compiled_children_declines_and_matches() {
    use crate::parser::{StmtKind, parse};
    let def = "module _fab_poc_localmodkids(k=1) { module wrap() { children(); translate([k*2,0,0]) children(); } wrap() cube(k); }";
    // Armed, provably — otherwise `ran == 0` below would also pass for a drifted reference.
    let prog = parse(def).expect("parses");
    let Some(StmtKind::ModuleDef { params, body, .. }) = prog.stmts.first().map(|s| &s.kind) else {
        panic!("expected a module def");
    };
    assert!(
        super::resolve_module("_fab_poc_localmodkids", params, body, &crate::Scope::new())
            .is_some(),
        "the POC must wire before the decline can mean anything"
    );

    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = format!("{def}\n_fab_poc_localmodkids(3);");
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(&src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(&src, false);
    assert_eq!(
        geo_on, geo_off,
        "the declined native left different geometry"
    );
    assert_eq!(
        msgs_on, msgs_off,
        "the declined native left a different console"
    );
    assert_eq!(
        ran, 0,
        "the native COMPLETED — a compiled child block reached an interpreted local def, \
         which AR.20.7 says must decline"
    );
    assert!(
        geo_on.contains("Leaf"),
        "no geometry — empty trees would agree"
    );
}

/// AD.1, compiled (AR.14.4.5's rung-1 invoke): a PARAMETER holding a function value is the callee.
/// The interpreter's oracle-pinned rule is that a local binding holding a closure shadows any
/// like-named function in call position — `call_fn` used to DECLINE this shape (the whole module
/// re-interpreted, `ran == 0`); it now runs the interpreter's own `CallValue` machinery, so the
/// `ran > 0` assertion is what distinguishes the invoke from the old decline.
#[test]
fn a_param_holding_a_closure_is_invoked_by_the_native() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "module _fab_poc_callparam(f) { v = f(4); cube([v, 1, 1]); }\n\
               _fab_poc_callparam(function(x) x + 1);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(geo_on, geo_off, "closure-param geometry diverged");
    assert_eq!(msgs_on, msgs_off, "closure-param console diverged");
    assert!(
        ran > 0,
        "the native declined — rung 1 should INVOKE a param-held closure now"
    );
    assert!(
        geo_on.contains("Leaf"),
        "no geometry — empty trees would agree"
    );
}

/// AR.14.4.5 on REAL content: `cuboid` — the most-instantiated module in BOSL2, and the band's
/// prize — runs COMPILED with its five nested module defs registered (`corner_shape` reads the
/// post-reassignment `size`/`chamfer`/`rounding`, `xtcyl`/`tsphere` read `teardrop` — all through
/// the materialized frame). Three argument shapes drive the plain, chamfered and rounded paths;
/// `half_of` rides along as the real-content decline shape (its nested defs take children, so
/// equality is the assertion, not `ran`).
#[test]
fn bosl2_cuboid_runs_compiled_with_its_nested_defs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2");
    if !root.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, &root, &[], config).expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let programs = [
        ("plain", "include <std.scad>\ncuboid([8, 6, 4]);"),
        (
            "chamfered",
            "include <std.scad>\ncuboid([8, 6, 4], chamfer=1);",
        ),
        (
            "rounded",
            "include <std.scad>\ncuboid([8, 6, 4], rounding=2, $fn=16);",
        ),
        (
            "half_of",
            "include <std.scad>\nhalf_of(UP) sphere(d=8, $fn=16);",
        ),
    ];
    for (label, src) in programs {
        let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
        let (geo_on, msgs_on) = run(src, true);
        let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
        let (geo_off, msgs_off) = run(src, false);
        assert_eq!(geo_on, geo_off, "{label}: compiled BOSL2 render diverged");
        assert_eq!(msgs_on, msgs_off, "{label}: different console");
        assert!(geo_on.contains("Leaf"), "{label}: no geometry");
        if label != "half_of" {
            assert!(ran > 0, "{label}: no module native completed a run");
        }
    }
}

/// The thunk-bridge lexical split (AR.14.4.5's adversarial finding 1): a registered nested fn
/// called from INSIDE a compiled child block, rendered under ANOTHER module's ctx. The AR.22
/// bridge replaced the whole scope with the render point's — right for the `$`-chain, wrong for
/// the creator's LEXICAL frame where the letrec closure lives — so `h` answered warn-and-`undef`
/// and the cube rendered WRONG GEOMETRY while both tiers returned Ok. This is `edge_profile_asym`
/// live: its per-edge helpers are called from `default_tag`'s thunks. Both the POC (with a
/// ran-counter, so it provably went compiled) and the BOSL2 program (geometry-bearing) pin it.
#[test]
fn a_nested_fn_called_from_a_compiled_child_block_resolves() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    let src = "module _fab_poc_mod(k=1) { children(); }\n\
               module _fab_poc_localfnthunk(k=1) { function h(x) = x * 2; _fab_poc_mod(k) { cube(h(k)); } }\n\
               _fab_poc_localfnthunk(3);";
    let before = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get);
    let (geo_on, msgs_on) = run(src, true);
    let ran = crate::eval::module_rt::NATIVE_MODULE_RUNS.with(std::cell::Cell::get) - before;
    let (geo_off, msgs_off) = run(src, false);
    assert_eq!(
        geo_on, geo_off,
        "the thunk lost the creator's lexical frame"
    );
    assert_eq!(
        msgs_on, msgs_off,
        "console diverged (an `Ignoring unknown function` leak)"
    );
    assert!(
        !msgs_on.contains("Ignoring unknown function"),
        "h must RESOLVE, not warn: {msgs_on}"
    );
    assert!(
        ran > 0,
        "the POC never ran compiled — the bridge was not exercised"
    );
    assert!(
        geo_on.contains("Leaf"),
        "no geometry — empty trees would agree"
    );

    // Live: edge_profile_asym's helpers through default_tag's thunks, geometry-bearing.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2");
    if !root.join("std.scad").exists() {
        eprintln!("skipping BOSL2 half: libs/BOSL2 submodule not checked out");
        return;
    }
    let run_b = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, &root, &[], config).expect("renders");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    for src in [
        "include <std.scad>\ncuboid(50) edge_profile_asym(BOT+FWD, flip=true) square(10);",
        "include <std.scad>\ndiff() cuboid(50) edge_profile_asym(FRONT, flip=true) mask2d_roundover(10, $fn=12);",
    ] {
        let (geo_on, msgs_on) = run_b(src, true);
        let (geo_off, msgs_off) = run_b(src, false);
        assert_eq!(
            geo_on, geo_off,
            "edge_profile_asym geometry diverged: {src}"
        );
        assert_eq!(
            msgs_on, msgs_off,
            "edge_profile_asym console diverged: {src}"
        );
        assert!(
            !msgs_on.contains("Ignoring unknown function"),
            "the helpers must resolve: {msgs_on}"
        );
    }
}

/// Recursion-verdict PARITY across the tier seam (adversarial finding 2): a mutual-recursion
/// cycle between a compiled body's local module and an interpreted user module must trip the
/// depth guard at the SAME rung — same module name, same span — as interpreting the whole
/// program. Before the fix the native run took no `module_depth` level, so the cycle's parity
/// was off by one and the two tiers named different modules in the error.
#[test]
fn a_recursion_cycle_across_the_tier_seam_trips_the_same_verdict() {
    let src = "module cube(v) { inner(0); }\n\
               module _fab_poc_localmod(k=1) { module inner(a) { cube([a, w, 1]); } w = k * 2; inner(3); inner(w); }\n\
               _fab_poc_localmod(2);";
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
            .expect_err("infinite mutual recursion must error")
    };
    let on = format!("{:?}", run(true));
    let off = format!("{:?}", run(false));
    assert_eq!(
        on, off,
        "the recursion verdict (module name + span) must not depend on which tier ran the call"
    );
    assert!(on.contains("Recursion detected"), "wrong error class: {on}");
}

/// Terminal-assert parity through NESTED driver re-entries (adversarial finding 3, pre-existing
/// since AR.20.1 but inherited by every armed module): the L.5.8 export-what-came-before rule is
/// a TOP-LEVEL rule, and the compiled tier's children/callee re-entries were applying it one
/// level deep — swallowing the terminal error, exporting a partial subtree, letting LATER
/// top-level statements run, and printing a second terminal ERROR. All three shapes pinned.
#[test]
fn a_terminal_assert_inside_compiled_children_halts_like_the_interpreter() {
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("the assert rule is non-fatal at top level — this still renders Ok");
        (format!("{geo:?}"), format!("{msgs:?}"))
    };
    for (label, src) in [
        (
            "partial subtree must not export",
            "module _fab_poc_mod(k=1) { children(); }\n\
             _fab_poc_mod(1) { cube(3); assert(false, \"boom\"); }",
        ),
        (
            "later statements must not run",
            "module _fab_poc_mod(k=1) { children(); }\n\
             _fab_poc_mod(1) { cube(3); assert(false, \"boom\"); }\n\
             sphere(5, $fn=8);\n\
             echo(\"after\");",
        ),
        (
            "only ONE terminal error prints",
            "module _fab_poc_mod(k=1) { children(); }\n\
             _fab_poc_mod(1) assert(false, \"one\");\n\
             _fab_poc_mod(1) assert(false, \"two\");",
        ),
        (
            "the $-set band's compiled block, same rule",
            "$fab_ds = 1;\n\
             module _fab_poc_mod(k=1) { children(); }\n\
             module _fab_poc_dollarset(k=1) { $fab_ds = $fab_ds + k; echo(ds=$fab_ds); _fab_poc_mod(k) { cube($fab_ds); children(); } }\n\
             _fab_poc_dollarset(2) assert(false, \"boom\");",
        ),
    ] {
        let (geo_on, msgs_on) = run(src, true);
        let (geo_off, msgs_off) = run(src, false);
        assert_eq!(geo_on, geo_off, "{label}: geometry diverged");
        assert_eq!(msgs_on, msgs_off, "{label}: console diverged");
    }
}

/// AR.17 stage C — first-class functions through the flipped ABI, both emitted shapes, proven at
/// SOURCE level with the console as the witness (`echo` output must match tier-on vs tier-off).
///
/// `_fab_poc_callshadow` is the AN.10 rung-1 rule INLINE: its param `last` shadows the generated
/// sibling of the same name, so a closure argument is INVOKED through `fx.call_value` and a
/// non-function argument falls through to the sibling — `is_vector`'s `all_nonzero` shape, the
/// single gate 12 of stage A's blocked migrations stood behind. `_fab_poc_curried2` is the
/// computed callee (`fs[i](x)`). Both entries are proven ARMABLE against the exact sources the
/// test evaluates (`resolve` — the fingerprint gate), so agreement is tier agreement, not two
/// interpreters agreeing with each other.
#[test]
fn first_class_functions_run_through_the_flipped_abi() {
    use crate::parser::{StmtKind, parse};
    let last_ref = reference_of("last").expect("registered");
    let shadow_ref = reference_of("_fab_poc_callshadow").expect("registered");
    let curried_ref = reference_of("_fab_poc_curried2").expect("registered");
    for name in ["_fab_poc_callshadow", "_fab_poc_curried2"] {
        let r = reference_of(name).expect("registered");
        let prog = parse(r).expect("parses");
        let Some(StmtKind::FunctionDef { params, body, .. }) = prog.stmts.first().map(|s| &s.kind)
        else {
            panic!("expected a function def");
        };
        assert!(
            resolve(name, params, body).is_some(),
            "{name} must wire before agreement means anything"
        );
    }
    let src = format!(
        "{last_ref}\n{shadow_ref}\n{curried_ref}\n\
         echo(a=_fab_poc_callshadow(function(v) v * 10, 4));\n\
         echo(b=_fab_poc_callshadow([7, 8, 9], [1, 2]));\n\
         echo(c=_fab_poc_curried2([function(y) y * 2, function(y) y + 9], 1, 5));\n\
         echo(d=_fab_poc_curried2([1, 2], 0, 5));"
    );
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) =
            crate::evaluate_geometry_with_base_config(&src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{msgs:?}")
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(on, off, "first-class function shapes diverged across tiers");
    // Pin the VALUES, not just agreement: the closure branch (40), the non-function fallthrough
    // to the named `last` (2), the indexed closure (14), and the non-function callee's silent
    // undef — both tiers answering wrong together would still agree.
    for want in ["a = 40", "b = 2", "c = 14", "d = undef"] {
        assert!(on.contains(want), "missing `{want}` in {on}");
    }
}

/// AR.17.2 — minted literals end-to-end at SOURCE level, every census position through the real
/// emitter: returned (a), let-bound letrec (c), call-argument with a computed callee on the
/// result (d), list elements with DISTINCT paths (e, g). The console is the witness — including
/// `str()` of a minted value (g), which pins `repr` against the interpreter's own rendering, and
/// the AH.2.7 identity pins (h, i): two mints are two identities, an alias is the same one.
///
/// The wiring asserts run through `resolve` (the fingerprint gate) AND `build_intrinsics` would
/// still guard-decline silently — the stage-C lesson — so the values are pinned too: a
/// guard-declined native makes the tiers trivially agree, but `a = 7` catching a wrong answer
/// requires the answer, not the agreement.
#[test]
fn minted_literals_run_through_the_native_tier() {
    use crate::parser::{StmtKind, parse};
    let names = [
        "_fab_poc_mint_ret",
        "_fab_poc_mint_letrec",
        "_fab_poc_mint_id",
        "_fab_poc_mint_arg",
        "_fab_poc_mint_list",
    ];
    let mut refs = String::new();
    for name in names {
        let r = reference_of(name).expect("registered");
        let prog = parse(r).expect("parses");
        let Some(StmtKind::FunctionDef { params, body, .. }) = prog.stmts.first().map(|s| &s.kind)
        else {
            panic!("expected a function def");
        };
        assert!(
            resolve(name, params, body).is_some(),
            "{name} must wire before agreement means anything"
        );
        refs.push_str(r);
        refs.push('\n');
    }
    let src = format!(
        "{refs}\
         echo(a=_fab_poc_mint_ret(3)(4));\n\
         f = _fab_poc_mint_ret(10);\n\
         echo(b=f(5));\n\
         echo(c=_fab_poc_mint_letrec(5));\n\
         echo(d=_fab_poc_mint_arg(2));\n\
         echo(e=_fab_poc_mint_list(5)[0](1));\n\
         echo(g=str(_fab_poc_mint_list(5)[1]));\n\
         p = _fab_poc_mint_ret(1);\n\
         q = _fab_poc_mint_ret(1);\n\
         r = p;\n\
         echo(h=p == q, i=p == r);"
    );
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) =
            crate::evaluate_geometry_with_base_config(&src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{msgs:?}")
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(on, off, "minted-literal shapes diverged across tiers");
    for want in [
        "a = 7",
        "b = 15",
        "c = 120",
        "d = 12",
        "e = 6",
        "function(x) (x - a)",
        "h = false, i = true",
    ] {
        assert!(on.contains(want), "missing `{want}` in {on}");
    }
}

/// AR.17.2.3 — the first REAL mint slice at source level: fnliterals' currying factories
/// (nested literals, partial-application branches, a zero-param literal), `reduce`'s letrec
/// worker riding a captured parameter, and `f_gt`'s literal-as-argument + computed-callee
/// shape — the 71-strong f_* battery's exact form — all through their verbatim registry
/// references. Values pinned per the stage-C lesson: agreement alone survives a guard-decline.
#[test]
fn the_fnliterals_mint_slice_runs_native() {
    use crate::parser::{StmtKind, parse};
    let names = [
        "reduce",
        "f_1arg",
        "f_2arg",
        "f_2arg_simple",
        "f_3arg",
        "f_gt",
    ];
    let mut refs = String::new();
    for name in names {
        let r = reference_of(name).expect("registered");
        let prog = parse(r).expect("parses");
        let Some(StmtKind::FunctionDef { params, body, .. }) = prog.stmts.first().map(|s| &s.kind)
        else {
            panic!("expected a function def");
        };
        assert!(
            resolve(name, params, body).is_some(),
            "{name} must wire before agreement means anything"
        );
        refs.push_str(r);
        refs.push('\n');
    }
    // Every factory call sits in STATEMENT position — the skeptic pass proved top-level
    // assignment RHS evaluates at HOIST time, which never dispatches intrinsics, so an
    // assignment-shaped test exercises the interpreter twice and calls it agreement.
    let src = format!(
        "{refs}\
         echo(a=reduce(function(x,y) x + y, [1, 2, 3, 4]));\n\
         echo(b=f_gt(5, 2)(), c=f_gt(2, 5)());\n\
         echo(f=str(f_gt(1, 2)));\n\
         echo(d=(f_3arg(function(x,y,z) x * 100 + y * 10 + z)(1, undef, 3))(5));\n\
         echo(e=(f_1arg(function(x) x * 3)(undef))(7));\n\
         echo(g=(f_2arg(function(x,y) x - y)(undef, 4))(10));\n\
         echo(h=(f_2arg_simple(function(x,y) x * y)(3, undef))(6));"
    );
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) =
            crate::evaluate_geometry_with_base_config(&src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{msgs:?}")
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(on, off, "the fnliterals mint slice diverged across tiers");
    for want in [
        "a = 10",
        "b = true, c = false",
        "d = 153",
        "e = 21",
        "g = 6",
        "h = 18",
        // upstream's factories return partially-applied THUNKS — `f_gt(5,2)` is a zero-arg
        // closure, applied above; this pins the thunk's repr (a nested minted literal) too.
        "function() target_func(a, b)",
    ] {
        assert!(on.contains(want), "missing `{want}` in {on}");
    }
}

/// AR.24 — past the depth budget a native re-interprets its proven reference in the LIVE
/// evaluator: one machine, explicit stack, closures minting into the REAL table. The ladder is
/// the skeptic pass's BOSL2-idiomatic shape (recursive fold: native reduce → `call_value` →
/// nested machine → native …), which under the OLD throwaway fallback refused loudly the moment
/// a closure crossed the boundary — `deep`'s factory return (y) is exactly the previously
/// refused shape. `w` stays under the budget (both tiers native-capable), `x` crosses it.
#[test]
fn depth_declines_reinterpret_in_the_live_evaluator() {
    use crate::parser::{StmtKind, parse};
    let mut refs = String::new();
    for name in ["reduce", "f_1arg"] {
        let r = reference_of(name).expect("registered");
        let prog = parse(r).expect("parses");
        let Some(StmtKind::FunctionDef { params, body, .. }) = prog.stmts.first().map(|s| &s.kind)
        else {
            panic!("expected a function def");
        };
        assert!(
            resolve(name, params, body).is_some(),
            "{name} must wire before agreement means anything"
        );
        refs.push_str(r);
        refs.push('\n');
    }
    let src = format!(
        "{refs}\
         function nest(n) = reduce(function(a,b) n <= 1 ? a + b : b + nest(n-1), [1], 0);\n\
         deep = function(n) n <= 0 ? f_1arg(function(x) x * 2) : reduce(function(a,b) deep(n-1), [1], 0);\n\
         echo(w=nest(20));\n\
         echo(x=nest(40));\n\
         echo(y=(deep(40)(undef))(21));"
    );
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) =
            crate::evaluate_geometry_with_base_config(&src, std::path::Path::new("."), &[], config)
                .expect("renders — the depth decline must ANSWER, not refuse");
        format!("{msgs:?}")
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(on, off, "the depth boundary diverged across tiers");
    for want in ["w = 20", "x = 40", "y = 42"] {
        assert!(on.contains(want), "missing `{want}` in {on}");
    }
}

/// AR.25 — the hand-native cone runs GENERATED, end to end. Wiring pinned at `build_ctx` level
/// (`ctx.intrinsics` membership — the level a guard-decline is visible at, which output
/// differentials structurally cannot see: a declined native makes the tiers trivially agree),
/// then a tier differential with values pinned. `determinant` is the first SELF-recursive
/// generated native (the 5x5 pin forces the recursion into the 4x4 dispatch); `constrain`'s
/// matrix branch exercises `flatten` + `list_to_matrix` + `default`; `vector_angle` and
/// `affine3d_rot_from_to` are the MIGRATIONS — their hand fns are deleted, so these calls run
/// the generated cone or nothing.
#[test]
fn the_hand_native_cone_runs_generated() {
    use crate::parser::parse;
    let names = [
        "is_nan",
        "is_finite",
        "all_nonzero",
        "_list_pattern",
        "is_consistent",
        "same_shape",
        "is_def",
        "is_vector",
        "is_matrix",
        "_sum",
        "sum",
        "default",
        "flatten",
        "list_to_matrix",
        "det2",
        "det3",
        "det4",
        "determinant",
        "constrain",
        "vector_angle",
        // affine3d_rot_from_to's tail: unit/point3d/point2d/approx/idx/posmod + the affine seeds.
        "unit",
        "point3d",
        "point2d",
        "approx",
        "idx",
        "posmod",
        "affine3d_identity",
        "ident",
        "affine3d_zrot",
        "v_theta",
        "vector_axis",
        "v_abs",
        "affine3d_rot_from_to",
    ];
    let mut refs = String::from(
        "_EPSILON = 1e-9;\nPI = 3.141592653589793;\nUP = [0, 0, 1];\nRIGHT = [1, 0, 0];\n",
    );
    for name in names {
        refs.push_str(reference_of(name).expect("registered"));
        refs.push('\n');
    }
    let program = parse(&refs).expect("the cone parses");
    // Wiring at the level FAB_EXPLAIN reports, POST-hoist arming included (the const-guarded
    // entries — all_nonzero, is_vector — only arm after the globals publish): FnOracle::new is
    // the whole arming path in one testable unit.
    {
        use crate::parser::StmtKind as SK;
        let functions: Vec<(&str, &[crate::Parameter], &crate::Expr)> = program
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                SK::FunctionDef { name, params, body } => {
                    Some((name.as_str(), params.as_slice(), body))
                }
                _ => None,
            })
            .collect();
        let globals: Vec<(&str, &crate::Expr)> = program
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                SK::Assignment { name, value } => Some((&**name, value)),
                _ => None,
            })
            .collect();
        let oracle = crate::eval::FnOracle::new(&functions, &globals).expect("oracle builds");
        for name in names {
            assert!(
                oracle.ctx.intrinsics.contains_key(name),
                "`{name}` must WIRE (not guard-decline) for the differential to mean anything"
            );
        }
    }
    let src = format!(
        "{refs}\
         echo(dt=determinant([[6,4,-2,9],[1,-2,8,3],[1,5,7,6],[4,2,5,1]]));\n\
         echo(d5=determinant([[1,2,0,0,0],[2,1,2,0,0],[0,2,1,2,0],[0,0,2,1,2],[0,0,0,2,1]]));\n\
         echo(cn=constrain([5,-2,9], 0, 4));\n\
         echo(cm=constrain([[5,-2],[9,1]], 0, 4));\n\
         echo(va=vector_angle([1,0,0], [0,1,0]));\n\
         echo(rt=affine3d_rot_from_to([1,0,0], [0,0,1])[0]);"
    );
    let run = |intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (_, msgs) =
            crate::evaluate_geometry_with_base_config(&src, std::path::Path::new("."), &[], config)
                .expect("renders");
        format!("{msgs:?}")
    };
    let on = run(true);
    let off = run(false);
    assert_eq!(on, off, "the cone diverged across tiers");
    for want in [
        "dt = 2267", // linalg.scad's own doc example
        "d5 = 33",   // 5x5 — the self-recursion into the det4 dispatch (oracle-computed)
        "cn = [4, 0, 4]",
        "cm = [[4, 0], [4, 1]]", // the matrix branch: flatten + list_to_matrix + default
        "va = 90",
        "rt = [0, 0, -1, 0]",
    ] {
        assert!(on.contains(want), "missing `{want}` in {on}");
    }
}

/// AR.17.2 skeptic regression — a FIRED assert inside a wired native matches the interpreted
/// verdict exactly: the render COMPLETES (Ok), geometry built before the assert exports, and
/// the console carries `Error("assertion failed …")` with the REAL condition text (and the
/// evaluated message, posmod's shape). The old `bosl_assert("generated")` was a fatal `Eval`
/// that killed the render with the text lost — outcome, geometry and console all diverged,
/// and the class predated the mint band (posmod, armed since band 3).
#[test]
fn a_fired_assert_in_a_native_matches_the_interpreted_verdict() {
    use crate::parser::{StmtKind, parse};
    let assemble = |names: &[&str]| -> String {
        let mut refs = String::new();
        for name in names {
            let r = reference_of(name).expect("registered");
            let prog = parse(r).expect("parses");
            let Some(StmtKind::FunctionDef { params, body, .. }) =
                prog.stmts.first().map(|s| &s.kind)
            else {
                panic!("expected a function def");
            };
            assert!(
                resolve(name, params, body).is_some(),
                "{name} must wire before agreement means anything"
            );
            refs.push_str(r);
            refs.push('\n');
        }
        refs
    };
    let run = |src: &str, intrinsics: bool| {
        let config = crate::Config {
            intrinsics,
            ..crate::Config::default()
        };
        let (geo, msgs) =
            crate::evaluate_geometry_with_base_config(src, std::path::Path::new("."), &[], config)
                .expect("the render must COMPLETE — a fired assert is soft-caught");
        format!("{geo:?} {msgs:?}")
    };

    // The message-less shape (reduce's is_function guard), with geometry BEFORE the assert.
    let src = format!(
        "{}cube(1);\necho(z=reduce(5, [1, 2, 3]));",
        assemble(&["reduce"])
    );
    let on = run(&src, true);
    let off = run(&src, false);
    assert_eq!(on, off, "the fired-assert verdict diverged across tiers");
    assert!(
        on.contains("assertion failed [assert(is_function(func))]"),
        "missing the real assert text in {on}"
    );

    // The MESSAGE-carrying shape (posmod's finite guard) — the evaluated message is part of
    // the answer. Its cone: approx bakes _EPSILON, so the program must bind it to the bake.
    let src = format!(
        "_EPSILON = 1e-9;\n{}echo(p=posmod(1, 0));",
        assemble(&["is_nan", "is_finite", "approx", "posmod"])
    );
    let on = run(&src, true);
    let off = run(&src, false);
    assert_eq!(on, off, "the message-carrying assert diverged across tiers");
    assert!(
        on.contains("The divisor cannot be zero."),
        "missing the evaluated message in {on}"
    );
}

#[test]
fn a_sibling_call_with_a_hole_takes_the_callees_default() {
    let reference = reference_of("_fab_poc_hole").expect("registered");
    let (params, body) = parse_fn(reference);
    let func = resolve("_fab_poc_hole", &params, &body)
        .expect("its own reference must register")
        .func;
    let deps = [reference_of("_fab_poc_sib").expect("registered")];
    for input in [
        &[Value::Num(1.0)][..],
        &[Value::Num(-2.5)][..],
        &[Value::Undef][..],
        &[][..],
    ] {
        let fast = func(&crate::surface::NoClosures, input);
        let slow = interpret_with_deps(reference, &deps, input);
        assert!(
            same_result(&fast, &slow),
            "hole-filled sibling call diverged on {input:?}: fast {fast:?}"
        );
        // Pin the VALUE too, not just agreement — both tiers returning `undef` for `b` would
        // agree with each other and still be wrong against upstream.
        if let Ok(Value::NumList(xs)) = &fast {
            assert_eq!(xs[1], 7.0, "the hole must carry `b`'s default, got {xs:?}");
        }
    }
}

/// AR.26.2 — THE EMITTED REGISTRY ROWS AGREE WITH THE HAND-WRITTEN ONES.
///
/// The emitter now writes `Entry` rows beside the natives it generates, which is what lets a
/// library crate hand over its own dispatch table. That table is only worth having if it says the
/// same thing the hand table has been saying: rows whose guard lists were written, audited and
/// corrected by hand over the whole phase (AR.5a found three wrong), against rows derived
/// mechanically. Any disagreement is either an emitter bug or a hand-list bug, and both are worth
/// knowing about before AR.21 deletes the side that has been checked.
///
/// DIRECTIONAL on the guard sets, not equal: the emitted lists are the full TRANSITIVE closure over
/// the batch while a hand list is author-PRUNED (a branch no accepted argument shape can reach is
/// deliberately left out — `select`'s `all_nonzero` is the recorded example). So the emitted set
/// must CONTAIN the hand set. A superset means more guards, more declines, and a missed speedup;
/// the other direction would be a native standing on semantics nobody proved, which is the one
/// outcome this whole tier exists to prevent.
#[test]
fn the_emitted_rows_agree_with_the_hand_registry() {
    use std::collections::{BTreeMap, BTreeSet};

    let hand: BTreeMap<&str, &super::Entry> =
        super::REGISTRY.iter().map(|e| (e.name, e)).collect();
    let mut checked = 0_usize;
    for row in super::generated::REGISTRY {
        let Some(h) = hand.get(row.name) else {
            continue; // generated but not registered by hand — nothing to compare against
        };
        checked += 1;
        assert_eq!(
            row.reference, h.reference,
            "`{}`: the emitted row and the hand row describe different source",
            row.name
        );

        let emitted_deps: BTreeSet<&str> = row.deps.iter().copied().collect();
        let hand_deps: BTreeSet<&str> = h.deps.iter().copied().collect();
        assert!(
            hand_deps.is_subset(&emitted_deps),
            "`{}`: the hand row guards deps the emitted closure does not — {:?}. The emitted row \
             would wire on semantics nobody proved.",
            row.name,
            hand_deps.difference(&emitted_deps).collect::<Vec<_>>()
        );

        let emitted_b: BTreeSet<&str> = row.builtins.iter().copied().collect();
        let hand_b: BTreeSet<&str> = h.builtins.iter().copied().collect();
        assert!(
            hand_b.is_subset(&emitted_b),
            "`{}`: the hand row guards builtins the emitted closure does not — {:?}",
            row.name,
            hand_b.difference(&emitted_b).collect::<Vec<_>>()
        );

        // CONSTANTS compare as an EQUALITY, names and bits, because a bake is not a guard that can
        // safely be widened: a native either compiled that value in or it did not, and the two
        // tables have to agree about which.
        let emitted_c: BTreeSet<&str> = row.consts_v.iter().map(|&(n, _)| n).collect();
        let hand_c: BTreeSet<&str> = h
            .consts
            .iter()
            .map(|&(n, _)| n)
            .chain(h.consts_v.iter().map(|&(n, _)| n))
            .collect();
        assert_eq!(
            emitted_c, hand_c,
            "`{}`: the two rows disagree about which constants the native baked",
            row.name
        );
        for &(name, build) in row.consts_v {
            let want = h
                .consts
                .iter()
                .find(|&&(n, _)| n == name)
                .map(|&(_, x)| Value::Num(x))
                .or_else(|| {
                    h.consts_v
                        .iter()
                        .find(|&&(n, _)| n == name)
                        .map(|&(_, b)| b())
                })
                .expect("the name sets already compared equal");
            assert!(
                super::value_bits_eq(&build(), &want),
                "`{}`: baked `{name}` differs between the emitted and hand rows — {:?} vs {:?}. \
                 That is a native answering with a different constant, a wrong ANSWER rather than \
                 a missed compilation.",
                row.name,
                build(),
                want
            );
        }
    }
    // The AR.14.3 lesson, made a gate: an audit that silently checks NOTHING passes just as
    // cheerfully as one that checks everything, and a routing bug did exactly that once. The floor
    // is MEASURED (the two tables overlap on 79 of the emitted 80) rather than nominal, so a row
    // quietly falling out of one side fails here instead of shrinking the audit in silence.
    assert!(
        checked >= 79,
        "only {checked} rows compared — the emitted registry and the hand registry stopped \
         overlapping, so this test is no longer checking anything"
    );
}
