//! `scad-gen` — drive the K.3 generator: emit N programs (seed → program), LABEL each through the evaluator
//! (+ the JIT bit-identity check), and write a corpus + a JSONL manifest — the ML-training dataset.
//!
//!   scad-gen --count 50000 --out gen/out          # generate + label a corpus
//!   scad-gen --replay 12345                        # print the exact program a seed produces (repro)
//!   scad-gen --count 1e6 --max-time 3600 --out …   # bounded by count OR wall-clock, whichever first
//!
//! Each program lands at `<out>/scad/<seed>.scad`; each label is one line of `<out>/manifest.jsonl`:
//!   {"seed":42,"file":"scad/42.scad","status":"ok","ms":1,"jit":"match","bytes":312}
//! `status` ∈ ok | err_{eval,parse,lower,load,unimplemented,unknown}; `jit` ∈ match | n/a | MISMATCH. A
//! MISMATCH is a JIT-vs-interpreter divergence (doctrine #36) — the program is copied to `<out>/divergences/`
//! and flagged LOUD, since a generated program that miscompiles is exactly what this harness exists to catch.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

use fab_lang::{Error, Scope, StmtKind, Value, eval_expr, parse};

struct Args {
    count: u32,
    start: u32,
    out: PathBuf,
    max_time: Option<u64>,
    replay: Option<u32>,
}

fn parse_args() -> Args {
    let mut a = Args {
        count: 50_000,
        start: 0,
        out: PathBuf::from("gen/out"),
        max_time: None,
        replay: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().expect("flag needs a value");
        match flag.as_str() {
            "--count" => a.count = parse_num(&val()),
            "--start" => a.start = parse_num(&val()),
            "--out" => a.out = PathBuf::from(val()),
            "--max-time" => a.max_time = Some(u64::from(parse_num(&val()))),
            "--replay" => a.replay = Some(parse_num(&val())),
            other => panic!("unknown flag {other}"),
        }
    }
    a
}

/// Accept plain ints and `1e6`-style shorthand (convenient for big counts).
fn parse_num(s: &str) -> u32 {
    if let Ok(n) = s.parse::<u32>() {
        return n;
    }
    let f: f64 = s.parse().unwrap_or_else(|_| panic!("not a number: {s}"));
    f as u32
}

fn main() {
    let args = parse_args();

    if let Some(seed) = args.replay {
        print!("{}", fab_gen::generate(seed));
        return;
    }

    let scad_dir = args.out.join("scad");
    let div_dir = args.out.join("divergences");
    fs::create_dir_all(&scad_dir).expect("create scad dir");
    fs::create_dir_all(&div_dir).expect("create divergences dir");
    let manifest = fs::File::create(args.out.join("manifest.jsonl")).expect("create manifest");
    let mut manifest = BufWriter::new(manifest);

    let start = Instant::now();
    let mut stats = Stats::default();
    for i in 0..args.count {
        if let Some(mt) = args.max_time
            && start.elapsed().as_secs() >= mt
        {
            eprintln!("max-time reached at {i} programs");
            break;
        }
        let seed = args.start.wrapping_add(i);
        let src = fab_gen::generate(seed);
        fs::write(scad_dir.join(format!("{seed}.scad")), &src).expect("write program");
        let label = label(&src);
        if label.jit == "MISMATCH" {
            fs::write(div_dir.join(format!("{seed}.scad")), &src).expect("write divergence");
            eprintln!("!! JIT DIVERGENCE at seed {seed} — copied to divergences/{seed}.scad");
        }
        writeln!(manifest, "{}", label.to_json(seed, src.len())).expect("write manifest line");
        stats.record(&label);
        if i > 0 && i % 5000 == 0 {
            eprintln!("... {i}/{} — {}", args.count, stats.summary());
        }
    }
    manifest.flush().expect("flush manifest");
    eprintln!(
        "DONE {} programs in {:.1}s → {}",
        stats.total,
        start.elapsed().as_secs_f64(),
        args.out.display()
    );
    eprintln!("{}", stats.summary());
}

struct Label {
    status: &'static str,
    ms: u128,
    jit: &'static str,
    err: Option<String>,
}

