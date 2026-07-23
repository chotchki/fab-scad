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

use std::sync::OnceLock;

use super::value::Value;
use crate::parser::{Expr, Parameter};

mod affine;
mod fingerprint;
mod geometry;
mod lists;
mod math;
mod poc;
mod regions;
mod shape;
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::float_cmp,
    reason = "test harness: expect/panic ARE the assertions; intrinsics must bit-match, so == is exact"
)]
mod tests;
mod vectors;

pub(super) use fingerprint::fingerprint;

// The fast==slow harness addresses every native as `super::<name>`; these bindings keep those
// paths valid from the tests submodule.
#[cfg(test)]
use affine::{
    affine3d_identity, affine3d_rot_by_axis, affine3d_rot_from_to, affine3d_translate,
    affine3d_xrot, affine3d_yrot, affine3d_zrot, apply, apply_transform, ident, is_2d_transform,
    rot,
};
#[cfg(test)]
use geometry::{
    get_ear, is_at_left, is_point_on_line, none_inside, point_dist, point2d, tri_class,
    vnf_centroid,
};
#[cfg(test)]
use lists::{force_list, group_sort_by_index, idx, in_list};
#[cfg(test)]
use math::{approx, posmod, sum, sum_tail};
#[cfg(test)]
use poc::{poc_isup, poc_near0, poc_sq};
#[cfg(test)]
use shape::{
    all_nonzero, is_consistent, is_matrix, is_path, is_vector, list_pattern, num_defined,
    same_shape,
};
#[cfg(test)]
use vectors::{bt_search, unit, v_abs, v_theta, vector_angle, vector_axis};

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
/// A named top-level constant + a builder for its expected `Value` (statics can't hold one directly).
type ValueConst = (&'static str, fn() -> Value);

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
    /// The VALUE-typed half of the const guard (O.8): named top-level constants whose baked value is NOT a
    /// number — BOSL2's direction vectors (`UP`/`RIGHT`) and sentinels (`_NO_ARG`). Each `fn()` builds the
    /// expected `Value` (statics can't hold one); the arm step compares it against the home-scope binding
    /// BIT-level ([`value_bits_eq`]: f64s by `to_bits`, exact variant, recursive) — same
    /// wire-only-if-proven contract as `consts`, same post-hoist arm timing.
    pub(super) consts_v: &'static [ValueConst],
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
    // O.10 band dep (vectors.scad).
    (
        "vector_search",
        "function vector_search(query, r, target) =
    query==[] ? [] :
    is_list(query) && target==[] ? is_vector(query) ? [] : [for(q=query) [] ] :
    assert( is_finite(r) && r>=0, 
            \"\\nThe query radius should be a positive number.\" )
    let(
        tgpts  = is_matrix(target),   // target is a point list
        tgtree = is_list(target)      // target is a tree
                 && (len(target)==2)
                 && is_matrix(target[0])
                 && is_list(target[1])
                 && (len(target[1])==4 || (len(target[1])==1 && is_list(target[1][0])) )
    )
    assert( tgpts || tgtree, 
            \"\\nThe target should be a list of points or a search tree compatible with the query.\" )
    let( 
        dim    = tgpts ? len(target[0]) : len(target[0][0]),
        simple = is_vector(query, dim)
        )
    assert( simple || is_matrix(query,undef,dim), 
            \"\\nThe query points should be a list of points compatible with the target point list.\")
    tgpts 
    ?   len(target)<=400
        ?   simple ? [for(i=idx(target)) if(norm(target[i]-query)<=r) i ] :
            [for(q=query) [for(i=idx(target)) if(norm(target[i]-q)<=r) i ] ]
        :   let( tree = _bt_tree(target, count(len(target)), leafsize=25) )
            simple ? _bt_search(query, r, target, tree) :
            [for(q=query) _bt_search(q, r, target, tree)]
    :   simple ?  _bt_search(query, r, target[0], target[1]) :
        [for(q=query) _bt_search(q, r, target[0], target[1])];",
    ),
    // O.10 band dep (vectors.scad).
    (
        "_bt_tree",
        "function _bt_tree(points, ind, leafsize=25) =
    len(ind)<=leafsize ? [ind] :
    let( 
        bounds = pointlist_bounds(select(points,ind)),
        coord  = max_index(bounds[1]-bounds[0]), 
        projc  = [for(i=ind) points[i][coord] ],
        meanpr = mean(projc), 
        pivot  = min_index([for(p=projc) abs(p-meanpr)]),
        radius = max([for(i=ind) norm(points[ind[pivot]]-points[i]) ]),
        Lind   = [for(i=idx(ind)) if(projc[i]<=meanpr && i!=pivot) ind[i] ],
        Rind   = [for(i=idx(ind)) if(projc[i] >meanpr && i!=pivot) ind[i] ]
      )
    [ ind[pivot], radius, _bt_tree(points, Lind, leafsize), _bt_tree(points, Rind, leafsize) ];",
    ),
    // O.10 band dep (utility.scad) — `column`'s index assert.
    (
        "is_int",
        "function is_int(n) = is_finite(n) && n == round(n);",
    ),
    // O.10 band dep (comparisons.scad).
    (
        "list_wrap",
        "function list_wrap(list, eps=_EPSILON) =
    assert(is_list(list))
    assert(is_finite(eps) && eps>=0)
    len(list)<2 || are_ends_equal(list,eps=eps)? list : [each list, list[0]];",
    ),
    // O.10 band dep (comparisons.scad).
    (
        "are_ends_equal",
        "function are_ends_equal(list, eps=_EPSILON) =
  assert(is_list(list) && len(list)>0, \"Must give a nonempty list\")
  approx(list[0], list[len(list)-1], eps=eps);",
    ),
    // O.10 band dep (geometry.scad).
    (
        "_general_line_intersection",
        "function _general_line_intersection(s1,s2,eps=_EPSILON) =
    let(
        denominator = cross(s1[0]-s1[1],s2[0]-s2[1])
    )
    approx(denominator,0,eps=eps) ? undef :
    let(
        t = cross(s1[0]-s2[0],s2[0]-s2[1]) / denominator,
        u = cross(s1[0]-s2[0],s1[0]-s1[1]) / denominator
    )
    [s1[0]+t*(s1[1]-s1[0]), t, u];",
    ),
    // O.10 band dep (lists.scad).
    (
        "flatten",
        "function flatten(l) =
    !is_list(l)? l :
    [for (a=l) if (is_list(a)) (each a) else a];",
    ),
    // O.10 band dep (linalg.scad).
    (
        "column",
        "function column(M, i) =
    assert( is_list(M), \"The input is not a list.\" )
    assert( is_int(i) && i>=0, \"Invalid index\")
    [for(row=M) row[i]];",
    ),
    // O.10 band dep (math.scad).
    (
        "count",
        "function count(n,s=0,step=1,reverse=false) = let(n=is_list(n) ? len(n) : n)
                                             reverse? [for (i=[n-1:-1:0]) s+i*step]
                                                    : [for (i=[0:1:n-1]) s+i*step];",
    ),
    // O.10 band dep (math.scad).
    (
        "mean",
        "function mean(v) = 
    assert(is_list(v) && len(v)>0, \"\\nInvalid list.\")
    sum(v)/len(v);",
    ),
    // O.10 band dep (comparisons.scad).
    (
        "min_index",
        "function min_index(vals, all=false) =
    assert( is_vector(vals), \"Invalid or list of numbers.\")
    all ? search(min(vals),vals,0) : search(min(vals), vals)[0];",
    ),
    // O.10 band dep (comparisons.scad).
    (
        "max_index",
        "function max_index(vals, all=false) =
    assert( is_vector(vals) && len(vals)>0 , \"Invalid or empty list of numbers.\")
    all ? search(max(vals),vals,0) : search(max(vals), vals)[0];",
    ),
    // O.10 band dep (linalg.scad).
    (
        "transpose",
        "function transpose(M, reverse=false) =
    assert( is_list(M) && len(M)>0, \"Input to transpose must be a nonempty list.\")
    is_list(M[0])
    ?   let( len0 = len(M[0]) )
        assert([for(a=M) if(!is_list(a) || len(a)!=len0) 1 ]==[], \"Input to transpose has inconsistent row lengths.\" )
        reverse
        ? [for (i=[0:1:len0-1]) 
              [ for (j=[0:1:len(M)-1]) M[len(M)-1-j][len0-1-i] ] ] 
        : [for (i=[0:1:len0-1]) 
              [ for (j=[0:1:len(M)-1]) M[j][i] ] ] 
    :  assert( is_vector(M), \"Input to transpose must be a vector or list of lists.\")
           M;",
    ),
    // O.10 band dep (vectors.scad).
    (
        "pointlist_bounds",
        "function pointlist_bounds(pts) =
    assert(is_path(pts,dim=undef,fast=true) , \"\\nInvalid pointlist.\" )
    let(
        select = ident(len(pts[0])),
        spread = [
            for(i=[0:len(pts[0])-1])
            let( spreadi = pts*select[i] )
            [ min(spreadi), max(spreadi) ]
        ]
    ) transpose(spread);",
    ),
    // O.10 band dep (comparisons.scad).
    (
        "_sort_vectors",
        "function _sort_vectors(arr, idxlist, _i=0) =
    len(arr)<=1 || ( is_list(idxlist) && _i>=len(idxlist) ) || _i>=len(arr[0])  ? arr :
    let(
        k = is_list(idxlist) ? idxlist[_i] : _i,
        pivot   = arr[floor(len(arr)/2)][k],
        lesser  = [ for (entry=arr) if (entry[k]  < pivot ) entry ],
        equal   = [ for (entry=arr) if (entry[k] == pivot ) entry ],
        greater = [ for (entry=arr) if (entry[k]  > pivot ) entry ]
      )
    concat(
        _sort_vectors(lesser,  idxlist, _i  ), 
        _sort_vectors(equal,   idxlist, _i+1), 
        _sort_vectors(greater, idxlist, _i  ) );",
    ),
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
    // linalg.scad — `apply`'s mirror-detection chain. From `apply` only the 4×4 lane is reachable (the vnf
    // branch's _apply asserts force tdim==datadim==3 before determinant runs), but the pins cover the whole
    // bodies.
    (
        "determinant",
        "function determinant(M) =
    assert(is_list(M), \"Input must be a square matrix.\" )
    len(M)==1? M[0][0] :
    len(M)==2? det2(M) :
    len(M)==3? det3(M) :
    len(M)==4? det4(M) :
    assert(is_matrix(M, square=true), \"Input must be a square matrix.\" )
    sum(
        [for (col=[0:1:len(M)-1])
            ((col%2==0)? 1 : -1) *
                M[col][0] *
                determinant(
                    [for (r=[1:1:len(M)-1])
                        [for (c=[0:1:len(M)-1])
                            if (c!=col) M[c][r]
                        ]
                    ]
                )
        ]
    );",
    ),
    (
        "det2",
        "function det2(M) =
    assert(is_def(M) && M*0==[[0,0],[0,0]], \"Expected square matrix (2x2)\")
    cross(M[0],M[1]);",
    ),
    (
        "det3",
        "function det3(M) =
    assert(is_def(M) && M*0==[[0,0,0],[0,0,0],[0,0,0]], \"Expected square matrix (3x3).\")
    M[0][0] * (M[1][1]*M[2][2]-M[2][1]*M[1][2]) -
    M[1][0] * (M[0][1]*M[2][2]-M[2][1]*M[0][2]) +
    M[2][0] * (M[0][1]*M[1][2]-M[1][1]*M[0][2]);",
    ),
    (
        "det4",
        "function det4(M) =
    assert(is_def(M) && M*0==[[0,0,0,0],[0,0,0,0],[0,0,0,0],[0,0,0,0]], \"Expected square matrix (4x4).\")
    M[0][0]*M[1][1]*M[2][2]*M[3][3] + M[0][0]*M[1][2]*M[2][3]*M[3][1] + M[0][0]*M[1][3]*M[2][1]*M[3][2]
    + M[0][1]*M[1][0]*M[2][3]*M[3][2] + M[0][1]*M[1][2]*M[2][0]*M[3][3] + M[0][1]*M[1][3]*M[2][2]*M[3][0]
    + M[0][2]*M[1][0]*M[2][1]*M[3][3] + M[0][2]*M[1][1]*M[2][3]*M[3][0] + M[0][2]*M[1][3]*M[2][0]*M[3][1]
    + M[0][3]*M[1][0]*M[2][2]*M[3][1] + M[0][3]*M[1][1]*M[2][0]*M[3][2] + M[0][3]*M[1][2]*M[2][1]*M[3][0]
    - M[0][0]*M[1][1]*M[2][3]*M[3][2] - M[0][0]*M[1][2]*M[2][1]*M[3][3] - M[0][0]*M[1][3]*M[2][2]*M[3][1]
    - M[0][1]*M[1][0]*M[2][2]*M[3][3] - M[0][1]*M[1][2]*M[2][3]*M[3][0] - M[0][1]*M[1][3]*M[2][0]*M[3][2]
    - M[0][2]*M[1][0]*M[2][3]*M[3][1] - M[0][2]*M[1][1]*M[2][0]*M[3][3] - M[0][2]*M[1][3]*M[2][1]*M[3][0]
    - M[0][3]*M[1][0]*M[2][1]*M[3][2] - M[0][3]*M[1][1]*M[2][2]*M[3][0] - M[0][3]*M[1][2]*M[2][0]*M[3][1];",
    ),
    // lists.scad — BOSL2's `reverse` SHADOWS the builtin (the per-entry shadow check knows); reached from
    // `apply` via `vnf_reverse_faces`. Its string lane reaches `str_join` (strings.scad).
    (
        "reverse",
        "function reverse(list) =
    assert(is_list(list)||is_string(list), str(\"Input to reverse must be a list or string. Got: \",list))
    let (elems = [ for (i = [len(list)-1 : -1 : 0]) list[i] ])
    is_string(list)? str_join(elems) : elems;",
    ),
    (
        "vnf_reverse_faces",
        "function vnf_reverse_faces(vnf) =
    [vnf[0], [for (face=vnf[1]) reverse(face)]];",
    ),
    (
        "str_join",
        "function str_join(list,sep=\"\",_i=0, _result=\"\") =
    assert(is_list(list))
    _i >= len(list)-1 ? (_i==len(list) ? _result : str(_result,list[_i])) :
    str_join(list,sep,_i+1,str(_result,list[_i],sep));",
    ),
    // transforms/linalg/utility/lists.scad — `rot`'s closure. From rot, `move` is only ever called with a
    // VECTOR cp (rot's own assert), so its string lane (centroid/mean/pointlist_bounds) stays unreachable;
    // `rot_inverse` (the reverse=true lane) pulls hstack → all → _all_bool/is_func and min/max_length.
    (
        "move",
        "function move(v=[0,0,0], p=_NO_ARG) =
    is_string(v) ? (
        assert(is_vnf(p) || is_path(p),\"String movements only work with point lists and VNFs\")
        let(
             center = v==\"centroid\" ? centroid(p)
                    : v==\"mean\" ? mean(p)
                    : v==\"box\" ? mean(pointlist_bounds(p))
                    : assert(false,str(\"Unknown string movement \",v))
        )
        move(-center,p=p)
      )
    :
    assert(is_vector(v) && (len(v)==3 || len(v)==2), \"Invalid value for `v`\")
    let(
        m = affine3d_translate(point3d(v))
    )
    p==_NO_ARG ? m : apply(m, p);",
    ),
    (
        "rot_inverse",
        "function rot_inverse(T) =
    assert(is_matrix(T,square=true),\"Matrix must be square\")
    let( n = len(T))
    assert(n==3 || n==4, \"Matrix must be 3x3 or 4x4\")
    let(
        rotpart =  [for(i=[0:n-2]) [for(j=[0:n-2]) T[j][i]]],
        transpart = [for(row=[0:n-2]) T[row][n-1]]
    )
    assert(approx(determinant(T),1),\"Matrix is not a rotation\")
    concat(hstack(rotpart, -rotpart*transpart),[[for(i=[2:n]) 0, 1]]);",
    ),
    (
        "hstack",
        "function hstack(M1, M2, M3) =
    (M3!=undef)? hstack([M1,M2,M3]) :
    (M2!=undef)? hstack([M1,M2]) :
    assert(all([for(v=M1) is_list(v)]), \"One of the inputs to hstack is not a list\")
    let(
        minlen = min_length(M1),
        maxlen = max_length(M1)
    )
    assert(minlen==maxlen, \"Input vectors to hstack must have the same length\")
    [for(row=[0:1:minlen-1])
        [for(matrix=M1)
           each matrix[row]
        ]
    ];",
    ),
    (
        "all",
        "function all(l, func) =
    assert(is_list(l), \"The input is not a list.\")
    assert(func==undef || is_func(func))
    is_func(func)
      ? _all_func(l, func)
      : _all_bool(l);",
    ),
    (
        "_all_bool",
        "function _all_bool(l, i=0, out=true) =
    i >= len(l) || !out? out :
    _all_bool(l, i=i+1, out=out && l[i]);",
    ),
    (
        "is_func",
        "function is_func(x) = version_num()>20210000 && is_function(x);",
    ),
    (
        "min_length",
        "function min_length(list) =
    assert(is_list(list), \"Invalid input.\" )
    min([for (v = list) len(v)]);",
    ),
    (
        "max_length",
        "function max_length(list) =
    assert(is_list(list), \"Invalid input.\" )
    max([for (v = list) len(v)]);",
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
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: poc::poc_sq,
    },
    // BOSL2 `is_def`/`is_str` — the two hottest LEAF predicates (called in nearly every optional-arg check
    // and string guard). Verbatim from libs/BOSL2/builtins.scad.
    Entry {
        name: "is_def",
        reference: "function is_def(x) = !is_undef(x);",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_undef"],
        func: shape::is_def,
    },
    Entry {
        name: "is_str",
        reference: "function is_str(x) = is_string(x);",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_string"],
        func: shape::is_str,
    },
    // BOSL2 `is_nan`/`is_finite` — the #1 and #2 hottest user functions on the model profile (56% of calls
    // combined), the workhorses of BOSL2's input validation. Verbatim from libs/BOSL2/utility.scad.
    Entry {
        name: "is_nan",
        reference: "function is_nan(x) = (x!=x);",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: shape::is_nan,
    },
    Entry {
        name: "is_finite",
        reference: "function is_finite(x) = is_num(x) && !is_nan(0*x);",
        consts: &[],
        consts_v: &[],
        deps: &["is_nan"],
        builtins: &["is_num"],
        func: shape::is_finite,
    },
    // BOSL2 `last` (9.6% of user-fn calls) + `default` (2.5%) — the next two down the profile. Both call only
    // builtins (`len`, `is_undef`), so the plain interpreter is their oracle. Verbatim from lists.scad /
    // utility.scad.
    Entry {
        name: "last",
        reference: "function last(list) = list[len(list)-1];",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["len"],
        func: lists::last,
    },
    Entry {
        name: "default",
        reference: "function default(v,dflt=undef) = is_undef(v)? dflt : v;",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_undef"],
        func: lists::default,
    },
    // `_is_liststr` (2.2%) — a pure leaf (calls only the `is_str` intrinsic + the `is_list` builtin), from
    // strings.scad. `point3d` (1.8%) from coords.scad — the first intrinsic with an inline `assert` (raises on
    // a non-list, exercising the fallible ABI) that also BUILDS a value.
    Entry {
        name: "_is_liststr",
        reference: "function _is_liststr(s) = is_list(s) || is_str(s);",
        consts: &[],
        consts_v: &[],
        deps: &["is_str"],
        builtins: &["is_list", "is_string"],
        func: shape::is_liststr,
    },
    Entry {
        name: "point3d",
        reference: "function point3d(p, fill=0) = assert(is_list(p)) [for (i=[0:2]) (p[i]==undef)? fill : p[i]];",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_list"],
        func: geometry::point3d,
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
        consts_v: &[],
        deps: &["is_vector", "is_range", "is_finite", "is_nan"],
        builtins: &["len", "is_list", "is_string", "is_num", "norm", "is_undef"],
        func: lists::select,
    },
    // The CONST-GUARD POC (O.5.1, a synthetic collision-proof name like `_fab_poc_sq`): its reference bakes
    // the top-level constant `_EPSILON`, so it exercises the guarded-arm path end-to-end — it wires only
    // AFTER island globals are built and only when the home scope's `_EPSILON` is bit-exactly 1e-9
    // (`super::arm_guarded_intrinsics`). The real `_EPSILON` family (is_vector/approx/_tri_class…) is O.5.2+.
    Entry {
        name: "_fab_poc_near0",
        reference: "function _fab_poc_near0(x) = abs(x) < _EPSILON;",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &[],
        builtins: &["abs"],
        func: poc::poc_near0,
    },
    // The VALUE-const guard POC (O.8): bakes the vector constant `UP` — wires only when the home scope's
    // `UP` is bit-exactly `[0,0,1]` AS A NumList ([`value_bits_eq`] is variant-exact). The real consumers
    // (vector_axis's UP/RIGHT, rot's _NO_ARG sentinel) are O.9.
    Entry {
        name: "_fab_poc_isup",
        reference: "function _fab_poc_isup(v) = v == UP;",
        consts: &[],
        consts_v: &[("UP", poc::poc_up_value)],
        deps: &[],
        builtins: &[],
        func: poc::poc_isup,
    },
    // ── O.5.2, the SHAPE band (utility.scad / lists.scad) ────────────────────────────────────────────────
    // The `is_consistent`/`_list_pattern`/`same_shape` bundle is ~4.7s of self time across the O.4 four
    // (every BOSL2 path/vector assert funnels through it), `num_defined`/`force_list` are its cheap leaf
    // companions. All verbatim; every op routes through the interpreter's own primitives (`iter_values_raw` for
    // comprehension iteration, `build_vector` for result coalescing, `apply_binary`/`index` for ops), so
    // variant identity (NumList vs List) and exotic-input behavior match by construction.
    Entry {
        name: "_list_pattern",
        reference: "function _list_pattern(list) =
  is_list(list)
  ? [for(entry=list) is_list(entry) ? _list_pattern(entry) : 0]
  : 0;",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_list"],
        func: shape::list_pattern,
    },
    Entry {
        name: "same_shape",
        reference: "function same_shape(a,b) = is_def(b) && _list_pattern(a) == b*0;",
        consts: &[],
        consts_v: &[],
        deps: &["is_def", "_list_pattern"],
        builtins: &["is_undef", "is_list"],
        func: shape::same_shape,
    },
    Entry {
        name: "is_consistent",
        reference: "function is_consistent(list, pattern) =
    is_list(list)
    && (len(list)==0
       || (let(pattern = is_undef(pattern) ? _list_pattern(list[0]): _list_pattern(pattern) )
          []==[for(entry=0*list) if (entry != pattern) entry]));",
        consts: &[],
        consts_v: &[],
        deps: &["_list_pattern"],
        builtins: &["is_list", "len", "is_undef"],
        func: shape::is_consistent,
    },
    Entry {
        name: "num_defined",
        reference: "function num_defined(v) =
    len([for(vi=v) if(!is_undef(vi)) 1]);",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["len", "is_undef"],
        func: shape::num_defined,
    },
    Entry {
        name: "force_list",
        reference: "function force_list(value, n=1, fill) =
    is_list(value) ? value :
    is_undef(fill)? [for (i=[1:1:n]) value] : [value, for (i=[2:1:n]) fill];",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_list", "is_undef"],
        func: lists::force_list,
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
        consts_v: &[],
        deps: &["idx", "posmod", "is_finite", "is_nan"],
        builtins: &["is_bool", "is_num", "abs", "is_list", "is_string", "len"],
        func: math::approx,
    },
    Entry {
        name: "posmod",
        reference: "function posmod(x,m) =
    assert( is_finite(x) && is_finite(m) && !approx(m,0) , \"\\nInput must be finite numbers. The divisor cannot be zero.\")
    (x%m+m)%m;",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &["is_finite", "is_nan", "approx"],
        builtins: &["is_num", "abs", "is_bool"],
        func: math::posmod,
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
        consts_v: &[],
        deps: &["posmod", "is_finite", "is_nan", "approx"],
        builtins: &["is_list", "is_string", "len", "is_num", "abs", "is_bool"],
        func: lists::idx,
    },
    Entry {
        name: "all_nonzero",
        reference: "function all_nonzero(x, eps=_EPSILON) =
    is_finite(x)? abs(x)>eps :
    is_vector(x) && [for (xx=x) if(abs(xx)<eps) 1] == [];",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &["is_finite", "is_nan", "is_vector"],
        builtins: &["is_num", "abs", "is_list", "len", "is_undef"],
        func: shape::all_nonzero,
    },
    Entry {
        name: "is_vector",
        reference: "function is_vector(v, length, zero, all_nonzero=false, eps=_EPSILON) =
    is_list(v) && len(v)>0 && []==[for(vi=v) if(!is_finite(vi)) 0]
    && (is_undef(length) || (assert(is_num(length))len(v)==length))
    && (is_undef(zero) || ((norm(v) >= eps) == !zero))
    && (!all_nonzero || all_nonzero(v)) ;",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &["is_finite", "is_nan", "all_nonzero"],
        builtins: &["is_list", "len", "is_undef", "is_num", "norm", "abs"],
        func: shape::is_vector,
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
        consts_v: &[],
        deps: &["is_vector", "is_finite", "is_nan", "is_consistent", "_list_pattern"],
        builtins: &["is_list", "len", "is_undef", "is_num"],
        func: shape::is_matrix,
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
        consts_v: &[],
        deps: &[],
        builtins: &["cross", "norm", "abs", "sign"],
        func: geometry::tri_class,
    },
    Entry {
        name: "_is_at_left",
        reference: "function _is_at_left(pt,line,eps=_EPSILON) = _tri_class([pt,line[0],line[1]],eps) <= 0;",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &["_tri_class"],
        builtins: &["cross", "norm", "abs", "sign"],
        func: geometry::is_at_left,
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
        consts_v: &[],
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
        func: geometry::none_inside,
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
        consts_v: &[],
        deps: &[],
        builtins: &["len"],
        func: math::sum_tail,
    },
    Entry {
        name: "sum",
        reference: "function sum(v, dflt=0) =
    v==[]? dflt :
    assert(is_consistent(v), \"\\nInput to sum is non-numeric or inconsistent.\")
    is_finite(v[0]) || is_vector(v[0]) ? [for(i=v) 1]*v :
    _sum(v,v[0]*0);",
        consts: &[],
        consts_v: &[],
        deps: &[
            "is_consistent",
            "_list_pattern",
            "is_finite",
            "is_nan",
            "is_vector",
            "_sum",
        ],
        builtins: &["is_list", "len", "is_undef", "is_num"],
        func: math::sum,
    },
    Entry {
        name: "unit",
        reference: "function unit(v, error=[[[\"ASSERT\"]]]) =
    assert(is_vector(v), \"\\nInvalid vector.\")
    norm(v)<_EPSILON? (error==[[[\"ASSERT\"]]]? assert(norm(v)>=_EPSILON,\"\\nCannot normalize a zero vector.\") : error) :
    v/norm(v);",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["norm", "is_list", "len", "is_undef", "is_num"],
        func: vectors::unit,
    },
    Entry {
        name: "is_2d_transform",
        reference: "function is_2d_transform(t) =    // z-parameters are zero, except we allow t[2][2]!=1 so scale() works
  t[2][0]==0 && t[2][1]==0 && t[2][3]==0 && t[0][2] == 0 && t[1][2]==0 &&
  (t[2][2]==1 || !(t[0][0]==1 && t[0][1]==0 && t[1][0]==0 && t[1][1]==1));",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: affine::is_2d_transform,
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
        consts_v: &[],
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
        func: affine::apply_transform,
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
        consts_v: &[],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["is_list", "len", "is_num", "norm", "concat", "is_undef"],
        func: vectors::bt_search,
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
        consts_v: &[],
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
        func: vectors::vector_angle,
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
        consts_v: &[],
        deps: &["select", "is_vector", "is_range", "is_finite", "is_nan"],
        builtins: &[
            "min", "norm", "len", "is_list", "is_string", "is_num", "is_undef",
        ],
        func: geometry::point_dist,
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
        consts_v: &[],
        deps: &["force_list"],
        builtins: &["abs", "cross", "norm", "len", "is_list", "is_undef"],
        func: geometry::is_point_on_line,
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
        consts_v: &[],
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
        func: geometry::vnf_centroid,
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
        consts_v: &[],
        deps: &[],
        builtins: &[],
        func: affine::ident,
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
        consts_v: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine::affine3d_zrot,
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
        consts_v: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine::affine3d_xrot,
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
        consts_v: &[],
        deps: &["is_finite", "is_nan"],
        builtins: &["sin", "cos", "is_num"],
        func: affine::affine3d_yrot,
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
        consts_v: &[],
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
        func: geometry::get_ear,
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
        consts_v: &[],
        deps: &["is_finite", "is_nan", "is_def"],
        builtins: &["search", "is_list", "is_undef", "is_num"],
        func: lists::in_list,
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
        consts_v: &[],
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
        func: shape::is_path,
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
        consts_v: &[],
        deps: &[],
        builtins: &["len", "floor", "concat"],
        func: lists::group_sort_by_index,
    },
    // ── O.9 tree 1 (vectors/coords/affine.scad) — the band the O.8 Value-const guard unlocked ───────────
    // vector_axis bakes UP/RIGHT (consts_v) and affine3d_rot_from_to composes it with the whole
    // vector_angle/approx knot; v_abs/v_theta/point2d/affine3d_identity are their small deps, landed as
    // entries so the pins are natives too.
    Entry {
        name: "v_abs",
        reference: "function v_abs(v) =
    assert( is_vector(v), \"\\nInvalid vector.\" )
    [for (x=v) abs(x)];",
        consts: &[],
        consts_v: &[],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["abs", "is_list", "len", "is_undef", "is_num"],
        func: vectors::v_abs,
    },
    Entry {
        name: "v_theta",
        reference: "function v_theta(v) =
    assert( is_vector(v,2) || is_vector(v,3) , \"\\nInvalid vector.\")
    atan2(v.y,v.x);",
        consts: &[],
        consts_v: &[],
        deps: &["is_vector", "is_finite", "is_nan"],
        builtins: &["atan2", "is_list", "len", "is_undef", "is_num"],
        func: vectors::v_theta,
    },
    Entry {
        name: "point2d",
        reference: "function point2d(p, fill=0) = assert(is_list(p)) [for (i=[0:1]) (p[i]==undef)? fill : p[i]];",
        consts: &[],
        consts_v: &[],
        deps: &[],
        builtins: &["is_list"],
        func: geometry::point2d,
    },
    Entry {
        name: "affine3d_identity",
        reference: "function affine3d_identity() = ident(4);",
        consts: &[],
        consts_v: &[],
        deps: &["ident"],
        builtins: &[],
        func: affine::affine3d_identity,
    },
    // `is_vector(v1, zero=false)` runs the zero clause with its DEFAULT eps → the `_EPSILON` guard; the
    // `all_nonzero` branch stays unreachable (the flag is never passed). The in-body `eps = 1e-6` is a
    // LITERAL, not a constant read.
    Entry {
        name: "vector_axis",
        reference: "function vector_axis(v1,v2=undef,v3=undef) =
    is_vector(v3)
    ?   assert(is_consistent([v3,v2,v1]), \"\\nBad arguments.\")
        vector_axis(v1-v2, v3-v2)
    :   assert( is_undef(v3), \"\\nBad arguments.\")
        is_undef(v2)
        ?   assert( is_list(v1), \"\\nBad arguments.\")
            len(v1) == 2
            ?   vector_axis(v1[0],v1[1])
            :   vector_axis(v1[0],v1[1],v1[2])
        :   assert( is_vector(v1,zero=false) && is_vector(v2,zero=false) && is_consistent([v1,v2])
                    , \"\\nBad arguments.\")
            let(
              eps = 1e-6,
              w1 = point3d(v1/norm(v1)),
              w2 = point3d(v2/norm(v2)),
              w3 = (norm(w1-w2) > eps && norm(w1+w2) > eps) ? w2
                   : (norm(v_abs(w2)-UP) > eps)? UP
                   : RIGHT
            ) unit(cross(w1,w3));",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[("UP", vectors::bosl_up), ("RIGHT", vectors::bosl_right)],
        deps: &[
            "is_vector",
            "is_finite",
            "is_nan",
            "is_consistent",
            "_list_pattern",
            "point3d",
            "unit",
            "v_abs",
        ],
        builtins: &[
            "is_list", "len", "is_undef", "is_num", "norm", "abs", "cross",
        ],
        func: vectors::vector_axis,
    },
    // `approx(from,to)` on two 3-vectors runs the LIST branch → the idx/posmod knot rides in; the
    // vector_angle chain (same_shape/constrain-pin/is_matrix) too. UP/RIGHT guard because the native
    // composes [`vector_axis`], which bakes them.
    Entry {
        name: "affine3d_rot_from_to",
        reference: "function affine3d_rot_from_to(from, to) =
    assert(is_vector(from))
    assert(is_vector(to))
    assert(len(from)==len(to))
    let(
        from = unit(point3d(from)),
        to = unit(point3d(to))
    ) approx(from,to)? affine3d_identity() :
    from.z==0 && to.z==0 ?  affine3d_zrot(v_theta(point2d(to)) - v_theta(point2d(from)))
    :
    let(
        u = vector_axis(from,to),
        ang = vector_angle(from,to),
        c = cos(ang),
        c2 = 1-c,
        s = sin(ang)
    ) [
        [u.x*u.x*c2+c    , u.x*u.y*c2-u.z*s, u.x*u.z*c2+u.y*s, 0],
        [u.y*u.x*c2+u.z*s, u.y*u.y*c2+c    , u.y*u.z*c2-u.x*s, 0],
        [u.z*u.x*c2-u.y*s, u.z*u.y*c2+u.x*s, u.z*u.z*c2+c    , 0],
        [               0,                0,                0, 1]
    ];",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[("UP", vectors::bosl_up), ("RIGHT", vectors::bosl_right)],
        deps: &[
            "is_vector",
            "is_finite",
            "is_nan",
            "unit",
            "point3d",
            "point2d",
            "approx",
            "idx",
            "posmod",
            "affine3d_identity",
            "ident",
            "affine3d_zrot",
            "v_theta",
            "vector_axis",
            "v_abs",
            "vector_angle",
            "same_shape",
            "is_def",
            "is_matrix",
            "is_consistent",
            "_list_pattern",
            "constrain",
            "all_nonzero",
        ],
        builtins: &[
            "is_list", "len", "is_undef", "is_num", "norm", "abs", "cross", "atan2", "sin",
            "cos", "acos", "min", "max", "is_bool", "is_string",
        ],
        func: affine::affine3d_rot_from_to,
    },
    // ── O.9 tree 2a (transforms.scad) ────────────────────────────────────────────────────────────────────
    // `apply` — the public transform-application dispatcher over the already-native `_apply`. Its vnf lane's
    // mirror check reaches the determinant chain (4×4 only — the _apply asserts force tdim==datadim==3
    // before determinant runs) and `vnf_reverse_faces` → BOSL2's `reverse` (the builtin shadow) → whose
    // string lane reaches `str_join` (a degenerate-but-is_vnf-passing input can put a string face through
    // it, so the native reproduces that too).
    Entry {
        name: "apply",
        reference: "function apply(transform,points) =
    points==[] ? []
  : is_vector(points) ? _apply(transform, [points])[0]    // point
  : is_vnf(points) ?                                      // vnf
        let(
            newvnf = [_apply(transform, points[0]), points[1]],
            reverse = (len(transform)==len(transform[0])) && determinant(transform)<0
        )
        reverse ? vnf_reverse_faces(newvnf) : newvnf
  : is_list(points) && is_list(points[0]) && is_vector(points[0][0])    // bezier patch
        ? [for (x=points) _apply(transform,x)]
  : _apply(transform,points);",
        consts: &[],
        consts_v: &[],
        deps: &[
            "_apply",
            "is_matrix",
            "is_vector",
            "is_finite",
            "is_nan",
            "is_consistent",
            "_list_pattern",
            "is_2d_transform",
            "is_vnf",
            "determinant",
            "det2",
            "det3",
            "det4",
            "is_def",
            "reverse",
            "vnf_reverse_faces",
            "str_join",
        ],
        builtins: &[
            "is_list", "len", "is_undef", "is_num", "concat", "str", "cross", "is_string",
        ],
        func: affine::apply,
    },
    // ── O.9 tree 2b (transforms/affine.scad) — rot, the band's finale ───────────────────────────────────
    Entry {
        name: "affine3d_translate",
        reference: "function affine3d_translate(v=[0,0,0]) =
    assert(is_list(v))
    let( v = [for (i=[0:2]) default(v[i],0)] )
    [
        [1, 0, 0, v.x],
        [0, 1, 0, v.y],
        [0, 0, 1, v.z],
        [0 ,0, 0,   1]
    ];",
        consts: &[],
        consts_v: &[],
        deps: &["default"],
        builtins: &["is_list", "is_undef"],
        func: affine::affine3d_translate,
    },
    // `approx(ang, 0)` runs on ASSERTED-finite numbers — its list branch (and the idx/posmod knot) is
    // unreachable, but its default eps is `_EPSILON` → the guard. `u=UP` is the reference's own default.
    Entry {
        name: "affine3d_rot_by_axis",
        reference: "function affine3d_rot_by_axis(u=UP, ang=0) =
    assert(is_finite(ang))
    assert(is_vector(u,3))
    approx(ang,0)? affine3d_identity() :
    let(
        u = unit(u),
        c = cos(ang),
        c2 = 1-c,
        s = sin(ang)
    ) [
        [u.x*u.x*c2+c    , u.x*u.y*c2-u.z*s, u.x*u.z*c2+u.y*s, 0],
        [u.y*u.x*c2+u.z*s, u.y*u.y*c2+c    , u.y*u.z*c2-u.x*s, 0],
        [u.z*u.x*c2-u.y*s, u.z*u.y*c2+u.x*s, u.z*u.z*c2+c    , 0],
        [               0,                0,                0, 1]
    ];",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[("UP", vectors::bosl_up)],
        deps: &[
            "is_finite",
            "is_nan",
            "is_vector",
            "approx",
            "unit",
            "affine3d_identity",
            "ident",
        ],
        builtins: &[
            "is_num", "is_list", "len", "is_undef", "norm", "sin", "cos", "is_bool",
        ],
        func: affine::affine3d_rot_by_axis,
    },
    // The band's finale: rot's own body is a dispatcher, but its CLOSURE is the whole affine family — every
    // lane composes already-landed natives (rot_from_to, rot_by_axis, the translate conjugation, the
    // rot_inverse/hstack reachable slice, apply). `_NO_ARG` is the p-sentinel; UP/RIGHT ride in through the
    // composed natives' bakes.
    Entry {
        name: "rot",
        reference: "function rot(a=0, v, cp, from, to, reverse=false, p=_NO_ARG) =
    assert(is_undef(from)==is_undef(to), \"from and to must be specified together.\")
    assert(is_undef(from) || is_vector(from, zero=false), \"'from' must be a non-zero vector.\")
    assert(is_undef(to) || is_vector(to, zero=false), \"'to' must be a non-zero vector.\")
    assert(is_undef(v) || is_vector(v, zero=false), \"'v' must be a non-zero vector.\")
    assert(is_undef(cp) || is_vector(cp), \"'cp' must be a vector.\")
    assert(is_finite(a) || is_vector(a), \"'a' must be a finite scalar or a vector.\")
    assert(is_bool(reverse))
    let(
        m = let(
                from = is_undef(from)? undef : point3d(from),
                to = is_undef(to)? undef : point3d(to),
                cp = is_undef(cp)? undef : point3d(cp),
                m1 = !is_undef(from) ?
                        assert(is_num(a))
                        affine3d_rot_from_to(from,to) * affine3d_rot_by_axis(from,a)
                   : !is_undef(v)?
                        assert(is_num(a))
                        affine3d_rot_by_axis(v,a)
                   : is_num(a) ? affine3d_zrot(a)
                   : affine3d_zrot(a.z) * affine3d_yrot(a.y) * affine3d_xrot(a.x),
                m2 = is_undef(cp)? m1 : (move(cp) * m1 * move(-cp)),
                m3 = reverse? rot_inverse(m2) : m2
            ) m3
    )
    p==_NO_ARG ? m : apply(m, p);",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[
            ("_NO_ARG", no_arg_value),
            ("UP", vectors::bosl_up),
            ("RIGHT", vectors::bosl_right),
        ],
        deps: &[
            "point3d",
            "affine3d_rot_from_to",
            "affine3d_rot_by_axis",
            "affine3d_zrot",
            "affine3d_yrot",
            "affine3d_xrot",
            "affine3d_translate",
            "affine3d_identity",
            "ident",
            "default",
            "move",
            "rot_inverse",
            "hstack",
            "all",
            "_all_bool",
            "is_func",
            "min_length",
            "max_length",
            "determinant",
            "det2",
            "det3",
            "det4",
            "apply",
            "_apply",
            "is_2d_transform",
            "is_vnf",
            "reverse",
            "vnf_reverse_faces",
            "str_join",
            "vector_axis",
            "v_abs",
            "v_theta",
            "point2d",
            "vector_angle",
            "same_shape",
            "is_def",
            "is_matrix",
            "is_consistent",
            "_list_pattern",
            "constrain",
            "unit",
            "approx",
            "idx",
            "posmod",
            "is_vector",
            "all_nonzero",
            "is_finite",
            "is_nan",
        ],
        builtins: &[
            "is_list",
            "len",
            "is_undef",
            "is_num",
            "is_bool",
            "norm",
            "abs",
            "cross",
            "sin",
            "cos",
            "acos",
            "atan2",
            "min",
            "max",
            "is_string",
            "concat",
            "str",
            "version_num",
            "is_function",
        ],
        func: affine::rot,
    },
    // ── O.10c (regions.scad) — the region monster: shoe_holder's ~9.7s/6-call residual ─────────────
    Entry {
        name: "_region_region_intersections",
        reference: "function _region_region_intersections(region1, region2, closed1=true,closed2=true, eps=_EPSILON) =
   let(
       intersections =   [
           for(p1=idx(region1))
              let(
                  path = closed1?list_wrap(region1[p1]):region1[p1]
              )
              for(i = [0:1:len(path)-2])
                  let(
                      a1 = path[i],
                      a2 = path[i+1],
                      nrm = norm(a1-a2)
                  )
                  if( nrm>eps )  // ignore zero-length path edges
                       let( 
                           seg_normal = [-(a2-a1).y, (a2-a1).x]/nrm,
                           ref = a1*seg_normal
                       )
                           // `signs[j]` is the sign of the signed distance from
                           // poly vertex j to the line [a1,a2] where near zero
                           // distances are snapped to zero;  poly edges 
                           //  with equal signs at its vertices cannot intersect
                           // the path edge [a1,a2] or they are collinear and 
                           // further tests can be discarded.
                       for(p2=idx(region2))
                           let(
                               poly  = closed2?list_wrap(region2[p2]):region2[p2],
                               signs = [for(v=poly*seg_normal) abs(v-ref) < eps ? 0 : sign(v-ref) ]
                           ) 
                           if(max(signs)>=0 && min(signs)<=0) // some edge intersects line [a1,a2]
                               for(j=[0:1:len(poly)-2]) 
                                   if(signs[j]!=signs[j+1])
                                        let( // exclude non-crossing and collinear segments
                                            b1 = poly[j],
                                            b2 = poly[j+1],
                                            isect = _general_line_intersection([a1,a2],[b1,b2],eps=eps) 
                                        )
                                        if (isect 
                                            && isect[1]>= -eps 
                                            && isect[1]<= 1+eps 
                                            && isect[2]>= -eps
                                            && isect[2]<= 1+eps)       
                                         [[p1,i,isect[1]], [p2,j,isect[2]]]
         ],
         regions=[region1,region2],
         // Create a flattened index list corresponding to the points in region1 and region2
         // that gives each point as an intersection point
         ptind = [for(i=[0:1])   
                    [for(p=idx(regions[i]))
                       for(j=idx(regions[i][p])) [p,j,0]]],
         points = [for(i=[0:1]) flatten(regions[i])],
         // Corner points are those points where the region touches itself, hence duplicate
         // points in the region's point set
         cornerpts = [for(i=[0:1])
                         [for(k=vector_search(points[i],eps,points[i]))
                             each if (len(k)>1) select(ptind[i],k)]],
         risect = [for(i=[0:1]) concat(column(intersections,i), cornerpts[i])],
         counts = [count(len(region1)), count(len(region2))],
         pathind = [for(i=[0:1]) search(counts[i], risect[i], 0)]
       )
       [for(i=[0:1]) [for(j=counts[i]) _sort_vectors(select(risect[i],pathind[i][j]))]];",
        consts: &[("_EPSILON", 1e-9)],
        consts_v: &[],
        deps: &[
            "idx",
            "list_wrap",
            "are_ends_equal",
            "approx",
            "is_finite",
            "is_nan",
            "posmod",
            "_general_line_intersection",
            "flatten",
            "vector_search",
            "_bt_tree",
            "_bt_search",
            "pointlist_bounds",
            "ident",
            "transpose",
            "is_path",
            "is_matrix",
            "is_vector",
            "is_consistent",
            "_list_pattern",
            "same_shape",
            "in_list",
            "force_list",
            "all_nonzero",
            "is_range",
            "max_index",
            "min_index",
            "mean",
            "sum",
            "_sum",
            "column",
            "is_int",
            "count",
            "select",
            "_sort_vectors",
        ],
        builtins: &[
            "norm", "sign", "cross", "search", "max", "min", "abs", "floor", "round", "concat",
            "len", "is_list", "is_num", "is_undef",
        ],
        func: regions::rri_val,
    },
];

/// BOSL2 `_NO_ARG` (transforms.scad) — the p-not-given sentinel: `[true,[123232345],false]`.
fn no_arg_value() -> Value {
    Value::list(vec![
        Value::Bool(true),
        Value::num_list(vec![123_232_345.0]),
        Value::Bool(false),
    ])
}

/// Is `v` a list to the `is_list` BUILTIN (the branch every shape function turns on)? Both vector variants;
/// nothing else (a string/range iterates in `for` but is NOT a list).
fn v_is_list(v: &Value) -> bool {
    matches!(v, Value::List(_) | Value::NumList(_))
}

/// A raised BOSL2 `assert(…)` — the message is a diagnostic LOCATOR (fast==slow matches "both raised", not
/// text), same contract as [`select_assert`].
fn bosl_assert(msg: &str) -> crate::Error {
    crate::Error::Eval(format!("assert failed: {msg}"))
}

/// Bit-level `Value` equality — the [`Entry::consts_v`] guard's notion of "unchanged": `f64`s compare by
/// `to_bits` (same-bits NaNs are EQUAL, `0.0`/`-0.0` are DISTINCT — the determinism doctrine), lists
/// recurse, and the VARIANT must match exactly (a `NumList` never equals the element-wise-equal `List` —
/// conservative: an unexpected construction declines rather than wires).
pub(super) fn value_bits_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.to_bits() == y.to_bits(),
        (Value::NumList(x), Value::NumList(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(p, q)| p.to_bits() == q.to_bits())
        }
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| value_bits_eq(p, q))
        }
        (
            Value::Range {
                start: s1,
                step: t1,
                end: e1,
            },
            Value::Range {
                start: s2,
                step: t2,
                end: e2,
            },
        ) => {
            s1.to_bits() == s2.to_bits()
                && t1.to_bits() == t2.to_bits()
                && e1.to_bits() == e2.to_bits()
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Undef, Value::Undef) => true,
        _ => false,
    }
}

/// `is_finite` as the BOSL2 user fn computes it (`is_num(x) && !is_nan(0*x)`): a NON-NaN finite number.
fn v_is_finite(v: &Value) -> bool {
    matches!(v, Value::Num(n) if n.is_finite())
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

/// SU.2 (sustainment): every audited `(name, reference-fingerprint, is_pin)` — registry entries first,
/// then the [`PINS`]. The parity matrix walks these against whatever library a program actually loaded;
/// the fingerprints are the SAME cached ones dispatch uses, so the audit can never disagree with the
/// wire gate about what "matched" means. The `_fab_` namespace (the O.1 proof-of-concept trio) is
/// fab-authored — no upstream defines it, so upstream parity doesn't apply and it's excluded.
pub(super) fn matrix_targets() -> impl Iterator<Item = (&'static str, u64, bool)> {
    table()
        .iter()
        .filter(|(_, e)| !e.name.starts_with("_fab_"))
        .map(|(fp, e)| (e.name, *fp, false))
        .chain(pin_table().iter().map(|&(n, fp)| (n, fp, true)))
}

/// Test-only: every MATRIX-AUDITED reference source (entries + pins, `_fab_` POC excluded to mirror
/// [`matrix_targets`]) — the matrix tests assemble a synthetic library at exactly the pinned revision.
#[cfg(test)]
pub(super) fn all_reference_sources() -> Vec<(&'static str, &'static str)> {
    REGISTRY
        .iter()
        .filter(|e| !e.name.starts_with("_fab_"))
        .map(|e| (e.name, e.reference))
        .chain(PINS.iter().copied())
        .collect()
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
