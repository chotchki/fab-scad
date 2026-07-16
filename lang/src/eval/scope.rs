//! The evaluator's lexical environment — a persistent `Rc<Frame>` chain (I.2.1 decision).
//!
//! A [`Scope`] is the current (mutable) frame plus a chain of enclosing frames shared by `Rc`. Lookup
//! walks child→parent (inner bindings SHADOW outer). Two moves make this cheap AND correct:
//!   - **`child()` is one `Rc` clone** — a call/`let` pushes a fresh empty frame whose parent is the
//!     current chain. This is how closures will capture their definition env (I.2.3): one `Rc` clone.
//!   - **`bind` is copy-on-write** (`Rc::make_mut`) — free while the frame is unshared, and once a
//!     closure has captured the frame a later bind COWs it, so the closure keeps seeing its
//!     capture-time env. That's exactly OpenSCAD's lexical-capture semantics, for free.
//!
//! `$fn`/`$fa`/`$fs` are dynamically-scoped context variables (NOT module params); OpenSCAD reads them
//! from the evaluation scope at primitive-construction time (`CurveDiscretizer`). Defaults are the
//! special-variable literals from `Builtins.cc`: `$fn=0`, `$fa=12`, `$fs=2`. Non-number `$`-vars
//! resolve to `0.0` (OpenSCAD `toDouble`). They live at the ROOT frame and resolve through the chain.
//!
//! The `$`-specials map is a `BTreeMap` (deterministic iteration for [`Scope::specials`]; `HashMap` is banned
//! crate-wide anyway). The regular `vars` are an adaptive [`VarMap`] — a flat `Vec` while small (the per-call
//! frame), spilling to a `BTreeMap` for the thousand-constant island globals (N.2d); `vars` is never iterated,
//! so it owes no ordering.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::value::Value;

/// A frame's regular (non-`$`) bindings. ADAPTIVE (N.2d): a flat `Vec` while small — the common per-call /
/// `let` / comprehension frame holds a HANDFUL of bindings, and there a linear scan beats a `BTreeMap`'s
/// node-per-insert allocation + tree-walk drop + tree-rebuild clone (the ~15% of allocation the `slice_parts`
/// profile pinned to the per-frame map). SPILLS to a `BTreeMap` past [`SPILL`] entries, because a BOSL2
/// island GLOBAL holds THOUSANDS of constants and a linear scan THERE would be catastrophic — the exact
/// split this module's header warns about. `vars` is ONLY ever get/insert (never iterated — grep-confirmed),
/// so order is irrelevant: the `Vec` needs no sorting, and inserts scan-and-replace to keep keys unique.
#[derive(Debug, Clone)]
enum VarMap {
    /// Few bindings — linear scan. The per-call / `let` / comprehension frame.
    Small(Vec<(Rc<str>, Value)>),
    /// Many bindings — a `BTreeMap`. An island global (BOSL2: thousands of constants).
    Large(BTreeMap<Rc<str>, Value>),
}

/// Above this many bindings a [`VarMap`] spills `Small`→`Large`. ~16: below it a linear scan of short string
/// keys wins on the per-call frame; an island global blows past it once and stays a `BTreeMap` for O(log n).
const SPILL: usize = 16;

impl VarMap {
    fn new() -> Self {
        VarMap::Small(Vec::new())
    }

    fn get(&self, name: &str) -> Option<&Value> {
        match self {
            VarMap::Small(v) => v
                .iter()
                .find(|(k, _)| k.as_ref() == name)
                .map(|(_, val)| val),
            VarMap::Large(m) => m.get(name),
        }
    }

    /// Bind or REBIND `name` (last write wins — load-bearing for OpenSCAD's two-phase param binding, where
    /// phase-2 args overwrite phase-1 defaults). Scan-and-replace keeps `Small` keys unique; the `Small`→
    /// `Large` spill fires only when a genuinely NEW key would push past [`SPILL`]. `name` arrives as an
    /// `Rc<str>` so the hot per-call / per-iteration bind is a refcount BUMP, not a fresh `String` alloc
    /// (N.2b): the AST holds each identifier's `Rc<str>` once, and bind clones it.
    fn insert(&mut self, name: Rc<str>, value: Value) {
        match self {
            VarMap::Small(v) => {
                if let Some(slot) = v.iter_mut().find(|(k, _)| **k == *name) {
                    slot.1 = value;
                } else if v.len() >= SPILL {
                    let mut map: BTreeMap<Rc<str>, Value> = std::mem::take(v).into_iter().collect();
                    map.insert(name, value);
                    *self = VarMap::Large(map);
                } else {
                    v.push((name, value));
                }
            }
            VarMap::Large(m) => {
                m.insert(name, value);
            }
        }
    }
}

