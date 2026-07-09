//! P.1.1 — the JIT registry: many numeric functions compiled into ONE module, keyed by name, the
//! non-numeric ones DECLINED (left to the interpreter). Proves coverage (which compiled) and that a
//! registry-dispatched call is bit-identical to the interpreter — the same `fast == JIT` guarantee as
//! the standalone spike, now through the production cache.

use fab_jit::JitRegistry;
use fab_lang::{Expr, Program, Scope, StmtKind, Value, eval_expr, parse};

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

/// The interpreter baseline for one function of `prog`, found by name.
fn interp(prog: &Program, name: &str, args: &[f64]) -> f64 {
    let (params, body) = prog
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::FunctionDef { name: n, params, body } if &**n == name => {
                Some((params, body))
            }
            _ => None,
        })
        .expect("function is defined");
    let mut scope = Scope::new();
    for (p, &v) in params.iter().zip(args) {
        scope.bind(p.name.clone(), Value::Num(v));
    }
    match eval_expr(body, &scope) {
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
        let jit = reg.get(name).expect("compiled").call(args);
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
fn empty_registry_when_nothing_is_numeric() {
    // A program whose only function builds a list → an empty (valid) registry, everything interpreted.
    let prog = program("function only_list(x) = [x, x, x];");
    let reg = JitRegistry::build(defs(&prog).iter().map(|(n, p, b)| (*n, p.as_slice(), *b)))
        .expect("registry builds even with nothing to compile");
    assert!(reg.is_empty(), "no numeric function → empty registry");
    assert_eq!(reg.len(), 0);
}
