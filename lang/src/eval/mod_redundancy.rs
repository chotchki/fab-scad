//! Dev probe (J.5.1 — the CSG-cache decision): would a content-addressed MODULE-instantiation memo PAY on the
//! geometry pass? The sibling of [`redundancy`](super::redundancy) — that one brackets the FUNCTION-call cache
//! ceiling (N.2c); THIS one brackets the MODULE-call ceiling (J.5), the cache that would memoize a whole `Geo`
//! subtree so a redundant nested-partition rebuild runs ONCE instead of every time.
//!
//! It answers the fork the models profile left open: is a slow model (`slice_parts`) REDUNDANT — the same
//! sub-geometry rebuilt over and over, which a memo collapses — or a combinatorial BLOWUP of DISTINCT geometry,
//! which no cache can help? `distinct vs total` (overall AND per-module) is the discriminator: distinct ≪ total
//! is redundancy the cache eats; distinct ≈ total is a blowup the cache can't touch.
//!
//! Each call is keyed two ways, BRACKETING the true ceiling (same shape as the function probe):
//!   - `(module, params)` — IGNORES the reaching `$`-context. A correct cache MUST add it (adding a key
//!     component only SPLITS keys further), so this is a strict UPPER bound. Low here → the cache is dead on
//!     arrival, end of discussion.
//!   - `(module, params, reaching $-context)` — ALL reaching `$`-vars of the CALL frame: `$fn`/`$fa`/`$fs`,
//!     `$children`, `$parent_modules`, and any `$`-args. A module body reads only SOME of them, so this
//!     OVER-specifies the key → a LOWER bound. A realistic content-hashed cache lands here or a touch above.
//!
//! Pointer-identity keying (what N.2c uses for FUNCTIONS) is NOT measured — it's structurally dead for modules:
//! `push_user_module` binds `$children` on EVERY call, and a `$`-bind mints a fresh `dyn_ctx` (`scope.rs`), so
//! every module call's frame is a distinct pointer → ~0% hits. Content-hashing the `$`-context is the only
//! viable module key, and a module call is heavyweight enough to amortize the `specials()` walk the function
//! cache couldn't afford. `params` identity is the resolved VALUES (bit-exact), never the arg exprs.
//!
//! Gated by `FAB_CSG_REDUNDANCY=1`; when off, [`record`] is one atomic-bool load and returns. Rolling stderr
//! prints every `FAB_CSG_REDUNDANCY_ROLL` calls (default 50k) so a model that never finishes (the timeout
//! bucket) still shows its accumulating ratio before the parent harness kills it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use crate::parser::Parameter;

use super::scope::Scope;
use super::value::Value;

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("FAB_CSG_REDUNDANCY").is_some())
}

