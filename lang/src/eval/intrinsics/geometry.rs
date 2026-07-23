use super::lists::{force_list, idx, select};
use super::math::{approx, sum};
use super::shape::is_vector;
use super::{bosl_assert, no_progress, non_terminating, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_range, build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// A finite-or-not 2D point view: `Some([x, y])` iff the value is a 2-element `NumList` (any bits — the
/// f64 formulas below are bit-faithful for inf/NaN too, so no finiteness gate).
pub(super) fn as_p2(v: &Value) -> Option<[f64; 2]> {
    match v {
        Value::NumList(xs) if xs.len() == 2 => Some([xs[0], xs[1]]),
        _ => None,
    }
}

/// The `_tri_class` scalar core on three 2D points — EXACTLY the reference's arithmetic: `crx = cross(
/// tri[1]-tri[2], tri[0]-tri[2])` with the builtins' own formulas (2D cross `a0*b1 - a1*b0`, `norm` =
/// sequential sum-of-squares sqrt), the tolerance product left-associated (`(eps*n1)*n2`), `sign` with 0 at
/// NaN.
pub(super) fn tri_class_2d(t0: [f64; 2], t1: [f64; 2], t2: [f64; 2], eps: f64) -> f64 {
    let u = [t1[0] - t2[0], t1[1] - t2[1]];
    let w = [t0[0] - t2[0], t0[1] - t2[1]];
    let crx = u[0] * w[1] - u[1] * w[0];
    let n1 = (u[0] * u[0] + u[1] * u[1]).sqrt();
    let n2 = (w[0] * w[0] + w[1] * w[1]).sqrt();
    if crx.abs() <= eps * n1 * n2 {
        0.0
    } else if crx > 0.0 {
        1.0
    } else if crx < 0.0 {
        -1.0
    } else {
        0.0 // sign(NaN) — unreachable (a NaN crx fails the <= above only when the bound is NaN too… routed
        // and fast agree either way because both use this exact chain)
    }
}

/// BOSL2 `_tri_class(tri, eps=_EPSILON)` — CW(1)/collinear(0)/CCW(-1) of a 2D triangle. Fast path for the
/// `[[x,y],[x,y],[x,y]]` + numeric-eps shape; everything else (3D points → undef, short lists, exotic eps)
/// routes through the real builtins/ops.
pub(super) fn tri_class(args: &[Value]) -> crate::Result<Value> {
    let tri = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    Ok(tri_class_val(&tri, &eps))
}
pub(super) fn tri_class_val(tri: &Value, eps: &Value) -> Value {
    if let (Value::Num(e), Value::List(xs)) = (eps, tri)
        && xs.len() == 3
        && let (Some(t0), Some(t1), Some(t2)) = (as_p2(&xs[0]), as_p2(&xs[1]), as_p2(&xs[2]))
    {
        return Value::Num(tri_class_2d(t0, t1, t2, *e));
    }
    let t0 = ops::index(tri.clone(), &Value::Num(0.0));
    let t1 = ops::index(tri.clone(), &Value::Num(1.0));
    let t2 = ops::index(tri.clone(), &Value::Num(2.0));
    let a = ops::apply_binary(BinOp::Sub, t1, t2.clone());
    let b = ops::apply_binary(BinOp::Sub, t0, t2);
    let crx = builtins::apply("cross", &[a.clone(), b.clone()]);
    let bound = ops::apply_binary(
        BinOp::Mul,
        ops::apply_binary(
            BinOp::Mul,
            eps.clone(),
            builtins::apply("norm", std::slice::from_ref(&a)),
        ),
        builtins::apply("norm", std::slice::from_ref(&b)),
    );
    let near = ops::apply_binary(
        BinOp::Le,
        builtins::apply("abs", std::slice::from_ref(&crx)),
        bound,
    );
    if near.is_truthy() {
        Value::Num(0.0)
    } else {
        builtins::apply("sign", std::slice::from_ref(&crx))
    }
}

/// BOSL2 `_is_at_left(pt,line,eps=_EPSILON) = _tri_class([pt,line[0],line[1]],eps) <= 0` — is `pt` left of
/// (or on) the directed 2D line? The routed tail builds the triangle with the interpreter's `build_vector`
/// (three Nums would coalesce to a `NumList` exactly like the literal would).
pub(super) fn is_at_left(args: &[Value]) -> crate::Result<Value> {
    let pt = args.first().cloned().unwrap_or(Value::Undef);
    let line = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Num(1e-9));
    Ok(is_at_left_val(&pt, &line, &eps))
}
pub(super) fn is_at_left_val(pt: &Value, line: &Value, eps: &Value) -> Value {
    if let (Value::Num(e), Some(p), Value::List(ls)) = (eps, as_p2(pt), line)
        && ls.len() == 2
        && let (Some(l0), Some(l1)) = (as_p2(&ls[0]), as_p2(&ls[1]))
    {
        return Value::Bool(tri_class_2d(p, l0, l1, *e) <= 0.0);
    }
    let l0 = ops::index(line.clone(), &Value::Num(0.0));
    let l1 = ops::index(line.clone(), &Value::Num(1.0));
    let tri = build_vector(vec![pt.clone(), l0, l1]);
    ops::apply_binary(BinOp::Le, tri_class_val(&tri, eps), Value::Num(0.0))
}

