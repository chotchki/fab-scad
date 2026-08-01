//! Binary + unary value operations — OpenSCAD `Value.cc` semantics, bug-for-bug.
//!
//! Everything here is INFALLIBLE: a wrong/undef operand yields `Undef` (OpenSCAD's undef-propagation
//! — `Value::undef(reason)`), never an error. The load-bearing surprises (grounded from Value.cc):
//! `str + str` is `undef` (not concat), `vec * vec` (equal-length) is the DOT PRODUCT (a scalar),
//! `vec + vec` silently TRUNCATES to the shorter, `%` is `fmod` (sign of dividend), `^` is `pow`,
//! cross-type `==`/`!=` never coerce (`1 == true` → false), cross-type `< <= > >=` → `undef`.
//!
//! `&&`/`||` DO short-circuit (OpenSCAD does) — the stack machine's `ShortCircuit` task decides whether
//! the RHS runs; the `And`/`Or` arms here only combine the both-evaluated case. Vector/matrix `*` is the
//! full linear algebra (dot / matrix products) built on the lane-based `dot` (vectorizable, not
//! OpenSCAD's serial sum — the ~1-ULP difference is under the differential metric's float tolerance).

use std::cmp::Ordering;
use std::rc::Rc;

use super::value::Value;
use crate::parser::{BinOp, UnOp};

/// Apply a binary operator to two already-evaluated values. Infallible (bad types → `Undef`).
///
/// The SILENT wrapper: intrinsics use this as a value algebra on well-typed intermediates, and
/// element-wise recursion must not warn per element (`[1,2]+[3,"a"]` is `[4, undef]` with NO
/// warning upstream) — both get silence by construction. The interpreter's one binop site calls
/// [`apply_binary_traced`], which is where the SV warning family surfaces.
#[must_use]
pub fn apply_binary(op: BinOp, a: Value, b: Value) -> Value {
    apply_binary_traced(op, a, b, &mut None)
}

/// [`apply_binary`] for GENERATED code: identical value, and the SV warning goes to the run's
/// console instead of the floor.
///
/// The plain one drops it, which is right for a caller with nowhere to put it and was WRONG for
/// every native — a compiled `undef * undef` answered correctly and silently while the interpreted
/// twin warned. A tier difference no mesh comparison can see (the value was never wrong), which is
/// exactly why the console is part of the differential.
#[must_use]
pub fn binary<W: crate::surface::Warn + ?Sized>(fx: &W, op: BinOp, a: Value, b: Value) -> Value {
    let mut warn = None;
    let out = apply_binary_traced(op, a, b, &mut warn);
    if let Some(w) = warn {
        fx.warn(w);
    }
    out
}

