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
use super::{Ctx, Imported, eval_with_ctx, geometry, text};
use crate::Mesh;
use crate::geom::{Vec2, Vec3};
use crate::parser::ModuleInstantiation;

/// Evaluate a module instantiation's arguments: positional values, named values, and a child scope
/// with the `$`-args bound (dynamic scope). Shared by the primitive dispatch and the transform-matrix
/// builder (J.2) — both need the same OpenSCAD arg-matching.
pub(super) fn eval_args<'a>(
    mi: &'a ModuleInstantiation,
    scope: &Scope,
    ctx: &Ctx<'a>,
) -> crate::Result<(Vec<Value>, BTreeMap<String, Value>, Scope)> {
    // child(), not clone+COW: a `$`-arg bind on a clone would COW the SHARED scope frame — inside a
    // cached module body that frame can be the read-capture's entry (BU.8 review finding 1). The child
    // keeps `$`-arg binds below the boundary, so `sphere(1, $fa=12)` inside a body KILLS the read.
    let mut child = scope.child();
    let mut positional = Vec::new();
    let mut named = BTreeMap::new();
    for arg in &mi.args {
        let value = eval_with_ctx(&arg.value, scope, ctx)?;
        match &arg.name {
            Some(name) if name.starts_with('$') => child.bind(name.clone(), value),
            Some(name) => {
                named.insert(name.to_string(), value);
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
        "polyhedron" => Ok(leaf3(eval_polyhedron(&positional, &named, ctx))),
        // 2D primitives → a contour polygon (the Shape2D leaf, J.3.2).
        "square" => Ok(poly2(eval_square(&positional, &named))),
        "circle" => Ok(poly2(eval_circle(&positional, &named, &child))),
        "polygon" => Ok(poly2(eval_polygon(&positional, &named))),
        "text" => Ok(poly2(eval_text(&positional, &named, &child))),
        // import()/surface() reference a FILE by a RUNTIME path — resolvable only by EXECUTING to here (the
        // path is an expression, not a static `<...>` token like use/include). Ask the caller's table (M.3):
        // its payload if present, else an EMPTY placeholder + a recorded File need so the run keeps going and
        // surfaces the REST of its needs (a file rarely gates control flow → the caller usually closes the
        // fixpoint in one more round). The reader that fills the table is caller-side (M.5). The payload is
        // dimension-TAGGED ([`Imported`]): a `.stl`/`.3mf` → a 3D mesh leaf, a `.svg`/`.dxf` → a 2D contour
        // polygon (Q.4) — so `import` wraps whichever the reader (or the placeholder) decided by extension.
        "import" => Ok(match ctx.request_file(file_arg(&positional, &named)) {
            Imported::Mesh(mesh) => leaf3(mesh),
            Imported::Contours(contours) => poly2(contours),
            // Bytes only fulfill the EXPRESSION-import channel (AI.1) — a geometry `import()`
            // never requests them (its table keys are raw mesh paths), so this arm is a
            // key-collision safety: render nothing rather than something wrong.
            Imported::Bytes(_) => leaf3(Mesh::new()),
        }),
        // surface() is import's heightmap sibling, plus `center` — a pure XY translate the path-only reader
        // can't do, so it's applied HERE from the eval arg (M.5.2). (`invert` is PNG-only → deferred.) A
        // heightmap is always 3D; a 2D payload here (a misnamed `.svg`) can't be a surface → empty, not wrong.
        "surface" => {
            let mesh = match ctx.request_file(file_arg(&positional, &named)) {
                Imported::Mesh(mesh) => mesh,
                Imported::Contours(_) | Imported::Bytes(_) => Mesh::new(),
            };
            let map = bind(&positional, &named, &["file", "center"]);
            Ok(leaf3(if is_true(&map, "center") {
                center_xy(mesh)
            } else {
                mesh
            }))
        }
        // KNOWN-but-deferred builtins — recognized so the error NAMES the feature + its task instead of a
        // misleading "typo?". These stay LOUD-deferred stubs: blow up naming the feature, never silently
        // empty. (`offset`, the extrudes, and `projection` are wired in eval_stmt as of J.3.3–J.3.6.)
        // `text` is a 2D primitive, dispatched above (→ glyph-outline contours); it never reaches here.
        // `minkowski` is intercepted in `eval_stmt` (like `hull`) → `GeoNode::Minkowski`; it never reaches
        // this stub. Kept out of the deferred list now that it's wired to Manifold's native sum (J.4.4).
        // Not a builtin primitive (sphere/cube/cylinder/polyhedron, square/circle/polygon), transform,
        // boolean, or a defined user module — a typo or a builtin still deferred past the current subset.
        // Naming it turns the corpus's generic "unknown module" cluster into a per-symbol worklist (L.2).
        // WARN and render NOTHING for this node — OpenSCAD's "Ignoring unknown module 'name'"
        // (`ModuleInstantiation::evaluate`). Faithful-to-oracle (L.5.7): a corpus naming a newer-BOSL2
        // module (`hulling`, `force_tags`) or a typo renders the REST instead of hard-failing. A builtin
        // fab hasn't wired yet still surfaces — as this NAMED console warning PLUS a geometry divergence the
        // differential catches (empty here vs the oracle's real node), never a silent pass.
        other => {
            ctx.warn(format!("Ignoring unknown module '{other}'"));
            Ok(Geo::D3(GeoNode::Empty))
        }
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

/// `text(t, size, font, halign, valign, spacing, direction, language, script, $fn)` → the glyph outlines as
/// 2D contours (J.4.3), shaped by rustybuzz + outlined by ttf-parser over the bundled Liberation Sans (see
/// [`super::text`]). OpenSCAD defaults: `size = 10`, `halign = "left"`, `valign = "baseline"`, `spacing = 1`.
/// A missing/non-string `text` → no contours (a present-but-empty 2D leaf, like `circle(0)`).
fn eval_text(positional: &[Value], named: &BTreeMap<String, Value>, scope: &Scope) -> Vec<Contour> {
    let map = bind(
        positional,
        named,
        &[
            "text",
            "size",
            "font",
            "halign",
            "valign",
            "spacing",
            "direction",
            "language",
            "script",
        ],
    );
    let params = text::TextParams {
        text: get_string(&map, "text").unwrap_or_default(),
        size: get_num(&map, "size").unwrap_or(10.0),
        halign: get_string(&map, "halign").unwrap_or_else(|| "left".to_string()),
        valign: get_string(&map, "valign").unwrap_or_else(|| "baseline".to_string()),
        spacing: get_num(&map, "spacing").unwrap_or(1.0),
        direction: get_string(&map, "direction").unwrap_or_default(),
        language: get_string(&map, "language").unwrap_or_default(),
        script: get_string(&map, "script").unwrap_or_default(),
    };
    let (fn_, fa, fs) = scope.fn_fa_fs();
    text::text_contours(&params, fn_, fa, fs)
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
        Some(Value::List(items)) => Some(to_faces(items)),
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

/// A bound STRING argument (`text`/`halign`/`font`/…), else `None` for absent or non-string — the caller
/// supplies the OpenSCAD default. Used by `text()` (J.4.3).
fn get_string(map: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// The `import`/`surface` `file=` path — positional-leading or named (M.3). Both builtins take the file as
/// their first argument, so binding just `["file"]` catches `import("a.stl")` and `import(file = "a.stl")`
/// alike. `None` for an absent or non-string `file=` (`import(undef)`, `import(5)`) → an empty result, which
/// matches the oracle's warn-and-render on a bad path (the reader-specific args like `surface`'s
/// invert/center ride in M.5, so they're ignored here).
/// Center a mesh on the XY origin — `surface()`'s `center` (M.5.2): translate so the XY bounding-box center
/// sits at `(0, 0)`; z (the heights) is untouched. The reader produces the natural-position heightmap, and
/// `center` is an EVAL arg the path-only reader never sees, so it's applied here.
fn center_xy(mesh: Mesh) -> Mesh {
    if mesh.verts.is_empty() {
        return mesh;
    }
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for v in &mesh.verts {
        min_x = min_x.min(v.x);
        max_x = max_x.max(v.x);
        min_y = min_y.min(v.y);
        max_y = max_y.max(v.y);
    }
    let (cx, cy) = (f64::midpoint(min_x, max_x), f64::midpoint(min_y, max_y));
    Mesh {
        verts: mesh
            .verts
            .iter()
            .map(|v| Vec3::new(v.x - cx, v.y - cy, v.z))
            .collect(),
        tris: mesh.tris,
    }
}

fn file_arg(positional: &[Value], named: &BTreeMap<String, Value>) -> Option<String> {
    let map = bind(positional, named, &["file"]);
    match map.get("file") {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// `polyhedron(points, faces, convexity)` → a mesh (J.2.6). `points` is a list of 3-vectors, `faces` a
/// list of vertex-index loops; both feed [`geometry::polyhedron`], which fan-triangulates. `convexity` is
/// a render hint we don't need. Malformed entries (a non-3 point, a bad index) drop, so a bad input
/// yields a partial/empty mesh here — the exact OpenSCAD ERROR/WARNING is the validation layer (J.2.6.2).
fn eval_polyhedron(positional: &[Value], named: &BTreeMap<String, Value>, ctx: &Ctx) -> Mesh {
    let map = bind(positional, named, &["points", "faces", "convexity"]);
    let points = to_points(map.get("points"));
    let n = u32::try_from(points.len()).unwrap_or(u32::MAX);
    geometry::polyhedron(points, &validated_faces(map.get("faces"), n, ctx))
}

/// Faces → index loops, matching OpenSCAD's `polyhedron` VALIDATION (J.2.6.2): a face that references an
/// out-of-range point index is DROPPED WHOLE (not just the triangles touching it) and, for a clean
/// too-big index, warned with OpenSCAD's exact text — the WARN-AND-RENDER behavior (never an error).
/// A non-numeric face drops silently, as does a `<3`-vertex face downstream (an empty fan). The exact
/// warning-text-vs-oracle comparison is the warning-differential channel (#94); this emits the right text.
fn validated_faces(v: Option<&Value>, n: u32, ctx: &Ctx) -> Vec<Vec<u32>> {
    let Some(Value::List(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let Value::NumList(raw) = f else {
                return None; // a non-numeric face → dropped silently (as before)
            };
            // The FIRST out-of-range index fails the whole face (OpenSCAD drops the face, not the tri).
            for (j, &r) in raw.iter().enumerate() {
                let idx = to_index(r);
                if idx >= n {
                    // Only a clean non-negative index gets OpenSCAD's "out of bounds" warning; a negative
                    // or non-finite one is a different malformation (its exact text is #94's job) — drop
                    // it silently rather than emit a misleading `4294967295`.
                    if r.is_finite() && r >= 0.0 {
                        ctx.warn(format!(
                            "Point index {idx} is out of bounds (from faces[{i}][{j}])"
                        ));
                    }
                    return None;
                }
            }
            Some(raw.iter().map(|&r| to_index(r)).collect())
        })
        .collect()
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

/// Index loops from a list's `items`: each numeric index-list is a contour/path (a non-numeric entry is
/// dropped); [`to_index`] maps each index. The caller has already matched the outer `Value::List`, so this
/// takes the items directly. Used for `polygon`'s `paths` (`polyhedron`'s faces go through
/// [`validated_faces`], which adds the out-of-range check + warning).
fn to_faces(items: &[Value]) -> Vec<Vec<u32>> {
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
