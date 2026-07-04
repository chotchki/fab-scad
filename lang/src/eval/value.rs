//! The scad-rs value model (v0 subset).
//!
//! SPEC decision: a plain enum with FAST-PATH variants (NaN-boxing rejected). v0 is the
//! tracer-bullet subset — `Undef`/`Bool`/`Num`/`Str`/`NumList`. The general heterogeneous list,
//! lazy ranges, functions, and objects land at I.1/I.4. `NumList` is the contiguous-`f64` fast path
//! (BOSL2 is ~90% numeric-list math); `Str`/`NumList` are `Rc`-shared so cloning a `Value` is cheap.
//! (Finer `make_mut` copy-on-write for the list-BUILD path is an I.1 profile-driven decision.)
//!
//! All numbers are `f64` — OpenSCAD has no integer type (`Value.cc`). Conformance reference for the
//! semantics here: OpenSCAD `src/core/Value.cc`.

use std::rc::Rc;

/// A scad-rs runtime value (v0 subset).
///
/// Derived `PartialEq` matches OpenSCAD's `==` for this subset by construction: different variants
/// compare unequal (OpenSCAD never coerces across types — `1 == true` is `false`), same variants
/// compare fieldwise, and `undef == undef` is `true`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `undef` — the absence of a value; propagates through nearly every operation.
    Undef,
    /// A boolean.
    Bool(bool),
    /// A number (all OpenSCAD numbers are `f64`).
    Num(f64),
    /// A string (shared, immutable).
    Str(Rc<str>),
    /// A contiguous numeric list — the fast path for vector math (shared, immutable).
    NumList(Rc<[f64]>),
}

impl Value {
    /// A string value from anything convertible to a shared `str`.
    #[must_use]
    pub fn string(s: impl Into<Rc<str>>) -> Self {
        Value::Str(s.into())
    }

    /// A numeric-list value from anything convertible to a shared `[f64]`.
    #[must_use]
    pub fn num_list(xs: impl Into<Rc<[f64]>>) -> Self {
        Value::NumList(xs.into())
    }

    /// The OpenSCAD "truthiness" of this value (`Value.cc:294-309`): `undef`, `false`, `0`/`-0`, `""`,
    /// and `[]` are falsy; everything else — including `NaN` (since `NaN != 0`) — is truthy.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Undef => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::NumList(xs) => !xs.is_empty(),
        }
    }

    /// A human-facing type name for diagnostics.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Undef => "undef",
            Value::Bool(_) => "bool",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::NumList(_) => "list",
        }
    }
}
