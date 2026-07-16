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
pub(super) struct Entry {
    /// The function name the intrinsic implements (registry bucket key).
    pub(super) name: &'static str,
    /// The verbatim reference source of that function — fingerprinted + run as the harness oracle.
    reference: &'static str,
    /// Named TOP-LEVEL CONSTANTS the reference hardcodes (default exprs like `eps=_EPSILON`, or body reads
    /// — `PI` counts too, it's just a seeded binding), with the value the native impl bakes in. Empty =
    /// self-contained. Non-empty makes the entry CONST-GUARDED (O.5.1): the fingerprint proves the FUNCTION
    /// source, not the constants it names, so a user override (`_EPSILON = 1e-6;`) would make the baked
    /// value silently wrong. Guarded entries never wire at ctx build and arm ONLY after island globals are
    /// built, when each named constant's BOUND value in the fn's home-island global bit-matches — see
    /// `super::arm_guarded_intrinsics`. Mismatch (or mid-hoist, before globals exist) → interpreted: the
    /// worst case stays "missed speedup, never a wrong answer".
    pub(super) consts: &'static [(&'static str, f64)],
    /// USER-FUNCTION names interpreting the reference can reach (O.5.2 dep pins), TRANSITIVELY CLOSED by the
    /// author over every arg shape the native accepts (`select` → `is_vector`/`is_range` → `is_finite` →
    /// `is_nan`; a branch no accepted arg shape can reach — `all_nonzero` behind select's fixed 1-arg
    /// `is_vector(start)` — is excluded). The entry wires only if each dep's DEFINED body fingerprints to
    /// that dep's own registry/[`PINS`] reference — the fingerprint gate extended one hop, because the
    /// native bakes the dep's semantics without the dep's own fingerprint ever being consulted at dispatch.
    pub(super) deps: &'static [&'static str],
    /// BUILTIN names interpreting the reference (or a pinned dep) can reach. A user function may SHADOW a
    /// builtin (dispatch resolves user fns first — BOSL2 itself shadows `reverse`), which would reroute the
    /// interpreted body while the native keeps the real builtin. The entry wires only if none of these names
    /// has a user-function definition.
    pub(super) builtins: &'static [&'static str],
    /// The native implementation.
    pub(super) func: Intrinsic,
}

