//! P.1.1 — the JIT registry: many numeric functions compiled into ONE module, keyed by name, the
//! non-numeric ones DECLINED (left to the interpreter). Proves coverage (which compiled) and that a
//! registry-dispatched call is bit-identical to the interpreter — the same `fast == JIT` guarantee as
//! the standalone spike, now through the production cache.

use fab_jit::JitRegistry;
use fab_lang::{
    Expr, JitOutcome, NumericJit, Parameter, Program, StmtKind, Value, interpret_fn, parse,
};

/// Parse a multi-function program (kept alive by the caller so bodies can be borrowed from it).
fn program(src: &str) -> Program {
    parse(src).expect("program parses")
}

/// Every `(name, params, body)` in `prog`, in source order — the shape [`JitRegistry::build`] eats.
fn defs(prog: &Program) -> Vec<(&str, &[Parameter], &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::FunctionDef { name, params, body } => {
                Some((name.as_str(), params.as_slice(), body))
            }
            _ => None,
        })
        .collect()
}

/// Every top-level `name = expr` constant in `prog` — the globals half [`JitRegistry::build`] eats (P.1.4).
/// A numeric function that references one of these resolves it by inlining the value-expr.
fn consts(prog: &Program) -> Vec<(&str, &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::Assignment { name, value } => Some((name.as_ref(), value)),
            _ => None,
        })
        .collect()
}

/// The interpreter baseline for one function of `prog`, found by name — via the whole-program oracle so a
/// body that calls OTHER user functions (the chains the JIT inlines) resolves them.
fn interp(prog: &Program, name: &str, args: &[f64]) -> f64 {
    let vals: Vec<Value> = args.iter().map(|&v| Value::Num(v)).collect();
    match interpret_fn(prog, name, &vals) {
        Ok(Value::Num(n)) => n,
        other => panic!("interpreter didn't yield a number: {other:?}"),
    }
}

