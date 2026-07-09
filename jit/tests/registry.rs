//! P.1.1 — the JIT registry: many numeric functions compiled into ONE module, keyed by name, the
//! non-numeric ones DECLINED (left to the interpreter). Proves coverage (which compiled) and that a
//! registry-dispatched call is bit-identical to the interpreter — the same `fast == JIT` guarantee as
//! the standalone spike, now through the production cache.

use fab_jit::JitRegistry;
use fab_lang::{Expr, Program, StmtKind, Value, interpret_fn, parse};

/// Parse a multi-function program (kept alive by the caller so bodies can be borrowed from it).
fn program(src: &str) -> Program {
    parse(src).expect("program parses")
}

/// Every `(name, param_names, body)` in `prog`, in source order — the shape [`JitRegistry::build`] eats.
fn defs(prog: &Program) -> Vec<(&str, Vec<&str>, &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|s| match &s.kind {
            StmtKind::FunctionDef { name, params, body } => {
                Some((name.as_ref(), params.iter().map(|p| p.name.as_ref()).collect(), body))
            }
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
    let reg = JitRegistry::build(defs(&prog).iter().map(|(n, p, b)| (*n, p.as_slice(), *b)))
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
    let reg = JitRegistry::build(defs(&prog).iter().map(|(n, p, b)| (*n, p.as_slice(), *b)))
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
    let reg = JitRegistry::build(defs(&prog).iter().map(|(n, p, b)| (*n, p.as_slice(), *b)))
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
fn empty_registry_when_nothing_is_numeric() {
    // A program whose only function builds a list → an empty (valid) registry, everything interpreted.
    let prog = program("function only_list(x) = [x, x, x];");
    let reg = JitRegistry::build(defs(&prog).iter().map(|(n, p, b)| (*n, p.as_slice(), *b)))
        .expect("registry builds even with nothing to compile");
    assert!(reg.is_empty(), "no numeric function → empty registry");
    assert_eq!(reg.len(), 0);
}
