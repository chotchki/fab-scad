//! O.10 — the region-monster band's dependency tier: the small BOSL2 list/geometry helpers
//! `_region_region_intersections` reaches (`list_wrap`, `_general_line_intersection`, `flatten`,
//! `column`, `count`, `mean`, `min_index`/`max_index`, `transpose`, `pointlist_bounds`,
//! `_sort_vectors`). Every value-level helper
//! here replicates its PINNED reference exactly, routing arithmetic/indexing/builtins through the
//! interpreter's own primitives (`ops::*`, `builtins::apply`) so exotic inputs — undef, ragged rows,
//! NaN, `-0.0` — degrade bit-identically to interpreting the reference. The fast==slow batteries in
//! `tests.rs` are the proof; the registry entry lands with `_rri` itself (O.10c).

use super::math::{approx_val, sum};
use super::{bosl_assert, non_terminating, v_is_list};
use crate::eval::value::Value;
use crate::eval::{build_vector, builtins, ops};
use crate::parser::BinOp;

/// BOSL2 `list_wrap(list, eps=_EPSILON)` — close an open path by repeating its first point, unless the
/// ends already match (`are_ends_equal`) or the list is shorter than 2.
pub(super) fn list_wrap_val(list: &Value, eps: &Value) -> crate::Result<Value> {
    let Value::Bool(true) = builtins::apply("is_list", std::slice::from_ref(list)) else {
        return Err(bosl_assert("list_wrap: not a list"));
    };
    // assert(is_finite(eps) && eps>=0)
    let finite = matches!(eps, Value::Num(e) if e.is_finite());
    if !finite || ops::apply_binary(BinOp::Lt, eps.clone(), Value::Num(0.0)).is_truthy() {
        return Err(bosl_assert("list_wrap: invalid eps"));
    }
    let n = match list {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0,
    };
    if n < 2 || are_ends_equal_val(list, eps)?.is_truthy() {
        return Ok(list.clone());
    }
    // [each list, list[0]] — element order preserved, variant re-coalesced like the interpreter's
    // `build_vector` (an all-Num wrap stays a NumList).
    let mut out: Vec<Value> = match list {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        // Can't happen (is_list held above); the input unchanged is the no-panic fallback.
        _ => return Ok(list.clone()),
    };
    out.push(ops::index(list.clone(), &Value::Num(0.0)));
    Ok(build_vector(out))
}

/// BOSL2 `are_ends_equal(list, eps=_EPSILON)` — `approx(list[0], list[len-1], eps)` behind a
/// nonempty-list assert.
pub(super) fn are_ends_equal_val(list: &Value, eps: &Value) -> crate::Result<Value> {
    let n = match list {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("are_ends_equal: must give a nonempty list")),
    };
    if n == 0 {
        return Err(bosl_assert("are_ends_equal: must give a nonempty list"));
    }
    let first = ops::index(list.clone(), &Value::Num(0.0));
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the interpreter indexes with the same f64"
    )]
    let final_elem = ops::index(list.clone(), &Value::Num((n - 1) as f64));
    approx_val(&first, &final_elem, eps)
}

/// BOSL2 `_general_line_intersection(s1, s2, eps=_EPSILON)` — the 2D segment/segment intersection:
/// `undef` on (near-)parallel, else `[point, t, u]`. Every cross/sub/mul routes through ops (`cross`
/// on 2D vectors is the scalar `a.x*b.y - a.y*b.x` builtin).
pub(super) fn gli_val(s1: &Value, s2: &Value, eps: &Value) -> crate::Result<Value> {
    let p = |v: &Value, i: f64| ops::index(v.clone(), &Value::Num(i));
    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    let denominator = builtins::apply(
        "cross",
        &[sub(p(s1, 0.0), p(s1, 1.0)), sub(p(s2, 0.0), p(s2, 1.0))],
    );
    if approx_val(&denominator, &Value::Num(0.0), eps)?.is_truthy() {
        return Ok(Value::Undef);
    }
    let t = ops::apply_binary(
        BinOp::Div,
        builtins::apply(
            "cross",
            &[sub(p(s1, 0.0), p(s2, 0.0)), sub(p(s2, 0.0), p(s2, 1.0))],
        ),
        denominator.clone(),
    );
    let u = ops::apply_binary(
        BinOp::Div,
        builtins::apply(
            "cross",
            &[sub(p(s1, 0.0), p(s2, 0.0)), sub(p(s1, 0.0), p(s1, 1.0))],
        ),
        denominator,
    );
    // s1[0] + t*(s1[1]-s1[0])
    let pt = ops::apply_binary(
        BinOp::Add,
        p(s1, 0.0),
        ops::apply_binary(BinOp::Mul, t.clone(), sub(p(s1, 1.0), p(s1, 0.0))),
    );
    Ok(build_vector(vec![pt, t, u]))
}

