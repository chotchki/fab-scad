//! N.2c eval-memo cache — memoize USER-FUNCTION-call results so an 82–92%-redundant BOSL2 call graph
//! (measured, `docs/models-profile.md`) evaluates each distinct call ONCE. Design + the adversarial review
//! that shaped it: `docs/eval-cache-design.md`. It is correctness-CRITICAL — a wrong hit is a silently wrong
//! mesh — so the key is EXACT and the fence is conservative.
//!
//! ## Key
//! A memoized result is a pure function of `(which function, its captured env, its args, the reaching
//! $-context)`:
//!   - `body` — the body `Expr` pointer (as `usize`); the AST outlives the per-program cache, stable+unique.
//!   - `env`  — the lexical base (a named fn's home global = stable; a CLOSURE's captured env = per-instance),
//!     HELD as a `Scope` so its `Rc<Frame>` can't be freed + its address recycled under a live entry (ABA),
//!     compared by frame pointer. Without this, two closures sharing a body but capturing different free vars
//!     would collide → wrong mesh (review B1).
//!   - `dyn_ctx` — the reaching-`$`-context IDENTITY (`Scope::dyn_ctx`), an O(1) read that sidesteps the
//!     O(depth) `specials()` walk (review B2); HELD as its `Rc` (ABA), compared by pointer.
//!   - `args` — the bound argument `Value`s, compared BIT-EXACT (`to_bits`: `+0`≠`-0`, `NaN`==`NaN`) because
//!     `f(+0)`/`f(-0)` can diverge; a `Value::Function` arg keys on `(closure_id, self_name)`, never
//!     `Value::==` (which has no `Function` arm → would never match → silently un-cache the higher-order slice).
//!
//! Collision safety: `HashMap` with a bit-EXACT `Eq`, so a hash collision resolves to a real compare, never a
//! wrong hit. A fixed-seed hasher (not `RandomState`) makes the cache run-reproducible for debugging.
//!
//! ## Purity fence (in the `Apply`/`CacheStore` handlers, not here)
//! Only a call whose evaluation left NO observable side effect is stored — snapshot
//! `(messages.len, rand_stream.draws, closures.len, impure_reads)` at the miss, and store only if all are
//! unchanged when the body's value lands. That catches `echo`/warnings, seedless `rands`, closure creation,
//! and `parent_module` (which bumps `impure_reads`) — transitively, for free.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;
use std::sync::OnceLock;

use crate::parser::Expr;

use super::scope::{DynCtxNode, Scope};
use super::value::Value;

/// Is the cache on? Default OFF, opt-in with `FAB_EVAL_CACHE=1`. It is CORRECT (bit-identical to no-cache
/// across the BOSL2 corpus) and a real win where redundancy is high with expensive bodies (+18–27% on the hot
/// models), but on low-benefit call-heavy models the per-call GATE overhead alone is a net loss (~-16% on
/// under_sink_guide) — do-no-harm says don't inflict that by default. The follow-up that makes default-ON safe
/// is a program-level AUTO-OFF (self-disable when the running cost/benefit shows net-negative). The A/B
/// differential (cache on == off) still runs by toggling this, mirroring M.3's `FAB_GEO_DRIVER`.
pub(super) fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var_os("FAB_EVAL_CACHE").as_deref() == Some(std::ffi::OsStr::new("1")))
}

/// Per-generation entry cap. Two generations (hot/cold) bound the cache to ~2× this — a rotate-not-scan LRU
/// approximation (review: unbounded risks OOM + cancels the drop-cost saving). The concentration is high
/// (10 keys = 22–61% of calls), so a modest cap captures most of the win.
const GEN_CAP: usize = 1 << 16;

/// Skip caching a call whose args are too BIG to be worth keying (review: cost-weight the memo). Cloning +
/// bit-hashing a huge list arg on every call is overhead the body-skip may never repay — it's exactly what
/// tips low-redundancy stress tests (`gaussian_rands`' 300k-element comprehension, high-`$fn` vertex math)
/// over their budget while the redundant SMALL-key calls (the 82–92% win) sail through. Shallow element count
/// (top-level list/string lengths, O(#args)); over the cap → don't cache. Tune with `FAB_EVAL_CACHE_ARGCAP`.
fn arg_cap() -> usize {
    static C: OnceLock<usize> = OnceLock::new();
    *C.get_or_init(|| {
        std::env::var("FAB_EVAL_CACHE_ARGCAP").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
    })
}

/// Is this call's argument list small enough to be worth memoizing? See [`arg_cap`].
pub(super) fn worth_caching(args: &[Value]) -> bool {
    let cap = arg_cap();
    let mut total = 0usize;
    for v in args {
        total += match v {
            Value::NumList(xs) => xs.len(),
            Value::List(xs) => xs.len(),
            Value::Str(s) => s.len(),
            _ => 1,
        };
        if total > cap {
            return false;
        }
    }
    true
}

type FixedHasher = BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

/// The exact memo key. `env`/`dyn_ctx` are HELD (their `Rc`s pin the pointers we compare by — no ABA).
pub(super) struct Key {
    body: usize,
    env: Scope,
    dyn_ctx: Rc<DynCtxNode>,
    args: Box<[Value]>,
}

