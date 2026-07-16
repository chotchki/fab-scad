//! The intrinsic tier (O.1) — replace a hot BOSL2 function's INTERPRETED body with a hand-written native
//! Rust implementation, selected by an AST FINGERPRINT so it's never silently wrong.
//!
//! The safety doctrine, stated once: an intrinsic is dispatched ONLY when the running function's
//! `(params, body)` AST fingerprints EXACTLY to the version the intrinsic was written and verified against.
//! A user on a different BOSL2 revision (a renamed local, a tweaked formula, an extra clamp) fingerprints
//! DIFFERENTLY → the registry misses → the interpreter runs the real body. So an intrinsic can never be
//! applied to a function it wasn't proven equivalent to; the worst case is a missed speedup, never a wrong
//! answer. The fast==slow harness ([`tests`]) is the other half: it runs the intrinsic AND the interpreted
//! reference on the same inputs and asserts BIT-IDENTICAL, so a divergent intrinsic fails the build.
//!
//! This module is O.1 — the MECHANISM (fingerprint + registry + the never-wrong gate). The intrinsics
//! themselves (the hand-written bodies for the profile's hot functions) are O.2.

// Every intrinsic conforms to one fallible fn-pointer type (`Intrinsic`), because SOME functions have an
// inline `assert` that must raise. The predicates that CAN'T fail still return `Ok(..)` to fit that shared
// ABI — `unnecessary_wraps` fires on those, but the wrap is required by the type, not incidental.
#![allow(
    clippy::unnecessary_wraps,
    reason = "the uniform fallible Intrinsic fn-pointer type; infallible impls wrap in Ok to conform"
)]

use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use super::value::Value;
use crate::parser::{Arg, BinOp, Expr, ExprKind, Parameter};

/// A hand-written native implementation of a specific user function. Receives the call's POSITIONAL argument
/// VALUES (already evaluated, in source order) and returns the result — the same `Value` the interpreted body
/// would, or the same ERROR (a BOSL2 function with an inline `assert(…)` raises when the assert fails, so the
/// ABI is fallible; the native reproduces the assert's CONTROL FLOW — it errors where the body errors — not
/// its diagnostic string, which is a locator, not output). PURE: a function of its args only (no scope, no
/// `$`-vars); the dispatch gate ([`super`]) only routes all-positional calls here, so the ABI stays a flat
/// slice. An intrinsic implements the WHOLE function for the arg shapes it accepts; it hardcodes the
/// reference's parameter defaults (it matches that exact source), so a short positional call still gets it.
pub(super) type Intrinsic = fn(&[Value]) -> crate::Result<Value>;

/// One registered intrinsic: the exact function it stands in for. `reference` is the VERBATIM source of that
/// function (one `function name(params) = body;`) — the single source of truth: its fingerprint gates
/// dispatch, and the fast==slow harness runs its interpreted body as the oracle the `func` must bit-match.
struct Entry {
    /// The function name the intrinsic implements (registry bucket key).
    name: &'static str,
    /// The verbatim reference source of that function — fingerprinted + run as the harness oracle.
    reference: &'static str,
    /// Named TOP-LEVEL CONSTANTS the reference hardcodes (default exprs like `eps=_EPSILON`, or body reads),
    /// with the value the native impl bakes in. Empty = self-contained. Non-empty makes the entry
    /// CONST-GUARDED (O.5.1): the fingerprint proves the FUNCTION source, not the constants it names, so a
    /// user override (`_EPSILON = 1e-6;`) would make the baked value silently wrong. Guarded entries skip
    /// [`lookup`] (never wire at ctx build) and arm ONLY after island globals are built, when each named
    /// constant's BOUND value in the fn's home-island global bit-matches — see
    /// `super::arm_guarded_intrinsics`. Mismatch (or mid-hoist, before globals exist) → interpreted: the
    /// worst case stays "missed speedup, never a wrong answer".
    consts: &'static [(&'static str, f64)],
    /// The native implementation.
    func: Intrinsic,
}

/// The intrinsic registry. `_fab_poc_sq` is the O.1 mechanism POC (a synthetic, collision-proof name); the
/// rest are O.2 — the profile's hot BOSL2 predicates. Each entry's `reference` is the VERBATIM BOSL2 source
/// (from `libs/BOSL2`); the fast==slow harness proves the native `func` bit-matches interpreting it, and
/// `FAB_EXPLAIN` confirms it WIREs (vs DRIFTs) against the user's actual library.
///
/// Some references call only OpenSCAD BUILTINS (`is_undef`, `is_string`, `is_num`), so the harness interprets
/// them with a default `Ctx`. Others call ANOTHER BOSL2 function — `is_finite` → `is_nan` — so their harness
/// case interprets the reference with that dependency defined (the dependency-aware harness; see
/// [`tests::interpret_with_deps`]). `is_vector` (→ `_EPSILON` + a loop + optional params) is the next step.
///
/// The O.2 targets are the top of the `FAB_PROFILE_FNS` call profile on `slice_parts` (docs/models-profile.md):
/// `is_finite` (34.6% of user-fn calls) and `is_nan` (21.3%) alone are 56% of all calls, and both are hot
/// BECAUSE every BOSL2 assert validates its inputs through them. Intrinsic-ing `is_finite` ALSO erases its
/// `is_num`/`is_nan` sub-calls (the interpreted body dispatches them; the native body computes directly).
static REGISTRY: &[Entry] = &[
    Entry {
        name: "_fab_poc_sq",
        reference: "function _fab_poc_sq(x) = x * x;",
        consts: &[],
        func: poc_sq,
    },
    // BOSL2 `is_def`/`is_str` — the two hottest LEAF predicates (called in nearly every optional-arg check
    // and string guard). Verbatim from libs/BOSL2/builtins.scad.
    Entry {
        name: "is_def",
        reference: "function is_def(x) = !is_undef(x);",
        consts: &[],
        func: is_def,
    },
    Entry {
        name: "is_str",
        reference: "function is_str(x) = is_string(x);",
        consts: &[],
        func: is_str,
    },
    // BOSL2 `is_nan`/`is_finite` — the #1 and #2 hottest user functions on the model profile (56% of calls
    // combined), the workhorses of BOSL2's input validation. Verbatim from libs/BOSL2/utility.scad.
    Entry {
        name: "is_nan",
        reference: "function is_nan(x) = (x!=x);",
        consts: &[],
        func: is_nan,
    },
    Entry {
        name: "is_finite",
        reference: "function is_finite(x) = is_num(x) && !is_nan(0*x);",
        consts: &[],
        func: is_finite,
    },
    // BOSL2 `last` (9.6% of user-fn calls) + `default` (2.5%) — the next two down the profile. Both call only
    // builtins (`len`, `is_undef`), so the plain interpreter is their oracle. Verbatim from lists.scad /
    // utility.scad.
    Entry {
        name: "last",
        reference: "function last(list) = list[len(list)-1];",
        consts: &[],
        func: last,
    },
    Entry {
        name: "default",
        reference: "function default(v,dflt=undef) = is_undef(v)? dflt : v;",
        consts: &[],
        func: default,
    },
    // `_is_liststr` (2.2%) — a pure leaf (calls only the `is_str` intrinsic + the `is_list` builtin), from
    // strings.scad. `point3d` (1.8%) from coords.scad — the first intrinsic with an inline `assert` (raises on
    // a non-list, exercising the fallible ABI) that also BUILDS a value.
    Entry {
        name: "_is_liststr",
        reference: "function _is_liststr(s) = is_list(s) || is_str(s);",
        consts: &[],
        func: is_liststr,
    },
    Entry {
        name: "point3d",
        reference: "function point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]];",
        consts: &[],
        func: point3d,
    },
    // BOSL2 `select` (lists.scad) — the WRAPAROUND list indexer, the single hottest function in the path/list
    // layer: 86% of the ipad_holder_decorative_front profile's user-fn calls (5.8M at $fn=20), hammered by
    // the O(n²) `path_merge_collinear`/`path_sweep2d` inner loops. The first MULTI-BRANCH intrinsic — scalar
    // index / vector-or-range gather / two-index slice, THREE assert raise-sites, string-OR-list input — and
    // it earns its complexity: every op routes through the interpreter's own primitives (`%`/`+` via
    // `apply_binary`, `ops::index`, `range_iter`, `build_vector`), so it's bit-identical by construction.
    // Verbatim from lists.scad.
    Entry {
        name: "select",
        reference: "function select(list, start, end) = \
            assert( is_list(list) || is_string(list), \"Invalid list.\") \
            let(l=len(list)) \
            l==0 \
              ? [] \
              : end==undef \
                  ? is_num(start) \
                      ? list[ (start%l+l)%l ] \
                      : assert( start==[] || is_vector(start) || is_range(start), \"Invalid start parameter\") \
                        [for (i=start) list[ (i%l+l)%l ] ] \
                  : assert(is_finite(start), \"When `end` is given, `start` parameter should be a number.\") \
                    assert(is_finite(end), \"Invalid end parameter.\") \
                    let( s = (start%l+l)%l, e = (end%l+l)%l ) \
                    (s <= e) \
                      ? [ for (i = [s:1:e])   list[i] ] \
                      : [ for (i = [s:1:l-1]) list[i], for (i = [0:1:e])   list[i] ] ;",
        consts: &[],
        func: select,
    },
    // The CONST-GUARD POC (O.5.1, a synthetic collision-proof name like `_fab_poc_sq`): its reference bakes
    // the top-level constant `_EPSILON`, so it exercises the guarded-arm path end-to-end — it wires only
    // AFTER island globals are built and only when the home scope's `_EPSILON` is bit-exactly 1e-9
    // (`super::arm_guarded_intrinsics`). The real `_EPSILON` family (is_vector/approx/_tri_class…) is O.5.2+.
    Entry {
        name: "_fab_poc_near0",
        reference: "function _fab_poc_near0(x) = abs(x) < _EPSILON;",
        consts: &[("_EPSILON", 1e-9)],
        func: poc_near0,
    },
];

