//! P.1.3 — `fast == JIT` over the REAL BOSL2 library. The end-to-end never-silently-wrong gate: every
//! numeric-subset function the JIT compiles from the shipped `libs/BOSL2` MUST return bit-identical results
//! to interpreting its body, across a battery of inputs (IEEE corners included). A single flipped bit here —
//! an auto-FMA slipping in, a reordered accumulation, an `%`/`^` routed to a differently-rounding library —
//! fails the build, so the JIT can never silently diverge from the interpreter on a function it accepts.
//!
//! A compilable body references ONLY parameters and number literals (the compiler DECLINES any call, ternary,
//! index, or free variable), so each compiled function interprets STANDALONE — no dependency harness needed,
//! unlike the intrinsic tier. This scans the whole library, compiles what fits the subset, and differentials
//! each; it also reports the COVERAGE (how many functions compiled vs declined) — the number P.1.4 grows.

use std::path::{Path, PathBuf};

use fab_jit::JitRegistry;
use fab_lang::{Expr, FnOracle, JitOutcome, NumericJit, Parameter, Program, StmtKind, Value, parse};

/// The numeric corners the differential batteries draw from — signs, magnitudes, and the IEEE edges (±0,
/// ±inf, NaN, π). Shared by the scalar [`input_battery`] and the rung-B [`vector_rows`] so both engines see
/// the same adversarial inputs.
const CORNERS: [f64; 18] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    2.0,
    0.5,
    -0.5,
    3.5,
    -3.75,
    100.0,
    -100.0,
    1e8,
    1e-8,
    123.456,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
    std::f64::consts::PI,
];

/// The shipped BOSL2 library dir (`<workspace>/libs/BOSL2`), relative to this crate.
fn bosl2_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../libs/BOSL2")
}

/// Every `.scad` source under `dir`, parsed. Kept alive by the caller so function bodies borrow from them.
/// A file that doesn't parse is skipped (the differential is about the functions we CAN compile, not lexing).
fn parse_library(dir: &Path) -> Vec<Program> {
    let mut programs = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "scad")
                && let Ok(src) = std::fs::read_to_string(&path)
                && let Ok(prog) = parse(&src)
            {
                programs.push(prog);
            }
        }
    }
    programs
}

/// Collect `(name, params, body)` for every top-level function def across the parsed programs, LAST-wins on a
/// duplicate name (the loader's precedence). Borrows from `programs`.
fn functions(programs: &[Program]) -> Vec<(&str, &[Parameter], &Expr)> {
    let mut by_name: std::collections::BTreeMap<&str, (&[Parameter], &Expr)> =
        std::collections::BTreeMap::new();
    for prog in programs {
        for stmt in &prog.stmts {
            if let StmtKind::FunctionDef { name, params, body } = &stmt.kind {
                by_name.insert(name.as_ref(), (params.as_slice(), body));
            }
        }
    }
    by_name.into_iter().map(|(n, (p, b))| (n, p, b)).collect()
}

/// Collect every top-level CONSTANT `(name, value)` across the parsed programs, LAST-wins on a duplicate name
/// (mirrors [`functions`] precedence). The globals half the registry + the oracle both eat (P.1.4): a numeric
/// function that reads `_EPSILON`/`INF`/`PHI`/`NAN` resolves it by inlining the value-expr. Borrows from
/// `programs`. Both sides of the differential are fed THIS SAME set, so no derivation mismatch can hide a bug.
fn globals(programs: &[Program]) -> Vec<(&str, &Expr)> {
    let mut by_name: std::collections::BTreeMap<&str, &Expr> = std::collections::BTreeMap::new();
    for prog in programs {
        for stmt in &prog.stmts {
            if let StmtKind::Assignment { name, value } = &stmt.kind {
                by_name.insert(name.as_ref(), value);
            }
        }
    }
    by_name.into_iter().collect()
}

/// A deterministic input battery of `arity` f64s per row — the IEEE corners plus a spread of magnitudes and
/// signs, laid out so element `i` of each row differs from its neighbors (a bug that only bites when two args
/// differ still gets caught). A fixed xorshift keeps it reproducible with no `rand` dep.
fn input_battery(arity: usize) -> Vec<Vec<f64>> {
    let mut rows = Vec::new();
    // Uniform rows: every arg the same corner (exercises x==y paths like x-x, x/x, x%x).
    for &c in &CORNERS {
        rows.push(vec![c; arity.max(1)]);
    }
    // Mixed rows: a fixed xorshift walks the corner table so args in a row differ.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..64 {
        let row = (0..arity.max(1))
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                CORNERS[(state as usize) % CORNERS.len()]
            })
            .collect();
        rows.push(row);
    }
    rows
}