/// BOSL2 `_none_inside(idxs,poly,p0,p1,p2,eps,i=0)` — the ear-cut containment scan: is NO polygon vertex
/// (of `idxs`) blocking the candidate ear `[p0,p1,p2]`? The reference's tail recursion becomes a loop with
/// the same early-exit `false`; neighbor lookups go through the REAL native [`select`] (whose asserts are
/// what terminates the exotic-input shapes — a non-list `idxs` or non-numeric `i` raises there exactly like
/// the interpreter). Per-iteration fast path when everything is 2D + numeric; any shape break routes that
/// iteration through the same builtins/ops the body would run.
pub(super) fn none_inside(args: &[Value]) -> crate::Result<Value> {
    let idxs = args.first().cloned().unwrap_or(Value::Undef);
    let poly = args.get(1).cloned().unwrap_or(Value::Undef);
    let p0 = args.get(2).cloned().unwrap_or(Value::Undef);
    let p1 = args.get(3).cloned().unwrap_or(Value::Undef);
    let p2 = args.get(4).cloned().unwrap_or(Value::Undef);
    let eps = args.get(5).cloned().unwrap_or(Value::Undef); // eps has NO default in the reference
    let mut i = args.get(6).cloned().unwrap_or(Value::Num(0.0));

    // `_tri_class([a,b,c],eps)` as the body composes it — fast scalar or the literal-built routed form.
    let tc = |a: &Value, b: &Value, c: &Value, eps: &Value| -> Value {
        if let (Value::Num(e), Some(pa), Some(pb), Some(pc)) = (eps, as_p2(a), as_p2(b), as_p2(c)) {
            Value::Num(tri_class_2d(pa, pb, pc, *e))
        } else {
            tri_class_val(&build_vector(vec![a.clone(), b.clone(), c.clone()]), eps)
        }
    };
    // `_is_at_left(pt,[la,lb],eps)` as the body composes it.
    let left = |pt: &Value, la: &Value, lb: &Value, eps: &Value| -> Value {
        if let (Value::Num(e), Some(p), Some(a), Some(b)) = (eps, as_p2(pt), as_p2(la), as_p2(lb)) {
            Value::Bool(tri_class_2d(p, a, b, *e) <= 0.0)
        } else {
            is_at_left_val(pt, &build_vector(vec![la.clone(), lb.clone()]), eps)
        }
    };

    loop {
        let ll = builtins::apply("len", std::slice::from_ref(&idxs));
        if ops::apply_binary(BinOp::Ge, i.clone(), ll).is_truthy() {
            return Ok(Value::Bool(true));
        }
        let vert = ops::index(poly.clone(), &ops::index(idxs.clone(), &i));
        let prev = ops::index(
            poly.clone(),
            &select(&[
                idxs.clone(),
                ops::apply_binary(BinOp::Sub, i.clone(), Value::Num(1.0)),
            ])?,
        );
        let next = ops::index(
            poly.clone(),
            &select(&[
                idxs.clone(),
                ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0)),
            ])?,
        );
        // reflex && (inside-the-ear || touches-p1-and-crosses) ? false : next i — short-circuits preserved.
        let reflex = ops::apply_binary(BinOp::Le, tc(&prev, &vert, &next, &eps), Value::Num(0.0));
        if reflex.is_truthy() {
            let inside = ops::apply_binary(BinOp::Gt, tc(&p0, &p1, &vert, &eps), Value::Num(0.0))
                .is_truthy()
                && ops::apply_binary(BinOp::Gt, tc(&p1, &p2, &vert, &eps), Value::Num(0.0))
                    .is_truthy()
                && ops::apply_binary(BinOp::Ge, tc(&p2, &p0, &vert, &eps), Value::Num(0.0))
                    .is_truthy();
            let blocking = inside || {
                let d = ops::apply_binary(BinOp::Sub, vert.clone(), p1.clone());
                ops::apply_binary(
                    BinOp::Lt,
                    builtins::apply("norm", std::slice::from_ref(&d)),
                    eps.clone(),
                )
                .is_truthy()
                    && left(&p0, &prev, &p1, &eps).is_truthy()
                    && left(&p2, &p1, &prev, &eps).is_truthy()
                    && left(&p2, &p1, &next, &eps).is_truthy()
                    && left(&p0, &next, &p1, &eps).is_truthy()
            };
            if blocking {
                return Ok(Value::Bool(false));
            }
        }
        let next_i = ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
        if no_progress(&i, &next_i) {
            return Err(non_terminating("_none_inside"));
        }
        i = next_i;
    }
}

