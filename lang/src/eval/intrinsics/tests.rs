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
fn fp(src: &str) -> u64 {
    let (params, body) = parse_fn(src);
    fingerprint(&params, &body)
}

/// The SLOW side of the harness: interpret a reference function's body with its params bound to
/// `inputs`, via `eval_expr` (a default `Ctx` — NO intrinsics, so this is the pure interpreter). Returns a
/// `Result` so an inline-`assert` reference (its failure IS the reference's behavior) compares against the
/// intrinsic's error, not a panic.
fn interpret(reference: &str, inputs: &[Value]) -> crate::Result<Value> {
    let (params, body) = parse_fn(reference);
    let mut scope = Scope::new();
    for (i, p) in params.iter().enumerate() {
        // A provided arg fills the slot; an unprovided one takes the param's DEFAULT (else undef) — the
        // real call path binds defaults, so an oracle that skipped them would run a short call with the
        // wrong values (e.g. `point3d(p)` with `fill` unbound instead of `fill=0`).
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
    let mut scope = Scope::new();
    for (i, p) in params.iter().enumerate() {
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => crate::eval::eval_with_ctx(d, &scope, &ctx)?,
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
            same_result(&poc_sq(&input), &interpret(reference, &input)),
            "intrinsic vs interpreter diverged at x={x}"
        );
    }
    // A non-number arg: the intrinsic must ALSO match the interpreter's undef (x*x on a string → undef).
    let bad = [Value::string("nope")];
    assert!(
        same_result(&poc_sq(&bad), &interpret(reference, &bad)),
        "undef path must match too"
    );
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
                &super::poc_near0(&args),
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
    for (i, p) in params.iter().enumerate() {
        let v = match inputs.get(i) {
            Some(v) => v.clone(),
            None => match &p.default {
                Some(d) => crate::eval::eval_with_ctx(d, &scope, &ctx)?,
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
                &super::list_pattern(&args),
                &interpret_with_deps(lp_ref, &[], &args)
            ),
            "_list_pattern diverged on {v:?}"
        );
        let nd_ref = reference_of("num_defined").unwrap();
        assert!(
            same_result(
                &super::num_defined(&args),
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
                    &super::same_shape(&args),
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
                        &super::approx(&args),
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
                    &super::posmod(&args),
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
                    &super::idx(&args),
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
                    &super::all_nonzero(&args),
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
                    &super::is_vector(&args),
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
                        &super::is_vector(&args),
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
                    &super::is_vector(&args),
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
                            &super::is_matrix(&args),
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
                    &super::tri_class(&args),
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
                        &super::is_at_left(&args),
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
                &super::none_inside(args),
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
                    &super::sum(&args),
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
                    &super::unit(&args),
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
                    &super::apply_transform(&args),
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
                &super::bt_search(args),
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
                &super::vector_angle(args),
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
                &super::point_dist(args),
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
                    &super::is_point_on_line(&args),
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
                &super::vnf_centroid(&args),
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
                &super::group_sort_by_index(args),
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
        ("affine3d_zrot", super::affine3d_zrot as super::Intrinsic),
        ("affine3d_xrot", super::affine3d_xrot),
        ("affine3d_yrot", super::affine3d_yrot),
    ] {
        let r = reference_of(name).unwrap();
        for ang in &angles {
            let args: Vec<Value> = ang.iter().cloned().collect();
            assert!(
                same_result(
                    &func(&args),
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
                &super::get_ear(args),
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
                &super::is_path(args),
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
                &super::poc_isup(&args),
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
                &super::apply(args),
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
                &super::affine3d_translate(&args),
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
                &super::affine3d_rot_by_axis(args),
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
                &super::rot(args),
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
                &super::v_abs(&args),
                &interpret_with_deps_consts(va_ref, &iv_knot, &consts, &args)
            ),
            "v_abs diverged on {v:?}"
        );
        assert!(
            same_result(
                &super::v_theta(&args),
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
                &super::point2d(&args),
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
                &super::vector_axis(args),
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
                &super::affine3d_rot_from_to(args),
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
    // The pin anchors resolve too — a dep check needs their fingerprints.
    assert!(
        super::anchor_fp("is_range").is_some(),
        "PINS must anchor is_range"
    );
    assert!(
        super::anchor_fp("no_such_fn").is_none(),
        "an unanchored name is a registry authoring bug the dep check declines over"
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
                same_result(&func(&one), &interpret(reference, &one)),
                "{name}({input:?}) diverged"
            );
        }
        // Zero args: the single param defaults to undef in both paths.
        assert!(
            same_result(&func(&[]), &interpret(reference, &[])),
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
            same_result(&func(&one), &interpret(reference, &one)),
            "is_nan({input:?}) diverged"
        );
    }
    assert!(
        same_result(&func(&[]), &interpret(reference, &[])),
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
            same_result(&func(&one), &interpret_with_deps(reference, &deps, &one)),
            "is_finite({input:?}) diverged"
        );
    }
    assert!(
        same_result(&func(&[]), &interpret_with_deps(reference, &deps, &[])),
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
            same_result(&func(&one), &interpret(reference, &one)),
            "last({input:?}) diverged"
        );
    }
    // A longer list, to prove it's the LAST element and not the first/second.
    let long = [Value::list(
        (0..7).map(|i| Value::Num(f64::from(i))).collect::<Vec<_>>(),
    )];
    assert!(
        same_result(&func(&long), &interpret(reference, &long)),
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
            same_result(&func(&one), &interpret(reference, &one)),
            "default({v:?}) diverged"
        );
        for d in &battery {
            let two = [v.clone(), d.clone()];
            assert!(
                same_result(&func(&two), &interpret(reference, &two)),
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
            same_result(&func(&one), &interpret_with_deps(reference, &deps, &one)),
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
            same_result(&func(&one), &interpret(reference, &one)),
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
            same_result(&func(&one), &interpret(reference, &one)),
            "point3d({p:?}) diverged"
        );
        let two = [p.clone(), Value::Num(-1.0)];
        assert!(
            same_result(&func(&two), &interpret(reference, &two)),
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
                &func(inputs),
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
                &super::regions::list_wrap_val(list, &Value::Num(1e-9)),
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
                &super::regions::are_ends_equal_val(list, &Value::Num(1e-9)),
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
                &super::regions::gli_val(s1, s2, &Value::Num(1e-9)),
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
                &super::regions::mean_val(v),
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
                &super::regions::pointlist_bounds_val(pts),
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
                &super::regions::vector_search_val(q, r, target),
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
    let tree = super::regions::bt_tree_val(&small, &ind, &Value::Num(5.0)).unwrap();
    let prebuilt = Value::list(vec![small.clone(), tree.clone()]);
    let args = vec![q1.clone(), Value::Num(25.0), prebuilt.clone()];
    assert!(
        same_result(
            &super::regions::vector_search_val(&q1, &Value::Num(25.0), &prebuilt),
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
                &super::regions::bt_tree_val(pts, &ind, &Value::Num(leafsize)),
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
                &super::regions::rri_val(&args),
                &interpret_with_deps_consts(rri_ref, &deps, &consts, &args)
            ),
            "_rri diverged on closed=({c1:?},{c2:?}) eps={eps:?} r1={r1:?}"
        );
    }
}
