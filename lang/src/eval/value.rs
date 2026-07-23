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
    /// A general heterogeneous list: nested lists, mixed types (shared, immutable). Wrapped in [`ValueList`]
    /// so the deep-nesting iterative `Drop` lives on the PAYLOAD, not on `Value` — a `Drop` on `Value` itself
    /// forbids the by-value field moves the arithmetic hot path (`ops.rs`) relies on (E0509). M.1b.
    List(ValueList),
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
        /// The name this closure was DEFINED as (`g` in `g = function…` / `let(g = function…)`), if any.
        /// Re-injected into the body's scope at call time so the closure can call ITSELF by name — OpenSCAD's
        /// letrec semantics (a shared-mutable context sees its own binding; we re-inject since our frames are
        /// copy-on-write). `None` for an anonymous literal (never bound to a name).
        self_name: Option<Rc<str>>,
        /// OpenSCAD's `str()` rendering — the closure's SOURCE (`function(x) target_func(x)`), pre-computed
        /// at creation because `str` can't reach the eval `Ctx`'s AST table. See `print::function_value_repr`.
        repr: Rc<str>,
        /// The letrec GROUP: the OTHER nested `function`s defined in the SAME body scope. OpenSCAD makes all
        /// functions in a scope mutually visible — so `_gather_contiguous_edges` can call
        /// `_gather_contiguous_edges_r` defined ten lines BELOW it. Our frames are copy-on-write, so (as with
        /// `self_name`) we don't store the siblings in `env` — which would cycle — but re-inject them into the
        /// body scope at call time from this list. `None`/empty for a lone body function or a plain literal.
        /// L.5.4.
        group: Option<Rc<[SiblingFn]>>,
    },
}

/// One member of a nested-function letrec [`group`](Value::Function): the data to RECONSTRUCT its
/// [`Value::Function`] at call time. `env` is NOT stored — each sibling is rebuilt with the CALLING
/// sibling's captured `env` (the shared body scope), so mutually-recursive body functions that read their
/// params + each other resolve; a sibling that reads an enclosing local defined textually BETWEEN it and the
/// caller sees that local as `undef` (a documented v1 bound, like the L.2.8m nested-def simplifications).
#[derive(Debug, Clone)]
pub struct SiblingFn {
    /// The sibling's bound name — how a caller names it, and its own `self_name` when reconstructed.
    pub name: Rc<str>,
    /// Its slot in the `Ctx` closure table (params + body), registered ONCE when the group is built.
    pub closure_id: usize,
    /// Its pre-computed `str()` source rendering.
    pub repr: Rc<str>,
}

/// The payload of [`Value::List`] — a shared, heterogeneous `[Value]`. A newtype PURELY so its `Drop` can be
/// iterative: a general list is the ONLY value that nests other values, so a runtime-deep `[[[…]]]` (a deep
/// recursive comprehension) drops one host-stack frame per level and overflows — the same class M.1 killed for
/// the geometry trees. `Drop` lives HERE, not on `Value`: a `Drop` on `Value` forbids the by-value field moves
/// the arithmetic hot path (`ops.rs`) relies on (E0509). Derefs to `[Value]`, so every read site (`.iter()`,
/// `.len()`, indexing) is transparent — cloning stays a cheap `Rc` bump.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueList(Rc<[Value]>);

impl std::ops::Deref for ValueList {
    type Target = [Value];
    fn deref(&self) -> &[Value] {
        &self.0
    }
}

impl Drop for ValueList {
    fn drop(&mut self) {
        // Leaf fast path: an empty (or already-drained) payload needs no worklist — the common case.
        if self.0.is_empty() {
            return;
        }
        // Unlink every SOLELY-owned nested list into one flat worklist and drop iteratively (mirrors M.1's
        // geo_drop + Frame::Drop). `Rc::get_mut` succeeds only at strong-count 1 — a SHARED payload is someone
        // else's to keep, so we leave its nested lists alone and just let the `Rc` decrement. Taking each
        // nested payload BEFORE its `Value::List` drops means that re-entrant `ValueList::drop` hits the empty
        // fast path above — so nothing recurses past depth 1.
        let mut work = vec![std::mem::take(&mut self.0)];
        while let Some(mut rc) = work.pop() {
            if let Some(slice) = Rc::get_mut(&mut rc) {
                for v in slice {
                    if let Value::List(inner) = v {
                        let nested = std::mem::take(&mut inner.0);
                        if !nested.is_empty() {
                            work.push(nested);
                        }
                    }
                }
            }
            // `rc` drops here: sole-owner payloads are now shallow (nested lists drained), shared ones just
            // decrement their count. Either way, no recursive `Drop` chain down the nesting.
        }
    }
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
        Value::List(ValueList(xs.into()))
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
                // SEQUENCE equality (AH.2.1, operators-tests golden): two ranges are equal iff they
                // ITERATE the same — `[1:-1:3] == [-1:-1:1]` is true (both empty, fields be damned).
                // Computed from the arithmetic parameters, never by expansion. This also keeps the
                // L.2.8k pin: `[0:NAN:INF]` is empty, so it EQUALS ITSELF (`is_nan(r)` false) and
                // BOSL2's `typeof` still falls through `is_range` to `"invalid"`.
            ) => range_seq_cmp((*s0, *p0, *e0), (*s1, *p1, *e1)) == Some(std::cmp::Ordering::Equal),
            // Function equality is OBJECT IDENTITY (AH.2.7, the function-literal-compare golden):
            // `b = a` is equal; a SECOND evaluation of the same literal — same closure_id, fresh
            // captured env — is not ("might have captured different values, which are not checked
            // against"). Both env Rcs are held by the operands, so the pointer identity can't ABA.
            (
                Value::Function {
                    closure_id: c1,
                    env: e1,
                    ..
                },
                Value::Function {
                    closure_id: c2,
                    env: e2,
                    ..
                },
            ) => c1 == c2 && e1.same_frame(e2),
            _ => false, // cross-type is never equal
        }
    }
}

