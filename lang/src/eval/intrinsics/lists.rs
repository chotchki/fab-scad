use super::math::posmod;
use super::{bosl_assert, v_is_finite, v_is_list};
use crate::eval::value::{self, Value};
use crate::eval::{build_range, build_vector, builtins, iter_values_raw, ops};
use crate::parser::BinOp;

/// BOSL2 `last(list) = list[len(list)-1]` — the final element. `len` is `undef` for anything but a
/// list/string (numbers, ranges, `undef`), and `undef-1` then indexes to `undef`, so a non-indexable arg is
/// `undef` here too; an EMPTY list gives `len 0 → index -1 → undef` (out of range), matching the interpreter.
/// The length uses the SAME `count = n as f64` the `len` builtin does, so `list[n-1]` routes through the real
/// [`ops::index`] with a bit-identical index.
pub(super) fn last(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    let n = match &list {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        Value::Str(s) => s.chars().count(),
        _ => return Ok(Value::Undef), // len(x) is undef → undef-1 → index(_, undef) → undef
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "matches the `len` builtin's `count(n) = n as f64`; a list past 2^52 elements is unreachable"
    )]
    Ok(ops::index(list, &Value::Num(n as f64 - 1.0)))
}

/// BOSL2 `default(v, dflt=undef) = is_undef(v) ? dflt : v` — `v` unless it's `undef`, then the fallback. A
/// 1-arg call leaves `dflt` at its `undef` default (so `default(undef)` is `undef`); the dispatch gate only
/// routes all-positional calls here, so the slice is `[v]` or `[v, dflt]`.
pub(super) fn default(args: &[Value]) -> crate::Result<Value> {
    Ok(match args.first() {
        None | Some(Value::Undef) => args.get(1).cloned().unwrap_or(Value::Undef),
        Some(v) => v.clone(),
    })
}

/// BOSL2 `select(list, start, end)` — one or more items with WRAPAROUND indexing (`(i%l+l)%l`), the hottest
/// function in BOSL2's path/list layer. Bit-identical BY CONSTRUCTION: every operation routes through the
/// interpreter's OWN primitives — the wrap math via [`ops::apply_binary`]'s `%`/`+`, indexing via
/// [`ops::index`], range iteration via [`value::range_iter`], element iteration via
/// [`iter_values_raw`], and result coalescing via [`build_vector`] (all-`Num` → `NumList`, else
/// `List`) — so no float-modulo/index/coalesce semantics are re-derived. The win is skipping the per-call
/// function/scope machinery plus the `is_num`/`is_vector`/`is_range`/`is_finite`/`len` sub-dispatch the
/// interpreted body pays on EVERY call. Reproduces all three assert raise-sites: (1) a non-list/string
/// `list`; (2) a non-num single `start` that isn't `[]`/a vector/a range; (3) a non-finite `start`/`end` in
/// the two-index form. The BOSL2 predicates reduce, in our value model, to: `is_num` = a NON-NaN `Num`
/// (`func.cc` excludes NaN, so `select(l, nan)` takes the else branch and RAISES); `is_vector` = a non-empty
/// list of all FINITE `Num`s (BOSL2's `[for(vi=v) if(!is_finite(vi)) 0]==[]`); `is_range` = a `Range` with
/// all-finite fields; `is_finite` = a finite `Num`.
pub(super) fn select(args: &[Value]) -> crate::Result<Value> {
    use crate::eval::ops::index;
    let list = args.first().cloned().unwrap_or(Value::Undef);
    // assert( is_list(list) || is_string(list), "Invalid list." )
    if !matches!(list, Value::NumList(_) | Value::List(_) | Value::Str(_)) {
        return Err(select_assert("Invalid list."));
    }
    let l = sel_len(&list); // len(list) as f64 — element count, or CHAR count for a string
    if l == 0.0 {
        return Ok(build_vector(Vec::new())); // l==0 ? []   (the `[]` literal is an empty NumList)
    }
    let lv = Value::Num(l);
    let start = args.get(1).cloned().unwrap_or(Value::Undef);
    let end = args.get(2).cloned().unwrap_or(Value::Undef);

    if matches!(end, Value::Undef) {
        // end==undef — the single-`start` form.
        if sel_is_num(&start) {
            // list[ (start%l+l)%l ]
            Ok(index(list, &wrap(start, &lv)))
        } else {
            // assert( start==[] || is_vector(start) || is_range(start), "Invalid start parameter" )
            if !(sel_is_empty_list(&start) || sel_is_vector(&start) || sel_is_range(&start)) {
                return Err(select_assert("Invalid start parameter"));
            }
            // [for (i=start) list[ (i%l+l)%l ]]
            let out = iter_values_raw(&start)
                .into_iter()
                .map(|i| index(list.clone(), &wrap(i, &lv)))
                .collect();
            Ok(build_vector(out))
        }
    } else {
        // end given — the two-index form.
        if !sel_is_finite(&start) {
            return Err(select_assert(
                "When `end` is given, `start` parameter should be a number.",
            ));
        }
        if !sel_is_finite(&end) {
            return Err(select_assert("Invalid end parameter."));
        }
        let s = wrap(start, &lv);
        let e = wrap(end, &lv);
        let (sn, en) = (sel_f64(&s), sel_f64(&e));
        let mut out = Vec::new();
        // (s <= e) via the interpreter's own `<=`; `s`/`e` are finite here (asserts passed), so it's a plain
        // numeric compare.
        if ops::apply_binary(BinOp::Le, s, e).is_truthy() {
            // [ for (i=[s:1:e]) list[i] ]
            for i in value::range_iter(sn, 1.0, en) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
        } else {
            // [ for (i=[s:1:l-1]) list[i], for (i=[0:1:e]) list[i] ] — the wraparound: tail then head, one list
            for i in value::range_iter(sn, 1.0, l - 1.0) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
            for i in value::range_iter(0.0, 1.0, en) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
        }
        Ok(build_vector(out))
    }
}

