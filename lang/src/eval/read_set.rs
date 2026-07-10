//! J.5.2b — dynamic `$`-read recording for the read-set-precise CSG cache. While a module body evaluates (a
//! recorder is pushed by the geometry driver), every `$`-var LOOKUP is captured (name → the value it resolved
//! to) into the top recorder. The CSG cache then keys/validates a module call on ONLY the `$`-vars its body
//! actually read — not the whole reaching `$`-context (2a), which BOSL2 over-specifies with per-copy `$idx`
//! and 8 KB attachment vars a leaf never reads (the nail_polish 81-identical-cups case: 88.5% redundant at
//! (module,params) but 5.8% with the full context).
//!
//! Correctness by construction: a cache HIT re-checks the current context against the entry's recorded reads,
//! so a hit means the call agrees on every `$`-var the cached geometry depended on. Path-dependent reads just
//! record a different set → a different entry. Reads PROPAGATE up the stack on [`pop`]: a nested body's reads
//! are the enclosing body's dependencies too (whether the nested call was a cache hit or a fresh eval).
//!
//! First-read-wins per name (a stable `$`-value within one body eval; if the body locally rebinds and re-reads,
//! the FIRST — usually inherited — read is what the caller-facing key should depend on). Near-free when off: a
//! `Cell<u32>` depth gate short-circuits before any `RefCell` borrow, so a `$`-lookup pays nothing unless the
//! CSG cache pushed a recorder.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use super::value::Value;

thread_local! {
    /// Recorder nesting depth — the cheap gate. `0` ⇒ no recorder ⇒ [`record`] is a single `Cell` read.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The stack of per-body read-sets. Top = the innermost active module body.
    static STACK: RefCell<Vec<BTreeMap<Rc<str>, Value>>> = const { RefCell::new(Vec::new()) };
}

/// The recorded read-set of a module body: the `$`-vars it read, with the value each resolved to.
pub(super) type Reads = BTreeMap<Rc<str>, Value>;

/// Record a `$`-var read (name → resolved value; `None` = unbound ⇒ the body used `undef`). No-op when no
/// recorder is active — one `Cell` read on the hot `lookup_opt` path. First-read-wins per name.
#[inline]
pub(super) fn record(name: &str, value: Option<&Value>) {
    if DEPTH.with(Cell::get) == 0 {
        return;
    }
    STACK.with(|s| {
        if let Some(top) = s.borrow_mut().last_mut()
            && !top.contains_key(name)
        {
            top.insert(Rc::from(name), value.cloned().unwrap_or(Value::Undef));
        }
    });
}

/// Begin recording a module body's reads (push a fresh recorder).
pub(super) fn push() {
    DEPTH.with(|d| d.set(d.get() + 1));
    STACK.with(|s| s.borrow_mut().push(BTreeMap::new()));
}

/// Finish a module body: pop its read-set, and PROPAGATE its reads into the enclosing recorder (transitive
/// dependency) before returning them. Returns an empty set if — defensively — the stack was empty.
pub(super) fn pop() -> Reads {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    STACK.with(|s| {
        let mut stack = s.borrow_mut();
        let reads = stack.pop().unwrap_or_default();
        if let Some(parent) = stack.last_mut() {
            for (name, value) in &reads {
                parent.entry(Rc::clone(name)).or_insert_with(|| value.clone());
            }
        }
        reads
    })
}

/// Whether the current context still AGREES with `reads` — i.e. every `$`-var the entry's body read resolves to
/// the same (bit-exact) value now. The soundness check behind a cache hit. Uses the RAW `$`-lookup so validating
/// a hit doesn't itself record into an enclosing recorder.
pub(super) fn agrees(reads: &Reads, scope: &super::scope::Scope) -> bool {
    reads.iter().all(|(name, recorded)| {
        let current = scope.lookup_special_raw(name).unwrap_or(Value::Undef);
        super::eval_cache::value_bits_eq(&current, recorded)
    })
}

/// Propagate a cache-HIT entry's reads into the enclosing recorder — the parent body transitively depends on
/// them even though the hit skipped re-running (the [`pop`] path covers the MISS case). No-op when off.
pub(super) fn propagate(reads: &Reads) {
    if DEPTH.with(Cell::get) == 0 {
        return;
    }
    STACK.with(|s| {
        if let Some(top) = s.borrow_mut().last_mut() {
            for (name, value) in reads {
                top.entry(Rc::clone(name)).or_insert_with(|| value.clone());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Reads, agrees, pop, push, record};
    use crate::Value;
    use std::rc::Rc;

    fn v(n: f64) -> Value {
        Value::Num(n)
    }

    /// record → pop returns the read-set; nested pop propagates into the parent; off ⇒ record is a no-op.
    #[test]
    fn records_and_propagates() {
        // off: no recorder → nothing captured.
        record("$fn", Some(&v(8.0)));
        push();
        record("$fn", Some(&v(100.0)));
        record("$fn", Some(&v(999.0))); // first-read-wins → 100 stays
        record("$foo", None); // unbound → Undef
        // nested body reads $bar; on pop it propagates up to the parent.
        push();
        record("$bar", Some(&v(2.0)));
        let inner: Reads = pop();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[&Rc::from("$bar")], v(2.0));
        let outer: Reads = pop();
        assert_eq!(outer[&Rc::from("$fn")], v(100.0), "first-read-wins");
        assert_eq!(outer[&Rc::from("$foo")], Value::Undef, "unbound → undef");
        assert_eq!(outer[&Rc::from("$bar")], v(2.0), "nested read propagated up");
        // back to off: record is a no-op again.
        record("$baz", Some(&v(1.0)));
        push();
        assert!(pop().is_empty());
    }

    /// `agrees` is bit-exact: a matching context passes, a differing one fails, and undef-read matches unbound.
    #[test]
    fn agreement_is_bit_exact() {
        let mut reads = Reads::new();
        reads.insert(Rc::from("$fn"), v(100.0));
        reads.insert(Rc::from("$gap"), Value::Undef);
        // A scope with $fn=100 and $gap unbound agrees.
        let mut s = crate::Scope::new();
        s.bind("$fn", v(100.0));
        assert!(agrees(&reads, &s), "$fn=100, $gap unbound → agrees");
        // $fn=50 disagrees.
        let mut s2 = crate::Scope::new();
        s2.bind("$fn", v(50.0));
        assert!(!agrees(&reads, &s2), "$fn=50 → disagrees");
        // binding $gap breaks the undef match.
        let mut s3 = crate::Scope::new();
        s3.bind("$fn", v(100.0));
        s3.bind("$gap", v(0.0));
        assert!(!agrees(&reads, &s3), "$gap now bound → disagrees");
    }
}
