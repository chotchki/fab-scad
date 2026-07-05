//! The scad-rs value model.
//!
//! SPEC decision: a plain enum with FAST-PATH variants (NaN-boxing rejected). `NumList` is the
//! contiguous-`f64` fast path (BOSL2 is ~90% numeric-list math); `List` is the general heterogeneous
//! list (nested lists, mixed types). Both are the SAME OpenSCAD "vector" — a list of all-numbers is
//! stored as `NumList`, anything else as `List`, and the two compare EQUAL element-for-element (the
//! `fast == slow` property, I.1.5). `Str`/`NumList`/`List` are `Rc`-shared so cloning is cheap.
//!
//! All numbers are `f64` — OpenSCAD has no integer type (`Value.cc`). Lazy ranges, functions, and
//! objects land at I.1.2/I.1.3. Conformance reference: OpenSCAD `src/core/Value.cc`.

use std::rc::Rc;

use super::scope::Scope;

/// A scad-rs runtime value.
#[derive(Debug, Clone)]
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
    /// A general heterogeneous list: nested lists, mixed types (shared, immutable).
    List(Rc<[Value]>),
    /// A LAZY range `[start : step : end]` — a first-class value (assignable, iterable), NOT
    /// materialized. Iterate with [`range_iter`]. `Value.cc`/`RangeType`.
    Range {
        /// The first value.
        start: f64,
        /// The increment (may be negative; `0` yields no values).
        step: f64,
        /// The INCLUSIVE upper (or lower, if descending) bound.
        end: f64,
    },
    /// A function-literal VALUE (closure): its params + body live in the eval `Ctx`'s closure table
    /// (indexed by `closure_id`, `&'prog` AST refs — so `Value` stays `'static`), and `env` is the
    /// scope captured at definition (an `Rc<Frame>` clone). Calling it reuses the I.2.3.2 machinery
    /// with `base = env`. `FunctionType` in OpenSCAD.
    Function {
        /// Index into the `Ctx` closure table (this eval's lifetime).
        closure_id: usize,
        /// The lexical environment captured when the literal was evaluated.
        env: Scope,
    },
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

    /// A general list value from anything convertible to a shared `[Value]`.
    #[must_use]
    pub fn list(xs: impl Into<Rc<[Value]>>) -> Self {
        Value::List(xs.into())
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
            Value::List(xs) => !xs.is_empty(),
            Value::Range { .. } | Value::Function { .. } => true, // ranges + functions are truthy
        }
    }

    /// A human-facing type name for diagnostics. A list is a "list" whichever representation it uses.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Undef => "undef",
            Value::Bool(_) => "bool",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::NumList(_) | Value::List(_) => "list",
            Value::Range { .. } => "range",
            Value::Function { .. } => "function",
        }
    }
}

/// OpenSCAD `==` (`Value.cc`): NEVER coerces across types (`1 == true` is `false`), same variants
/// compare fieldwise, `undef == undef` is `true`, and — crucially — the two list representations are
/// the SAME vector, so a `NumList` equals a `List` of the matching numbers (element-for-element).
/// `NaN != NaN` (IEEE), so `[NaN] != [NaN]`.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        use Value::{Bool, List, Num, NumList, Str, Undef};
        match (self, other) {
            (Undef, Undef) => true,
            (Bool(a), Bool(b)) => a == b,
            (Num(a), Num(b)) => a == b,
            (Str(a), Str(b)) => a == b,
            (NumList(a), NumList(b)) => a == b,
            (List(a), List(b)) => a == b, // recurses through Value::eq
            // the fast path and the slow path are the same vector.
            (NumList(n), List(l)) | (List(l), NumList(n)) => {
                n.len() == l.len()
                    && n.iter()
                        .zip(l.iter())
                        .all(|(x, v)| matches!(v, Num(y) if x == y))
            }
            (
                Value::Range {
                    start: s0,
                    step: p0,
                    end: e0,
                },
                Value::Range {
                    start: s1,
                    step: p1,
                    end: e1,
                },
            ) => s0 == s1 && p0 == p1 && e0 == e1,
            _ => false, // cross-type is never equal
        }
    }
}

/// A runaway range (`[0:1e12]`) must not hang the evaluator, so iteration is capped. OpenSCAD warns +
/// caps too (the warning TEXT is I.5); this is chosen well above any real model's range length.
pub const RANGE_MAX: u64 = 10_000_000;

/// The values of a range `[start : step : end]`, INDEX-BASED (`start + i*step`) to match OpenSCAD's
/// `RangeType` and avoid float-accumulation drift. Ascending (`step > 0`) runs while `<= end`,
/// descending (`step < 0`) while `>= end`; a `0`/non-finite step or wrong direction yields nothing.
/// Capped at [`RANGE_MAX`].
#[must_use]
pub fn range_iter(start: f64, step: f64, end: f64) -> RangeIter {
    RangeIter {
        start,
        step,
        i: 0,
        len: range_len(start, step, end).min(RANGE_MAX),
    }
}

