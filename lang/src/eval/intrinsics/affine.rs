use super::geometry::{point2d, point3d};
use super::math::approx;
use super::shape::is_matrix;
use super::vectors::{unit, v_theta, vector_angle, vector_axis};
use super::{bosl_assert, is_vector_core, v_is_finite};
use crate::eval::value::{self, Value};
use crate::eval::{build_range, build_vector, builtins, iter_values, ops};
use crate::parser::BinOp;

/// BOSL2 `is_2d_transform(t)` — the affine matrix's z-action is trivial (with the zscale carve-out). Pure
/// index chains + `==`, fully routed; every branch value is a `Bool` like the interpreter's `&&`/`||` yield.
pub(super) fn is_2d_transform(args: &[Value]) -> crate::Result<Value> {
    let t = args.first().cloned().unwrap_or(Value::Undef);
    let at = |r: f64, c: f64| ops::index(ops::index(t.clone(), &Value::Num(r)), &Value::Num(c));
    let eq = |v: Value, k: f64| ops::apply_binary(BinOp::Eq, v, Value::Num(k)).is_truthy();
    let zs_clear = eq(at(2.0, 0.0), 0.0)
        && eq(at(2.0, 1.0), 0.0)
        && eq(at(2.0, 3.0), 0.0)
        && eq(at(0.0, 2.0), 0.0)
        && eq(at(1.0, 2.0), 0.0);
    if !zs_clear {
        return Ok(Value::Bool(false));
    }
    let xy_identity = eq(at(0.0, 0.0), 1.0)
        && eq(at(0.0, 1.0), 0.0)
        && eq(at(1.0, 0.0), 0.0)
        && eq(at(1.0, 1.0), 1.0);
    Ok(Value::Bool(eq(at(2.0, 2.0), 1.0) || !xy_identity))
}

/// BOSL2 `_apply(transform, points)` — affine matrix × point list. The interpreted version rebuilds the
/// transposed/scaled matrix and augments every point through per-element comprehension TASKS; here the same
/// values come from a handful of op calls (`concat` per point, one `/`, one matrix `*` — all already native
/// inside ops/builtins), which is where the 2.2s went.
#[allow(
    clippy::float_cmp,
    reason = "the reference's dimension checks (len==tdim, datadim==2) ARE exact f64 equalities"
)]
pub(super) fn apply_transform(args: &[Value]) -> crate::Result<Value> {
    let transform = args.first().cloned().unwrap_or(Value::Undef);
    let points = args.get(1).cloned().unwrap_or(Value::Undef);
    if !is_matrix(std::slice::from_ref(&transform))?.is_truthy() {
        return Err(bosl_assert("_apply: invalid transformation matrix"));
    }
    if !is_matrix(std::slice::from_ref(&points))?.is_truthy() {
        return Err(bosl_assert("_apply: invalid points list"));
    }
    // is_matrix guarantees lists-of-vectors, so the dims are plain numbers.
    let num_len = |v: &Value| -> f64 {
        match builtins::apply("len", std::slice::from_ref(v)) {
            Value::Num(n) => n,
            _ => f64::NAN, // unreachable: is_matrix above
        }
    };
    let lt = num_len(&transform);
    let tdim = num_len(&ops::index(transform.clone(), &Value::Num(0.0))) - 1.0;
    let datadim = num_len(&ops::index(points.clone(), &Value::Num(0.0)));
    if !(lt == tdim || lt - 1.0 == tdim) {
        return Err(bosl_assert(
            "_apply: transform matrix height not compatible with width",
        ));
    }
    if !(datadim == 2.0 || datadim == 3.0) {
        return Err(bosl_assert("_apply: data must be 2D or 3D"));
    }
    let scale = if lt == tdim {
        Value::Num(1.0)
    } else {
        ops::index(
            ops::index(transform.clone(), &Value::Num(tdim)),
            &Value::Num(tdim),
        )
    };
    let mut rows = Vec::new();
    for i in value::range_iter(0.0, 1.0, tdim) {
        let mut row = Vec::new();
        for j in value::range_iter(0.0, 1.0, datadim - 1.0) {
            row.push(ops::index(
                ops::index(transform.clone(), &Value::Num(j)),
                &Value::Num(i),
            ));
        }
        rows.push(build_vector(row));
    }
    let matrix = ops::apply_binary(BinOp::Div, build_vector(rows), scale);
    if tdim == datadim {
        let aug: Vec<Value> = iter_values(&points)
            .iter()
            .map(|p| builtins::apply("concat", &[p.clone(), Value::Num(1.0)]))
            .collect();
        return Ok(ops::apply_binary(BinOp::Mul, build_vector(aug), matrix));
    }
    if tdim == 3.0 && datadim == 2.0 {
        if !is_2d_transform(std::slice::from_ref(&transform))?.is_truthy() {
            return Err(bosl_assert(
                "_apply: transform is 3D and acts on Z, but points are 2D",
            ));
        }
        let aug: Vec<Value> = iter_values(&points)
            .iter()
            .map(|p| builtins::apply("concat", &[p.clone(), Value::num_list(vec![0.0, 1.0])]))
            .collect();
        return Ok(ops::apply_binary(BinOp::Mul, build_vector(aug), matrix));
    }
    Err(bosl_assert("_apply: unsupported combination"))
}

