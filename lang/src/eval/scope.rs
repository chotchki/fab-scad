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
//! Frames are backed by `BTreeMap` for deterministic iteration order (SPEC determinism doctrine;
//! `HashMap` is banned crate-wide anyway).

use std::collections::BTreeMap;
use std::rc::Rc;

use super::value::Value;

/// One frame of bindings plus a link to the enclosing frame (`None` at the root). `$`-specials live in
/// their OWN map, apart from the (often huge) lexical `vars`: every user call inherits the caller's reaching
/// `$`-context via [`Scope::specials`], which must iterate ONLY the `$`-vars — a BOSL2 island global holds
/// THOUSANDS of constants in `vars`, so folding `$`-vars in there made `specials()` (and thus every call)
/// O(scope-size), the L.2.7 timeout tax. Kept split, each bound/looked-up by its `$` prefix.
#[derive(Debug, Clone)]
struct Frame {
    vars: BTreeMap<String, Value>,
    specials: BTreeMap<String, Value>,
    /// LEXICAL parent — walked for regular (`vars`) lookups. A `let`/comprehension child shares its
    /// enclosing scope here; a CALL frame instead points at the callee's home global (hygiene).
    parent: Option<Rc<Frame>>,
    /// DYNAMIC parent — walked for `$`-special lookups. For a `let`/comprehension it's the SAME as `parent`
    /// (both inherit the enclosing scope). For a CALL frame it's the CALLER's frame, so the callee inherits
    /// the caller's reaching `$`-context BY REFERENCE — no per-call copy of the (BOSL2: 42-strong) `$`-set,
    /// which is the L.2.7 timeout fix (`specials()`-copy was O(#`$`-vars) on every call). See [`call_frame`].
    dynamic_parent: Option<Rc<Frame>>,
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
        specials.insert("$fn".to_string(), Value::Num(0.0));
        specials.insert("$fa".to_string(), Value::Num(12.0));
        specials.insert("$fs".to_string(), Value::Num(2.0));
        let mut vars = BTreeMap::new();
        vars.insert("PI".to_string(), Value::Num(std::f64::consts::PI));
        Self {
            frame: Rc::new(Frame {
                vars,
                specials,
                parent: None,
                dynamic_parent: None,
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
                vars: BTreeMap::new(),
                specials: BTreeMap::new(),
                parent: Some(Rc::clone(&lexical_base.frame)),
                dynamic_parent: Some(Rc::clone(&caller.frame)),
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
            loop {
                if let Some(value) = frame.specials.get(name) {
                    return Some(value.clone());
                }
                match &frame.dynamic_parent {
                    Some(parent) => frame = parent,
                    None => return None,
                }
            }
        } else {
            loop {
                if let Some(value) = frame.vars.get(name) {
                    return Some(value.clone());
                }
                match &frame.parent {
                    Some(parent) => frame = parent,
                    None => return None,
                }
            }
        }
    }

    /// Bind (or rebind) a name in the CURRENT frame — a `$`-name into `specials`, else `vars`. Copy-on-write:
    /// free while this frame is unshared, clones it once if a child/closure already holds it (so their view
    /// stays at capture time).
    pub fn bind(&mut self, name: impl Into<String>, value: Value) {
        let name = name.into();
        let frame = Rc::make_mut(&mut self.frame);
        if name.starts_with('$') {
            frame.specials.insert(name, value);
        } else {
            frame.vars.insert(name, value);
        }
    }

    /// Push a fresh empty child frame whose parent is this chain — one `Rc` clone. The unit of scope
    /// entry for calls (I.2.3), `let`, and comprehensions (I.3).
    #[must_use]
    pub fn child(&self) -> Self {
        // A `let`/comprehension child inherits the enclosing scope both lexically AND dynamically (same
        // frame for both) — only a `call_frame` splits them.
        Self {
            frame: Rc::new(Frame {
                vars: BTreeMap::new(),
                specials: BTreeMap::new(),
                parent: Some(Rc::clone(&self.frame)),
                dynamic_parent: Some(Rc::clone(&self.frame)),
            }),
        }
    }

    /// The reaching special (`$`-)variables, walking child→parent (inner SHADOWS outer). Unlike regular
    /// variables (lexically scoped), `$`-vars are DYNAMICALLY scoped — a callee inherits the CALLER's
    /// `$`-context (I.2.2), so a call seeds its scope with the caller's `specials()`. Walks only the
    /// `specials` maps (a handful of `$`-vars), NOT the constant-laden `vars` — that split is what keeps
    /// this O(#`$`-vars) instead of O(scope-size) on the every-call hot path (the L.2.7 fix).
    #[must_use]
    pub fn specials(&self) -> BTreeMap<String, Value> {
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
