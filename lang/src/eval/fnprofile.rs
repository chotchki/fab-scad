//! Dev probe (O.2, 2026-07-08): which functions/builtins/modules does a model call, and how OFTEN?
//! The N.1 sampling profiler attributes self-time to Rust SYMBOLS (`lc_for`, `check_assert`) — but every
//! BOSL2 function evaluates through the SAME eval loop, so samply CAN'T tell `is_vector` from `is_path`.
//! This probe closes that gap: a per-NAME call counter, so intrinsic-picking (O.2) aims at the functions a
//! real model actually leans on instead of a guess. Pairs with [`super::redundancy`] — that probe measures
//! the memoization CEILING (how many calls repeat their key), this one names WHICH functions to make cheap.
//!
//! Counts three call kinds separately: user `fn`s (the intrinsic targets), `builtin`s (already native, but
//! they show what the fns bottom out in), and `module` instantiations. A call is counted where it's
//! DISPATCHED, so a cache HIT still counts (it's a logical call the intrinsic would replace); a closure
//! called by value isn't name-counted (it has no static name — a v1 gap, BOSL2 predicates are all named).
//!
//! Gated by `FAB_PROFILE_FNS=1`; when off, each `record_*` is one atomic-bool load and returns. First sight
//! of a name allocs its key; repeats are a `BTreeMap` lookup + increment. Report to stderr via [`report`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// How many rows per section the report prints before collapsing the tail into a "... and N more" line.
const REPORT_LIMIT: usize = 30;

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("FAB_PROFILE_FNS").is_some())
}

/// Per-kind call tallies. `BTreeMap` (not `HashMap`) so the record path is deterministic and the report can
/// still re-sort by count; the name key allocs only on a name's FIRST sighting (lookup-then-insert below).
#[derive(Default)]
struct State {
    funcs: BTreeMap<String, u64>,
    builtins: BTreeMap<String, u64>,
    modules: BTreeMap<String, u64>,
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
/// partial runs don't bleed into the final complete one (mirrors [`super::redundancy::reset`]).
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
    STATE.with(|s| bump(&mut s.borrow_mut().get_or_insert_with(State::default).funcs, name));
}

/// Record one builtin call by name (already native — shows what the fns bottom out in).
pub(super) fn record_builtin(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| bump(&mut s.borrow_mut().get_or_insert_with(State::default).builtins, name));
}

/// Record one module instantiation by name.
pub(super) fn record_module(name: &str) {
    if !enabled() {
        return;
    }
    STATE.with(|s| bump(&mut s.borrow_mut().get_or_insert_with(State::default).modules, name));
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
        print_section("user functions (intrinsic targets)", &st.funcs, REPORT_LIMIT);
        print_section("builtins", &st.builtins, REPORT_LIMIT);
        print_section("modules", &st.modules, REPORT_LIMIT);
    });
}