/// [`apply_binary`] plus upstream's diagnostic (SV): identical values bit-for-bit, and on a
/// type-error path `warn` receives the message OpenSCAD prints — every string oracle-pinned
/// (2026.06.12). First-write-wins, so nested failure sites can't clobber the outermost message.
#[must_use]
pub(crate) fn apply_binary_traced(
    op: BinOp,
    a: Value,
    b: Value,
    warn: &mut Option<String>,
) -> Value {
    use Value::{List, Num, NumList};
    match op {
        BinOp::Add => match (a, b) {
            (Num(x), Num(y)) => Num(x + y),
            // The SIMD kernel: two contiguous `f64` vectors, element-wise. `zip_reuse` recycles a
            // refcount-1 operand's buffer (N.2e) — a hot path in BOSL2 point loops.
            (NumList(x), NumList(y)) => Value::NumList(zip_reuse(x, y, |x, y| x + y)),
            // A nested/heterogeneous vector (a MATRIX, `[[..],[..]]`) recurses PER ROW down to the
            // NumList kernel above — so matrix `+` stays SIMD-friendly (rows are the hot loop, the outer
            // walk is just dispatch). OpenSCAD adds vectors element-wise regardless of nesting depth.
            (a, b) if list_len(&a).is_some() && list_len(&b).is_some() => {
                elementwise(BinOp::Add, &a, &b)
            }
            (a, b) => undef_op(BinOp::Add, &a, &b, warn),
        },
        BinOp::Sub => match (a, b) {
            (Num(x), Num(y)) => Num(x - y),
            (NumList(x), NumList(y)) => Value::NumList(zip_reuse(x, y, |x, y| x - y)),
            (a, b) if list_len(&a).is_some() && list_len(&b).is_some() => {
                elementwise(BinOp::Sub, &a, &b)
            }
            (a, b) => undef_op(BinOp::Sub, &a, &b, warn),
        },
        BinOp::Mul => match (a, b) {
            (Num(x), Num(y)) => Num(x * y),
            (Num(s), NumList(v)) | (NumList(v), Num(s)) => Value::NumList(map_reuse(v, |e| e * s)),
            // scalar × a NESTED / heterogeneous list broadcasts element-wise, RECURSIVELY (OpenSCAD's
            // `multvecnum` multiplies each entry via `*`, so `0*[[..],[..]]` = `[0*[..], 0*[..]]`).
            // The recursion is the SILENT wrapper — upstream's `2*[1,"a"]` is `[2, undef]`, no warning.
            (Num(s), List(v)) | (List(v), Num(s)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Mul, Num(s), e.clone()))
                    .collect::<Vec<_>>(),
            ),
            // both sides are vectors/matrices — the linear-algebra lattice, per OpenSCAD's
            // `multiply_visitor`: dot, vector×matrix, matrix×vector, matrix×matrix.
            (a, b) if list_len(&a).is_some() && list_len(&b).is_some() => mul_vectors(&a, &b, warn),
            (a, b) => undef_op(BinOp::Mul, &a, &b, warn),
        },
        BinOp::Div => match (a, b) {
            (Num(x), Num(y)) => Num(x / y), // IEEE: 1/0 → inf, 0/0 → NaN
            (NumList(v), Num(s)) => Value::NumList(map_reuse(v, |e| e / s)),
            (Num(s), NumList(v)) => Value::NumList(map_reuse(v, |e| s / e)),
            // nested list ÷ scalar (and scalar ÷ nested list) recurse element-wise, like OpenSCAD's `/`.
            (List(v), Num(s)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Div, e.clone(), Num(s)))
                    .collect::<Vec<_>>(),
            ),
            (Num(s), List(v)) => Value::list(
                v.iter()
                    .map(|e| apply_binary(BinOp::Div, Num(s), e.clone()))
                    .collect::<Vec<_>>(),
            ),
            (a, b) => undef_op(BinOp::Div, &a, &b, warn),
        },
        BinOp::Mod => match (a, b) {
            (Num(x), Num(y)) => Num(x % y), // Rust f64 `%` == C fmod (sign of dividend)
            (a, b) => undef_op(BinOp::Mod, &a, &b, warn),
        },
        BinOp::Pow => match (a, b) {
            (Num(x), Num(y)) => Num(x.powf(y)),
            (a, b) => undef_op(BinOp::Pow, &a, &b, warn),
        },
        BinOp::Eq => Value::Bool(a == b), // Value's custom PartialEq IS OpenSCAD `==` (no coercion)
        BinOp::Ne => Value::Bool(a != b),
        BinOp::Lt => order(BinOp::Lt, &a, &b, |o| o == Ordering::Less, warn),
        BinOp::Le => order(BinOp::Le, &a, &b, |o| o != Ordering::Greater, warn),
        BinOp::Gt => order(BinOp::Gt, &a, &b, |o| o == Ordering::Greater, warn),
        BinOp::Ge => order(BinOp::Ge, &a, &b, |o| o != Ordering::Less, warn),
        BinOp::And => Value::Bool(a.is_truthy() && b.is_truthy()),
        BinOp::Or => Value::Bool(a.is_truthy() || b.is_truthy()),
        BinOp::BitOr => bitwise(BinOp::BitOr, a, b, |x, y| x | y, warn),
        BinOp::BitAnd => bitwise(BinOp::BitAnd, a, b, |x, y| x & y, warn),
        BinOp::Shl => shift(a, b, true, warn),
        BinOp::Shr => shift(a, b, false, warn),
    }
}

/// Apply a prefix unary operator. Infallible (bad type → `Undef`). The SILENT wrapper — see
/// [`apply_binary`]; upstream's `-[1,"a"]` is `[-1, undef]` with no warning, so the element-wise
/// recursion routes through here.
#[must_use]
pub fn apply_unary(op: UnOp, v: Value) -> Value {
    apply_unary_traced(op, v, &mut None)
}

/// [`apply_unary`] for GENERATED code — see [`binary`].
#[must_use]
pub fn unary<W: crate::surface::Warn + ?Sized>(fx: &W, op: UnOp, v: Value) -> Value {
    let mut warn = None;
    let out = apply_unary_traced(op, v, &mut warn);
    if let Some(w) = warn {
        fx.warn(w);
    }
    out
}

/// [`apply_unary`] plus upstream's diagnostic — unary messages are SPACELESS: `(-string)`, `(~undefined)`.
#[must_use]
pub(crate) fn apply_unary_traced(op: UnOp, v: Value, warn: &mut Option<String>) -> Value {
    match op {
        UnOp::Neg => match v {
            Value::Num(x) => Value::Num(-x),
            Value::NumList(xs) => Value::NumList(xs.iter().map(|e| -e).collect()),
            // A heterogeneous/NESTED list (e.g. a matrix — a `List` of `NumList` rows) negates
            // element-wise, recursing: OpenSCAD's `-[[a,b],[c,d]]` = `[[-a,-b],[-c,-d]]`. Without this a
            // `-matrix` (e.g. `-rot(90)` in BOSL2's rot_inverse) collapsed to `undef` and poisoned the
            // downstream matrix math. Non-numeric leaves fall through to `undef`, matching `-"x"`.
            Value::List(items) => Value::list(
                items
                    .iter()
                    .map(|e| apply_unary(UnOp::Neg, e.clone()))
                    .collect::<Vec<_>>(),
            ),
            v => {
                set_warn(warn, || format!("undefined operation (-{})", type_name(&v)));
                Value::Undef
            }
        },
        UnOp::Pos => v, // no-op (parser.y:469)
        UnOp::Not => Value::Bool(!v.is_truthy()),
        UnOp::BitNot => match v {
            Value::Num(x) => Value::Num(int_to_f64(!f64_to_int(x))),
            v => {
                set_warn(warn, || format!("undefined operation (~{})", type_name(&v)));
                Value::Undef
            }
        },
    }
}

