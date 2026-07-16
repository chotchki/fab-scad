//! Dev probe (O.2, 2026-07-08; TIMED O.4, 2026-07-16): which functions/builtins/modules does a model call,
//! how OFTEN — and where does the TIME go? The N.1 sampling profiler attributes self-time to Rust SYMBOLS
//! (`lc_for`, `check_assert`) — but every BOSL2 function evaluates through the SAME eval loop, so samply
//! CAN'T tell `is_vector` from `is_path`. This probe closes that gap: a per-NAME call counter plus a per-NAME
//! self-time clock, so intrinsic-picking aims at the functions a real model actually spends time in instead
//! of a guess. Pairs with [`super::redundancy`] — that probe measures the memoization CEILING (how many calls
//! repeat their key), this one names WHICH functions to make cheap.
//!
//! Counts four call kinds separately: user `fn`s (the intrinsic targets), `builtin`s (already native, but
//! they show what the fns bottom out in), `module` instantiations, and `intrinsic` dispatches (already
//! replaced — the coverage denominator). A call is counted where it's DISPATCHED, so a cache HIT still counts
//! (it's a logical call the intrinsic would replace); a closure called by value isn't name-counted (it has no
//! static name — a v1 gap, BOSL2 predicates are all named).
//!
//! TIMING (user fns only): bodies evaluate on the EXPLICIT task stack — no host recursion — so a scope-bounded
//! tracing span can't bracket them. Instead the dispatch site calls [`enter_fn`] and pushes a
//! `Task::FnTimeReturn` that fires the instant the return value lands (LIFO, like `TraceReturn`); the task
//! stack makes enter/exit pairs strictly well-nested, so a SHADOW STACK computes classic profiler numbers:
//! SELF time (own cost minus timed callees — the worklist metric) and OUTERMOST-INCLUSIVE time (whole cost of
//! non-nested calls — what erasing the function entirely would reclaim; recursion doesn't double-book).
//! Attribution convention: a call's window opens at dispatch, so ARG EVALUATION books to the CALLEE. An
//! errored eval abandons the task stack mid-flight — in-flight frames just never book (the probe under-counts
//! that run; the next [`reset`] clears the stack). Builtin/module TIME stays with the tracing layer in the
//! models harness (they ARE span-bounded); this clock is only for the span-invisible user fns.
//!
//! Gated by `FAB_PROFILE_FNS=1`; when off, each `record_*`/`enter_fn` is one atomic-bool load and returns.
//! First sight of a name allocs its key; repeats are a `BTreeMap` lookup + increment. Report to stderr via
//! [`report`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How many rows per section the report prints before collapsing the tail into a "... and N more" line.
const REPORT_LIMIT: usize = 30;

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("FAB_PROFILE_FNS").is_some())
}

/// One user fn's clock: booked (exited) calls, self time, outermost-inclusive time, live nesting depth.
#[derive(Default)]
struct FnClock {
    name: String,
    booked: u64,
    self_time: Duration,
    outermost_time: Duration,
    depth: u32,
}

/// One in-flight call on the shadow stack: which clock, when it dispatched, timed-callee time to subtract,
/// and whether it's the outermost live call of its name (only that one books inclusive time).
struct Frame {
    id: usize,
    start: Instant,
    child: Duration,
    outermost: bool,
}

/// Per-kind call tallies + the user-fn shadow stack. `BTreeMap` (not `HashMap`) so the record path is
/// deterministic and the report can still re-sort by count; the name key allocs only on a name's FIRST
/// sighting (lookup-then-insert below). The shadow stack interns names to indices so the per-call hot path
/// never allocs past first sighting.
#[derive(Default)]
struct State {
    funcs: BTreeMap<String, u64>,
    builtins: BTreeMap<String, u64>,
    modules: BTreeMap<String, u64>,
    intrinsics: BTreeMap<String, u64>,
    ids: BTreeMap<String, usize>,
    clocks: Vec<FnClock>,
    stack: Vec<Frame>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Bump `name`'s tally in `map` without allocating on the repeat path (only a first sighting owns the key).
fn bump(map: &mut BTreeMap<String, u64>, name: &str) {
    if let Some(c) = map.get_mut(name) {
        *c += 1;
    } else {
        map.insert(name.to_string(), 1);
    }
}

/// Start (or restart) a measurement — called at each `resolve_source` run so the import fixpoint's earlier
/// partial runs don't bleed into the final complete one (mirrors [`super::redundancy::reset`]). Also drops
/// any shadow-stack frames a PRIOR errored eval abandoned mid-flight.
pub(super) fn reset() {
    if enabled() {
        STATE.with(|s| *s.borrow_mut() = Some(State::default()));
    }
}

/// Record one user-function call by name (the intrinsic-target tally). Off unless `FAB_PROFILE_FNS=1`.
pub(super) fn record_fn(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        bump(
            &mut s.borrow_mut().get_or_insert_with(State::default).funcs,
            name,
        );
    });
}

/// Open a timed user-fn call window (the dispatch site pairs a `true` with pushing `Task::FnTimeReturn`,
/// which calls [`exit_fn`] when the return value lands). `false` = probe off, push nothing.
pub(super) fn enter_fn(name: &str) -> bool {
    if !enabled() {
        return false;
    }
    STATE.with(|s| {
        let mut slot = s.borrow_mut();
        let st = slot.get_or_insert_with(State::default);
        let id = if let Some(&id) = st.ids.get(name) {
            id
        } else {
            let id = st.clocks.len();
            st.ids.insert(name.to_string(), id);
            st.clocks.push(FnClock {
                name: name.to_string(),
                ..FnClock::default()
            });
            id
        };
        let clock = &mut st.clocks[id];
        let outermost = clock.depth == 0;
        clock.depth += 1;
        st.stack.push(Frame {
            id,
            start: Instant::now(),
            child: Duration::ZERO,
            outermost,
        });
    });
    true
}