/// The POC intrinsic: `x * x`. Mirrors the interpreter's `Num * Num` (and `undef` for a non-number arg, as
/// `apply_binary` yields). Deliberately trivial — it exists to exercise the mechanism, not to be fast.
fn poc_sq(args: &[Value]) -> crate::Result<Value> {
    Ok(match args {
        [Value::Num(x)] => Value::Num(x * x),
        _ => Value::Undef,
    })
}

/// The const-guard POC: `abs(x) < _EPSILON` with `_EPSILON` baked as 1e-9 (the guard proves the bake).
/// Routes through the REAL `abs` builtin + the interpreter's own `<`, so it can't diverge on exotic inputs
/// (`abs` of a list/undef, `undef < num`) — bit-identical by construction, like `select`.
fn poc_near0(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let a = super::builtins::apply("abs", &[x]);
    Ok(super::ops::apply_binary(BinOp::Lt, a, Value::Num(1e-9)))
}

/// BOSL2 `is_def(x) = !is_undef(x)` — true iff `x` is anything but `undef`. Only the first positional arg
/// binds to `x` (extras are ignored, per OpenSCAD); zero args → `x` is `undef` → `false`.
fn is_def(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(!matches!(
        args.first(),
        None | Some(Value::Undef)
    )))
}

/// BOSL2 `is_str(x) = is_string(x)` — true iff `x` is a string.
fn is_str(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::Str(_)))))
}

/// BOSL2 `is_nan(x) = (x!=x)` — a value equals itself EXCEPT `NaN`, so this is true iff `x` is `NaN`. The hot
/// scalar path is native (`f64::is_nan`); any other type routes through the interpreter's own `!=` so the
/// intrinsic can't diverge from `x!=x` on an exotic input (e.g. a `NaN` inside a list, where element-wise `!=`
/// makes `[nan]!=[nan]` TRUE — a case the native scalar check would miss, but the op reproduces exactly).
fn is_nan(args: &[Value]) -> crate::Result<Value> {
    Ok(match args.first() {
        Some(Value::Num(n)) => Value::Bool(n.is_nan()),
        other => {
            let x = other.cloned().unwrap_or(Value::Undef);
            super::ops::apply_binary(BinOp::Ne, x.clone(), x)
        }
    })
}

/// BOSL2 `is_finite(x) = is_num(x) && !is_nan(0*x)` — true iff `x` is a finite number. `0*x` is `NaN` when `x`
/// is `±inf`/`NaN` and `0` when finite, so the whole expression collapses to `f64::is_finite` on a number and
/// `false` on any non-number (the `is_num` short-circuit). Computing it directly erases the reference's
/// `is_num`/`is_nan`/`*` sub-evaluation — the point of the intrinsic. Proven bit-identical by the harness,
/// which interprets the reference WITH `is_nan` defined (the dependency-aware oracle).
fn is_finite(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(
        matches!(args.first(), Some(Value::Num(n)) if n.is_finite()),
    ))
}

/// BOSL2 `last(list) = list[len(list)-1]` — the final element. `len` is `undef` for anything but a
/// list/string (numbers, ranges, `undef`), and `undef-1` then indexes to `undef`, so a non-indexable arg is
/// `undef` here too; an EMPTY list gives `len 0 → index -1 → undef` (out of range), matching the interpreter.
/// The length uses the SAME `count = n as f64` the `len` builtin does, so `list[n-1]` routes through the real
/// [`super::ops::index`] with a bit-identical index.
fn last(args: &[Value]) -> crate::Result<Value> {
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
    Ok(super::ops::index(list, &Value::Num(n as f64 - 1.0)))
}

/// BOSL2 `default(v, dflt=undef) = is_undef(v) ? dflt : v` — `v` unless it's `undef`, then the fallback. A
/// 1-arg call leaves `dflt` at its `undef` default (so `default(undef)` is `undef`); the dispatch gate only
/// routes all-positional calls here, so the slice is `[v]` or `[v, dflt]`.
fn default(args: &[Value]) -> crate::Result<Value> {
    Ok(match args.first() {
        None | Some(Value::Undef) => args.get(1).cloned().unwrap_or(Value::Undef),
        Some(v) => v.clone(),
    })
}

/// BOSL2 `_is_liststr(s) = is_list(s) || is_str(s)` — true iff `s` is a list (either representation) or a
/// string. A pure leaf: `is_list` is true for `List`/`NumList`, `is_str` for `Str`.
fn is_liststr(args: &[Value]) -> crate::Result<Value> {
    Ok(Value::Bool(matches!(
        args.first(),
        Some(Value::List(_) | Value::NumList(_) | Value::Str(_))
    )))
}

/// BOSL2 `point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]]` — pad/truncate a
/// point to 3 coords. A non-list RAISES (the inline assert; the message is a locator, so the harness matches
/// on "both errored", not the text). Each coord replicates the reference ternary through the REAL `==`
/// (`undef==undef` is true → an out-of-range slot takes `fill`) and `is_truthy`, then `build_vector` coalesces
/// exactly as the interpreter does (all-numeric → `NumList`, else `List`). `fill` defaults to `0` (1-arg call).
fn point3d(args: &[Value]) -> crate::Result<Value> {
    let p = args.first().cloned().unwrap_or(Value::Undef);
    if !matches!(p, Value::List(_) | Value::NumList(_)) {
        return Err(crate::Error::Eval(
            "assertion failed [assert(is_list(p))]".to_string(),
        ));
    }
    let fill = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let coords = (0..3)
        .map(|i| {
            let pi = super::ops::index(p.clone(), &Value::Num(f64::from(i)));
            if super::ops::apply_binary(BinOp::Eq, pi.clone(), Value::Undef).is_truthy() {
                fill.clone()
            } else {
                pi
            }
        })
        .collect();
    Ok(super::build_vector(coords))
}