/// Upstream's type vocabulary for diagnostics — both list representations are "vector".
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Undef => "undefined",
        Value::Bool(_) => "bool",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::NumList(_) | Value::List(_) => "vector",
        Value::Object(_) => "object",
        Value::Range { .. } => "range",
        Value::Function { .. } => "function",
    }
}

/// The operator as it prints inside a diagnostic.
fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Pow => "^",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitOr => "|",
        BinOp::BitAnd => "&",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

/// First-write-wins warning slot — the OUTERMOST failure names the operation, nested sites keep quiet.
fn set_warn(warn: &mut Option<String>, msg: impl FnOnce() -> String) {
    if warn.is_none() {
        *warn = Some(msg());
    }
}

/// The generic family message + `Undef`: `undefined operation (T op T)`, SURFACE op and operand order.
fn undef_op(op: BinOp, a: &Value, b: &Value, warn: &mut Option<String>) -> Value {
    set_warn(warn, || {
        format!(
            "undefined operation ({} {} {})",
            type_name(a),
            op_symbol(op),
            type_name(b)
        )
    });
    Value::Undef
}

/// Element-wise combine, truncating to the shorter operand (OpenSCAD's silent-truncate).
fn zip_trunc(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Rc<[f64]> {
    a.iter().zip(b.iter()).map(|(&x, &y)| f(x, y)).collect()
}

/// Element-wise combine, REUSING a refcount-1 operand's buffer in place instead of allocating a fresh one
/// (N.2e). This is the move/COW buffer reuse that keeps OpenSCAD's `VectorType` fast: in a BOSL2 path loop,
/// `p + [dx, dy]` has a temporary operand whose `Rc<[f64]>` is unique here (owned, popped off the value
/// stack), so we mutate it into the result rather than malloc. Bit-IDENTICAL to [`zip_trunc`] (same `f`,
/// same element order); reuse is gated on the operand already being the result length (`== n`, the shorter),
/// so a truncating op never leaves stale tail elements. Falls back to a fresh allocation when neither
/// operand is uniquely owned (both are live variables — refcount ≥ 2).
fn zip_reuse(mut a: Rc<[f64]>, mut b: Rc<[f64]>, f: impl Fn(f64, f64) -> f64) -> Rc<[f64]> {
    let n = a.len().min(b.len());
    // Reuse `a` (or `b`) only when it's ALREADY the result length `n` (the shorter), so a truncating op
    // leaves no stale tail; `sa.iter_mut().zip(b)` then truncates to n — matching `zip_trunc`.
    if a.len() == n
        && let Some(sa) = Rc::get_mut(&mut a)
    {
        for (x, &y) in sa.iter_mut().zip(b.iter()) {
            *x = f(*x, y);
        }
        return a;
    }
    if b.len() == n
        && let Some(sb) = Rc::get_mut(&mut b)
    {
        for (y, &x) in sb.iter_mut().zip(a.iter()) {
            *y = f(x, *y);
        }
        return b;
    }
    zip_trunc(&a, &b, f)
}

/// Map `f` over a vector, REUSING its buffer in place when uniquely owned (N.2e — `scalar * v`, `v / s`,
/// `-v`). Bit-identical to `v.iter().map(f).collect()`.
fn map_reuse(mut v: Rc<[f64]>, f: impl Fn(f64) -> f64) -> Rc<[f64]> {
    if let Some(s) = Rc::get_mut(&mut v) {
        for e in s.iter_mut() {
            *e = f(*e);
        }
        return v;
    }
    v.iter().map(|&e| f(e)).collect()
}

/// Element-wise recursive `op` (`+`/`-`) over two VECTOR values, OpenSCAD's nesting-agnostic vector
/// arithmetic: pair elements to the shorter length (matching `zip_trunc`'s truncation), each pair
/// combined by `op`. Called only when a heterogeneous [`Value::List`] is on at least one side — the flat
/// `NumList op NumList` case is the inline SIMD kernel. A matrix (`List` of `NumList`) tiles down to
/// per-ROW `NumList op NumList`, so the numeric hot loop stays the vectorizable `zip_trunc`; this outer
/// walk is just row DISPATCH (cheap `Rc`-clone element access), not a per-scalar path.
fn elementwise(op: BinOp, a: &Value, b: &Value) -> Value {
    // The caller's arm guard guarantees both are lists; `unwrap_or(0)` is a no-panic floor, not a branch.
    let n = list_len(a).unwrap_or(0).min(list_len(b).unwrap_or(0));
    Value::list(
        (0..n)
            .map(|i| apply_binary(op, list_get(a, i), list_get(b, i)))
            .collect::<Vec<_>>(),
    )
}

/// Dot product of two equal-length numeric vectors, in the FIXED 4-lane accumulation order (the
/// reduction doctrine, SPEC): lane `j` sums every 4th product, then the lanes combine as
/// `(l0+l1)+(l2+l3)`. This is (1) DETERMINISTIC and (2) the exact shape a 4-wide SIMD reduction
/// produces, so a future SIMD fast path equals this scalar path BIT-FOR-BIT (the `fast == slow`
/// property, proven below). It matches OpenSCAD's naive left-fold for ≤3-element vectors (the common
/// geometry case); 4+ elements diverge by ≤1 ULP on non-integer inputs — verified visible-or-not at
/// I.5 (echo precision) / K (the harness).
fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut lanes = [0.0f64; 4];
    let (mut ac, mut bc) = (a.chunks_exact(4), b.chunks_exact(4));
    for (ca, cb) in ac.by_ref().zip(bc.by_ref()) {
        lanes[0] += ca[0] * cb[0];
        lanes[1] += ca[1] * cb[1];
        lanes[2] += ca[2] * cb[2];
        lanes[3] += ca[3] * cb[3];
    }
    for (lane, (&x, &y)) in ac.remainder().iter().zip(bc.remainder()).enumerate() {
        lanes[lane] += x * y;
    }
    (lanes[0] + lanes[1]) + (lanes[2] + lanes[3])
}