thread_local! {
    /// The per-thread monotonic boundary-id mint (see `Frame::boundary`). Never reused within a thread;
    /// captures are thread-local too, so cross-thread collisions can't be observed.
    static NEXT_BOUNDARY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn mint_boundary() -> u64 {
    NEXT_BOUNDARY.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

/// One frame of bindings plus a link to the enclosing frame (`None` at the root). `$`-specials live in
/// their OWN map, apart from the (often huge) lexical `vars`: every user call inherits the caller's reaching
/// `$`-context via [`Scope::specials`], which must iterate ONLY the `$`-vars — a BOSL2 island global holds
/// THOUSANDS of constants in `vars`, so folding `$`-vars in there made `specials()` (and thus every call)
/// O(scope-size), the L.2.7 timeout tax. Kept split, each bound/looked-up by its `$` prefix.
#[derive(Debug, Clone)]
struct Frame {
    vars: VarMap,
    specials: BTreeMap<Rc<str>, Value>,
    /// LEXICAL parent — walked for regular (`vars`) lookups. A `let`/comprehension child shares its
    /// enclosing scope here; a CALL frame instead points at the callee's home global (hygiene).
    parent: Option<Rc<Frame>>,
    /// DYNAMIC parent — walked for `$`-special lookups. For a `let`/comprehension it's the SAME as `parent`
    /// (both inherit the enclosing scope). For a CALL frame it's the CALLER's frame, so the callee inherits
    /// the caller's reaching `$`-context BY REFERENCE — no per-call copy of the (BOSL2: 42-strong) `$`-set,
    /// which is the L.2.7 timeout fix (`specials()`-copy was O(#`$`-vars) on every call). See [`call_frame`].
    dynamic_parent: Option<Rc<Frame>>,
    /// An O(1) IDENTITY for this frame's reaching `$`-context — the eval-memo cache key's `$`-context
    /// component (N.2c). Inherited by Rc-clone when a frame adds no `$`-binding (the common case: a call
    /// inherits its caller's context), replaced by a fresh node on any `$`-bind. Its POINTER is the identity;
    /// see [`DynCtxNode`]. This is what lets the cache key a call's `$`-context WITHOUT the O(depth)
    /// `specials()` walk B2 flagged — read it in O(1), share it across all calls under the same context.
    dyn_ctx: Rc<DynCtxNode>,
    /// COW-SURVIVING identity for the CSG memo's read-capture boundary (BU.8 review findings 1+2): minted
    /// once per LOGICAL frame at the explicit constructors ([`Scope::new`]/[`child`](Scope::child)/
    /// [`call_frame`](Scope::call_frame)) and PRESERVED by `Rc::make_mut`'s derived-`Clone` copy — so a
    /// shared frame that COWs on [`bind`](Scope::bind) keeps its boundary id and the capture walk still
    /// crosses it (pointer identity does NOT survive the COW; keying on it under-recorded read sets →
    /// wrong hits). Monotonic per thread, never reused (no ABA on freed frames). Distinct from
    /// [`frame_id`](Scope::frame_id) (pointer identity — the eval-cache's sharing-sensitive contract).
    boundary: u64,
}

/// An opaque IDENTITY for a reaching `$`-context (N.2c cache key). The Rc POINTER is the whole meaning: two
/// frames holding the same `Rc<DynCtxNode>` share an identical reaching `$`-context (one was Rc-cloned from
/// the other on inherit); a `$`-bind mints a fresh node, so a changed context is a distinct pointer. Pointer
/// identity is EXACT and collision-free, and the cache HOLDS the `Rc` in its key so the address can't be
/// recycled under a stale entry (the ABA hazard). Deliberately holds NO parent link: a deep `$`-override
/// recursion (`module r(n){ $fn=n; r(n-1); }`) would otherwise build a chain that drops recursively and
/// overflows — the M.1/L.2.7 class. `depth` keeps it non-ZST (so `Rc::new` yields distinct addresses) and is
/// a debug aid only; two DISTINCT contexts that happen to share a depth are still distinct allocations.
#[derive(Debug)]
pub(super) struct DynCtxNode {
    #[allow(
        dead_code,
        reason = "non-ZST guarantee + debug aid; identity is the Rc pointer, not this field"
    )]
    depth: u32,
}