/// BOSL2 `_get_ear(poly, ind, eps, _i=0)` — the ear-cut driver's per-candidate scan: the first `_i` whose
/// fan triangle is convex and empty ([`tri_class_val`] + the native [`none_inside`], with [`select`]'s
/// slice for the exclusion window), else the whisker fallback. Tail recursion → loop with the
/// [`no_progress`] guard; the whisker lane's `idx(ind)` runs the real native (its assert raises on a
/// non-list `ind` exactly like the reference).
#[allow(
    clippy::similar_names,
    reason = "`ind`/`lind` ARE the reference's own parameter and let names"
)]
pub(super) fn get_ear(args: &[Value]) -> crate::Result<Value> {
    let poly = args.first().cloned().unwrap_or(Value::Undef);
    let ind = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Undef); // eps has NO default in the reference
    let mut i = args.get(3).cloned().unwrap_or(Value::Num(0.0));
    let at = |k: &Value| ops::index(poly.clone(), &ops::index(ind.clone(), k));
    loop {
        let lind = builtins::apply("len", std::slice::from_ref(&ind));
        if ops::apply_binary(BinOp::Eq, lind.clone(), Value::Num(3.0)).is_truthy() {
            return Ok(Value::Num(0.0));
        }
        let wrap = |off: f64| {
            ops::apply_binary(
                BinOp::Mod,
                ops::apply_binary(BinOp::Add, i.clone(), Value::Num(off)),
                lind.clone(),
            )
        };
        let p0 = at(&i);
        let p1 = at(&wrap(1.0));
        let p2 = at(&wrap(2.0));
        let tri = build_vector(vec![p0.clone(), p1.clone(), p2.clone()]);
        if ops::apply_binary(BinOp::Gt, tri_class_val(&tri, &eps), Value::Num(0.0)).is_truthy() {
            let window = select(&[
                ind.clone(),
                ops::apply_binary(BinOp::Add, i.clone(), Value::Num(2.0)),
                i.clone(),
            ])?;
            if none_inside(&[window, poly.clone(), p0, p1, p2, eps.clone()])?.is_truthy() {
                return Ok(i);
            }
        }
        if ops::apply_binary(
            BinOp::Lt,
            i.clone(),
            ops::apply_binary(BinOp::Sub, lind.clone(), Value::Num(1.0)),
        )
        .is_truthy()
        {
            let next = ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
            if no_progress(&i, &next) {
                return Err(non_terminating("_get_ear"));
            }
            i = next;
            continue;
        }
        // whiskers: adjacent-but-one vertices closer than eps
        let jrange = idx(std::slice::from_ref(&ind))?;
        let mut ws: Vec<Value> = Vec::new();
        for j in iter_values_raw(&jrange) {
            let far = ops::apply_binary(
                BinOp::Mod,
                ops::apply_binary(BinOp::Add, j.clone(), Value::Num(2.0)),
                lind.clone(),
            );
            let d = ops::apply_binary(BinOp::Sub, at(&j), at(&far));
            if ops::apply_binary(
                BinOp::Lt,
                builtins::apply("norm", std::slice::from_ref(&d)),
                eps.clone(),
            )
            .is_truthy()
            {
                ws.push(j);
            }
        }
        let wsv = build_vector(ws);
        return Ok(
            if ops::apply_binary(BinOp::Eq, wsv.clone(), build_vector(Vec::new())).is_truthy() {
                Value::Undef
            } else {
                build_vector(vec![ops::index(wsv, &Value::Num(0.0))])
            },
        );
    }
}

