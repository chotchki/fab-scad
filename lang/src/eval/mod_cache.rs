//! J.5.2a — the CSG-memo cache: memoize a USER-MODULE call's produced [`Geo`](super::Geo) subtree so a
//! redundant nested-partition rebuild runs ONCE. The geometry sibling of the N.2c eval cache
//! ([`eval_cache`](super::eval_cache)) — that one memoizes FUNCTION values keyed on (fn, env, args, $-ctx);
//! this one memoizes a whole `Geo` at the user-module boundary (`geo_stack::push_user_module`), the redundancy
//! the value cache can't see (measured [`mod_redundancy`](super::mod_redundancy): slice_parts is 99.4% redundant
//! in (module,params), ~42% with the full reaching $-context). It is correctness-CRITICAL — a wrong hit is a
//! silently wrong mesh — so the key is EXACT (bit-compared) and the eligibility fence is conservative.
//!
//! ## Eligibility — the `$children == 0` fence (rung 2a)
//! Only a call with NO children is memoized. A module that renders `children()` depends on its CALL-SITE
//! children, which are NOT in the key (two callers pass different children under the same params + $-ctx) → a
//! wrong hit. `$children == 0` ⇒ `children()` renders NOTHING ⇒ the result is a pure function of the key. This
//! is exactly the leaf hot path (`cyl(h,r);`, `cube(size);` — the ~98.6%-redundant primitives BOSL2 wraps): a
//! child-less leaf is fully determined by (body, home, params, reaching $-context). A wrapper WITH children
//! (`attachable`) isn't cached directly, but it rides inside its child-less ancestor's cached subtree. The
//! wider read-set-precise key that would also catch the wrapped calls is rung 2b.
//!
//! ## Key
//! `(body ptr, home frame_id, resolved params [bit-exact], reaching $-context CONTENT [bit-exact])`.
//!   - `body`  — the module body `Stmt` pointer; the AST outlives the per-program cache, stable + unique.
//!   - `home`  — the home-island global (or a scope-local module's captured defining scope) frame pointer, the
//!     same disambiguator [`eval_cache`] holds so two look-alike defs don't collide.
//!   - `params`/`dctx` — bit-EXACT (`to_bits`: `+0` ≠ `-0`, `NaN` == `NaN`) via [`eval_cache`]'s shared walker.
//!
//! Pointer-identity keying (what [`eval_cache`] uses for the $-ctx) is DEAD for modules: `push_user_module`
//! binds `$children` every call and a $-bind mints a fresh `dyn_ctx`, so every call is a distinct pointer →
//! ~0% hits. A module call is heavy enough to amortize the O(#$-vars) content hash the function cache couldn't.
//!
//! ## Purity fence (in the `CacheStoreModule` handler, `geo_stack`)
//! Store only if the body left NO observable side effect — the SAME snapshot [`eval_cache`] uses
//! (messages / rand-draws / closures / impure_reads). Catches echo/assert, seedless `rands`, `parent_module`,
//! transitively. `$children == 0` already fenced the children hazard, so no extra counter is needed here.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;

use super::Geo;
use super::eval_cache::{hash_value_bits, value_bits_eq};
use super::scope::Scope;
use super::value::Value;

/// The gate ([`Config::csg_cache`]) is bit-identical to no-cache (the A/B differential toggles it); the per-call
/// key-hash is do-no-harm overhead on a low-redundancy model, so default-ON waits on a program-level auto-off
/// (as N.2c's). Enabled/cap now live in [`Config`], read off `ctx.config` at the call site — not a local gate.

/// Per-generation entry cap — two generations (hot/cold) bound the cache to ~2× this, a rotate-not-scan LRU
/// approximation (same as [`eval_cache`]). A `Geo` subtree can be large, so this caps the tree, not just keys.
const GEN_CAP: usize = 1 << 15;