/// Close the innermost open call window (strict LIFO — the task stack guarantees nesting): book self time
/// (elapsed minus timed callees), outermost-inclusive time, and propagate elapsed into the parent's callee
/// accumulator. Tolerates an empty stack (a reused thread after an errored eval) by doing nothing.
pub(super) fn exit_fn() {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let mut slot = s.borrow_mut();
        let Some(st) = slot.as_mut() else { return };
        let Some(frame) = st.stack.pop() else { return };
        let elapsed = frame.start.elapsed();
        let clock = &mut st.clocks[frame.id];
        clock.depth = clock.depth.saturating_sub(1);
        clock.booked += 1;
        clock.self_time += elapsed.saturating_sub(frame.child);
        if frame.outermost {
            clock.outermost_time += elapsed;
        }
        if let Some(parent) = st.stack.last_mut() {
            parent.child += elapsed;
        }
    });
}

/// Record one builtin call by name (already native — shows what the fns bottom out in).
pub(super) fn record_builtin(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        bump(
            &mut s.borrow_mut().get_or_insert_with(State::default).builtins,
            name,
        );
    });
}

/// Record one module instantiation by name.
pub(super) fn record_module(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        bump(
            &mut s.borrow_mut().get_or_insert_with(State::default).modules,
            name,
        );
    });
}

/// Record one intrinsic dispatch by name (a user-fn call that went NATIVE instead of interpreting — the
/// already-covered side of the worklist).
pub(super) fn record_intrinsic(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        bump(
            &mut s.borrow_mut().get_or_insert_with(State::default).intrinsics,
            name,
        );
    });
}

/// Print the top entries of one kind, sorted by descending call count (ties broken by name for a stable
/// report). Shows every entry up to `limit`, then a one-line "... and N more" tail so a truncated section
/// never reads as "that's all there is" (LOUD about what it dropped).
#[allow(
    clippy::cast_precision_loss,
    reason = "a call-count percentage in a dev-only stderr report — precision past 2^52 calls is cosmetic"
)]
fn print_section(label: &str, map: &BTreeMap<String, u64>, limit: usize) {
    if map.is_empty() {
        return;
    }
    let total: u64 = map.values().sum();
    let mut rows: Vec<(&str, u64)> = map.iter().map(|(n, &c)| (n.as_str(), c)).collect();
    rows.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    eprintln!(
        "\n[fnprofile] === {label} === {} distinct, {total} calls",
        map.len()
    );
    for (name, count) in rows.iter().take(limit) {
        let pct = 100.0 * *count as f64 / total as f64;
        eprintln!("[fnprofile]   {count:>10}  {pct:5.1}%  {name}");
    }
    if rows.len() > limit {
        let shown: u64 = rows.iter().take(limit).map(|(_, c)| c).sum();
        eprintln!(
            "[fnprofile]   ... and {} more ({} calls not shown)",
            rows.len() - limit,
            total - shown
        );
    }
}

/// Print the user-fn TIME table: self-time ranked (the intrinsic worklist), with outermost-inclusive and
/// booked-call columns. `%self` is of the summed self time — the interpreter's total user-fn cost — so the
/// top rows ARE the worklist cut. Same LOUD-tail rule as [`print_section`].
#[allow(
    clippy::cast_precision_loss,
    reason = "stderr percentages in a dev-only report — cosmetic past 2^52"
)]
fn print_time_section(clocks: &[FnClock], limit: usize) {
    if clocks.is_empty() {
        return;
    }
    let total_self: Duration = clocks.iter().map(|c| c.self_time).sum();
    let mut rows: Vec<&FnClock> = clocks.iter().collect();
    rows.sort_unstable_by(|a, b| {
        b.self_time
            .cmp(&a.self_time)
            .then_with(|| a.name.cmp(&b.name))
    });
    eprintln!(
        "\n[fnprofile] === user-fn SELF time (the worklist) === {} distinct, {:.3}s total self",
        clocks.len(),
        total_self.as_secs_f64()
    );
    eprintln!("[fnprofile]      self-ms  %self  outermost-ms       calls  name");
    for c in rows.iter().take(limit) {
        let pct = 100.0 * c.self_time.as_secs_f64() / total_self.as_secs_f64().max(1e-9);
        eprintln!(
            "[fnprofile]   {:>10.1}  {pct:5.1}  {:>12.1}  {:>10}  {}",
            c.self_time.as_secs_f64() * 1e3,
            c.outermost_time.as_secs_f64() * 1e3,
            c.booked,
            c.name
        );
    }
    if rows.len() > limit {
        let shown: Duration = rows.iter().take(limit).map(|c| c.self_time).sum();
        eprintln!(
            "[fnprofile]   ... and {} more ({:.3}s self not shown)",
            rows.len() - limit,
            (total_self.saturating_sub(shown)).as_secs_f64()
        );
    }
}

/// Print the per-name call profile to stderr (once, when enabled) and clear state. Called after the top-level
/// eval completes. A no-op when the probe is off or saw no calls.
pub(super) fn report() {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let Some(st) = s.borrow_mut().take() else {
            return;
        };
        print_time_section(&st.clocks, REPORT_LIMIT);
        print_section(
            "user functions (intrinsic targets)",
            &st.funcs,
            REPORT_LIMIT,
        );
        print_section("builtins", &st.builtins, REPORT_LIMIT);
        print_section("modules", &st.modules, REPORT_LIMIT);
        print_section(
            "intrinsic dispatches (already native)",
            &st.intrinsics,
            REPORT_LIMIT,
        );
    });
}
