//! Module instantiation → [`Mesh`]: sphere/cube/cylinder argument resolution + tessellation dispatch.
//!
//! Argument binding (OpenSCAD `primitives.cc`): positional args fill the primitive's parameter list
//! in order, named args bind by name; diameter beats radius (`d = 2r`). `$`-args (`$fn=8`) set the
//! dynamically-scoped `$`-variables in a child scope, feeding the fragment count. Transforms,
//! booleans, and user modules are deferred LOUD.

use std::collections::BTreeMap;

use super::fragments::fragments;
use super::geo::GeoNode;
use super::geo2d::{Contour, Geo, Shape2D};
use super::scope::Scope;
use super::value::Value;
use super::{Ctx, eval_with_ctx, geometry};
use crate::Mesh;
use crate::geom::Vec2;
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

/// Evaluate a PRIMITIVE module instantiation to a dimension-tagged [`Geo`]: the 3D primitives
/// (sphere/cube/cylinder/polyhedron) tessellate to a [`GeoNode::Leaf`] mesh, the 2D ones
/// (square/circle/polygon) to a [`Shape2D::Polygon`] of contours. Transforms + booleans + user modules
/// are dispatched by the caller ([`super::eval_stmt`]); anything else fails LOUD.
///
/// A degenerate primitive (`cube(0)`, `circle(0)`) still returns a PRESENT leaf (an empty mesh / no
/// contours) — NOT [`Geo::is_null`]. That distinction is load-bearing: a `cube(0)` is an empty-but-present
/// 3D object that fixes a group's dimension, exactly as OpenSCAD treats it (verified against 2026.06.12).
pub(super) fn eval_module<'a>(
    mi: &'a ModuleInstantiation,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<Geo> {
    // A benchmark span per primitive (I.6): its busy-time is the tessellation cost. TRACE level, so a
    // subscriber-less build pays one atomic load and `release_max_level_off` strips it entirely.
    let _span = tracing::trace_span!("module", module = mi.name.as_str()).entered();
    let (positional, named, child) = eval_args(mi, scope, ctx)?;
    match mi.name.as_str() {
        // 3D primitives → a tessellated mesh Leaf.
        "sphere" => Ok(leaf3(eval_sphere(&positional, &named, &child))),
        "cube" => Ok(leaf3(eval_cube(&positional, &named))),
        "cylinder" => Ok(leaf3(eval_cylinder(&positional, &named, &child))),
        "polyhedron" => Ok(leaf3(eval_polyhedron(&positional, &named))),
        // 2D primitives → a contour polygon (the Shape2D leaf, J.3.2).
        "square" => Ok(poly2(eval_square(&positional, &named))),
        "circle" => Ok(poly2(eval_circle(&positional, &named, &child))),
        "polygon" => Ok(poly2(eval_polygon(&positional, &named))),
        // KNOWN-but-deferred builtins — recognized so the error NAMES the feature + its task instead of
        // a misleading "typo?". The 2D↔3D bridge modules + `offset` are the next J.3 tasks; each fails
        // LOUD here until wired (never silently wrong). (text/import/minkowski/surface are J.4.)
        "projection" => Err(crate::Error::Unimplemented(
            "projection() (the 3D→2D flatten bridge) is not yet wired — J.3.6",
        )),
        _ => Err(crate::Error::Unimplemented(
            "unknown module — not a builtin primitive (sphere/cube/cylinder/polyhedron, \
             square/circle/polygon), transform, boolean, or a defined user module (a typo, or a builtin \
             still deferred past the current subset)",
        )),
    }
}

/// Wrap a tessellated mesh as a 3D geometry leaf.
fn leaf3(mesh: Mesh) -> Geo {
    Geo::D3(GeoNode::Leaf(mesh))
}