/// BOSL2 `ident(n)` — the n×n identity matrix, rows built like the comprehension would (`build_vector`
/// coalesces each all-num row to a `NumList`); a garbage `n` degenerates through `build_range` exactly as
/// interpreted.
pub(super) fn ident(args: &[Value]) -> crate::Result<Value> {
    let n = args.first().cloned().unwrap_or(Value::Undef);
    let end = ops::apply_binary(BinOp::Sub, n, Value::Num(1.0));
    let range = build_range(&Value::Num(0.0), &Value::Num(1.0), &end);
    let is_idx = iter_values(&range);
    let mut rows: Vec<Value> = Vec::new();
    for i in &is_idx {
        let row: Vec<Value> = is_idx
            .iter()
            .map(|j| {
                if ops::apply_binary(BinOp::Eq, i.clone(), j.clone()).is_truthy() {
                    Value::Num(1.0)
                } else {
                    Value::Num(0.0)
                }
            })
            .collect();
        rows.push(build_vector(row));
    }
    Ok(build_vector(rows))
}

/// One axis-rotation affine builder — the shared shape of `affine3d_zrot`/`xrot`/`yrot`: assert the angle
/// finite, take `sin`/`cos` through the REAL builtins (the exact-degree snap lives there), lay out the rows.
/// `layout` receives `(c, s, -s)` and returns the 16 cells in row order.
pub(super) fn axis_rot(
    args: &[Value],
    layout: fn(Value, Value, Value) -> [[Value; 4]; 4],
) -> crate::Result<Value> {
    let ang = args.first().cloned().unwrap_or(Value::Num(0.0));
    if !v_is_finite(&ang) {
        return Err(bosl_assert("affine3d rotation: angle must be finite"));
    }
    let c = builtins::apply("cos", std::slice::from_ref(&ang));
    let s = builtins::apply("sin", std::slice::from_ref(&ang));
    let ns = ops::apply_unary(crate::parser::UnOp::Neg, s.clone());
    let rows: Vec<Value> = layout(c, s, ns)
        .into_iter()
        .map(|row| build_vector(row.into_iter().collect()))
        .collect();
    Ok(build_vector(rows))
}
pub(super) fn affine3d_zrot(args: &[Value]) -> crate::Result<Value> {
    axis_rot(args, |c, s, ns| {
        let z = || Value::Num(0.0);
        let one = || Value::Num(1.0);
        [
            [c.clone(), ns, z(), z()],
            [s, c, z(), z()],
            [z(), z(), one(), z()],
            [z(), z(), z(), one()],
        ]
    })
}
pub(super) fn affine3d_xrot(args: &[Value]) -> crate::Result<Value> {
    axis_rot(args, |c, s, ns| {
        let z = || Value::Num(0.0);
        let one = || Value::Num(1.0);
        [
            [one(), z(), z(), z()],
            [z(), c.clone(), ns, z()],
            [z(), s, c, z()],
            [z(), z(), z(), one()],
        ]
    })
}
pub(super) fn affine3d_yrot(args: &[Value]) -> crate::Result<Value> {
    axis_rot(args, |c, s, ns| {
        let z = || Value::Num(0.0);
        let one = || Value::Num(1.0);
        [
            [c.clone(), z(), s, z()],
            [z(), one(), z(), z()],
            [ns, z(), c, z()],
            [z(), z(), z(), one()],
        ]
    })
}