/// ITERATIVE drop for the frame chain. Deep recursion (`f(n)=f(n-1)`) builds an N-deep `dynamic_parent`
/// chain (each call frame references its caller); the default recursive `Drop` would overflow the HOST
/// stack unwinding it — exactly the "recursion is heap-bounded" property the explicit-stack evaluator
/// exists to guarantee. So unlink the chain into a worklist and drop iteratively. A frame we don't
/// uniquely own is shared (another live scope holds it) — decrement and stop, don't descend.
impl Drop for Frame {
    fn drop(&mut self) {
        let mut worklist: Vec<Rc<Frame>> = Vec::new();
        worklist.extend(self.parent.take());
        worklist.extend(self.dynamic_parent.take());
        while let Some(rc) = worklist.pop() {
            if let Ok(mut frame) = Rc::try_unwrap(rc) {
                worklist.extend(frame.parent.take());
                worklist.extend(frame.dynamic_parent.take());
                // `frame` drops here with both links already None → no recursion.
            }
        }
    }
}

/// A lexical/dynamic scope: an `Rc<Frame>` chain seeded with the `$fn`/`$fa`/`$fs` defaults at the root.
#[derive(Debug, Clone)]
pub struct Scope {
    frame: Rc<Frame>,
}

impl Scope {
    /// A fresh root scope with OpenSCAD's `$fn=0`, `$fa=12`, `$fs=2` fragment defaults plus the builtin
    /// `PI` constant (`Builtins.cc`). `PI` is OpenSCAD's ONE named math constant — a plain shadowable
    /// variable, not a keyword — so it lives at the root like the `$`-vars; BOSL2 leans on `2*PI` heavily
    /// (`segs`, every arc/circle), and without it those go `undef` and cascade.
    #[must_use]
    pub fn new() -> Self {
        let mut specials = BTreeMap::new();
        specials.insert(Rc::from("$fn"), Value::Num(0.0));
        specials.insert(Rc::from("$fa"), Value::Num(12.0));
        specials.insert(Rc::from("$fs"), Value::Num(2.0));
        let mut vars = VarMap::new();
        vars.insert(Rc::from("PI"), Value::Num(std::f64::consts::PI));
        Self {
            frame: Rc::new(Frame {
                vars,
                specials,
                parent: None,
                dynamic_parent: None,
                // The root $-context: the `$fn`/`$fa`/`$fs` defaults above. Every scope descends from a root,
                // so all default-context calls share THIS node (depth 0) → the cache keys them together.
                dyn_ctx: Rc::new(DynCtxNode { depth: 0 }),
                boundary: mint_boundary(),
            }),
        }
    }

    /// A fresh CALL frame: lexically a child of `lexical_base` (the callee's home global — hygiene, so it
    /// sees its own file's constants, not the caller's locals), but DYNAMICALLY a child of `caller` (so it
    /// inherits the caller's reaching `$`-context by reference). This is what replaces the old
    /// copy-every-`$`-var-into-the-call-scope, making a call O(1) in the `$`-set instead of O(#`$`-vars).
    /// The caller then binds the call's params into `vars` and any `$`-args into `specials` (shadowing).
    #[must_use]
    pub fn call_frame(lexical_base: &Scope, caller: &Scope) -> Self {
        Self {
            frame: Rc::new(Frame {
                vars: VarMap::new(),
                specials: BTreeMap::new(),
                parent: Some(Rc::clone(&lexical_base.frame)),
                dynamic_parent: Some(Rc::clone(&caller.frame)),
                // Inherit the CALLER's $-context identity (the callee reads the caller's reaching $-vars):
                // an O(1) Rc-clone, and every call sharing the caller's context shares this node. A `$`-arg
                // bound into this frame below then mints a fresh node via `bind`.
                dyn_ctx: Rc::clone(&caller.frame.dyn_ctx),
                boundary: mint_boundary(),
            }),
        }
    }

