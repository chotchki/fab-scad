//! Variable + special-`$`-variable scope for the evaluator.
//!
//! `$fn`/`$fa`/`$fs` are dynamically-scoped context variables (NOT module params); OpenSCAD reads
//! them from the evaluation scope at primitive-construction time (`CurveDiscretizer`). Defaults are
//! the special-variable literals from `Builtins.cc`: `$fn=0`, `$fa=12`, `$fs=2`. Non-number `$`-vars
//! resolve to `0.0` (OpenSCAD `toDouble`).
//!
//! Backed by a `BTreeMap` for deterministic iteration order (SPEC determinism doctrine; `HashMap` is
//! banned crate-wide anyway).

use std::collections::BTreeMap;

use super::value::Value;

/// A lexical/dynamic scope: name → value bindings, seeded with the `$fn`/`$fa`/`$fs` defaults.
#[derive(Debug, Clone)]
pub struct Scope {
    vars: BTreeMap<String, Value>,
}

impl Scope {
    /// A fresh scope with OpenSCAD's `$fn=0`, `$fa=12`, `$fs=2` defaults.
    #[must_use]
    pub fn new() -> Self {
        let mut vars = BTreeMap::new();
        vars.insert("$fn".to_string(), Value::Num(0.0));
        vars.insert("$fa".to_string(), Value::Num(12.0));
        vars.insert("$fs".to_string(), Value::Num(2.0));
        Self { vars }
    }

    /// Look up a variable — an unbound name resolves to `undef` (OpenSCAD warns + returns undef).
    #[must_use]
    pub fn lookup(&self, name: &str) -> Value {
        self.vars.get(name).cloned().unwrap_or(Value::Undef)
    }

    /// Bind (or rebind) a name.
    pub fn bind(&mut self, name: impl Into<String>, value: Value) {
        self.vars.insert(name.into(), value);
    }

    /// Resolve `$fn`, `$fa`, `$fs` as `f64` (non-number → `0.0`, per OpenSCAD `toDouble`) — the
    /// inputs to the fragment formula.
    #[must_use]
    pub fn fn_fa_fs(&self) -> (f64, f64, f64) {
        (self.num("$fn"), self.num("$fa"), self.num("$fs"))
    }

    fn num(&self, name: &str) -> f64 {
        match self.vars.get(name) {
            Some(Value::Num(n)) => *n,
            _ => 0.0,
        }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}