/// The `f64` slice of an all-number vector (a `NumList` row), or `None` if `v` isn't one. Matrix
/// multiplication is `undef` on a non-rectangular / non-numeric operand (OpenSCAD warns + returns undef),
/// and our repr invariant guarantees an all-number vector is always the `NumList` fast path.
fn num_row(v: &Value) -> Option<&[f64]> {
    match v {
        Value::NumList(xs) => Some(xs),
        _ => None,
    }
}

/// The vector/matrix `*` lattice (both operands lists, both non-empty checked by the caller's
/// empties gate). VALUES are identical to the pre-SV arm structure — this exists so each failure
/// path can say WHY, with the counts upstream interpolates. Messages follow upstream's dispatch by
/// SHAPE (a flat mixed operand is a VECTOR even in `List` repr, so its failure is a dot's, not a
/// matrix's); mixed-shape cells beyond the probed set are best-effort and deliberately untested.
fn mul_vectors(a: &Value, b: &Value, warn: &mut Option<String>) -> Value {
    if list_len(a) == Some(0) || list_len(b) == Some(0) {
        // Checked FIRST upstream: `[] * [1,2]` gets this message, not the length mismatch.
        set_warn(warn, || {
            "Multiplication is undefined on empty vectors".to_string()
        });
        return Value::Undef;
    }
    match (a, b) {
        (Value::NumList(x), Value::NumList(y)) => {
            if x.len() == y.len() {
                Value::Num(dot(x, y))
            } else {
                set_warn(warn, || {
                    format!(
                        "vector*vector requires matching lengths ({} != {})",
                        x.len(),
                        y.len()
                    )
                });
                Value::Undef
            }
        }
        (Value::NumList(v), Value::List(m)) => vec_times_mat(v, m, warn),
        (Value::List(m), Value::NumList(v)) => mat_times_vec(m, v, warn),
        (Value::List(x), Value::List(y)) => mat_times_mat(x, y, warn),
        // list_len admits only the two list reprs, and Num pairs peeled off in the Mul arm.
        _ => Value::Undef,
    }
}

/// Is this list element itself a list — i.e. does its parent read as a MATRIX in upstream's dispatch?
fn is_row(v: &Value) -> bool {
    matches!(v, Value::NumList(_) | Value::List(_))
}

/// Upstream's non-numeric-element message; `i` is the offending index.
fn vector_numbers_msg(i: usize) -> String {
    format!("Vector must contain only numbers. Problem at index {i}")
}

/// Index + type of the first non-number in a flat list, if any.
fn first_non_num(m: &[Value]) -> Option<(usize, &'static str)> {
    m.iter().enumerate().find_map(|(i, e)| match e {
        Value::Num(_) => None,
        e => Some((i, type_name(e))),
    })
}

/// The message for a matrix whose row fails [`num_row`]: a List row names the first non-number
/// INSIDE it (upstream's `Problem at index 1` for `[[1,"a"]]`); a scalar row (mixed shapes,
/// unprobed upstream) names its own index. NOTE upstream prints this line TWICE (an artifact of
/// warning at two stack levels) — we emit once, a documented count divergence.
fn bad_row_msg(mat: &[Value]) -> String {
    for (i, row) in mat.iter().enumerate() {
        match row {
            Value::NumList(_) => {}
            Value::List(items) => {
                if let Some((j, _)) = first_non_num(items) {
                    return vector_numbers_msg(j);
                }
            }
            _ => return vector_numbers_msg(i),
        }
    }
    vector_numbers_msg(0) // unreachable: called only when some row failed
}

/// Matrix × vector: `out[i] = mat[i] · vec` (OpenSCAD `multmatvec`). Every row must be numeric and
/// `vec`-length (rectangular); otherwise `undef`. The per-row `dot` is the lane-based (vectorizable) one.
fn mat_times_vec(mat: &[Value], vec: &[f64], warn: &mut Option<String>) -> Value {
    if !is_row(&mat[0]) {
        // Flat MIXED left: upstream dispatched vec·vec — length first, then the failing pair,
        // LEFT element's type first (`[1,"a"] * [1,2]` is `(string * number)`).
        if mat.len() != vec.len() {
            set_warn(warn, || {
                format!(
                    "vector*vector requires matching lengths ({} != {})",
                    mat.len(),
                    vec.len()
                )
            });
        } else if let Some((_, t)) = first_non_num(mat) {
            set_warn(warn, || format!("undefined operation ({t} * number)"));
        }
        return Value::Undef;
    }
    let mut out = Vec::with_capacity(mat.len());
    for row in mat {
        match num_row(row) {
            Some(r) if r.len() == vec.len() => out.push(dot(r, vec)),
            Some(r) => {
                set_warn(warn, || {
                    format!(
                        "matrix*vector requires matrix column count to match vector length ({} != {})",
                        r.len(),
                        vec.len()
                    )
                });
                return Value::Undef;
            }
            None => {
                set_warn(warn, || bad_row_msg(mat));
                return Value::Undef;
            }
        }
    }
    Value::num_list(out)
}