/// Reference-only dependency anchors: BOSL2 functions we PIN (verbatim source → fingerprint) because a
/// registry entry's reference calls them, without shipping a native impl of our own. [`anchor_fp`] resolves
/// a dep name against entries first, then here.
static PINS: &[(&str, &str)] = &[
    // vectors.scad — `select`'s start-vector assert calls `is_vector(start)` (1-arg: the
    // `all_nonzero`/`zero`/`length` branches are unreachable, so they add no further deps).
    (
        "is_vector",
        "function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) =
    is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0]
    && (is_undef(length) || (assert(is_num(length))len(v)==length))
    && (is_undef(zero) || ((norm(v) >= eps) == !zero))
    && (!all_nonzero || all_nonzero(v)) ;",
    ),
    // utility.scad — `select`'s other assert branch.
    (
        "is_range",
        "function is_range(x) = !is_list(x) && is_finite(x[0]) && is_finite(x[1]) && is_finite(x[2]) ;",
    ),
];

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
        deps: &[],
        builtins: &[],
        func: poc_sq,
    },
    // BOSL2 `is_def`/`is_str` — the two hottest LEAF predicates (called in nearly every optional-arg check
    // and string guard). Verbatim from libs/BOSL2/builtins.scad.
    Entry {
        name: "is_def",
        reference: "function is_def(x) = !is_undef(x);",
        consts: &[],
        deps: &[],
        builtins: &["is_undef"],
        func: is_def,
    },
    Entry {
        name: "is_str",
        reference: "function is_str(x) = is_string(x);",
        consts: &[],
        deps: &[],
        builtins: &["is_string"],
        func: is_str,
    },
    // BOSL2 `is_nan`/`is_finite` — the #1 and #2 hottest user functions on the model profile (56% of calls
    // combined), the workhorses of BOSL2's input validation. Verbatim from libs/BOSL2/utility.scad.
    Entry {
        name: "is_nan",
        reference: "function is_nan(x) = (x!=x);",
        consts: &[],
        deps: &[],
        builtins: &[],
        func: is_nan,
    },
    Entry {
        name: "is_finite",
        reference: "function is_finite(x) = is_num(x) && !is_nan(0*x);",
        consts: &[],
        deps: &["is_nan"],
        builtins: &["is_num"],
        func: is_finite,
    },
    // BOSL2 `last` (9.6% of user-fn calls) + `default` (2.5%) — the next two down the profile. Both call only
    // builtins (`len`, `is_undef`), so the plain interpreter is their oracle. Verbatim from lists.scad /
    // utility.scad.
    Entry {
        name: "last",
        reference: "function last(list) = list[len(list)-1];",
        consts: &[],
        deps: &[],
        builtins: &["len"],
        func: last,
    },
    Entry {
        name: "default",
        reference: "function default(v,dflt=undef) = is_undef(v)? dflt : v;",
        consts: &[],
        deps: &[],
        builtins: &["is_undef"],
        func: default,
    },
    // `_is_liststr` (2.2%) — a pure leaf (calls only the `is_str` intrinsic + the `is_list` builtin), from
    // strings.scad. `point3d` (1.8%) from coords.scad — the first intrinsic with an inline `assert` (raises on
    // a non-list, exercising the fallible ABI) that also BUILDS a value.
    Entry {
        name: "_is_liststr",
        reference: "function _is_liststr(s) = is_list(s) || is_str(s);",
        consts: &[],
        deps: &["is_str"],
        builtins: &["is_list", "is_string"],
        func: is_liststr,
    },
    Entry {
        name: "point3d",
        reference: "function point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]];",
        consts: &[],
        deps: &[],
        builtins: &["is_list"],
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
        deps: &["is_vector", "is_range", "is_finite", "is_nan"],
        builtins: &["len", "is_list", "is_string", "is_num", "norm", "is_undef"],
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
        deps: &[],
        builtins: &["abs"],
        func: poc_near0,
    },
    // ── O.5.2, the SHAPE band (utility.scad / lists.scad) ────────────────────────────────────────────────
    // The `is_consistent`/`_list_pattern`/`same_shape` bundle is ~4.7s of self time across the O.4 four
    // (every BOSL2 path/vector assert funnels through it), `num_defined`/`force_list` are its cheap leaf
    // companions. All verbatim; every op routes through the interpreter's own primitives (`iter_values` for
    // comprehension iteration, `build_vector` for result coalescing, `apply_binary`/`index` for ops), so
    // variant identity (NumList vs List) and exotic-input behavior match by construction.
    Entry {
        name: "_list_pattern",
        reference: "function _list_pattern(list) =
  is_list(list)
  ? [for(entry=list) is_list(entry) ? _list_pattern(entry) : 0]
  : 0;",
        consts: &[],
        deps: &[],
        builtins: &["is_list"],
        func: list_pattern,
    },
    Entry {
        name: "same_shape",
        reference: "function same_shape(a,b) = is_def(b) && _list_pattern(a) == b*0;",
        consts: &[],
        deps: &["is_def", "_list_pattern"],
        builtins: &["is_undef", "is_list"],
        func: same_shape,
    },
    Entry {
        name: "is_consistent",
        reference: "function is_consistent(list, pattern) =
    is_list(list)
    && (len(list)==0
       || (let(pattern = is_undef(pattern) ? _list_pattern(list[0]): _list_pattern(pattern) )
          []==[for(entry=0*list) if (entry != pattern) entry]));",
        consts: &[],
        deps: &["_list_pattern"],
        builtins: &["is_list", "len", "is_undef"],
        func: is_consistent,
    },
    Entry {
        name: "num_defined",
        reference: "function num_defined(v) =
    len([for(vi=v) if(!is_undef(vi)) 1]);",
        consts: &[],
        deps: &[],
        builtins: &["len", "is_undef"],
        func: num_defined,
    },
    Entry {
        name: "force_list",
        reference: "function force_list(value, n=1, fill) =
    is_list(value) ? value :
    is_undef(fill)? [for (i=[1:1:n]) value] : [value, for (i=[2:1:n]) fill];",
        consts: &[],
        deps: &[],
        builtins: &["is_list", "is_undef"],
        func: force_list,
    },
    // ── O.5.2, the `_EPSILON` family (vectors/comparisons/math/lists/linalg.scad) ───────────────────────
    // The band's core: is_vector 8.8s / approx 5.9s of cross-model self time (every BOSL2 input assert
    // funnels through them), plus the posmod↔approx↔idx cycle group they drag in and is_matrix on top. All
    // const-guarded on `_EPSILON` (they bake 1e-9); the deps lists close the reachable-call graph so a
    // drifted neighbor declines the whole knot rather than running stale.
    Entry {
        name: "approx",
        reference: "function approx(a,b,eps=_EPSILON) =
    a == b? is_bool(a) == is_bool(b) :
    is_num(a) && is_num(b)? abs(a-b) <= eps :
    is_list(a) && is_list(b) && len(a) == len(b)? (
        [] == [
            for (i=idx(a))
            let(aa=a[i], bb=b[i])
            if(
                is_num(aa) && is_num(bb)? abs(aa-bb) > eps :
                !approx(aa,bb,eps=eps)
            ) 1
        ]
    ) : false;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["idx", "posmod", "is_finite", "is_nan"],
        builtins: &["is_bool", "is_num", "abs", "is_list", "is_string", "len"],
        func: approx,
    },
    Entry {
        name: "posmod",
        reference: "function posmod(x,m) =
    assert( is_finite(x) && is_finite(m) && !approx(m,0) , \"\\nInput must be finite numbers. The divisor cannot be zero.\")
    (x%m+m)%m;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["is_finite", "is_nan", "approx"],
        builtins: &["is_num", "abs", "is_bool"],
        func: posmod,
    },
    Entry {
        name: "idx",
        reference: "function idx(list, s=0, e=-1, step=1) =
    assert(is_list(list)||is_string(list), \"Invalid input.\" )
    let( ll = len(list) )
    ll == 0 ? [0:1:ll-1] :
    let(
        _s = posmod(s,ll),
        _e = posmod(e,ll)
    ) [_s : step : _e];",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["posmod", "is_finite", "is_nan", "approx"],
        builtins: &["is_list", "is_string", "len", "is_num", "abs", "is_bool"],
        func: idx,
    },
    Entry {
        name: "all_nonzero",
        reference: "function all_nonzero(x, eps=_EPSILON) =
    is_finite(x)? abs(x)>eps :
    is_vector(x) && [for (xx=x) if(abs(xx)<eps) 1] == [];",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["is_finite", "is_nan", "is_vector"],
        builtins: &["is_num", "abs", "is_list", "len", "is_undef"],
        func: all_nonzero,
    },
    Entry {
        name: "is_vector",
        reference: "function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) =
    is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0]
    && (is_undef(length) || (assert(is_num(length))len(v)==length))
    && (is_undef(zero) || ((norm(v) >= eps) == !zero))
    && (!all_nonzero || all_nonzero(v)) ;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["is_finite", "is_nan", "all_nonzero"],
        builtins: &["is_list", "len", "is_undef", "is_num", "norm", "abs"],
        func: is_vector,
    },
    // `is_vector(A[0],n)` is a fixed 2-arg call, so the zero/norm and all_nonzero branches are unreachable
    // from is_matrix — the deps close over what interpreting THIS reference can run, not all of is_vector.
    Entry {
        name: "is_matrix",
        reference: "function is_matrix(A,m,n,square=false) =
   is_list(A)
   && (( is_undef(m) && len(A) ) || len(A)==m)
   && (!square || len(A) == len(A[0]))
   && is_vector(A[0],n)
   && is_consistent(A);",
        consts: &[],
        deps: &["is_vector", "is_finite", "is_nan", "is_consistent", "_list_pattern"],
        builtins: &["is_list", "len", "is_undef", "is_num"],
        func: is_matrix,
    },
    // ── O.5.3, the EARCUT band (geometry.scad) ───────────────────────────────────────────────────────────
    // BOSL2 triangulates every VNF polygon by ear-cutting IN THE INTERPRETER: `_tri_class` (the CW/CCW/
    // collinear classifier) is 12.4s/3.9M calls across the O.4 four — window_air_cover's single biggest
    // line — and `_none_inside` (the per-ear containment scan, 4.8s/1.6M) is its hottest caller. The 2D
    // fast paths reproduce the builtins' EXACT formulas (`norm` = sequential sum-of-squares sqrt, 2D
    // `cross` = a0*b1 - a1*b0, `sign` = comparison chain, 0 at NaN); any other shape routes through the
    // real builtins/ops (a 3D triangle degenerates to undef exactly as interpreted).
    Entry {
        name: "_tri_class",
        reference: "function _tri_class(tri, eps=_EPSILON) =
    let( crx = cross(tri[1]-tri[2],tri[0]-tri[2]) )
    abs( crx ) <= eps*norm(tri[1]-tri[2])*norm(tri[0]-tri[2]) ? 0 : sign( crx );",
        consts: &[("_EPSILON", 1e-9)],
        deps: &[],
        builtins: &["cross", "norm", "abs", "sign"],
        func: tri_class,
    },
    Entry {
        name: "_is_at_left",
        reference: "function _is_at_left(pt,line,eps=_EPSILON) = _tri_class([pt,line[0],line[1]],eps) <= 0;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["_tri_class"],
        builtins: &["cross", "norm", "abs", "sign"],
        func: is_at_left,
    },
    // `eps` here is an EXPLICIT parameter (no default) — every internal call forwards it — so this entry
    // needs no `_EPSILON` guard despite living knee-deep in the tolerance family. The exotic-input
    // termination story rides on `select`: a non-list `idxs` or non-numeric `i` reaches `select`'s asserts
    // (which the native `select` raises identically), so the loop can't diverge where the interpreter
    // wouldn't.
    Entry {
        name: "_none_inside",
        reference: "function _none_inside(idxs,poly,p0,p1,p2,eps,i=0) =
    i>=len(idxs) ? true :
    let(
        vert      = poly[idxs[i]],
        prev_vert = poly[select(idxs,i-1)],
        next_vert = poly[select(idxs,i+1)]
    )
    // check if vert prevent [p0,p1,p2] to be an ear
    // this conditions might have a simpler expression
    _tri_class([prev_vert, vert, next_vert],eps) <= 0  // reflex condition
    &&  (  // vert is a cw reflex poly vertex inside the triangle [p0,p1,p2]
          ( _tri_class([p0,p1,vert],eps)>0 &&
            _tri_class([p1,p2,vert],eps)>0 &&
            _tri_class([p2,p0,vert],eps)>=0  )
          // or it is equal to p1 and some of its adjacent edges cross the open segment (p0,p2)
          ||  ( norm(vert-p1) < eps
                && _is_at_left(p0,[prev_vert,p1],eps) && _is_at_left(p2,[p1,prev_vert],eps)
                && _is_at_left(p2,[p1,next_vert],eps) && _is_at_left(p0,[next_vert,p1],eps)
              )
        )
    ?   false
    :   _none_inside(idxs,poly,p0,p1,p2,eps,i=i+1);",
        consts: &[],
        deps: &[
            "select",
            "_tri_class",
            "_is_at_left",
            "is_vector",
            "is_range",
            "is_finite",
            "is_nan",
        ],
        builtins: &[
            "len",
            "cross",
            "norm",
            "abs",
            "sign",
            "is_list",
            "is_string",
            "is_num",
            "is_undef",
        ],
        func: none_inside,
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

/// Is `v` a list to the `is_list` BUILTIN (the branch every shape function turns on)? Both vector variants;
/// nothing else (a string/range iterates in `for` but is NOT a list).
fn v_is_list(v: &Value) -> bool {
    matches!(v, Value::List(_) | Value::NumList(_))
}

/// BOSL2 `_list_pattern(list)` — the shape skeleton: every non-list leaf becomes `0`, lists recurse. Results
/// coalesce through the interpreter's own `build_vector`, so a flat numeric level becomes the same `NumList`
/// the comprehension would build — VARIANT identity matters, the callers compare patterns with `==`/`!=`.
fn list_pattern(args: &[Value]) -> crate::Result<Value> {
    Ok(list_pattern_of(args.first().unwrap_or(&Value::Undef)))
}
fn list_pattern_of(v: &Value) -> Value {
    if v_is_list(v) {
        let out: Vec<Value> = super::iter_values(v).iter().map(list_pattern_of).collect();
        super::build_vector(out)
    } else {
        Value::Num(0.0)
    }
}

/// BOSL2 `same_shape(a,b) = is_def(b) && _list_pattern(a) == b*0` — do `a` and `b` have the same nesting
/// skeleton? `b*0` and the `==` route through `apply_binary` (`0*"str"` is undef, list `==` is elementwise),
/// and a falsy `is_def(b)` short-circuits to `false` exactly like the interpreter's `&&`.
fn same_shape(args: &[Value]) -> crate::Result<Value> {
    if matches!(args.get(1), None | Some(Value::Undef)) {
        return Ok(Value::Bool(false)); // is_def(b) is false → && yields false
    }
    let a = args.first().cloned().unwrap_or(Value::Undef);
    let b = args.get(1).cloned().unwrap_or(Value::Undef);
    let pattern = list_pattern_of(&a);
    let b0 = super::ops::apply_binary(BinOp::Mul, b, Value::Num(0.0));
    let eq = super::ops::apply_binary(BinOp::Eq, pattern, b0);
    Ok(Value::Bool(eq.is_truthy()))
}

/// BOSL2 `is_consistent(list, pattern)` — is every element of `list` shaped like `pattern` (default: like
/// `list[0]`)? The reference compares each entry of `0*list` against the pattern with `!=`; both the zeroing
/// and the compare route through `apply_binary`, iteration through `iter_values` — so a heterogeneous list
/// (where `0*entry` is undef) answers exactly as interpreted.
fn is_consistent(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    if !v_is_list(&list) {
        return Ok(Value::Bool(false));
    }
    let n = match &list {
        Value::List(xs) => xs.len(),
        Value::NumList(xs) => xs.len(),
        _ => 0, // unreachable: v_is_list above
    };
    if n == 0 {
        return Ok(Value::Bool(true));
    }
    let pattern = match args.get(1) {
        None | Some(Value::Undef) => {
            list_pattern_of(&super::ops::index(list.clone(), &Value::Num(0.0)))
        }
        Some(p) => list_pattern_of(p),
    };
    let zeroed = super::ops::apply_binary(BinOp::Mul, Value::Num(0.0), list);
    let ok = super::iter_values(&zeroed)
        .into_iter()
        .all(|entry| !super::ops::apply_binary(BinOp::Ne, entry, pattern.clone()).is_truthy());
    Ok(Value::Bool(ok))
}

/// BOSL2 `num_defined(v) = len([for(vi=v) if(!is_undef(vi)) 1])` — how many entries are defined? Iteration
/// via `iter_values` (the interpreter's own `for` expansion: a scalar iterates once, a range expands), count
/// as the `len` builtin would report it.
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
fn num_defined(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let count = super::iter_values(&v)
        .iter()
        .filter(|vi| !matches!(vi, Value::Undef))
        .count();
    Ok(Value::Num(count as f64))
}

/// A raised BOSL2 `assert(…)` — the message is a diagnostic LOCATOR (fast==slow matches "both raised", not
/// text), same contract as [`select_assert`].
fn bosl_assert(msg: &str) -> crate::Error {
    crate::Error::Eval(format!("assert failed: {msg}"))
}

/// `is_finite` as the BOSL2 user fn computes it (`is_num(x) && !is_nan(0*x)`): a NON-NaN finite number.
fn v_is_finite(v: &Value) -> bool {
    matches!(v, Value::Num(n) if n.is_finite())
}

/// BOSL2 `approx(a,b,eps=_EPSILON)` — tolerant equality, recursing into lists. The num fast path requires
/// BOTH operands non-NaN (`is_num(NaN)` is false, so the interpreter routes NaN past that branch to the
/// list-check → `false`); an exotic (non-num) `eps` routes the compare through the interpreter's own op so
/// its undef-propagation survives. The list branch iterates pairwise (the reference's `idx(a)` is
/// `[0:1:len-1]` here — `posmod`'s assert can't fire, `len>0` when this branch differs from the `a==b` one).
fn approx(args: &[Value]) -> crate::Result<Value> {
    let a = args.first().cloned().unwrap_or(Value::Undef);
    let b = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Num(1e-9));
    approx_val(&a, &b, &eps)
}
fn approx_val(a: &Value, b: &Value, eps: &Value) -> crate::Result<Value> {
    use Value::{Bool, Num};
    if super::ops::apply_binary(BinOp::Eq, a.clone(), b.clone()).is_truthy() {
        return Ok(Bool(matches!(a, Bool(_)) == matches!(b, Bool(_))));
    }
    if let (Num(x), Num(y)) = (a, b)
        && !x.is_nan()
        && !y.is_nan()
    {
        return Ok(if let Num(e) = eps {
            Bool((x - y).abs() <= *e)
        } else {
            super::ops::apply_binary(
                BinOp::Le,
                super::builtins::apply("abs", &[Num(x - y)]),
                eps.clone(),
            )
        });
    }
    if v_is_list(a) && v_is_list(b) {
        let av = super::iter_values(a);
        let bv = super::iter_values(b);
        if av.len() == bv.len() {
            for (aa, bb) in av.iter().zip(bv.iter()) {
                let mismatch = if let (Num(x), Num(y)) = (aa, bb)
                    && !x.is_nan()
                    && !y.is_nan()
                {
                    if let Num(e) = eps {
                        (x - y).abs() > *e
                    } else {
                        super::ops::apply_binary(
                            BinOp::Gt,
                            super::builtins::apply("abs", &[Num(x - y)]),
                            eps.clone(),
                        )
                        .is_truthy()
                    }
                } else {
                    !approx_val(aa, bb, eps)?.is_truthy()
                };
                if mismatch {
                    return Ok(Bool(false)); // one collected entry → `[] == [..]` is false
                }
            }
            return Ok(Bool(true));
        }
    }
    Ok(Bool(false))
}

/// BOSL2 `posmod(x,m)` — the always-positive modulo. The assert passes iff both are finite numbers and
/// `approx(m,0)` (default eps) is false — i.e. `|m| > 1e-9`; then `(x%m+m)%m` routes through the
/// interpreter's own `%`/`+`.
fn posmod(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let m = args.get(1).cloned().unwrap_or(Value::Undef);
    let ok = matches!(&x, Value::Num(n) if n.is_finite())
        && matches!(&m, Value::Num(n) if n.is_finite() && n.abs() > 1e-9);
    if !ok {
        return Err(bosl_assert(
            "posmod: input must be finite numbers, divisor nonzero",
        ));
    }
    let r = super::ops::apply_binary(BinOp::Mod, x, m.clone());
    let r = super::ops::apply_binary(BinOp::Add, r, m.clone());
    Ok(super::ops::apply_binary(BinOp::Mod, r, m))
}

/// BOSL2 `idx(list, s=0, e=-1, step=1)` — the index RANGE of a list (`[0:1:len-1]` for the defaults; an
/// empty list yields the empty `[0:1:-1]`). Start/end wrap through the real [`posmod`] (so its assert raises
/// on a non-finite `s`/`e` exactly like the reference), the range builds through the interpreter's
/// `build_range`.
fn idx(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    if !(v_is_list(&list) || matches!(list, Value::Str(_))) {
        return Err(bosl_assert("idx: invalid input"));
    }
    let ll = super::builtins::apply("len", &[list]);
    let s = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    let e = args.get(2).cloned().unwrap_or(Value::Num(-1.0));
    let step = args.get(3).cloned().unwrap_or(Value::Num(1.0));
    if matches!(ll, Value::Num(n) if n == 0.0) {
        return Ok(super::build_range(
            &Value::Num(0.0),
            &Value::Num(1.0),
            &Value::Num(-1.0),
        ));
    }
    let s2 = posmod(&[s, ll.clone()])?;
    let e2 = posmod(&[e, ll])?;
    Ok(super::build_range(&s2, &step, &e2))
}

/// The `is_vector` CORE (its first three clauses — the 1-arg semantics): a nonempty list whose every element
/// is a finite number. Shared by [`is_vector`], [`all_nonzero`]'s vector branch, and [`is_matrix`]'s row
/// check.
fn is_vector_core(v: &Value) -> bool {
    match v {
        Value::NumList(xs) => !xs.is_empty() && xs.iter().all(|x| x.is_finite()),
        Value::List(xs) => !xs.is_empty() && xs.iter().all(v_is_finite),
        _ => false,
    }
}

/// BOSL2 `all_nonzero(x, eps=_EPSILON)` — a finite scalar farther than `eps` from zero, or a vector of them.
/// Exotic `eps` routes the compares through the interpreter's ops (undef-propagation intact).
fn all_nonzero(args: &[Value]) -> crate::Result<Value> {
    let x = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    if v_is_finite(&x) {
        return Ok(match (&x, &eps) {
            (Value::Num(n), Value::Num(e)) => Value::Bool(n.abs() > *e),
            _ => super::ops::apply_binary(
                BinOp::Gt,
                super::builtins::apply("abs", std::slice::from_ref(&x)),
                eps.clone(),
            ),
        });
    }
    if !is_vector_core(&x) {
        return Ok(Value::Bool(false)); // is_vector(x) && … short-circuits
    }
    let near_zero = super::iter_values(&x)
        .into_iter()
        .any(|xx| match (&xx, &eps) {
            (Value::Num(n), Value::Num(e)) => n.abs() < *e,
            _ => super::ops::apply_binary(
                BinOp::Lt,
                super::builtins::apply("abs", std::slice::from_ref(&xx)),
                eps.clone(),
            )
            .is_truthy(),
        });
    Ok(Value::Bool(!near_zero)) // `[collected…] == []`
}

/// BOSL2 `is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON)` — THE type predicate (8.8s of self
/// time across the O.4 four). Core + the three optional clauses in reference order; the `length` assert is
/// the one raise-site; `zero` compares `norm(v) >= eps` against `!zero`; a truthy `all_nonzero` delegates to
/// the real [`all_nonzero`] with ITS default eps (the reference's inner call passes none).
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
#[allow(
    clippy::float_cmp,
    reason = "the reference's `len(v)==length` IS an exact f64 equality; a tolerance would diverge"
)]
fn is_vector(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&v) {
        return Ok(Value::Bool(false));
    }
    let n = match &v {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0, // unreachable: is_vector_core above
    } as f64;
    if let Some(length) = args.get(1)
        && !matches!(length, Value::Undef)
    {
        let Value::Num(l) = length else {
            return Err(bosl_assert("is_vector: length must be a number"));
        };
        if l.is_nan() {
            return Err(bosl_assert("is_vector: length must be a number")); // is_num(NaN) is false
        }
        if *l != n {
            return Ok(Value::Bool(false));
        }
    }
    if let Some(zero) = args.get(2)
        && !matches!(zero, Value::Undef)
    {
        let eps = args.get(4).cloned().unwrap_or(Value::Num(1e-9));
        let norm_v = super::builtins::apply("norm", std::slice::from_ref(&v));
        let cmp = match (&norm_v, &eps) {
            (Value::Num(nv), Value::Num(e)) => Value::Bool(nv >= e),
            _ => super::ops::apply_binary(BinOp::Ge, norm_v, eps),
        };
        let want = Value::Bool(!zero.is_truthy());
        if !super::ops::apply_binary(BinOp::Eq, cmp, want).is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    if let Some(anz) = args.get(3)
        && anz.is_truthy()
    {
        return all_nonzero(&[v]); // 1-arg → the reference's inner call takes all_nonzero's own default eps
    }
    Ok(Value::Bool(true))
}