/// `(i % l + l) % l` via the interpreter's OWN `%`/`+` ([`ops::apply_binary`]) — the wrapped index is
/// then bit-identical to what the interpreted body computes, with no re-derived float-modulo semantics.
pub(super) fn wrap(i: Value, l: &Value) -> Value {
    use crate::eval::ops::apply_binary;
    let m = apply_binary(BinOp::Mod, i, l.clone());
    let plus = apply_binary(BinOp::Add, m, l.clone());
    apply_binary(BinOp::Mod, plus, l.clone())
}

/// `len(list)` as the `f64` the `len` builtin yields — element count, or CHAR count for a string.
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count(n) = n as f64`; a list past 2^52 elements is unreachable"
)]
pub(super) fn sel_len(v: &Value) -> f64 {
    let n = match v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        Value::Str(s) => s.chars().count(),
        _ => 0, // unreachable: `list` is asserted list-or-string above
    };
    n as f64
}

/// OpenSCAD `is_num` — a `Num` that is NOT NaN (`func.cc` guards `type()==NUMBER && !isnan`).
pub(super) fn sel_is_num(v: &Value) -> bool {
    matches!(v, Value::Num(n) if !n.is_nan())
}

/// BOSL2 `is_finite` — a FINITE `Num` (`is_num(x) && !is_nan(0*x)` collapses to `f64::is_finite`).
pub(super) fn sel_is_finite(v: &Value) -> bool {
    matches!(v, Value::Num(n) if n.is_finite())
}

/// `start == []` — an empty list in EITHER representation (`[]` is an empty `NumList`, and the two list
/// reprs compare equal element-for-element, so an empty `List` matches too).
pub(super) fn sel_is_empty_list(v: &Value) -> bool {
    match v {
        Value::NumList(xs) => xs.is_empty(),
        Value::List(xs) => xs.is_empty(),
        _ => false,
    }
}

/// BOSL2 `is_vector` at DEFAULT args — a NON-EMPTY list whose every element is a FINITE number
/// (`is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0]`; the `length`/`zero`/`all_nonzero`
/// clauses all short-circuit true on their `undef`/`false` defaults). Content-based, not repr-based, so it's
/// exact even for a heterogeneous `List` that happens to hold only finite numbers.
pub(super) fn sel_is_vector(v: &Value) -> bool {
    match v {
        Value::NumList(xs) => !xs.is_empty() && xs.iter().all(|x| x.is_finite()),
        Value::List(xs) => {
            !xs.is_empty()
                && xs
                    .iter()
                    .all(|e| matches!(e, Value::Num(n) if n.is_finite()))
        }
        _ => false,
    }
}

/// BOSL2 `is_range` — a `Range` whose three fields are all finite (`!is_list(x) && is_finite(x[0]) &&
/// is_finite(x[1]) && is_finite(x[2])`; only a `Range` indexes to numbers at 0/1/2, so nothing else qualifies).
pub(super) fn sel_is_range(v: &Value) -> bool {
    matches!(v, Value::Range { start, step, end } if start.is_finite() && step.is_finite() && end.is_finite())
}

