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

/// One frame of bindings plus a link to the enclosing frame (`None` at the root).
#[derive(Debug, Clone)]
struct Frame {
    vars: BTreeMap<String, Value>,
    parent: Option<Rc<Frame>>,
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
        let mut vars = BTreeMap::new();
        vars.insert("$fn".to_string(), Value::Num(0.0));
        vars.insert("$fa".to_string(), Value::Num(12.0));
        vars.insert("$fs".to_string(), Value::Num(2.0));
        vars.insert("PI".to_string(), Value::Num(std::f64::consts::PI));
        Self {
            frame: Rc::new(Frame { vars, parent: None }),
        }
    }

    /// Look up a variable, walking child→parent (inner SHADOWS outer). Unbound → `undef` (OpenSCAD
    /// warns + returns undef).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Value {
        let mut frame = &self.frame;
        loop {
            if let Some(value) = frame.vars.get(name) {
                return value.clone();
            }
            match &frame.parent {
                Some(parent) => frame = parent,
                None => return Value::Undef,
            }
        }
    }

    /// Bind (or rebind) a name in the CURRENT frame. Copy-on-write: free while this frame is unshared,
    /// clones it once if a child/closure already holds it (so their view stays at capture time).
    pub fn bind(&mut self, name: impl Into<String>, value: Value) {
        Rc::make_mut(&mut self.frame)
            .vars
            .insert(name.into(), value);
    }

    /// Push a fresh empty child frame whose parent is this chain — one `Rc` clone. The unit of scope
    /// entry for calls (I.2.3), `let`, and comprehensions (I.3).
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            frame: Rc::new(Frame {
                vars: BTreeMap::new(),
                parent: Some(Rc::clone(&self.frame)),
            }),
        }
    }

    /// The reaching special (`$`-)variables, walking child→parent (inner SHADOWS outer). Unlike regular
    /// variables (lexically scoped), `$`-vars are DYNAMICALLY scoped — a callee inherits the CALLER's
    /// `$`-context (I.2.2), so a call seeds its scope with the caller's `specials()`.
    #[must_use]
    pub fn specials(&self) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        let mut frame = &self.frame;
        loop {
            for (name, value) in &frame.vars {
                if name.starts_with('$') {
                    out.entry(name.clone()).or_insert_with(|| value.clone()); // child wins
                }
            }
            match &frame.parent {
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