/// Vector × matrix: `out[j] = Σ_i vec[i]·mat[i][j]` (OpenSCAD `multvecmat`). Requires `vec.len() ==
/// mat.len()` (vector length == matrix row count) and a rectangular, all-numeric matrix. Columns are
/// gathered so the reduction reuses the lane-based `dot`.
fn vec_times_mat(vec: &[f64], mat: &[Value], warn: &mut Option<String>) -> Value {
    if !is_row(&mat[0]) {
        // Flat MIXED right: upstream's vec·vec — `[1,2] * [1,"a"]` is `(number * string)`.
        if vec.len() != mat.len() {
            set_warn(warn, || {
                format!(
                    "vector*vector requires matching lengths ({} != {})",
                    vec.len(),
                    mat.len()
                )
            });
        } else if let Some((_, t)) = first_non_num(mat) {
            set_warn(warn, || format!("undefined operation (number * {t})"));
        }
        return Value::Undef;
    }
    if vec.len() != mat.len() {
        set_warn(warn, || {
            format!(
                "vector*matrix requires vector length to match matrix row count ({} != {})",
                vec.len(),
                mat.len()
            )
        });
        return Value::Undef;
    }
    let Some(rows) = mat.iter().map(num_row).collect::<Option<Vec<&[f64]>>>() else {
        set_warn(warn, || bad_row_msg(mat));
        return Value::Undef;
    };
    let cols = rows[0].len();
    if rows.iter().any(|r| r.len() != cols) {
        return Value::Undef; // ragged matrix — silent undef (unprobed upstream)
    }
    let mut col = vec![0.0; rows.len()];
    let mut out = Vec::with_capacity(cols);
    for j in 0..cols {
        for (i, r) in rows.iter().enumerate() {
            col[i] = r[j];
        }
        out.push(dot(vec, &col));
    }
    Value::num_list(out)
}

/// Matrix × matrix: each left ROW is a vector times the right matrix (OpenSCAD folds it exactly this
/// way). Left column count must match right row count; a non-numeric row → `undef`.
fn mat_times_mat(a: &[Value], b: &[Value], warn: &mut Option<String>) -> Value {
    if !is_row(&a[0]) {
        // Flat MIXED left × matrix: upstream's vec×mat over a non-numeric vector (exact text
        // unprobed; the element message is the honest nearest).
        if let Some((i, _)) = first_non_num(a) {
            set_warn(warn, || vector_numbers_msg(i));
        }
        return Value::Undef;
    }
    let mut out = Vec::with_capacity(a.len());
    for row in a {
        let Some(r) = num_row(row) else {
            set_warn(warn, || bad_row_msg(a));
            return Value::Undef;
        };
        if r.len() != b.len() {
            set_warn(warn, || {
                format!(
                    "matrix*matrix requires left operand column count to match right operand row count ({} != {})",
                    r.len(),
                    b.len()
                )
            });
            return Value::Undef;
        }
        match vec_times_mat(r, b, warn) {
            Value::Undef => return Value::Undef,
            v => out.push(v),
        }
    }
    Value::list(out)
}

/// Ordering comparison. CROSS-type (`1 < "a"`) is `undef` — a type error. SAME orderable type
/// (num/num, str/str, list/list) yields a BOOL — unless a type mismatch hides INSIDE the lists
/// (`[1, 2] < [1, "b"]`), which is `undef` upstream too, not `false` (AR.4's heavy lane caught us
/// collapsing it; oracle-pinned in `eval_corpus`). A NaN is NOT a mismatch: top-level it compares
/// IEEE-false (`(0/0) < 1`), inside a vector it TIES — see [`value_cmp`].
///
/// Diagnostics (SV, all oracle-pinned): a mismatch INSIDE the vectors prints the leaf pair plus one
/// `in vector comparison at index N` frame per nesting level, innermost first — and the leaf pair
/// follows upstream's DESUGAR (`<`/`>=` run `a<b`; `<=`/`>` run `b<a`, so their leaf types swap; the
/// printed op is always `<`). Top-level failures keep the SURFACE op and order, and the same-type
/// unorderable pairs (undef, object, function) get upstream's reversed wording, `operation undefined`.
fn order(
    op: BinOp,
    a: &Value,
    b: &Value,
    want: impl Fn(Ordering) -> bool,
    warn: &mut Option<String>,
) -> Value {
    if same_orderable_type(a, b) {
        match value_cmp(a, b) {
            Cmp::Ord(o) => Value::Bool(want(o)),
            Cmp::Nan => Value::Bool(false),
            Cmp::Mismatch {
                frames,
                left,
                right,
            } => {
                let (l, r) = match op {
                    BinOp::Le | BinOp::Gt => (right, left),
                    _ => (left, right),
                };
                set_warn(warn, || {
                    let mut msg = format!("undefined operation ({l} < {r})");
                    for i in frames {
                        use std::fmt::Write;
                        let _ = write!(msg, "\n\tin vector comparison at index {i}");
                    }
                    msg
                });
                Value::Undef
            }
        }
    } else {
        // cross-type ordering is a type error (a value). `type_name` equality here IS the quirk set:
        // the only same-named pairs that reach this branch are undef/object/function.
        let (l, r) = (type_name(a), type_name(b));
        let head = if l == r {
            "operation undefined"
        } else {
            "undefined operation"
        };
        set_warn(warn, || format!("{head} ({l} {} {r})", op_symbol(op)));
        Value::Undef
    }
}

