use super::geometry::{point2d, point3d};
use super::math::approx;
use super::shape::is_matrix;
use super::vectors::{unit, v_theta, vector_angle, vector_axis};
use super::{bosl_assert, is_vector_core, v_is_finite};
use crate::eval::value::{self, Value};
use crate::eval::{build_range, build_vector, builtins, iter_values_raw, ops};
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
        let aug: Vec<Value> = iter_values_raw(&points)
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
        let aug: Vec<Value> = iter_values_raw(&points)
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
    let is_idx = iter_values_raw(&range);
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
    Ok(rodrigues_rows(&u, &c, &c2, &s))
}

/// The `apply`-reachable slice of BOSL2 `determinant`: the closed-form 1–4 lanes (each with its own
/// `M*0 == [[0,…]]` shape assert and the reference's exact term order/associativity). The n≥5 minor
/// recursion is UNREACHABLE from `apply` (the vnf lane's `_apply` asserts force a 4×4 before determinant
/// runs) — LOUD error, not a silent wrong answer, if that proof ever breaks.
fn det_reachable(m: &Value) -> crate::Result<Value> {
    if !super::v_is_list(m) {
        return Err(bosl_assert("determinant: input must be a square matrix"));
    }
    let n = builtins::apply("len", std::slice::from_ref(m));
    let at = |r: f64, c: f64| ops::index(ops::index(m.clone(), &Value::Num(r)), &Value::Num(c));
    let is_n = |k: f64| ops::apply_binary(BinOp::Eq, n.clone(), Value::Num(k)).is_truthy();
    // the det2/3/4 shape assert: `M*0 == [[0,…],…]` through the interpreter's own ops
    let shape_ok = |k: usize| {
        let zero_row = Value::num_list(vec![0.0; k]);
        let zeros = build_vector(vec![zero_row; k]);
        ops::apply_binary(
            BinOp::Eq,
            ops::apply_binary(BinOp::Mul, m.clone(), Value::Num(0.0)),
            zeros,
        )
        .is_truthy()
    };
    if is_n(1.0) {
        return Ok(at(0.0, 0.0));
    }
    let mul = |a: Value, b: Value| ops::apply_binary(BinOp::Mul, a, b);
    let add = |a: Value, b: Value| ops::apply_binary(BinOp::Add, a, b);
    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    if is_n(2.0) {
        if !shape_ok(2) {
            return Err(bosl_assert("det2: expected square matrix (2x2)"));
        }
        let r0 = ops::index(m.clone(), &Value::Num(0.0));
        let r1 = ops::index(m.clone(), &Value::Num(1.0));
        return Ok(builtins::apply("cross", &[r0, r1]));
    }
    if is_n(3.0) {
        if !shape_ok(3) {
            return Err(bosl_assert("det3: expected square matrix (3x3)"));
        }
        // M[0][0]*(M[1][1]*M[2][2]-M[2][1]*M[1][2]) - M[1][0]*(…) + M[2][0]*(…)
        let minor = |a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)| {
            sub(
                mul(at(a.0, a.1), at(b.0, b.1)),
                mul(at(c.0, c.1), at(d.0, d.1)),
            )
        };
        let t0 = mul(
            at(0.0, 0.0),
            minor((1.0, 1.0), (2.0, 2.0), (2.0, 1.0), (1.0, 2.0)),
        );
        let t1 = mul(
            at(1.0, 0.0),
            minor((0.0, 1.0), (2.0, 2.0), (2.0, 1.0), (0.0, 2.0)),
        );
        let t2 = mul(
            at(2.0, 0.0),
            minor((0.0, 1.0), (1.0, 2.0), (1.0, 1.0), (0.0, 2.0)),
        );
        return Ok(add(sub(t0, t1), t2));
    }
    if is_n(4.0) {
        // det4's 24 four-factor terms folded in SOURCE order (12 added, 12 subtracted), each product
        // left-associated: ((M[a][b]*M[c][d])*M[e][f])*M[g][h].
        #[rustfmt::skip]
        const TERMS: [(bool, [(f64, f64); 4]); 24] = [
            (true,  [(0.,0.),(1.,1.),(2.,2.),(3.,3.)]), (true,  [(0.,0.),(1.,2.),(2.,3.),(3.,1.)]),
            (true,  [(0.,0.),(1.,3.),(2.,1.),(3.,2.)]), (true,  [(0.,1.),(1.,0.),(2.,3.),(3.,2.)]),
            (true,  [(0.,1.),(1.,2.),(2.,0.),(3.,3.)]), (true,  [(0.,1.),(1.,3.),(2.,2.),(3.,0.)]),
            (true,  [(0.,2.),(1.,0.),(2.,1.),(3.,3.)]), (true,  [(0.,2.),(1.,1.),(2.,3.),(3.,0.)]),
            (true,  [(0.,2.),(1.,3.),(2.,0.),(3.,1.)]), (true,  [(0.,3.),(1.,0.),(2.,2.),(3.,1.)]),
            (true,  [(0.,3.),(1.,1.),(2.,0.),(3.,2.)]), (true,  [(0.,3.),(1.,2.),(2.,1.),(3.,0.)]),
            (false, [(0.,0.),(1.,1.),(2.,3.),(3.,2.)]), (false, [(0.,0.),(1.,2.),(2.,1.),(3.,3.)]),
            (false, [(0.,0.),(1.,3.),(2.,2.),(3.,1.)]), (false, [(0.,1.),(1.,0.),(2.,2.),(3.,3.)]),
            (false, [(0.,1.),(1.,2.),(2.,3.),(3.,0.)]), (false, [(0.,1.),(1.,3.),(2.,0.),(3.,2.)]),
            (false, [(0.,2.),(1.,0.),(2.,3.),(3.,1.)]), (false, [(0.,2.),(1.,1.),(2.,0.),(3.,3.)]),
            (false, [(0.,2.),(1.,3.),(2.,1.),(3.,0.)]), (false, [(0.,3.),(1.,0.),(2.,1.),(3.,2.)]),
            (false, [(0.,3.),(1.,1.),(2.,2.),(3.,0.)]), (false, [(0.,3.),(1.,2.),(2.,0.),(3.,1.)]),
        ];
        if !shape_ok(4) {
            return Err(bosl_assert("det4: expected square matrix (4x4)"));
        }
        let product = |ix: &[(f64, f64); 4]| {
            let mut acc = at(ix[0].0, ix[0].1);
            for &(r, c) in &ix[1..] {
                acc = mul(acc, at(r, c));
            }
            acc
        };
        let mut acc = product(&TERMS[0].1);
        for (plus, ix) in &TERMS[1..] {
            let p = product(ix);
            acc = if *plus { add(acc, p) } else { sub(acc, p) };
        }
        return Ok(acc);
    }
    Err(crate::Error::Eval(
        "determinant: n>4 unreachable from apply (intrinsic guard)".to_string(),
    ))
}

