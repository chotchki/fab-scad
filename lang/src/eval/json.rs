//! JSON → [`Value`] (AI.2) — the parse half of expression-position `import("data.json")`.
//!
//! `serde_json` with DEFAULT features on purpose: its `BTreeMap`-backed map SORTS keys, which is
//! exactly upstream's behavior (nlohmann's `std::map` — the import-json golden echoes members
//! alphabetically, NOT in file order). Mapping: object → [`Value::Object`] (inserted in sorted
//! order), array → list, number → `f64`, string/bool direct, `null` → undef. Pure conversion —
//! the io shell reads the bytes; recursion is bounded by `serde_json`'s own depth limit (128).

use std::rc::Rc;

use super::object::ObjectMap;
use super::value::Value;

/// Parse JSON `bytes` into a [`Value`] tree, or the parse error's text.
///
/// # Errors
/// The `serde_json` message for malformed input (the caller warns + yields undef, upstream's
/// tolerant shape).
pub(super) fn value_from_json(bytes: &[u8]) -> Result<Value, String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    Ok(convert(&v))
}

fn convert(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Undef,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Num(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Value::string(s.clone()),
        serde_json::Value::Array(xs) => super::build_vector(xs.iter().map(convert).collect()),
        serde_json::Value::Object(map) => {
            let mut o = ObjectMap::new();
            for (k, val) in map {
                o.set(Rc::from(k.as_str()), convert(val));
            }
            Value::Object(Rc::new(o))
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, reason = "test-only shape asserts")]
mod tests {
    use super::*;

    #[test]
    fn json_maps_to_values_with_sorted_keys() {
        let v = value_from_json(br#"{"b": 2, "a": [1, "x", null], "c": {"n": 3.5e-10}}"#).unwrap();
        let Value::Object(o) = &v else {
            panic!("object expected")
        };
        let keys: Vec<_> = o.keys().map(ToString::to_string).collect();
        assert_eq!(keys, ["a", "b", "c"], "keys sort (upstream's std::map)");
        assert_eq!(o.get("b"), Some(&Value::Num(2.0)));
        let Some(Value::List(a)) = o.get("a") else {
            panic!("mixed array is a List")
        };
        assert_eq!(a.last(), Some(&Value::Undef), "null → undef");
        assert!(value_from_json(b"{nope").is_err());
    }
}
