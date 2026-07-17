//! J.5.2 — the CSG-memo cache: memoize a USER-MODULE call's produced [`Geo`](super::Geo) subtree so a
//! redundant nested-partition rebuild runs ONCE. The geometry sibling of the N.2c eval cache
//! ([`eval_cache`](super::eval_cache)) — that one memoizes FUNCTION values keyed on (fn, env, args, $-ctx);
//! this one memoizes a whole `Geo` at the user-module boundary (`geo_stack::push_user_module`), the redundancy
//! the value cache can't see (measured [`mod_redundancy`](super::mod_redundancy): `slice_parts` is 99.4% redundant
//! in (module,params), ~42% with the full reaching $-context). It is correctness-CRITICAL — a wrong hit is a
//! silently wrong mesh — so keys are EXACT (bit-compared) and the eligibility fence is conservative.
//!
//! ## Eligibility — the `$children == 0` fence (rung 2a, unchanged)
//! Only a call with NO children is memoized. A module that renders `children()` depends on its CALL-SITE
//! children, which are NOT in the key (two callers pass different children under the same params + $-ctx) → a
//! wrong hit. `$children == 0` ⇒ `children()` renders NOTHING ⇒ the result is a pure function of the key. This
//! is exactly the leaf hot path (`cyl(h,r);`, `cube(size);` — the ~98.6%-redundant primitives BOSL2 wraps): a
//! child-less leaf is fully determined by (body, home, params) + the `$`-vars it actually READS.
//!
//! ## Key — rung 2b (BU.8, docs/mod-cache-rung2b-design.md): read-set-precise
//! Rung 2a keyed the FULL reaching `$`-context (all ~42 BOSL2 `$`-vars, bit-exact) — sound but over-wide:
//! BOSL2's distributors mint `$idx`/`$pos` per copy, so `xcopies(n) child()` missed N times on N identical
//! children. Rung 2b keys `(body ptr, home frame_id, resolved params)` and per-key stores ENTRIES of
//! `(observed $-read set, Geo)`:
//!   - While a cacheable body evaluates, a CAPTURE records every `$`-read that resolves AT-or-ABOVE the
//!     call's entry frame (the call frame itself carries the `$`-args / `$children` binds — part of the
//!     call's identity; frames minted DURING the body kill reads below them, the leaves-up gen/kill
//!     invariant performed by the scope walk itself). The recording hook is [`ReadWalk`], driven from the
//!     ONE choke point every `$`-read flows through: `Scope::lookup_opt`'s dynamic-chain walk.
//!   - A later call probes entries by resolving each recorded name in ITS context and bit-comparing
//!     (UNBOUND is its own marker — binding a var a stored trace saw missing must MISS). Soundness is the
//!     incremental-computation trace argument: branch decisions are themselves reads, so agreement on the
//!     recorded set forces the same trace.
//!   - On a HIT the matched entry's reads are REPLAYED (plain `lookup_opt`, recording live) so ENCLOSING
//!     captures inherit them — the reads logically happened even though the body was skipped.
//!   - `params`/read values are owned snapshots (bit-compared). `body` is ABA-safe because the AST
//!     outlives the cache; `home` is NOT address-safe by lifetime — frames drop mid-eval — so the key
//!     HOLDS the scope (see [`ModKey`]; the P.1.5.2 lesson).
//!
//! ## Purity fence (in the `CacheStoreModule` handler, `geo_stack` — unchanged)
//! Store only if the body left NO observable side effect — the SAME snapshot [`eval_cache`] uses
//! (messages / rand-draws / `impure_reads`). Catches echo/assert, seedless `rands`, `parent_module`,
//! transitively. `$children == 0` already fenced the children hazard, so no extra counter is needed here.
//!
//! ## The eval-cache interaction (BU.8 audit)
//! A FUNCTION-cache hit inside an active capture would SKIP the fn body — hiding its `$`-reads from the
//! capture → an under-recorded (unsound) store. [`captures_active`] lets `eval_cache::get` force a miss
//! while any capture is open (the enclosing CSG hit skips those bodies wholesale, so the loss is
//! second-order); stores stay allowed.

use std::cell::RefCell;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;

use super::Geo;
use super::eval_cache::{hash_value_bits, value_bits_eq};
use super::scope::Scope;
use super::value::Value;