/// BOSL2 `select(list, start, end)` — one or more items with WRAPAROUND indexing (`(i%l+l)%l`), the hottest
/// function in BOSL2's path/list layer. Bit-identical BY CONSTRUCTION: every operation routes through the
/// interpreter's OWN primitives — the wrap math via [`super::ops::apply_binary`]'s `%`/`+`, indexing via
/// [`super::ops::index`], range iteration via [`super::value::range_iter`], element iteration via
/// [`super::iter_values`], and result coalescing via [`super::build_vector`] (all-`Num` → `NumList`, else
/// `List`) — so no float-modulo/index/coalesce semantics are re-derived. The win is skipping the per-call
/// function/scope machinery plus the `is_num`/`is_vector`/`is_range`/`is_finite`/`len` sub-dispatch the
/// interpreted body pays on EVERY call. Reproduces all three assert raise-sites: (1) a non-list/string
/// `list`; (2) a non-num single `start` that isn't `[]`/a vector/a range; (3) a non-finite `start`/`end` in
/// the two-index form. The BOSL2 predicates reduce, in our value model, to: `is_num` = a NON-NaN `Num`
/// (`func.cc` excludes NaN, so `select(l, nan)` takes the else branch and RAISES); `is_vector` = a non-empty
/// list of all FINITE `Num`s (BOSL2's `[for(vi=v) if(!is_finite(vi)) 0]==[]`); `is_range` = a `Range` with
/// all-finite fields; `is_finite` = a finite `Num`.
fn select(args: &[Value]) -> crate::Result<Value> {
    use super::ops::index;
    let list = args.first().cloned().unwrap_or(Value::Undef);
    // assert( is_list(list) || is_string(list), "Invalid list." )
    if !matches!(list, Value::NumList(_) | Value::List(_) | Value::Str(_)) {
        return Err(select_assert("Invalid list."));
    }
    let l = sel_len(&list); // len(list) as f64 — element count, or CHAR count for a string
    if l == 0.0 {
        return Ok(super::build_vector(Vec::new())); // l==0 ? []   (the `[]` literal is an empty NumList)
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
            let out = super::iter_values(&start)
                .into_iter()
                .map(|i| index(list.clone(), &wrap(i, &lv)))
                .collect();
            Ok(super::build_vector(out))
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
        if super::ops::apply_binary(BinOp::Le, s, e).is_truthy() {
            // [ for (i=[s:1:e]) list[i] ]
            for i in super::value::range_iter(sn, 1.0, en) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
        } else {
            // [ for (i=[s:1:l-1]) list[i], for (i=[0:1:e]) list[i] ] — the wraparound: tail then head, one list
            for i in super::value::range_iter(sn, 1.0, l - 1.0) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
            for i in super::value::range_iter(0.0, 1.0, en) {
                out.push(index(list.clone(), &Value::Num(i)));
            }
        }
        Ok(super::build_vector(out))
    }
}

/// `(i % l + l) % l` via the interpreter's OWN `%`/`+` ([`super::ops::apply_binary`]) — the wrapped index is
/// then bit-identical to what the interpreted body computes, with no re-derived float-modulo semantics.
fn wrap(i: Value, l: &Value) -> Value {
    use super::ops::apply_binary;
    let m = apply_binary(BinOp::Mod, i, l.clone());
    let plus = apply_binary(BinOp::Add, m, l.clone());
    apply_binary(BinOp::Mod, plus, l.clone())
}