/// BOSL2 `str_join(list, sep)` as `reverse`'s string lane reaches it: the tail recursion becomes a loop,
/// every concatenation through the REAL `str` builtin.
fn str_join_val(list: &Value, sep: &Value) -> Value {
    let ll = builtins::apply("len", std::slice::from_ref(list));
    let last_idx = ops::apply_binary(BinOp::Sub, ll.clone(), Value::Num(1.0));
    let mut i = 0.0;
    let mut result = Value::string("");
    loop {
        let iv = Value::Num(i);
        let item = ops::index(list.clone(), &iv);
        if ops::apply_binary(BinOp::Ge, iv.clone(), last_idx.clone()).is_truthy() {
            return if ops::apply_binary(BinOp::Eq, iv, ll).is_truthy() {
                result
            } else {
                builtins::apply("str", &[result, item])
            };
        }
        result = builtins::apply("str", &[result, item, sep.clone()]);
        i += 1.0;
    }
}

/// BOSL2 `reverse(list)` — the USER fn that shadows the builtin: reversed-range gather, with the string
/// lane rejoining through [`str_join_val`].
fn reverse_val(list: &Value) -> crate::Result<Value> {
    let is_str = matches!(list, Value::Str(_));
    if !(super::v_is_list(list) || is_str) {
        return Err(bosl_assert("reverse: input must be a list or string"));
    }
    let last_idx = ops::apply_binary(
        BinOp::Sub,
        builtins::apply("len", std::slice::from_ref(list)),
        Value::Num(1.0),
    );
    let range = build_range(&last_idx, &Value::Num(-1.0), &Value::Num(0.0));
    let elems: Vec<Value> = iter_values_raw(&range)
        .iter()
        .map(|i| ops::index(list.clone(), i))
        .collect();
    let elems = build_vector(elems);
    Ok(if is_str {
        str_join_val(&elems, &Value::string(""))
    } else {
        elems
    })
}

