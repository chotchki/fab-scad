//! I.8.4 — the de-risking result: a Cranelift-JIT'd numeric function is BIT-IDENTICAL to the
//! interpreter (`fast == JIT`, the sibling of `fast == slow`). Bitwise equality proves the whole
//! float-discipline recipe at once: an auto-FMA, a reordered accumulation, or an `%`/`^` routed to a
//! differently-rounding library would each flip a bit and fail here. Plus the speedup benchmark.

use std::time::Instant;

use fab_jit::compile_function;
use fab_lang::{Expr, Program, Scope, StmtKind, Value, eval_expr, parse};
use proptest::prelude::*;

/// Parse `function f(...) = EXPR;` and hand back the parsed program (kept alive by the caller so the
/// body + parameter names can be borrowed from it).
fn program(src: &str) -> Program {
    parse(src).expect("function def parses")
}

/// The parameter names + body expr of the program's first (and only) statement.
fn func(prog: &Program) -> (Vec<&str>, &Expr) {
    match &prog.stmts[0].kind {
        StmtKind::FunctionDef { params, body, .. } => {
            (params.iter().map(|p| p.name.as_ref()).collect(), body)
        }
        other => panic!("expected a function def, got {other:?}"),
    }
}

/// The interpreter baseline: evaluate `body` with the parameters bound to `args`.
fn interp(names: &[&str], body: &Expr, args: &[f64]) -> f64 {
    let mut scope = Scope::new();
    for (name, &v) in names.iter().zip(args) {
        scope.bind(*name, Value::Num(v));
    }
    match eval_expr(body, &scope) {
        Ok(Value::Num(n)) => n,
        other => panic!("interpreter didn't yield a number: {other:?}"),
    }
}

/// Assert the JIT and the interpreter agree BITWISE on every sample (NaN payloads included).
fn assert_fast_eq_jit(src: &str, samples: &[&[f64]]) {
    let prog = program(src);
    let (names, body) = func(&prog);
    let jitted = compile_function(&names, body).expect("compiles to native code");
    for args in samples {
        let jit = jitted.call(args);
        let slow = interp(&names, body, args);
        assert_eq!(
            jit.to_bits(),
            slow.to_bits(),
            "fast != JIT for {src} at {args:?}: jit={jit} ({:#018x}) interp={slow} ({:#018x})",
            jit.to_bits(),
            slow.to_bits()
        );
    }
}

// A spread of inputs that exercises signs, magnitudes, and the IEEE corners (0, div-by-zero → inf,
// 0/0 → nan). Every sample is a 1-element slice; the 2-param cases use their own below.
const ONE: &[&[f64]] = &[
    &[0.0],
    &[1.0],
    &[-1.0],
    &[2.5],
    &[-3.75],
    &[100.0],
    &[1e8],
    &[1e-8],
    &[123.456],
    &[-0.0],
];

// Two-parameter samples for the comparison/max cases: ordered pairs, equal pairs, and NaN/inf mixes.
const TWO: &[&[f64]] = &[
    &[1.0, 2.0],
    &[2.0, 1.0],
    &[3.0, 3.0],
    &[-1.0, 1.0],
    &[0.0, -0.0],
    &[f64::NAN, 1.0],
    &[1.0, f64::NAN],
    &[f64::INFINITY, 1.0],
    &[-5.0, -5.0],
];

#[test]
fn polynomial_horner() {
    // The classic hot numeric function — a Horner-form polynomial (nested mul+add, the shape an
    // auto-FMA would fuse). Bit-identity here IS the no-FMA proof.
    assert_fast_eq_jit("function f(x) = 1 + x*(2 + x*(3 + x*(4 + x*5)));", ONE);
    assert_fast_eq_jit("function f(x) = x*x + 2*x + 1;", ONE);
}

#[test]
fn all_four_arithmetic_ops() {
    assert_fast_eq_jit("function f(x) = x + 3;", ONE);
    assert_fast_eq_jit("function f(x) = x - 7;", ONE);
    assert_fast_eq_jit("function f(x) = x * 6;", ONE);
    assert_fast_eq_jit("function f(x) = x / 2;", ONE); // and x/0 → inf at x=0? no, 0/2 = 0
    assert_fast_eq_jit("function f(x) = 10 / x;", ONE); // 10/0 → inf, 10/-0 → -inf: bit-identical
}

#[test]
fn unary_neg_and_nesting() {
    assert_fast_eq_jit("function f(x) = -x;", ONE);
    assert_fast_eq_jit("function f(x) = -(x*x) + -x;", ONE);
    assert_fast_eq_jit("function f(x) = ((x*x)*(x*x)) - x;", ONE);
}

#[test]
fn mod_and_pow_route_to_our_math() {
    // % and ^ have no deterministic native Cranelift instruction, so they compile to CALLS into the
    // interpreter's exact Rust ops (a % b, a.powf(b)). Bit-identity confirms the routing.
    assert_fast_eq_jit("function f(x) = x % 3;", ONE);
    assert_fast_eq_jit("function f(x) = x % 0;", ONE); // fmod by 0 → nan, both sides
    assert_fast_eq_jit("function f(x) = x ^ 3;", ONE);
    assert_fast_eq_jit("function f(x) = x ^ 0.5;", ONE); // sqrt of negatives → nan, both sides
    assert_fast_eq_jit("function f(x) = 2 ^ x;", ONE);
}

#[test]
fn two_parameter_functions() {
    let samples: &[&[f64]] = &[
        &[1.0, 2.0],
        &[3.5, -1.25],
        &[0.0, 5.0],
        &[7.0, 0.0], // a/0, a%0
        &[-4.0, 3.0],
        &[1e6, 1e-6],
    ];
    assert_fast_eq_jit("function f(a, b) = a*b + a/b - a;", samples);
    assert_fast_eq_jit("function f(a, b) = (a + b) * (a - b);", samples);
    assert_fast_eq_jit("function f(a, b) = a % b + b ^ a;", samples);
}