// The gate (`Config::csg_cache`) is bit-identical to no-cache (the A/B differential toggles it); the per-call
// key-hash is do-no-harm overhead on a low-redundancy model, so default-ON waits on a program-level auto-off
// (as N.2c's). Enabled/cap now live in `Config`, read off `ctx.config` at the call site — not a local gate.

/// Per-generation entry cap — two generations (hot/cold) bound the cache to ~2× this, a rotate-not-scan LRU
/// approximation (same as [`eval_cache`]). A `Geo` subtree can be large, so this caps the tree, not just keys.
const GEN_CAP: usize = 1 << 15;

/// Per-base-key entry cap: distinct observed traces under one (body, home, params). Divergent `$`-driven
/// branches produce a few; a runaway (a module keyed on an unbounded `$`-value) evicts oldest-first instead
/// of growing.
const MAX_ENTRIES_PER_KEY: usize = 8;

/// Shallow key size: top-level element count over the resolved params, against `cap`
/// ([`Config::csg_cache_keycap`]). A param holding a huge list makes the per-call key hash a loss the
/// body-skip may not repay. O(#params), short-circuits at cap. (Rung 2b: the reaching `$`-context left the
/// key, so the gate is params-only; the READ SET gets the same cap at store time — [`reads_within_cap`].)
pub(super) fn worth_caching(params: &[Value], cap: usize) -> bool {
    let mut total = 0usize;
    for v in params {
        total += value_weight(v);
        if total > cap {
            return false;
        }
    }
    true
}

/// The store-time sibling of [`worth_caching`]: cap the OBSERVED read set the same way (a body that read a
/// huge `$`-list would make every later probe pay a deep bit-compare).
fn reads_within_cap(reads: &[(Rc<str>, ReadValue)], cap: usize) -> bool {
    let mut total = 0usize;
    for (_, rv) in reads {
        total += match rv {
            ReadValue::Bound(v) => value_weight(v),
            ReadValue::Unbound => 1,
        };
        if total > cap {
            return false;
        }
    }
    true
}

fn value_weight(v: &Value) -> usize {
    match v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        Value::Str(s) => s.len(),
        _ => 1,
    }
}

type FixedHasher = BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

/// Fixed-hasher map for the memo generations — same rationale as [`eval_cache`]'s `FixedMap`:
/// run-reproducible layout, lookup-only (never iterated for output), and the per-call gate path is
/// too hot for `BTreeMap`.
#[allow(
    clippy::disallowed_types,
    reason = "fixed hasher + lookup-only (never iterated for output); BTreeMap taxes the gate path"
)]
type FixedMap<K, V> = std::collections::HashMap<K, V, FixedHasher>;

/// The rung-2b BASE key: `(body, home, params)` — the reaching `$`-context is NOT here (it lives in each
/// entry's observed read set). `params` are owned snapshots (bit-compared).
///
/// `home` HOLDS the scope (its live `Rc<Frame>`), never a bare frame address: pointer identity is only
/// sound while both frames are ALIVE. The pre-fix `home: usize` stored `frame_id()` without holding the
/// frame — a dropped captured-defining-scope's address could be REUSED by a different frame (classic ABA,
/// allocator-layout-dependent), making a later call compare EQUAL to a stale key and serve the wrong
/// subtree (the P.1.5.2 `pill_holder` flake: one cuboid corner flipping rounded/sharp under
/// `MallocNanoZone=0`/`MallocPreScribble=1`). Same contract as `eval_cache`'s `Key.env` and
/// [`DynCtxNode`](super::scope): the key holds the `Rc` so the address can't recycle under it.
pub(super) struct ModKey {
    body: usize,
    home: Scope,
    params: Box<[Value]>,
}

impl ModKey {
    /// Build the base key for a `$children==0` module call: `body` = the module body pointer, `home` = its
    /// lexical base (home global / captured defining scope), `params` = resolved argument values in
    /// declaration order.
    pub(super) fn new(body: *const (), home: &Scope, params: &[Value]) -> Self {
        Self {
            body: body as usize,
            home: home.clone(),
            params: params.to_vec().into_boxed_slice(),
        }
    }
}