/// BOSL2 `apply(transform, points)` — the public dispatcher over [`apply_transform`]: single point,
/// VNF (with the mirror-detection determinant + `vnf_reverse_faces` lane), bezier patch, or plain point
/// list. Branch ORDER and the vnf `let`'s eager `newvnf`-before-determinant evaluation preserved (the
/// `_apply` asserts fire first, which is what makes [`det_reachable`]'s 4×4-only proof hold).
pub(super) fn apply(args: &[Value]) -> crate::Result<Value> {
    let transform = args.first().cloned().unwrap_or(Value::Undef);
    let points = args.get(1).cloned().unwrap_or(Value::Undef);
    if ops::apply_binary(BinOp::Eq, points.clone(), build_vector(Vec::new())).is_truthy() {
        return Ok(build_vector(Vec::new()));
    }
    if is_vector_core(&points) {
        let one = apply_transform(&[transform, build_vector(vec![points])])?;
        return Ok(ops::index(one, &Value::Num(0.0)));
    }
    if super::geometry::is_vnf_check(&points)? {
        let new_verts = apply_transform(&[
            transform.clone(),
            ops::index(points.clone(), &Value::Num(0.0)),
        ])?;
        let faces = ops::index(points, &Value::Num(1.0));
        let newvnf = build_vector(vec![new_verts, faces.clone()]);
        let lt = builtins::apply("len", std::slice::from_ref(&transform));
        let lt0 = builtins::apply(
            "len",
            std::slice::from_ref(&ops::index(transform.clone(), &Value::Num(0.0))),
        );
        let mirror = ops::apply_binary(BinOp::Eq, lt, lt0).is_truthy()
            && ops::apply_binary(BinOp::Lt, det_reachable(&transform)?, Value::Num(0.0))
                .is_truthy();
        if !mirror {
            return Ok(newvnf);
        }
        let rev_faces: crate::Result<Vec<Value>> =
            iter_values_raw(&faces).iter().map(reverse_val).collect();
        return Ok(build_vector(vec![
            ops::index(newvnf, &Value::Num(0.0)),
            build_vector(rev_faces?),
        ]));
    }
    let p0 = ops::index(points.clone(), &Value::Num(0.0));
    if super::v_is_list(&points)
        && super::v_is_list(&p0)
        && super::shape::is_vector(std::slice::from_ref(&ops::index(p0, &Value::Num(0.0))))?
            .is_truthy()
    {
        let rows: crate::Result<Vec<Value>> = iter_values_raw(&points)
            .iter()
            .map(|x| apply_transform(&[transform.clone(), x.clone()]))
            .collect();
        return Ok(build_vector(rows?));
    }
    apply_transform(&[transform, points])
}