#[test]
fn unsupported_nodes_decline_cleanly() {
    // The compiler must DECLINE (not miscompile) anything outside the numeric subset.
    let prog = program("function f(x) = sin(x);"); // a call — deferred to P.1.4b
    let (names, body) = func(&prog);
    assert!(compile_function(&names, body).is_err());

    let prog = program("function f(x) = x + y;"); // free variable y
    let (names, body) = func(&prog);
    assert!(compile_function(&names, body).is_err());

    let prog = program("function f(x) = x > 0;"); // a BOOL-valued body → declines (dispatch wraps as Num)
    let (names, body) = func(&prog);
    assert!(compile_function(&names, body).is_err());
}

// A battery reaching the IEEE corners the comparison/ternary paths turn on (±0, ±inf, NaN, ordering).
const EDGE: &[&[f64]] = &[
    &[0.0],
    &[-0.0],
    &[1.0],
    &[-1.0],
    &[3.5],
    &[-3.5],
    &[1e300],
    &[f64::INFINITY],
    &[f64::NEG_INFINITY],
    &[f64::NAN],
];

#[test]
fn ternary_and_comparisons_fast_eq_jit() {
    // P.1.4a: comparisons (ORDERED `< <= > >= ==`, UNORDERED `!=`) + a ternary that SELECTS numbers, all
    // bit-identical to the interpreter across the IEEE corners — NaN (unordered → false branch), ±0, ±inf.
    assert_fast_eq_jit("function f(x) = x > 0 ? x : -x;", EDGE); // abs-like
    assert_fast_eq_jit("function f(x) = x < 0 ? -x : x;", EDGE);
    assert_fast_eq_jit("function f(x) = x >= 1 ? x*x : x + 1;", EDGE);
    assert_fast_eq_jit("function f(x) = x <= 0 ? 0 : x;", EDGE); // clamp-low
    assert_fast_eq_jit("function f(x) = x == 0 ? 1 : 1/x;", EDGE); // untaken 1/0=inf discarded
    assert_fast_eq_jit("function f(x) = x != x ? 0 : x;", EDGE); // NaN-detect (x!=x is unordered-true)
    assert_fast_eq_jit("function f(a, b) = a > b ? a : b;", TWO); // max
    assert_fast_eq_jit("function f(a, b) = a > 0 && b > 0 ? a + b : 0;", TWO); // &&
    assert_fast_eq_jit("function f(a, b) = a > 0 || b > 0 ? 1 : -1;", TWO); // ||
    assert_fast_eq_jit("function f(x) = !(x > 0) ? -1 : 1;", EDGE); // unary !
    // Nested ternary (chained clamp): lo/hi bounds.
    assert_fast_eq_jit("function f(x) = x < 0 ? 0 : (x > 10 ? 10 : x);", EDGE);
}

#[test]
fn speedup_benchmark() {
    // Measure the JIT vs the interpreter on a hot polynomial. Reported, not gated (timing is noisy on
    // shared CI) — but the loop also re-checks bit-identity, so a wrong JIT still fails here.
    let src = "function f(x) = 1 + x*(2 + x*(3 + x*(4 + x*(5 + x*6))));";
    let prog = program(src);
    let (names, body) = func(&prog);
    let jitted = compile_function(&names, body).expect("compiles");

    let iters = 2_000_000u64;
    let step = 1.0
        / f64::from(
            u32::try_from(iters % u64::from(u32::MAX))
                .unwrap_or(1)
                .max(1),
        );

    // Interpreter timing.
    let mut acc_interp = 0.0f64;
    let t0 = Instant::now();
    for i in 0..iters {
        let x = f64::from(u32::try_from(i & 0xffff).unwrap_or(0)) * step;
        acc_interp += interp(&names, body, &[x]);
    }
    let interp_ns = t0.elapsed().as_nanos();

    // JIT timing.
    let mut acc_jit = 0.0f64;
    let t1 = Instant::now();
    for i in 0..iters {
        let x = f64::from(u32::try_from(i & 0xffff).unwrap_or(0)) * step;
        acc_jit += jitted.call(&[x]);
    }
    let jit_ns = t1.elapsed().as_nanos();

    assert_eq!(acc_interp.to_bits(), acc_jit.to_bits(), "bench diverged");
    #[allow(clippy::cast_precision_loss)]
    let speedup = interp_ns as f64 / jit_ns.max(1) as f64;
    eprintln!(
        "speedup: interp {} ns, jit {} ns over {iters} calls → {speedup:.1}x",
        interp_ns, jit_ns
    );
}

proptest! {
    // Bit-identity over GENERATED quadratics + inputs. The fixed structure keeps the compiler's subset
    // in scope while the coefficients + argument roam; both engines read the SAME parsed AST, so the
    // property is fast==JIT regardless of any format/parse rounding of the literals.
    #[test]
    fn quadratic_fast_eq_jit(
        c0 in -1e6..1e6f64,
        c1 in -1e6..1e6f64,
        c2 in -1e6..1e6f64,
        x in -1e3..1e3f64,
    ) {
        let src = format!("function f(x) = {c0} + {c1}*x + {c2}*x*x;");
        let prog = program(&src);
        let (names, body) = func(&prog);
        let jitted = compile_function(&names, body).expect("compiles");
        prop_assert_eq!(jitted.call(&[x]).to_bits(), interp(&names, body, &[x]).to_bits());
    }
}