/// Shallow key size: top-level element count over the resolved params + reaching `$`-context, against `cap`
/// ([`Config::csg_cache_keycap`]). `specials()` is BOSL2's ~42 `$`-vars; a param or `$`-var holding a huge list
/// makes the per-call key hash a loss the body-skip may not repay. O(#params + #$-vars), short-circuits at cap.
pub(super) fn worth_caching(
    params: &[Value],
    specials: &BTreeMap<Rc<str>, Value>,
    cap: usize,
) -> bool {
    let mut total = 0usize;
    for v in params.iter().chain(specials.values()) {
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

/// The exact CSG-memo key. `params`/`dctx` are owned snapshots (bit-compared), so no ABA on the AST-outlives-
/// cache pointers we key `body`/`home` by.
pub(super) struct ModKey {
    body: usize,
    home: usize,
    params: Box<[Value]>,
    dctx: Box<[(Rc<str>, Value)]>,
}

impl ModKey {
    /// Build the key for a `$children==0` module call: `body` = the module body pointer, `home` = its lexical
    /// base (home global / captured defining scope), `params` = resolved argument values in declaration order,
    /// `specials` = the fully-bound call frame's reaching `$`-context ([`Scope::specials`], already sorted).
    pub(super) fn new(
        body: *const (),
        home: &Scope,
        params: &[Value],
        specials: &BTreeMap<Rc<str>, Value>,
    ) -> Self {
        Self {
            body: body as usize,
            home: home.frame_id(),
            params: params.to_vec().into_boxed_slice(),
            dctx: specials
                .iter()
                .map(|(k, v)| (Rc::clone(k), v.clone()))
                .collect(),
        }
    }
}

impl Hash for ModKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.body.hash(h);
        self.home.hash(h);
        for v in &self.params {
            hash_value_bits(v, h);
        }
        for (k, v) in &self.dctx {
            k.hash(h);
            hash_value_bits(v, h);
        }
    }
}

impl PartialEq for ModKey {
    fn eq(&self, o: &Self) -> bool {
        self.body == o.body
            && self.home == o.home
            && self.params.len() == o.params.len()
            && self
                .params
                .iter()
                .zip(&o.params)
                .all(|(a, b)| value_bits_eq(a, b))
            && self.dctx.len() == o.dctx.len()
            && self
                .dctx
                .iter()
                .zip(&o.dctx)
                .all(|((n1, v1), (n2, v2))| n1 == n2 && value_bits_eq(v1, v2))
    }
}
impl Eq for ModKey {}

/// A two-generation bounded memo (identical shape to [`eval_cache::Cache`]): `get` promotes a cold hit to hot;
/// a full hot rotates to cold and a fresh hot starts — eviction without an O(n) scan. `hits`/`misses`/`stores`
/// are the realized hit-rate counters (the redundancy probe measured the CEILING; these measure REALITY, gated
/// print via [`report`]).
pub(super) struct ModCache {
    hot: HashMap<ModKey, Geo, FixedHasher>,
    cold: HashMap<ModKey, Geo, FixedHasher>,
    hits: u64,
    misses: u64,
    stores: u64,
    // Decline breakdown (which purity counter moved) — diagnoses WHY an eligible miss wasn't stored.
    declined_msg: u64,
    declined_draws: u64,
    declined_impure: u64,
}

impl Default for ModCache {
    fn default() -> Self {
        Self {
            hot: HashMap::default(),
            cold: HashMap::default(),
            hits: 0,
            misses: 0,
            stores: 0,
            declined_msg: 0,
            declined_draws: 0,
            declined_impure: 0,
        }
    }
}

impl ModCache {
    /// A cached `Geo` for `key`, if any — a cold hit is PROMOTED to hot so it survives the next rotation.
    pub(super) fn get(&mut self, key: &ModKey) -> Option<Geo> {
        if let Some(g) = self.hot.get(key) {
            self.hits += 1;
            return Some(g.clone());
        }
        if let Some(g) = self.cold.remove(key) {
            self.hits += 1;
            let out = g.clone();
            self.insert_hot(clone_key(key), g);
            return Some(out);
        }
        self.misses += 1;
        None
    }