/// BOSL2 `affine3d_translate(v=[0,0,0])` — the translation matrix; the slot-defaulting
/// `[for (i=[0:2]) default(v[i],0)]` runs through the real [`default`](super::lists::default).
pub(super) fn affine3d_translate(args: &[Value]) -> crate::Result<Value> {
    let v = args
        .first()
        .cloned()
        .unwrap_or_else(|| Value::num_list(vec![0.0, 0.0, 0.0]));
    if !super::v_is_list(&v) {
        return Err(bosl_assert("affine3d_translate: v must be a list"));
    }
    let slot = |i: f64| -> crate::Result<Value> {
        super::lists::default(&[ops::index(v.clone(), &Value::Num(i)), Value::Num(0.0)])
    };
    let (vx, vy, vz) = (slot(0.0)?, slot(1.0)?, slot(2.0)?);
    let z = || Value::Num(0.0);
    let one = || Value::Num(1.0);
    let rows = vec![
        build_vector(vec![one(), z(), z(), vx]),
        build_vector(vec![z(), one(), z(), vy]),
        build_vector(vec![z(), z(), one(), vz]),
        build_vector(vec![z(), z(), z(), one()]),
    ];
    Ok(build_vector(rows))
}

/// The Rodrigues rotation rows shared by [`affine3d_rot_by_axis`] and [`affine3d_rot_from_to`] — the
/// references' identical cell arithmetic: diagonal `((u.i*u.i)*c2)+c`, off-diagonal `((u.i*u.j)*c2) ± u.k*s`.
fn rodrigues_rows(u: &Value, c: &Value, c2: &Value, s: &Value) -> Value {
    let ux = ops::member(u.clone(), "x");
    let uy = ops::member(u.clone(), "y");
    let uz = ops::member(u.clone(), "z");
    let mul = |a: &Value, b: &Value| ops::apply_binary(BinOp::Mul, a.clone(), b.clone());
    let add = |a: Value, b: Value| ops::apply_binary(BinOp::Add, a, b);
    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    let cell = |a: &Value, b: &Value| mul(&mul(a, b), c2);
    let z = || Value::Num(0.0);
    build_vector(vec![
        build_vector(vec![
            add(cell(&ux, &ux), c.clone()),
            sub(cell(&ux, &uy), mul(&uz, s)),
            add(cell(&ux, &uz), mul(&uy, s)),
            z(),
        ]),
        build_vector(vec![
            add(cell(&uy, &ux), mul(&uz, s)),
            add(cell(&uy, &uy), c.clone()),
            sub(cell(&uy, &uz), mul(&ux, s)),
            z(),
        ]),
        build_vector(vec![
            sub(cell(&uz, &ux), mul(&uy, s)),
            add(cell(&uz, &uy), mul(&ux, s)),
            add(cell(&uz, &uz), c.clone()),
            z(),
        ]),
        build_vector(vec![z(), z(), z(), Value::Num(1.0)]),
    ])
}

/// BOSL2 `affine3d_rot_by_axis(u=UP, ang=0)` — Rodrigues about an arbitrary axis; identity shortcut through
/// the real [`approx`] (numeric lane only — `ang` is asserted finite).
pub(super) fn affine3d_rot_by_axis(args: &[Value]) -> crate::Result<Value> {
    let u = args
        .first()
        .cloned()
        .unwrap_or_else(super::vectors::bosl_up);
    let ang = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    if !v_is_finite(&ang) {
        return Err(bosl_assert("affine3d_rot_by_axis: angle must be finite"));
    }
    if !super::shape::is_vector(&[u.clone(), Value::Num(3.0)])?.is_truthy() {
        return Err(bosl_assert("affine3d_rot_by_axis: u must be a 3-vector"));
    }
    if approx(&[ang.clone(), Value::Num(0.0)])?.is_truthy() {
        return affine3d_identity(&[]);
    }
    let u = unit(std::slice::from_ref(&u))?;
    let c = builtins::apply("cos", std::slice::from_ref(&ang));
    let c2 = ops::apply_binary(BinOp::Sub, Value::Num(1.0), c.clone());
    let s = builtins::apply("sin", std::slice::from_ref(&ang));
    Ok(rodrigues_rows(&u, &c, &c2, &s))
}

