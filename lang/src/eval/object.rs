//! First-class OBJECT values (Phase AF) — OpenSCAD's experimental `object()` type, always-on here
//! (the dev-snapshot-with-flags surface is the de-facto platform the corpus tests; object VALUES
//! already flow from `textmetrics()`/`fontmetrics()`/JSON `import()` regardless of the flag).
//!
//! An object is an INSERTION-ORDERED string→[`Value`] map: iteration and echo render in the order
//! members were set, lookup must survive the spec's 100k-random-accesses-over-100k-keys perf case.
//! `entries` carries the order; `index` (a `BTreeMap` — deterministic, no hasher, doctrine #36)
//! carries O(log n) lookup. Values are built once and shared (`Rc`), like every other Value payload.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::value::Value;

/// The payload of [`Value::Object`]: insertion-ordered members + a name index.
#[derive(Debug, Clone, Default)]
pub struct ObjectMap {
    /// `(name, value)` in FIRST-set order — a later `set` of an existing name updates the value
    /// IN PLACE (order keeps the first position, matching upstream's accumulate-left-to-right).
    entries: Vec<(Rc<str>, Value)>,
    /// name → slot in `entries`.
    index: BTreeMap<Rc<str>, usize>,
}

impl ObjectMap {
    /// An empty object (`object()`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `name` — a NEW name appends (insertion order), an existing one updates in place
    /// (upstream: "later settings for a member replace earlier settings", position kept).
    pub fn set(&mut self, name: Rc<str>, value: Value) {
        if let Some(&i) = self.index.get(&name) {
            self.entries[i].1 = value;
        } else {
            self.index.insert(Rc::clone(&name), self.entries.len());
            self.entries.push((name, value));
        }
    }

    /// Remove `name` if present (the `[["key"]]` edit form). Order of the survivors is preserved;
    /// the index rebuilds (removal is edit-time-only, never on a hot path).
    pub fn remove(&mut self, name: &str) {
        if let Some(i) = self.index.remove(name) {
            self.entries.remove(i);
            for slot in self.index.values_mut() {
                if *slot > i {
                    *slot -= 1;
                }
            }
        }
    }

    /// Member lookup.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.index.get(name).map(|&i| &self.entries[i].1)
    }

    /// Does `name` exist? (`has_key`.)
    #[must_use]
    pub fn has_key(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// Member count (`len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No members?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The members in insertion order (echo/str + key iteration).
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Rc<str>, &Value)> {
        self.entries.iter().map(|(n, v)| (n, v))
    }

    /// The keys in insertion order (`for (k = obj)` iterates KEYS upstream).
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &Rc<str>> {
        self.entries.iter().map(|(n, _)| n)
    }
}

/// Structural equality, ORDER-INSENSITIVE (same key set, per-key equal values — both indexes are
/// `BTreeMap`s, so the zipped walk is deterministic). Function members compare by the AH.2.7
/// identity rule through `Value`'s own `PartialEq`.
impl PartialEq for ObjectMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .index
                .iter()
                .zip(other.index.iter())
                .all(|((ka, &ia), (kb, &ib))| ka == kb && self.entries[ia].1 == other.entries[ib].1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove_keep_insertion_order() {
        let mut o = ObjectMap::new();
        o.set(Rc::from("a"), Value::Num(1.0));
        o.set(Rc::from("b"), Value::Num(2.0));
        o.set(Rc::from("a"), Value::Num(3.0)); // update in place, position kept
        assert_eq!(o.len(), 2);
        assert_eq!(o.get("a"), Some(&Value::Num(3.0)));
        let keys: Vec<_> = o.keys().map(ToString::to_string).collect();
        assert_eq!(keys, ["a", "b"]);
        o.remove("a");
        assert_eq!(o.len(), 1);
        assert!(!o.has_key("a"));
        assert_eq!(o.get("b"), Some(&Value::Num(2.0)), "index survives removal");
    }

    #[test]
    fn equality_is_key_set_plus_values_not_order() {
        let mut a = ObjectMap::new();
        a.set(Rc::from("x"), Value::Num(1.0));
        a.set(Rc::from("y"), Value::Num(2.0));
        let mut b = ObjectMap::new();
        b.set(Rc::from("y"), Value::Num(2.0));
        b.set(Rc::from("x"), Value::Num(1.0));
        assert_eq!(a, b);
        b.set(Rc::from("y"), Value::Num(9.0));
        assert_ne!(a, b);
    }
}