impl Hash for ModKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.body.hash(h);
        // Hashing the held frame's address is sound: the key keeps the frame alive, so the address is
        // stable and two equal-hashing LIVE frames at one address are genuinely the same frame.
        self.home.frame_id().hash(h);
        for v in &self.params {
            hash_value_bits(v, h);
        }
    }
}

impl PartialEq for ModKey {
    fn eq(&self, o: &Self) -> bool {
        self.body == o.body
            && self.home.same_frame(&o.home)
            && self.params.len() == o.params.len()
            && self
                .params
                .iter()
                .zip(&o.params)
                .all(|(a, b)| value_bits_eq(a, b))
    }
}
impl Eq for ModKey {}

// ─── The read capture (rung 2b) ─────────────────────────────────────────────────────────────────

/// One observed `$`-read: the value it resolved to, or UNBOUND (the walk exhausted the chain). UNBOUND is
/// its own marker — a context that later BINDS the name must miss (OpenSCAD distinguishes unbound from
/// bound-to-`undef` for its unknown-variable warning; the cache must too).
pub(super) enum ReadValue {
    Bound(Value),
    Unbound,
}

impl ReadValue {
    fn matches(&self, current: Option<&Value>) -> bool {
        match (self, current) {
            (ReadValue::Bound(v), Some(c)) => value_bits_eq(v, c),
            (ReadValue::Unbound, None) => true,
            _ => false,
        }
    }
}

/// An ACTIVE capture: one cacheable module call currently evaluating its body. `entry` is the call frame's
/// identity ([`Scope::frame_id`]) — the boundary the walk-crossing test keys on.
struct Capture {
    entry: u64,
    reads: Vec<(Rc<str>, ReadValue)>,
}

#[derive(Default)]
struct CaptureStack {
    /// Innermost capture LAST (a stack). Nesting mirrors the module-call stack, so the scope walk crosses
    /// entry frames innermost-first — the crossed set is always a suffix of this vec.
    caps: Vec<Capture>,
    /// Recording suppressed while > 0 (probe lookups, dev probes' `specials()` walks).
    suppress: u32,
}

thread_local! {
    /// Thread-local, not `Ctx`-threaded: `Scope::lookup_opt` has no `Ctx` access, and an eval machine runs
    /// on one thread (parallel test/eval isolation for free).
    static CAPTURES: RefCell<CaptureStack> = RefCell::default();
}

/// Open a capture for the cacheable call whose entry frame is `entry` (a MISS about to evaluate its body).
pub(super) fn open_capture(entry: u64) {
    CAPTURES.with(|c| {
        c.borrow_mut().caps.push(Capture {
            entry,
            reads: Vec::new(),
        });
    });
}

/// Close the capture for `entry` — called from the `PopModuleFrame` CLEANUP (both the happy path and the
/// error drain), so an errored body can't leak its capture. Pops only if the top matches: nested captures
/// pop LIFO on both paths, so a mismatch means a driver bug.
pub(super) fn close_capture(entry: u64) {
    CAPTURES.with(|c| {
        let mut s = c.borrow_mut();
        debug_assert_eq!(
            s.caps.last().map(|t| t.entry),
            Some(entry),
            "capture stack out of LIFO order"
        );
        if s.caps.last().is_some_and(|t| t.entry == entry) {
            s.caps.pop();
        }
    });
}

/// Snapshot the reads of the capture for `entry` (the top — the store handler runs before the frame pop).
/// Clones because the capture stays open until `PopModuleFrame`.
pub(super) fn capture_reads(entry: u64) -> Option<Vec<(Rc<str>, ReadValue)>> {
    CAPTURES.with(|c| {
        let s = c.borrow();
        let top = s.caps.last()?;
        (top.entry == entry).then(|| {
            top.reads
                .iter()
                .map(|(n, rv)| {
                    (
                        Rc::clone(n),
                        match rv {
                            ReadValue::Bound(v) => ReadValue::Bound(v.clone()),
                            ReadValue::Unbound => ReadValue::Unbound,
                        },
                    )
                })
                .collect()
        })
    })
}

/// Any capture open on this thread? The `eval_cache::get` bypass (module-doc: the fn-cache-hides-reads
/// hazard) and the replay-skip both key on this.
pub(super) fn captures_active() -> bool {
    CAPTURES.with(|c| !c.borrow().caps.is_empty())
}