    /// The identity of this scope's current frame — the `Rc<Frame>` pointer as an integer. Two scopes that
    /// SHARE a frame (a clone, or a named function's re-fetched home global) compare equal; a closure's
    /// distinct captured env compares distinct. Used as the captured-env component of the eval-memo cache key
    /// (N.2c) — a closure's result depends on its capture, which is neither an arg nor a `$`-var, so the key
    /// must carry this or two closures sharing a body would collide. Pointer identity is SAFE (never a false
    /// match; at worst a missed share of two structurally-equal-but-distinct frames).
    #[must_use]
    pub(super) fn frame_id(&self) -> usize {
        Rc::as_ptr(&self.frame) as usize
    }

    /// The COW-surviving boundary id of this scope's frame (see `Frame::boundary`) — the CSG memo's
    /// read-capture token.
    #[must_use]
    pub(super) fn boundary_id(&self) -> u64 {
        self.frame.boundary
    }

    /// Look up a variable, walking child→parent (inner SHADOWS outer). Unbound → `undef`.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Value {
        self.lookup_opt(name).unwrap_or(Value::Undef)
    }

    /// Like [`lookup`](Self::lookup) but distinguishes UNBOUND (`None`) from bound-to-`undef`
    /// (`Some(Undef)`) — the difference OpenSCAD keys its "Ignoring unknown variable" warning on: a
    /// genuinely-unknown name warns, an explicit `x = undef;` (or an unfilled defaultless param) stays
    /// silent. Same child→parent walk as `lookup`.
    #[must_use]
    pub fn lookup_opt(&self, name: &str) -> Option<Value> {
        // `$`-names live in `specials` and follow the DYNAMIC chain; everything else is in `vars` on the
        // LEXICAL chain. Route by prefix, then walk that chain child→parent (inner shadows outer).
        let mut frame = &self.frame;
        if name.starts_with('$') {
            // Rung 2b (BU.8): this walk is the ONE choke point every `$`-read flows through, so the CSG
            // memo's read capture rides it — `visit` each frame BEFORE its map check (a resolution AT a
            // capture's entry frame is at-or-above the boundary), `record` the outcome (UNBOUND included).
            // Near-free when no capture is open ([`mod_cache::ReadWalk::begin`] short-circuits).
            let mut walk = super::mod_cache::ReadWalk::begin();
            loop {
                walk.visit(frame.boundary);
                if let Some(value) = frame.specials.get(name) {
                    walk.record(name, Some(value));
                    return Some(value.clone());
                }
                let Some(parent) = frame.dynamic_parent.as_ref() else {
                    walk.record(name, None);
                    return None;
                };
                frame = parent;
            }
        } else {
            loop {
                if let Some(value) = frame.vars.get(name) {
                    return Some(value.clone());
                }
                frame = frame.parent.as_ref()?;
            }
        }
    }

    /// Bind (or rebind) a name in the CURRENT frame — a `$`-name into `specials`, else `vars`. Copy-on-write:
    /// free while this frame is unshared, clones it once if a child/closure already holds it (so their view
    /// stays at capture time).
    pub fn bind(&mut self, name: impl Into<Rc<str>>, value: Value) {
        let name = name.into();
        let frame = Rc::make_mut(&mut self.frame);
        if name.starts_with('$') {
            frame.specials.insert(name, value);
            // The reaching $-context just CHANGED → mint a fresh identity so the cache doesn't confuse this
            // scope with its pre-bind self (or a sibling that never bound). A new allocation = a new pointer.
            frame.dyn_ctx = Rc::new(DynCtxNode {
                depth: frame.dyn_ctx.depth + 1,
            });
        } else {
            frame.vars.insert(name, value);
        }
    }

    /// This scope's reaching-`$`-context identity (N.2c cache key). Cheap: an `Rc` clone. Two scopes returning
    /// `Rc`s that [`Rc::ptr_eq`] have an identical reaching `$`-context; the cache holds this `Rc` so the
    /// pointer stays valid (no ABA). See [`DynCtxNode`].
    #[must_use]
    pub(super) fn dyn_ctx(&self) -> Rc<DynCtxNode> {
        Rc::clone(&self.frame.dyn_ctx)
    }

    /// Push a fresh empty child frame whose parent is this chain — one `Rc` clone. The unit of scope
    /// entry for calls (I.2.3), `let`, and comprehensions (I.3).
    #[must_use]
    pub fn child(&self) -> Self {
        // A `let`/comprehension child inherits the enclosing scope both lexically AND dynamically (same
        // frame for both) — only a `call_frame` splits them.
        Self {
            frame: Rc::new(Frame {
                vars: VarMap::new(),
                specials: BTreeMap::new(),
                parent: Some(Rc::clone(&self.frame)),
                dynamic_parent: Some(Rc::clone(&self.frame)),
                // A `let`/comprehension child adds no `$`-binding of its own → inherit this scope's context.
                dyn_ctx: Rc::clone(&self.frame.dyn_ctx),
                boundary: mint_boundary(),
            }),
        }
    }

    /// The reaching special (`$`-)variables, walking child→parent (inner SHADOWS outer). Unlike regular
    /// variables (lexically scoped), `$`-vars are DYNAMICALLY scoped — a callee inherits the CALLER's
    /// `$`-context (I.2.2), so a call seeds its scope with the caller's `specials()`. Walks only the
    /// `specials` maps (a handful of `$`-vars), NOT the constant-laden `vars` — that split is what keeps
    /// this O(#`$`-vars) instead of O(scope-size) on the every-call hot path (the L.2.7 fix).
    #[must_use]
    pub fn specials(&self) -> BTreeMap<Rc<str>, Value> {
        let mut out = BTreeMap::new();
        let mut frame = &self.frame;
        loop {
            for (name, value) in &frame.specials {
                out.entry(name.clone()).or_insert_with(|| value.clone()); // child wins
            }
            match &frame.dynamic_parent {
                Some(parent) => frame = parent,
                None => return out,
            }
        }
    }

    /// Resolve `$fn`, `$fa`, `$fs` as `f64` (non-number → `0.0`, per OpenSCAD `toDouble`) — the inputs
    /// to the fragment formula. Resolves through the chain, so a child scope sees the root defaults.
    #[must_use]
    pub fn fn_fa_fs(&self) -> (f64, f64, f64) {
        (self.num("$fn"), self.num("$fa"), self.num("$fs"))
    }

    fn num(&self, name: &str) -> f64 {
        match self.lookup(name) {
            Value::Num(n) => n,
            _ => 0.0,
        }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the $-var values are exact deterministic literals (0/12/2/64)"
)]
mod tests {
    use super::{Scope, Value};