/// BOSL2 `flatten(l)` — ONE level: a non-list passes through; list elements splice if they are lists,
/// else ride along.
pub(super) fn flatten_val(l: &Value) -> crate::Result<Value> {
    if !v_is_list(l) {
        return Ok(l.clone());
    }
    let mut out = Vec::new();
    match l {
        // A NumList's elements are Nums — never lists — so flatten is the identity on its elements.
        Value::NumList(xs) => out.extend(xs.iter().map(|&x| Value::Num(x))),
        Value::List(xs) => {
            for a in xs.iter() {
                match a {
                    Value::NumList(inner) => out.extend(inner.iter().map(|&x| Value::Num(x))),
                    Value::List(inner) => out.extend(inner.iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
        }
        // Can't happen (v_is_list held above); an unflattened passthrough is the no-panic fallback.
        _ => return Ok(l.clone()),
    }
    Ok(build_vector(out))
}

/// BOSL2 `column(M, i)` — `[for(row=M) row[i]]` behind list/index asserts. Row indexing routes through
/// `ops::index` so a short row yields `undef` exactly as interpreted.
pub(super) fn column_val(m: &Value, i: &Value) -> crate::Result<Value> {
    if !v_is_list(m) {
        return Err(bosl_assert("column: the input is not a list"));
    }
    let is_int = matches!(i, Value::Num(x) if x.fract() == 0.0 && x.is_finite());
    if !is_int || ops::apply_binary(BinOp::Lt, i.clone(), Value::Num(0.0)).is_truthy() {
        return Err(bosl_assert("column: invalid index"));
    }
    let rows: Vec<Value> = match m {
        Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
        Value::List(xs) => xs.to_vec(),
        // Can't happen (v_is_list held above); the assert is the no-panic fallback.
        _ => return Err(bosl_assert("column: the input is not a list")),
    };
    Ok(build_vector(
        rows.into_iter().map(|row| ops::index(row, i)).collect(),
    ))
}

/// BOSL2 `count(n, s=0, step=1, reverse=false)` — `[for(i=[0:1:n-1]) s+i*step]` (or the reversed
/// range). `n` may be a LIST (its len) or any number — fractional/negative `n` must produce exactly
/// the interpreter's range expansion, so the ranges go through [`crate::eval::value::range_iter`].
pub(super) fn count_val(
    n: &Value,
    s: &Value,
    step: &Value,
    reverse: &Value,
) -> crate::Result<Value> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the reference's len(n) is the same f64"
    )]
    let n = match n {
        Value::NumList(xs) => Value::Num(xs.len() as f64),
        Value::List(xs) => Value::Num(xs.len() as f64),
        other => other.clone(),
    };
    let one = Value::Num(1.0);
    let (start, rstep, end) = if reverse.is_truthy() {
        // [n-1 : -1 : 0]
        (
            ops::apply_binary(BinOp::Sub, n, one),
            Value::Num(-1.0),
            Value::Num(0.0),
        )
    } else {
        // [0 : 1 : n-1]
        (
            Value::Num(0.0),
            one.clone(),
            ops::apply_binary(BinOp::Sub, n, one),
        )
    };
    // Non-numeric bounds can't occur on the band's inputs (`n` is always a real length); a NaN bound
    // makes `range_iter` yield nothing, which is also what the interpreter's range does off-domain.
    let as_f = |v: &Value| match v {
        Value::Num(x) => *x,
        _ => f64::NAN,
    };
    let mut out = Vec::new();
    for i in crate::eval::value::range_iter(as_f(&start), as_f(&rstep), as_f(&end)) {
        out.push(ops::apply_binary(
            BinOp::Add,
            s.clone(),
            ops::apply_binary(BinOp::Mul, Value::Num(i), step.clone()),
        ));
    }
    Ok(build_vector(out))
}

/// BOSL2 `mean(v)` — `sum(v)/len(v)` behind a nonempty-list assert; `sum` is the existing native
/// (same reference the `sum` entry pins).
pub(super) fn mean_val(v: &Value) -> crate::Result<Value> {
    let n = match v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("mean: invalid list")),
    };
    if n == 0 {
        return Err(bosl_assert("mean: invalid list"));
    }
    let total = sum(std::slice::from_ref(v))?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "list lengths are far below 2^52; the reference divides by the same f64 len"
    )]
    Ok(ops::apply_binary(BinOp::Div, total, Value::Num(n as f64)))
}

/// BOSL2 `min_index(vals)` / `max_index(vals)` (the 1-arg `all=false` shape the band reaches):
/// `search(min(vals), vals)[0]` — extremum + search through the real builtins, index through ops.
pub(super) fn min_index_val(vals: &Value) -> crate::Result<Value> {
    extremum_index(vals, "min")
}

pub(super) fn max_index_val(vals: &Value) -> crate::Result<Value> {
    extremum_index(vals, "max")
}

