//! `scad-gen` — drive the K.3 generator: emit N programs (seed → program), LABEL each through the evaluator
//! (+ the JIT bit-identity check), and write a corpus + a JSONL manifest — the ML-training dataset.
//!
//!   scad-gen --count 50000 --out gen/out          # generate + label a corpus
//!   scad-gen --replay 12345                        # print the exact program a seed produces (repro)
//!   scad-gen --count 1e6 --max-time 3600 --out …   # bounded by count OR wall-clock, whichever first
//!
//! Each program lands at `<out>/scad/<seed>.scad`; each label is one line of `<out>/manifest.jsonl`:
//!   {"seed":42,"file":"scad/42.scad","status":"ok","cost":218,"ms":1,"jit":"match","bytes":312}
//! `status` ∈ ok | err_{eval,parse,lower,load,unimplemented,unknown}; `jit` ∈ match | n/a | MISMATCH. A
//! MISMATCH is a JIT-vs-interpreter divergence (doctrine #36) — the program is copied to `<out>/divergences/`
//! and flagged LOUD, since a generated program that miscompiles is exactly what this harness exists to catch.
//! `cost` (R.1) is the deterministic `eval_steps` count — the perf SUCCESS-FUNCTION score; the run also writes
//! `<out>/perf_report.md` (cost distribution + decade histogram + the top-N worst-case seeds to replay).

use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

use fab_lang::{Error, Program, Scope, StmtKind, Value, eval_expr, parse, tier_eq};

/// The eval budget the R.1 cost metric runs each program under: high enough that any bounded generated
/// program completes with its TRUE `eval_steps` cost, but a cap so a runaway (the grammar bounds ranges, so
/// this shouldn't fire — belt only) ranks as a worst-case instead of hanging the labeler.
const COST_BUDGET: u64 = 50_000_000;

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
        stats.record(&label, seed);
        if i > 0 && i % 5000 == 0 {
            eprintln!("... {i}/{} — {}", args.count, stats.summary());
        }
    }
    manifest.flush().expect("flush manifest");
    let report = stats.perf_report(20);
    fs::write(args.out.join("perf_report.md"), &report).expect("write perf report");
    eprint!("{report}");
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
    /// R.1 perf success-function SCORE: deterministic `eval_steps` cost (machine-independent, reproducible).
    /// Interpreter work only — the `Geo` tree isn't tessellated, so this is NOT geometry-kernel cost.
    cost: u64,
    jit: &'static str,
    err: Option<String>,
}