/// The `f64` of a `Num` — used on the wrap results (always numbers here); `NaN` for anything else (unreached).
pub(super) fn sel_f64(v: &Value) -> f64 {
    match v {
        Value::Num(n) => *n,
        _ => f64::NAN,
    }
}

/// A `select` assert failure. The message is a diagnostic LOCATOR (the fast==slow harness matches on
/// "both raised", not on text), so it reproduces the reference's assert CONTROL FLOW, not its exact string.
pub(super) fn select_assert(msg: &str) -> crate::Error {
    crate::Error::Eval(format!("assert failed: {msg}"))
}

/// BOSL2 `force_list(value, n=1, fill)` — a list passes through; a scalar becomes `n` copies (or
/// `[value, fill, fill, …]` when `fill` is given). The repeat counts come from iterating the reference's own
/// ranges (`[1:1:n]` / `[2:1:n]`) built with the interpreter's `build_range` — so a garbage `n` degenerates
/// exactly as interpreted instead of needing its own numeric validation.
pub(super) fn force_list(args: &[Value]) -> crate::Result<Value> {
    let value = args.first().cloned().unwrap_or(Value::Undef);
    if v_is_list(&value) {
        return Ok(value);
    }
    let n = args.get(1).cloned().unwrap_or(Value::Num(1.0));
    let one = Value::Num(1.0);
    match args.get(2) {
        None | Some(Value::Undef) => {
            let range = build_range(&one, &one, &n);
            let out: Vec<Value> = iter_values_raw(&range)
                .iter()
                .map(|_| value.clone())
                .collect();
            Ok(build_vector(out))
        }
        Some(fill) => {
            let range = build_range(&Value::Num(2.0), &one, &n);
            let mut out = vec![value];
            out.extend(iter_values_raw(&range).iter().map(|_| fill.clone()));
            Ok(build_vector(out))
        }
    }
}

/// BOSL2 `idx(list, s=0, e=-1, step=1)` — the index RANGE of a list (`[0:1:len-1]` for the defaults; an
/// empty list yields the empty `[0:1:-1]`). Start/end wrap through the real [`posmod`] (so its assert raises
/// on a non-finite `s`/`e` exactly like the reference), the range builds through the interpreter's
/// `build_range`.
pub(super) fn idx(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    if !(v_is_list(&list) || matches!(list, Value::Str(_))) {
        return Err(bosl_assert("idx: invalid input"));
    }
    let ll = builtins::apply("len", &[list]);
    let s = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let e = args.get(2).cloned().unwrap_or(Value::Num(-1.0));
    let step = args.get(3).cloned().unwrap_or(Value::Num(1.0));
    if matches!(ll, Value::Num(n) if n == 0.0) {
        return Ok(build_range(
            &Value::Num(0.0),
            &Value::Num(1.0),
            &Value::Num(-1.0),
        ));
    }
    let s2 = posmod(&[s, ll.clone()])?;
    let e2 = posmod(&[e, ll])?;
    Ok(build_range(&s2, &step, &e2))
}