/// BOSL2 `is_matrix(A,m,n,square=false)` — rectangular numeric matrix, optionally shape-pinned. Composes the
/// band's own natives: `is_vector(A[0],n)` is the fixed 2-arg call (`zero`/`all_nonzero` branches unreachable —
/// which is why this entry needs NO `_EPSILON` guard even though `is_vector`'s does), `is_consistent(A)`
/// closes it. `len(A)` participates as a TRUTHINESS value in the `m`-undef clause (`0` rows → false).
#[allow(
    clippy::cast_precision_loss,
    reason = "matches the `len` builtin's `count as f64`; a list past 2^52 elements is unreachable"
)]
fn is_matrix(args: &[Value]) -> crate::Result<Value> {
    let a = args.first().cloned().unwrap_or(Value::Undef);
    if !v_is_list(&a) {
        return Ok(Value::Bool(false));
    }
    let la = match &a {
        Value::NumList(xs) => xs.len(),
        Value::List(xs) => xs.len(),
        _ => 0, // unreachable: v_is_list above
    } as f64;
    let rows_ok = match args.get(1) {
        None | Some(Value::Undef) => la != 0.0, // (is_undef(m) && len(A)) — Num truthiness
        Some(m) => super::ops::apply_binary(BinOp::Eq, Value::Num(la), m.clone()).is_truthy(),
    };
    if !rows_ok {
        return Ok(Value::Bool(false));
    }
    let a0 = super::ops::index(a.clone(), &Value::Num(0.0));
    if let Some(square) = args.get(3)
        && square.is_truthy()
    {
        let l0 = super::builtins::apply("len", std::slice::from_ref(&a0));
        if !super::ops::apply_binary(BinOp::Eq, Value::Num(la), l0).is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    let n = args.get(2).cloned().unwrap_or(Value::Undef);
    if !is_vector(&[a0, n])?.is_truthy() {
        return Ok(Value::Bool(false));
    }
    is_consistent(&[a])
}

/// A finite-or-not 2D point view: `Some([x, y])` iff the value is a 2-element `NumList` (any bits — the
/// f64 formulas below are bit-faithful for inf/NaN too, so no finiteness gate).
fn as_p2(v: &Value) -> Option<[f64; 2]> {
    match v {
        Value::NumList(xs) if xs.len() == 2 => Some([xs[0], xs[1]]),
        _ => None,
    }
}

/// The `_tri_class` scalar core on three 2D points — EXACTLY the reference's arithmetic: `crx = cross(
/// tri[1]-tri[2], tri[0]-tri[2])` with the builtins' own formulas (2D cross `a0*b1 - a1*b0`, `norm` =
/// sequential sum-of-squares sqrt), the tolerance product left-associated (`(eps*n1)*n2`), `sign` with 0 at
/// NaN.
fn tri_class_2d(t0: [f64; 2], t1: [f64; 2], t2: [f64; 2], eps: f64) -> f64 {
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
fn tri_class(args: &[Value]) -> crate::Result<Value> {
    let tri = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    Ok(tri_class_val(&tri, &eps))
}
fn tri_class_val(tri: &Value, eps: &Value) -> Value {
    if let (Value::Num(e), Value::List(xs)) = (eps, tri)
        && xs.len() == 3
        && let (Some(t0), Some(t1), Some(t2)) = (as_p2(&xs[0]), as_p2(&xs[1]), as_p2(&xs[2]))
    {
        return Value::Num(tri_class_2d(t0, t1, t2, *e));
    }
    let t0 = super::ops::index(tri.clone(), &Value::Num(0.0));
    let t1 = super::ops::index(tri.clone(), &Value::Num(1.0));
    let t2 = super::ops::index(tri.clone(), &Value::Num(2.0));
    let a = super::ops::apply_binary(BinOp::Sub, t1, t2.clone());
    let b = super::ops::apply_binary(BinOp::Sub, t0, t2);
    let crx = super::builtins::apply("cross", &[a.clone(), b.clone()]);
    let bound = super::ops::apply_binary(
        BinOp::Mul,
        super::ops::apply_binary(
            BinOp::Mul,
            eps.clone(),
            super::builtins::apply("norm", std::slice::from_ref(&a)),
        ),
        super::builtins::apply("norm", std::slice::from_ref(&b)),
    );
    let near = super::ops::apply_binary(
        BinOp::Le,
        super::builtins::apply("abs", std::slice::from_ref(&crx)),
        bound,
    );
    if near.is_truthy() {
        Value::Num(0.0)
    } else {
        super::builtins::apply("sign", std::slice::from_ref(&crx))
    }
}

/// BOSL2 `_is_at_left(pt,line,eps=_EPSILON) = _tri_class([pt,line[0],line[1]],eps) <= 0` — is `pt` left of
/// (or on) the directed 2D line? The routed tail builds the triangle with the interpreter's `build_vector`
/// (three Nums would coalesce to a `NumList` exactly like the literal would).
fn is_at_left(args: &[Value]) -> crate::Result<Value> {
    let pt = args.first().cloned().unwrap_or(Value::Undef);
    let line = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Num(1e-9));
    Ok(is_at_left_val(&pt, &line, &eps))
}
fn is_at_left_val(pt: &Value, line: &Value, eps: &Value) -> Value {
    if let (Value::Num(e), Some(p), Value::List(ls)) = (eps, as_p2(pt), line)
        && ls.len() == 2
        && let (Some(l0), Some(l1)) = (as_p2(&ls[0]), as_p2(&ls[1]))
    {
        return Value::Bool(tri_class_2d(p, l0, l1, *e) <= 0.0);
    }
    let l0 = super::ops::index(line.clone(), &Value::Num(0.0));
    let l1 = super::ops::index(line.clone(), &Value::Num(1.0));
    let tri = super::build_vector(vec![pt.clone(), l0, l1]);
    super::ops::apply_binary(BinOp::Le, tri_class_val(&tri, eps), Value::Num(0.0))
}

/// BOSL2 `_none_inside(idxs,poly,p0,p1,p2,eps,i=0)` — the ear-cut containment scan: is NO polygon vertex
/// (of `idxs`) blocking the candidate ear `[p0,p1,p2]`? The reference's tail recursion becomes a loop with
/// the same early-exit `false`; neighbor lookups go through the REAL native [`select`] (whose asserts are
/// what terminates the exotic-input shapes — a non-list `idxs` or non-numeric `i` raises there exactly like
/// the interpreter). Per-iteration fast path when everything is 2D + numeric; any shape break routes that
/// iteration through the same builtins/ops the body would run.
fn none_inside(args: &[Value]) -> crate::Result<Value> {
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
            tri_class_val(
                &super::build_vector(vec![a.clone(), b.clone(), c.clone()]),
                eps,
            )
        }
    };
    // `_is_at_left(pt,[la,lb],eps)` as the body composes it.
    let left = |pt: &Value, la: &Value, lb: &Value, eps: &Value| -> Value {
        if let (Value::Num(e), Some(p), Some(a), Some(b)) = (eps, as_p2(pt), as_p2(la), as_p2(lb)) {
            Value::Bool(tri_class_2d(p, a, b, *e) <= 0.0)
        } else {
            is_at_left_val(pt, &super::build_vector(vec![la.clone(), lb.clone()]), eps)
        }
    };

    loop {
        let ll = super::builtins::apply("len", std::slice::from_ref(&idxs));
        if super::ops::apply_binary(BinOp::Ge, i.clone(), ll).is_truthy() {
            return Ok(Value::Bool(true));
        }
        let vert = super::ops::index(poly.clone(), &super::ops::index(idxs.clone(), &i));
        let prev = super::ops::index(
            poly.clone(),
            &select(&[
                idxs.clone(),
                super::ops::apply_binary(BinOp::Sub, i.clone(), Value::Num(1.0)),
            ])?,
        );
        let next = super::ops::index(
            poly.clone(),
            &select(&[
                idxs.clone(),
                super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0)),
            ])?,
        );
        // reflex && (inside-the-ear || touches-p1-and-crosses) ? false : next i — short-circuits preserved.
        let reflex =
            super::ops::apply_binary(BinOp::Le, tc(&prev, &vert, &next, &eps), Value::Num(0.0));
        if reflex.is_truthy() {
            let inside =
                super::ops::apply_binary(BinOp::Gt, tc(&p0, &p1, &vert, &eps), Value::Num(0.0))
                    .is_truthy()
                    && super::ops::apply_binary(
                        BinOp::Gt,
                        tc(&p1, &p2, &vert, &eps),
                        Value::Num(0.0),
                    )
                    .is_truthy()
                    && super::ops::apply_binary(
                        BinOp::Ge,
                        tc(&p2, &p0, &vert, &eps),
                        Value::Num(0.0),
                    )
                    .is_truthy();
            let blocking = inside || {
                let d = super::ops::apply_binary(BinOp::Sub, vert.clone(), p1.clone());
                super::ops::apply_binary(
                    BinOp::Lt,
                    super::builtins::apply("norm", std::slice::from_ref(&d)),
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
        i = super::ops::apply_binary(BinOp::Add, i, Value::Num(1.0));
    }
}

/// BOSL2 `force_list(value, n=1, fill)` — a list passes through; a scalar becomes `n` copies (or
/// `[value, fill, fill, …]` when `fill` is given). The repeat counts come from iterating the reference's own
/// ranges (`[1:1:n]` / `[2:1:n]`) built with the interpreter's `build_range` — so a garbage `n` degenerates
/// exactly as interpreted instead of needing its own numeric validation.
fn force_list(args: &[Value]) -> crate::Result<Value> {
    let value = args.first().cloned().unwrap_or(Value::Undef);
    if v_is_list(&value) {
        return Ok(value);
    }
    let n = args.get(1).cloned().unwrap_or(Value::Num(1.0));
    let one = Value::Num(1.0);
    match args.get(2) {
        None | Some(Value::Undef) => {
            let range = super::build_range(&one, &one, &n);
            let out: Vec<Value> = super::iter_values(&range)
                .iter()
                .map(|_| value.clone())
                .collect();
            Ok(super::build_vector(out))
        }
        Some(fill) => {
            let range = super::build_range(&Value::Num(2.0), &one, &n);
            let mut out = vec![value];
            out.extend(super::iter_values(&range).iter().map(|_| fill.clone()));
            Ok(super::build_vector(out))
        }
    }
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

/// `(fingerprint, entry)` for every registry entry, computed ONCE by parsing each `reference` and
/// fingerprinting its `(params, body)`. Lazy + cached: the parse cost is paid the first time an intrinsic is
/// looked up in the process, never per call. A `reference` that doesn't parse to a single `function` def is
/// a registry BUG — it's dropped with a debug assert rather than silently mis-registering.
fn table() -> &'static [(u64, &'static Entry)] {
    static TABLE: OnceLock<Vec<(u64, &'static Entry)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        REGISTRY
            .iter()
            .filter_map(|entry| Some((reference_fingerprint(entry.reference)?, entry)))
            .collect()
    })
}