    #[test]
    fn child_shadows_parent_without_mutating_it() {
        let mut root = Scope::new();
        root.bind("x", Value::Num(1.0));

        let mut child = root.child();
        child.bind("x", Value::Num(2.0)); // shadows

        assert_eq!(child.lookup("x"), Value::Num(2.0)); // child sees its own
        assert_eq!(root.lookup("x"), Value::Num(1.0)); // parent unchanged
    }

    #[test]
    fn child_falls_through_to_parent() {
        let mut root = Scope::new();
        root.bind("outer", Value::Num(7.0));
        let child = root.child().child(); // two frames deep, both empty

        assert_eq!(child.lookup("outer"), Value::Num(7.0)); // found up the chain
        assert_eq!(child.lookup("missing"), Value::Undef); // off the top → undef
    }

    #[test]
    fn special_vars_resolve_through_the_chain() {
        let child = Scope::new().child().child();
        assert_eq!(child.fn_fa_fs(), (0.0, 12.0, 2.0)); // root defaults, seen from a deep child

        let mut root = Scope::new();
        root.bind("$fn", Value::Num(64.0));
        assert_eq!(root.child().fn_fa_fs().0, 64.0); // a per-scope $fn flows down
        assert_eq!(root.child().fn_fa_fs(), (64.0, 12.0, 2.0));
    }

    #[test]
    fn binding_a_clone_leaves_the_original_alone() {
        // Rc COW: `eval_program` clones the caller's scope then binds — the caller must not see it.
        let root = Scope::new();
        let mut clone = root.clone();
        clone.bind("y", Value::Num(9.0));

        assert_eq!(clone.lookup("y"), Value::Num(9.0));
        assert_eq!(root.lookup("y"), Value::Undef);
    }

    #[test]
    fn non_number_special_var_is_zero() {
        let mut root = Scope::new();
        root.bind("$fn", Value::string("nonsense"));
        assert_eq!(root.fn_fa_fs().0, 0.0); // toDouble(non-number) → 0
    }
}
