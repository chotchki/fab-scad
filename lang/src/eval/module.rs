//! Module instantiation → [`Mesh`]: sphere/cube/cylinder argument resolution + tessellation dispatch.
//!
//! Argument binding (OpenSCAD `primitives.cc`): positional args fill the primitive's parameter list
//! in order, named args bind by name; diameter beats radius (`d = 2r`). `$`-args (`$fn=8`) set the
//! dynamically-scoped `$`-variables in a child scope, feeding the fragment count. Transforms,
//! booleans, and user modules are deferred LOUD.

use std::collections::BTreeMap;

use super::fragments::fragments;
use super::scope::Scope;
use super::value::Value;
use super::{Ctx, eval_with_ctx, geometry};
use crate::Mesh;
use crate::parser::ModuleInstantiation;

/// Evaluate a module instantiation's arguments: positional values, named values, and a child scope
/// with the `$`-args bound (dynamic scope). Shared by the primitive dispatch and the transform-matrix
/// builder (J.2) — both need the same OpenSCAD arg-matching.
pub(super) fn eval_args<'a>(
    mi: &'a ModuleInstantiation,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<(Vec<Value>, BTreeMap<String, Value>, Scope)> {
    let mut child = scope.clone();
    let mut positional = Vec::new();
    let mut named = BTreeMap::new();
    for arg in &mi.args {
        let value = eval_with_ctx(&arg.value, scope, ctx)?;
        match &arg.name {
            Some(name) if name.starts_with('$') => child.bind(name.clone(), value),
            Some(name) => {
                named.insert(name.clone(), value);
            }
            None => positional.push(value),
        }
    }
    Ok((positional, named, child))
}

/// Evaluate a PRIMITIVE module instantiation to a mesh (sphere/cube/cylinder). Transforms + booleans +
/// user modules are dispatched by the caller ([`super::eval_stmt`]); anything else fails LOUD.
pub(super) fn eval_module<'a>(
    mi: &'a ModuleInstantiation,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Mesh> {
    // A benchmark span per primitive (I.6): its busy-time is the tessellation cost. TRACE level, so a
    // subscriber-less build pays one atomic load and `release_max_level_off` strips it entirely.
    let _span = tracing::trace_span!("module", module = mi.name.as_str()).entered();
    let (positional, named, child) = eval_args(mi, scope, ctx)?;
    match mi.name.as_str() {
        "sphere" => Ok(eval_sphere(&positional, &named, &child)),
        "cube" => Ok(eval_cube(&positional, &named)),
        "cylinder" => Ok(eval_cylinder(&positional, &named, &child)),
        _ => Err(crate::Error::Unimplemented(
            "unknown module — not a builtin primitive (sphere/cube/cylinder), transform, boolean, or a \
             defined user module (a typo, or a builtin still deferred past the current subset)",
        )),
    }
}

fn eval_sphere(positional: &[Value], named: &BTreeMap<String, Value>, scope: &Scope) -> Mesh {
    let map = bind(positional, named, &["r"]);
    let r = get_radius(&map, "r", "d").unwrap_or(1.0);
    let (fn_, fa, fs) = scope.fn_fa_fs();
    geometry::sphere(r, fragments(r, fn_, fa, fs))
}

fn eval_cube(positional: &[Value], named: &BTreeMap<String, Value>) -> Mesh {
    let map = bind(positional, named, &["size", "center"]);
    let size = match map.get("size") {
        Some(Value::Num(s)) => [*s, *s, *s],
        Some(Value::NumList(v)) => match v[..] {
            [x, y, z, ..] => [x, y, z],
            _ => [1.0, 1.0, 1.0],
        },
        _ => [1.0, 1.0, 1.0],
    };
    geometry::cube(size, is_true(&map, "center"))
}

fn eval_cylinder(positional: &[Value], named: &BTreeMap<String, Value>, scope: &Scope) -> Mesh {
    let map = bind(positional, named, &["h", "r1", "r2", "center"]);
    let h = get_num(&map, "h").unwrap_or(1.0);
    // `r`/`d` set both radii; `r1`/`r2` (and `d1`/`d2`) then override.
    let both = get_radius(&map, "r", "d");
    let r1 = get_radius(&map, "r1", "d1").or(both).unwrap_or(1.0);
    let r2 = get_radius(&map, "r2", "d2").or(both).unwrap_or(1.0);
    let (fn_, fa, fs) = scope.fn_fa_fs();
    let frags = fragments(r1.max(r2), fn_, fa, fs); // fmax(r1, r2)
    geometry::cylinder(h, r1, r2, frags, is_true(&map, "center"))
}

/// Bind positional args to their parameter names in order; named args (already in `named`) win.
fn bind(
    positional: &[Value],
    named: &BTreeMap<String, Value>,
    params: &[&str],
) -> BTreeMap<String, Value> {
    let mut map = named.clone();
    for (value, name) in positional.iter().zip(params.iter()) {
        map.entry((*name).to_string())
            .or_insert_with(|| value.clone());
    }
    map
}

fn get_num(map: &BTreeMap<String, Value>, key: &str) -> Option<f64> {
    match map.get(key) {
        Some(Value::Num(n)) => Some(*n),
        _ => None,
    }
}

/// Radius, with diameter winning (`d`/2). Returns `None` if neither is a number.
fn get_radius(map: &BTreeMap<String, Value>, r_key: &str, d_key: &str) -> Option<f64> {
    get_num(map, d_key)
        .map(|d| d / 2.0)
        .or_else(|| get_num(map, r_key))
}

fn is_true(map: &BTreeMap<String, Value>, key: &str) -> bool {
    matches!(map.get(key), Some(Value::Bool(true)))
}