/// The [`PINS`] fingerprints, same lazy shape as [`table`].
fn pin_table() -> &'static [(&'static str, u64)] {
    static TABLE: OnceLock<Vec<(&'static str, u64)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        PINS.iter()
            .filter_map(|&(name, reference)| Some((name, reference_fingerprint(reference)?)))
            .collect()
    })
}

/// The reference fingerprint a DEP name must match to satisfy an entry's dep pin: the dep's own registry
/// entry if it has one, else its [`PINS`] row. `None` = the dep isn't anchored anywhere — a registry
/// authoring bug the depending entry then never wires over.
#[must_use]
pub(super) fn anchor_fp(name: &str) -> Option<u64> {
    table()
        .iter()
        .find(|(_, e)| e.name == name)
        .map(|(fp, _)| *fp)
        .or_else(|| {
            pin_table()
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, fp)| *fp)
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

/// Resolve a defined function to its registry entry, if one is registered for EXACTLY this body. Called ONCE
/// per function at [`super::build_ctx`] time (never per call): fingerprint the running `(params, body)`,
/// then match on (name, fingerprint). A miss — no entry for the name, or the name matches but the body
/// doesn't — returns `None`, so the interpreter runs the real body. This is the never-silently-wrong gate's
/// FIRST hop; the caller must still clear the entry's `deps`/`builtins` guards (and, for a non-empty
/// `consts`, arm post-hoist) before wiring `func` — see `super::build_intrinsics` /
/// `super::arm_guarded_intrinsics`.
#[must_use]
pub(super) fn resolve(name: &str, params: &[Parameter], body: &Expr) -> Option<&'static Entry> {
    let fp = fingerprint(params, body);
    table()
        .iter()
        .find(|(f, e)| e.name == name && *f == fp)
        .map(|(_, e)| *e)
}

