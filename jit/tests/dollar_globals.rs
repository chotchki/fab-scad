//! Task #51 / P.1.3a — `$`-variables are DYNAMICALLY scoped, so the JIT must never lexicalize one. The
//! hazard (a reviewer find on the P.1.4-globals path): `tagged_globals` fed every top-level assignment to
//! the registry, `$fn = 32;` included — so a compiled body reading `$fn` INLINED 32, while the interpreter
//! resolves `$fn` up the dynamic call chain (a module-scoped `$fn = 16;` reaches every call beneath it).
//! Note the P.1.3 corpus differential could never catch this class: it feeds BOTH engines the same lexical
//! globals, so both sides were identically wrong. The end-to-end probe here pins the divergence instead.
//!
//! The dispatch gate already declines a call with EXPLICIT `$`-args (`f($fn=16)` interprets); this covers
//! the other route in: a `$`-binding INHERITED from an ancestor scope, which keeps the call JIT-eligible.
//! The fix is two independent layers: the compiler declines any `$`-ident (the authoritative guard, in the
//! `Ident` arm), and `tagged_globals` stops shipping `$`-assignments as consts (so a `$`-global can't even
//! arrive). Either alone kills the bug; both together keep a future caller honest.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fab_jit::{JitFactory, JitRegistry};
use fab_lang::{
    Config, Expr, JitOutcome, NumericJit, NumericJitFactory, Parameter, Program, RandStream,
    StmtKind, Value, parse, resolve_geometry_from_sources,
};

/// The canonical divergence shape: a top-level `$fn` (the idiomatic OpenSCAD model preamble), a numeric
/// function reading it, and a module that dynamically overrides it before calling. The in-scad `assert`
/// turns a wrong `res()` into a LOUD eval error, so no message plumbing is needed — a divergent engine
/// fails to produce geometry at all.
const PROBE: &str = "\
$fn = 32;
function res() = $fn;
module probe() {
    $fn = 16;
    assert(res() == 16, \"res() read the top-level $fn, not the dynamic override\");
    cube(1);
}
probe();
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
fn dynamic_dollar_override_fast_eq_jit() {
    // The interpreter baseline: the dynamic override reaches `res()`, the assert passes.
    run(PROBE, false).expect("interpreter: module-scoped $fn reaches res()");
    // fast == JIT: the same program with the JIT armed must take the same path. Pre-fix this failed —
    // res() compiled with 32 baked in and the assert raised.
    run(PROBE, true)
        .expect("JIT: a $-reading function must interpret, never inline the lexical $fn");
}

/// `(name, params, body)` of every function def in the parsed program — the registry-building shape
/// `corpus_diff` uses.
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

/// `(name, value)` of every top-level assignment — `$`-names INCLUDED, deliberately: this feeds the
/// registry the pre-fix `tagged_globals` shape to prove the compiler-side guard alone suffices.
fn globals_of(prog: &Program) -> Vec<(&str, &Expr)> {
    prog.stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Assignment { name, value } => Some((name.as_ref(), value)),
            _ => None,
        })
        .collect()
}

/// `call_numeric` with a fresh `RandStream` (none of these bodies draw).
fn jit_call(registry: &JitRegistry, name: &str, vals: &[Value]) -> Option<JitOutcome> {
    let mut stream = RandStream::default();
    let ptr = std::ptr::from_mut(&mut stream).cast::<core::ffi::c_void>();
    registry.call_numeric(name, vals, ptr)
}

#[test]
fn registry_declines_dollar_global_but_inlines_plain_global() {
    // Registry-level pin of the authoritative guard: even when a caller DOES hand the registry a
    // `$`-assignment as a const (the pre-fix `tagged_globals` behavior), the `Ident` arm must decline —
    // while the like-shaped plain global keeps inlining (the P.1.4-globals feature this guard must not eat).
    let prog = parse("$fn = 32;\nK = 32;\nfunction dollar() = $fn;\nfunction plain() = K;\n")
        .expect("probe parses");
    let registry = JitRegistry::build(defs_of(&prog), globals_of(&prog)).expect("registry builds");
    assert!(
        jit_call(&registry, "dollar", &[]).is_none(),
        "a $-reading body must DECLINE — $-vars are dynamically scoped, a lexical inline diverges"
    );
    assert_eq!(
        match jit_call(&registry, "plain", &[]) {
            Some(JitOutcome::Num(n)) => Some(n),
            _ => None,
        },
        Some(32.0),
        "the plain-global inline (P.1.4) must survive the $-guard"
    );
}