/// BOSL2 `_point_dist(path, pathseg_unit, pathseg_len, pt)` — min distance from `pt` to a precomputed
/// segment chain; `offset()`'s inner scan (4.9s/1770 calls in `shoe_holder` — ~10 elements per call ×
/// interpreted let-chains). Fully routed: dots through `apply_binary` (the 4-lane `ops::dot`), the final
/// reduction through the real `min` builtin, the wraparound neighbor through the native [`select`] (its
/// assert raises exactly like the reference on a degenerate `i+1`).
pub(super) fn point_dist(args: &[Value]) -> crate::Result<Value> {
    let path = args.first().cloned().unwrap_or(Value::Undef);
    let unit = args.get(1).cloned().unwrap_or(Value::Undef);
    let seg_len = args.get(2).cloned().unwrap_or(Value::Undef);
    let pt = args.get(3).cloned().unwrap_or(Value::Undef);
    let ll = builtins::apply("len", std::slice::from_ref(&unit));
    let end = ops::apply_binary(BinOp::Sub, ll, Value::Num(1.0));
    let range = build_range(&Value::Num(0.0), &Value::Num(1.0), &end);
    let mut dists: Vec<Value> = Vec::new();
    for iv in iter_values_raw(&range) {
        let pi = ops::index(path.clone(), &iv);
        let v = ops::apply_binary(BinOp::Sub, pt.clone(), pi.clone());
        let ui = ops::index(unit.clone(), &iv);
        let projection = ops::apply_binary(BinOp::Mul, v.clone(), ui.clone());
        let li = ops::index(seg_len.clone(), &iv);
        let d = if ops::apply_binary(BinOp::Lt, projection.clone(), Value::Num(0.0)).is_truthy() {
            ops::apply_binary(BinOp::Sub, pt.clone(), pi)
        } else if ops::apply_binary(BinOp::Gt, projection.clone(), li).is_truthy() {
            let next = select(&[
                path.clone(),
                ops::apply_binary(BinOp::Add, iv.clone(), Value::Num(1.0)),
            ])?;
            ops::apply_binary(BinOp::Sub, pt.clone(), next)
        } else {
            ops::apply_binary(BinOp::Sub, v, ops::apply_binary(BinOp::Mul, projection, ui))
        };
        dists.push(builtins::apply("norm", std::slice::from_ref(&d)));
    }
    let list = build_vector(dists);
    Ok(builtins::apply("min", std::slice::from_ref(&list)))
}

