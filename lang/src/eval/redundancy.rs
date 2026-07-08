//! Dev probe (N.2 / P.2 decision, 2026-07-08): would a content-addressed EVAL memo cache PAY? It measures
//! the THEORETICAL CEILING — the fraction of user-function calls whose key REPEATS, i.e. the best hit-rate a
//! perfect cache could ever get. The 57% eval-allocation the N.1 sampler found is the cost a cache would
//! skip on a hit; this says how many hits there are to be had.
//!
//! Each call is keyed two ways, which BRACKET the true ceiling:
//!   - `(fn, args)` — IGNORES the reaching `$`-context. A correct cache must include it, and adding it can
//!     only SPLIT keys further, so this is a strict UPPER BOUND on any correct cache's hit-rate. If it's
//!     low, the cache is dead on arrival — end of discussion.
//!   - `(fn, args, reaching $-context)` — ALL reaching `$`-vars. BOSL2 sets ~42 at top level and a loop-var
//!     like `$idx` changes per iteration, so this OVER-specifies the key (a real cache would depend on only
//!     the `$`-vars each fn READS) → a LOWER bound. The true ceiling sits in between.
//!
//! Gated by `FAB_REDUNDANCY=1`; when off, [`record`] is one atomic-bool load and returns. `fn` identity is
//! the body-`Expr` pointer (stable per definition, no name threading). Report to stderr via [`report`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use crate::parser::Expr;

use super::scope::Scope;
use super::value::Value;

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("FAB_REDUNDANCY").is_some())
}

#[derive(Default)]
struct State {
    total: u64,
    no_ctx: HashMap<u64, u64>,   // key(fn,args)            -> occurrences
    with_ctx: HashMap<u64, u64>, // key(fn,args,$-context)  -> occurrences
    key_elems: u64,              // total Value elements hashed — the key-SIZE a real cache would pay to hash
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start (or restart) a measurement — called at each `resolve_source` run so the import fixpoint's earlier
/// partial runs don't bleed into the final complete one.
pub(super) fn reset() {
    if enabled() {
        STATE.with(|s| *s.borrow_mut() = Some(State::default()));
    }
}

/// Hash a value deterministically into `h`, counting the elements walked (the key-size proxy). Exact-bit for
/// numbers (`to_bits`, so `NaN`/`±0` are stable), recursive for lists — the same shape a real cache key needs.
fn hash_value<H: Hasher>(v: &Value, h: &mut H, elems: &mut u64) {
    *elems += 1;
    std::mem::discriminant(v).hash(h);
    match v {
        Value::Undef => {}
        Value::Bool(b) => b.hash(h),
        Value::Num(n) => h.write_u64(n.to_bits()),
        Value::Str(s) => s.hash(h),
        Value::NumList(xs) => {
            for x in xs.iter() {
                h.write_u64(x.to_bits());
                *elems += 1;
            }
        }
        Value::List(xs) => {
            for e in xs.iter() {
                hash_value(e, h, elems);
            }
        }
        Value::Range { start, step, end } => {
            h.write_u64(start.to_bits());
            h.write_u64(step.to_bits());
            h.write_u64(end.to_bits());
        }
        Value::Function { closure_id, .. } => closure_id.hash(h),
    }
}

fn hash64(seed: u64, f: impl FnOnce(&mut std::collections::hash_map::DefaultHasher)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_u64(seed);
    f(&mut h);
    h.finish()
}

/// Record one user-function call: `body` identifies the function, `base` is its lexical base (the captured
/// ENV for a closure, the stable home global for a named fn — see the closure-key blocker B1), `args` are its
/// bound argument values (in order), `caller` carries the reaching `$`-context. Off unless `FAB_REDUNDANCY=1`.
pub(super) fn record(body: &Expr, base: &Scope, args: &[Value], caller: &Scope) {
    if !enabled() {
        return;
    }
    // fn IDENTITY is (body ptr, captured-env ptr): a closure shares the body AST with its siblings but
    // captures a distinct env, so the env ptr is what keeps `adder(1)` and `adder(2)` from colliding. For a
    // named fn the env is the stable home global (same ptr every call) → no effect. Without this, the ceiling
    // OVER-counts unsafe closure hits (the review's B1).
    let fn_id = (std::ptr::from_ref(body) as u64) ^ (base.frame_id() as u64).rotate_left(1);
    STATE.with(|s| {
        let mut guard = s.borrow_mut();
        let st = guard.get_or_insert_with(State::default);
        st.total += 1;

        // (fn, args) — the upper-bound key.
        let mut elems = 0u64;
        let k_no = hash64(fn_id, |h| {
            for a in args {
                hash_value(a, h, &mut elems);
            }
        });
        st.key_elems += elems;
        *st.no_ctx.entry(k_no).or_default() += 1;

        // (fn, args, reaching $-context) — the lower-bound key. `specials()` is a sorted BTreeMap (dedup +
        // deterministic order), so equal effective contexts hash equal. It allocates per call — that's the
        // probe's cost, dev-only, and a real cache pays a hash of the SAME data on every lookup anyway.
        let specials = caller.specials();
        let k_ctx = hash64(k_no, |h| {
            for (name, val) in &specials {
                name.hash(h);
                let mut e = 0u64;
                hash_value(val, h, &mut e);
            }
        });
        *st.with_ctx.entry(k_ctx).or_default() += 1;
    });
}

/// Print the redundancy report to stderr (once, when enabled) and clear state. Called after the top-level
/// eval completes. A no-op when the probe is off or saw no calls.
pub(super) fn report() {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let Some(st) = s.borrow_mut().take() else {
            return;
        };
        if st.total == 0 {
            eprintln!("[redundancy] no user-function calls recorded");
            return;
        }
        let total = st.total as f64;
        let distinct_no = st.no_ctx.len() as f64;
        let distinct_ctx = st.with_ctx.len() as f64;
        let red_no = 100.0 * (1.0 - distinct_no / total);
        let red_ctx = 100.0 * (1.0 - distinct_ctx / total);
        let avg_key = st.key_elems as f64 / total;

        // The concentration: how many calls the hottest keys absorb (a cache captures those cheaply).
        let mut top: Vec<u64> = st.no_ctx.values().copied().collect();
        top.sort_unstable_by(|a, b| b.cmp(a));
        let top10: u64 = top.iter().take(10).sum();

        eprintln!("\n[redundancy] === eval-memo cache ceiling (user-function calls) ===");
        eprintln!("[redundancy] total calls:            {}", st.total);
        eprintln!(
            "[redundancy] distinct (fn,args):      {}  -> redundancy {:.1}%  (UPPER bound on any cache)",
            st.no_ctx.len(),
            red_no
        );
        eprintln!(
            "[redundancy] distinct (fn,args,$ctx):  {}  -> redundancy {:.1}%  (LOWER bound — $ctx over-specified)",
            st.with_ctx.len(),
            red_ctx
        );
        eprintln!(
            "[redundancy] true cache-hit ceiling is BRACKETED: [{:.1}% .. {:.1}%]",
            red_ctx, red_no
        );
        eprintln!("[redundancy] avg key size:            {avg_key:.1} Value-elements hashed / call");
        eprintln!(
            "[redundancy] top-10 hottest (fn,args) keys absorb {} of {} calls ({:.1}%)",
            top10,
            st.total,
            100.0 * top10 as f64 / total
        );
    });
}