/// Interpret `name(args)` through the whole-library `oracle` — the slow side of the differential, as
/// `(value, is_bool)`. A numeric result is `(n, false)`; a BOOLEAN result (a predicate / comparison body,
/// P.1.4e) is `(0.0/1.0, true)` so it compares against the JIT's `0.0`/`1.0` return AND its type tag. `None`
/// when the body RAISES (an inline `assert` failed) or yields neither. Unlike a standalone eval, the oracle
/// resolves the callee's OWN calls (the chains the JIT inlines) and top-level constants (the globals it
/// inlines) — required now that the compiled subset reaches past leaf functions. `None` mirrors the JIT's
/// raise-`None`.
fn interpret(oracle: &FnOracle, name: &str, args: &[f64]) -> Option<(f64, bool)> {
    let vals: Vec<Value> = args.iter().map(|&v| Value::Num(v)).collect();
    match oracle.call(name, &vals) {
        Ok(Value::Num(n)) => Some((n, false)),
        Ok(Value::Bool(b)) => Some((if b { 1.0 } else { 0.0 }, true)),
        _ => None, // an assert failure (Err) or a non-number/bool → the JIT returns None here too
    }
}

#[test]
fn fast_equals_jit_over_the_bosl2_library() {
    let programs = parse_library(&bosl2_dir());
    assert!(!programs.is_empty(), "expected to parse BOSL2 sources from {}", bosl2_dir().display());
    let defs = functions(&programs);
    let consts = globals(&programs);
    let total = defs.len();

    // Registry + oracle are fed the SAME functions + constants, so neither side can diverge on its inputs.
    let registry =
        JitRegistry::build(defs.iter().map(|&(n, p, b)| (n, p, b)), consts.iter().map(|&(n, v)| (n, v)))
            .expect("registry builds");
    // Build the interpreter oracle ONCE — it publishes every top-level constant, and republishing per
    // (function × battery-row) would be quadratic over the library.
    let oracle = FnOracle::new(&defs, &consts).expect("oracle builds");

    // Every compiled function: JIT == interpreter, BITWISE, across the whole battery.
    let mut checked = 0usize;
    for (name, params, _body) in &defs {
        let Some(compiled) = registry.get(name) else { continue };
        for args in input_battery(params.len()) {
            // Pair the JIT's raw f64 with its static type tag, so a Num-vs-Bool mismatch fails too (not just
            // differing bits) — the untyped ABI must reconstruct the interpreter's Value type exactly.
            let jit = compiled.call(&args[..params.len()]).map(|f| (f, compiled.returns_bool()));
            let slow = interpret(&oracle, name, &args[..params.len()]);
            // Agree if both raised (`None` — an inline assert failed on both sides) OR both a value with the
            // SAME type tag and identical bits. A mixed Some/None, a type mismatch, or differing bits diverges.
            let agree = match (jit, slow) {
                (Some((a, at)), Some((b, bt))) => at == bt && a.to_bits() == b.to_bits(),
                (None, None) => true,
                _ => false,
            };
            assert!(agree, "fast != JIT for BOSL2 `{name}` at {args:?}: jit={jit:?} interp={slow:?}");
        }
        checked += 1;
    }

    // Coverage: how much of the real library the current numeric subset reaches. Printed (not asserted) —
    // it's the number P.1.4 (ternary/comparisons/transcendental calls) grows. `--nocapture` to see it.
    let compiled = registry.len();
    eprintln!(
        "[jit-corpus] BOSL2: {compiled}/{total} functions compiled ({:.1}%), all {checked} bit-identical",
        100.0 * compiled as f64 / total as f64
    );
    // The absorption ceiling: which node kind FIRST blocks each declined function, aggregated. Printed (not
    // asserted) so the next subset feature to add is data-driven — the number P.1.4/P.1.5 chase down.
    let mut hist: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for &reason in registry.declined().values() {
        *hist.entry(reason).or_default() += 1;
    }
    let mut rows: Vec<(&str, usize)> = hist.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    eprintln!("[jit-corpus] declined first-blocker histogram (absorption ceiling):");
    for (reason, count) in rows {
        eprintln!("[jit-corpus]   {count:>5}  {reason}");
    }
    assert_eq!(checked, compiled, "every compiled function was differentialed");
}

