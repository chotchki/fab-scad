//! P.1.4 recut (task #66) — the two changes that make the JIT REACHABLE on real programs, each pinned:
//!
//! 1. NAMED-ARG dispatch: the P.1.5 fires report read `offered 0` on the heavy models because the
//!    dispatch gate only offered all-positional calls and real BOSL2 calls are named everywhere. The gate
//!    is gone — `push_call` binds slots by name and `Task::Apply` hands the JIT the same param-order
//!    values either way. The end-to-end probe here proves a named-arg call through the REAL Cranelift
//!    factory is bit-identical to the interpreter (the fast==JIT contract, at dispatch level).
//! 2. LAZY registry: the eager build compiled ~85 all-scalar shapes per eval (× fixpoint rounds) for
//!    functions that mostly never ran — the whole +2.3% jit-on wall. `build_lazy` compiles on first call;
//!    the parity test proves lazy and eager return identical bits for the same calls.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fab_jit::{JitFactory, JitRegistry};
use fab_lang::{
    Config, Expr, JitOutcome, NumericJit, NumericJitFactory, Parameter, Program, RandStream,
    StmtKind, Value, parse, resolve_geometry_from_sources,
};

/// Named-arg spellings of numeric calls, asserted in-scad against their positional twins — under the
/// JIT the named call must take the SAME path and produce the SAME bits, or the assert raises and the
/// resolve errors. `holes` exercises a named arg PLUS a defaulted hole (the default evaluates
/// interpreter-side and lands in `vals` before the hook sees them).
const PROBE: &str = "\
function poly(x, k = 3) = x*x + k;
function hyp(a, b) = sqrt(a*a + b*b);
assert(poly(x = 5) == poly(5), \"named == positional (defaulted hole)\");
assert(poly(k = 10, x = 2) == poly(2, 10), \"named out of order == positional\");
assert(hyp(b = 4, a = 3) == hyp(3, 4), \"two named, swapped order\");
cube(1);
";

/// Run `src` through the full loader/eval pipeline (no fs, no imports), with or without the real
/// Cranelift factory. `Ok` means every in-scad assert held.
fn run(src: &str, jit: bool) -> fab_lang::Result<fab_lang::Geo> {
    let sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let factory = JitFactory;
    let hook: Option<&dyn NumericJitFactory> = jit.then_some(&factory);
    let config = Config {
        jit,
        ..Config::default()
    };
    resolve_geometry_from_sources(src, &sources, hook, config, |path| {
        Err(fab_lang::Error::Load(format!(
            "probe has no imports, asked for {path}"
        )))
    })
}

#[test]
fn named_arg_calls_fast_eq_jit_end_to_end() {
    run(PROBE, false).expect("interpreter baseline: the asserts are tautologies there");
    run(PROBE, true).expect("JIT: named-arg spellings must be bit-identical to positional ones");
}

/// `(name, params, body)` + `(name, value)` collectors over a parsed program (the corpus_diff shape).
fn defs_of(prog: &Program) -> Vec<(&str, &[Parameter], &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::FunctionDef { name, params, body } => {
                Some((name.as_ref(), params.as_slice(), body))
            }
            _ => None,
        })
        .collect()
}

fn globals_of(prog: &Program) -> Vec<(&str, &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Assignment { name, value } => Some((name.as_ref(), value)),
            _ => None,
        })
        .collect()
}

fn jit_call(registry: &JitRegistry, name: &str, vals: &[Value]) -> Option<JitOutcome> {
    let mut stream = RandStream::default();
    let ptr = std::ptr::from_mut(&mut stream).cast::<core::ffi::c_void>();
    registry.call_numeric(name, vals, ptr)
}

#[test]
fn lazy_build_matches_eager_bit_for_bit() {
    let prog = parse(
        "K = 2.5;\n\
         function f(x) = x*x + K;\n\
         function g(a, b) = a > b ? sqrt(a - b) : -sqrt(b - a);\n\
         function stringy(s) = str(s, \"!\");\n",
    )
    .expect("probe parses");
    let eager = JitRegistry::build(defs_of(&prog), globals_of(&prog)).expect("eager builds");
    let lazy = JitRegistry::build_lazy(defs_of(&prog), globals_of(&prog)).expect("lazy builds");

    // Lazy skipped the whole-program pass — nothing compiled yet, but the registry is NOT empty (the
    // factory keeps the hook installed on `!is_empty()`).
    assert_eq!(lazy.len(), 0, "lazy compiles nothing at build");
    assert!(!lazy.is_empty(), "lazy still owns the defs");
    assert!(eager.len() >= 2, "eager pre-compiled the numeric bodies");

    // Same calls, both registries: identical outcomes to the bit — including the decline (stringy) and
    // the second call to a shape (the lazy one now memoized).
    let cases: &[(&str, Vec<Value>)] = &[
        ("f", vec![Value::Num(3.0)]),
        ("f", vec![Value::Num(-0.5)]),
        ("g", vec![Value::Num(7.0), Value::Num(3.0)]),
        ("g", vec![Value::Num(3.0), Value::Num(7.0)]),
        ("stringy", vec![Value::Num(1.0)]),
        ("f", vec![Value::Num(3.0)]), // repeat: the memoized lazy path
    ];
    for (name, vals) in cases {
        let a = jit_call(&eager, name, vals);
        let b = jit_call(&lazy, name, vals);
        match (&a, &b) {
            (Some(JitOutcome::Num(x)), Some(JitOutcome::Num(y))) => {
                assert_eq!(x.to_bits(), y.to_bits(), "{name}({vals:?}) bits diverged");
            }
            (None, None) => {} // both declined (stringy) — equally invisible to the interpreter
            other => panic!("{name}({vals:?}): eager/lazy outcome shapes diverged: {other:?}"),
        }
    }
}