/// BOSL2 `_is_point_on_line(point, line, bounded=false, eps=_EPSILON)` — collinearity within tolerance,
/// optionally clamped to the segment on either end (`bounded` goes through the real [`force_list`]). The
/// 2D/3D split (`abs(cross)` vs `norm(cross)`) and the `t` parameter all route through ops.
pub(super) fn is_point_on_line(args: &[Value]) -> crate::Result<Value> {
    let point = args.first().cloned().unwrap_or(Value::Undef);
    let line = args.get(1).cloned().unwrap_or(Value::Undef);
    let bounded = args.get(2).cloned().unwrap_or(Value::Bool(false));
    let eps = args.get(3).cloned().unwrap_or(Value::Num(1e-9));
    let l0 = ops::index(line.clone(), &Value::Num(0.0));
    let l1 = ops::index(line, &Value::Num(1.0));
    let v1 = ops::apply_binary(BinOp::Sub, l1, l0.clone());
    let v0 = ops::apply_binary(BinOp::Sub, point, l0);
    let t = ops::apply_binary(
        BinOp::Div,
        ops::apply_binary(BinOp::Mul, v0.clone(), v1.clone()),
        ops::apply_binary(BinOp::Mul, v1.clone(), v1.clone()),
    );
    let bounded2 = force_list(&[bounded, Value::Num(2.0)])?;
    let crx = builtins::apply("cross", &[v0, v1.clone()]);
    let ncp = if ops::apply_binary(
        BinOp::Eq,
        builtins::apply("len", std::slice::from_ref(&v1)),
        Value::Num(2.0),
    )
    .is_truthy()
    {
        builtins::apply("abs", std::slice::from_ref(&crx))
    } else {
        builtins::apply("norm", std::slice::from_ref(&crx))
    };
    let on_line = ops::apply_binary(
        BinOp::Le,
        ncp,
        ops::apply_binary(
            BinOp::Mul,
            eps.clone(),
            builtins::apply("norm", std::slice::from_ref(&v1)),
        ),
    );
    if !on_line.is_truthy() {
        return Ok(Value::Bool(false));
    }
    if ops::index(bounded2.clone(), &Value::Num(0.0)).is_truthy()
        && !ops::apply_binary(
            BinOp::Ge,
            t.clone(),
            ops::apply_unary(crate::parser::UnOp::Neg, eps.clone()),
        )
        .is_truthy()
    {
        return Ok(Value::Bool(false));
    }
    if ops::index(bounded2, &Value::Num(1.0)).is_truthy()
        && !ops::apply_binary(
            BinOp::Lt,
            t,
            ops::apply_binary(BinOp::Add, Value::Num(1.0), eps),
        )
        .is_truthy()
    {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(true))
}

/// The [`PINS`]' `is_vnf(x)` as [`vnf_centroid`]'s assert needs it, composed from the band's own natives
/// (`is_vector(x[0][0], 3)` / `is_vector(x[1][0])`).
pub(super) fn is_vnf_check(x: &Value) -> crate::Result<bool> {
    if !v_is_list(x) {
        return Ok(false);
    }
    let ll = builtins::apply("len", std::slice::from_ref(x));
    if !ops::apply_binary(BinOp::Eq, ll, Value::Num(2.0)).is_truthy() {
        return Ok(false);
    }
    let x0 = ops::index(x.clone(), &Value::Num(0.0));
    let x1 = ops::index(x.clone(), &Value::Num(1.0));
    if !(v_is_list(&x0) && v_is_list(&x1)) {
        return Ok(false);
    }
    let empty = build_vector(Vec::new());
    let verts_ok = ops::apply_binary(BinOp::Eq, x0.clone(), empty.clone()).is_truthy()
        || (ops::apply_binary(
            BinOp::Ge,
            builtins::apply("len", std::slice::from_ref(&x0)),
            Value::Num(3.0),
        )
        .is_truthy()
            && is_vector(&[ops::index(x0.clone(), &Value::Num(0.0)), Value::Num(3.0)])?
                .is_truthy());
    if !verts_ok {
        return Ok(false);
    }
    Ok(ops::apply_binary(BinOp::Eq, x1.clone(), empty).is_truthy()
        || is_vector(std::slice::from_ref(&ops::index(x1, &Value::Num(0.0))))?.is_truthy())
}

