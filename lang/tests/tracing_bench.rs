//! I.6 — the SUBSCRIBER side of the evaluator's tracing spans: an aggregating benchmark layer plus an
//! overhead measurement. The lib emits TRACE-level spans on the eval path (`eval_program`, `builtin`,
//! `module`) + a `call` event; here a `tracing-subscriber` [`Layer`] times each span by name, and a
//! second test measures the instrumented-vs-bare overhead. This is a REFERENCE layer — the Phase-K
//! benchmark harness (#33) can lift it or reimplement; the lib's spans are the reusable contract.
//!
//! Why a subscriber-less lib pays almost nothing: a disabled span is one atomic level check, and a
//! release binary that sets `tracing/release_max_level_off` strips these TRACE spans at compile time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions; timing uses the I.6-sanctioned Instant"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fab_lang::{Scope, eval_program, parse};
use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// Per-span-name aggregate: how many span instances closed, and their summed wall-time (inclusive of
/// children — a first-cut "time under this span", which for `eval_program` is the whole evaluation).
type Stats = BTreeMap<&'static str, (u64, Duration)>;

/// An aggregating benchmark layer: on span exit, add its enter→exit interval to the per-name total.
#[derive(Clone, Default)]
struct Bench {
    stats: Arc<Mutex<Stats>>,
}

/// Per-span timing state, stashed in the span's registry extensions.
struct Timing {
    last_enter: Option<Instant>,
}

impl<S> Layer<S> for Bench
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Timing { last_enter: None });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(timing) = span.extensions_mut().get_mut::<Timing>()
        {
            timing.last_enter = Some(Instant::now());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let name = span.name();
        let elapsed = span
            .extensions_mut()
            .get_mut::<Timing>()
            .and_then(|timing| timing.last_enter.take())
            .map(|start| start.elapsed());
        if let Some(dur) = elapsed {
            let mut stats = self.stats.lock().unwrap();
            let entry = stats.entry(name).or_insert((0, Duration::ZERO));
            entry.0 += 1;
            entry.1 += dur;
        }
    }
}

/// Wall-time a closure (I.6-sanctioned `Instant` — instrumentation only, never geometry).
fn time(f: impl FnOnce()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

#[test]
fn bench_layer_aggregates_the_eval_path() {
    // sqrt → a `builtin` span; sphere → a `module` span; the whole thing → one `eval_program` span.
    let program = parse("x = sqrt(16); sphere(x, $fn = 8);").expect("parses");
    let bench = Bench::default();
    let stats = bench.stats.clone();
    tracing::subscriber::with_default(Registry::default().with(bench), || {
        eval_program(&program, &Scope::new()).expect("evaluates");
    });

    let stats = stats.lock().unwrap();
    assert_eq!(stats["eval_program"].0, 1, "the top span runs exactly once");
    assert!(stats.contains_key("builtin"), "sqrt is a builtin span");
    assert!(stats.contains_key("module"), "sphere is a module span");
    // every captured span recorded a non-negative duration (the timer fired on each exit).
    assert!(stats.values().all(|&(count, _)| count >= 1));
}

#[test]
fn bench_layer_counts_every_builtin_call() {
    // 61 sqrt calls in the comprehension + one len() → 62 `builtin` span instances.
    let program = parse("a = [for (i = [0:60]) sqrt(i)]; n = len(a);").expect("parses");
    let bench = Bench::default();
    let stats = bench.stats.clone();
    tracing::subscriber::with_default(Registry::default().with(bench), || {
        eval_program(&program, &Scope::new()).expect("evaluates");
    });

    assert_eq!(stats.lock().unwrap()["builtin"].0, 62);
}

#[test]
fn tracing_overhead_is_bounded_and_measured() {
    let program =
        parse("a = [for (i = [0:60]) sqrt(i)]; sphere(len(a), $fn = 16);").expect("parses");
    let n = 500;

    // No subscriber: each span is one disabled-level atomic check.
    let off = time(|| {
        for _ in 0..n {
            eval_program(&program, &Scope::new()).expect("evaluates");
        }
    });
    // With the bench layer: spans are live + timed per instance.
    let bench = Bench::default();
    let on = tracing::subscriber::with_default(Registry::default().with(bench), || {
        time(|| {
            for _ in 0..n {
                eval_program(&program, &Scope::new()).expect("evaluates");
            }
        })
    });

    // The measurement (visible under `--nocapture`): this is the deliverable.
    println!("I.6 tracing overhead over {n} evals: no-subscriber={off:?}, bench-layer={on:?}");
    // Assert only against pathological blow-up — a loose bound stays robust to CI timing noise; the
    // printed numbers are the real signal, and the compile-out claim (release_max_level_off) is what
    // makes the shipped cost zero.
    assert!(
        on < off * 200 + Duration::from_secs(2),
        "off={off:?} on={on:?}"
    );
}