/// The `rot`-reachable slice of BOSL2 `move`: `cp` is asserted a VECTOR in rot before `move(cp)` runs, so
/// the string lane (`centroid`/`mean`/`pointlist_bounds`) is unreachable — this is the
/// `affine3d_translate(point3d(v))` matrix branch with `p` defaulted.
fn move_mat(v: &Value) -> crate::Result<Value> {
    let len_ok = |k: f64| {
        ops::apply_binary(
            BinOp::Eq,
            builtins::apply("len", std::slice::from_ref(v)),
            Value::Num(k),
        )
        .is_truthy()
    };
    if !(is_vector_core(v) && (len_ok(3.0) || len_ok(2.0))) {
        return Err(bosl_assert("move: invalid value for v"));
    }
    affine3d_translate(&[point3d(std::slice::from_ref(v))?])
}

/// The `rot`-reachable slice of BOSL2 `rot_inverse` (the `reverse=true` lane): transpose the rotation
/// block, verify `approx(determinant(T), 1)` through [`det_reachable`], and reassemble via the pinned
/// `hstack`'s reachable semantics — row-wise `each`-splice of the transposed block and the negated
/// back-rotated translation, plus the `[0,…,0,1]` bottom row.
fn rot_inverse_val(t: &Value) -> crate::Result<Value> {
    if !super::shape::is_matrix(&[t.clone(), Value::Undef, Value::Undef, Value::Bool(true)])?
        .is_truthy()
    {
        return Err(bosl_assert("rot_inverse: matrix must be square"));
    }
    let n = builtins::apply("len", std::slice::from_ref(t));
    let is_n = |k: f64| ops::apply_binary(BinOp::Eq, n.clone(), Value::Num(k)).is_truthy();
    if !(is_n(3.0) || is_n(4.0)) {
        return Err(bosl_assert("rot_inverse: matrix must be 3x3 or 4x4"));
    }
    let last = ops::apply_binary(BinOp::Sub, n.clone(), Value::Num(1.0));
    let at = |r: &Value, c: &Value| ops::index(ops::index(t.clone(), r), c);
    let idx_range = build_range(
        &Value::Num(0.0),
        &Value::Num(1.0),
        &ops::apply_binary(BinOp::Sub, n.clone(), Value::Num(2.0)),
    );
    let idxs = iter_values_raw(&idx_range);
    let rotpart = build_vector(
        idxs.iter()
            .map(|i| build_vector(idxs.iter().map(|j| at(j, i)).collect()))
            .collect(),
    );
    let transpart = build_vector(idxs.iter().map(|row| at(row, &last)).collect());
    if !approx(&[det_reachable(t)?, Value::Num(1.0)])?.is_truthy() {
        return Err(bosl_assert("rot_inverse: matrix is not a rotation"));
    }
    // hstack(rotpart, -rotpart*transpart): row-wise each-splice — a row's elements then the scalar
    let back = ops::apply_binary(
        BinOp::Mul,
        ops::apply_unary(crate::parser::UnOp::Neg, rotpart.clone()),
        transpart,
    );
    let mut rows: Vec<Value> = Vec::new();
    for row in &idxs {
        let mut cells: Vec<Value> = iter_values_raw(&ops::index(rotpart.clone(), row));
        let b = ops::index(back.clone(), row);
        match b {
            Value::NumList(_) | Value::List(_) => cells.extend(iter_values_raw(&b)),
            other => cells.push(other),
        }
        rows.push(build_vector(cells));
    }
    // the bottom row: [for(i=[2:n]) 0, 1]
    let zrange = build_range(&Value::Num(2.0), &Value::Num(1.0), &n);
    let mut bottom: Vec<Value> = iter_values_raw(&zrange)
        .iter()
        .map(|_| Value::Num(0.0))
        .collect();
    bottom.push(Value::Num(1.0));
    rows.push(build_vector(bottom));
    Ok(build_vector(rows))
}