fn extremum_index(vals: &Value, which: &'static str) -> crate::Result<Value> {
    if !super::is_vector_core(vals) {
        return Err(bosl_assert("min_index/max_index: invalid list of numbers"));
    }
    // max_index additionally asserts len>0 (min_index's reference does not, but search on an empty
    // list yields [] and [0] indexes undef — identical either way for the shapes the band reaches).
    let ext = builtins::apply(which, std::slice::from_ref(vals));
    let found = builtins::apply("search", &[ext, vals.clone()]);
    Ok(ops::index(found, &Value::Num(0.0)))
}

/// BOSL2 `transpose(M)` (the 1-arg `reverse=false` shape): rows↔columns behind the rectangularity
/// assert; a plain vector passes through.
pub(super) fn transpose_val(m: &Value) -> crate::Result<Value> {
    let nonempty = match m {
        Value::NumList(xs) => !xs.is_empty(),
        Value::List(xs) => !xs.is_empty(),
        _ => false,
    };
    if !nonempty {
        return Err(bosl_assert("transpose: input must be a nonempty list"));
    }
    let first = ops::index(m.clone(), &Value::Num(0.0));
    if !v_is_list(&first) {
        if !super::is_vector_core(m) {
            return Err(bosl_assert(
                "transpose: input must be a vector or list of lists",
            ));
        }
        return Ok(m.clone());
    }
    let rows: Vec<Value> = match m {
        Value::List(xs) => xs.to_vec(),
        // A NumList's rows are Nums, but `first` was a list — unreachable; keep the exact fallback.
        _ => return Err(bosl_assert("transpose: input has inconsistent row lengths")),
    };
    let len0 = match &first {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        // Can't happen (`first` proved a list above); the assert is the no-panic fallback.
        _ => return Err(bosl_assert("transpose: input has inconsistent row lengths")),
    };
    for row in &rows {
        let ok = match row {
            Value::NumList(xs) => xs.len() == len0,
            Value::List(xs) => xs.len() == len0,
            _ => false,
        };
        if !ok {
            return Err(bosl_assert("transpose: input has inconsistent row lengths"));
        }
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "row/column indices are far below 2^52; the interpreter indexes with the same f64"
    )]
    let out: Vec<Value> = (0..len0)
        .map(|i| {
            build_vector(
                rows.iter()
                    .map(|row| ops::index(row.clone(), &Value::Num(i as f64)))
                    .collect(),
            )
        })
        .collect();
    Ok(build_vector(out))
}

/// BOSL2 `pointlist_bounds(pts)` — per-axis `[min, max]` spread, then transpose to `[mins, maxs]`.
/// The reference projects with `pts * ident(dim)[i]` — a matrix×basis-vector product whose per-row
/// dot is the interpreter's 4-LANED `ops::dot` (sign-of-zero differs from naive column extraction:
/// `-0.0` coordinates come out `+0.0` through the lane sum) — so the projection goes through
/// `ops::apply_binary(Mul, …)`, never a hand-rolled column read.
pub(super) fn pointlist_bounds_val(pts: &Value) -> crate::Result<Value> {
    let fast_path = super::shape::is_path(&[pts.clone(), Value::Undef, Value::Bool(true)])?;
    if !fast_path.is_truthy() {
        return Err(bosl_assert("pointlist_bounds: invalid pointlist"));
    }
    let first = ops::index(pts.clone(), &Value::Num(0.0));
    let dim = match &first {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => return Err(bosl_assert("pointlist_bounds: invalid pointlist")),
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "point dimensions are tiny; the reference builds the same f64 identity"
    )]
    let ident_rows: Vec<Value> = (0..dim)
        .map(|i| {
            build_vector(
                (0..dim)
                    .map(|j| Value::Num(if i == j { 1.0 } else { 0.0 }))
                    .collect(),
            )
        })
        .collect();
    let spread: Vec<Value> = ident_rows
        .into_iter()
        .map(|basis| {
            let spreadi = ops::apply_binary(BinOp::Mul, pts.clone(), basis);
            build_vector(vec![
                builtins::apply("min", std::slice::from_ref(&spreadi)),
                builtins::apply("max", std::slice::from_ref(&spreadi)),
            ])
        })
        .collect();
    transpose_val(&build_vector(spread))
}