/// Run `f` with read-recording suppressed — probe lookups (their reads are replayed explicitly on a hit)
/// and the dev probes' full-context `specials()` walks (diagnostics, not semantic reads).
pub(super) fn suppressed<R>(f: impl FnOnce() -> R) -> R {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            CAPTURES.with(|c| c.borrow_mut().suppress -= 1);
        }
    }
    CAPTURES.with(|c| c.borrow_mut().suppress += 1);
    let _guard = Guard; // decrements on unwind too — a caught panic must not kill recording forever
    f()
}

/// Reset this thread's capture state — called once at the top of a program evaluation, so a PANIC that
/// unwound a previous eval on a REUSED thread (the gui's crash-isolated worker catches panics) can't leave
/// stale captures / a stuck suppression behind. Never called mid-eval (nested drivers share the stack).
pub(super) fn reset_thread_state() {
    CAPTURES.with(|c| {
        let mut s = c.borrow_mut();
        s.caps.clear();
        s.suppress = 0;
    });
}

/// The per-lookup walk hook (`Scope::lookup_opt`, `$`-branch): [`visit`](Self::visit) every frame the walk
/// passes (BEFORE checking its map — a resolution AT a capture's entry frame is at-or-above), then
/// [`record`](Self::record) the outcome. Crossing entry frames innermost-first means the crossed set is a
/// suffix of the capture stack — `seen` counts it.
pub(super) struct ReadWalk {
    n: usize,
    seen: usize,
    live: bool,
}

impl ReadWalk {
    #[inline]
    pub(super) fn begin() -> Self {
        CAPTURES.with(|c| {
            let s = c.borrow();
            let live = !s.caps.is_empty() && s.suppress == 0;
            Self {
                n: s.caps.len(),
                seen: 0,
                live,
            }
        })
    }

    #[inline]
    pub(super) fn visit(&mut self, boundary: u64) {
        if !self.live {
            return;
        }
        CAPTURES.with(|c| {
            let s = c.borrow();
            while self.seen < self.n && s.caps[self.n - 1 - self.seen].entry == boundary {
                self.seen += 1;
            }
        });
    }

    /// Record the resolved value (`None` = UNBOUND) into every capture whose entry the walk crossed —
    /// first-read-wins per name (within one context the value can't differ between reads).
    #[inline]
    pub(super) fn record(self, name: &str, value: Option<&Value>) {
        if !self.live || self.seen == 0 {
            return;
        }
        CAPTURES.with(|c| {
            let mut s = c.borrow_mut();
            let n = self.n;
            for cap in &mut s.caps[n - self.seen..n] {
                if cap.reads.iter().any(|(rn, _)| &**rn == name) {
                    continue;
                }
                cap.reads.push((
                    Rc::from(name),
                    value.map_or(ReadValue::Unbound, |v| ReadValue::Bound(v.clone())),
                ));
            }
        });
    }
}

// ─── The cache ──────────────────────────────────────────────────────────────────────────────────

/// One stored trace under a base key: the `$`-reads the body performed (resolved at-or-above its entry)
/// and the `Geo` it produced.
struct Entry {
    reads: Box<[(Rc<str>, ReadValue)]>,
    geo: Geo,
}

/// A two-generation bounded memo (identical shape to [`eval_cache::Cache`]): `get` promotes a cold hit to hot;
/// a full hot rotates to cold and a fresh hot starts — eviction without an O(n) scan. `hits`/`misses`/`stores`
/// are the realized hit-rate counters (the redundancy probe measured the CEILING; these measure REALITY, gated
/// print via [`report`](ModCache::report)).
#[derive(Default)]
pub(super) struct ModCache {
    hot: FixedMap<ModKey, Vec<Entry>>,
    cold: FixedMap<ModKey, Vec<Entry>>,
    hits: u64,
    misses: u64,
    stores: u64,
    // Decline breakdown (which purity counter moved) — diagnoses WHY an eligible miss wasn't stored.
    declined_msg: u64,
    declined_draws: u64,
    declined_impure: u64,
    /// Stores declined because the OBSERVED read set blew the keycap (rung 2b's store-time gate).
    declined_wide: u64,
}