/// BOSL2 `in_list(val, list, idx)` — membership via the REAL `search` builtin (its named args are
/// positional slots 2/3 — OpenSCAD builtins read by position), with the reference's first-hit shortcut and
/// the all-hits retry. The retry's `[for(hit=…) if(…) 1] != []` is an any-match — collecting past the first
/// match is unobservable, so the native breaks early.
pub(super) fn in_list(args: &[Value]) -> crate::Result<Value> {
    let val = args.first().cloned().unwrap_or(Value::Undef);
    let list = args.get(1).cloned().unwrap_or(Value::Undef);
    let idxv = args.get(2).cloned().unwrap_or(Value::Undef);
    if !v_is_list(&list) {
        return Err(bosl_assert("in_list: input is not a list"));
    }
    let idx_undef = matches!(idxv, Value::Undef);
    if !(idx_undef || v_is_finite(&idxv)) {
        return Err(bosl_assert("in_list: invalid idx value"));
    }
    let val_list = build_vector(vec![val.clone()]);
    let firsthit = ops::index(
        builtins::apply(
            "search",
            &[
                val_list.clone(),
                list.clone(),
                Value::Num(1.0),
                idxv.clone(),
            ],
        ),
        &Value::Num(0.0),
    );
    let empty = build_vector(Vec::new());
    if ops::apply_binary(BinOp::Eq, firsthit.clone(), empty).is_truthy() {
        return Ok(Value::Bool(false));
    }
    let hit_item = |hit: &Value| {
        let item = ops::index(list.clone(), hit);
        if idx_undef {
            item
        } else {
            ops::index(item, &idxv)
        }
    };
    if ops::apply_binary(BinOp::Eq, val.clone(), hit_item(&firsthit)).is_truthy() {
        return Ok(Value::Bool(true));
    }
    let allhits = ops::index(
        builtins::apply(
            "search",
            &[val_list, list.clone(), Value::Num(0.0), idxv.clone()],
        ),
        &Value::Num(0.0),
    );
    for hit in iter_values_raw(&allhits) {
        if ops::apply_binary(BinOp::Eq, hit_item(&hit), val.clone()).is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// BOSL2 `_group_sort_by_index(l, idx)` — quicksort-flavored grouping by `l[i][idx]`. The reference's
/// `concat(recurse(lesser), [equal], recurse(greater))` flattens to an iterative IN-ORDER walk (a
/// pre-sorted 20k-element input would recurse ~20k deep otherwise); partitions are strictly smaller — the
/// pivot's own element lands in `equal`, or (NaN/incomparable index) in none — so the walk terminates.
/// All comparisons route through ops (mixed-type `<`/`>` yield undef → dropped, like the comprehensions).
pub(super) fn group_sort_by_index(args: &[Value]) -> crate::Result<Value> {
    enum Work {
        Split(Value),
        Emit(Value),
    }
    let idx = args.get(1).cloned().unwrap_or(Value::Undef);
    let mut out: Vec<Value> = Vec::new();
    let mut stack = vec![Work::Split(args.first().cloned().unwrap_or(Value::Undef))];
    while let Some(work) = stack.pop() {
        let l = match work {
            Work::Emit(group) => {
                out.push(group);
                continue;
            }
            Work::Split(l) => l,
        };
        let ll = builtins::apply("len", std::slice::from_ref(&l));
        if ops::apply_binary(BinOp::Eq, ll.clone(), Value::Num(0.0)).is_truthy() {
            continue; // `[]` contributes nothing to the flat walk
        }
        if ops::apply_binary(BinOp::Eq, ll.clone(), Value::Num(1.0)).is_truthy() {
            out.push(l);
            continue;
        }
        let mid = builtins::apply(
            "floor",
            &[ops::apply_binary(BinOp::Div, ll, Value::Num(2.0))],
        );
        let pivot = ops::index(ops::index(l.clone(), &mid), &idx);
        let mut equal: Vec<Value> = Vec::new();
        let mut lesser: Vec<Value> = Vec::new();
        let mut greater: Vec<Value> = Vec::new();
        for li in iter_values_raw(&l) {
            let key = ops::index(li.clone(), &idx);
            if ops::apply_binary(BinOp::Eq, key.clone(), pivot.clone()).is_truthy() {
                equal.push(li);
            } else if ops::apply_binary(BinOp::Lt, key.clone(), pivot.clone()).is_truthy() {
                lesser.push(li);
            } else if ops::apply_binary(BinOp::Gt, key, pivot.clone()).is_truthy() {
                greater.push(li);
            }
        }
        stack.push(Work::Split(build_vector(greater)));
        stack.push(Work::Emit(build_vector(equal)));
        stack.push(Work::Split(build_vector(lesser))); // LIFO → lesser first: in-order
    }
    Ok(build_vector(out))
}