/// BOSL2 `_sort_vectors(arr, idxlist, _i=0)` — the lexicographic 3-way quicksort (comparisons.scad
/// defines the name TWICE; last-wins makes THIS 3-param form the effective one — the wire check
/// caught the first-form pin as drift). Column `k` is `idxlist[_i]` when an idxlist rides along,
/// else `_i`; partition on `k`'s pivot, recurse `(lesser, _i)`, `(equal, _i+1)`, `(greater, _i)`,
/// concatenate in that order. Ported ITERATIVELY (explicit work stack, output appended in-order) —
/// BOSL2's recursion on a big intersection set would ride the Rust stack otherwise. All comparisons
/// route through ops so ragged/exotic rows order exactly as interpreted.
pub(super) fn sort_vectors_val(arr: &Value, idxlist: &Value) -> crate::Result<Value> {
    // Work item: sort THIS run starting at column `i`, appending its rows to the output in order.
    let mut out: Vec<Value> = Vec::new();
    let mut stack: Vec<(Vec<Value>, usize)> = vec![(value_rows(arr)?, 0)];
    let mut steps = 0usize;
    while let Some((rows, i)) = stack.pop() {
        // The interpreter stops runaway recursion only at its step budget; the native must not hang.
        steps += 1;
        if steps > 10_000_000 {
            return Err(non_terminating("_sort_vectors"));
        }
        // len(arr)<=1 → emit as-is. `_i >= len(arr[0])` on a NON-list first row is `>= undef` →
        // false in the interpreter → its recursion never terminates; LOUD here instead.
        if rows.len() <= 1 {
            out.extend(rows);
            continue;
        }
        // ( is_list(idxlist) && _i>=len(idxlist) ) — the idxlist-exhausted emit.
        if let Some(il) = list_len(idxlist)
            && i >= il
        {
            out.extend(rows);
            continue;
        }
        let width = match rows.first() {
            Some(Value::NumList(xs)) => Some(xs.len()),
            Some(Value::List(xs)) => Some(xs.len()),
            _ => None,
        };
        match width {
            Some(w) if i >= w => {
                out.extend(rows);
                continue;
            }
            None => return Err(non_terminating("_sort_vectors: non-list row")),
            _ => {}
        }
        // k = is_list(idxlist) ? idxlist[_i] : _i — the sort column for THIS depth.
        #[allow(
            clippy::cast_precision_loss,
            reason = "column depth is tiny; the reference indexes idxlist[_i] with the same f64"
        )]
        let k = if list_len(idxlist).is_some() {
            ops::index(idxlist.clone(), &Value::Num(i as f64))
        } else {
            Value::Num(i as f64)
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "row counts are far below 2^52; the reference indexes the same"
        )]
        let pivot = ops::index(
            ops::index(
                build_vector(rows.clone()),
                &Value::Num((rows.len() as f64 / 2.0).floor()),
            ),
            &k,
        );
        let (mut lesser, mut equal, mut greater) = (Vec::new(), Vec::new(), Vec::new());
        for row in rows {
            let cell = ops::index(row.clone(), &k);
            if ops::apply_binary(BinOp::Lt, cell.clone(), pivot.clone()).is_truthy() {
                lesser.push(row);
            } else if ops::apply_binary(BinOp::Eq, cell.clone(), pivot.clone()).is_truthy() {
                equal.push(row);
            } else if ops::apply_binary(BinOp::Gt, cell, pivot.clone()).is_truthy() {
                greater.push(row);
            }
            // A row incomparable to the pivot (NaN cell, type mismatch) lands in NO partition —
            // exactly the reference's three-filter behavior (the row is silently dropped).
        }
        // LIFO: push greater first so lesser pops (and emits) first — concat order preserved.
        stack.push((greater, i));
        stack.push((equal, i + 1));
        stack.push((lesser, i));
    }
    Ok(build_vector(out))
}

/// A list's length, or `None` for a non-list — the reference's `is_list(x) ? len(x) : …` shape.
fn list_len(v: &Value) -> Option<usize> {
    match v {
        Value::NumList(xs) => Some(xs.len()),
        Value::List(xs) => Some(xs.len()),
        _ => None,
    }
}

/// A list Value's elements as owned rows (the sort's working form).
fn value_rows(v: &Value) -> crate::Result<Vec<Value>> {
    match v {
        Value::NumList(xs) => Ok(xs.iter().map(|&x| Value::Num(x)).collect()),
        Value::List(xs) => Ok(xs.to_vec()),
        // The reference's len(arr) on a non-list is undef → `len(arr)<=1` false → non-terminating.
        _ => Err(non_terminating("_sort_vectors: non-list input")),
    }
}