/// Rolling-report interval (calls). A model in the timeout bucket never reaches [`report`], so dump the
/// accumulating ratio every this-many calls; the last line before the kill is the reading. `0` disables.
fn roll_every() -> u64 {
    static R: OnceLock<u64> = OnceLock::new();
    *R.get_or_init(|| {
        std::env::var("FAB_CSG_REDUNDANCY_ROLL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50_000)
    })
}

/// Per-module accounting — the blowup discriminator, split out by definition so the report names the culprit.
#[derive(Default)]
struct ModStat {
    name: String,
    total: u64,
    distinct_ctx: BTreeSet<u64>, // distinct (module, params, $-context) keys seen for THIS module
}

#[derive(Default)]
struct State {
    total: u64,
    no_ctx: BTreeMap<u64, u64>, // key(module, params)              -> occurrences (UPPER-bound bracket)
    with_ctx: BTreeMap<u64, u64>, // key(module, params, $-context)   -> occurrences (LOWER-bound bracket)
    // by body ptr -> its call/redundancy split. BTreeMap so the report's tie ROWS come out in a stable
    // order (a HashMap here made equal-redundancy modules print in random order).
    per_module: BTreeMap<u64, ModStat>,
    key_elems: u64, // total Value elements hashed — the key-SIZE a real cache would pay
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start (or restart) a measurement — called at each top-level geometry eval so the import fixpoint's earlier
/// partial runs don't bleed into the final complete one (mirrors [`redundancy::reset`](super::redundancy)).
pub(super) fn reset() {
    if enabled() {
        STATE.with(|s| *s.borrow_mut() = Some(State::default()));
    }
}

/// Hash a value deterministically into `h`, counting the elements walked. Bit-exact for numbers (`to_bits`, so
/// `NaN`/`±0` are stable), recursive for lists — the same shape a real cache key needs. (A copy of the function
/// probe's walker; the two probes are independent instruments.)
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
        Value::Object(o) => {
            for (name, val) in o.iter() {
                name.hash(h);
                hash_value(val, h, elems);
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

/// Record one USER-module instantiation. `body` identifies the module def (stable AST pointer), `home` is its
/// lexical base (the home-island global, or a scope-local module's captured defining scope — the same
/// disambiguator the eval cache holds so two look-alike defs don't collide), `name` is for the report, `params`
/// are the module's declared parameters, and `call` is the fully-bound call frame — its `specials()` are the
/// reaching `$`-context and each `param.name` looks up its RESOLVED value. Off unless `FAB_CSG_REDUNDANCY=1`.
pub(super) fn record(
    body: *const (),
    home: &Scope,
    name: &str,
    params: &[Parameter],
    call: &Scope,
) {
    if !enabled() {
        return;
    }
    // Module IDENTITY = (body ptr, lexical-base ptr): a scope-local module shares its body AST across the
    // definitions that captured it but carries a distinct base, exactly the closure-key concern (redundancy B1).
    let mod_id = (body as u64) ^ (home.frame_id() as u64).rotate_left(1);
    // Resolved param VALUES in declaration order (what the body actually sees), read back out of the call frame.
    let param_vals: Vec<Value> = params.iter().map(|p| call.lookup(&p.name)).collect();
    let specials = call.specials();

    STATE.with(|s| {
        let mut guard = s.borrow_mut();
        let st = guard.get_or_insert_with(State::default);
        st.total += 1;

        // (module, params) — the upper-bound key.
        let mut elems = 0u64;
        let k_no = hash64(mod_id, |h| {
            for v in &param_vals {
                hash_value(v, h, &mut elems);
            }
        });
        st.key_elems += elems;
        *st.no_ctx.entry(k_no).or_default() += 1;

        // (module, params, reaching $-context) — the lower-bound key. `specials()` is a sorted BTreeMap (dedup +
        // deterministic order), so equal effective contexts hash equal.
        let k_ctx = hash64(k_no, |h| {
            for (n, val) in &specials {
                n.hash(h);
                let mut e = 0u64;
                hash_value(val, h, &mut e);
            }
        });
        *st.with_ctx.entry(k_ctx).or_default() += 1;

        // Per-module split (the blowup discriminator): keyed by body ptr (unique per def), name stored once.
        let ms = st.per_module.entry(mod_id).or_default();
        if ms.name.is_empty() {
            ms.name = name.to_string();
        }
        ms.total += 1;
        ms.distinct_ctx.insert(k_ctx);

        let roll = roll_every();
        if roll != 0 && st.total % roll == 0 {
            print_report(st, "rolling");
        }
    });
}

/// Print the redundancy report to stderr (once, when enabled) and clear state. Called after the top-level
/// geometry eval completes. A no-op when the probe is off or saw no module calls.
pub(super) fn report() {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let Some(st) = s.borrow_mut().take() else {
            return;
        };
        if st.total == 0 {
            eprintln!("[mod-redundancy] no user-module instantiations recorded");
            return;
        }
        print_report(&st, "final");
    });
}

/// Render the bracket + the per-module culprit table. Shared by the rolling and final passes.
#[allow(
    clippy::cast_precision_loss,
    reason = "probe counters rendered as stderr percentages — call counts never approach 2^52"
)]
fn print_report(st: &State, label: &str) {
    let total = st.total as f64;
    let distinct_no = st.no_ctx.len() as f64;
    let distinct_ctx = st.with_ctx.len() as f64;
    let red_no = 100.0 * (1.0 - distinct_no / total);
    let red_ctx = 100.0 * (1.0 - distinct_ctx / total);
    let avg_key = st.key_elems as f64 / total;

    eprintln!(
        "\n[mod-redundancy] === CSG-memo cache ceiling ({label}: user-module instantiations) ==="
    );
    eprintln!("[mod-redundancy] total instantiations:   {}", st.total);
    eprintln!(
        "[mod-redundancy] distinct (mod,params):     {}  -> redundancy {red_no:.1}%  (UPPER bound on any cache)",
        st.no_ctx.len()
    );
    eprintln!(
        "[mod-redundancy] distinct (mod,params,$ctx): {}  -> redundancy {red_ctx:.1}%  (LOWER bound — $ctx over-specified)",
        st.with_ctx.len()
    );
    eprintln!(
        "[mod-redundancy] true cache-hit ceiling is BRACKETED: [{red_ctx:.1}% .. {red_no:.1}%]"
    );
    eprintln!(
        "[mod-redundancy] avg key size:            {avg_key:.1} Value-elements hashed / call"
    );

    // The blowup discriminator, per module: sort by REDUNDANT calls (total - distinct$ctx) — the calls a cache
    // would eliminate. A module with distinct ≈ total is a BLOWUP (nothing to hit); distinct ≪ total is the win.
    let mut mods: Vec<&ModStat> = st.per_module.values().collect();
    mods.sort_unstable_by_key(|m| std::cmp::Reverse(m.total - m.distinct_ctx.len() as u64));
    eprintln!(
        "[mod-redundancy] top modules by REDUNDANT calls (total / distinct$ctx / redundancy%):"
    );
    for m in mods.iter().take(15) {
        let d = m.distinct_ctx.len() as u64;
        let pct = 100.0 * (1.0 - d as f64 / m.total as f64);
        eprintln!(
            "[mod-redundancy]   {:<28} {:>9} / {:>9} / {:>5.1}%",
            m.name, m.total, d, pct
        );
    }
}