/// `len(list)` as the `f64` the `len` builtin yields — element count, or CHAR count for a string.
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count(n) = n as f64`; a list past 2^52 elements is unreachable"
)]
fn sel_len(v: &Value) -> f64 {
    let n = match v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        Value::Str(s) => s.chars().count(),
        _ => 0, // unreachable: `list` is asserted list-or-string above
    };
    n as f64
}

/// OpenSCAD `is_num` — a `Num` that is NOT NaN (`func.cc` guards `type()==NUMBER && !isnan`).
fn sel_is_num(v: &Value) -> bool {
    matches!(v, Value::Num(n) if !n.is_nan())
}

/// BOSL2 `is_finite` — a FINITE `Num` (`is_num(x) && !is_nan(0*x)` collapses to `f64::is_finite`).
fn sel_is_finite(v: &Value) -> bool {
    matches!(v, Value::Num(n) if n.is_finite())
}

/// `start == []` — an empty list in EITHER representation (`[]` is an empty `NumList`, and the two list
/// reprs compare equal element-for-element, so an empty `List` matches too).
fn sel_is_empty_list(v: &Value) -> bool {
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
fn sel_is_vector(v: &Value) -> bool {
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
fn sel_is_range(v: &Value) -> bool {
    matches!(v, Value::Range { start, step, end } if start.is_finite() && step.is_finite() && end.is_finite())
}

/// The `f64` of a `Num` — used on the wrap results (always numbers here); `NaN` for anything else (unreached).
fn sel_f64(v: &Value) -> f64 {
    match v {
        Value::Num(n) => *n,
        _ => f64::NAN,
    }
}

/// A `select` assert failure. The message is a diagnostic LOCATOR (the fast==slow harness matches on
/// "both raised", not on text), so it reproduces the reference's assert CONTROL FLOW, not its exact string.
fn select_assert(msg: &str) -> crate::Error {
    crate::Error::Eval(format!("assert failed: {msg}"))
}

/// `name → (fingerprint, intrinsic, const guard)` for every registry entry, computed ONCE by parsing each
/// `reference` and fingerprinting its `(params, body)`. Lazy + cached: the parse cost is paid the first time
/// an intrinsic is looked up in the process, never per call. A `reference` that doesn't parse to a single
/// `function` def is a registry BUG — it's dropped with a debug assert rather than silently mis-registering.
type Row = (&'static str, u64, Intrinsic, &'static [(&'static str, f64)]);
fn table() -> &'static [Row] {
    static TABLE: OnceLock<Vec<Row>> = OnceLock::new();
    TABLE.get_or_init(|| {
        REGISTRY
            .iter()
            .filter_map(|entry| {
                let fp = reference_fingerprint(entry.reference)?;
                Some((entry.name, fp, entry.func, entry.consts))
            })
            .collect()
    })
}

/// Parse a registry `reference` (one `function` def) and fingerprint it, or `None` if it isn't exactly that
/// (a registry authoring bug).
fn reference_fingerprint(reference: &str) -> Option<u64> {
    use crate::parser::{StmtKind, parse};
    let program = parse(reference).ok()?;
    let stmt = program.stmts.into_iter().next()?;
    if let StmtKind::FunctionDef { params, body, .. } = stmt.kind {
        Some(fingerprint(&params, &body))
    } else {
        debug_assert!(
            false,
            "intrinsic reference is not a single function def: {reference}"
        );
        None
    }
}

/// Resolve a defined function to its UNGUARDED intrinsic, if one is registered for EXACTLY this body. Called
/// ONCE per function at [`super::build_ctx`] time (never per call): fingerprint the running `(params, body)`,
/// then match on (name, fingerprint). A miss — no entry for the name, or the name matches but the body
/// doesn't — returns `None`, so the interpreter runs the real body. This is the never-silently-wrong gate.
/// CONST-GUARDED entries (non-empty `consts`) never resolve here — they arm later, after island globals are
/// built, via [`lookup_guarded`] + `super::arm_guarded_intrinsics` (their baked constants can't be checked at
/// ctx build, and mid-hoist the interpreter must run).
#[must_use]
pub(super) fn lookup(name: &str, params: &[Parameter], body: &Expr) -> Option<Intrinsic> {
    let fp = fingerprint(params, body);
    table()
        .iter()
        .find(|(n, f, _, consts)| *n == name && *f == fp && consts.is_empty())
        .map(|(_, _, func, _)| *func)
}

/// The CONST-GUARDED half of [`lookup`]: a fingerprint-matched entry with a non-empty const guard, returned
/// WITH the guard so the caller (`super::arm_guarded_intrinsics`) can verify each named constant's bound
/// value before wiring.
#[must_use]
pub(super) fn lookup_guarded(
    name: &str,
    params: &[Parameter],
    body: &Expr,
) -> Option<(Intrinsic, &'static [(&'static str, f64)])> {
    let fp = fingerprint(params, body);
    table()
        .iter()
        .find(|(n, f, _, consts)| *n == name && *f == fp && !consts.is_empty())
        .map(|(_, _, func, consts)| (*func, *consts))
}

/// Test-only access to a registry entry's reference source, for the fast==slow harness.
#[cfg(test)]
pub(super) fn reference_of(name: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.reference)
}

/// How a defined function relates to the intrinsic registry — the EXPLAIN classification (O.3).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Plan {
    /// An intrinsic is registered for this name AND the body fingerprint matches → native dispatch will fire.
    Wired,
    /// An intrinsic is registered for this NAME, but the defined body fingerprints DIFFERENTLY (a BOSL2
    /// revision the intrinsic's reference doesn't match) → it silently INTERPRETS. The actionable case:
    /// either the user's library drifted, or the intrinsic's reference source is stale and needs updating.
    Drift,
    /// No intrinsic registered for this name — the ordinary interpreted function (the vast majority).
    NotRegistered,
}

/// Classify a defined function against the registry (O.3 EXPLAIN). Pure + testable; the `FAB_EXPLAIN`
/// stderr report ([`super::build_intrinsics`]) is just this plus a print.
#[must_use]
pub(super) fn classify(name: &str, params: &[Parameter], body: &Expr) -> Plan {
    if !REGISTRY.iter().any(|e| e.name == name) {
        return Plan::NotRegistered;
    }
    // Fingerprint-level truth: a const-guarded match is WIRED here (the source matched); whether its guard
    // then arms is a separate, per-program verdict `arm_guarded_intrinsics` prints under the same EXPLAIN.
    let fp = fingerprint(params, body);
    if table().iter().any(|(n, f, ..)| *n == name && *f == fp) {
        Plan::Wired
    } else {
        Plan::Drift
    }
}

/// The registered REFERENCE fingerprint for `name` — the hash a running function must match to WIRE — or
/// `None` if no intrinsic is registered under that name. Feeds the EXPLAIN DRIFT diagnostic, which prints it
/// next to the running function's own fingerprint so an author can SEE how the two differ (stale reference vs
/// a genuinely different library version). See [`fingerprint`].
#[must_use]
pub(super) fn reference_fp(name: &str) -> Option<u64> {
    table()
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(_, fp, ..)| *fp)
}

/// Is the `FAB_EXPLAIN` intrinsic-plan report on? Cached once (env read per ctx build would be silly).
pub(super) fn explain_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FAB_EXPLAIN").is_some())
}

/// A structural fingerprint of a function's `(params, body)`: a 64-bit hash over the AST SHAPE — variant
/// discriminants, operators, literal bits (`f64` by `to_bits`, so `NaN`/`±0` are exact), names, and nesting
/// — with SPANS EXCLUDED (a fingerprint is source-formatting-independent; only the structure counts). Two
/// functions fingerprinting equal are structurally identical. A fixed-seed hasher makes it run-reproducible.
///
/// Collision note: a 64-bit hash CAN alias in theory, but the registry pairs the fingerprint with the
/// function NAME and the fast==slow harness proves each registered intrinsic bit-matches its reference — so
/// a collision would have to hit a same-NAMED, harness-verified function to matter, which the harness would
/// itself catch. Fingerprint is a fast pre-filter, not the whole proof.
#[must_use]
pub(super) fn fingerprint(params: &[Parameter], body: &Expr) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_params(params, &mut h);
    hash_expr(body, &mut h);
    h.finish()
}

/// Hash a parameter list: arity, then each name + whether/how it defaults. Names ARE part of the identity
/// (a renamed param is a different function to us — and a different intrinsic contract).
fn hash_params(params: &[Parameter], h: &mut impl Hasher) {
    params.len().hash(h);
    for p in params {
        p.name.hash(h);
        match &p.default {
            Some(default) => {
                1u8.hash(h);
                hash_expr(default, h);
            }
            None => 0u8.hash(h),
        }
    }
}

/// Hash an argument list (call args + comprehension bindings): each arg's optional name + its value expr.
fn hash_args(args: &[Arg], h: &mut impl Hasher) {
    args.len().hash(h);
    for arg in args {
        match &arg.name {
            Some(name) => {
                1u8.hash(h);
                name.hash(h);
            }
            None => 0u8.hash(h),
        }
        hash_expr(&arg.value, h);
    }
}

/// Hash an expression's STRUCTURE, recursively, span-free. The match is EXHAUSTIVE with NO wildcard arm on
/// purpose: adding an `ExprKind` variant is then a COMPILE error here, forcing the fingerprint to account
/// for it — a silently-unhashed field would let two different functions collide and mis-dispatch an
/// intrinsic. Each arm leads with a distinct discriminant byte so structurally-different shapes can't alias
/// by field coincidence.
#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive per-variant match IS the safety mechanism — one arm per ExprKind, no wildcard, \
    so a new AST variant is a compile error here rather than a silently-unhashed field that could collide"
)]
fn hash_expr(e: &Expr, h: &mut impl Hasher) {
    match &e.kind {
        ExprKind::Num(n) => {
            0u8.hash(h);
            n.to_bits().hash(h);
        }
        ExprKind::Str(s) => {
            1u8.hash(h);
            s.hash(h);
        }
        ExprKind::Bool(b) => {
            2u8.hash(h);
            b.hash(h);
        }
        ExprKind::Undef => 3u8.hash(h),
        ExprKind::Ident(name) => {
            4u8.hash(h);
            name.hash(h);
        }
        ExprKind::Unary { op, operand } => {
            5u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(operand, h);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            6u8.hash(h);
            (*op as u8).hash(h);
            hash_expr(lhs, h);
            hash_expr(rhs, h);
        }
        ExprKind::Ternary { cond, then, els } => {
            7u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_expr(els, h);
        }
        ExprKind::Index { base, index } => {
            8u8.hash(h);
            hash_expr(base, h);
            hash_expr(index, h);
        }
        ExprKind::Member { base, field } => {
            9u8.hash(h);
            hash_expr(base, h);
            field.hash(h);
        }
        ExprKind::Call { callee, args } => {
            10u8.hash(h);
            hash_expr(callee, h);
            hash_args(args, h);
        }
        ExprKind::Vector(items) => {
            11u8.hash(h);
            items.len().hash(h);
            for item in items {
                hash_expr(item, h);
            }
        }
        ExprKind::Range { start, step, end } => {
            12u8.hash(h);
            hash_expr(start, h);
            match step {
                Some(step) => {
                    1u8.hash(h);
                    hash_expr(step, h);
                }
                None => 0u8.hash(h),
            }
            hash_expr(end, h);
        }
        ExprKind::FunctionLiteral { params, body } => {
            13u8.hash(h);
            hash_params(params, h);
            hash_expr(body, h);
        }
        ExprKind::Let { bindings, body } => {
            14u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::Assert { args, body } => {
            15u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::Echo { args, body } => {
            16u8.hash(h);
            hash_args(args, h);
            hash_opt(body.as_deref(), h);
        }
        ExprKind::LcFor { bindings, body } => {
            17u8.hash(h);
            hash_args(bindings, h);
            hash_expr(body, h);
        }
        ExprKind::LcForC {
            init,
            cond,
            update,
            body,
        } => {
            18u8.hash(h);
            hash_args(init, h);
            hash_expr(cond, h);
            hash_args(update, h);
            hash_expr(body, h);
        }
        ExprKind::LcEach(body) => {
            19u8.hash(h);
            hash_expr(body, h);
        }
        ExprKind::LcIf { cond, then, els } => {
            20u8.hash(h);
            hash_expr(cond, h);
            hash_expr(then, h);
            hash_opt(els.as_deref(), h);
        }
    }
}

/// Hash an optional sub-expression (a present/absent flag then the expr) — `assert`/`echo`/`LcIf` bodies.
fn hash_opt(e: Option<&Expr>, h: &mut impl Hasher) {
    match e {
        Some(e) => {
            1u8.hash(h);
            hash_expr(e, h);
        }
        None => 0u8.hash(h),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::float_cmp,
    reason = "test harness: expect/panic ARE the assertions; intrinsics must bit-match, so == is exact"
)]
mod tests {
    use super::{fingerprint, lookup, poc_sq, reference_of};
    use crate::eval::build_ctx;
    use crate::parser::{Expr, Parameter, StmtKind, parse};
    use crate::{Scope, Value, eval_expr};

    /// Parse `src` (one `function` def) → its `(params, body)`.
    fn parse_fn(src: &str) -> (Vec<Parameter>, Expr) {
        let program = parse(src).expect("parses");
        let stmt = program.stmts.into_iter().next().expect("one stmt");
        match stmt.kind {
            StmtKind::FunctionDef { params, body, .. } => (params, body),
            other => panic!("expected a function def, got {other:?}"),
        }
    }

    /// `parse_fn` then fingerprint.
    fn fp(src: &str) -> u64 {
        let (params, body) = parse_fn(src);
        fingerprint(&params, &body)
    }

    /// The SLOW side of the harness: interpret a reference function's body with its params bound to
    /// `inputs`, via `eval_expr` (a default `Ctx` — NO intrinsics, so this is the pure interpreter). Returns a
    /// `Result` so an inline-`assert` reference (its failure IS the reference's behavior) compares against the
    /// intrinsic's error, not a panic.
    fn interpret(reference: &str, inputs: &[Value]) -> crate::Result<Value> {
        let (params, body) = parse_fn(reference);
        let mut scope = Scope::new();
        for (i, p) in params.iter().enumerate() {
            // A provided arg fills the slot; an unprovided one takes the param's DEFAULT (else undef) — the
            // real call path binds defaults, so an oracle that skipped them would run a short call with the
            // wrong values (e.g. `point3d(p)` with `fill` unbound instead of `fill=0`).
            let v = match inputs.get(i) {
                Some(v) => v.clone(),
                None => match &p.default {
                    Some(d) => eval_expr(d, &scope)?,
                    None => Value::Undef,
                },
            };
            scope.bind(p.name.clone(), v);
        }
        eval_expr(&body, &scope)
    }

    /// Fast (intrinsic) and slow (interpreter) agree: both `Ok` with bit-identical values, or both `Err` (the
    /// message is a diagnostic locator, not output — an intrinsic reproduces the assert's CONTROL FLOW, so
    /// "both raised" is the match). A mixed `Ok`/`Err` is a real divergence.
    fn same_result(fast: &crate::Result<Value>, slow: &crate::Result<Value>) -> bool {
        match (fast, slow) {
            (Ok(a), Ok(b)) => bit_eq(a, b),
            (Err(_), Err(_)) => true,
            _ => false,
        }
    }

    /// The SLOW side for a reference that calls OTHER BOSL2 functions (the dependency-aware oracle). `deps` are
    /// the verbatim source of those functions; they precede `target` in one program so its body can resolve
    /// them. The built `Ctx` has its intrinsics table CLEARED, so the oracle is FULLY interpreted end-to-end
    /// (a dep that happens to be a registered intrinsic doesn't shortcut — we're proving against the
    /// interpreter, not against another intrinsic). `target` must be the LAST definition.
    fn interpret_with_deps(target: &str, deps: &[&str], inputs: &[Value]) -> crate::Result<Value> {
        let src = format!("{}\n{target}", deps.join("\n"));
        let program = parse(&src).expect("deps+target parse");
        let mut ctx = build_ctx(&program, crate::Config::default());
        ctx.intrinsics.clear(); // force full interpretation — no intrinsic shortcut even for the deps
        let (params, body) = match &program.stmts.last().expect("has target").kind {
            StmtKind::FunctionDef { params, body, .. } => (params, body),
            other => panic!("target is not a function def: {other:?}"),
        };
        let mut scope = Scope::new();
        for (i, p) in params.iter().enumerate() {
            let v = match inputs.get(i) {
                Some(v) => v.clone(),
                None => match &p.default {
                    Some(d) => crate::eval::eval_with_ctx(d, &scope, &ctx)?,
                    None => Value::Undef,
                },
            };
            scope.bind(p.name.clone(), v);
        }
        crate::eval::eval_with_ctx(body, &scope, &ctx)
    }

    /// Bit-level `Value` equality — the harness's notion of "bit-identical". `f64`s compare by `to_bits`, so
    /// two `NaN`s (same bits) are EQUAL where `==` says `NaN != NaN`, and `0.0`/`-0.0` (different bits) are
    /// DISTINCT where `==` says equal — exactly the determinism doctrine. Recurses into lists; other variants
    /// fall back to `==` (they carry no float). Used wherever an intrinsic can RETURN a number (`last`/
    /// `default`); the `Bool`-returning predicates are fine with plain `==`.
    fn bit_eq(a: &Value, b: &Value) -> bool {
        use Value::{List, Num, NumList, Range};
        match (a, b) {
            (Num(x), Num(y)) => x.to_bits() == y.to_bits(),
            (NumList(x), NumList(y)) => {
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|(p, q)| p.to_bits() == q.to_bits())
            }
            (List(x), List(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| bit_eq(p, q))
            }
            (
                Range {
                    start: s1,
                    step: t1,
                    end: e1,
                },
                Range {
                    start: s2,
                    step: t2,
                    end: e2,
                },
            ) => {
                s1.to_bits() == s2.to_bits()
                    && t1.to_bits() == t2.to_bits()
                    && e1.to_bits() == e2.to_bits()
            }
            _ => a == b,
        }
    }

    /// The value battery the predicate intrinsics are proven against — one of every `Value` shape, with the
    /// float edges (`±0`, `±inf`, `NaN`) that `is_nan`/`is_finite` turn on, plus a `NaN`/`inf` INSIDE a list
    /// (the element-wise-`!=` corner that separates a naive scalar `is_nan` from the real `x!=x`).
    fn value_battery() -> Vec<Value> {
        vec![
            Value::Undef,
            Value::Num(0.0),
            Value::Num(-0.0),
            Value::Num(3.5),
            Value::Num(-42.0),
            Value::Num(f64::INFINITY),
            Value::Num(f64::NEG_INFINITY),
            Value::Num(f64::NAN),
            Value::Bool(true),
            Value::Bool(false),
            Value::string("hi"),
            Value::string(""),
            Value::list(vec![Value::Num(1.0), Value::Num(2.0)]),
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::num_list(vec![f64::NAN]),
            Value::num_list(vec![f64::INFINITY]),
            Value::list(vec![]),
            Value::Range {
                start: 0.0,
                step: 1.0,
                end: 5.0,
            },
        ]
    }

    #[test]
    fn fingerprint_is_span_independent() {
        // Same STRUCTURE, different source formatting (whitespace/comments shift every span) → SAME
        // fingerprint. This is the property the registry relies on: it matches structure, not bytes.
        let a = fp("function f(x) = x + 1;");
        let b = fp("function f( x ) =\n   x  +  1 ; // trailing");
        assert_eq!(a, b, "whitespace/comments must not change the fingerprint");
    }

    #[test]
    fn a_changed_body_fingerprints_differently() {
        // The never-silently-wrong gate: a tweaked formula, a renamed param, or a changed literal is a
        // DIFFERENT function → different fingerprint → the intrinsic misses and the interpreter runs.
        let base = fp("function f(x) = x + 1;");
        assert_ne!(base, fp("function f(x) = x + 2;"), "literal change");
        assert_ne!(base, fp("function f(x) = x - 1;"), "operator change");
        assert_ne!(base, fp("function f(y) = y + 1;"), "param rename");
        assert_ne!(base, fp("function f(x, y) = x + 1;"), "arity change");
        assert_ne!(
            base,
            fp("function f(x) = x + 1.0000001;"),
            "epsilon literal change"
        );
    }

    #[test]
    fn structurally_identical_functions_collide_by_design() {
        // Two DIFFERENTLY-NAMED functions with identical params+body fingerprint the SAME — the registry
        // pairs the fingerprint with the NAME, so this is fine (name disambiguates); the fingerprint only
        // certifies the BODY matches. Documents that fingerprint alone is body-identity, not full identity.
        assert_eq!(fp("function a(x) = x * x;"), fp("function b(x) = x * x;"));
    }

    #[test]
    fn deep_structural_features_are_captured() {
        // Comprehensions, lets, ternaries, ranges, calls — the shapes real BOSL2 functions are built from —
        // all feed the hash; a change deep inside flips the fingerprint (no shallow-only hashing).
        let a = fp("function g(n) = [for (i = [0:n]) let(j = i*2) [i, j > 3 ? j : 0]];");
        let b = fp("function g(n) = [for (i = [0:n]) let(j = i*2) [i, j > 4 ? j : 0]];");
        assert_ne!(
            a, b,
            "a literal buried in a nested comprehension must still register"
        );
    }

    #[test]
    fn fast_equals_slow_bit_for_bit() {
        // THE correctness gate: every registered intrinsic must return EXACTLY what interpreting its
        // reference body returns, for every input. This is what makes an intrinsic safe to exist — it's
        // proven equivalent to the code it replaces. O.2 extends this per new intrinsic + its inputs.
        let reference = reference_of("_fab_poc_sq").expect("POC registered");
        for x in [0.0, 1.0, -3.5, 2.5, 1e9, std::f64::consts::PI, -0.0] {
            let input = [Value::Num(x)];
            assert!(
                same_result(&poc_sq(&input), &interpret(reference, &input)),
                "intrinsic vs interpreter diverged at x={x}"
            );
        }
        // A non-number arg: the intrinsic must ALSO match the interpreter's undef (x*x on a string → undef).
        let bad = [Value::string("nope")];
        assert!(
            same_result(&poc_sq(&bad), &interpret(reference, &bad)),
            "undef path must match too"
        );
    }

    /// The SLOW side for a reference that reads a TOP-LEVEL CONSTANT (`_EPSILON`): like [`interpret`], plus
    /// the named constants bound into the scope first — in a real program they'd resolve from the home-island
    /// global, and the const GUARD (O.5.1) is what certifies the bound value matches the intrinsic's bake.
    fn interpret_with_consts(
        reference: &str,
        consts: &[(&str, Value)],
        inputs: &[Value],
    ) -> crate::Result<Value> {
        let (params, body) = parse_fn(reference);
        let mut scope = Scope::new();
        for (name, v) in consts {
            scope.bind((*name).to_string(), v.clone());
        }
        for (i, p) in params.iter().enumerate() {
            let v = match inputs.get(i) {
                Some(v) => v.clone(),
                None => match &p.default {
                    Some(d) => eval_expr(d, &scope)?,
                    None => Value::Undef,
                },
            };
            scope.bind(p.name.clone(), v);
        }
        eval_expr(&body, &scope)
    }

    #[test]
    fn fast_equals_slow_fab_poc_near0() {
        // The const-guard POC's correctness half: with `_EPSILON` bound to the guarded 1e-9 (the only state
        // the intrinsic ever arms under), native must bit-match the interpreter over the whole battery plus
        // the near-epsilon edges (strictly-less, exactly-equal, just-above).
        let reference = reference_of("_fab_poc_near0").expect("POC registered");
        let eps = [("_EPSILON", Value::Num(1e-9))];
        let mut inputs = value_battery();
        inputs.extend([5e-10, 1e-9, 2e-9, -5e-10, -1e-9].map(Value::Num));
        for v in inputs {
            let args = [v.clone()];
            assert!(
                same_result(
                    &super::poc_near0(&args),
                    &interpret_with_consts(reference, &eps, &args)
                ),
                "intrinsic vs interpreter diverged at {v:?}"
            );
        }
    }

    #[test]
    fn a_const_guarded_entry_never_wires_through_the_unguarded_lookup() {
        // The build-time gate: `lookup` (what `build_intrinsics` wires from) must SKIP a guarded entry even
        // on an exact fingerprint match — it arms later, after the guard verifies (`lookup_guarded`).
        let (p, b) = parse_fn(reference_of("_fab_poc_near0").unwrap());
        assert!(
            lookup("_fab_poc_near0", &p, &b).is_none(),
            "guarded entries must not wire at ctx build"
        );
        assert!(
            super::lookup_guarded("_fab_poc_near0", &p, &b).is_some(),
            "the guarded lookup must find it (exact fingerprint)"
        );
        assert!(
            super::lookup_guarded("_fab_poc_sq", &p, &b).is_none(),
            "an unguarded name never resolves through the guarded lookup"
        );
    }

    #[test]
    fn the_fingerprint_gate_matches_only_the_exact_body() {
        // Never silently wrong: the intrinsic registers for the EXACT reference, and misses on any
        // perturbation (different body) or a name mismatch → the interpreter runs the real body instead.
        let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
        assert!(
            lookup("_fab_poc_sq", &p, &b).is_some(),
            "the exact reference must register"
        );

        let (p2, b2) = parse_fn("function _fab_poc_sq(x) = x + x;");
        assert!(
            lookup("_fab_poc_sq", &p2, &b2).is_none(),
            "a changed body must NOT match"
        );

        let (p3, b3) = parse_fn("function _fab_poc_sq(x, y) = x * x;");
        assert!(
            lookup("_fab_poc_sq", &p3, &b3).is_none(),
            "a changed arity must NOT match"
        );

        assert!(
            lookup("some_other_name", &p, &b).is_none(),
            "same body, wrong name → no match"
        );
    }

    #[test]
    fn build_ctx_wires_the_intrinsic_for_a_matching_program() {
        // The dispatch is authorized at ctx build: a program defining the exact reference function gets the
        // intrinsic in ctx.intrinsics (so `dispatch_call` will route its all-positional calls natively). A
        // program with a perturbed body does NOT — it stays interpreted.
        let matched = parse("function _fab_poc_sq(x) = x * x;").expect("parses");
        assert!(
            build_ctx(&matched, crate::Config::default())
                .intrinsics
                .contains_key("_fab_poc_sq"),
            "the exact reference must be wired as an intrinsic"
        );
        let perturbed = parse("function _fab_poc_sq(x) = x * x + 1;").expect("parses");
        assert!(
            !build_ctx(&perturbed, crate::Config::default())
                .intrinsics
                .contains_key("_fab_poc_sq"),
            "a perturbed body must fall back to the interpreter (no intrinsic wired)"
        );
    }

    #[test]
    fn a_matching_call_dispatches_through_the_intrinsic_task() {
        // End-to-end: exercise `Task::Intrinsic` through the real eval loop. A program defines the exact
        // reference; its call's RHS is evaluated with the built ctx, so `dispatch_call` routes the
        // all-positional call to the native `poc_sq` → 7*7 = 49. (The corpus proves the arm doesn't break
        // anything; this proves it RUNS — nothing in BOSL2 fingerprints to the POC, so only this hits it.)
        let program =
            parse("function _fab_poc_sq(x) = x * x; z = _fab_poc_sq(7);").expect("parses");
        let ctx = build_ctx(&program, crate::Config::default());
        let call = match &program.stmts[1].kind {
            StmtKind::Assignment { value, .. } => value,
            other => panic!("expected an assignment, got {other:?}"),
        };
        let result = crate::eval::eval_with_ctx(call, &Scope::new(), &ctx).expect("evaluates");
        assert_eq!(
            result,
            Value::Num(49.0),
            "the intrinsic-dispatched call returns x*x"
        );
    }

    #[test]
    fn leaf_predicate_intrinsics_match_their_references_bit_for_bit() {
        // O.2: each real predicate intrinsic must equal interpreting its VERBATIM BOSL2 reference, across
        // every value type. (These references call only builtins — is_undef/is_string — so `interpret`'s
        // default Ctx can run them.)
        let cases = [
            Value::Undef,
            Value::Num(3.0),
            Value::Num(-0.0),
            Value::Bool(false),
            Value::string("hi"),
            Value::list(vec![Value::Num(1.0), Value::Num(2.0)]),
        ];
        for name in ["is_def", "is_str"] {
            let reference = reference_of(name).expect("registered");
            let (params, body) = parse_fn(reference);
            let func = lookup(name, &params, &body).expect("its own reference must register");
            for input in &cases {
                let one = [input.clone()];
                assert!(
                    same_result(&func(&one), &interpret(reference, &one)),
                    "{name}({input:?}) diverged"
                );
            }
            // Zero args: the single param defaults to undef in both paths.
            assert!(
                same_result(&func(&[]), &interpret(reference, &[])),
                "{name}() diverged"
            );
        }
    }

    #[test]
    fn is_nan_matches_its_reference_bit_for_bit() {
        // `is_nan(x) = (x!=x)` — no deps, so the plain interpreter is the oracle. The list-with-NaN case is
        // the one that matters: `[nan]!=[nan]` is TRUE (element-wise), so a scalar-only intrinsic would be
        // wrong there — the intrinsic routes non-numbers through the real `!=`, and this proves it.
        let reference = reference_of("is_nan").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("is_nan", &params, &body).expect("its own reference must register");
        for input in value_battery() {
            let one = [input.clone()];
            assert!(
                same_result(&func(&one), &interpret(reference, &one)),
                "is_nan({input:?}) diverged"
            );
        }
        assert!(
            same_result(&func(&[]), &interpret(reference, &[])),
            "is_nan() diverged"
        );
    }

    #[test]
    fn is_finite_matches_its_reference_bit_for_bit() {
        // `is_finite(x) = is_num(x) && !is_nan(0*x)` calls `is_nan` — the dependency-aware oracle interprets
        // the reference WITH `is_nan` defined (and intrinsics cleared, so `is_nan` interprets too). Proves the
        // direct `f64::is_finite` collapse equals the full is_num/`0*x`/is_nan chain across every value shape.
        let reference = reference_of("is_finite").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("is_finite", &params, &body).expect("its own reference must register");
        let deps = ["function is_nan(x) = (x!=x);"];
        for input in value_battery() {
            let one = [input.clone()];
            assert!(
                same_result(&func(&one), &interpret_with_deps(reference, &deps, &one)),
                "is_finite({input:?}) diverged"
            );
        }
        assert!(
            same_result(&func(&[]), &interpret_with_deps(reference, &deps, &[])),
            "is_finite() diverged"
        );
    }

    #[test]
    fn last_matches_its_reference_bit_for_bit() {
        // `last(list) = list[len(list)-1]` calls only builtins (`len`, index) → plain interpreter oracle. The
        // battery hits every shape: a populated list/numlist (real last element), an EMPTY list (len 0 →
        // index -1 → undef), a string (last char), and non-indexables (num/range/undef → undef).
        let reference = reference_of("last").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("last", &params, &body).expect("its own reference must register");
        for input in value_battery() {
            let one = [input.clone()];
            assert!(
                same_result(&func(&one), &interpret(reference, &one)),
                "last({input:?}) diverged"
            );
        }
        // A longer list, to prove it's the LAST element and not the first/second.
        let long = [Value::list(
            (0..7).map(|i| Value::Num(f64::from(i))).collect::<Vec<_>>(),
        )];
        assert!(
            same_result(&func(&long), &interpret(reference, &long)),
            "last(0..6) diverged"
        );
    }

    #[test]
    fn default_matches_its_reference_bit_for_bit() {
        // `default(v, dflt=undef) = is_undef(v) ? dflt : v` — two params, so prove BOTH the 1-arg (dflt takes
        // its undef default) and 2-arg forms across the battery. `is_undef` is a builtin → plain oracle.
        let reference = reference_of("default").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("default", &params, &body).expect("its own reference must register");
        let battery = value_battery();
        for v in &battery {
            let one = [v.clone()];
            assert!(
                same_result(&func(&one), &interpret(reference, &one)),
                "default({v:?}) diverged"
            );
            for d in &battery {
                let two = [v.clone(), d.clone()];
                assert!(
                    same_result(&func(&two), &interpret(reference, &two)),
                    "default({v:?}, {d:?}) diverged"
                );
            }
        }
    }

    #[test]
    fn is_liststr_matches_its_reference_bit_for_bit() {
        // `_is_liststr(s) = is_list(s) || is_str(s)` calls the `is_str` BOSL2 fn → dependency-aware oracle
        // (is_list is a builtin). True for List/NumList/Str, false otherwise, across the whole battery.
        let reference = reference_of("_is_liststr").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("_is_liststr", &params, &body).expect("its own reference must register");
        let deps = ["function is_str(x) = is_string(x);"];
        for input in value_battery() {
            let one = [input.clone()];
            assert!(
                same_result(&func(&one), &interpret_with_deps(reference, &deps, &one)),
                "_is_liststr({input:?}) diverged"
            );
        }
    }

    #[test]
    fn point3d_matches_its_reference_bit_for_bit() {
        // `point3d` is the first asserting intrinsic: a non-list must ERROR on BOTH sides (same_result treats
        // any two errors as matching), a list pads/truncates to 3 coords with `fill`. Proves the 1-arg
        // (fill=0) and 2-arg forms, and the padding (short vector) / truncation (long) / out-of-range→fill
        // paths — including the NumList-vs-List coalescing of the result.
        let reference = reference_of("point3d").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("point3d", &params, &body).expect("its own reference must register");
        for input in value_battery() {
            let one = [input.clone()];
            assert!(
                same_result(&func(&one), &interpret(reference, &one)),
                "point3d({input:?}) diverged"
            );
        }
        // Explicit shape cases: short (pad), exact, long (truncate), a heterogeneous list (List result), and a
        // custom 2-arg fill. Each proves value AND the assert-passes path.
        let shapes = [
            vec![Value::Num(5.0)],
            vec![Value::Num(1.0), Value::Num(2.0)],
            vec![Value::Num(1.0), Value::Num(2.0), Value::Num(3.0)],
            vec![
                Value::Num(1.0),
                Value::Num(2.0),
                Value::Num(3.0),
                Value::Num(4.0),
            ],
            vec![Value::Num(1.0), Value::string("x")],
        ];
        for s in shapes {
            let p = Value::list(s);
            let one = [p.clone()];
            assert!(
                same_result(&func(&one), &interpret(reference, &one)),
                "point3d({p:?}) diverged"
            );
            let two = [p.clone(), Value::Num(-1.0)];
            assert!(
                same_result(&func(&two), &interpret(reference, &two)),
                "point3d({p:?}, -1) diverged"
            );
        }
    }

    #[test]
    fn select_matches_its_reference_bit_for_bit() {
        // `select` is the first MULTI-BRANCH intrinsic — scalar index / vector-or-range gather / two-index
        // slice, three assert raise-sites, list-OR-string input. The dependency-aware oracle interprets the
        // verbatim reference WITH the real BOSL2 predicate chain defined (is_vector → is_finite → is_nan,
        // is_range) and intrinsics cleared, so the native `func` is proven against the FULLY-interpreted body.
        // `_EPSILON`/`norm`/`all_nonzero` are inert at is_vector's default args (short-circuited), so they need
        // no definition — an unknown `_EPSILON` resolves to undef and is never read.
        let reference = reference_of("select").expect("registered");
        let (params, body) = parse_fn(reference);
        let func = lookup("select", &params, &body).expect("its own reference must register");
        let deps = [
            "function is_nan(x) = (x!=x);",
            "function is_finite(x) = is_num(x) && !is_nan(0*x);",
            "function is_range(x) = !is_list(x) && is_finite(x[0]) && is_finite(x[1]) && is_finite(x[2]) ;",
            "function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) = \
                is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0] \
                && (is_undef(length) || (assert(is_num(length))len(v)==length)) \
                && (is_undef(zero) || ((norm(v) >= eps) == !zero)) \
                && (!all_nonzero || all_nonzero(v)) ;",
        ];

        let n = |xs: &[f64]| Value::num_list(xs.to_vec());
        let l7 = n(&[3., 4., 5., 6., 7., 8., 9.]); // the lists.scad doc example
        let hetero = Value::list(vec![
            Value::Num(1.0),
            Value::string("a"),
            Value::num_list(vec![2.0, 3.0]),
        ]);
        let s = Value::string("hello");
        let rng = |start: f64, step: f64, end: f64| Value::Range { start, step, end };

        let inf = f64::INFINITY;
        let nan = f64::NAN;
        let cases: Vec<Vec<Value>> = vec![
            // assert #1: a non-list/string `list` raises (both sides).
            vec![Value::Num(5.0), Value::Num(0.0)],
            vec![Value::Undef, Value::Num(0.0)],
            vec![rng(0., 1., 5.), Value::Num(0.0)],
            // l==0 → [] (list AND string), single- and two-arg.
            vec![n(&[]), Value::Num(2.0)],
            vec![Value::string(""), Value::Num(0.0)],
            vec![n(&[]), Value::Num(2.0), Value::Num(4.0)],
            // scalar start — wraparound, negatives, out-of-range, fractional (truncates), ±inf.
            vec![l7.clone(), Value::Num(5.0)],
            vec![l7.clone(), Value::Num(0.0)],
            vec![l7.clone(), Value::Num(6.0)],
            vec![l7.clone(), Value::Num(7.0)], // == l → wraps to 0
            vec![l7.clone(), Value::Num(-2.0)],
            vec![l7.clone(), Value::Num(-1.0)],
            vec![l7.clone(), Value::Num(100.0)],
            vec![l7.clone(), Value::Num(-100.0)],
            vec![l7.clone(), Value::Num(3.5)],
            vec![l7.clone(), Value::Num(inf)], // is_num TRUE (not NaN) → wrap→nan→index undef
            vec![l7.clone(), Value::Num(-inf)],
            // NaN start: is_num is FALSE for NaN → else branch → assert #2 raises.
            vec![l7.clone(), Value::Num(nan)],
            // vector start — gather with wraparound, and the empty vector → [].
            vec![l7.clone(), n(&[1., 3.])],
            vec![l7.clone(), n(&[3., 1.])],
            vec![l7.clone(), n(&[-1., -2.])],
            vec![l7.clone(), n(&[])],
            // range start.
            vec![l7.clone(), rng(1., 1., 3.)],
            vec![l7.clone(), rng(0., 2., 6.)],
            // BAD non-num start → assert #2 raises: non-num elem, nested, inf/nan elem, non-finite range,
            // string/bool/undef.
            vec![
                l7.clone(),
                Value::list(vec![Value::Num(1.0), Value::string("a")]),
            ],
            vec![
                l7.clone(),
                Value::list(vec![Value::num_list(vec![1.0, 2.0])]),
            ],
            vec![l7.clone(), n(&[1., inf])],
            vec![l7.clone(), n(&[nan, 2.])],
            vec![l7.clone(), rng(0., 1., inf)],
            vec![l7.clone(), Value::string("x")],
            vec![l7.clone(), Value::Bool(true)],
            vec![l7.clone(), Value::Undef],
            // two-index form — the doc examples + s>e wraparound + fractional bounds.
            vec![l7.clone(), Value::Num(5.0), Value::Num(6.0)],
            vec![l7.clone(), Value::Num(5.0), Value::Num(8.0)],
            vec![l7.clone(), Value::Num(5.0), Value::Num(2.0)],
            vec![l7.clone(), Value::Num(-3.0), Value::Num(-1.0)],
            vec![l7.clone(), Value::Num(3.0), Value::Num(3.0)],
            vec![l7.clone(), Value::Num(0.0), Value::Num(0.0)],
            vec![l7.clone(), Value::Num(6.0), Value::Num(0.0)],
            vec![l7.clone(), Value::Num(2.5), Value::Num(5.5)],
            // two-index non-finite → assert #3 raises (a non-num or inf/nan bound).
            vec![l7.clone(), Value::Num(inf), Value::Num(2.0)],
            vec![l7.clone(), Value::Num(2.0), Value::Num(nan)],
            vec![l7.clone(), Value::Num(2.0), Value::string("x")],
            vec![l7.clone(), Value::string("x"), Value::Num(2.0)],
            // heterogeneous List as `list` — element access, gather, slice (List result).
            vec![hetero.clone(), Value::Num(1.0)],
            vec![hetero.clone(), Value::Num(2.0)],
            vec![hetero.clone(), n(&[0., 2.])],
            vec![hetero.clone(), Value::Num(0.0), Value::Num(2.0)],
            // string as `list` — single char, gather + slice (List-of-Str result).
            vec![s.clone(), Value::Num(1.0)],
            vec![s.clone(), Value::Num(-1.0)],
            vec![s.clone(), n(&[0., 4.])],
            vec![s.clone(), Value::Num(1.0), Value::Num(3.0)],
            vec![s.clone(), Value::Num(3.0), Value::Num(1.0)],
        ];

        for inputs in &cases {
            assert!(
                same_result(
                    &func(inputs),
                    &interpret_with_deps(reference, &deps, inputs)
                ),
                "select diverged on {inputs:?}"
            );
        }
    }

    #[test]
    fn explain_classifies_wired_drift_and_unregistered() {
        use super::Plan;
        // WIRED: exact reference → will dispatch natively.
        let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
        assert_eq!(super::classify("_fab_poc_sq", &p, &b), Plan::Wired);
        // DRIFT: registered NAME, different body → interprets silently (the case EXPLAIN surfaces).
        let (pd, bd) = parse_fn("function _fab_poc_sq(x) = x * x + 1;");
        assert_eq!(super::classify("_fab_poc_sq", &pd, &bd), Plan::Drift);
        // NotRegistered: an ordinary function.
        let (pn, bn) = parse_fn("function ordinary(x) = x + 1;");
        assert_eq!(super::classify("ordinary", &pn, &bn), Plan::NotRegistered);
    }
}