/// BOSL2 `_bt_tree(points, ind, leafsize=25)` — the ball-tree builder behind `vector_search`'s
/// over-400-point branch: a leaf is `[ind]`; a node is `[pivot, radius, Ltree, Rtree]` split on the
/// widest-spread coordinate about its mean. Ported ITERATIVELY (slot arena + build/assemble work
/// stack) — the reference recurses per split, and a degenerate point set makes that O(n) deep. Every
/// projection/comparison routes through ops; `max_index`/`min_index`/`mean`/`pointlist_bounds` are
/// the band's own natives, `select` the existing one.
#[allow(
    clippy::too_many_lines,
    reason = "one iterative work-stack mirror of one recursive reference — splitting it would \
              separate the build/assemble halves the invariant lives across"
)]
pub(super) fn bt_tree_val(points: &Value, ind: &Value, leafsize: &Value) -> crate::Result<Value> {
    enum Work {
        Build {
            ind: Vec<Value>,
            slot: usize,
        },
        Assemble {
            pivot_val: Value,
            radius: Value,
            slot: usize,
            l: usize,
            r: usize,
        },
    }
    let mut nodes: Vec<Option<Value>> = vec![None];
    let mut stack = vec![Work::Build {
        ind: value_rows(ind)?,
        slot: 0,
    }];
    let mut steps = 0usize;
    while let Some(work) = stack.pop() {
        steps += 1;
        if steps > 10_000_000 {
            return Err(non_terminating("_bt_tree"));
        }
        match work {
            Work::Assemble {
                pivot_val,
                radius,
                slot,
                l,
                r,
            } => {
                let ltree = nodes[l].take().unwrap_or(Value::Undef);
                let rtree = nodes[r].take().unwrap_or(Value::Undef);
                nodes[slot] = Some(build_vector(vec![pivot_val, radius, ltree, rtree]));
            }
            Work::Build { ind, slot } => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "index-list lengths are far below 2^52; the reference compares the same f64 len"
                )]
                let over = match leafsize {
                    Value::Num(ls) => (ind.len() as f64) > *ls,
                    // A non-numeric leafsize makes the reference's `len(ind)<=leafsize` false → it
                    // splits forever on small sets; the band always passes 25, so fail LOUD instead.
                    _ => return Err(non_terminating("_bt_tree: non-numeric leafsize")),
                };
                if !over {
                    nodes[slot] = Some(build_vector(vec![build_vector(ind)]));
                    continue;
                }
                let ind_v = build_vector(ind.clone());
                let selected = super::lists::select(&[points.clone(), ind_v.clone()])?;
                let bounds = pointlist_bounds_val(&selected)?;
                let spread = ops::apply_binary(
                    BinOp::Sub,
                    ops::index(bounds.clone(), &Value::Num(1.0)),
                    ops::index(bounds, &Value::Num(0.0)),
                );
                let coord = max_index_val(&spread)?;
                let projc: Vec<Value> = ind
                    .iter()
                    .map(|i| ops::index(ops::index(points.clone(), i), &coord))
                    .collect();
                let projc_v = build_vector(projc.clone());
                let meanpr = mean_val(&projc_v)?;
                let deviations: Vec<Value> = projc
                    .iter()
                    .map(|p| {
                        builtins::apply(
                            "abs",
                            std::slice::from_ref(&ops::apply_binary(
                                BinOp::Sub,
                                p.clone(),
                                meanpr.clone(),
                            )),
                        )
                    })
                    .collect();
                let pivot = min_index_val(&build_vector(deviations))?;
                let pivot_pt = ops::index(points.clone(), &ops::index(ind_v.clone(), &pivot));
                let dists: Vec<Value> = ind
                    .iter()
                    .map(|i| {
                        builtins::apply(
                            "norm",
                            std::slice::from_ref(&ops::apply_binary(
                                BinOp::Sub,
                                pivot_pt.clone(),
                                ops::index(points.clone(), i),
                            )),
                        )
                    })
                    .collect();
                let radius = builtins::apply("max", std::slice::from_ref(&build_vector(dists)));
                let (mut left_ind, mut right_ind) = (Vec::new(), Vec::new());
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "positions within one index list are far below 2^52; the reference \
                              compares the same f64 loop index"
                )]
                for (i, iv) in ind.iter().enumerate() {
                    let not_pivot =
                        ops::apply_binary(BinOp::Ne, Value::Num(i as f64), pivot.clone())
                            .is_truthy();
                    if !not_pivot {
                        continue;
                    }
                    if ops::apply_binary(BinOp::Le, projc[i].clone(), meanpr.clone()).is_truthy() {
                        left_ind.push(iv.clone());
                    } else if ops::apply_binary(BinOp::Gt, projc[i].clone(), meanpr.clone())
                        .is_truthy()
                    {
                        right_ind.push(iv.clone());
                    }
                    // A NaN projection lands in NEITHER side, exactly as the reference's two filters.
                }
                let l = nodes.len();
                nodes.push(None);
                let r = nodes.len();
                nodes.push(None);
                let pivot_val = ops::index(ind_v, &pivot);
                stack.push(Work::Assemble {
                    pivot_val,
                    radius,
                    slot,
                    l,
                    r,
                });
                stack.push(Work::Build {
                    ind: right_ind,
                    slot: r,
                });
                stack.push(Work::Build {
                    ind: left_ind,
                    slot: l,
                });
            }
        }
    }
    Ok(nodes
        .first_mut()
        .and_then(Option::take)
        .unwrap_or(Value::Undef))
}