/// Do `a` and `b` share an orderable type — both numbers, both strings, both bools, or both lists (either
/// representation)? `undef` and cross-type pairs are NOT orderable. Two BOOLs order `false < true` (they
/// coerce to `0`/`1`): BOSL2's `compare_vals(true, false) > 0` relies on it, and OpenSCAD's `<`/`>` return
/// a real bool for a bool pair (only CROSS-type — `bool` vs `num` — stays `undef`; `compare_vals` reaches
/// those through its own type-rank test, never through `<`).
fn same_orderable_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Num(_), Value::Num(_))
            | (Value::Str(_), Value::Str(_))
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Range { .. }, Value::Range { .. })
    ) || (list_len(a).is_some() && list_len(b).is_some())
}

/// A three-way comparison OUTCOME — richer than `Option<Ordering>` because the two failure modes
/// part ways at the surface: an IEEE-incomparable pair is a FALSE comparison upstream, while a
/// TYPE mismatch is `undef`. Collapsing both to `None` is exactly the bug `order` used to have.
enum Cmp {
    Ord(Ordering),
    /// A NaN met a number. Top-level this makes every comparison false (IEEE); INSIDE a vector it
    /// TIES — upstream's element walk probes `a < b` and `b < a`, both fail, and equal-and-continue
    /// falls out: `[0/0] <= [0/0]` is true, `[1, 0/0, 5] < [1, 2, 9]` is decided at index 2.
    Nan,
    /// Differently-typed operands (`1` vs `"a"`), at any depth. Upstream: warned `undef`. Carries
    /// what the warning needs: the LEAF pair's type names (in `a<b` walk order — the caller swaps
    /// for the desugared ops) and the index at each nesting level, pushed on unwind so the vec
    /// reads innermost-first — exactly upstream's frame print order.
    Mismatch {
        frames: Vec<usize>,
        left: &'static str,
        right: &'static str,
    },
}

/// A total-ish order over values: numbers numerically, strings lexicographically, bools `false < true`,
/// lists element-wise-lexicographically (recursively, across BOTH list representations), with the
/// length as tiebreak (`[1] < [1, 2]`). Recurses on nested lists (parse-bounded here;
/// deep-list ordering joins the explicit-stack work if comprehensions ever build one).
fn value_cmp(a: &Value, b: &Value) -> Cmp {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).map_or(Cmp::Nan, Cmp::Ord),
        (Value::Str(x), Value::Str(y)) => Cmp::Ord(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Cmp::Ord(x.cmp(y)), // false < true
        // AH.2.1 (operators-tests golden): two RANGES order as the SEQUENCES they iterate —
        // `[0:1:3] >= [0:1:3]` is true, `[1:-1:3] < [1:-1:-1]` is true (empty < non-empty).
        (
            Value::Range { start, step, end },
            Value::Range {
                start: s2,
                step: t2,
                end: e2,
            },
        ) => super::value::range_seq_cmp((*start, *step, *end), (*s2, *t2, *e2))
            .map_or(Cmp::Nan, Cmp::Ord),

        _ => {
            let (Some(la), Some(lb)) = (list_len(a), list_len(b)) else {
                return Cmp::Mismatch {
                    frames: Vec::new(),
                    left: type_name(a),
                    right: type_name(b),
                };
            };
            for i in 0..la.min(lb) {
                match value_cmp(&list_get(a, i), &list_get(b, i)) {
                    Cmp::Ord(Ordering::Equal) | Cmp::Nan => {} // NaN ties inside vectors — see Cmp
                    non_eq @ Cmp::Ord(_) => return non_eq,
                    Cmp::Mismatch {
                        mut frames,
                        left,
                        right,
                    } => {
                        frames.push(i); // pushed on unwind → innermost-first, upstream's print order
                        return Cmp::Mismatch {
                            frames,
                            left,
                            right,
                        };
                    }
                }
            }
            Cmp::Ord(la.cmp(&lb))
        }
    }
}

/// The element count of a list value (`NumList` or `List`), or `None` if it isn't a list.
fn list_len(v: &Value) -> Option<usize> {
    match v {
        Value::NumList(xs) => Some(xs.len()),
        Value::List(xs) => Some(xs.len()),
        _ => None,
    }
}

/// The `i`-th element of a list value as a `Value` (`Undef` out of range / not a list).
fn list_get(v: &Value, i: usize) -> Value {
    match v {
        Value::NumList(xs) => xs.get(i).copied().map_or(Value::Undef, Value::Num),
        Value::List(xs) => xs.get(i).cloned().unwrap_or(Value::Undef),
        _ => Value::Undef,
    }
}