/// Compare two ranges AS THE SEQUENCES THEY ITERATE, lexicographically, in O(1) — no expansion.
/// Arithmetic sequences agree on a prefix iff they share start (and step, once both have a second
/// element), so the first difference is decided by start, then step, then length; empty sequences
/// are all equal and precede everything non-empty. `None` never actually escapes: a NaN bound
/// makes a range EMPTY (`range_len` → 0), so the `partial_cmp`s below only see comparable floats.
pub(crate) fn range_seq_cmp(
    (s1, p1, e1): (f64, f64, f64),
    (s2, p2, e2): (f64, f64, f64),
) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let n1 = range_len(s1, p1, e1);
    let n2 = range_len(s2, p2, e2);
    match (n1 == 0, n2 == 0) {
        (true, true) => return Some(Ordering::Equal),
        (true, false) => return Some(Ordering::Less),
        (false, true) => return Some(Ordering::Greater),
        (false, false) => {}
    }
    match s1.partial_cmp(&s2) {
        Some(Ordering::Equal) => {}
        other => return other, // first elements differ
    }
    if n1 >= 2 && n2 >= 2 {
        match p1.partial_cmp(&p2) {
            Some(Ordering::Equal) => {}
            other => return other, // second elements (start + step) differ
        }
    }
    Some(n1.cmp(&n2)) // equal common prefix — the shorter sequence orders first
}

/// A runaway range (`[0:1e12]`) must not hang the evaluator, so iteration is capped. OpenSCAD warns +
/// caps too (the warning TEXT is I.5); this is chosen well above any real model's range length.
pub const RANGE_MAX: u64 = 10_000_000;

/// AD.3, upstream's boundary for a range in a `for`/`each`: an element count that overflows `uint32`
/// (their `RangeType::numValues` return type) warns "too many elements" and iterates ZERO times —
/// for-tests.scad's own annotations pin the edge (`[0:1:4294967294]`, count exactly `u32::MAX`, already
/// warns). The evaluator's expansion seams check this BEFORE the [`RANGE_MAX`] cap, so a billion-element
/// range matches the oracle's warn-and-skip instead of silently grinding out 10 M capped values.
pub const RANGE_TOO_MANY: u64 = u32::MAX as u64;

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
    // An INFINITE step is legal upstream (AH.2.6, the for-tests golden): `[0:inf:0]` yields
    // exactly ONE value (the quotient below is 0 → count 1); only a 0/NaN step or non-finite
    // BOUNDS yield nothing. Direction is checked EXPLICITLY, upstream's way — a wrong-direction
    // infinite step gives a `-0.0` quotient the `n < 0` test can't see.
    if step == 0.0 || step.is_nan() || !start.is_finite() || !end.is_finite() {
        return 0;
    }
    if step < 0.0 {
        if start < end {
            return 0;
        }
    } else if start > end {
        return 0;
    }
    let n = (end - start) / step;
    // Upstream's one-ULP bump (`std::nextafter(steps, max)` in `RangeType::numValues`):
    // a fractional step whose quotient lands JUST below a whole number — `[0:1/93:1]` computes
    // 92.99999… — must still count the endpoint (the range-tests golden sweeps d=1..1000 and
    // expects zero misses).
    (n.next_up().floor() as u64).saturating_add(1)
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
        // Index 0 is `start` DIRECTLY: with an infinite step (one-value ranges, AH.2.6) the
        // index-based form would compute `0 * inf = NaN`.
        let v = if self.i == 0 {
            self.start
        } else {
            self.start + (self.i as f64) * self.step
        };
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
        // an INFINITE step yields exactly ONE value in the valid direction (AH.2.6, for-tests
        // golden `[0:inf:0]`); wrong-way infinite still yields nothing.
        assert_eq!(range_len(0.0, f64::INFINITY, 5.0), 1);
        assert_eq!(range_len(0.0, f64::NEG_INFINITY, 5.0), 0);
        assert_eq!(range_len(1.0, f64::NEG_INFINITY, 0.0), 1);
        assert_eq!(range_len(0.0, f64::NAN, 5.0), 0); // NaN step
        // the one-ULP endpoint compensation (upstream's nextafter): [0:1/93:1] counts 94.
        assert_eq!(range_len(0.0, 1.0 / 93.0, 1.0), 94);
        // a runaway range is CAPPED, not iterated to its (enormous) length.
        assert_eq!(
            u64::try_from(range_iter(0.0, 1.0, 1e12).count()).unwrap_or(0),
            RANGE_MAX
        );
    }
}