/// Evaluate + JIT-diff one program into its label. Uses `evaluate_geometry` (→ the resolved `Geo` CSG tree),
/// NOT `evaluate` (→ `Mesh`) — the Mesh path is the tracer-bullet subset and rejects transforms/booleans as
/// `Unimplemented`, which the grammar emits constantly. `Geo` resolution is the real "did it evaluate" signal
/// and needs no Manifold backend.
fn label(src: &str) -> Label {
    let t0 = Instant::now();
    let ev = fab_lang::evaluate_geometry(src);
    let ms = t0.elapsed().as_millis();
    let (status, err) = match ev {
        Ok(_geo) => ("ok", None),
        Err(e) => (
            class_of(&e),
            Some(
                format!("{e}")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            ),
        ),
    };
    Label {
        status,
        ms,
        jit: jit_label(src),
        err,
    }
}

fn class_of(e: &Error) -> &'static str {
    match e {
        Error::Parse(_) => "err_parse",
        Error::Eval(_) => "err_eval",
        Error::Lower(_) => "err_lower",
        Error::Load(_) => "err_load",
        Error::Unimplemented(_) => "err_unimplemented",
        Error::Unknown(_) => "err_unknown",
        _ => "err_other", // Error is #[non_exhaustive]
    }
}

/// The interp==JIT label: compile the first JIT-eligible numeric function and diff it bitwise against the
/// interpreter over the IEEE battery. `match` = compiled + agreed everywhere; `n/a` = nothing JIT-eligible;
/// `MISMATCH` = a real divergence. Mirrors the `jit_diff` fuzz target exactly (conservative: only compares
/// when both tiers yield a number).
fn jit_label(src: &str) -> &'static str {
    let Ok(prog) = parse(src) else {
        return "n/a";
    };
    for stmt in &prog.stmts {
        let StmtKind::FunctionDef { params, body, .. } = &stmt.kind else {
            continue;
        };
        let names: Vec<&str> = params.iter().map(|p| p.name.as_ref()).collect();
        if names.is_empty() || names.len() > 4 {
            continue;
        }
        let Ok(jitted) = fab_jit::compile_function(&names, body) else {
            continue; // not in the numeric subset
        };
        for args in sample_args(names.len()) {
            let Some(j) = jitted.call(&args) else {
                continue;
            };
            let mut scope = Scope::new();
            for (name, &v) in names.iter().zip(&args) {
                scope.bind(*name, Value::Num(v));
            }
            if let Ok(Value::Num(s)) = eval_expr(body, &scope)
                && j.to_bits() != s.to_bits()
            {
                return "MISMATCH";
            }
        }
        return "match"; // first eligible function compiled + agreed on every sample
    }
    "n/a"
}

/// The IEEE-corner arg battery, replicated across the arity (same corners the JIT proptest/fuzzer probe).
fn sample_args(arity: usize) -> Vec<Vec<f64>> {
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
    CORNERS.iter().map(|&c| vec![c; arity]).collect()
}

impl Label {
    fn to_json(&self, seed: u32, bytes: usize) -> String {
        let err = match &self.err {
            Some(e) => format!(",\"err\":\"{}\"", json_escape(e)),
            None => String::new(),
        };
        format!(
            "{{\"seed\":{seed},\"file\":\"scad/{seed}.scad\",\"status\":\"{}\",\"ms\":{},\"jit\":\"{}\",\"bytes\":{bytes}{err}}}",
            self.status, self.ms, self.jit
        )
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[derive(Default)]
struct Stats {
    total: u64,
    ok: u64,
    errors: u64,
    jit_match: u64,
    jit_mismatch: u64,
    jit_na: u64,
}

impl Stats {
    fn record(&mut self, l: &Label) {
        self.total += 1;
        if l.status == "ok" {
            self.ok += 1;
        } else {
            self.errors += 1;
        }
        match l.jit {
            "match" => self.jit_match += 1,
            "MISMATCH" => self.jit_mismatch += 1,
            _ => self.jit_na += 1,
        }
    }

    fn summary(&self) -> String {
        let pct = |n: u64| {
            if self.total == 0 {
                0.0
            } else {
                100.0 * n as f64 / self.total as f64
            }
        };
        format!(
            "ok {:.0}%, err {:.0}% | jit: match {} / n/a {} / MISMATCH {}",
            pct(self.ok),
            pct(self.errors),
            self.jit_match,
            self.jit_na,
            self.jit_mismatch,
        )
    }
}