impl Key {
    /// Build the key for a call at `Apply`: `body` = the function, `base` = its lexical env, `args` = its
    /// bound argument values, `caller` = the scope whose reaching `$`-context the callee inherits.
    pub(super) fn new(body: &Expr, base: &Scope, args: &[Value], caller: &Scope) -> Self {
        Self {
            body: std::ptr::from_ref(body) as usize,
            env: base.clone(),
            dyn_ctx: caller.dyn_ctx(),
            args: args.to_vec().into_boxed_slice(),
        }
    }
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.body.hash(h);
        self.env.frame_id().hash(h);
        (Rc::as_ptr(&self.dyn_ctx) as usize).hash(h);
        for v in &self.args {
            hash_value_bits(v, h);
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, o: &Self) -> bool {
        self.body == o.body
            && self.env.frame_id() == o.env.frame_id()
            && Rc::ptr_eq(&self.dyn_ctx, &o.dyn_ctx)
            && self.args.len() == o.args.len()
            && self.args.iter().zip(&o.args).all(|(a, b)| value_bits_eq(a, b))
    }
}
impl Eq for Key {}

/// Hash a `Value` BIT-EXACTLY (matches [`value_bits_eq`]). Shared with the CSG-memo cache
/// ([`mod_cache`](super::mod_cache)) so both caches key `Value`s by the identical bit-exact rule.
pub(super) fn hash_value_bits<H: Hasher>(v: &Value, h: &mut H) {
    std::mem::discriminant(v).hash(h);
    match v {
        Value::Undef => {}
        Value::Bool(b) => b.hash(h),
        Value::Num(n) => h.write_u64(n.to_bits()),
        Value::Str(s) => s.hash(h),
        Value::NumList(xs) => {
            for x in xs.iter() {
                h.write_u64(x.to_bits());
            }
        }
        Value::List(xs) => {
            for e in xs.iter() {
                hash_value_bits(e, h);
            }
        }
        Value::Range { start, step, end } => {
            h.write_u64(start.to_bits());
            h.write_u64(step.to_bits());
            h.write_u64(end.to_bits());
        }
        // A closure arg: identity is (closure_id, self_name) — closure_id carries its env + params, self_name
        // pins a tagged/untagged pair of the same id apart. NEVER Value::== (no Function arm).
        Value::Function { closure_id, self_name, .. } => {
            closure_id.hash(h);
            self_name.hash(h);
        }
    }
}

/// Bit-exact `Value` equality for the key (`+0`≠`-0`, `NaN`==`NaN`) — stricter than `Value::==`, so it never
/// yields a wrong hit. Shared with the CSG-memo cache ([`mod_cache`](super::mod_cache)).
pub(super) fn value_bits_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undef, Value::Undef) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::NumList(x), Value::NumList(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| a.to_bits() == b.to_bits())
        }
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| value_bits_eq(a, b))
        }
        (
            Value::Range { start: s1, step: t1, end: e1 },
            Value::Range { start: s2, step: t2, end: e2 },
        ) => s1.to_bits() == s2.to_bits() && t1.to_bits() == t2.to_bits() && e1.to_bits() == e2.to_bits(),
        (
            Value::Function { closure_id: c1, self_name: n1, .. },
            Value::Function { closure_id: c2, self_name: n2, .. },
        ) => c1 == c2 && n1 == n2,
        _ => false,
    }
}

/// A two-generation bounded memo. `get` promotes a cold hit to hot; when hot fills, hot rotates to cold and a
/// fresh hot starts — evicting the older generation without an O(n) scan. Lookup-only in the sense that no
/// value is ever produced by iterating the maps; eviction changes hit/miss, never output.
pub(super) struct Cache {
    hot: HashMap<Key, Value, FixedHasher>,
    cold: HashMap<Key, Value, FixedHasher>,
}

impl Default for Cache {
    fn default() -> Self {
        Self { hot: HashMap::default(), cold: HashMap::default() }
    }
}

impl Cache {
    /// A cached result for `key`, if any. A cold hit is PROMOTED to hot so it survives the next rotation.
    pub(super) fn get(&mut self, key: &Key) -> Option<Value> {
        if let Some(v) = self.hot.get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.cold.remove(key) {
            let out = v.clone();
            self.insert_hot(clone_key(key), v);
            return Some(out);
        }
        None
    }

    /// Memoize `val` under `key`.
    pub(super) fn put(&mut self, key: Key, val: Value) {
        self.insert_hot(key, val);
    }

    fn insert_hot(&mut self, key: Key, val: Value) {
        if self.hot.len() >= GEN_CAP {
            self.cold = std::mem::take(&mut self.hot); // rotate generations (evict the older)
        }
        self.hot.insert(key, val);
    }
}

/// Clone a key to re-insert a promoted cold hit. Cheap: `Rc`/`Scope` bumps + an arg `Value` clone (also
/// bumps for the heap variants).
fn clone_key(k: &Key) -> Key {
    Key {
        body: k.body,
        env: k.env.clone(),
        dyn_ctx: Rc::clone(&k.dyn_ctx),
        args: k.args.clone(),
    }
}

/// The cache handle stored in `Ctx` — a `RefCell` because eval borrows it mutably on every call.
pub(super) type CacheCell = RefCell<Cache>;