/// BOSL2 `vector_search(query, r, target)` — the dispatcher: empty-query/target short-circuits, then
/// the point-list target splits ≤400 (quadratic scan) vs >400 (ball tree via [`bt_tree_val`] +
/// the existing `_bt_search` native), and a pre-built `[points, tree]` target searches directly.
/// BOTH branches ported faithfully — they return indices in DIFFERENT orders (ascending vs tree
/// order), and downstream `search`/`select` in `_rri` see that order.
#[allow(
    clippy::too_many_lines,
    reason = "one dispatcher mirroring one reference dispatcher — the branch ladder IS the shape"
)]
pub(super) fn vector_search_val(query: &Value, r: &Value, target: &Value) -> crate::Result<Value> {
    let empty = build_vector(vec![]);
    if ops::apply_binary(BinOp::Eq, query.clone(), empty.clone()).is_truthy() {
        return Ok(empty);
    }
    if v_is_list(query) && ops::apply_binary(BinOp::Eq, target.clone(), empty.clone()).is_truthy() {
        if super::shape::is_vector(std::slice::from_ref(query))?.is_truthy() {
            return Ok(empty);
        }
        let n = value_rows(query)?.len();
        return Ok(build_vector(vec![build_vector(vec![]); n]));
    }
    let r_finite = matches!(r, Value::Num(x) if x.is_finite());
    if !r_finite || ops::apply_binary(BinOp::Lt, r.clone(), Value::Num(0.0)).is_truthy() {
        return Err(bosl_assert("vector_search: invalid radius"));
    }
    let tgpts = super::shape::is_matrix(std::slice::from_ref(target))?.is_truthy();
    let tgtree = {
        let rows = if v_is_list(target) {
            value_rows(target)?
        } else {
            Vec::new()
        };
        rows.len() == 2
            && super::shape::is_matrix(std::slice::from_ref(&rows[0]))?.is_truthy()
            && v_is_list(&rows[1])
            && {
                let t1 = value_rows(&rows[1])?;
                t1.len() == 4 || (t1.len() == 1 && v_is_list(&t1[0]))
            }
    };
    if !tgpts && !tgtree {
        return Err(bosl_assert("vector_search: invalid target"));
    }
    let dim_of = |v: &Value| -> Value { builtins::apply("len", std::slice::from_ref(v)) };
    let dim = if tgpts {
        dim_of(&ops::index(target.clone(), &Value::Num(0.0)))
    } else {
        dim_of(&ops::index(
            ops::index(target.clone(), &Value::Num(0.0)),
            &Value::Num(0.0),
        ))
    };
    let simple = super::shape::is_vector(&[query.clone(), dim.clone()])?.is_truthy();
    if !simple && !super::shape::is_matrix(&[query.clone(), Value::Undef, dim.clone()])?.is_truthy()
    {
        return Err(bosl_assert("vector_search: query incompatible with target"));
    }
    // The quadratic scan both point-list sub-branches share: indices of `target` within `r` of `q`.
    let scan = |q: &Value| -> crate::Result<Value> {
        let rows = value_rows(target)?;
        let mut hits = Vec::new();
        #[allow(
            clippy::cast_precision_loss,
            reason = "point counts are far below 2^52; idx(target) iterates the same f64 indices"
        )]
        for (i, pt) in rows.iter().enumerate() {
            let d = builtins::apply(
                "norm",
                std::slice::from_ref(&ops::apply_binary(BinOp::Sub, pt.clone(), q.clone())),
            );
            if ops::apply_binary(BinOp::Le, d, r.clone()).is_truthy() {
                hits.push(Value::Num(i as f64));
            }
        }
        Ok(build_vector(hits))
    };
    if tgpts {
        let n = value_rows(target)?.len();
        if n <= 400 {
            return if simple {
                scan(query)
            } else {
                let out: crate::Result<Vec<Value>> = value_rows(query)?.iter().map(&scan).collect();
                Ok(build_vector(out?))
            };
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "point counts are far below 2^52; the reference's count(len(target)) is the same"
        )]
        let ind = count_val(
            &Value::Num(n as f64),
            &Value::Num(0.0),
            &Value::Num(1.0),
            &Value::Bool(false),
        )?;
        let tree = bt_tree_val(target, &ind, &Value::Num(25.0))?;
        return if simple {
            super::vectors::bt_search(&[query.clone(), r.clone(), target.clone(), tree])
        } else {
            let out: crate::Result<Vec<Value>> = value_rows(query)?
                .iter()
                .map(|q| {
                    super::vectors::bt_search(&[q.clone(), r.clone(), target.clone(), tree.clone()])
                })
                .collect();
            Ok(build_vector(out?))
        };
    }
    // tgtree: target = [points, tree].
    let points = ops::index(target.clone(), &Value::Num(0.0));
    let tree = ops::index(target.clone(), &Value::Num(1.0));
    if simple {
        super::vectors::bt_search(&[query.clone(), r.clone(), points, tree])
    } else {
        let out: crate::Result<Vec<Value>> = value_rows(query)?
            .iter()
            .map(|q| {
                super::vectors::bt_search(&[q.clone(), r.clone(), points.clone(), tree.clone()])
            })
            .collect();
        Ok(build_vector(out?))
    }
}