/// BOSL2 `rot(a=0, v, cp, from, to, reverse=false, p=_NO_ARG)` — the rotation dispatcher: from/to
/// (`rot_from_to` × `rot_by_axis`), axis-angle, scalar z-rotation, or Euler `zrot*yrot*xrot`; optional
/// centerpoint conjugation through the translate matrices; optional inversion through the
/// [`rot_inverse_val`] slice; `p` applied through the native [`apply`] unless it is the `_NO_ARG` sentinel
/// (the O.8 guard proves the bake).
pub(super) fn rot(args: &[Value]) -> crate::Result<Value> {
    let a = args.first().cloned().unwrap_or(Value::Num(0.0));
    let v = args.get(1).cloned().unwrap_or(Value::Undef);
    let cp = args.get(2).cloned().unwrap_or(Value::Undef);
    let from = args.get(3).cloned().unwrap_or(Value::Undef);
    let to = args.get(4).cloned().unwrap_or(Value::Undef);
    let reverse = args.get(5).cloned().unwrap_or(Value::Bool(false));
    let from_undef = matches!(from, Value::Undef);
    let to_undef = matches!(to, Value::Undef);
    if from_undef != to_undef {
        return Err(bosl_assert("rot: from and to must be specified together"));
    }
    let nonzero_vec = |x: &Value| -> crate::Result<bool> {
        Ok(matches!(x, Value::Undef)
            || super::shape::is_vector(&[x.clone(), Value::Undef, Value::Bool(false)])?.is_truthy())
    };
    if !nonzero_vec(&from)? {
        return Err(bosl_assert("rot: 'from' must be a non-zero vector"));
    }
    if !nonzero_vec(&to)? {
        return Err(bosl_assert("rot: 'to' must be a non-zero vector"));
    }
    if !nonzero_vec(&v)? {
        return Err(bosl_assert("rot: 'v' must be a non-zero vector"));
    }
    if !(matches!(cp, Value::Undef)
        || super::shape::is_vector(std::slice::from_ref(&cp))?.is_truthy())
    {
        return Err(bosl_assert("rot: 'cp' must be a vector"));
    }
    if !(v_is_finite(&a) || super::shape::is_vector(std::slice::from_ref(&a))?.is_truthy()) {
        return Err(bosl_assert("rot: 'a' must be a finite scalar or a vector"));
    }
    if !matches!(reverse, Value::Bool(_)) {
        return Err(bosl_assert("rot: reverse must be a bool"));
    }
    let a_is_num = matches!(&a, Value::Num(n) if !n.is_nan());
    let mul = |x: Value, y: Value| ops::apply_binary(BinOp::Mul, x, y);
    let m1 = if !from_undef {
        if !a_is_num {
            return Err(bosl_assert("rot: 'a' must be a number with from/to"));
        }
        let from3 = point3d(std::slice::from_ref(&from))?;
        let to3 = point3d(std::slice::from_ref(&to))?;
        mul(
            affine3d_rot_from_to(&[from3.clone(), to3])?,
            affine3d_rot_by_axis(&[from3, a])?,
        )
    } else if !matches!(v, Value::Undef) {
        if !a_is_num {
            return Err(bosl_assert("rot: 'a' must be a number with v"));
        }
        affine3d_rot_by_axis(&[v, a])?
    } else if a_is_num {
        affine3d_zrot(std::slice::from_ref(&a))?
    } else {
        let part = |field: &str| ops::member(a.clone(), field);
        mul(
            mul(affine3d_zrot(&[part("z")])?, affine3d_yrot(&[part("y")])?),
            affine3d_xrot(&[part("x")])?,
        )
    };
    let m2 = if matches!(cp, Value::Undef) {
        m1
    } else {
        let cp3 = point3d(std::slice::from_ref(&cp))?;
        let neg_cp = ops::apply_unary(crate::parser::UnOp::Neg, cp3.clone());
        mul(mul(move_mat(&cp3)?, m1), move_mat(&neg_cp)?)
    };
    let m3 = if reverse.is_truthy() {
        rot_inverse_val(&m2)?
    } else {
        m2
    };
    match args.get(6) {
        None => Ok(m3), // p defaulted → the _NO_ARG sentinel → the matrix
        Some(p) => {
            if ops::apply_binary(BinOp::Eq, p.clone(), super::no_arg_value()).is_truthy() {
                Ok(m3)
            } else {
                apply(&[m3, p.clone()])
            }
        }
    }
}
