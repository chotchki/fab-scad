//! Fuzz the JIT for BIT-IDENTITY with the interpreter — the one tool that can. miri and Kani both work
//! over MIR and CANNOT execute the JIT's finalized native code (a `transmute(code)` → machine-code call);
//! a fuzzer RUNS it, and under cargo-fuzz's ASan the JIT's unsafe seam (raw-pointer arena/list helpers +
//! the `unsafe extern "C"` call) is memory-checked for real. The oracle is doctrine #36: a JIT'd numeric
//! function must equal the interpreter BITWISE (an auto-FMA, a reordered sum, or an `%`/`^`/transcendental
//! routed to a differently-rounding path would each flip a bit and trip here). This is the continuous,
//! any-input sibling of the bounded `fast_eq_jit.rs` proptest — same comparison, driven by the fuzzer.
//!
//! CONSERVATIVE by design (no false trophies on an unattended run): it asserts ONLY the unambiguous case
//! — the JIT compiled the function AND both tiers produced a NUMBER → the bits must match. A body outside
//! the numeric subset (`compile_function` Errs), a raised inline assert (JIT `call` → None), or a
//! non-number interpreter result are all SKIPPED rather than asserted, so the only way to fail is a
//! genuine same-shape numeric divergence between the two tiers.
#![no_main]

use libfuzzer_sys::fuzz_target;

use fab_lang::{Scope, StmtKind, Value, eval_expr, parse};

/// True if `src` contains a run of >= 12 consecutive digits — an absurd literal no real program has, and the
/// signature of the JIT-compile OOM class (a 60-digit literal in a wide nested list).
fn has_huge_literal(src: &str) -> bool {
    let mut run = 0u32;
    for b in src.bytes() {
        if b.is_ascii_digit() {
            run += 1;
            if run >= 12 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// f64 arg batteries derived from the fuzzer bytes PLUS the fixed IEEE corners (0, ±1, div-by-zero → inf,
/// 0/0 → nan, big/small) — the corners `fast_eq_jit` pins, so the campaign starts already probing them.
fn sample_args(arity: usize, data: &[u8]) -> Vec<Vec<f64>> {
    const CORNERS: &[f64] = &[
        0.0,
        -0.0,
        1.0,
        -1.0,
        2.5,
        -3.75,
        100.0,
        1e8,
        1e-8,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    let mut out = Vec::new();
    // One all-same-corner vector per corner (cheap, hits the edges across every parameter at once).
    for &c in CORNERS {
        out.push(vec![c; arity]);
    }
    // A few vectors decoded from the trailing bytes: each 8-byte window is one f64 (bit-reinterpreted, so
    // the fuzzer can steer toward any value including its own NaN payloads). Bounded to keep it cheap.
    let mut chunks = data.chunks_exact(8);
    'outer: for _ in 0..4 {
        let mut v = Vec::with_capacity(arity);
        for _ in 0..arity {
            match chunks.next() {
                Some(b) => v.push(f64::from_le_bytes(b.try_into().unwrap())),
                None => break 'outer,
            }
        }
        out.push(v);
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // Skip pathologically-large inputs: a huge deeply-nested numeric body makes the JIT compiler expand to
    // an out-of-memory unit (a resource-exhaustion class, NOT a bit-identity divergence — see Q.5/TROPHIES).
    // Real BOSL2 functions + any reasonable generated body are well under this, so the cap costs no coverage
    // of the property under test (interp == JIT) while keeping the campaign (and the nightly CI job) alive.
    if data.len() > 4096 {
        return;
    }
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // Skip inputs carrying an absurd numeric literal (a >=12-digit run): compiling a body full of giant
    // literals + wide nested lists can OOM the JIT's analysis BEFORE it declines the (non-numeric) body — a
    // compile-complexity-budget gap (Q.5), not a bit-identity divergence. Real code never has such literals,
    // so this costs no coverage of interp==JIT.
    if has_huge_literal(src) {
        return;
    }
    let Ok(prog) = parse(src) else {
        return;
    };
    // The JIT compiles ONE `function f(nums...) = expr;` at a time — target the first def.
    let Some(stmt) = prog.stmts.first() else {
        return;
    };
    let StmtKind::FunctionDef { params, body, .. } = &stmt.kind else {
        return;
    };
    let names: Vec<&str> = params.iter().map(|p| p.name.as_ref()).collect();
    // Bound the arity: zero params has no numeric input to vary, and the sample battery stays small.
    if names.is_empty() || names.len() > 4 {
        return;
    }
    // Not in the numeric subset (lists, strings, unsupported calls) → the JIT declines. Skip; the `eval`
    // target covers those. Only a function the JIT ACCEPTS reaches the bit-identity check.
    let Ok(jitted) = fab_jit::compile_function(&names, body) else {
        return;
    };

    for args in sample_args(names.len(), data) {
        // JIT `None` = a raised inline assert or a non-numeric result — the interpreter side is then
        // Err/non-Num too, so there's nothing to compare. Only Some(number) is asserted.
        let Some(jit) = jitted.call(&args) else {
            continue;
        };
        let mut scope = Scope::new();
        for (name, &v) in names.iter().zip(&args) {
            scope.bind(*name, Value::Num(v));
        }
        if let Ok(Value::Num(slow)) = eval_expr(body, &scope) {
            assert_eq!(
                jit.to_bits(),
                slow.to_bits(),
                "interp != JIT for `{src}` at {args:?}: jit={jit} ({:#018x}) interp={slow} ({:#018x})",
                jit.to_bits(),
                slow.to_bits(),
            );
        }
    }
});