#[test]
fn registry_compiles_numeric_declines_the_rest() {
    // Two numeric functions compile; `pair` (a vector literal) and `pick` (indexing) are outside the
    // numeric subset and must be DECLINED — absent from the registry, so the caller interprets them.
    let prog = program(
        "function sq(x) = x*x;\
         function lerp(a,b,t) = a + (b-a)*t;\
         function pair(x) = [x, x];\
         function pick(v) = v[0];",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds");

    assert_eq!(reg.len(), 2, "exactly the two numeric functions compiled");
    assert!(reg.get("sq").is_some(), "sq is numeric → compiled");
    assert!(reg.get("lerp").is_some(), "lerp is numeric → compiled");
    assert!(reg.get("pair").is_none(), "pair builds a vector → declined");
    assert!(reg.get("pick").is_none(), "pick indexes → declined");
    assert_eq!(
        reg.compiled_names().collect::<Vec<_>>(),
        ["lerp", "sq"],
        "coverage is name-sorted (BTreeMap)"
    );
}

#[test]
fn registry_calls_are_bit_identical_to_the_interpreter() {
    let prog = program(
        "function sq(x) = x*x;\
         function lerp(a,b,t) = a + (b-a)*t;\
         function horner(x) = 1 + x*(2 + x*(3 + x*(4 + x*5)));",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds");

    // Every compiled function, called through the registry, matches the interpreter BITWISE (NaN/inf
    // corners included) — the never-silently-wrong gate carried through the cache.
    let cases: &[(&str, &[f64])] = &[
        ("sq", &[3.0]),
        ("sq", &[-0.0]),
        ("sq", &[1e8]),
        ("lerp", &[0.0, 10.0, 0.25]),
        ("lerp", &[-1.0, 1.0, 0.5]),
        ("lerp", &[2.0, 2.0, 123.0]),
        ("horner", &[0.0]),
        ("horner", &[1.5]),
        ("horner", &[-3.75]),
    ];
    for (name, args) in cases {
        let jit = reg.get(name).expect("compiled").call(args).expect("no assert raised");
        let slow = interp(&prog, name, args);
        assert_eq!(
            jit.to_bits(),
            slow.to_bits(),
            "registry {name}({args:?}): jit={jit} ({:#018x}) interp={slow} ({:#018x})",
            jit.to_bits(),
            slow.to_bits()
        );
    }
}

#[test]
fn inlining_user_function_calls_is_bit_identical() {
    // P.1.4c step 2: a call to a user function INLINES its body. sumsq inlines sq (twice); dist inlines both
    // sumsq and sqrt(builtin) — a whole call CHAIN absorbed into one compiled function. A recursive function
    // (fact) and its caller DECLINE (step-3 territory), so they're absent, but the non-recursive callers
    // compile and match the interpreter bit-for-bit.
    let prog = program(
        "function sq(x) = x*x;\
         function sumsq(a, b) = sq(a) + sq(b);\
         function dist(a, b) = sqrt(sumsq(a, b));\
         function scale(x) = let(k = sq(x)) k + sq(k);\
         function fact(n) = n <= 1 ? 1 : n * fact(n - 1);",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds");

    assert!(reg.get("sumsq").is_some(), "sumsq inlines sq");
    assert!(reg.get("dist").is_some(), "dist inlines sumsq + sqrt");
    assert!(reg.get("scale").is_some(), "scale inlines sq under a let");
    assert!(reg.get("fact").is_none(), "a recursive function declines (step 3)");

    let cases: &[(&str, &[f64])] = &[
        ("sumsq", &[3.0, 4.0]),
        ("dist", &[3.0, 4.0]),   // 5.0
        ("dist", &[-1.0, -1.0]), // sqrt(2)
        ("scale", &[2.0]),       // k=4 → 4 + 16 = 20
        ("scale", &[-1.5]),
    ];
    for (name, args) in cases {
        let jit = reg.get(name).expect("compiled").call(args).expect("no assert raised");
        let slow = interp(&prog, name, args);
        assert_eq!(
            jit.to_bits(),
            slow.to_bits(),
            "inlined {name}({args:?}): jit={jit} interp={slow}"
        );
    }
}

#[test]
fn inlining_binds_unfilled_params_to_defaults() {
    // P.1.4c defaults: a SHORT inlined call binds the missing params to their defaults (compiled in the
    // definition scope). `use_default` inlines `scaled(x)` with k defaulting to 2; `use_some` inlines
    // `bump(x, 5)` with s defaulting to 10. Bit-identical to the interpreter, which applies the same defaults.
    let prog = program(
        "function scaled(x, k = 2) = x * k;\
         function bump(x, d = 1, s = 10) = (x + d) * s;\
         function use_default(x) = scaled(x);\
         function use_some(x) = bump(x, 5);",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds");

    assert!(reg.get("use_default").is_some(), "inlines scaled with a defaulted k");
    assert!(reg.get("use_some").is_some(), "inlines bump with a defaulted s");

    let cases: &[(&str, &[f64])] = &[
        ("use_default", &[3.0]), // scaled(3, k=2) = 6
        ("use_default", &[-4.0]),
        ("use_some", &[4.0]), // bump(4, 5, s=10) = 90
        ("use_some", &[0.0]),
    ];
    for (name, args) in cases {
        let jit = reg.get(name).expect("compiled").call(args).expect("no assert raised");
        let slow = interp(&prog, name, args);
        assert_eq!(jit.to_bits(), slow.to_bits(), "default {name}({args:?}): jit={jit} interp={slow}");
    }
}

#[test]
fn references_top_level_constants_is_bit_identical() {
    // P.1.4 globals: a numeric function that reads a top-level CONSTANT compiles by inlining the constant's
    // value-expr — the self-contained BOSL2 shapes `_EPSILON = 1e-9`, `INF = 1/0` (+inf), `PHI = (1+sqrt(5))/2`.
    // A constant referencing ANOTHER global (`B = A + 1`) makes its referrer DECLINE (the safe match for the
    // interpreter's whole-scope forward-reference rule), and a vector constant declines the same way.
    let prog = program(
        "_EPS = 1e-9;\
         INF = 1/0;\
         PHI = (1 + sqrt(5)) / 2;\
         DIR = [1, 0, 0];\
         A = 1;\
         B = A + 1;\
         function near(x) = x < _EPS ? 0 : x;\
         function cap(x) = x > INF ? INF : x;\
         function golden(x) = x * PHI;\
         function use_dir(x) = x + DIR;\
         function use_chained(x) = x + B;",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    assert!(reg.get("near").is_some(), "reads self-contained constant _EPS → compiled");
    assert!(reg.get("cap").is_some(), "reads INF = 1/0 → compiled");
    assert!(reg.get("golden").is_some(), "reads PHI = (1+sqrt(5))/2 → compiled");
    assert!(reg.get("use_dir").is_none(), "reads a vector constant → declined");
    assert!(
        reg.get("use_chained").is_none(),
        "reads B, a constant that references another global → declined"
    );

    let cases: &[(&str, &[f64])] = &[
        ("near", &[1e-12]), // < _EPS → 0
        ("near", &[5.0]),   // >= _EPS → 5
        ("near", &[-1.0]),
        ("cap", &[3.0]), // never > INF → x
        ("cap", &[f64::INFINITY]),
        ("golden", &[2.0]), // 2 * PHI
        ("golden", &[-4.5]),
    ];
    for (name, args) in cases {
        let jit = reg.get(name).expect("compiled").call(args).expect("no assert raised");
        let slow = interp(&prog, name, args);
        assert_eq!(jit.to_bits(), slow.to_bits(), "global {name}({args:?}): jit={jit} interp={slow}");
    }
}

#[test]
fn bool_returning_functions_are_type_tagged_and_bit_identical() {
    // P.1.4e: a predicate / comparison body compiles and returns a BOOL — the untyped f64 ABI carries a
    // static return tag (`returns_bool`) so the dispatch wraps `Value::Bool`, not `Value::Num`. A bool literal
    // (`true`/`false`) now compiles too, so a `cond ? true : false` body works. A numeric body stays tagged num.
    let prog = program(
        "function positive(x) = x > 0;\
         function between(x) = x > 0 && x < 10;\
         function flag(x) = x > 5 ? true : false;\
         function sq(x) = x * x;",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    assert!(reg.get("positive").unwrap().returns_bool(), "a comparison body → bool");
    assert!(reg.get("between").unwrap().returns_bool(), "an && body → bool");
    assert!(reg.get("flag").unwrap().returns_bool(), "a bool-literal ternary → bool");
    assert!(!reg.get("sq").unwrap().returns_bool(), "an arithmetic body → num");

    let cases: &[(&str, f64)] = &[
        ("positive", 3.0),
        ("positive", -1.0),
        ("positive", 0.0), // 0 is not > 0 → false
        ("between", 5.0),
        ("between", -1.0),
        ("between", 10.0), // 10 is not < 10 → false
        ("flag", 6.0),
        ("flag", 2.0),
        ("sq", 4.0),
    ];
    for &(name, arg) in cases {
        let compiled = reg.get(name).expect("compiled");
        let jit = compiled.call(&[arg]).expect("no assert raised");
        let slow = interpret_fn(&prog, name, &[Value::Num(arg)]).expect("interprets");
        match slow {
            // A bool result: the JIT must be tagged bool, and its 0.0/1.0 must match the interpreter's truthiness.
            Value::Bool(b) => {
                assert!(compiled.returns_bool(), "{name}: interp bool but jit tagged num");
                assert_eq!(jit, if b { 1.0 } else { 0.0 }, "{name}({arg}) bool value mismatch");
            }
            // A number result: the JIT must be tagged num, bit-identical.
            Value::Num(n) => {
                assert!(!compiled.returns_bool(), "{name}: interp num but jit tagged bool");
                assert_eq!(jit.to_bits(), n.to_bits(), "{name}({arg}) num bits mismatch");
            }
            other => panic!("interpreter yielded neither num nor bool: {other:?}"),
        }
    }
}

#[test]
fn scalarized_vectors_are_bit_identical() {
    // P.1.6 rung A: a vector used INTERNALLY (a `[a,b,c]` literal, static index, elementwise / scale / dot
    // arithmetic) SCALARIZES — no memory — and reduces to a scalar. Bit-identical to the interpreter, incl. the
    // 4-lane `dot` (`vec*vec`). A vector-RETURNING function still declines (rung C).
    let prog = program(
        "function dot3(x) = let(v = [x, x*2, x*3], w = [1, 2, 3]) v * w;\
         function esum(x) = let(v = [x, x], w = [1, 2], s = v + w) s[0] + s[1];\
         function scaled(x) = let(v = [x, x] * 3) v[0] + v[1];\
         function mid(x) = let(p = [x, x + 1, x + 2]) (p[0] + p[2]) / 2;\
         function dot5(x) = [x, x, x, x, x] * [1, 2, 3, 4, 5];\
         function xyz(x) = let(p = [x, x + 1, x + 2]) p.x + p.y * p.z;\
         function badmember(x) = let(p = [x, x]) p.w;\
         function shortz(x) = let(p = [x, x]) p.z;\
         function mkvec(x) = [x, x + 1, x + 2];",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    assert!(reg.get("dot3").is_some(), "dot product → scalar compiles");
    assert!(reg.get("esum").is_some(), "elementwise add + index compiles");
    assert!(reg.get("scaled").is_some(), "scalar scale + index compiles");
    assert!(reg.get("mid").is_some(), "static index compiles");
    assert!(reg.get("dot5").is_some(), "a 5-element dot (exercises the 4-lane remainder) compiles");
    assert!(reg.get("xyz").is_some(), "member .x/.y/.z on a scalarized vector compiles (rung B)");
    assert!(reg.get("badmember").is_none(), "a non-xyz member (.w → undef) declines");
    assert!(reg.get("shortz").is_none(), "a .z on a too-short vector (→ undef) declines");
    assert!(reg.get("mkvec").is_none(), "a vector-RETURNING body declines (rung C)");

    let cases: &[(&str, &[f64])] = &[
        ("dot3", &[3.0]), // 14*3 = 42
        ("dot3", &[-1.5]),
        ("dot3", &[0.0]),
        ("esum", &[4.0]), // (4+1)+(4+2) = 11
        ("scaled", &[2.5]), // 6*2.5 = 15
        ("mid", &[7.0]),  // 7+1 = 8
        ("dot5", &[2.0]), // (1+2+3+4+5)*2 = 30 — but via the 4-lane reduction
        ("dot5", &[-3.25]),
        ("dot5", &[1e8]),
        ("xyz", &[3.0]), // 3 + 4*5 = 23
        ("xyz", &[-2.5]),
        ("xyz", &[0.0]),
    ];
    for (name, args) in cases {
        let jit = reg.get(name).expect("compiled").call(args).expect("no assert raised");
        let slow = interp(&prog, name, args);
        assert_eq!(jit.to_bits(), slow.to_bits(), "vector {name}({args:?}): jit={jit} interp={slow}");
    }
}

/// A `Value::NumList` from a slice of `f64`s — a scalarizable vector arg.
fn vec_arg(xs: &[f64]) -> Value {
    Value::num_list(xs.to_vec())
}

/// Assert `reg.call_numeric(name, vals)` returns a NUMERIC result BITWISE-equal to the interpreter — the
/// rung-B `fast == JIT` gate over the on-demand vector-arg path.
fn assert_call_eq(reg: &JitRegistry, prog: &Program, name: &str, vals: &[Value]) {
    let jit = match reg.call_numeric(name, vals) {
        Some(JitOutcome::Num(n)) => n,
        other => panic!("{name}{vals:?}: expected a JIT numeric result, got {other:?}"),
    };
    let slow = match interpret_fn(prog, name, vals) {
        Ok(Value::Num(n)) => n,
        other => panic!("{name}{vals:?}: interpreter didn't yield a number: {other:?}"),
    };
    assert_eq!(jit.to_bits(), slow.to_bits(), "{name}{vals:?}: jit={jit} interp={slow}");
}

#[test]
fn vector_arg_shapes_compile_on_demand() {
    // P.1.6 rung B: a function that takes a VECTOR parameter (indexed / member-accessed / dotted) declines
    // its all-scalar shape at build, then compiles ON DEMAND for the exact arg shape it's first called with.
    // Each on-demand compile is a fresh define+finalize into the already-finalized module — so this test also
    // proves Cranelift's INCREMENTAL finalize (a later batch leaves earlier code pointers valid). Every
    // result is bit-identical to the interpreter.
    let prog = program(
        "function nrm2(v) = v[0]*v[0] + v[1]*v[1] + v[2]*v[2];\
         function mag(p) = sqrt(p.x*p.x + p.y*p.y + p.z*p.z);\
         function dot(a, b) = a * b;\
         function saxpy(s, v) = s*v[0] + v[1];\
         function first(v) = v[0];\
         function sq(x) = x*x;",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    // At build, the pure-scalar shapes compile: `sq` (x*x) and `dot` (a*b — scalar multiply is fine). The
    // INDEXING / MEMBER functions DECLINE their scalar shape (you can't index a scalar) and wait for a vector
    // shape. `dot` gets BOTH — a scalar spec now, a vec*vec DOT spec on demand below.
    assert_eq!(reg.len(), 2, "the two pure-scalar shapes compiled at build (sq, dot)");
    assert!(reg.get("sq").is_some(), "the scalar control is pre-compiled");
    assert!(reg.get("dot").is_some(), "dot's scalar shape (a*b) pre-compiles too");
    assert!(reg.get("nrm2").is_none(), "an indexing function's SCALAR shape declines");
    assert!(reg.get("first").is_none());

    // On-demand vector shapes — index, member (.x/.y/.z), dot (vec*vec, incl. the 5-elem 4-lane remainder),
    // and mixed scalar+vector args. Each triggers a fresh compile; all bit-identical.
    assert_call_eq(&reg, &prog, "nrm2", &[vec_arg(&[3.0, 4.0, 12.0])]); // 169
    assert_call_eq(&reg, &prog, "nrm2", &[vec_arg(&[-1.5, 0.0, 2.0])]);
    assert_call_eq(&reg, &prog, "mag", &[vec_arg(&[3.0, 4.0, 12.0])]); // 13
    assert_call_eq(&reg, &prog, "dot", &[vec_arg(&[1.0, 2.0, 3.0]), vec_arg(&[4.0, 5.0, 6.0])]); // 32
    assert_call_eq(
        &reg,
        &prog,
        "dot",
        &[vec_arg(&[1.0, 2.0, 3.0, 4.0, 5.0]), vec_arg(&[5.0, 4.0, 3.0, 2.0, 1.0])], // 5-elem 4-lane
    );
    assert_call_eq(&reg, &prog, "saxpy", &[Value::Num(2.0), vec_arg(&[3.0, 4.0])]); // 10

    // Scalar-vs-vec-1 are DISTINCT shapes, never conflated: `first([7])` compiles + returns 7; `first(7)`
    // (a scalar indexed) DECLINES for its shape → the interpreter takes over (call_numeric None).
    assert_call_eq(&reg, &prog, "first", &[vec_arg(&[7.0])]); // vec-1 → 7
    assert_call_eq(&reg, &prog, "first", &[vec_arg(&[7.0, 8.0])]); // vec-2 → 7 (a THIRD shape of `first`)
    assert!(
        reg.call_numeric("first", &[Value::Num(7.0)]).is_none(),
        "a scalar arg to a body that indexes it declines (distinct from the vec-1 shape)"
    );

    // The scalar control still works through the same registry after all the on-demand compiles.
    assert_call_eq(&reg, &prog, "sq", &[Value::Num(6.0)]); // 36
}

#[test]
fn vector_builtins_are_bit_identical() {
    // P.1.6 rung B builtins: `norm`/`len`/`cross` over scalarized vector args, each replicating the
    // interpreter's exact computation. `nrm_cross` composes a 3D cross (a Vec) INTO norm — fully scalarized,
    // no memory. `mag5` is the load-bearing guard: a 5-element norm where the interpreter's SEQUENTIAL sum of
    // squares would diverge from a 4-lane `dot` reduction, so a bit-identical result proves norm is the
    // left-fold, not the dot.
    let prog = program(
        "function mag(v) = norm(v);\
         function mag5(v) = norm(v);\
         function count(v) = len(v);\
         function cross2(a, b) = cross(a, b);\
         function nrm_cross(a, b) = norm(cross(a, b));\
         function unit0(v) = v[0] / norm(v);",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    assert_call_eq(&reg, &prog, "mag", &[vec_arg(&[3.0, 4.0, 12.0])]); // 13
    assert_call_eq(&reg, &prog, "mag", &[vec_arg(&[0.0, 0.0])]); // 0
    // The 4-lane-vs-left-fold guard: sums of squares whose ORDER changes the rounding.
    assert_call_eq(&reg, &prog, "mag5", &[vec_arg(&[1e8, 1.0, 2.0, 3.0, 4.0])]);
    assert_call_eq(&reg, &prog, "mag5", &[vec_arg(&[-1.5, 2.25, 0.0, 1e-8, 7.0])]);
    assert_call_eq(&reg, &prog, "count", &[vec_arg(&[9.0, 9.0, 9.0])]); // 3
    assert_call_eq(&reg, &prog, "count", &[vec_arg(&[1.0, 2.0])]); // 2
    assert_call_eq(&reg, &prog, "cross2", &[vec_arg(&[1.0, 2.0]), vec_arg(&[3.0, 4.0])]); // -2
    assert_call_eq(&reg, &prog, "nrm_cross", &[vec_arg(&[1.0, 0.0, 0.0]), vec_arg(&[0.0, 1.0, 0.0])]); // 1
    assert_call_eq(&reg, &prog, "nrm_cross", &[vec_arg(&[1.0, 2.0, 3.0]), vec_arg(&[4.0, 5.0, 6.0])]);
    assert_call_eq(&reg, &prog, "unit0", &[vec_arg(&[3.0, 4.0])]); // 3/5

    // A user redefinition of a builtin WINS (interpreter resolves user functions first): here `norm` is a
    // user function returning a constant, so the JIT must inline THAT, not the builtin.
    let shadow = program("function norm(v) = 42; function f(v) = norm(v) + v[0];");
    let reg2 = JitRegistry::build(
        defs(&shadow).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&shadow).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");
    assert_call_eq(&reg2, &shadow, "f", &[vec_arg(&[5.0, 6.0])]); // 42 + 5 = 47 (user norm, not builtin)
}

#[test]
fn min_max_are_bit_identical() {
    // P.1.6 rung B: min/max reduce one vector arg, one scalar, or several scalars via jit_fmin/jit_fmax — the
    // interpreter's `f64::min`/`max` (IEEE minNum/maxNum: NaN is IGNORED, the non-NaN operand wins). That's
    // exactly why they route through helper CALLS and not Cranelift `fmin`/`fmax` (which PROPAGATE NaN) — the
    // NaN + signed-zero corners below are the guard that would fail on the native instruction.
    let prog = program(
        "function mx(a, b) = max(a, b);\
         function mn(a, b) = min(a, b);\
         function vmax(v) = max(v);\
         function vmin(v) = min(v);\
         function m1(x) = max(x);\
         function clamp(x, lo, hi) = min(max(x, lo), hi);",
    );
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");

    // Scalar 2-arg, incl. the corners that separate minNum from a NaN-propagating fmin: NaN in either slot,
    // both NaN, signed zeros, an infinity, an equal pair.
    for (a, b) in [
        (1.0, 2.0),
        (2.0, 1.0),
        (-0.0, 0.0),
        (0.0, -0.0),
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (f64::NAN, f64::NAN),
        (f64::INFINITY, 1.0),
        (-3.0, -3.0),
    ] {
        assert_call_eq(&reg, &prog, "mx", &[Value::Num(a), Value::Num(b)]);
        assert_call_eq(&reg, &prog, "mn", &[Value::Num(a), Value::Num(b)]);
    }
    // One vector arg → REDUCE (fold left-to-right), incl. a NaN mid-list and signed zeros.
    assert_call_eq(&reg, &prog, "vmax", &[vec_arg(&[3.0, 1.0, 4.0, 1.0, 5.0])]);
    assert_call_eq(&reg, &prog, "vmin", &[vec_arg(&[3.0, 1.0, 4.0, 1.0, 5.0])]);
    assert_call_eq(&reg, &prog, "vmax", &[vec_arg(&[1.0, f64::NAN, 2.0])]);
    assert_call_eq(&reg, &prog, "vmin", &[vec_arg(&[1.0, f64::NAN, 2.0])]);
    assert_call_eq(&reg, &prog, "vmax", &[vec_arg(&[-0.0, 0.0])]);
    // A single scalar arg → itself.
    assert_call_eq(&reg, &prog, "m1", &[Value::Num(7.0)]);
    // The clamp idiom (nested max-then-min), all scalar — including the NaN pass-through.
    for x in [-5.0, 0.0, 5.0, 15.0, f64::NAN] {
        assert_call_eq(&reg, &prog, "clamp", &[Value::Num(x), Value::Num(0.0), Value::Num(10.0)]);
    }
}

#[test]
fn over_long_vector_arg_declines() {
    // A vector longer than MAX_VEC_ARG (16) is NOT scalarized — unrolling it would explode compile/code size,
    // and that dynamic-length case is rung D. call_numeric declines → the interpreter runs the body.
    let prog = program("function first(v) = v[0];");
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
    .expect("registry builds");
    let seventeen: Vec<f64> = (0..17).map(f64::from).collect();
    assert!(
        reg.call_numeric("first", &[Value::num_list(seventeen)]).is_none(),
        "a 17-element vector arg exceeds the scalarization cap → declines"
    );
    // A 16-element arg is exactly at the cap → compiles + is bit-identical.
    let sixteen: Vec<f64> = (0..16).map(|i| f64::from(i) + 0.5).collect();
    assert_call_eq(&reg, &prog, "first", &[Value::num_list(sixteen)]);
}

#[test]
fn nothing_compiles_but_the_registry_holds_the_def() {
    // A program whose only function returns a vector → NOTHING compiles (a vector return is rung C, declined
    // for any arg shape). Post-rung-B, `is_empty()` means "no functions at all", NOT "nothing compiled" — the
    // registry retains the def so an on-demand VECTOR-arg shape could still compile (this one never will, but
    // the registry can't know that cheaply). So: len()==0 (no scalar spec), get() is None, and call_numeric
    // declines every shape — but the registry is not "empty".
    let prog = program("function only_list(x) = [x, x, x];");
    let reg = JitRegistry::build(
        defs(&prog).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&prog).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds even with nothing to compile");
    assert!(!reg.is_empty(), "the def is retained for on-demand recompile");
    assert_eq!(reg.len(), 0, "no all-scalar specialization compiled");
    assert!(reg.get("only_list").is_none(), "the scalar shape declines (vector return)");
    assert!(
        reg.call_numeric("only_list", &[Value::Num(3.0)]).is_none(),
        "every shape of a vector-returning body declines → interpret"
    );

    // A TRULY empty program (no user functions) IS empty — the factory installs no hook.
    let none = program("x = 1;");
    let reg = JitRegistry::build(
        defs(&none).iter().map(|&(n, p, b)| (n, p, b)),
        consts(&none).iter().map(|&(n, v)| (n, v)),
    )
        .expect("registry builds");
    assert!(reg.is_empty(), "no user functions → truly empty");
}