impl ModCache {
    /// Probe the entries under `key` against `call`'s context: an entry hits when every recorded read
    /// resolves to the same value TODAY (bit-exact; UNBOUND must still be unbound). Probe lookups run
    /// [`suppressed`]; on a hit the matched reads are REPLAYED live so enclosing captures inherit them
    /// (the design doc's hit-merge). A cold hit promotes the whole entry list.
    pub(super) fn get(&mut self, key: &ModKey, call: &Scope) -> Option<Geo> {
        let probe = |entries: &[Entry]| -> Option<usize> {
            suppressed(|| {
                entries.iter().position(|e| {
                    e.reads
                        .iter()
                        .all(|(name, rv)| rv.matches(call.lookup_opt(name).as_ref()))
                })
            })
        };
        let (geo, reads_to_replay): (Geo, Vec<Rc<str>>) = if let Some(entries) = self.hot.get(key) {
            let Some(i) = probe(entries) else {
                self.misses += 1;
                return None;
            };
            let e = &entries[i];
            (
                e.geo.clone(),
                e.reads.iter().map(|(n, _)| Rc::clone(n)).collect(),
            )
        } else if let Some(entries) = self.cold.remove(key) {
            let Some(i) = probe(&entries) else {
                self.cold.insert(clone_key(key), entries);
                self.misses += 1;
                return None;
            };
            let out = (
                entries[i].geo.clone(),
                entries[i].reads.iter().map(|(n, _)| Rc::clone(n)).collect(),
            );
            self.insert_hot(clone_key(key), entries);
            out
        } else {
            self.misses += 1;
            return None;
        };
        self.hits += 1;
        // Hit-merge: the skipped body's reads logically happened — replay them (recording live) so any
        // ENCLOSING capture records what this subtree depends on. Values were just verified equal.
        if captures_active() {
            for name in &reads_to_replay {
                let _ = call.lookup_opt(name);
            }
        }
        Some(geo)
    }

    /// Memoize `geo` under `key` with the body's observed read set (a MISS the purity fence cleared —
    /// `stores ≤ misses`, the gap being the impure/over-wide subtrees that re-run every time).
    pub(super) fn put(
        &mut self,
        key: ModKey,
        geo: Geo,
        reads: Vec<(Rc<str>, ReadValue)>,
        keycap: usize,
    ) {
        if !reads_within_cap(&reads, keycap) {
            self.declined_wide += 1;
            return;
        }
        self.stores += 1;
        let entry = Entry {
            reads: reads.into_boxed_slice(),
            geo,
        };
        if let Some(entries) = self.hot.get_mut(&key) {
            if entries.len() >= MAX_ENTRIES_PER_KEY {
                entries.remove(0); // oldest-first eviction inside one key
            }
            entries.push(entry);
            return;
        }
        // Promote any cold entry list so earlier traces stay probeable, then insert hot (consumes `key`).
        let mut entries = self.cold.remove(&key).unwrap_or_default();
        if entries.len() >= MAX_ENTRIES_PER_KEY {
            entries.remove(0);
        }
        entries.push(entry);
        self.insert_hot(key, entries);
    }

    /// Note an eligible miss the purity fence DECLINED, tagged by which counter moved (a body can trip more
    /// than one; count each). Diagnoses the store gap.
    pub(super) fn note_decline(&mut self, msg: bool, draws: bool, impure: bool) {
        self.declined_msg += u64::from(msg);
        self.declined_draws += u64::from(draws);
        self.declined_impure += u64::from(impure);
    }

    /// Print the realized hit-rate to stderr. The caller gates this on `ctx.config.csg_cache` (only meaningful
    /// when the cache ran).
    #[allow(
        clippy::cast_precision_loss,
        reason = "hit/miss counters rendered as a stderr percentage — call counts never approach 2^52"
    )]
    pub(super) fn report(&self) {
        #[cfg(test)]
        TEST_STATS.with(|s| s.set((self.hits, self.misses)));
        let lookups = self.hits + self.misses;
        if lookups == 0 {
            eprintln!("[csg-cache] no child-less module lookups (nothing eligible)");
            return;
        }
        let rate = 100.0 * self.hits as f64 / lookups as f64;
        eprintln!(
            "[csg-cache] lookups {lookups}  hits {}  ({rate:.1}%)  misses {}  stored {}  live-keys {}",
            self.hits,
            self.misses,
            self.stores,
            self.hot.len() + self.cold.len(),
        );
        eprintln!(
            "[csg-cache] declined stores by reason: messages {}  rand-draws {}  impure-reads {}  read-set-too-wide {}",
            self.declined_msg, self.declined_draws, self.declined_impure, self.declined_wide,
        );
    }

    fn insert_hot(&mut self, key: ModKey, entries: Vec<Entry>) {
        if self.hot.len() >= GEN_CAP {
            self.cold = std::mem::take(&mut self.hot); // rotate generations (evict the older)
        }
        self.hot.insert(key, entries);
    }
}