/// Deterministic VECTOR-arg rows: `count` rows, each `arity` `NumList`s of length `n`, drawn from [`CORNERS`]
/// by a per-shape xorshift so the vectors differ within and across rows. Seeded from `(arity, n)` so a given
/// shape always yields the same battery (reproducible failures).
fn vector_rows(arity: usize, n: usize, count: usize) -> Vec<Vec<Value>> {
    let mut state: u64 = 0xD1B5_4A32_D192_ED03 ^ ((arity as u64) << 8) ^ (n as u64);
    (0..count)
        .map(|_| {
            (0..arity)
                .map(|_| {
                    let xs: Vec<f64> = (0..n)
                        .map(|_| {
                            state ^= state << 13;
                            state ^= state >> 7;
                            state ^= state << 17;
                            CORNERS[(state as usize) % CORNERS.len()]
                        })
                        .collect();
                    Value::num_list(xs)
                })
                .collect()
        })
        .collect()
}

#[test]
fn fast_equals_jit_over_bosl2_vector_arg_shapes() {
    // P.1.6 rung B, END-TO-END over the REAL library: drive BOSL2 functions with VECTOR args through the
    // on-demand specialization path. For every (function, vector-shape) the JIT ACCEPTS (compiles a
    // specialization for that shape), the result MUST be bit-identical to the interpreter — the
    // never-silently-wrong gate extended to the vector ABI, on real code the unit tests don't reach. A DECLINED
    // shape (call_numeric → None) is the interpreter's job, so we skip it.
    //
    // Soundness note: a `None` conflates "declined this shape" with "compiled but the inline assert raised", so
    // skipping `None` doesn't verify raise-symmetry for the vector path. That's acceptable — the assert
    // condition compiles from the SAME expression the interpreter evaluates, and the scalar corpus already
    // proves the raise mechanism; the gate here is the load-bearing one: every shape the JIT ACCEPTS is exact.
    let programs = parse_library(&bosl2_dir());
    assert!(!programs.is_empty(), "expected to parse BOSL2 sources from {}", bosl2_dir().display());
    let defs = functions(&programs);
    let consts = globals(&programs);
    let registry =
        JitRegistry::build(defs.iter().map(|&(n, p, b)| (n, p, b)), consts.iter().map(|&(n, v)| (n, v)))
            .expect("registry builds");
    let oracle = FnOracle::new(&defs, &consts).expect("oracle builds");

    let mut accepted = 0usize; // (function, shape, row) triples the JIT compiled + differentialed bit-identical
    let mut fns_with_vec_spec: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (name, params, _body) in &defs {
        let arity = params.len();
        if arity == 0 {
            continue;
        }
        // Uniform vector shapes (every param vec-N) — BOSL2 points are vec2/vec3/vec4. A scalar-param function
        // given vec args, or a matrix (nested) param, DECLINES the shape (skipped). The (name, shape) key is
        // identical across a shape's rows, so only the first row compiles; the rest hit the memoized spec.
        for n in [2usize, 3, 4] {
            for row in vector_rows(arity, n, 8) {
                let Some(jit) = registry.call_numeric(name, &row) else {
                    continue; // declined-or-raised shape → the interpreter handles it, nothing to differential
                };
                // The JIT accepted this shape → it MUST match the interpreter, bits + type tag.
                let slow = oracle.call(name, &row);
                let ok = match (jit, &slow) {
                    (JitOutcome::Num(a), Ok(Value::Num(b))) => a.to_bits() == b.to_bits(),
                    (JitOutcome::Bool(a), Ok(Value::Bool(b))) => a == *b,
                    _ => false,
                };
                assert!(
                    ok,
                    "rung-B fast != JIT for BOSL2 `{name}` shape vec{n} at {row:?}: jit={jit:?} interp={slow:?}"
                );
                accepted += 1;
                fns_with_vec_spec.insert(name);
            }
        }
    }
    eprintln!(
        "[jit-corpus-vec] {} BOSL2 functions gained a vector-arg specialization; {accepted} (fn,shape,row) triples differentialed bit-identical",
        fns_with_vec_spec.len()
    );
    assert!(accepted > 0, "rung B should compile a vector-arg specialization for at least one BOSL2 function");
}