    /// Memoize `geo` under `key` (a MISS that the purity fence cleared — `stores ≤ misses`, the gap being the
    /// impure subtrees that re-run every time).
    pub(super) fn put(&mut self, key: ModKey, geo: Geo) {
        self.stores += 1;
        self.insert_hot(key, geo);
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
    pub(super) fn report(&self) {
        let lookups = self.hits + self.misses;
        if lookups == 0 {
            eprintln!("[csg-cache] no child-less module lookups (nothing eligible)");
            return;
        }
        let rate = 100.0 * self.hits as f64 / lookups as f64;
        eprintln!(
            "[csg-cache] lookups {lookups}  hits {}  ({rate:.1}%)  misses {}  stored {}  live-entries {}",
            self.hits,
            self.misses,
            self.stores,
            self.hot.len() + self.cold.len(),
        );
        eprintln!(
            "[csg-cache] declined stores by reason: messages {}  rand-draws {}  impure-reads {}",
            self.declined_msg, self.declined_draws, self.declined_impure,
        );
    }

    fn insert_hot(&mut self, key: ModKey, geo: Geo) {
        if self.hot.len() >= GEN_CAP {
            self.cold = std::mem::take(&mut self.hot); // rotate generations (evict the older)
        }
        self.hot.insert(key, geo);
    }
}

/// Clone a key to re-insert a promoted cold hit (an `Rc`/`Value` clone per component).
fn clone_key(k: &ModKey) -> ModKey {
    ModKey {
        body: k.body,
        home: k.home,
        params: k.params.clone(),
        dctx: k.dctx.clone(),
    }
}

/// The cache handle stored in `Ctx` — a `RefCell` because the geometry driver borrows it mutably per call.
pub(super) type CacheCell = RefCell<ModCache>;

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unit test: unwrap IS the assertion")]
mod tests {
    use super::super::{Config, Geo, Message};

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
    /// (cacheable, repeated → hits), `$`-context variation via `$`-args (distinct keys, no collision), nested
    /// + recursive modules, booleans/transforms/`for` (wrappers riding cached leaves), and the IMPURE /
    /// caller-dependent paths the fence + the `$children==0` gate must NOT serve stale (rands, echo, children).
    /// A wrong hit would make `on` diverge from `off`; `assert_eq` catches it.
    #[test]
    fn cache_on_equals_cache_off() {
        let programs = [
            // repeated child-less leaf → the redundant hit the cache exists for
            "module leaf(r){ sphere(r,$fn=8); } union(){ for(i=[0:4]) translate([i*3,0,0]) leaf(2); }",
            // same leaf, DIFFERENT reaching $fn (via $-arg) → distinct keys, must not collide to one shape
            "module leaf(r){ sphere(r); } union(){ leaf(2,$fn=8); leaf(2,$fn=16); leaf(2,$fn=8); }",
            // nested modules, the outer repeated
            "module o(){ i(); } module i(){ cube(1); } union(){ o(); translate([2,0,0]) o(); }",
            // recursion (each depth a distinct key; a sibling subtree repeats → hits)
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
        ];
        for src in programs {
            assert_eq!(
                geo(src, false),
                geo(src, true),
                "cache changed geometry:\n{src}"
            );
        }
    }

    /// The load-bearing wrong-hit: a child-RENDERING module keyed only on (args, $ctx) would serve the first
    /// call's children to the second. Two `w(){…}` calls with identical args but different children must stay
    /// distinct — the `$children==0` eligibility gate is what prevents the collision (both have $children=1 →
    /// never cached). Pinned directly (not just via the A/B loop) since it's the correctness crux.
    #[test]
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
}