#[cfg(test)]
thread_local! {
    /// (hits, misses) of the last `report()` on this thread — the integration tests' observability
    /// (`report()` runs whenever `csg_cache` is on, at the end of `evaluate_source`).
    static TEST_STATS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(super) fn last_run_stats() -> (u64, u64) {
    TEST_STATS.with(std::cell::Cell::get)
}

/// Clone a base key to re-insert a promoted cold hit (an `Rc`/`Value` clone per component).
fn clone_key(k: &ModKey) -> ModKey {
    ModKey {
        body: k.body,
        home: k.home.clone(),
        params: k.params.clone(),
    }
}

/// The cache handle stored in `Ctx` — a `RefCell` because the geometry driver borrows it mutably per call.
pub(super) type CacheCell = RefCell<ModCache>;

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unit test: unwrap IS the assertion")]
mod tests {
    use super::super::{Config, Geo, Message};

    /// P.1.5.2 regression — the `ModKey` ABA: a key must HOLD its home frame, never just remember its
    /// address. Pre-fix `home: usize` stored `frame_id()` without keeping the frame alive; a dropped
    /// captured scope's address could be REUSED by a different frame (allocator-dependent), a later
    /// call then compared EQUAL to the stale key, and the memo served the WRONG module subtree —
    /// `pill_holder`'s cuboid corner flipping rounded/sharp under `MallocNanoZone=0` /
    /// `MallocPreScribble=1` (~50% wrong on the right binary layout, invisible on others). Pinned at
    /// the semantic level: key equality means SAME LIVE FRAME — a cloned scope (shared frame) is the
    /// same key, a structurally-identical distinct frame is a different key, and because the key holds
    /// the `Rc`, no address can recycle under it by construction.
    #[test]
    fn mod_key_home_identity_is_live_frame_not_address() {
        use crate::Scope;
        let body = std::ptr::null::<()>();
        let a = Scope::new();
        let a_clone = a.clone(); // shares a's frame
        let b = Scope::new(); // structurally identical to a, DISTINCT frame
        let k_a = super::ModKey::new(body, &a, &[]);
        let k_a2 = super::ModKey::new(body, &a_clone, &[]);
        let k_b = super::ModKey::new(body, &b, &[]);
        assert!(k_a == k_a2, "a shared frame is one identity");
        assert!(
            k_a != k_b,
            "distinct frames stay distinct keys even when structurally identical"
        );
        // The key OWNS the identity: with every Scope dropped, the held frames keep the keys' meaning
        // stable (the pre-fix bare-address form is exactly what this line makes impossible).
        drop(a);
        drop(a_clone);
        drop(b);
        assert!(k_a == k_a2 && k_a != k_b);
    }

    /// Eval `src` to its geometry tree + messages with the CSG cache forced `on` (everything else off — the
    /// A/B differential is exactly this toggle). In-memory, CWD base, no libs — the module-cache mechanics
    /// don't need BOSL2 (that's the model sweep's job).
    fn run(src: &str, on: bool) -> (Geo, Vec<Message>) {
        let cfg = Config {
            csg_cache: on,
            ..Config::default()
        };
        super::super::evaluate_source(src, std::path::Path::new("."), None, &[], cfg).unwrap()
    }

    fn geo(src: &str, on: bool) -> Geo {
        run(src, on).0
    }

    /// THE gate: cache-on == cache-off geometry, across every path the memo touches — child-less leaves
    /// (cacheable, repeated → hits), `$`-context variation via `$`-args (distinct read sets, no collision),
    /// nested + recursive modules, booleans/transforms/`for` (wrappers riding cached leaves), and the
    /// IMPURE / caller-dependent paths the fence + the `$children==0` gate must NOT serve stale (rands,
    /// echo, children). A wrong hit would make `on` diverge from `off`; `assert_eq` catches it.
    #[test]
    fn cache_on_equals_cache_off() {
        let programs = [
            // repeated child-less leaf → the redundant hit the cache exists for
            "module leaf(r){ sphere(r,$fn=8); } union(){ for(i=[0:4]) translate([i*3,0,0]) leaf(2); }",
            // same leaf, DIFFERENT reaching $fn (via $-arg) → distinct read-set entries, must not collide
            "module leaf(r){ sphere(r); } union(){ leaf(2,$fn=8); leaf(2,$fn=16); leaf(2,$fn=8); }",
            // nested modules, the outer repeated
            "module o(){ i(); } module i(){ cube(1); } union(){ o(); translate([2,0,0]) o(); }",
            // recursion (each depth distinct params; a sibling subtree repeats → hits)
            "module rec(n){ if(n>0){ cube(1); translate([2,0,0]) rec(n-1); } } rec(4);",
            // booleans over repeated leaves
            "module leaf(r){ sphere(r,$fn=8); } difference(){ leaf(3); leaf(2); leaf(2); }",
            "module leaf(r){ cube(r,center=true); } intersection(){ leaf(3); leaf(2); }",
            // IMPURE: seedless rands advances per call → three DIFFERENT spheres; the draws-fence must decline
            // so `on` reproduces the advancing sequence, not one stale sphere thrice.
            "module r(){ sphere(rands(1,2,1)[0],$fn=8); } union(){ r(); r(); r(); }",
            // CALLER-DEPENDENT: same module, same (args,$ctx), DIFFERENT children — the $children==0 gate keeps
            // these off the cache, so the union is cube+sphere, never cube+cube.
            "module w(){ children(); } union(){ w(){ cube(1); } w(){ sphere(1,$fn=8); } }",
            // echo side effect inside a repeated module (messages-fence declines)
            "module e(){ echo(\"x\"); cube(1); } union(){ e(); translate([2,0,0]) e(); }",
            // RUNG 2B, the distributor pattern: $idx minted per copy, the child never reads it → same
            // entry every copy (the xcopies unlock); geometry must be identical either way.
            "module leaf(){ sphere(1,$fn=8); } union(){ for(i=[0:4]) { $idx=i; translate([i*3,0,0]) leaf(); } }",
            // RUNG 2B, the wrong-hit killer: the child READS $idx → per-copy DIFFERENT geometry; a stale
            // hit would collapse the sizes. (Each copy probes, misses on the $idx value, stores its own entry.)
            "module rleaf(){ sphere($idx+1,$fn=8); } union(){ for(i=[0:3]) { $idx=i; translate([i*9,0,0]) rleaf(); } }",
            // RUNG 2B, UNBOUND is a value: first call sees $q unbound; a block then binds $q → must MISS
            // (an unbound-trace hit would render the cube branch under a bound $q).
            "module m(){ if(is_undef($q)) cube(1); else sphere(1,$fn=8); } union(){ m(); translate([3,0,0]) { $q=5; m(); } }",
            // ── the BU.8 adversarial-review programs (finding 1): a bind inside the body used to COW the
            // capture's ENTRY frame — pointer identity died, nothing recorded, the second call hit STALE.
            // Each pairs a body-top bind (assignment / for / $-arg / local fn / if-block) with two calls
            // under DIFFERENT reaching $-contexts; a vacuous hit collapses them to one shape.
            "module leaf(){ r = 1; sphere(r); } union(){ leaf($fn=8); translate([9,0,0]) leaf($fn=64); }",
            "module leaf(){ for(i=[0:0]) sphere(1); } union(){ leaf($fn=8); translate([9,0,0]) leaf($fn=64); }",
            "module leaf(){ sphere(1, $fa=12); } union(){ leaf($fn=8); translate([9,0,0]) leaf($fn=64); }",
            "module leaf(){ function f() = 1; sphere(f()); } union(){ leaf($fn=8); translate([9,0,0]) leaf($fn=64); }",
            "module leaf(){ if(true){ r = 2; sphere(r); } } union(){ leaf($fn=8); translate([9,0,0]) leaf($fn=64); }",
            "module leaf(){ r = 1; sphere(r); } union(){ { $fn=8; leaf(); } translate([9,0,0]) { $fn=64; leaf(); } }",
            "module leaf(){ for(i=[0:0]) sphere(4); } union(){ { $fa=3; $fs=0.3; leaf(); } translate([19,0,0]) { $fa=30; $fs=3; leaf(); } }",
        ];
        for src in programs {
            assert_eq!(
                geo(src, false),
                geo(src, true),
                "cache changed geometry:\n{src}"
            );
        }
    }

    /// The load-bearing wrong-hit: a child-RENDERING module keyed only on (args, reads) would serve the first
    /// call's children to the second. Two `w(){…}` calls with identical args but different children must stay
    /// distinct — the `$children==0` eligibility gate is what prevents the collision (both have $children=1 →
    /// never cached). Pinned directly (not just via the A/B loop) since it's the correctness crux.
    #[test]
    #[allow(
        clippy::panic,
        reason = "test assertion — the panic message carries the mismatched Geo shape"
    )]
    fn different_children_never_collide() {
        let both = geo(
            "module w(){ children(); } union(){ w(){ cube(1); } w(){ sphere(1,$fn=8); } }",
            true,
        );
        match &both {
            Geo::D3(super::super::GeoNode::Union(kids)) => {
                assert_eq!(kids.len(), 2, "both wraps must render — no dedup");
                assert_ne!(
                    kids[0], kids[1],
                    "a cube and a sphere, NOT the same shape twice (a wrong hit)"
                );
            }
            other => {
                panic!("expected a top-level union of the two wrapped children, got {other:?}")
            }
        }
    }

    /// A repeated echoing module must echo EVERY call with the cache on (a hit that skipped the body would
    /// drop the echo → a divergent console stream). Count is cache-invariant AND equals the call count.
    #[test]
    fn echoing_module_emits_every_call() {
        let src = "module e(){ echo(\"ping\"); } union(){ e(); e(); e(); }";
        let echoes = |on: bool| {
            run(src, on)
                .1
                .iter()
                .filter(|m| matches!(m, Message::Echo(s) if s.contains("ping")))
                .count()
        };
        assert_eq!(echoes(false), echoes(true), "cache changed the echo count");
        assert_eq!(
            echoes(true),
            3,
            "each of the 3 calls must echo — no dedup on a hit"
        );
    }

    /// RUNG 2B's whole point, measured: the distributor pattern ($idx minted per copy, child blind to it)
    /// must HIT on every copy after the first — rung 2a keyed the full $-context and missed all N.
    #[test]
    fn distributor_minted_specials_still_hit() {
        let src = "module leaf(){ sphere(1,$fn=8); } union(){ for(i=[0:49]) { $idx=i; translate([i*3,0,0]) leaf(); } }";
        let _ = run(src, true);
        let (hits, misses) = super::last_run_stats();
        assert_eq!(hits, 49, "49 of 50 copies must hit (the xcopies unlock)");
        assert_eq!(misses, 1, "only the first copy evaluates");
    }

    /// The counter-case: a child that READS the minted `$idx` must MISS on every copy (each probe compares
    /// the recorded $idx value and fails) — hits here would be wrong geometry, caught by the A/B test above;
    /// this pins the mechanism.
    #[test]
    fn reading_the_minted_special_misses_per_copy() {
        let src = "module rleaf(){ sphere($idx+1,$fn=8); } union(){ for(i=[0:3]) { $idx=i; translate([i*9,0,0]) rleaf(); } }";
        let _ = run(src, true);
        let (hits, misses) = super::last_run_stats();
        assert_eq!(
            hits, 0,
            "every copy reads a different $idx — no entry can match"
        );
        assert_eq!(misses, 4);
    }

    /// Repeats of the SAME $-context still hit across distinct entries: two $fn values interleaved → two
    /// entries under one base key, each hit on its rerun.
    #[test]
    fn interleaved_contexts_hit_their_own_entries() {
        let src = "module leaf(r){ sphere(r); } union(){ leaf(2,$fn=8); leaf(2,$fn=16); leaf(2,$fn=8); leaf(2,$fn=16); }";
        let _ = run(src, true);
        let (hits, misses) = super::last_run_stats();
        assert_eq!(misses, 2, "one evaluation per distinct $fn");
        assert_eq!(hits, 2, "each rerun hits its own entry");
    }
}
