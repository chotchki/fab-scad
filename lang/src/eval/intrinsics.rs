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
    // vnf.scad — `_vnf_centroid`'s structural assert.
    (
        "is_vnf",
        "function is_vnf(x) =
    is_list(x) &&
    len(x)==2 &&
    is_list(x[0]) &&
    is_list(x[1]) &&
    (x[0]==[] || (len(x[0])>=3 && is_vector(x[0][0],3))) &&
    (x[1]==[] || is_vector(x[1][0]));",
    ),
    // math.scad — `vector_angle`'s acos-domain clamp. Only its `is_num` branch (and the assert-false tail
    // its predicate chain reaches on undef/NaN) is reachable there, but the pin covers the whole body — so
    // `flatten`/`list_to_matrix` stay unreachable as long as the pinned `is_matrix` answers false for
    // non-lists.
    (
        "constrain",
        "function constrain(v, minval, maxval) =
    is_num(v) ? max(minval, min(v, maxval))
    : is_vector(v) ? [for(f=v) max(minval, min(f, maxval))]
    : is_matrix(v) ? let( // for a matrix, this should be more efficient than indexing
        mflat = flatten(v),
        clamped = [ for(f=mflat) max(minval, min(f, maxval)) ]
    ) list_to_matrix(clamped, len(v[0]), 0)
    : is_list(v) ? [ for(vec=v) [ for(f=vec) max(minval, min(f, maxval)) ] ]
    : assert(false, \"\\nIn constrain(), v must be a number, 1D vector, rectangular matrix, or list of vectors.\");",
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
    // ── O.5.4, the AGGREGATE/AFFINE band (math/vectors/transforms.scad) ──────────────────────────────────
    // sum+_sum 3.1s, _apply 2.2s, _bt_search 5.2s, unit 1.1s, vector_angle 1.2s across the O.4 four. These
    // are OP-level natives: the heavy lifting (`[for(i=v)1]*v` dot/matrix products, `[…]/scale`, `concat`)
    // already runs native inside apply_binary/builtins — the intrinsic erases the per-call task-stack
    // orchestration around them, which is where the interpreted time actually went.
    Entry {
        name: "_sum",
        reference: "function _sum(v,_total,_i=0) = _i>=len(v) ? _total : _sum(v,_total+v[_i], _i+1);",
        consts: &[],
        deps: &[],
        builtins: &["len"],
        func: sum_tail,
    },
    Entry {
        name: "sum",
        reference: "function sum(v, dflt=0) =
    v==[]? dflt :
    assert(is_consistent(v), \"\\nInput to sum is non-numeric or inconsistent.\")
    is_finite(v[0]) || is_vector(v[0]) ? [for(i=v) 1]*v :
    _sum(v,v[0]*0);",
        consts: &[],
        deps: &[
            "is_consistent",
            "_list_pattern",
            "is_finite",
            "is_nan",
            "is_vector",
            "_sum",
        ],
        builtins: &["is_list", "len", "is_undef", "is_num"],
        func: sum,
    },
    Entry {
        name: "unit",
        reference: "function unit(v, error=[[[\"ASSERT\"]]]) =
    assert(is_vector(v), \"\\nInvalid vector.\")
    norm(v)<_EPSILON? (error==[[[\"ASSERT\"]]]? assert(norm(v)>=_EPSILON,\"\\nCannot normalize a zero vector.\") : error) :
    v/norm(v);",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["norm", "is_list", "len", "is_undef", "is_num"],
        func: unit,
    },
    Entry {
        name: "is_2d_transform",
        reference: "function is_2d_transform(t) =    // z-parameters are zero, except we allow t[2][2]!=1 so scale() works
  t[2][0]==0 && t[2][1]==0 && t[2][3]==0 && t[0][2] == 0 && t[1][2]==0 &&
  (t[2][2]==1 || !(t[0][0]==1 && t[0][1]==0 && t[1][0]==0 && t[1][1]==1));",
        consts: &[],
        deps: &[],
        builtins: &[],
        func: is_2d_transform,
    },
    Entry {
        name: "_apply",
        reference: "function _apply(transform,points) =
    assert(is_matrix(transform),\"Invalid transformation matrix\")
    assert(is_matrix(points),\"Invalid points list\")
    let(
        tdim = len(transform[0])-1,
        datadim = len(points[0])
    )
    assert(len(transform)==tdim || len(transform)-1==tdim, \"transform matrix height not compatible with width\")
    assert(datadim==2 || datadim==3,\"Data must be 2D or 3D\")
    let(
        scale = len(transform)==tdim ? 1 : transform[tdim][tdim],
        matrix = [for(i=[0:1:tdim]) [for(j=[0:1:datadim-1]) transform[j][i]]] / scale
    )
    tdim==datadim ? [for(p=points) concat(p,1)] * matrix
  : tdim == 3 && datadim == 2 ?
            assert(is_2d_transform(transform), str(\"Transforms is 3D and acts on Z, but points are 2D\"))
            [for(p=points) concat(p,[0,1])]*matrix
  : assert(false, str(\"Unsupported combination: \",len(transform),\"x\",len(transform[0]),\" transform (dimension \",tdim,
                          \"), data of dimension \",datadim));",
        consts: &[],
        deps: &[
            "is_matrix",
            "is_vector",
            "is_finite",
            "is_nan",
            "is_consistent",
            "_list_pattern",
            "is_2d_transform",
        ],
        builtins: &["is_list", "len", "is_undef", "is_num", "concat", "str"],
        func: apply_transform,
    },
    Entry {
        name: "_bt_search",
        reference: "function _bt_search(query, r, points, tree) =
    assert( is_list(tree)
            && (   ( len(tree)==1 && is_list(tree[0]) )
                || ( len(tree)==4 && is_num(tree[0]) && is_num(tree[1]) ) ),
            \"\\nThe tree is invalid.\")
    len(tree)==1
    ?   assert( tree[0]==[] || is_vector(tree[0]), \"\\nThe tree is invalid.\" )
        [for(i=tree[0]) if(norm(points[i]-query)<=r) i ]
    :   norm(query-points[tree[0]]) > r+tree[1] ? [] :
        concat(
            [ if(norm(query-points[tree[0]])<=r) tree[0] ],
            _bt_search(query, r, points, tree[2]),
            _bt_search(query, r, points, tree[3]) ) ;",
        consts: &[],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["is_list", "len", "is_num", "norm", "concat", "is_undef"],
        func: bt_search,
    },
    // `constrain` is a dep PIN, not an entry: only its `is_num` branch is reachable here (the argument is a
    // dot-product scalar or the undef the asserts then kill), but the pin covers the whole body.
    Entry {
        name: "vector_angle",
        reference: "function vector_angle(v1,v2,v3) =
    assert( ( is_undef(v3) && ( is_undef(v2) || same_shape(v1,v2) ) )
            || is_consistent([v1,v2,v3]) ,
            \"\\nBad arguments.\")
    assert( is_vector(v1) || is_consistent(v1), \"\\nBad arguments.\")
    let( vecs = ! is_undef(v3) ? [v1-v2,v3-v2] :
                ! is_undef(v2) ? [v1,v2] :
                len(v1) == 3   ? [v1[0]-v1[1], v1[2]-v1[1]]
                               : v1
    )
    assert(is_vector(vecs[0],2) || is_vector(vecs[0],3), \"\\nBad arguments.\")
    let(
        norm0 = norm(vecs[0]),
        norm1 = norm(vecs[1])
    )
    assert(norm0>0 && norm1>0, \"\\nZero length vector.\")
    // NOTE: constrain() corrects crazy FP rounding errors that exceed acos()'s domain.
    acos(constrain((vecs[0]*vecs[1])/(norm0*norm1), -1, 1));",
        consts: &[],
        deps: &[
            "same_shape",
            "is_def",
            "_list_pattern",
            "is_consistent",
            "is_vector",
            "is_matrix",
            "is_finite",
            "is_nan",
            "constrain",
        ],
        builtins: &[
            "is_undef", "is_list", "is_num", "len", "norm", "acos", "min", "max",
        ],
        func: vector_angle,
    },
    // ── O.7, band 5 batch 1 (regions/geometry/vnf/comparisons.scad) ─────────────────────────────────────
    // The post-O.6 residual's small-body big-timers: _point_dist is 4.9s in shoe_holder alone (offset()'s
    // per-point distance scan), _vnf_centroid/_group_sort_by_index are webcam_holder's #2/#3. All fully
    // ROUTED through ops/builtins — no hand-f64 fast paths here, because `ops::dot` is 4-laned (not a
    // sequential fold) and these bodies are dot-product-heavy; the win is erasing the task-stack
    // orchestration, not the arithmetic.
    Entry {
        name: "_point_dist",
        reference: "function _point_dist(path,pathseg_unit,pathseg_len,pt) =
    min([
        for(i=[0:len(pathseg_unit)-1]) let(
            v = pt-path[i],
            projection = v*pathseg_unit[i],
            segdist = projection < 0? norm(pt-path[i]) :
                projection > pathseg_len[i]? norm(pt-select(path,i+1)) :
                norm(v-projection*pathseg_unit[i])
        ) segdist
    ]);",
        consts: &[],
        deps: &["select", "is_vector", "is_range", "is_finite", "is_nan"],
        builtins: &[
            "min", "norm", "len", "is_list", "is_string", "is_num", "is_undef",
        ],
        func: point_dist,
    },
    Entry {
        name: "_is_point_on_line",
        reference: "function _is_point_on_line(point, line, bounded=false, eps=_EPSILON) =
    let(
        v1 = (line[1]-line[0]),
        v0 = (point-line[0]),
        t  = v0*v1/(v1*v1),
        bounded = force_list(bounded,2),
        norm_crossprod = len(v1)==2 ? abs(cross(v0,v1)) : norm(cross(v0,v1))
    )
    norm_crossprod <= eps*norm(v1)
    && (!bounded[0] || t>=-eps)
    && (!bounded[1] || t<1+eps) ;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &["force_list"],
        builtins: &["abs", "cross", "norm", "len", "is_list", "is_undef"],
        func: is_point_on_line,
    },
    // `sum` runs its `_sum` lane here (the summands are [scalar, vector] pairs — not vectors), and the
    // final `approx(pos[0], 0, eps)` is a plain num/num compare — both already entries, called natively.
    Entry {
        name: "_vnf_centroid",
        reference: "function _vnf_centroid(vnf,eps=_EPSILON) =
    assert(is_vnf(vnf) && len(vnf[0])!=0 && len(vnf[1])!=0,\"\\nInvalid or empty VNF given to centroid.\")
    let(
        verts = vnf[0],
        pos = sum([
            for(face=vnf[1], j=[1:1:len(face)-2]) let(
                v0  = verts[face[0]],
                v1  = verts[face[j]],
                v2  = verts[face[j+1]],
                vol = cross(v2,v1)*v0
            )
            [ vol, (v0+v1+v2)*vol ]
        ])
    )
    assert(!approx(pos[0],0, eps), \"\\nThe vnf has self-intersections.\")
    pos[1]/pos[0]/4;",
        consts: &[("_EPSILON", 1e-9)],
        deps: &[
            "is_vnf",
            "is_vector",
            "is_finite",
            "is_nan",
            "sum",
            "_sum",
            "is_consistent",
            "_list_pattern",
            "approx",
            "idx",
            "posmod",
        ],
        builtins: &[
            "len", "is_list", "is_undef", "is_num", "cross", "is_bool", "abs", "is_string",
        ],
        func: vnf_centroid,
    },
    // ── O.7, band 5 batch 2 (linalg/affine/geometry/lists/paths.scad) ───────────────────────────────────
    // The affine BUILDERS rot() leans on (rot itself is a dep avalanche — move/rot_inverse/_NO_ARG — and
    // stays interpreted, in the JIT bucket with _find_anchor/apply/vector_axis), the earcut driver's
    // per-candidate scan, and pill_holder's membership pair.
    Entry {
        name: "ident",
        reference: "function ident(n) = [
    for (i = [0:1:n-1]) [
        for (j = [0:1:n-1]) (i==j)? 1 : 0
    ]
];",
        consts: &[],
        deps: &[],
        builtins: &[],
        func: ident,
    },
    Entry {
        name: "affine3d_zrot",
        reference: "function affine3d_zrot(ang=0) =
    assert(is_finite(ang))
    [
        [cos(ang), -sin(ang), 0, 0],
        [sin(ang),  cos(ang), 0, 0],
        [       0,         0, 1, 0],
        [       0,         0, 0, 1]
    ];",
        consts: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine3d_zrot,
    },
    Entry {
        name: "affine3d_xrot",
        reference: "function affine3d_xrot(ang=0) =
    assert(is_finite(ang))
    [
        [1,        0,         0,   0],
        [0, cos(ang), -sin(ang),   0],
        [0, sin(ang),  cos(ang),   0],
        [0,        0,         0,   1]
    ];",
        consts: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine3d_xrot,
    },
    Entry {
        name: "affine3d_yrot",
        reference: "function affine3d_yrot(ang=0) =
    assert(is_finite(ang))
    [
        [ cos(ang), 0, sin(ang),   0],
        [        0, 1,        0,   0],
        [-sin(ang), 0, cos(ang),   0],
        [        0, 0,        0,   1]
    ];",
        consts: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine3d_yrot,
    },
    // eps is explicit here, but the wiskers lane runs `idx` → `posmod` → `approx`, whose DEFAULT eps is
    // `_EPSILON` — so the guard rides along.
    Entry {
        name: "_get_ear",
        reference: "function _get_ear(poly, ind,  eps, _i=0) =
    let( lind = len(ind) )
    lind==3 ? 0 :
    let( // the _i-th ear candidate
        p0 = poly[ind[_i]],
        p1 = poly[ind[(_i+1)%lind]],
        p2 = poly[ind[(_i+2)%lind]]
    )
    // if vertex p1 is a convex candidate to be an ear,
    // check if the triangle [p0,p1,p2] contains any other point
    // except possibly p0 and p2
    // exclude the ear candidate central vertex p1 from the verts to check
    _tri_class([p0,p1,p2],eps) > 0
    &&  _none_inside(select(ind,_i+2, _i),poly,p0,p1,p2,eps) ? _i : // found an ear
    // otherwise check the next ear candidate
    _i<lind-1 ?  _get_ear(poly, ind,  eps, _i=_i+1) :
    // poly has no ears, look for wiskers
    let( wiskers = [for(j=idx(ind)) if(norm(poly[ind[j]]-poly[ind[(j+2)%lind]])<eps) j ] )
    wiskers==[] ? undef : [wiskers[0]];",
        consts: &[("_EPSILON", 1e-9)],
        deps: &[
            "_tri_class",
            "_none_inside",
            "_is_at_left",
            "select",
            "idx",
            "posmod",
            "approx",
            "is_finite",
            "is_nan",
            "is_vector",
            "is_range",
        ],
        builtins: &[
            "len", "norm", "cross", "abs", "sign", "is_list", "is_string", "is_num", "is_undef",
            "is_bool",
        ],
        func: get_ear,
    },
    Entry {
        name: "in_list",
        reference: "function in_list(val,list,idx) =
    assert(is_list(list),\"Input is not a list\")
    assert(is_undef(idx) || is_finite(idx), \"Invalid idx value.\")
    let( firsthit = search([val], list, num_returns_per_match=1, index_col_num=idx)[0] )
    firsthit==[] ? false
    : is_undef(idx) && val==list[firsthit] ? true
    : is_def(idx) && val==list[firsthit][idx] ? true
    // first hit was found but didn't match, so try again with all hits
    : let ( allhits = search([val], list, 0, idx)[0])
      is_undef(idx) ? [for(hit=allhits) if (list[hit]==val) 1] != []
    : [for(hit=allhits) if (list[hit][idx]==val) 1] != [];",
        consts: &[],
        deps: &["is_finite", "is_nan", "is_def"],
        builtins: &["search", "is_list", "is_undef", "is_num"],
        func: in_list,
    },
    Entry {
        name: "is_path",
        reference: "function is_path(list, dim=[2,3], fast=false) =
    fast
    ?   is_list(list) && is_vector(list[0])
    :   is_matrix(list)
        && len(list)>1
        && len(list[0])>0
        && (is_undef(dim) || in_list(len(list[0]), force_list(dim)));",
        consts: &[],
        deps: &[
            "is_matrix",
            "is_vector",
            "is_finite",
            "is_nan",
            "is_consistent",
            "_list_pattern",
            "in_list",
            "is_def",
            "force_list",
        ],
        builtins: &[
            "is_list", "len", "is_undef", "is_num", "search",
        ],
        func: is_path,
    },
    // The reference's lesser/[equal]/greater concat recursion flattens to an iterative in-order walk (a
    // 20k-element pre-sorted input would recurse 20k deep); partition subsets are strictly smaller (the
    // pivot's own element lands in `equal` or — NaN/incomparable — nowhere), so the walk terminates.
    Entry {
        name: "_group_sort_by_index",
        reference: "function _group_sort_by_index(l,idx) =
    len(l) == 0 ? [] :
    len(l) == 1 ? [l] :
    let(
        pivot   = l[floor(len(l)/2)][idx],
        equal   = [ for(li=l) if( li[idx]==pivot) li ],
        lesser  = [ for(li=l) if( li[idx]< pivot) li ],
        greater = [ for(li=l) if( li[idx]> pivot) li ]
    )
    concat(
        _group_sort_by_index(lesser,idx),
        [equal],
        _group_sort_by_index(greater,idx)
    );",
        consts: &[],
        deps: &[],
        builtins: &["len", "floor", "concat"],
        func: group_sort_by_index,
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
        let next_i = super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
        if no_progress(&i, &next_i) {
            return Err(non_terminating("_none_inside"));
        }
        i = next_i;
    }
}

/// `i+1` made no progress (`i` is ±inf — undef/NaN shapes raise in `select` before reaching this): the
/// reference would recurse forever, which the interpreter only stops via its step budget. LOUD [`Err`]
/// beats a native hang; a real model never constructs this.
fn no_progress(i: &Value, next: &Value) -> bool {
    match (i, next) {
        (Value::Num(a), Value::Num(b)) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
        _ => false,
    }
}
fn non_terminating(name: &str) -> crate::Error {
    crate::Error::Eval(format!(
        "{name}: non-terminating recursion (the interpreter would only stop at its step budget)"
    ))
}

/// BOSL2 `_sum(v,_total,_i=0)` — the fold tail: `_total + v[_i]` per index, entirely through the
/// interpreter's `+`/index (so vector/matrix accumulation is elementwise exactly as interpreted). A stuck
/// `_i` (±inf) trips the [`no_progress`] guard instead of hanging.
fn sum_tail(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let mut total = args.get(1).cloned().unwrap_or(Value::Undef);
    let mut i = args.get(2).cloned().unwrap_or(Value::Num(0.0));
    loop {
        let ll = super::builtins::apply("len", std::slice::from_ref(&v));
        if super::ops::apply_binary(BinOp::Ge, i.clone(), ll.clone()).is_truthy() {
            return Ok(total);
        }
        if !matches!(ll, Value::Num(_)) {
            // len(v) is undef (non-list v): `_i >= undef` is never true, so the reference recurses forever
            // — only the interpreter's step budget would stop it. LOUD instead of a native hang.
            return Err(non_terminating("_sum"));
        }
        total = super::ops::apply_binary(BinOp::Add, total, super::ops::index(v.clone(), &i));
        let next_i = super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
        if no_progress(&i, &next_i) {
            return Err(non_terminating("_sum"));
        }
        i = next_i;
    }
}

/// BOSL2 `sum(v, dflt=0)` — the numeric/vector fast lane is the reference's own trick: `[for(i=v) 1]*v`
/// (a ones-vector dot / vector-matrix product through the interpreter's `*`); anything else consistent
/// (matrices…) folds through [`sum_tail`] with a `v[0]*0` seed.
fn sum(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    let dflt = args.get(1).cloned().unwrap_or(Value::Num(0.0));
    if super::ops::apply_binary(BinOp::Eq, v.clone(), super::build_vector(Vec::new())).is_truthy() {
        return Ok(dflt);
    }
    if !is_consistent(std::slice::from_ref(&v))?.is_truthy() {
        return Err(bosl_assert("sum: non-numeric or inconsistent input"));
    }
    let v0 = super::ops::index(v.clone(), &Value::Num(0.0));
    if v_is_finite(&v0) || is_vector_core(&v0) {
        let n = super::iter_values(&v).len();
        let ones = super::build_vector(vec![Value::Num(1.0); n]);
        return Ok(super::ops::apply_binary(BinOp::Mul, ones, v));
    }
    let seed = super::ops::apply_binary(BinOp::Mul, v0, Value::Num(0.0));
    sum_tail(&[v, seed])
}

/// The `unit` error-sentinel `[[["ASSERT"]]]`, built the way the literal would (`build_vector` all the way
/// down — a one-string level is a `List`).
fn unit_sentinel() -> Value {
    super::build_vector(vec![super::build_vector(vec![super::build_vector(vec![
        Value::string("ASSERT"),
    ])])])
}

/// BOSL2 `unit(v, error=[[["ASSERT"]]])` — `v/norm(v)`, raising on a non-vector and (by default) on a
/// near-zero one; a caller-provided `error` value is returned instead of raising. The near-zero compare and
/// division route through ops so a `List`-shaped vector (norm → undef) degrades exactly as interpreted.
fn unit(args: &[Value]) -> crate::Result<Value> {
    let v = args.first().cloned().unwrap_or(Value::Undef);
    if !is_vector_core(&v) {
        return Err(bosl_assert("unit: invalid vector"));
    }
    let norm_v = super::builtins::apply("norm", std::slice::from_ref(&v));
    if super::ops::apply_binary(BinOp::Lt, norm_v.clone(), Value::Num(1e-9)).is_truthy() {
        return match args.get(1) {
            // default error → the sentinel → the inner assert(norm(v)>=_EPSILON) fires
            None => Err(bosl_assert("unit: cannot normalize a zero vector")),
            Some(err) => {
                if super::ops::apply_binary(BinOp::Eq, err.clone(), unit_sentinel()).is_truthy() {
                    Err(bosl_assert("unit: cannot normalize a zero vector"))
                } else {
                    Ok(err.clone())
                }
            }
        };
    }
    Ok(super::ops::apply_binary(BinOp::Div, v, norm_v))
}

/// BOSL2 `is_2d_transform(t)` — the affine matrix's z-action is trivial (with the zscale carve-out). Pure
/// index chains + `==`, fully routed; every branch value is a `Bool` like the interpreter's `&&`/`||` yield.
fn is_2d_transform(args: &[Value]) -> crate::Result<Value> {
    let t = args.first().cloned().unwrap_or(Value::Undef);
    let at = |r: f64, c: f64| {
        super::ops::index(super::ops::index(t.clone(), &Value::Num(r)), &Value::Num(c))
    };
    let eq = |v: Value, k: f64| super::ops::apply_binary(BinOp::Eq, v, Value::Num(k)).is_truthy();
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
fn apply_transform(args: &[Value]) -> crate::Result<Value> {
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
        match super::builtins::apply("len", std::slice::from_ref(v)) {
            Value::Num(n) => n,
            _ => f64::NAN, // unreachable: is_matrix above
        }
    };
    let lt = num_len(&transform);
    let tdim = num_len(&super::ops::index(transform.clone(), &Value::Num(0.0))) - 1.0;
    let datadim = num_len(&super::ops::index(points.clone(), &Value::Num(0.0)));
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
        super::ops::index(
            super::ops::index(transform.clone(), &Value::Num(tdim)),
            &Value::Num(tdim),
        )
    };
    let mut rows = Vec::new();
    for i in super::value::range_iter(0.0, 1.0, tdim) {
        let mut row = Vec::new();
        for j in super::value::range_iter(0.0, 1.0, datadim - 1.0) {
            row.push(super::ops::index(
                super::ops::index(transform.clone(), &Value::Num(j)),
                &Value::Num(i),
            ));
        }
        rows.push(super::build_vector(row));
    }
    let matrix = super::ops::apply_binary(BinOp::Div, super::build_vector(rows), scale);
    if tdim == datadim {
        let aug: Vec<Value> = super::iter_values(&points)
            .iter()
            .map(|p| super::builtins::apply("concat", &[p.clone(), Value::Num(1.0)]))
            .collect();
        return Ok(super::ops::apply_binary(
            BinOp::Mul,
            super::build_vector(aug),
            matrix,
        ));
    }
    if tdim == 3.0 && datadim == 2.0 {
        if !is_2d_transform(std::slice::from_ref(&transform))?.is_truthy() {
            return Err(bosl_assert(
                "_apply: transform is 3D and acts on Z, but points are 2D",
            ));
        }
        let aug: Vec<Value> = super::iter_values(&points)
            .iter()
            .map(|p| {
                super::builtins::apply("concat", &[p.clone(), Value::num_list(vec![0.0, 1.0])])
            })
            .collect();
        return Ok(super::ops::apply_binary(
            BinOp::Mul,
            super::build_vector(aug),
            matrix,
        ));
    }
    Err(bosl_assert("_apply: unsupported combination"))
}

/// BOSL2 `_bt_search(query, r, points, tree)` — radius search over a ball tree. The reference's
/// `concat(root-hit, left, right)` tree recursion flattens to an ITERATIVE preorder DFS: the asserts force
/// every collected element to be a number, so a flat all-`Num` collection coalesces to the same `NumList`
/// the nested concats build — and an explicit stack can't blow the native stack on a crafted deep tree.
/// Assert/visit ORDER matches the interpreter (a raise in the left subtree fires before the right subtree
/// is looked at).
#[allow(
    clippy::float_cmp,
    reason = "the reference's len(tree)==1 / ==4 ARE exact f64 equalities on integer lengths"
)]
fn bt_search(args: &[Value]) -> crate::Result<Value> {
    let query = args.first().cloned().unwrap_or(Value::Undef);
    let r = args.get(1).cloned().unwrap_or(Value::Undef);
    let points = args.get(2).cloned().unwrap_or(Value::Undef);
    let mut out: Vec<Value> = Vec::new();
    let mut stack = vec![args.get(3).cloned().unwrap_or(Value::Undef)];
    while let Some(tree) = stack.pop() {
        let ll = super::builtins::apply("len", std::slice::from_ref(&tree));
        let t0 = super::ops::index(tree.clone(), &Value::Num(0.0));
        let leaf = matches!(ll, Value::Num(n) if n == 1.0) && v_is_list(&t0);
        let node = matches!(ll, Value::Num(n) if n == 4.0)
            && matches!(&t0, Value::Num(n) if !n.is_nan())
            && matches!(super::ops::index(tree.clone(), &Value::Num(1.0)), Value::Num(n) if !n.is_nan());
        if !(v_is_list(&tree) && (leaf || node)) {
            return Err(bosl_assert("_bt_search: the tree is invalid"));
        }
        if leaf {
            let empty_ok =
                super::ops::apply_binary(BinOp::Eq, t0.clone(), super::build_vector(Vec::new()))
                    .is_truthy();
            if !(empty_ok || is_vector_core(&t0)) {
                return Err(bosl_assert("_bt_search: the tree is invalid"));
            }
            for iv in super::iter_values(&t0) {
                let d = super::ops::apply_binary(
                    BinOp::Sub,
                    super::ops::index(points.clone(), &iv),
                    query.clone(),
                );
                if super::ops::apply_binary(
                    BinOp::Le,
                    super::builtins::apply("norm", std::slice::from_ref(&d)),
                    r.clone(),
                )
                .is_truthy()
                {
                    out.push(iv);
                }
            }
        } else {
            let d = super::ops::apply_binary(
                BinOp::Sub,
                query.clone(),
                super::ops::index(points.clone(), &t0),
            );
            let dist = super::builtins::apply("norm", std::slice::from_ref(&d));
            let radius = super::ops::apply_binary(
                BinOp::Add,
                r.clone(),
                super::ops::index(tree.clone(), &Value::Num(1.0)),
            );
            if super::ops::apply_binary(BinOp::Gt, dist.clone(), radius).is_truthy() {
                continue; // pruned subtree contributes `[]` — a no-op in the flat collection
            }
            if super::ops::apply_binary(BinOp::Le, dist, r.clone()).is_truthy() {
                out.push(t0);
            }
            stack.push(super::ops::index(tree.clone(), &Value::Num(3.0)));
            stack.push(super::ops::index(tree.clone(), &Value::Num(2.0)));
        }
    }
    Ok(super::build_vector(out))
}

/// The reachable slice of BOSL2 `constrain` for [`vector_angle`]'s clamp: a non-NaN number clamps through
/// the real `min`/`max` builtins; a vector clamps elementwise; everything the asserts let through that ISN'T
/// one of those (undef, NaN — `is_num(NaN)` is false) falls to the reference's `assert(false)`. The matrix
/// branch (`flatten`/`list_to_matrix`) is unreachable from `vector_angle`'s asserted shapes — LOUD error, not
/// a silent wrong answer, if that proof ever breaks.
fn constrain_clamp(v: &Value, minval: f64, maxval: f64) -> crate::Result<Value> {
    let clamp1 = |f: &Value| {
        super::builtins::apply(
            "max",
            &[
                Value::Num(minval),
                super::builtins::apply("min", &[f.clone(), Value::Num(maxval)]),
            ],
        )
    };
    match v {
        Value::Num(n) if !n.is_nan() => Ok(clamp1(v)),
        _ if is_vector_core(v) => {
            let out: Vec<Value> = super::iter_values(v).iter().map(clamp1).collect();
            Ok(super::build_vector(out))
        }
        _ if is_matrix(std::slice::from_ref(v))?.is_truthy() => Err(crate::Error::Eval(
            "constrain: matrix input unreachable from vector_angle (intrinsic guard)".to_string(),
        )),
        Value::List(_) | Value::NumList(_) => {
            let out: Vec<Value> = super::iter_values(v)
                .iter()
                .map(|vec| {
                    let row: Vec<Value> = super::iter_values(vec).iter().map(clamp1).collect();
                    super::build_vector(row)
                })
                .collect();
            Ok(super::build_vector(out))
        }
        _ => Err(bosl_assert("constrain: invalid input")),
    }
}

/// BOSL2 `vector_angle(v1,v2,v3)` — the angle between two vectors (or three points, or a pre-paired list),
/// `acos`-clamped. Assert chain in reference order with short-circuits preserved; the trig goes through the
/// REAL `acos` builtin (the exact-degree snap lives there).
#[allow(
    clippy::float_cmp,
    reason = "the reference's len(v1)==3 IS an exact f64 equality on an integer length"
)]
fn vector_angle(args: &[Value]) -> crate::Result<Value> {
    let v1 = args.first().cloned().unwrap_or(Value::Undef);
    let v2 = args.get(1).cloned().unwrap_or(Value::Undef);
    let v3 = args.get(2).cloned().unwrap_or(Value::Undef);
    let v2_undef = matches!(v2, Value::Undef);
    let v3_undef = matches!(v3, Value::Undef);
    let ok1 = (v3_undef && (v2_undef || same_shape(&[v1.clone(), v2.clone()])?.is_truthy()))
        || is_consistent(&[super::build_vector(vec![
            v1.clone(),
            v2.clone(),
            v3.clone(),
        ])])?
        .is_truthy();
    if !ok1 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let ok2 = is_vector(std::slice::from_ref(&v1))?.is_truthy()
        || is_consistent(std::slice::from_ref(&v1))?.is_truthy();
    if !ok2 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let vecs = if !v3_undef {
        super::build_vector(vec![
            super::ops::apply_binary(BinOp::Sub, v1, v2.clone()),
            super::ops::apply_binary(BinOp::Sub, v3, v2),
        ])
    } else if !v2_undef {
        super::build_vector(vec![v1, v2])
    } else if matches!(
        super::builtins::apply("len", std::slice::from_ref(&v1)),
        Value::Num(n) if n == 3.0
    ) {
        let p = |i: f64| super::ops::index(v1.clone(), &Value::Num(i));
        super::build_vector(vec![
            super::ops::apply_binary(BinOp::Sub, p(0.0), p(1.0)),
            super::ops::apply_binary(BinOp::Sub, p(2.0), p(1.0)),
        ])
    } else {
        v1
    };
    let vecs0 = super::ops::index(vecs.clone(), &Value::Num(0.0));
    let vecs1 = super::ops::index(vecs, &Value::Num(1.0));
    let ok3 = is_vector(&[vecs0.clone(), Value::Num(2.0)])?.is_truthy()
        || is_vector(&[vecs0.clone(), Value::Num(3.0)])?.is_truthy();
    if !ok3 {
        return Err(bosl_assert("vector_angle: bad arguments"));
    }
    let norm0 = super::builtins::apply("norm", std::slice::from_ref(&vecs0));
    let norm1 = super::builtins::apply("norm", std::slice::from_ref(&vecs1));
    let pos =
        |n: &Value| super::ops::apply_binary(BinOp::Gt, n.clone(), Value::Num(0.0)).is_truthy();
    if !(pos(&norm0) && pos(&norm1)) {
        return Err(bosl_assert("vector_angle: zero length vector"));
    }
    let dot = super::ops::apply_binary(BinOp::Mul, vecs0, vecs1);
    let ratio = super::ops::apply_binary(
        BinOp::Div,
        dot,
        super::ops::apply_binary(BinOp::Mul, norm0, norm1),
    );
    let clamped = constrain_clamp(&ratio, -1.0, 1.0)?;
    Ok(super::builtins::apply(
        "acos",
        std::slice::from_ref(&clamped),
    ))
}

/// BOSL2 `_point_dist(path, pathseg_unit, pathseg_len, pt)` — min distance from `pt` to a precomputed
/// segment chain; `offset()`'s inner scan (4.9s/1770 calls in `shoe_holder` — ~10 elements per call ×
/// interpreted let-chains). Fully routed: dots through `apply_binary` (the 4-lane `ops::dot`), the final
/// reduction through the real `min` builtin, the wraparound neighbor through the native [`select`] (its
/// assert raises exactly like the reference on a degenerate `i+1`).
fn point_dist(args: &[Value]) -> crate::Result<Value> {
    let path = args.first().cloned().unwrap_or(Value::Undef);
    let unit = args.get(1).cloned().unwrap_or(Value::Undef);
    let seg_len = args.get(2).cloned().unwrap_or(Value::Undef);
    let pt = args.get(3).cloned().unwrap_or(Value::Undef);
    let ll = super::builtins::apply("len", std::slice::from_ref(&unit));
    let end = super::ops::apply_binary(BinOp::Sub, ll, Value::Num(1.0));
    let range = super::build_range(&Value::Num(0.0), &Value::Num(1.0), &end);
    let mut dists: Vec<Value> = Vec::new();
    for iv in super::iter_values(&range) {
        let pi = super::ops::index(path.clone(), &iv);
        let v = super::ops::apply_binary(BinOp::Sub, pt.clone(), pi.clone());
        let ui = super::ops::index(unit.clone(), &iv);
        let projection = super::ops::apply_binary(BinOp::Mul, v.clone(), ui.clone());
        let li = super::ops::index(seg_len.clone(), &iv);
        let d = if super::ops::apply_binary(BinOp::Lt, projection.clone(), Value::Num(0.0))
            .is_truthy()
        {
            super::ops::apply_binary(BinOp::Sub, pt.clone(), pi)
        } else if super::ops::apply_binary(BinOp::Gt, projection.clone(), li).is_truthy() {
            let next = select(&[
                path.clone(),
                super::ops::apply_binary(BinOp::Add, iv.clone(), Value::Num(1.0)),
            ])?;
            super::ops::apply_binary(BinOp::Sub, pt.clone(), next)
        } else {
            super::ops::apply_binary(
                BinOp::Sub,
                v,
                super::ops::apply_binary(BinOp::Mul, projection, ui),
            )
        };
        dists.push(super::builtins::apply("norm", std::slice::from_ref(&d)));
    }
    let list = super::build_vector(dists);
    Ok(super::builtins::apply("min", std::slice::from_ref(&list)))
}

/// BOSL2 `_is_point_on_line(point, line, bounded=false, eps=_EPSILON)` — collinearity within tolerance,
/// optionally clamped to the segment on either end (`bounded` goes through the real [`force_list`]). The
/// 2D/3D split (`abs(cross)` vs `norm(cross)`) and the `t` parameter all route through ops.
fn is_point_on_line(args: &[Value]) -> crate::Result<Value> {
    let point = args.first().cloned().unwrap_or(Value::Undef);
    let line = args.get(1).cloned().unwrap_or(Value::Undef);
    let bounded = args.get(2).cloned().unwrap_or(Value::Bool(false));
    let eps = args.get(3).cloned().unwrap_or(Value::Num(1e-9));
    let l0 = super::ops::index(line.clone(), &Value::Num(0.0));
    let l1 = super::ops::index(line, &Value::Num(1.0));
    let v1 = super::ops::apply_binary(BinOp::Sub, l1, l0.clone());
    let v0 = super::ops::apply_binary(BinOp::Sub, point, l0);
    let t = super::ops::apply_binary(
        BinOp::Div,
        super::ops::apply_binary(BinOp::Mul, v0.clone(), v1.clone()),
        super::ops::apply_binary(BinOp::Mul, v1.clone(), v1.clone()),
    );
    let bounded2 = force_list(&[bounded, Value::Num(2.0)])?;
    let crx = super::builtins::apply("cross", &[v0, v1.clone()]);
    let ncp = if super::ops::apply_binary(
        BinOp::Eq,
        super::builtins::apply("len", std::slice::from_ref(&v1)),
        Value::Num(2.0),
    )
    .is_truthy()
    {
        super::builtins::apply("abs", std::slice::from_ref(&crx))
    } else {
        super::builtins::apply("norm", std::slice::from_ref(&crx))
    };
    let on_line = super::ops::apply_binary(
        BinOp::Le,
        ncp,
        super::ops::apply_binary(
            BinOp::Mul,
            eps.clone(),
            super::builtins::apply("norm", std::slice::from_ref(&v1)),
        ),
    );
    if !on_line.is_truthy() {
        return Ok(Value::Bool(false));
    }
    if super::ops::index(bounded2.clone(), &Value::Num(0.0)).is_truthy()
        && !super::ops::apply_binary(
            BinOp::Ge,
            t.clone(),
            super::ops::apply_unary(crate::parser::UnOp::Neg, eps.clone()),
        )
        .is_truthy()
    {
        return Ok(Value::Bool(false));
    }
    if super::ops::index(bounded2, &Value::Num(1.0)).is_truthy()
        && !super::ops::apply_binary(
            BinOp::Lt,
            t,
            super::ops::apply_binary(BinOp::Add, Value::Num(1.0), eps),
        )
        .is_truthy()
    {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(true))
}

/// The [`PINS`]' `is_vnf(x)` as [`vnf_centroid`]'s assert needs it, composed from the band's own natives
/// (`is_vector(x[0][0], 3)` / `is_vector(x[1][0])`).
fn is_vnf_check(x: &Value) -> crate::Result<bool> {
    if !v_is_list(x) {
        return Ok(false);
    }
    let ll = super::builtins::apply("len", std::slice::from_ref(x));
    if !super::ops::apply_binary(BinOp::Eq, ll, Value::Num(2.0)).is_truthy() {
        return Ok(false);
    }
    let x0 = super::ops::index(x.clone(), &Value::Num(0.0));
    let x1 = super::ops::index(x.clone(), &Value::Num(1.0));
    if !(v_is_list(&x0) && v_is_list(&x1)) {
        return Ok(false);
    }
    let empty = super::build_vector(Vec::new());
    let verts_ok = super::ops::apply_binary(BinOp::Eq, x0.clone(), empty.clone()).is_truthy()
        || (super::ops::apply_binary(
            BinOp::Ge,
            super::builtins::apply("len", std::slice::from_ref(&x0)),
            Value::Num(3.0),
        )
        .is_truthy()
            && is_vector(&[
                super::ops::index(x0.clone(), &Value::Num(0.0)),
                Value::Num(3.0),
            ])?
            .is_truthy());
    if !verts_ok {
        return Ok(false);
    }
    Ok(
        super::ops::apply_binary(BinOp::Eq, x1.clone(), empty).is_truthy()
            || is_vector(std::slice::from_ref(&super::ops::index(
                x1,
                &Value::Num(0.0),
            )))?
            .is_truthy(),
    )
}

/// BOSL2 `_vnf_centroid(vnf, eps=_EPSILON)` — the volume-weighted centroid: per face-fan triangle,
/// `vol = cross(v2,v1)*v0` and the running `[vol, (v0+v1+v2)*vol]` pairs sum through the REAL [`sum`]
/// entry (its `_sum` lane — the summands are [scalar, vector] pairs), then `approx(pos[0], 0, eps)` guards
/// self-intersection. 1.9s/30 calls in `webcam_holder` — the fan loop over every face, interpreted.
fn vnf_centroid(args: &[Value]) -> crate::Result<Value> {
    let vnf = args.first().cloned().unwrap_or(Value::Undef);
    let eps = args.get(1).cloned().unwrap_or(Value::Num(1e-9));
    let verts = super::ops::index(vnf.clone(), &Value::Num(0.0));
    let faces = super::ops::index(vnf.clone(), &Value::Num(1.0));
    let nonzero = |v: &Value| {
        !super::ops::apply_binary(
            BinOp::Eq,
            super::builtins::apply("len", std::slice::from_ref(v)),
            Value::Num(0.0),
        )
        .is_truthy()
    };
    if !(is_vnf_check(&vnf)? && nonzero(&verts) && nonzero(&faces)) {
        return Err(bosl_assert("_vnf_centroid: invalid or empty VNF"));
    }
    let mut pairs: Vec<Value> = Vec::new();
    for face in super::iter_values(&faces) {
        let jr = super::build_range(
            &Value::Num(1.0),
            &Value::Num(1.0),
            &super::ops::apply_binary(
                BinOp::Sub,
                super::builtins::apply("len", std::slice::from_ref(&face)),
                Value::Num(2.0),
            ),
        );
        for j in super::iter_values(&jr) {
            let vat = |idx: &Value| {
                super::ops::index(verts.clone(), &super::ops::index(face.clone(), idx))
            };
            let v0 = vat(&Value::Num(0.0));
            let v1 = vat(&j);
            let v2 = vat(&super::ops::apply_binary(
                BinOp::Add,
                j.clone(),
                Value::Num(1.0),
            ));
            let vol = super::ops::apply_binary(
                BinOp::Mul,
                super::builtins::apply("cross", &[v2.clone(), v1.clone()]),
                v0.clone(),
            );
            let centroid_part = super::ops::apply_binary(
                BinOp::Mul,
                super::ops::apply_binary(
                    BinOp::Add,
                    super::ops::apply_binary(BinOp::Add, v0, v1),
                    v2,
                ),
                vol.clone(),
            );
            pairs.push(super::build_vector(vec![vol, centroid_part]));
        }
    }
    let pos = sum(&[super::build_vector(pairs)])?;
    let p0 = super::ops::index(pos.clone(), &Value::Num(0.0));
    if approx(&[p0.clone(), Value::Num(0.0), eps])?.is_truthy() {
        return Err(bosl_assert("_vnf_centroid: the vnf has self-intersections"));
    }
    Ok(super::ops::apply_binary(
        BinOp::Div,
        super::ops::apply_binary(BinOp::Div, super::ops::index(pos, &Value::Num(1.0)), p0),
        Value::Num(4.0),
    ))
}

/// BOSL2 `_group_sort_by_index(l, idx)` — quicksort-flavored grouping by `l[i][idx]`. The reference's
/// `concat(recurse(lesser), [equal], recurse(greater))` flattens to an iterative IN-ORDER walk (a
/// pre-sorted 20k-element input would recurse ~20k deep otherwise); partitions are strictly smaller — the
/// pivot's own element lands in `equal`, or (NaN/incomparable index) in none — so the walk terminates.
/// All comparisons route through ops (mixed-type `<`/`>` yield undef → dropped, like the comprehensions).
fn group_sort_by_index(args: &[Value]) -> crate::Result<Value> {
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
        let ll = super::builtins::apply("len", std::slice::from_ref(&l));
        if super::ops::apply_binary(BinOp::Eq, ll.clone(), Value::Num(0.0)).is_truthy() {
            continue; // `[]` contributes nothing to the flat walk
        }
        if super::ops::apply_binary(BinOp::Eq, ll.clone(), Value::Num(1.0)).is_truthy() {
            out.push(l);
            continue;
        }
        let mid = super::builtins::apply(
            "floor",
            &[super::ops::apply_binary(BinOp::Div, ll, Value::Num(2.0))],
        );
        let pivot = super::ops::index(super::ops::index(l.clone(), &mid), &idx);
        let mut equal: Vec<Value> = Vec::new();
        let mut lesser: Vec<Value> = Vec::new();
        let mut greater: Vec<Value> = Vec::new();
        for li in super::iter_values(&l) {
            let key = super::ops::index(li.clone(), &idx);
            if super::ops::apply_binary(BinOp::Eq, key.clone(), pivot.clone()).is_truthy() {
                equal.push(li);
            } else if super::ops::apply_binary(BinOp::Lt, key.clone(), pivot.clone()).is_truthy() {
                lesser.push(li);
            } else if super::ops::apply_binary(BinOp::Gt, key, pivot.clone()).is_truthy() {
                greater.push(li);
            }
        }
        stack.push(Work::Split(super::build_vector(greater)));
        stack.push(Work::Emit(super::build_vector(equal)));
        stack.push(Work::Split(super::build_vector(lesser))); // LIFO → lesser first: in-order
    }
    Ok(super::build_vector(out))
}

/// BOSL2 `ident(n)` — the n×n identity matrix, rows built like the comprehension would (`build_vector`
/// coalesces each all-num row to a `NumList`); a garbage `n` degenerates through `build_range` exactly as
/// interpreted.
fn ident(args: &[Value]) -> crate::Result<Value> {
    let n = args.first().cloned().unwrap_or(Value::Undef);
    let end = super::ops::apply_binary(BinOp::Sub, n, Value::Num(1.0));
    let range = super::build_range(&Value::Num(0.0), &Value::Num(1.0), &end);
    let is_idx = super::iter_values(&range);
    let mut rows: Vec<Value> = Vec::new();
    for i in &is_idx {
        let row: Vec<Value> = is_idx
            .iter()
            .map(|j| {
                if super::ops::apply_binary(BinOp::Eq, i.clone(), j.clone()).is_truthy() {
                    Value::Num(1.0)
                } else {
                    Value::Num(0.0)
                }
            })
            .collect();
        rows.push(super::build_vector(row));
    }
    Ok(super::build_vector(rows))
}

/// One axis-rotation affine builder — the shared shape of `affine3d_zrot`/`xrot`/`yrot`: assert the angle
/// finite, take `sin`/`cos` through the REAL builtins (the exact-degree snap lives there), lay out the rows.
/// `layout` receives `(c, s, -s)` and returns the 16 cells in row order.
fn axis_rot(
    args: &[Value],
    layout: fn(Value, Value, Value) -> [[Value; 4]; 4],
) -> crate::Result<Value> {
    let ang = args.first().cloned().unwrap_or(Value::Num(0.0));
    if !v_is_finite(&ang) {
        return Err(bosl_assert("affine3d rotation: angle must be finite"));
    }
    let c = super::builtins::apply("cos", std::slice::from_ref(&ang));
    let s = super::builtins::apply("sin", std::slice::from_ref(&ang));
    let ns = super::ops::apply_unary(crate::parser::UnOp::Neg, s.clone());
    let rows: Vec<Value> = layout(c, s, ns)
        .into_iter()
        .map(|row| super::build_vector(row.into_iter().collect()))
        .collect();
    Ok(super::build_vector(rows))
}
fn affine3d_zrot(args: &[Value]) -> crate::Result<Value> {
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
fn affine3d_xrot(args: &[Value]) -> crate::Result<Value> {
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
fn affine3d_yrot(args: &[Value]) -> crate::Result<Value> {
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

/// BOSL2 `_get_ear(poly, ind, eps, _i=0)` — the ear-cut driver's per-candidate scan: the first `_i` whose
/// fan triangle is convex and empty ([`tri_class_val`] + the native [`none_inside`], with [`select`]'s
/// slice for the exclusion window), else the whisker fallback. Tail recursion → loop with the
/// [`no_progress`] guard; the whisker lane's `idx(ind)` runs the real native (its assert raises on a
/// non-list `ind` exactly like the reference).
#[allow(
    clippy::similar_names,
    reason = "`ind`/`lind` ARE the reference's own parameter and let names"
)]
fn get_ear(args: &[Value]) -> crate::Result<Value> {
    let poly = args.first().cloned().unwrap_or(Value::Undef);
    let ind = args.get(1).cloned().unwrap_or(Value::Undef);
    let eps = args.get(2).cloned().unwrap_or(Value::Undef); // eps has NO default in the reference
    let mut i = args.get(3).cloned().unwrap_or(Value::Num(0.0));
    let at = |k: &Value| super::ops::index(poly.clone(), &super::ops::index(ind.clone(), k));
    loop {
        let lind = super::builtins::apply("len", std::slice::from_ref(&ind));
        if super::ops::apply_binary(BinOp::Eq, lind.clone(), Value::Num(3.0)).is_truthy() {
            return Ok(Value::Num(0.0));
        }
        let wrap = |off: f64| {
            super::ops::apply_binary(
                BinOp::Mod,
                super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(off)),
                lind.clone(),
            )
        };
        let p0 = at(&i);
        let p1 = at(&wrap(1.0));
        let p2 = at(&wrap(2.0));
        let tri = super::build_vector(vec![p0.clone(), p1.clone(), p2.clone()]);
        if super::ops::apply_binary(BinOp::Gt, tri_class_val(&tri, &eps), Value::Num(0.0))
            .is_truthy()
        {
            let window = select(&[
                ind.clone(),
                super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(2.0)),
                i.clone(),
            ])?;
            if none_inside(&[window, poly.clone(), p0, p1, p2, eps.clone()])?.is_truthy() {
                return Ok(i);
            }
        }
        if super::ops::apply_binary(
            BinOp::Lt,
            i.clone(),
            super::ops::apply_binary(BinOp::Sub, lind.clone(), Value::Num(1.0)),
        )
        .is_truthy()
        {
            let next = super::ops::apply_binary(BinOp::Add, i.clone(), Value::Num(1.0));
            if no_progress(&i, &next) {
                return Err(non_terminating("_get_ear"));
            }
            i = next;
            continue;
        }
        // whiskers: adjacent-but-one vertices closer than eps
        let jrange = idx(std::slice::from_ref(&ind))?;
        let mut ws: Vec<Value> = Vec::new();
        for j in super::iter_values(&jrange) {
            let far = super::ops::apply_binary(
                BinOp::Mod,
                super::ops::apply_binary(BinOp::Add, j.clone(), Value::Num(2.0)),
                lind.clone(),
            );
            let d = super::ops::apply_binary(BinOp::Sub, at(&j), at(&far));
            if super::ops::apply_binary(
                BinOp::Lt,
                super::builtins::apply("norm", std::slice::from_ref(&d)),
                eps.clone(),
            )
            .is_truthy()
            {
                ws.push(j);
            }
        }
        let wsv = super::build_vector(ws);
        return Ok(
            if super::ops::apply_binary(BinOp::Eq, wsv.clone(), super::build_vector(Vec::new()))
                .is_truthy()
            {
                Value::Undef
            } else {
                super::build_vector(vec![super::ops::index(wsv, &Value::Num(0.0))])
            },
        );
    }
}

/// BOSL2 `in_list(val, list, idx)` — membership via the REAL `search` builtin (its named args are
/// positional slots 2/3 — OpenSCAD builtins read by position), with the reference's first-hit shortcut and
/// the all-hits retry. The retry's `[for(hit=…) if(…) 1] != []` is an any-match — collecting past the first
/// match is unobservable, so the native breaks early.
fn in_list(args: &[Value]) -> crate::Result<Value> {
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
    let val_list = super::build_vector(vec![val.clone()]);
    let firsthit = super::ops::index(
        super::builtins::apply(
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
    let empty = super::build_vector(Vec::new());
    if super::ops::apply_binary(BinOp::Eq, firsthit.clone(), empty).is_truthy() {
        return Ok(Value::Bool(false));
    }
    let hit_item = |hit: &Value| {
        let item = super::ops::index(list.clone(), hit);
        if idx_undef {
            item
        } else {
            super::ops::index(item, &idxv)
        }
    };
    if super::ops::apply_binary(BinOp::Eq, val.clone(), hit_item(&firsthit)).is_truthy() {
        return Ok(Value::Bool(true));
    }
    let allhits = super::ops::index(
        super::builtins::apply(
            "search",
            &[val_list, list.clone(), Value::Num(0.0), idxv.clone()],
        ),
        &Value::Num(0.0),
    );
    for hit in super::iter_values(&allhits) {
        if super::ops::apply_binary(BinOp::Eq, hit_item(&hit), val.clone()).is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

/// BOSL2 `is_path(list, dim=[2,3], fast=false)` — a matrix of ≥2 points whose width is in `dim`;
/// composes the band's own [`is_matrix`]/[`in_list`]/[`force_list`] natives.
fn is_path(args: &[Value]) -> crate::Result<Value> {
    let list = args.first().cloned().unwrap_or(Value::Undef);
    let dim = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Value::num_list(vec![2.0, 3.0]));
    let fast = args.get(2).cloned().unwrap_or(Value::Bool(false));
    if fast.is_truthy() {
        return Ok(Value::Bool(
            v_is_list(&list)
                && is_vector(std::slice::from_ref(&super::ops::index(
                    list.clone(),
                    &Value::Num(0.0),
                )))?
                .is_truthy(),
        ));
    }
    if !is_matrix(std::slice::from_ref(&list))?.is_truthy() {
        return Ok(Value::Bool(false));
    }
    let ll = super::builtins::apply("len", std::slice::from_ref(&list));
    if !super::ops::apply_binary(BinOp::Gt, ll, Value::Num(1.0)).is_truthy() {
        return Ok(Value::Bool(false));
    }
    let row0 = super::ops::index(list, &Value::Num(0.0));
    let l0 = super::builtins::apply("len", std::slice::from_ref(&row0));
    if !super::ops::apply_binary(BinOp::Gt, l0.clone(), Value::Num(0.0)).is_truthy() {
        return Ok(Value::Bool(false));
    }
    if matches!(dim, Value::Undef) {
        return Ok(Value::Bool(true));
    }
    let forced = force_list(std::slice::from_ref(&dim))?;
    in_list(&[l0, forced])
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
    fn fast_equals_slow_aggregate_band() {
        let consts = [("_EPSILON", Value::Num(1e-9))];
        let shape_deps = [
            reference_of("is_consistent").unwrap(),
            reference_of("_list_pattern").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("is_vector").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ];

        // _sum / sum — scalars, vectors, matrices (the _sum lane), inconsistent (raise), empty (dflt).
        let sum_ref = reference_of("sum").unwrap();
        let sum_deps: Vec<&str> = shape_deps
            .iter()
            .copied()
            .chain([reference_of("_sum").unwrap()])
            .collect();
        let st_ref = reference_of("_sum").unwrap();
        let m22 = Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![3.0, 4.0]),
        ]);
        let sums = [
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::num_list(vec![0.5]),
            Value::list(vec![
                Value::num_list(vec![1.0, 2.0]),
                Value::num_list(vec![10.0, 20.0]),
            ]),
            Value::list(vec![m22.clone(), m22.clone()]),
            Value::list(vec![]),
            Value::list(vec![Value::Num(1.0), Value::string("x")]),
            Value::num_list(vec![f64::NAN, 1.0]),
            Value::Num(7.0),
            Value::Undef,
        ];
        for v in &sums {
            for dflt in [None, Some(Value::Num(9.0)), Some(Value::string("d"))] {
                let mut args = vec![v.clone()];
                if let Some(d) = &dflt {
                    args.push(d.clone());
                }
                assert!(
                    same_result(
                        &super::sum(&args),
                        &interpret_with_deps_consts(sum_ref, &sum_deps, &consts, &args)
                    ),
                    "sum diverged on ({v:?}, dflt {dflt:?})"
                );
            }
            // a non-list v makes the reference recurse forever (len(v) is undef) — the oracle would HANG,
            // so those inputs are asserted native-side only below.
            if matches!(v, Value::List(_) | Value::NumList(_)) {
                let args = [v.clone(), Value::Num(0.0)];
                assert!(
                    same_result(
                        &super::sum_tail(&args),
                        &interpret_with_deps_consts(st_ref, &[], &consts, &args)
                    ),
                    "_sum diverged on {v:?}"
                );
            }
        }
        // the non-terminating shapes: LOUD Err, never a hang (the interpreter only stops at its budget).
        assert!(super::sum_tail(&[Value::Num(7.0), Value::Num(0.0)]).is_err());
        assert!(super::sum_tail(&[Value::Undef, Value::Num(0.0)]).is_err());
        assert!(
            super::sum_tail(&[
                Value::num_list(vec![1.0]),
                Value::Num(0.0),
                Value::Num(f64::NEG_INFINITY)
            ])
            .is_err()
        );

        // unit — ordinary, near-zero (default raise vs custom error value), non-vector raise, List-shaped.
        let unit_ref = reference_of("unit").unwrap();
        let unit_deps = [
            reference_of("is_vector").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ];
        let units = [
            Value::num_list(vec![3.0, 4.0]),
            Value::num_list(vec![0.0, 0.0]),
            Value::num_list(vec![1e-10, 0.0]),
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::Num(5.0),
            Value::Undef,
            Value::list(vec![Value::Num(1.0), Value::string("x")]),
        ];
        for v in &units {
            for err in [None, Some(Value::Num(-7.0)), Some(Value::Undef)] {
                let mut args = vec![v.clone()];
                if let Some(e) = &err {
                    args.push(e.clone());
                }
                assert!(
                    same_result(
                        &super::unit(&args),
                        &interpret_with_deps_consts(unit_ref, &unit_deps, &consts, &args)
                    ),
                    "unit diverged on ({v:?}, error {err:?})"
                );
            }
        }

        // is_2d_transform / _apply — real affine matrices (2D-in-3D, translation, scale, zscale), the
        // 2D-points-under-3D-transform lane, and the raise paths.
        let i2t_ref = reference_of("is_2d_transform").unwrap();
        let ap_ref = reference_of("_apply").unwrap();
        let ap_deps: Vec<&str> = shape_deps
            .iter()
            .copied()
            .chain([reference_of("is_matrix").unwrap(), i2t_ref])
            .collect();
        let mat4 = |rows: [[f64; 4]; 4]| {
            let rows: Vec<Value> = rows.iter().map(|r| Value::num_list(r.to_vec())).collect();
            Value::list(rows)
        };
        let ident = mat4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let translate = mat4([
            [1.0, 0.0, 0.0, 5.0],
            [0.0, 1.0, 0.0, -3.0],
            [0.0, 0.0, 1.0, 2.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let zscale = mat4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 4.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let scale2 = mat4([
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 4.0, 0.0],
            [0.0, 0.0, 0.0, 2.0],
        ]);
        let rot2d = mat4([
            [0.0, -1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let mats = [
            ident.clone(),
            translate.clone(),
            zscale.clone(),
            scale2.clone(),
            rot2d.clone(),
            m22.clone(),
            Value::Undef,
        ];
        for t in &mats {
            let args = [t.clone()];
            assert!(
                same_result(
                    &super::is_2d_transform(&args),
                    &interpret_with_deps_consts(i2t_ref, &[], &consts, &args)
                ),
                "is_2d_transform diverged on {t:?}"
            );
        }
        let pts3 = Value::list(vec![
            Value::num_list(vec![1.0, 2.0, 3.0]),
            Value::num_list(vec![-1.0, 0.5, 0.0]),
        ]);
        let pts2 = Value::list(vec![
            Value::num_list(vec![1.0, 2.0]),
            Value::num_list(vec![-1.0, 0.5]),
        ]);
        for t in &mats {
            for p in [&pts3, &pts2, &m22, &Value::Undef] {
                let args = [t.clone(), p.clone()];
                assert!(
                    same_result(
                        &super::apply_transform(&args),
                        &interpret_with_deps_consts(ap_ref, &ap_deps, &consts, &args)
                    ),
                    "_apply diverged on ({t:?}, {p:?})"
                );
            }
        }

        // _bt_search — a real 2-level tree over five 2D points, radii that hit the prune / root-hit / leaf
        // lanes, plus the malformed-tree raises.
        let bt_ref = reference_of("_bt_search").unwrap();
        let bt_deps = [
            reference_of("is_vector").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ];
        let points = Value::list(vec![
            p2(0.0, 0.0),
            p2(1.0, 0.0),
            p2(0.0, 1.0),
            p2(5.0, 5.0),
            p2(5.2, 5.0),
        ]);
        // node: [pivot_idx, radius, left, right]; leaves carry index lists
        let leaf = |ids: &[f64]| Value::list(vec![Value::num_list(ids.to_vec())]);
        let tree = Value::list(vec![
            Value::Num(0.0),
            Value::Num(1.5),
            leaf(&[1.0, 2.0]),
            Value::list(vec![
                Value::Num(3.0),
                Value::Num(0.5),
                leaf(&[4.0]),
                leaf(&[]),
            ]),
        ]);
        let bt_cases: Vec<Vec<Value>> = vec![
            vec![p2(0.0, 0.0), Value::Num(1.1), points.clone(), tree.clone()],
            vec![p2(0.0, 0.0), Value::Num(0.1), points.clone(), tree.clone()],
            vec![p2(5.0, 5.0), Value::Num(0.5), points.clone(), tree.clone()],
            vec![p2(9.0, 9.0), Value::Num(0.1), points.clone(), tree.clone()],
            vec![
                p2(0.0, 0.0),
                Value::Num(1.1),
                points.clone(),
                leaf(&[0.0, 3.0]),
            ],
            vec![p2(0.0, 0.0), Value::Num(1.1), points.clone(), leaf(&[])],
            vec![
                p2(0.0, 0.0),
                Value::Num(1.1),
                points.clone(),
                Value::Num(7.0),
            ],
            vec![
                p2(0.0, 0.0),
                Value::Num(1.1),
                points.clone(),
                Value::list(vec![
                    Value::Num(0.0),
                    Value::Num(1.0),
                    leaf(&[]),
                    Value::Num(9.0),
                ]),
            ],
            vec![p2(0.0, 0.0), Value::Undef, points.clone(), tree.clone()],
        ];
        for args in &bt_cases {
            assert!(
                same_result(
                    &super::bt_search(args),
                    &interpret_with_deps_consts(bt_ref, &bt_deps, &consts, args)
                ),
                "_bt_search diverged on {args:?}"
            );
        }

        // vector_angle — two-vector, three-point, paired-list, and the assert lanes (mismatched shapes,
        // zero-length, scalar input); the acos-domain clamp edge via antiparallel vectors.
        let va_ref = reference_of("vector_angle").unwrap();
        let va_deps: Vec<&str> = shape_deps
            .iter()
            .copied()
            .chain([
                reference_of("same_shape").unwrap(),
                reference_of("is_def").unwrap(),
                reference_of("is_matrix").unwrap(),
                pin_reference_of("constrain").unwrap(),
            ])
            .collect();
        let va_cases: Vec<Vec<Value>> = vec![
            vec![p2(1.0, 0.0), p2(0.0, 1.0)],
            vec![p2(1.0, 0.0), p2(-1.0, 0.0)],
            vec![p2(1.0, 0.0), p2(1.0, 0.0)],
            vec![
                Value::num_list(vec![1.0, 0.0, 0.0]),
                Value::num_list(vec![0.0, 0.0, 1.0]),
            ],
            vec![p2(1.0, 0.0), p2(0.0, 1.0), p2(1.0, 1.0)],
            vec![Value::list(vec![p2(1.0, 0.0), p2(0.0, 1.0)])],
            vec![Value::list(vec![p2(0.0, 2.0), p2(0.0, 0.0), p2(2.0, 0.0)])],
            vec![p2(1.0, 0.0), Value::num_list(vec![1.0, 0.0, 0.0])],
            vec![p2(0.0, 0.0), p2(1.0, 0.0)],
            vec![Value::Num(3.0)],
            vec![Value::Undef],
        ];
        for args in &va_cases {
            assert!(
                same_result(
                    &super::vector_angle(args),
                    &interpret_with_deps_consts(va_ref, &va_deps, &consts, args)
                ),
                "vector_angle diverged on {args:?}"
            );
        }
    }

    #[test]
    fn fast_equals_slow_band5_batch1() {
        let consts = [("_EPSILON", Value::Num(1e-9))];
        let select_knot = [
            reference_of("select").unwrap(),
            reference_of("is_vector").unwrap(),
            pin_reference_of("is_range").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ];

        // _point_dist — a real segment chain (precomputed unit/len like offset() passes), the three
        // segdist lanes (behind / beyond / perpendicular), plus degenerate shapes.
        let pd_ref = reference_of("_point_dist").unwrap();
        let path = Value::list(vec![p2(0.0, 0.0), p2(2.0, 0.0), p2(2.0, 2.0)]);
        let units = Value::list(vec![p2(1.0, 0.0), p2(0.0, 1.0)]);
        let lens = Value::num_list(vec![2.0, 2.0]);
        let pd_cases: Vec<Vec<Value>> = vec![
            vec![path.clone(), units.clone(), lens.clone(), p2(1.0, 1.0)],
            vec![path.clone(), units.clone(), lens.clone(), p2(-1.0, -1.0)],
            vec![path.clone(), units.clone(), lens.clone(), p2(5.0, 5.0)],
            vec![path.clone(), units.clone(), lens.clone(), p2(2.0, 1.0)],
            vec![
                path.clone(),
                Value::list(vec![]),
                Value::num_list(vec![]),
                p2(0.0, 0.0),
            ],
            vec![Value::Undef, units.clone(), lens.clone(), p2(0.0, 0.0)],
            vec![path.clone(), units.clone(), lens.clone(), Value::Undef],
        ];
        for args in &pd_cases {
            assert!(
                same_result(
                    &super::point_dist(args),
                    &interpret_with_deps_consts(pd_ref, &select_knot, &consts, args)
                ),
                "_point_dist diverged on {args:?}"
            );
        }

        // _is_point_on_line — on/off the line in 2D and 3D, each bounded mode, exotic shapes.
        let ipol_ref = reference_of("_is_point_on_line").unwrap();
        let ipol_deps = [reference_of("force_list").unwrap()];
        let line2 = Value::list(vec![p2(0.0, 0.0), p2(2.0, 0.0)]);
        let line3 = Value::list(vec![
            Value::num_list(vec![0.0, 0.0, 0.0]),
            Value::num_list(vec![0.0, 0.0, 2.0]),
        ]);
        let bounds = [
            None,
            Some(Value::Bool(true)),
            Some(Value::list(vec![Value::Bool(true), Value::Bool(false)])),
        ];
        let ipol_pts = [
            (p2(1.0, 0.0), line2.clone()),
            (p2(-1.0, 0.0), line2.clone()),
            (p2(3.0, 0.0), line2.clone()),
            (p2(1.0, 0.5), line2.clone()),
            (p2(1.0, 1e-12), line2.clone()),
            (Value::num_list(vec![0.0, 0.0, 1.0]), line3.clone()),
            (Value::num_list(vec![1.0, 0.0, 1.0]), line3.clone()),
            (Value::Undef, line2.clone()),
            (p2(1.0, 0.0), Value::Undef),
        ];
        for (pt, line) in &ipol_pts {
            for b in &bounds {
                let mut args = vec![pt.clone(), line.clone()];
                if let Some(b) = b {
                    args.push(b.clone());
                }
                assert!(
                    same_result(
                        &super::is_point_on_line(&args),
                        &interpret_with_deps_consts(ipol_ref, &ipol_deps, &consts, &args)
                    ),
                    "_is_point_on_line diverged on ({pt:?}, {line:?}, {b:?})"
                );
            }
        }

        // _vnf_centroid — a unit cube VNF (quad faces exercise the fan j-loop), a tet, empty/invalid
        // raises, and a degenerate (zero-volume) self-intersection raise.
        let vc_ref = reference_of("_vnf_centroid").unwrap();
        let vc_deps = [
            pin_reference_of("is_vnf").unwrap(),
            reference_of("is_vector").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("all_nonzero").unwrap(),
            reference_of("sum").unwrap(),
            reference_of("_sum").unwrap(),
            reference_of("is_consistent").unwrap(),
            reference_of("_list_pattern").unwrap(),
            reference_of("approx").unwrap(),
            reference_of("idx").unwrap(),
            reference_of("posmod").unwrap(),
        ];
        let p3 = |x: f64, y: f64, z: f64| Value::num_list(vec![x, y, z]);
        let f = |ids: &[f64]| Value::num_list(ids.to_vec());
        let cube = Value::list(vec![
            Value::list(vec![
                p3(0.0, 0.0, 0.0),
                p3(1.0, 0.0, 0.0),
                p3(1.0, 1.0, 0.0),
                p3(0.0, 1.0, 0.0),
                p3(0.0, 0.0, 1.0),
                p3(1.0, 0.0, 1.0),
                p3(1.0, 1.0, 1.0),
                p3(0.0, 1.0, 1.0),
            ]),
            Value::list(vec![
                f(&[0.0, 3.0, 2.0, 1.0]),
                f(&[4.0, 5.0, 6.0, 7.0]),
                f(&[0.0, 1.0, 5.0, 4.0]),
                f(&[1.0, 2.0, 6.0, 5.0]),
                f(&[2.0, 3.0, 7.0, 6.0]),
                f(&[3.0, 0.0, 4.0, 7.0]),
            ]),
        ]);
        let tet = Value::list(vec![
            Value::list(vec![
                p3(0.0, 0.0, 0.0),
                p3(1.0, 0.0, 0.0),
                p3(0.0, 1.0, 0.0),
                p3(0.0, 0.0, 1.0),
            ]),
            Value::list(vec![
                f(&[0.0, 2.0, 1.0]),
                f(&[0.0, 1.0, 3.0]),
                f(&[1.0, 2.0, 3.0]),
                f(&[0.0, 3.0, 2.0]),
            ]),
        ]);
        // one open face only → summed signed volume ≈ 0 → the self-intersection assert raises
        let flat = Value::list(vec![
            Value::list(vec![
                p3(0.0, 0.0, 0.0),
                p3(1.0, 0.0, 0.0),
                p3(0.0, 1.0, 0.0),
            ]),
            Value::list(vec![f(&[0.0, 1.0, 2.0])]),
        ]);
        let vc_cases = [
            cube,
            tet,
            flat,
            Value::list(vec![Value::list(vec![]), Value::list(vec![])]),
            Value::Undef,
            Value::Num(3.0),
        ];
        for vnf in &vc_cases {
            let args = [vnf.clone()];
            assert!(
                same_result(
                    &super::vnf_centroid(&args),
                    &interpret_with_deps_consts(vc_ref, &vc_deps, &consts, &args)
                ),
                "_vnf_centroid diverged on {vnf:?}"
            );
        }

        // _group_sort_by_index — grouping, ordering, NaN/mixed-type key drops, empty/single/scalar.
        let gs_ref = reference_of("_group_sort_by_index").unwrap();
        let rows = |ks: &[f64]| {
            let v: Vec<Value> = ks
                .iter()
                .enumerate()
                .map(|(i, &k)| {
                    #[allow(clippy::cast_precision_loss, reason = "tiny test indices")]
                    Value::list(vec![Value::Num(k), Value::Num(i as f64)])
                })
                .collect();
            Value::list(v)
        };
        let gs_cases: Vec<Vec<Value>> = vec![
            vec![rows(&[3.0, 1.0, 2.0, 1.0, 3.0]), Value::Num(0.0)],
            vec![rows(&[1.0, 1.0, 1.0]), Value::Num(0.0)],
            vec![rows(&[5.0, 4.0, 3.0, 2.0, 1.0]), Value::Num(0.0)],
            vec![rows(&[1.0, 2.0, 3.0, 4.0, 5.0]), Value::Num(0.0)],
            vec![rows(&[2.0, f64::NAN, 1.0]), Value::Num(0.0)],
            vec![rows(&[1.0]), Value::Num(0.0)],
            vec![Value::list(vec![]), Value::Num(0.0)],
            vec![
                Value::list(vec![
                    Value::list(vec![Value::Num(1.0)]),
                    Value::list(vec![Value::string("a")]),
                    Value::list(vec![Value::Num(0.0)]),
                ]),
                Value::Num(0.0),
            ],
            vec![Value::Num(5.0), Value::Num(0.0)],
            vec![rows(&[2.0, 1.0]), Value::Undef],
        ];
        for args in &gs_cases {
            assert!(
                same_result(
                    &super::group_sort_by_index(args),
                    &interpret_with_deps_consts(gs_ref, &[], &consts, args)
                ),
                "_group_sort_by_index diverged on {args:?}"
            );
        }
    }

    #[test]
    fn fast_equals_slow_band5_batch2() {
        let consts = [("_EPSILON", Value::Num(1e-9))];

        // ident / the axis rotations — sizes, angle values incl. the snap-relevant right angles, raises.
        let id_ref = reference_of("ident").unwrap();
        for n in [
            Value::Num(0.0),
            Value::Num(1.0),
            Value::Num(3.0),
            Value::Num(4.0),
            Value::Num(2.5),
            Value::Undef,
            Value::string("n"),
        ] {
            let args = [n.clone()];
            assert!(
                same_result(
                    &super::ident(&args),
                    &interpret_with_deps_consts(id_ref, &[], &consts, &args)
                ),
                "ident diverged on {n:?}"
            );
        }
        let rot_deps = [
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
        ];
        let angles = [
            None,
            Some(Value::Num(0.0)),
            Some(Value::Num(90.0)),
            Some(Value::Num(-30.0)),
            Some(Value::Num(123.456)),
            Some(Value::Num(f64::NAN)),
            Some(Value::Undef),
        ];
        for (name, func) in [
            ("affine3d_zrot", super::affine3d_zrot as super::Intrinsic),
            ("affine3d_xrot", super::affine3d_xrot),
            ("affine3d_yrot", super::affine3d_yrot),
        ] {
            let r = reference_of(name).unwrap();
            for ang in &angles {
                let args: Vec<Value> = ang.iter().cloned().collect();
                assert!(
                    same_result(
                        &func(&args),
                        &interpret_with_deps_consts(r, &rot_deps, &consts, &args)
                    ),
                    "{name} diverged on {ang:?}"
                );
            }
        }

        // _get_ear — the concave L-polygon (has real ears at various _i), a triangle (immediate 0), a
        // whisker polygon (duplicate-adjacent vertices, no ears), and the raise/exotic lanes.
        let ge_ref = reference_of("_get_ear").unwrap();
        let ge_deps = [
            reference_of("_tri_class").unwrap(),
            reference_of("_none_inside").unwrap(),
            reference_of("_is_at_left").unwrap(),
            reference_of("select").unwrap(),
            reference_of("idx").unwrap(),
            reference_of("posmod").unwrap(),
            reference_of("approx").unwrap(),
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("is_vector").unwrap(),
            pin_reference_of("is_range").unwrap(),
            reference_of("all_nonzero").unwrap(),
        ];
        // CW L-poly (BOSL2's earcut runs on CW): reversed order of the CCW L used in the earcut battery
        let lpoly_cw = Value::list(vec![
            p2(2.0, 0.0),
            p2(2.0, 1.0),
            p2(1.0, 1.0),
            p2(1.0, 2.0),
            p2(0.0, 2.0),
            p2(0.0, 0.0),
        ]);
        let tri_ind = Value::num_list(vec![0.0, 1.0, 2.0]);
        let all6 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        // a degenerate spike: b == d, so every candidate fails and the whisker lane fires
        let spike = Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(2.0, 0.0), p2(1.0, 0.0)]);
        let all4 = Value::num_list(vec![0.0, 1.0, 2.0, 3.0]);
        let e9 = Value::Num(1e-9);
        let ge_cases: Vec<Vec<Value>> = vec![
            vec![lpoly_cw.clone(), all6.clone(), e9.clone()],
            vec![lpoly_cw.clone(), all6.clone(), e9.clone(), Value::Num(3.0)],
            vec![lpoly_cw.clone(), tri_ind.clone(), e9.clone()],
            vec![spike.clone(), all4.clone(), e9.clone()],
            vec![spike.clone(), all4.clone(), Value::Undef],
            vec![Value::Undef, all4.clone(), e9.clone()],
            vec![lpoly_cw.clone(), Value::Num(7.0), e9.clone()],
        ];
        for args in &ge_cases {
            assert!(
                same_result(
                    &super::get_ear(args),
                    &interpret_with_deps_consts(ge_ref, &ge_deps, &consts, args)
                ),
                "_get_ear diverged on {args:?}"
            );
        }

        // in_list / is_path — hits, misses, idx-column lookups, the all-hits retry (a first hit that
        // doesn't match), raises, and is_path's dim/fast lanes.
        let il_ref = reference_of("in_list").unwrap();
        let il_deps = [
            reference_of("is_finite").unwrap(),
            reference_of("is_nan").unwrap(),
            reference_of("is_def").unwrap(),
        ];
        let nums = Value::num_list(vec![3.0, 5.0, 7.0]);
        let rows = Value::list(vec![
            Value::list(vec![Value::Num(1.0), Value::string("a")]),
            Value::list(vec![Value::Num(2.0), Value::string("b")]),
        ]);
        let il_cases: Vec<Vec<Value>> = vec![
            vec![Value::Num(5.0), nums.clone()],
            vec![Value::Num(4.0), nums.clone()],
            vec![Value::string("b"), rows.clone(), Value::Num(1.0)],
            vec![Value::string("c"), rows.clone(), Value::Num(1.0)],
            vec![Value::Num(2.0), rows.clone(), Value::Num(0.0)],
            vec![Value::string("a"), rows.clone()],
            vec![Value::Num(1.0), Value::Num(9.0)],
            vec![Value::Num(1.0), nums.clone(), Value::string("i")],
            vec![Value::Undef, nums.clone()],
        ];
        for args in &il_cases {
            assert!(
                same_result(
                    &super::in_list(args),
                    &interpret_with_deps_consts(il_ref, &il_deps, &consts, args)
                ),
                "in_list diverged on {args:?}"
            );
        }
        let ip_ref = reference_of("is_path").unwrap();
        let ip_deps: Vec<&str> = il_deps
            .iter()
            .copied()
            .chain([
                reference_of("is_matrix").unwrap(),
                reference_of("is_vector").unwrap(),
                reference_of("is_consistent").unwrap(),
                reference_of("_list_pattern").unwrap(),
                reference_of("in_list").unwrap(),
                reference_of("force_list").unwrap(),
                reference_of("all_nonzero").unwrap(),
            ])
            .collect();
        let path2 = Value::list(vec![p2(0.0, 0.0), p2(1.0, 0.0), p2(1.0, 1.0)]);
        let path4 = Value::list(vec![
            Value::num_list(vec![0.0, 0.0, 0.0, 0.0]),
            Value::num_list(vec![1.0, 0.0, 0.0, 0.0]),
        ]);
        let ip_cases: Vec<Vec<Value>> = vec![
            vec![path2.clone()],
            vec![path4.clone()],
            vec![path4.clone(), Value::Num(4.0)],
            vec![path2.clone(), Value::Undef],
            vec![path2.clone(), Value::num_list(vec![3.0])],
            vec![
                path2.clone(),
                Value::num_list(vec![2.0, 3.0]),
                Value::Bool(true),
            ],
            vec![
                Value::Num(5.0),
                Value::num_list(vec![2.0, 3.0]),
                Value::Bool(true),
            ],
            vec![Value::list(vec![p2(0.0, 0.0)])],
            vec![Value::Undef],
        ];
        for args in &ip_cases {
            assert!(
                same_result(
                    &super::is_path(args),
                    &interpret_with_deps_consts(ip_ref, &ip_deps, &consts, args)
                ),
                "is_path diverged on {args:?}"
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