/// Test-only access to a registry entry's reference source, for the fast==slow harness.
#[cfg(test)]
pub(super) fn reference_of(name: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.reference)
}

/// Test-only access to a PIN's reference source (dep-guard tests assemble programs from these).
#[cfg(test)]
pub(super) fn pin_reference_of(name: &str) -> Option<&'static str> {
    PINS.iter().find(|(n, _)| *n == name).map(|(_, r)| *r)
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
    // Fingerprint-level truth: a guarded match is WIRED here (the source matched); whether its deps/consts
    // guards then clear is a separate, per-program verdict the build/arm steps print under the same EXPLAIN.
    if resolve(name, params, body).is_some() {
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
        .find(|(_, e)| e.name == name)
        .map(|(fp, _)| *fp)
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
    use super::{fingerprint, pin_reference_of, poc_sq, reference_of, resolve};
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

    /// The full oracle: deps AND top-level consts — a reference whose DEFAULT reads `_EPSILON` (approx,
    /// is_vector…) needs the constant bound BEFORE params bind, exactly like the real definition scope (the
    /// island global) provides it. Same clear-intrinsics contract as [`interpret_with_deps`].
    fn interpret_with_deps_consts(
        target: &str,
        deps: &[&str],
        consts: &[(&str, Value)],
        inputs: &[Value],
    ) -> crate::Result<Value> {
        let src = format!("{}\n{target}", deps.join("\n"));
        let program = parse(&src).expect("deps+target parse");
        let mut ctx = build_ctx(&program, crate::Config::default());
        ctx.intrinsics.clear();
        let (params, body) = match &program.stmts.last().expect("has target").kind {
            StmtKind::FunctionDef { params, body, .. } => (params, body),
            other => panic!("target is not a function def: {other:?}"),
        };
        let mut scope = Scope::new();
        for (name, v) in consts {
            scope.bind((*name).to_string(), v.clone());
        }
        // PUBLISH the consts as island 0's global too — a DEP's defaults (approx's `eps=_EPSILON` when
        // posmod calls it) evaluate against the callee's home-island global, not the caller's scope. In a
        // real program both are the same hoisted global; the oracle must mirror that or a dep's default
        // silently reads undef (caught by the posmod battery).
        if let Some(slot) = ctx.island_globals.borrow_mut().first_mut() {
            *slot = scope.clone();
        }
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

    /// The shape band's richer battery: everything in [`value_battery`] plus the nested/mixed/undef-bearing
    /// shapes `_list_pattern`/`is_consistent`/`same_shape` actually discriminate on.
    fn shape_battery() -> Vec<Value> {
        let mut b = value_battery();
        b.extend([
            Value::list(vec![
                Value::num_list(vec![1.0, 2.0]),
                Value::num_list(vec![3.0, 4.0]),
            ]),
            Value::list(vec![
                Value::num_list(vec![1.0]),
                Value::list(vec![Value::Num(2.0), Value::string("a")]),
            ]),
            Value::list(vec![Value::Num(1.0), Value::num_list(vec![2.0])]),
            Value::list(vec![Value::Undef, Value::Num(1.0), Value::Undef]),
            Value::list(vec![Value::string("x"), Value::string("y")]),
            Value::list(vec![Value::list(vec![])]),
            Value::num_list(vec![0.0, -0.0]),
        ]);
        b
    }

    #[test]
    fn fast_equals_slow_shape_band() {
        // The O.5.2 shape band, whole-battery: 1-arg fns over every battery value, 2-arg fns over every
        // PAIR (shape comparisons are about how two inputs relate). interpret_with_deps supplies the
        // recursive/dep definitions; deps=[] still resolves self-recursion (build_ctx sees the target).
        let battery = shape_battery();
        let lp_ref = reference_of("_list_pattern").unwrap();
        for v in &battery {
            let args = [v.clone()];
            assert!(
                same_result(
                    &super::list_pattern(&args),
                    &interpret_with_deps(lp_ref, &[], &args)
                ),
                "_list_pattern diverged on {v:?}"
            );
            let nd_ref = reference_of("num_defined").unwrap();
            assert!(
                same_result(
                    &super::num_defined(&args),
                    &interpret_with_deps(nd_ref, &[], &args)
                ),
                "num_defined diverged on {v:?}"
            );
        }
        let ss_ref = reference_of("same_shape").unwrap();
        let ss_deps = [reference_of("is_def").unwrap(), lp_ref];
        let ic_ref = reference_of("is_consistent").unwrap();
        for a in &battery {
            for b in &battery {
                let args = [a.clone(), b.clone()];
                assert!(
                    same_result(
                        &super::same_shape(&args),
                        &interpret_with_deps(ss_ref, &ss_deps, &args)
                    ),
                    "same_shape diverged on ({a:?}, {b:?})"
                );
                assert!(
                    same_result(
                        &super::is_consistent(&args),
                        &interpret_with_deps(ic_ref, &[lp_ref], &args)
                    ),
                    "is_consistent diverged on ({a:?}, {b:?})"
                );
            }
            // the 1-arg form (pattern defaults to list[0]'s shape) — the overwhelmingly common call
            let args = [a.clone()];
            assert!(
                same_result(
                    &super::is_consistent(&args),
                    &interpret_with_deps(ic_ref, &[lp_ref], &args)
                ),
                "is_consistent/1 diverged on {a:?}"
            );
        }
        let fl_ref = reference_of("force_list").unwrap();
        let ns = [
            Value::Undef,
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Num(3.0),
            Value::Num(-1.0),
            Value::Num(2.5),
            Value::string("x"),
        ];
        let fills = [Value::Undef, Value::Num(7.0), Value::string("f")];
        for v in &battery {
            for n in &ns {
                for fill in &fills {
                    let args = [v.clone(), n.clone(), fill.clone()];
                    assert!(
                        same_result(
                            &super::force_list(&args),
                            &interpret_with_deps(fl_ref, &[], &args)
                        ),
                        "force_list diverged on ({v:?}, {n:?}, {fill:?})"
                    );
                }
            }
            let args = [v.clone()]; // defaults: n=1, fill undef
            assert!(
                same_result(
                    &super::force_list(&args),
                    &interpret_with_deps(fl_ref, &[], &args)
                ),
                "force_list/1 diverged on {v:?}"
            );
        }
    }

    /// The `_EPSILON` family's battery: numeric edges around the 1e-9 tolerance, vectors with NaN/inf
    /// poison, near-zero vectors, plus every non-vector shape from the base battery.
    fn eps_battery() -> Vec<Value> {
        let mut b = shape_battery();
        b.extend([1e-10, -1e-10, 1e-9, 2e-9, 1.0 + 1e-10, 0.5, -2.5, 1e12].map(Value::Num));
        b.extend([
            Value::num_list(vec![0.0, 0.0]),
            Value::num_list(vec![1e-10, 1.0]),
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::num_list(vec![1.0, f64::NAN]),
            Value::num_list(vec![1.0, f64::INFINITY]),
            Value::list(vec![Value::Num(1.0), Value::string("a")]),
        ]);
        b
    }

    #[test]
    fn fast_equals_slow_epsilon_family() {
        let consts = [("_EPSILON", Value::Num(1e-9))];
        let battery = eps_battery();
        let refs = |names: &[&str]| -> Vec<&'static str> {
            names.iter().map(|n| reference_of(n).expect(n)).collect()
        };
        let epses = [
            None,
            Some(Value::Num(1e-9)),
            Some(Value::Num(0.5)),
            Some(Value::Undef),
            Some(Value::string("x")),
        ];

        // approx(a,b[,eps]) — every pair × every eps shape (the recursion + NaN routing live here).
        let approx_ref = reference_of("approx").unwrap();
        let approx_deps = refs(&["idx", "posmod", "is_finite", "is_nan"]);
        for a in &battery {
            for b in &battery {
                for eps in &epses {
                    let mut args = vec![a.clone(), b.clone()];
                    if let Some(e) = eps {
                        args.push(e.clone());
                    }
                    assert!(
                        same_result(
                            &super::approx(&args),
                            &interpret_with_deps_consts(approx_ref, &approx_deps, &consts, &args)
                        ),
                        "approx diverged on ({a:?}, {b:?}, eps {eps:?})"
                    );
                }
            }
        }

        // posmod(x,m) — the assert-heavy one: both raise-sites and the wrap arithmetic.
        let posmod_ref = reference_of("posmod").unwrap();
        let posmod_deps = refs(&["is_finite", "is_nan", "approx", "idx"]);
        let nums = [
            Value::Num(0.0),
            Value::Num(-0.0),
            Value::Num(1e-10),
            Value::Num(-1e-10),
            Value::Num(5.0),
            Value::Num(-5.0),
            Value::Num(2.5),
            Value::Num(-7.25),
            Value::Num(f64::INFINITY),
            Value::Num(f64::NAN),
            Value::Undef,
            Value::string("m"),
            Value::num_list(vec![1.0]),
        ];
        for x in &nums {
            for m in &nums {
                let args = [x.clone(), m.clone()];
                assert!(
                    same_result(
                        &super::posmod(&args),
                        &interpret_with_deps_consts(posmod_ref, &posmod_deps, &consts, &args)
                    ),
                    "posmod diverged on ({x:?}, {m:?})"
                );
            }
        }

        // idx(list[,s,e,step]) — range identity (bit_eq compares Range fields) + the two raise-sites.
        let idx_ref = reference_of("idx").unwrap();
        let idx_deps = refs(&["posmod", "is_finite", "is_nan", "approx"]);
        let arg_sets: Vec<Vec<Value>> = vec![
            vec![],
            vec![Value::Num(1.0)],
            vec![Value::Num(1.0), Value::Num(-2.0)],
            vec![Value::Num(0.0), Value::Num(-1.0), Value::Num(2.0)],
            vec![Value::string("s")],
            vec![Value::Undef],
        ];
        for v in &battery {
            for tail in &arg_sets {
                let mut args = vec![v.clone()];
                args.extend(tail.iter().cloned());
                assert!(
                    same_result(
                        &super::idx(&args),
                        &interpret_with_deps_consts(idx_ref, &idx_deps, &consts, &args)
                    ),
                    "idx diverged on ({v:?}, tail {tail:?})"
                );
            }
        }

        // all_nonzero(x[,eps]).
        let anz_ref = reference_of("all_nonzero").unwrap();
        let anz_deps = refs(&["is_finite", "is_nan", "is_vector"]);
        for v in &battery {
            for eps in &epses {
                let mut args = vec![v.clone()];
                if let Some(e) = eps {
                    args.push(e.clone());
                }
                assert!(
                    same_result(
                        &super::all_nonzero(&args),
                        &interpret_with_deps_consts(anz_ref, &anz_deps, &consts, &args)
                    ),
                    "all_nonzero diverged on ({v:?}, eps {eps:?})"
                );
            }
        }

        // is_vector(v[,length,zero,all_nonzero,eps]) — clause-by-clause arg shapes over the battery.
        let iv_ref = reference_of("is_vector").unwrap();
        let iv_deps = refs(&["is_finite", "is_nan", "all_nonzero"]);
        let lengths = [
            Value::Undef,
            Value::Num(2.0),
            Value::Num(3.0),
            Value::string("L"),
            Value::Num(f64::NAN),
        ];
        let zeros = [Value::Undef, Value::Bool(true), Value::Bool(false)];
        let anzs = [Value::Bool(false), Value::Bool(true)];
        for v in &battery {
            for length in &lengths {
                let args = [v.clone(), length.clone()];
                assert!(
                    same_result(
                        &super::is_vector(&args),
                        &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                    ),
                    "is_vector diverged on ({v:?}, length {length:?})"
                );
            }
            for zero in &zeros {
                for eps in [Value::Num(1e-9), Value::Num(0.5), Value::Undef] {
                    let args = [
                        v.clone(),
                        Value::Undef,
                        zero.clone(),
                        Value::Bool(false),
                        eps.clone(),
                    ];
                    assert!(
                        same_result(
                            &super::is_vector(&args),
                            &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                        ),
                        "is_vector diverged on ({v:?}, zero {zero:?}, eps {eps:?})"
                    );
                }
            }
            for anz in &anzs {
                let args = [v.clone(), Value::Undef, Value::Undef, anz.clone()];
                assert!(
                    same_result(
                        &super::is_vector(&args),
                        &interpret_with_deps_consts(iv_ref, &iv_deps, &consts, &args)
                    ),
                    "is_vector diverged on ({v:?}, all_nonzero {anz:?})"
                );
            }
        }

        // is_matrix(A[,m,n,square]).
        let im_ref = reference_of("is_matrix").unwrap();
        let im_deps = refs(&[
            "is_vector",
            "is_finite",
            "is_nan",
            "is_consistent",
            "_list_pattern",
        ]);
        let mut mats = battery.clone();
        mats.extend([
            Value::list(vec![
                Value::num_list(vec![1.0, 2.0]),
                Value::num_list(vec![3.0, 4.0]),
            ]),
            Value::list(vec![
                Value::num_list(vec![1.0, 2.0]),
                Value::num_list(vec![3.0]),
            ]),
            Value::list(vec![
                Value::num_list(vec![1.0, 2.0, 5.0]),
                Value::num_list(vec![3.0, 4.0, 6.0]),
            ]),
        ]);
        let ms = [Value::Undef, Value::Num(2.0), Value::Num(3.0)];
        let ns = [Value::Undef, Value::Num(2.0), Value::string("n")];
        let squares = [Value::Bool(false), Value::Bool(true)];
        for a in &mats {
            for m in &ms {
                for n in &ns {
                    for square in &squares {
                        let args = [a.clone(), m.clone(), n.clone(), square.clone()];
                        assert!(
                            same_result(
                                &super::is_matrix(&args),
                                &interpret_with_deps_consts(im_ref, &im_deps, &consts, &args)
                            ),
                            "is_matrix diverged on ({a:?}, m {m:?}, n {n:?}, square {square:?})"
                        );
                    }
                }
            }
        }
    }

    /// A 2D point as the interpreter builds it.
    fn p2(x: f64, y: f64) -> Value {
        Value::num_list(vec![x, y])
    }

    #[test]
    fn fast_equals_slow_earcut_band() {
        let consts = [("_EPSILON", Value::Num(1e-9))];
        let tc_ref = reference_of("_tri_class").unwrap();
        let al_ref = reference_of("_is_at_left").unwrap();
        let ni_ref = reference_of("_none_inside").unwrap();
        let al_deps = [tc_ref];
        let ni_deps = [
            reference_of("select").unwrap(),
            tc_ref,
            al_ref,
            reference_of("is_vector").unwrap(),
            pin_reference_of("is_range").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
        ];

        // _tri_class: CW / CCW / collinear / near-collinear-within-eps triangles, 3D points (→ undef),
        // degenerate shapes, exotic eps.
        let tris = [
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(1.0, 0.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 1.0), p2(2.0, 2.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 1e-12), p2(2.0, 0.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 1e-3), p2(2.0, 0.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(0.0, 0.0), p2(1.0, 1.0)]),
            Value::list(vec![
                Value::num_list(vec![0.0, 0.0, 0.0]),
                Value::num_list(vec![1.0, 0.0, 0.0]),
                Value::num_list(vec![0.0, 1.0, 0.0]),
            ]),
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0)]),
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::Undef,
            Value::string("tri"),
            Value::list(vec![p2(f64::NAN, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
            Value::list(vec![p2(f64::INFINITY, 0.0), p2(1.0, 0.0), p2(0.0, 1.0)]),
        ];
        let epses = [
            None,
            Some(Value::Num(1e-9)),
            Some(Value::Num(0.1)),
            Some(Value::Undef),
            Some(Value::string("e")),
        ];
        for tri in &tris {
            for eps in &epses {
                let mut args = vec![tri.clone()];
                if let Some(e) = eps {
                    args.push(e.clone());
                }
                assert!(
                    same_result(
                        &super::tri_class(&args),
                        &interpret_with_deps_consts(tc_ref, &[], &consts, &args)
                    ),
                    "_tri_class diverged on ({tri:?}, eps {eps:?})"
                );
            }
        }

        // _is_at_left: points against directed segments, incl. on-the-line and exotic shapes.
        let pts = [
            p2(0.0, 1.0),
            p2(0.0, -1.0),
            p2(0.5, 0.0),
            p2(f64::NAN, 0.0),
            Value::Undef,
            Value::Num(3.0),
        ];
        let lines = [
            Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0)]),
            Value::list(vec![p2(1.0, 0.0), p2(0.0, 0.0)]),
            Value::list(vec![p2(0.0, 0.0), p2(0.0, 0.0)]),
            Value::list(vec![p2(0.0, 0.0)]),
            Value::Undef,
        ];
        for pt in &pts {
            for line in &lines {
                for eps in &epses {
                    let mut args = vec![pt.clone(), line.clone()];
                    if let Some(e) = eps {
                        args.push(e.clone());
                    }
                    assert!(
                        same_result(
                            &super::is_at_left(&args),
                            &interpret_with_deps_consts(al_ref, &al_deps, &consts, &args)
                        ),
                        "_is_at_left diverged on ({pt:?}, {line:?}, eps {eps:?})"
                    );
                }
            }
        }

        // _none_inside: real ear-scan shapes over a CW L-polygon (concave), incl. an ear a reflex vertex
        // blocks, a duplicate-vertex polygon (the norm(vert-p1)<eps arm), the i-offset start, and the
        // exotic-input raise paths (non-list idxs / NaN i → select's asserts fire on BOTH sides).
        let lpoly = Value::list(vec![
            p2(0.0, 0.0),
            p2(0.0, 2.0),
            p2(1.0, 2.0),
            p2(1.0, 1.0),
            p2(2.0, 1.0),
            p2(2.0, 0.0),
        ]);
        let sq = Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(1.0, 1.0), p2(1.0, 0.0)]);
        let dup = Value::list(vec![p2(0.0, 0.0), p2(0.0, 1.0), p2(0.0, 1.0), p2(1.0, 0.0)]);
        let all6 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        let all4 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0]);
        let e9 = Value::Num(1e-9);
        let cases: Vec<Vec<Value>> = vec![
            // (idxs, poly, p0, p1, p2, eps[, i])
            vec![
                all6.clone(),
                lpoly.clone(),
                p2(0.0, 0.0),
                p2(0.0, 2.0),
                p2(1.0, 2.0),
                e9.clone(),
            ],
            vec![
                all6.clone(),
                lpoly.clone(),
                p2(1.0, 2.0),
                p2(1.0, 1.0),
                p2(2.0, 1.0),
                e9.clone(),
            ],
            vec![
                all6.clone(),
                lpoly.clone(),
                p2(2.0, 1.0),
                p2(2.0, 0.0),
                p2(0.0, 0.0),
                e9.clone(),
            ],
            vec![
                all4.clone(),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                e9.clone(),
            ],
            vec![
                all4.clone(),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                e9.clone(),
                Value::Num(2.0),
            ],
            vec![
                all4.clone(),
                dup.clone(),
                p2(0.0, 1.0),
                p2(0.0, 1.0),
                p2(1.0, 0.0),
                e9.clone(),
            ],
            vec![
                Value::num_list(vec![]),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                e9.clone(),
            ],
            // exotic: eps undef, idxs non-list (select raises), i NaN (select raises)
            vec![
                all4.clone(),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                Value::Undef,
            ],
            vec![
                Value::Num(7.0),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                e9.clone(),
            ],
            vec![
                all4.clone(),
                sq.clone(),
                p2(0.0, 0.0),
                p2(0.0, 1.0),
                p2(1.0, 1.0),
                e9.clone(),
                Value::Num(f64::NAN),
            ],
            // 3D polygon: every tri_class degrades to undef exactly as interpreted
            vec![
                Value::num_list(vec![0.0, 1.0, 2.0]),
                Value::list(vec![
                    Value::num_list(vec![0.0, 0.0, 0.0]),
                    Value::num_list(vec![1.0, 0.0, 0.0]),
                    Value::num_list(vec![0.0, 1.0, 0.0]),
                ]),
                Value::num_list(vec![0.0, 0.0, 0.0]),
                Value::num_list(vec![1.0, 0.0, 0.0]),
                Value::num_list(vec![0.0, 1.0, 0.0]),
                e9.clone(),
            ],
        ];
        for args in &cases {
            assert!(
                same_result(
                    &super::none_inside(args),
                    &interpret_with_deps_consts(ni_ref, &ni_deps, &consts, args)
                ),
                "_none_inside diverged on {args:?}"
            );
        }
    }

    #[test]
    fn a_const_guarded_entry_resolves_with_its_guard_attached() {
        // The build-time gate reads `consts` off the resolved entry: non-empty means build_intrinsics skips
        // it (it arms post-hoist), and the guard travels with the entry for the arm step to verify.
        let (p, b) = parse_fn(reference_of("_fab_poc_near0").unwrap());
        let entry = resolve("_fab_poc_near0", &p, &b).expect("exact fingerprint resolves");
        assert_eq!(entry.consts, &[("_EPSILON", 1e-9)]);
        assert!(
            resolve("_fab_poc_sq", &p, &b).is_none(),
            "same body, different name → no entry"
        );
        // The pin anchors resolve too — a dep check needs their fingerprints.
        assert!(
            super::anchor_fp("is_range").is_some(),
            "PINS must anchor is_range"
        );
        assert!(
            super::anchor_fp("no_such_fn").is_none(),
            "an unanchored name is a registry authoring bug the dep check declines over"
        );
    }

    #[test]
    fn the_fingerprint_gate_matches_only_the_exact_body() {
        // Never silently wrong: the intrinsic registers for the EXACT reference, and misses on any
        // perturbation (different body) or a name mismatch → the interpreter runs the real body instead.
        let (p, b) = parse_fn(reference_of("_fab_poc_sq").unwrap());
        assert!(
            resolve("_fab_poc_sq", &p, &b).is_some(),
            "the exact reference must register"
        );

        let (p2, b2) = parse_fn("function _fab_poc_sq(x) = x + x;");
        assert!(
            resolve("_fab_poc_sq", &p2, &b2).is_none(),
            "a changed body must NOT match"
        );

        let (p3, b3) = parse_fn("function _fab_poc_sq(x, y) = x * x;");
        assert!(
            resolve("_fab_poc_sq", &p3, &b3).is_none(),
            "a changed arity must NOT match"
        );

        assert!(
            resolve("some_other_name", &p, &b).is_none(),
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
            let func = resolve(name, &params, &body)
                .expect("its own reference must register")
                .func;
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
        let func = resolve("is_nan", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("is_finite", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("last", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("default", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("_is_liststr", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("point3d", &params, &body)
            .expect("its own reference must register")
            .func;
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
        let func = resolve("select", &params, &body)
            .expect("its own reference must register")
            .func;
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