/// Evaluate + cost + JIT-diff one program into its label. Uses `evaluate_geometry_metered` (→ the resolved
/// `Geo` CSG tree PLUS the `eval_steps` cost), NOT `evaluate` (→ `Mesh`, which rejects transforms/booleans as
/// `Unimplemented` — the grammar emits those constantly). `Geo` resolution is the real "did it evaluate"
/// signal and needs no Manifold backend; the metered path is the raw-AST driver, exact for the self-contained
/// programs the grammar emits (no `use`/`include`/`import`). Parses ONCE, reused for eval + the JIT diff.
fn label(src: &str) -> Label {
    let first_line = |e: &Error| {
        format!("{e}")
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let prog = match parse(src) {
        Ok(p) => p,
        Err(e) => {
            return Label {
                status: "err_parse",
                ms: 0,
                cost: 0,
                jit: "n/a",
                err: Some(first_line(&e)),
            };
        }
    };
    let t0 = Instant::now();
    let (ev, cost) = fab_lang::evaluate_geometry_metered(&prog, COST_BUDGET);
    let ms = t0.elapsed().as_millis();
    let (status, err) = match ev {
        Ok(_geo) => ("ok", None),
        Err(e) => (class_of(&e), Some(first_line(&e))),
    };
    Label {
        status,
        ms,
        cost,
        jit: jit_label(&prog),
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
fn jit_label(prog: &Program) -> &'static str {
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
                && !tier_eq(j, s)
            {
                // `tier_eq` (doctrine #36): bitwise for information-carrying values, NaN as a class. A
                // `(-NaN)²`-shaped body no longer labels MISMATCH — only a real finite/±inf divergence does.
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
            "{{\"seed\":{seed},\"file\":\"scad/{seed}.scad\",\"status\":\"{}\",\"cost\":{},\"ms\":{},\"jit\":\"{}\",\"bytes\":{bytes}{err}}}",
            self.status, self.cost, self.ms, self.jit
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
    /// (cost, seed) per program — the R.1 perf success-function samples, ranked into [`Stats::perf_report`].
    costs: Vec<(u64, u32)>,
}

impl Stats {
    fn record(&mut self, l: &Label, seed: u32) {
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
        self.costs.push((l.cost, seed));
    }

    /// R.1.3 — the perf report: cost distribution (percentiles), a decade histogram, and the top-N worst-case
    /// seeds (each replayable via `scad-gen --replay <seed>`). Cost is deterministic `eval_steps` — the
    /// success-function score. NOTE: the grammar bounds ranges/fuel, so absolute costs are modest; this ranks
    /// the RELATIVELY expensive constructs (what the JIT/cache/intrinsics should target), and v1 (R.3) evolves
    /// toward genuine worst-cases.
    fn perf_report(&self, top_n: usize) -> String {
        let mut c = self.costs.clone();
        c.sort_unstable_by_key(|e| std::cmp::Reverse(e.0)); // descending by cost
        let n = c.len();
        if n == 0 {
            return "# perf report\n\n(no programs)\n".to_string();
        }
        // Percentiles off the descending vector: index 0 is the max, index n-1 the min.
        let at = |top_frac: f64| c[(((n as f64) * top_frac) as usize).min(n - 1)].0;
        let mut buckets = [0u64; 9]; // decade buckets: [0]=<10 … [8]=>=1e8
        for &(cost, _) in &c {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let b = if cost == 0 {
                0
            } else {
                ((cost as f64).log10().floor() as usize).min(8)
            };
            buckets[b] += 1;
        }
        let mut s = String::new();
        s.push_str("# perf report — R.1 eval-cost success function\n\n");
        s.push_str(&format!(
            "{n} programs. cost = deterministic `eval_steps` (interpreter work; NOT geometry-kernel cost).\n\n",
        ));
        s.push_str(&format!(
            "distribution: min {}, median {}, p90 {}, p99 {}, max {}\n\n",
            c[n - 1].0,
            at(0.50),
            at(0.10),
            at(0.01),
            c[0].0,
        ));
        s.push_str("histogram (eval_steps, decade buckets):\n");
        let edges = [
            "<10", "<100", "<1k", "<10k", "<100k", "<1M", "<10M", "<100M", ">=100M",
        ];
        for (edge, count) in edges.iter().zip(buckets) {
            s.push_str(&format!("  {edge:>7}  {count}\n"));
        }
        s.push_str(&format!(
            "\ntop {top_n} worst-case (replay: scad-gen --replay <seed>):\n"
        ));
        for &(cost, seed) in c.iter().take(top_n) {
            s.push_str(&format!("  seed {seed:<10}  cost {cost}\n"));
        }
        s
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Label, Stats, label};

    /// R.1 — the cost score is DETERMINISTIC (same program → same cost) and MONOTONE (heavier interpreter work
    /// costs strictly more). This is the success-function contract the ranking rests on.
    #[test]
    fn cost_is_deterministic_and_monotone() {
        let light = label("x = 1 + 2;").cost;
        let heavy = label("x = [for (i = [0:200]) i * i];").cost;
        assert!(
            heavy > light,
            "heavier program costs more: {heavy} vs {light}"
        );
        assert_eq!(
            heavy,
            label("x = [for (i = [0:200]) i * i];").cost,
            "cost is reproducible"
        );
    }

    /// R.1.3 — the report ranks worst-case FIRST and reports the true max.
    #[test]
    fn perf_report_ranks_worst_case_first() {
        let mut s = Stats::default();
        let lbl = |cost| Label {
            status: "ok",
            ms: 0,
            cost,
            jit: "n/a",
            err: None,
        };
        s.record(&lbl(10), 1);
        s.record(&lbl(9999), 2); // the worst case, seed 2
        s.record(&lbl(500), 3);
        let r = s.perf_report(3);
        assert!(r.contains("max 9999"), "report:\n{r}");
        let (i2, i3) = (r.find("seed 2").unwrap(), r.find("seed 3").unwrap());
        assert!(
            i2 < i3,
            "the worst case (seed 2) must rank before seed 3:\n{r}"
        );
    }
}