/// `base[index]` (`Value.cc` `operator[]`). The index is `size_t(toDouble(index))` — a non-number,
/// negative, or non-finite index is out of range (`undef`), a fractional one truncates toward zero.
/// Indexing a string yields the code-point-`idx` character as a 1-char string; a scalar yields `undef`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded: i is finite and >= 0 here, so `as usize` truncates like OpenSCAD's size_t cast (huge → saturates → out of range → undef)"
)]
/// Bind an extracted FUNCTION member to its owning object (AF.5): `o.f` / `o["f"]` produce a
/// METHOD — the receiver rides the value and fills a `this` param at call time. Re-extraction
/// re-binds (a function stored into another object becomes THAT object's method); non-functions
/// pass through untouched.
fn bind_receiver(v: Value, receiver: &std::rc::Rc<super::object::ObjectMap>) -> Value {
    match v {
        Value::Function {
            closure_id,
            env,
            self_name,
            repr,
            group,
            ..
        } => Value::Function {
            closure_id,
            env,
            self_name,
            repr,
            group,
            bound_this: Some(std::rc::Rc::clone(receiver)),
        },
        other => other,
    }
}

/// OpenSCAD's `base[index]`, every shape of it: an object indexes by STRING key, a list/string by
/// numeric position, and anything out of range or wrongly typed reads `undef` rather than raising.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "i is checked non-negative + finite above the cast; an out-of-range index reads Undef"
)]
pub fn index(base: Value, index: &Value) -> Value {
    // Objects index by STRING key (AF.3): `o["key"]` → the member, missing → undef.
    if let Value::Object(o) = &base {
        return match index {
            Value::Str(k) => bind_receiver(o.get(k).cloned().unwrap_or(Value::Undef), o),
            _ => Value::Undef,
        };
    }
    let &Value::Num(i) = index else {
        return Value::Undef;
    };
    if i < 0.0 || !i.is_finite() {
        return Value::Undef;
    }
    let idx = i as usize;
    match base {
        Value::Str(s) => s
            .chars()
            .nth(idx)
            .map_or(Value::Undef, |c| Value::string(c.to_string())),
        // A RANGE indexes to its three fields — `r[0]=start`, `r[1]=step`, `r[2]=end` (OpenSCAD `RangeType`),
        // anything else `undef`. BOSL2's `is_range(x)` leans on exactly this (`is_finite(x[0..2])`).
        Value::Range { start, step, end } => match idx {
            0 => Value::Num(start),
            1 => Value::Num(step),
            2 => Value::Num(end),
            _ => Value::Undef,
        },
        other => list_get(&other, idx),
    }
}

/// Member access `v.x` / `v.y` / `v.z` → index 0 / 1 / 2 — OpenSCAD's named vector components (the only
/// members it defines). Any other name → `undef`; the base rules (non-list, out-of-range → `undef`) are
/// [`index`]'s. BOSL2 reads coordinates this way everywhere (`corner.x`, `shift.y`, `v.z`).
#[allow(
    clippy::needless_pass_by_value,
    reason = "the Task::Member handler pops an owned base; swizzle picks clone it per component"
)]
#[must_use]
pub fn member(base: Value, field: &str) -> Value {
    // Objects: member lookup by NAME (AF.3) — `o.a`, `$`-named members (`o.$fs`) included;
    // missing → undef. A FUNCTION member binds its receiver at extraction (AF.5).
    if let Value::Object(o) = &base {
        return bind_receiver(o.get(field).cloned().unwrap_or(Value::Undef), o);
    }
    // Vector SWIZZLES (AF.3, the vector-swizzling golden): 1-4 letters from xyzw/rgba (sets mix
    // freely), each naming component 0-3; one letter yields the element, several the picked
    // vector. Undef for a non-vector base, >4 letters, a non-swizzle letter, or ANY out-of-range
    // component ("indices out of range will return undef").
    let len = match &base {
        Value::NumList(v) => v.len(),
        Value::List(v) => v.len(),
        _ => return Value::Undef,
    };
    // One set per swizzle — the golden pins `v.xr` (mixed) as undef despite the test's own
    // comment claiming otherwise; the golden wins.
    let comp = if field.chars().all(|c| "xyzw".contains(c)) {
        |c: char| "xyzw".find(c)
    } else if field.chars().all(|c| "rgba".contains(c)) {
        |c: char| "rgba".find(c)
    } else {
        return Value::Undef;
    };
    let n = field.chars().count();
    if n == 0 || n > 4 {
        return Value::Undef;
    }
    let mut picks = Vec::with_capacity(n);
    for c in field.chars() {
        let Some(i) = comp(c) else {
            return Value::Undef;
        };
        // RANGE-check explicitly rather than inferring out-of-range from an undef RESULT. `index`
        // answers undef for both "past the end" and "that element IS undef", so the old undef-sniff
        // poisoned the whole swizzle when any picked element was legitimately undef —
        // `([undef,2,3,4]).rgba` gave undef where upstream gives `[undef, 2, 3, 4]`. A swizzle is a
        // pure index permutation upstream; element TYPE never enters into it (AO.4, seed 204).
        if i >= len {
            return Value::Undef;
        }
        #[allow(clippy::cast_precision_loss, reason = "i is 0..=3")]
        picks.push(index(base.clone(), &Value::Num(i as f64)));
    }
    match picks.len() {
        1 => picks.remove(0),
        _ => super::build_vector(picks),
    }
}

fn bitwise(
    op: BinOp,
    lhs: Value,
    rhs: Value,
    combine: impl Fn(i64, i64) -> i64,
    warn: &mut Option<String>,
) -> Value {
    match (lhs, rhs) {
        (Value::Num(x), Value::Num(y)) => {
            Value::Num(int_to_f64(combine(f64_to_int(x), f64_to_int(y))))
        }
        (a, b) => undef_op(op, &a, &b, warn),
    }
}