/// BOSL2 `affine3d_identity() = ident(4)` — through the real [`ident`].
pub(super) fn affine3d_identity(_args: &[Value]) -> crate::Result<Value> {
    ident(&[Value::Num(4.0)])
}

/// BOSL2 `affine3d_rot_from_to(from, to)` — the rotation matrix taking `from` to `to`: identity when
/// already aligned ([`approx`] on the unit vectors), a z-rotation when both are planar (`v_theta` deltas),
/// else Rodrigues from [`vector_axis`]/[`vector_angle`] with the reference's exact cell arithmetic
/// (left-associated products, `.x/.y/.z` through the real member op, `sin`/`cos` through the builtins).
pub(super) fn affine3d_rot_from_to(args: &[Value]) -> crate::Result<Value> {
    let from = args.first().cloned().unwrap_or(Value::Undef);
    let to = args.get(1).cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&from) || !is_vector_core(&to) {
        return Err(bosl_assert("affine3d_rot_from_to: invalid vector"));
    }
    let lf = builtins::apply("len", std::slice::from_ref(&from));
    let lt = builtins::apply("len", std::slice::from_ref(&to));
    if !ops::apply_binary(BinOp::Eq, lf, lt).is_truthy() {
        return Err(bosl_assert("affine3d_rot_from_to: length mismatch"));
    }
    let from = unit(&[point3d(std::slice::from_ref(&from))?])?;
    let to = unit(&[point3d(std::slice::from_ref(&to))?])?;
    if approx(&[from.clone(), to.clone()])?.is_truthy() {
        return affine3d_identity(&[]);
    }
    let z0 = |v: &Value| {
        ops::apply_binary(BinOp::Eq, ops::member(v.clone(), "z"), Value::Num(0.0)).is_truthy()
    };
    if z0(&from) && z0(&to) {
        let theta =
            |v: &Value| -> crate::Result<Value> { v_theta(&[point2d(std::slice::from_ref(v))?]) };
        let dt = ops::apply_binary(BinOp::Sub, theta(&to)?, theta(&from)?);
        return affine3d_zrot(std::slice::from_ref(&dt));
    }
    let u = vector_axis(&[from.clone(), to.clone()])?;
    let ang = vector_angle(&[from, to])?;
    let c = builtins::apply("cos", std::slice::from_ref(&ang));
    let c2 = ops::apply_binary(BinOp::Sub, Value::Num(1.0), c.clone());
    let s = builtins::apply("sin", std::slice::from_ref(&ang));
    let ux = ops::member(u.clone(), "x");
    let uy = ops::member(u.clone(), "y");
    let uz = ops::member(u, "z");
    let mul = |a: &Value, b: &Value| ops::apply_binary(BinOp::Mul, a.clone(), b.clone());
    let add = |a: Value, b: Value| ops::apply_binary(BinOp::Add, a, b);
    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    // each cell exactly as written: ((u.i*u.j)*c2) ± (u.k*s), diagonal ((u.i*u.i)*c2) + c
    let cell = |a: &Value, b: &Value| mul(&mul(a, b), &c2);
    let row = |cells: Vec<Value>| build_vector(cells);
    let z = || Value::Num(0.0);
    let rows = vec![
        row(vec![
            add(cell(&ux, &ux), c.clone()),
            sub(cell(&ux, &uy), mul(&uz, &s)),
            add(cell(&ux, &uz), mul(&uy, &s)),
            z(),
        ]),
        row(vec![
            add(cell(&uy, &ux), mul(&uz, &s)),
            add(cell(&uy, &uy), c.clone()),
            sub(cell(&uy, &uz), mul(&ux, &s)),
            z(),
        ]),
        row(vec![
            sub(cell(&uz, &ux), mul(&uy, &s)),
            add(cell(&uz, &uy), mul(&ux, &s)),
            add(cell(&uz, &uz), c),
            z(),
        ]),
        row(vec![z(), z(), z(), Value::Num(1.0)]),
    ];
    Ok(build_vector(rows))
}
