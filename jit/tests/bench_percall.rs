//! P.1.5 — the per-call measurement: how much does absorbing ONE numeric call into native code actually
//! save? The `fast==JIT` differential (corpus_diff) proves the JIT is CORRECT; this proves it's WORTH it.
//!
//! It times the JIT against the REAL interpreter dispatch — [`FnOracle::call`] runs the true `eval_with_ctx`
//! task loop (bind params, clone the lexical scope, walk the explicit task stack), the same machinery a
//! `Task::Apply` call pays per invocation — NOT the leaner `eval_expr` the spike benchmark used, which
//! understates the win. So the ratio here is the per-call lever: a function absorbed into the JIT and then
//! called N times in a comprehension saves ~this factor × N (the case that makes the list/comprehension ABI
//! worth building — `gaussian_rands` is a 300k-element `sqrt`/`ln`/`cos` loop).
//!
//! Reported, NOT gated (timing is noisy on shared CI); the loop re-checks bit-identity every iteration, so a
//! wrong JIT still FAILS here. Run `cargo test -p fab-jit --test bench_percall -- --nocapture` to see it.

use std::time::Instant;

use fab_jit::JitRegistry;
use fab_lang::{Expr, FnOracle, Parameter, Program, StmtKind, Value, parse};

/// Parse a multi-function program (kept alive so bodies/params can be borrowed from it).
fn program(src: &str) -> Program {
    parse(src).expect("program parses")
}

/// `(name, params, body)` for every function def, in source order.
fn defs(prog: &Program) -> Vec<(&str, &[Parameter], &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::FunctionDef { name, params, body } => Some((name.as_str(), params.as_slice(), body)),
            _ => None,
        })
        .collect()
}

/// Time the JIT vs the real interpreter dispatch on `name` over `iters` calls of one f64 arg, re-checking
/// bit-identity each step (a divergent JIT fails the accumulator compare). Returns `(interp_ns, jit_ns)`.
fn bench_one(reg: &JitRegistry, oracle: &FnOracle, name: &str, iters: u64) -> (u128, u128) {
    let compiled = reg.get(name).expect("function compiled");
    // Vary the argument each iteration (no cache/branch-predictor freebie); a bounded ramp keeps it finite.
    let arg = |i: u64| f64::from(u32::try_from(i & 0xffff).unwrap_or(0)) * 0.001 - 32.0;

    // Interpreter: the real task-loop dispatch, param-bound per call.
    let mut acc_interp = 0.0f64;
    let t0 = Instant::now();
    for i in 0..iters {
        match oracle.call(name, &[Value::Num(arg(i))]).expect("interprets") {
            Value::Num(n) => acc_interp += n,
            other => panic!("{name}: interpreter didn't yield a number: {other:?}"),
        }
    }
    let interp_ns = t0.elapsed().as_nanos();

    // JIT: the finalized native code.
    let mut acc_jit = 0.0f64;
    let t1 = Instant::now();
    for i in 0..iters {
        acc_jit += compiled.call(&[arg(i)], &mut [0.0]).expect("no assert raised");
    }
    let jit_ns = t1.elapsed().as_nanos();

    assert_eq!(acc_interp.to_bits(), acc_jit.to_bits(), "{name}: JIT diverged from the interpreter in-bench");
    (interp_ns, jit_ns)
}

#[test]
#[allow(clippy::cast_precision_loss, reason = "ns→ratio in a dev-only stderr timing report")]
fn per_call_speedup_vs_real_dispatch() {
    // Four shapes spanning the compiled numeric subset: pure arithmetic (Horner), a `sqrt` builtin (distance),
    // degree-trig (the transcendental hot path `gaussian_rands` exemplifies), and a ternary clamp (branchy).
    let prog = program(
        "function poly(x) = 1 + x*(2 + x*(3 + x*(4 + x*(5 + x*6))));\
         function dist(x) = sqrt(x*x + 1);\
         function wave(x) = sin(x) + cos(2*x);\
         function clamp(x) = x < 0 ? 0 : (x > 100 ? 100 : x);",
    );
    let reg = JitRegistry::build(defs(&prog).iter().map(|&(n, p, b)| (n, p, b)), std::iter::empty())
        .expect("registry builds");
    let oracle = FnOracle::new(&defs(&prog), &[]).expect("oracle builds");

    let iters = 1_000_000u64;
    eprintln!("\n[jit-bench] per-call: JIT vs the real interpreter dispatch, {iters} calls each");
    for name in ["poly", "dist", "wave", "clamp"] {
        let (interp_ns, jit_ns) = bench_one(&reg, &oracle, name, iters);
        let speedup = interp_ns as f64 / jit_ns.max(1) as f64;
        let interp_per = interp_ns as f64 / iters as f64;
        let jit_per = jit_ns as f64 / iters as f64;
        eprintln!(
            "[jit-bench]   {name:<6} interp {interp_per:6.1} ns/call  jit {jit_per:6.2} ns/call  → {speedup:5.1}x"
        );
    }
}