/// Wrap 2D contours as a 2D geometry leaf (empty contours are still a PRESENT 2D object — dim-fixing,
/// not [`Geo::is_null`]).
fn poly2(contours: Vec<Contour>) -> Geo {
    Geo::D2(Shape2D::Polygon(contours))
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

/// `square(size, center)` → its contour (J.3.2). `size` is a scalar (→ `[s, s]`) or an `[x, y]` vector;
/// a malformed vector falls back to the unit square, mirroring [`eval_cube`]'s convention.
fn eval_square(positional: &[Value], named: &BTreeMap<String, Value>) -> Vec<Contour> {
    let map = bind(positional, named, &["size", "center"]);
    let (x, y) = match map.get("size") {
        Some(Value::Num(s)) => (*s, *s),
        Some(Value::NumList(v)) => match v[..] {
            [x, y, ..] => (x, y),
            _ => (1.0, 1.0),
        },
        _ => (1.0, 1.0),
    };
    geometry::square(x, y, is_true(&map, "center"))
}

/// `circle(r | d, $fn)` → its contour, a regular `$fn`-gon (J.3.2). `$fn`/`$fa`/`$fs` resolve the segment
/// count via the SAME [`fragments`] path as `sphere`/`cylinder`, so a `circle` and a same-radius `cylinder`
/// cap share vertices to the bit.
fn eval_circle(
    positional: &[Value],
    named: &BTreeMap<String, Value>,
    scope: &Scope,
) -> Vec<Contour> {
    let map = bind(positional, named, &["r"]);
    let r = get_radius(&map, "r", "d").unwrap_or(1.0);
    let (fn_, fa, fs) = scope.fn_fa_fs();
    geometry::circle(r, fragments(r, fn_, fa, fs))
}

/// `polygon(points, paths, convexity)` → its contours (J.3.2). `points` is a list of 2-vectors; `paths`
/// (optional) is a list of index loops into `points` (outer boundary + holes). Without `paths` the whole
/// point list is one contour. Malformed entries drop (a non-2 point, a bad index), mirroring
/// [`eval_polyhedron`] — the exact OpenSCAD out-of-range ERROR is the validation layer (a later J.3 task).
fn eval_polygon(positional: &[Value], named: &BTreeMap<String, Value>) -> Vec<Contour> {
    let map = bind(positional, named, &["points", "paths", "convexity"]);
    let points = to_points_2d(map.get("points"));
    let paths = to_paths(map.get("paths"));
    geometry::polygon(&points, paths.as_deref())
}

/// A `points` value → a 2D vertex table: a list of numeric ≥2-vectors → [`Vec2`]s (a shorter or
/// non-numeric entry drops, so its later path-references land out of range and drop too).
fn to_points_2d(v: Option<&Value>) -> Vec<Vec2> {
    let Some(Value::List(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| match p {
            Value::NumList(xs) if xs.len() >= 2 => Some(Vec2::new(xs[0], xs[1])),
            _ => None,
        })
        .collect()
}

/// A `paths` value → the index loops, or `None` when absent/`undef` (→ the single all-points contour).
/// A present list reuses [`to_faces`]' numeric-index-list parse (each entry a contour).
fn to_paths(v: Option<&Value>) -> Option<Vec<Vec<u32>>> {
    match v {
        Some(Value::List(_)) => Some(to_faces(v)),
        _ => None,
    }
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

/// `polyhedron(points, faces, convexity)` → a mesh (J.2.6). `points` is a list of 3-vectors, `faces` a
/// list of vertex-index loops; both feed [`geometry::polyhedron`], which fan-triangulates. `convexity` is
/// a render hint we don't need. Malformed entries (a non-3 point, a bad index) drop, so a bad input
/// yields a partial/empty mesh here — the exact OpenSCAD ERROR/WARNING is the validation layer (J.2.6.2).
fn eval_polyhedron(positional: &[Value], named: &BTreeMap<String, Value>) -> Mesh {
    let map = bind(positional, named, &["points", "faces", "convexity"]);
    geometry::polyhedron(to_points(map.get("points")), &to_faces(map.get("faces")))
}

/// A `points` value → the vertex table: a list of numeric 3-vectors → `Vec3`s (a shorter or non-numeric
/// entry is dropped, so its later face-references land out of range and drop too).
fn to_points(v: Option<&Value>) -> Vec<crate::geom::Vec3> {
    let Some(Value::List(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| match p {
            Value::NumList(xs) if xs.len() >= 3 => {
                Some(crate::geom::Vec3::new(xs[0], xs[1], xs[2]))
            }
            _ => None,
        })
        .collect()
}

/// A `faces` value → index loops: a list of numeric index lists (each a face). A non-numeric face is
/// dropped; [`to_index`] maps each entry.
fn to_faces(v: Option<&Value>) -> Vec<Vec<u32>> {
    let Some(Value::List(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|f| match f {
            Value::NumList(idx) => Some(idx.iter().map(|&i| to_index(i)).collect()),
            _ => None,
        })
        .collect()
}

/// A face's vertex index: a non-negative finite value truncates to its `u32` (OpenSCAD's `size_t` cast);
/// anything else (negative, fractional-only is fine, non-finite) → `u32::MAX`, an out-of-range sentinel
/// that [`geometry::polyhedron`] drops — matching OpenSCAD, where a bad index fails the face.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: a non-negative finite index truncates to u32; everything else becomes the \
    u32::MAX out-of-range sentinel"
)]
fn to_index(i: f64) -> u32 {
    if i >= 0.0 && i.is_finite() {
        i as u32
    } else {
        u32::MAX
    }
}