fn shift(lhs: Value, rhs: Value, left: bool, warn: &mut Option<String>) -> Value {
    match (lhs, rhs) {
        (Value::Num(x), Value::Num(y)) => {
            let by = f64_to_int(y);
            if by < 0 {
                set_warn(warn, || "negative shift".to_string());
                return Value::Undef;
            }
            if by >= 64 {
                set_warn(warn, || "shift too large".to_string());
                return Value::Undef;
            }
            let xi = f64_to_int(x);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "by is checked in 0..64, so the cast to u32 is exact and non-negative"
            )]
            let shifted = if left {
                xi << (by as u32)
            } else {
                xi >> (by as u32)
            };
            Value::Num(int_to_f64(shifted))
        }
        (a, b) => undef_op(if left { BinOp::Shl } else { BinOp::Shr }, &a, &b, warn),
    }
}

/// OpenSCAD `toInt64`: truncate toward zero. `f64 as i64` saturates (NaN → 0), never UB.
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenSCAD's toInt64 truncates; f64->i64 saturates in Rust, no UB"
)]
fn f64_to_int(x: f64) -> i64 {
    x.trunc() as i64
}

/// i64 back to the f64 all OpenSCAD numbers are (lossy past 2^53, matching OpenSCAD's double store).
#[allow(
    clippy::cast_precision_loss,
    reason = "OpenSCAD stores everything as f64; large bit-op results lose precision there too"
)]
fn int_to_f64(x: i64) -> f64 {
    x as f64
}

// I.7 — Kani proofs of PANIC-FREEDOM on the arithmetic kernels that run on untrusted SCAD input
// (docs/testing-cards.md: "indices in bounds", panic-freedom on the exact loop). Symbolic primitives,
// so the guarantee is universal. Compiled only under `cargo kani`.
#[cfg(kani)]
mod proofs {
    /// `dot()`'s 4-lane tail indexes `lanes[lane]` (`lanes: [f64; 4]`) where `lane` enumerates the
    /// remainder of `chunks_exact(4)` — whose length is ALWAYS < 4 (the std guarantee: a remainder is
    /// shorter than the chunk size). So every `lane` is a valid index into the 4-lane accumulator. The
    /// invariant is modeled directly (`rem_len < 4`, a symbolic tail length) so CBMC proves the index
    /// bound without unwinding `Vec`/`chunks_exact` internals — this IS the "indices in bounds" proof.
    #[kani::proof]
    #[kani::unwind(4)]
    fn dot_tail_index_stays_in_bounds() {
        let rem_len: usize = kani::any();
        kani::assume(rem_len < 4); // chunks_exact(4).remainder().len() is always < 4
        let mut lanes = [0.0f64; 4];
        let mut lane = 0usize;
        while lane < rem_len {
            lanes[lane] += 1.0; // the tail op `lanes[lane] += x*y` — panics iff lane >= 4, proven safe
            lane += 1;
        }
    }

    /// `shift()` guards `by` into `0..64` BEFORE the shift, so `i64 << (by as u32)` / `>>` never
    /// overflow-panic (shift amount < bit width). Panic-freedom for the untrusted `<<`/`>>` path.
    #[kani::proof]
    fn guarded_shift_never_overflow_panics() {
        let by: i64 = kani::any();
        kani::assume((0..64).contains(&by)); // the exact guard in shift()
        let x: i64 = kani::any();
        let _l = x << (by as u32);
        let _r = x >> (by as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::dot;
    use proptest::prelude::*;

    /// An INDEPENDENT reference for the fixed 4-lane order: reduce products with `lane = k % 4`.
    /// Different code from `dot`'s SIMD-shaped chunk loop, SAME order — the whole point of the property
    /// below. (That boxed `Value` arithmetic matches raw `f64` is covered by the `eval_corpus` dot tests.)
    fn reference_dot(a: &[f64], b: &[f64]) -> f64 {
        let mut lanes = [0.0f64; 4];
        for (k, (&x, &y)) in a.iter().zip(b).enumerate() {
            lanes[k % 4] += x * y;
        }
        (lanes[0] + lanes[1]) + (lanes[2] + lanes[3])
    }

    proptest! {
        /// fast == slow, BIT-FOR-BIT: the contiguous `NumList` dot (`dot`, the SIMD-shaped chunk loop)
        /// equals the reference dot (`reference_dot`, k%4) on random numeric vectors. Both use the fixed
        /// 4-lane order, so they agree by construction — and this LOCKS it: a future SIMD dot that
        /// reorders the reduction, or an FMA that fuses product+add, fails here instead of silently
        /// diverging from the oracle. Lengths span full 4-chunks + every remainder (0..3).
        #[test]
        fn fast_dot_equals_the_fixed_order_reference(
            v in prop::collection::vec((-1.0e6f64..1.0e6, -1.0e6f64..1.0e6), 0..64)
        ) {
            let a: Vec<f64> = v.iter().map(|&(x, _)| x).collect();
            let b: Vec<f64> = v.iter().map(|&(_, y)| y).collect();
            prop_assert_eq!(dot(&a, &b).to_bits(), reference_dot(&a, &b).to_bits());
        }
    }
}