/// BOSL2 `_region_region_intersections(region1, region2, closed1=true, closed2=true, eps=_EPSILON)`
/// — THE region monster (O.10c): for every region1 path edge, the sign-partition prefilter against
/// every region2 poly, exact `_general_line_intersection` on the surviving edge pairs; then corner
/// points (self-touch duplicates via `vector_search`), the per-path grouping (`search` builtin), and
/// the lexicographic `_sort_vectors` finish. Structure mirrors the reference comprehension-for-
/// comprehension; every product/compare routes through ops (`a1*seg_normal` and `poly*seg_normal`
/// are the interpreter's 4-LANED dots — never hand math).
#[allow(
    clippy::too_many_lines,
    reason = "the 61-line reference comprehension ported clause-for-clause; splitting would decouple the \
              sign-prefilter from the intersection loop it guards"
)]
pub(super) fn rri_val(args: &[Value]) -> crate::Result<Value> {
    let region1 = args.first().cloned().unwrap_or(Value::Undef);
    let region2 = args.get(1).cloned().unwrap_or(Value::Undef);
    let closed1 = args.get(2).cloned().unwrap_or(Value::Bool(true));
    let closed2 = args.get(3).cloned().unwrap_or(Value::Bool(true));
    let eps = args.get(4).cloned().unwrap_or(Value::Num(1e-9));

    let sub = |a: Value, b: Value| ops::apply_binary(BinOp::Sub, a, b);
    let idx_num = |i: usize| {
        #[allow(
            clippy::cast_precision_loss,
            reason = "path/point counts are far below 2^52; the reference indexes the same f64"
        )]
        Value::Num(i as f64)
    };

    // The intersections comprehension: [[p1, i, t], [p2, j, u]] per crossing edge pair.
    let mut intersections: Vec<Value> = Vec::new();
    let r1_paths = value_rows(&region1)?;
    let r2_paths = value_rows(&region2)?;
    // Pre-wrap region2's polys once per p2 — the reference re-evaluates `poly` per (p1, i) but the
    // wrap is pure, so hoisting is value-identical (each entry is the same Value either way).
    let mut polys: Vec<Value> = Vec::with_capacity(r2_paths.len());
    for path2 in &r2_paths {
        polys.push(if closed2.is_truthy() {
            list_wrap_val(path2, &Value::Num(1e-9))?
        } else {
            path2.clone()
        });
    }
    for (p1, path1_raw) in r1_paths.iter().enumerate() {
        let path = if closed1.is_truthy() {
            list_wrap_val(path1_raw, &Value::Num(1e-9))?
        } else {
            path1_raw.clone()
        };
        let path_pts = value_rows(&path)?;
        let n_edges = path_pts.len().saturating_sub(1); // [0:1:len-2] inclusive
        for i in 0..n_edges {
            let a1 = path_pts[i].clone();
            let a2 = path_pts[i + 1].clone();
            let nrm = builtins::apply("norm", std::slice::from_ref(&sub(a1.clone(), a2.clone())));
            if !ops::apply_binary(BinOp::Gt, nrm.clone(), eps.clone()).is_truthy() {
                continue; // zero-length path edge
            }
            // seg_normal = [-(a2-a1).y, (a2-a1).x] / nrm
            let d = sub(a2.clone(), a1.clone());
            let seg_normal = ops::apply_binary(
                BinOp::Div,
                build_vector(vec![
                    ops::apply_unary(crate::parser::UnOp::Neg, ops::member(d.clone(), "y")),
                    ops::member(d, "x"),
                ]),
                nrm,
            );
            // ref = a1 * seg_normal — the 4-laned dot.
            let ref_v = ops::apply_binary(BinOp::Mul, a1.clone(), seg_normal.clone());
            for (p2, poly) in polys.iter().enumerate() {
                // signs[j]: the snapped sign of each poly vertex's distance to the [a1,a2] line.
                let projected = ops::apply_binary(BinOp::Mul, poly.clone(), seg_normal.clone());
                let proj_rows = match &projected {
                    Value::NumList(xs) => xs.iter().map(|&x| Value::Num(x)).collect(),
                    Value::List(xs) => xs.to_vec(),
                    other => vec![other.clone()],
                };
                let signs: Vec<Value> = proj_rows
                    .iter()
                    .map(|v| {
                        let dist = sub(v.clone(), ref_v.clone());
                        let near = ops::apply_binary(
                            BinOp::Lt,
                            builtins::apply("abs", std::slice::from_ref(&dist)),
                            eps.clone(),
                        );
                        if near.is_truthy() {
                            Value::Num(0.0)
                        } else {
                            builtins::apply("sign", std::slice::from_ref(&dist))
                        }
                    })
                    .collect();
                let signs_v = build_vector(signs.clone());
                let smax = builtins::apply("max", std::slice::from_ref(&signs_v));
                let smin = builtins::apply("min", std::slice::from_ref(&signs_v));
                if !(ops::apply_binary(BinOp::Ge, smax, Value::Num(0.0)).is_truthy()
                    && ops::apply_binary(BinOp::Le, smin, Value::Num(0.0)).is_truthy())
                {
                    continue; // no poly edge can cross the [a1,a2] line
                }
                let poly_pts = value_rows(poly)?;
                let m_edges = poly_pts.len().saturating_sub(1);
                for j in 0..m_edges {
                    if !ops::apply_binary(BinOp::Ne, signs[j].clone(), signs[j + 1].clone())
                        .is_truthy()
                    {
                        continue; // non-crossing or collinear
                    }
                    let b1 = poly_pts[j].clone();
                    let b2 = poly_pts[j + 1].clone();
                    let isect = gli_val(
                        &build_vector(vec![a1.clone(), a2.clone()]),
                        &build_vector(vec![b1, b2]),
                        &eps,
                    )?;
                    if !isect.is_truthy() {
                        continue;
                    }
                    let t = ops::index(isect.clone(), &Value::Num(1.0));
                    let u = ops::index(isect, &Value::Num(2.0));
                    let neg_eps = ops::apply_unary(crate::parser::UnOp::Neg, eps.clone());
                    let one_eps = ops::apply_binary(BinOp::Add, Value::Num(1.0), eps.clone());
                    let inside = ops::apply_binary(BinOp::Ge, t.clone(), neg_eps.clone())
                        .is_truthy()
                        && ops::apply_binary(BinOp::Le, t.clone(), one_eps.clone()).is_truthy()
                        && ops::apply_binary(BinOp::Ge, u.clone(), neg_eps).is_truthy()
                        && ops::apply_binary(BinOp::Le, u.clone(), one_eps).is_truthy();
                    if inside {
                        intersections.push(build_vector(vec![
                            build_vector(vec![idx_num(p1), idx_num(i), t]),
                            build_vector(vec![idx_num(p2), idx_num(j), u]),
                        ]));
                    }
                }
            }
        }
    }
    let intersections_v = build_vector(intersections);

    // ptind / points / cornerpts / risect / counts / pathind — the flattened-index machinery.
    let both = [region1.clone(), region2.clone()];
    let mut out_halves: Vec<Value> = Vec::with_capacity(2);
    for (side, region) in both.iter().enumerate() {
        let paths = value_rows(region)?;
        // ptind: [p, j, 0] per point of this region.
        let mut ptind: Vec<Value> = Vec::new();
        for (pi, path) in paths.iter().enumerate() {
            for j in 0..value_rows(path)?.len() {
                ptind.push(build_vector(vec![idx_num(pi), idx_num(j), Value::Num(0.0)]));
            }
        }
        let ptind_v = build_vector(ptind);
        let points = flatten_val(region)?;
        // cornerpts: duplicate points (self-touch) via vector_search(points, eps, points).
        let ks = vector_search_val(&points, &eps, &points)?;
        let mut cornerpts: Vec<Value> = Vec::new();
        for k in value_rows(&ks)? {
            let kl = value_rows(&k)?.len();
            if kl > 1 {
                // `each select(ptind, k)` — splice the selected index triples.
                let sel = super::lists::select(&[ptind_v.clone(), k])?;
                cornerpts.extend(value_rows(&sel)?);
            }
        }
        // risect = concat(column(intersections, side), cornerpts)
        let col = column_val(&intersections_v, &idx_num(side))?;
        let mut risect = value_rows(&col)?;
        risect.extend(cornerpts);
        let risect_v = build_vector(risect);
        // counts = count(len(region)); pathind = search(counts, risect, 0)
        let counts = count_val(
            &idx_num(paths.len()),
            &Value::Num(0.0),
            &Value::Num(1.0),
            &Value::Bool(false),
        )?;
        let pathind = builtins::apply(
            "search",
            &[counts.clone(), risect_v.clone(), Value::Num(0.0)],
        );
        // [for(j=counts) _sort_vectors(select(risect, pathind[j]))] — j iterates counts' VALUES,
        // which are 0..n-1, so positional indexing into pathind matches the reference exactly.
        let mut half: Vec<Value> = Vec::new();
        for j in value_rows(&counts)? {
            let group_idx = ops::index(pathind.clone(), &j);
            let group = super::lists::select(&[risect_v.clone(), group_idx])?;
            half.push(sort_vectors_val(&group, &Value::Undef)?);
        }
        out_halves.push(build_vector(half));
    }
    Ok(build_vector(out_halves))
}