/// The number of values a range yields (`RangeType::numValues`): `floor((end-start)/step) + 1`, or `0`
/// when the step is `0`, either bound is non-finite, or the step points the wrong way.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "n is checked finite/NaN-free and >= 0; `as u64` saturates a huge n (then RANGE_MAX caps), no UB"
)]
pub fn range_len(start: f64, step: f64, end: f64) -> u64 {
    if step == 0.0 || !start.is_finite() || !step.is_finite() || !end.is_finite() {
        return 0;
    }
    let n = (end - start) / step;
    if n.is_nan() || n < 0.0 {
        return 0;
    }
    (n.floor() as u64).saturating_add(1)
}

/// Index-based iterator over a range's values (see [`range_iter`]).
#[derive(Debug, Clone)]
pub struct RangeIter {
    start: f64,
    step: f64,
    i: u64,
    len: u64,
}

impl Iterator for RangeIter {
    type Item = f64;

    #[allow(
        clippy::cast_precision_loss,
        reason = "i < len <= RANGE_MAX (1e7) is exact in f64; the index-based value avoids step drift"
    )]
    fn next(&mut self) -> Option<f64> {
        if self.i >= self.len {
            return None;
        }
        let v = self.start + (self.i as f64) * self.step;
        self.i += 1;
        Some(v)
    }
}

// I.7 — Kani proofs of range-iteration TERMINATION. Model-checked over SYMBOLIC f64 bounds (nan/inf/
// zero-step/wrong-direction all included), so the guarantee holds for every input, not just the tested
// ones. Compiled only under `cargo kani` (cfg(kani)); a normal build never sees them.
#[cfg(kani)]
mod proofs {
    use super::{RANGE_MAX, range_iter};

    /// The iterator's length is CAPPED at `RANGE_MAX` for any bounds — so `next()` is called at most
    /// `RANGE_MAX` times. This is the "runaway range can't hang the evaluator" guarantee.
    #[kani::proof]
    fn range_len_is_capped() {
        let it = range_iter(kani::any(), kani::any(), kani::any());
        assert!(it.len <= RANGE_MAX);
    }

    /// From ANY not-yet-exhausted index, `next()` yields a value AND advances the index by exactly 1 —
    /// strict progress toward the (bounded) length. Bounded length + monotone progress ⇒ termination.
    #[kani::proof]
    fn range_next_makes_progress() {
        let mut it = range_iter(kani::any(), kani::any(), kani::any());
        it.i = kani::any(); // an arbitrary progress point, not just the start
        kani::assume(it.i < it.len);
        let before = it.i;
        assert!(it.next().is_some());
        assert!(it.i == before + 1);
    }

    /// Once exhausted (`i >= len`), `next()` HALTS — `None`, no further advance.
    #[kani::proof]
    fn range_next_halts_when_exhausted() {
        let mut it = range_iter(kani::any(), kani::any(), kani::any());
        it.i = kani::any();
        kani::assume(it.i >= it.len);
        assert!(it.next().is_none());
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "range values are exact deterministic literals (integer steps)"
)]
mod tests {
    use super::{RANGE_MAX, range_iter, range_len};

    #[test]
    fn range_iteration_matches_openscad() {
        let vals = |s, st, e| range_iter(s, st, e).collect::<Vec<f64>>();
        assert_eq!(vals(0.0, 1.0, 5.0), [0., 1., 2., 3., 4., 5.]); // inclusive end
        assert_eq!(vals(0.0, 2.0, 10.0), [0., 2., 4., 6., 8., 10.]);
        assert_eq!(vals(0.0, 3.0, 10.0), [0., 3., 6., 9.]); // step overshoots end → stops before
        assert_eq!(vals(5.0, -1.0, 0.0), [5., 4., 3., 2., 1., 0.]); // descending
        assert!(range_iter(5.0, 1.0, 0.0).next().is_none()); // ascending step, start > end → empty
        assert!(range_iter(0.0, -1.0, 5.0).next().is_none()); // descending step, start < end → empty
        assert!(range_iter(0.0, 0.0, 5.0).next().is_none()); // zero step → empty, no infinite loop
        assert!(range_iter(f64::NAN, 1.0, 5.0).next().is_none()); // non-finite bound → empty
    }

    #[test]
    fn range_len_and_the_cap() {
        assert_eq!(range_len(0.0, 1.0, 5.0), 6);
        assert_eq!(range_len(0.0, 1.0, -1.0), 0); // wrong direction
        assert_eq!(range_len(0.0, 0.0, 5.0), 0); // zero step
        assert_eq!(range_len(0.0, f64::INFINITY, 5.0), 0); // non-finite step
        // a runaway range is CAPPED, not iterated to its (enormous) length.
        assert_eq!(
            u64::try_from(range_iter(0.0, 1.0, 1e12).count()).unwrap_or(0),
            RANGE_MAX
        );
    }
}