/// BOSL2 `_vnf_centroid(vnf, eps=_EPSILON)` — the volume-weighted centroid: per face-fan triangle,
/// `vol = cross(v2,v1)*v0` and the running `[vol, (v0+v1+v2)*vol]` pairs sum through the REAL [`sum`]
/// entry (its `_sum` lane — the summands are [scalar, vector] pairs), then `approx(pos[0], 0, eps)` guards
/// self-intersection. 1.9s/30 calls in `webcam_holder` — the fan loop over every face, interpreted.
pub(super) fn vnf_centroid(args: &[Value]) -> crate::Result<Value> {
    let vnf = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    let verts = ops::index(vnf.clone(), &Value::Num(0.0));
    let faces = ops::index(vnf.clone(), &Value::Num(1.0));
    let nonzero = |v: &Value| {
        !ops::apply_binary(
            BinOp::Eq,
            builtins::apply("len", std::slice::from_ref(v)),
            Value::Num(0.0),
        )
        .is_truthy()
    };
    if !(is_vnf_check(&vnf)? && nonzero(&verts) && nonzero(&faces)) {
        return Err(bosl_assert("_vnf_centroid: invalid or empty VNF"));
    }
    let mut pairs: Vec<Value> = Vec::new();
    for face in iter_values_raw(&faces) {
        let jr = build_range(
            &Value::Num(1.0),
            &Value::Num(1.0),
            &ops::apply_binary(
                BinOp::Sub,
                builtins::apply("len", std::slice::from_ref(&face)),
                Value::Num(2.0),
            ),
        );
        for j in iter_values_raw(&jr) {
            let vat = |idx: &Value| ops::index(verts.clone(), &ops::index(face.clone(), idx));
            let v0 = vat(&Value::Num(0.0));
            let v1 = vat(&j);
            let v2 = vat(&ops::apply_binary(BinOp::Add, j.clone(), Value::Num(1.0)));
            let vol = ops::apply_binary(
                BinOp::Mul,
                builtins::apply("cross", &[v2.clone(), v1.clone()]),
                v0.clone(),
            );
            let centroid_part = ops::apply_binary(
                BinOp::Mul,
                ops::apply_binary(BinOp::Add, ops::apply_binary(BinOp::Add, v0, v1), v2),
                vol.clone(),
            );
            pairs.push(build_vector(vec![vol, centroid_part]));
        }
    }
    let pos = sum(&[build_vector(pairs)])?;
    let p0 = ops::index(pos.clone(), &Value::Num(0.0));
    if approx(&[p0.clone(), Value::Num(0.0), eps])?.is_truthy() {
        return Err(bosl_assert("_vnf_centroid: the vnf has self-intersections"));
    }
    Ok(ops::apply_binary(
        BinOp::Div,
        ops::apply_binary(BinOp::Div, ops::index(pos, &Value::Num(1.0)), p0),
        Value::Num(4.0),
    ))
}

/// BOSL2 `point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]]` — pad/truncate a
/// point to 3 coords. A non-list RAISES (the inline assert; the message is a locator, so the harness matches
/// on "both errored", not the text). Each coord replicates the reference ternary through the REAL `==`
/// (`undef==undef` is true → an out-of-range slot takes `fill`) and `is_truthy`, then `build_vector` coalesces
/// exactly as the interpreter does (all-numeric → `NumList`, else `List`). `fill` defaults to `0` (1-arg call).
pub(super) fn point3d(args: &[Value]) -> crate::Result<Value> {
    let p = args.first().cloned().unwrap_or(Value::Undef);
    if !matches!(p, Value::List(_) | Value::NumList(_)) {
        // Error::Assert (not Eval): this mirrors the interpreted BOSL2 `assert(is_list(p))`, so it must
        // halt-and-export like a user assert (L.5.8), identical to the function it replaces.
        return Err(crate::Error::Assert(
            "assertion failed [assert(is_list(p))]".to_string(),
        ));
    }
    let fill = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let coords = (0..3)
        .map(|i| {
            let pi = ops::index(p.clone(), &Value::Num(f64::from(i)));
            if ops::apply_binary(BinOp::Eq, pi.clone(), Value::Undef).is_truthy() {
                fill.clone()
            } else {
                pi
            }
        })
        .collect();
    Ok(build_vector(coords))
}

/// BOSL2 `point2d(p, fill=0)` — force a point to 2 coords; [`point3d`]'s exact shape, one slot shorter.
pub(super) fn point2d(args: &[Value]) -> crate::Result<Value> {
    let p = args.first().cloned().unwrap_or(Value::Undef);
    if !v_is_list(&p) {
        return Err(bosl_assert("point2d: p must be a list"));
    }
    let fill = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let coords = (0..2)
        .map(|i| {
            let pi = ops::index(p.clone(), &Value::Num(f64::from(i)));
            if ops::apply_binary(BinOp::Eq, pi.clone(), Value::Undef).is_truthy() {
                fill.clone()
            } else {
                pi
            }
        })
        .collect();
    Ok(build_vector(coords))
}
