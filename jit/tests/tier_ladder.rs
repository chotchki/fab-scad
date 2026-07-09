//! P.1.5 illustration — the TIER LADDER on one function, `is_nan(x) = x != x`. This is the single function
//! that has BOTH an O.2 intrinsic AND compiles in the JIT (a comparison → bool, via the P.1.4e bool-return
//! ABI), so all four tiers run the IDENTICAL function and the comparison is apples-to-apples:
//!
//!   OpenSCAD          — the reference C++ tree-walking interpreter (subprocess, per-element of a comprehension)
//!   fab-scad interp   — our interpreter's task loop (bind, box every `Value`, walk the stack)
//!   fab-scad intrinsic— the hand-written native Rust that REPLACES the interpreted body (O.2), still boxing
//!   fab-scad JIT      — Cranelift native code, no boxing, no dispatch
//!
//! NOT a rigorous benchmark (it illustrates what the tiers buy); the three in-process tiers cross-check that
//! they agree on the answer, so a wrong tier still fails. Run:
//!   cargo test -p fab-jit --release --test tier_ladder -- --nocapture

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use fab_jit::JitRegistry;
use fab_lang::{Expr, FnOracle, Parameter, Program, StmtKind, Value, bench_intrinsic, parse};

const OPENSCAD: &str = "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD";

fn program(src: &str) -> Program {
    parse(src).expect("parses")
}

/// The first function def's `(name, params, body)`.
fn func(prog: &Program) -> (&str, &[Parameter], &Expr) {
    prog.stmts
        .iter()
        .find_map(|s| match &s.kind {
            StmtKind::FunctionDef { name, params, body } => Some((name.as_str(), params.as_slice(), body)),
            _ => None,
        })
        .expect("a function def")
}

/// The i-th argument: mostly finite (is_nan → false), one in seven a NaN (is_nan → true), so the accumulator
/// is non-trivial and DCE can't fold it away.
fn arg(i: u64) -> f64 {
    if i % 7 == 0 { f64::NAN } else { f64::from(u32::try_from(i & 0xffff).unwrap_or(0)) }
}

/// Time OpenSCAD evaluating `is_nan` over an `n`-element comprehension (per-element = `(t_n − t_small)/(n −
/// small)` cancels the ~constant startup/parse). `None` if the binary isn't present. Writes its temp `.scad`
/// under `dir`.
fn openscad_ns_per_call(dir: &Path) -> Option<f64> {
    if !Path::new(OPENSCAD).exists() {
        return None;
    }
    // MATERIALIZE the comprehension (`echo(v[1])` forces the whole list) so OpenSCAD actually runs is_nan per
    // element — an unmaterialized / `if(false)`-filtered comprehension gets folded away and times as startup.
    // A bare identity comprehension (`[for(i) i]`) is ~free in OpenSCAD, so this is essentially the call cost.
    // Range must stay under OpenSCAD's ~1M for-element cap. Min of a few runs cuts subprocess noise.
    let run = |n: u64| -> u128 {
        let src = format!(
            "function is_nan(x) = (x != x);\nv = [for (i = [0:1:{n}]) is_nan(i)];\necho(v[1]);\n"
        );
        let scad = dir.join(format!("isnan_{n}.scad"));
        std::fs::write(&scad, src).expect("write scad");
        let out = dir.join(format!("isnan_{n}.echo"));
        let mut best = u128::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let r = Command::new(OPENSCAD).arg("-o").arg(&out).arg(&scad).output().expect("run openscad");
            assert!(r.status.success(), "openscad failed: {}", String::from_utf8_lossy(&r.stderr));
            best = best.min(t.elapsed().as_nanos());
        }
        best
    };
    // per-call = (t_big − t_small)/(big − small): cancels OpenSCAD's ~constant startup + parse.
    let (small, big) = (10_000u64, 500_000u64);
    #[allow(clippy::cast_precision_loss)]
    Some((run(big).saturating_sub(run(small))) as f64 / (big - small) as f64)
}

#[test]
#[allow(clippy::cast_precision_loss, reason = "ns→ratio in a dev-only stderr illustration")]
fn tier_ladder_is_nan() {
    let prog = program("function is_nan(x) = (x != x);");
    let (name, params, body) = func(&prog);

    // The three in-process tiers, all running the same `is_nan`.
    let oracle = FnOracle::new(&[(name, params, body)], &[]).expect("oracle builds");
    let intrinsic = bench_intrinsic(name, params, body).expect("is_nan has an O.2 intrinsic");
    let reg = JitRegistry::build([(name, params, body)].into_iter(), std::iter::empty())
        .expect("registry builds");
    let compiled = reg.get(name).expect("is_nan compiles in the JIT");

    let iters = 2_000_000u64;

    // interp: the real task-loop dispatch.
    let mut acc = 0u64;
    let t = Instant::now();
    for i in 0..iters {
        if let Ok(Value::Bool(b)) = oracle.call(name, &[Value::Num(arg(i))]) {
            acc += u64::from(b);
        }
    }
    let interp_ns = t.elapsed().as_nanos() as f64 / iters as f64;
    let c_interp = acc;

    // intrinsic: the native Rust body, still returning a boxed `Value`.
    acc = 0;
    let t = Instant::now();
    for i in 0..iters {
        if let Ok(Value::Bool(b)) = intrinsic(&[Value::Num(arg(i))]) {
            acc += u64::from(b);
        }
    }
    let intrinsic_ns = t.elapsed().as_nanos() as f64 / iters as f64;
    let c_intrinsic = acc;

    // JIT: native code, unboxed 0.0/1.0.
    acc = 0;
    let t = Instant::now();
    for i in 0..iters {
        if compiled.call(&[arg(i)]) == Some(1.0) {
            acc += 1;
        }
    }
    let jit_ns = t.elapsed().as_nanos() as f64 / iters as f64;
    let c_jit = acc;

    // Correctness gate: the three tiers MUST agree on the count (a wrong tier fails here, not just times).
    assert_eq!(c_interp, c_intrinsic, "interp vs intrinsic disagree on is_nan count");
    assert_eq!(c_interp, c_jit, "interp vs JIT disagree on is_nan count");

    let scratch = std::env::temp_dir();
    let openscad_ns = openscad_ns_per_call(&scratch);

    eprintln!("\n[tier-ladder] is_nan(x) = (x != x)   ({iters} calls/tier, {} NaN)", c_interp);
    let base = openscad_ns.unwrap_or(interp_ns);
    let row = |label: &str, ns: f64| {
        eprintln!("[tier-ladder]   {label:<22} {ns:8.1} ns/call   {:6.1}x vs OpenSCAD", base / ns.max(1e-9));
    };
    match openscad_ns {
        Some(ns) => row("OpenSCAD (C++)", ns),
        None => eprintln!("[tier-ladder]   OpenSCAD (C++)            n/a  (binary not found)"),
    }
    row("fab-scad interp", interp_ns);
    row("fab-scad intrinsic", intrinsic_ns);
    row("fab-scad JIT", jit_ns);
}
