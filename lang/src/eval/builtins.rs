//! OpenSCAD builtin FUNCTIONS (`func.cc`), applied to already-evaluated arguments.
//!
//! A builtin is a leaf operation: its arguments evaluate on the explicit stack, then this dispatches
//! by name. Ill-typed / missing args yield `undef` (OpenSCAD's undef-propagation), never an error.
//! Trig is in DEGREES and reuses `trig`'s exact-quadrant `sin`/`cos` so `sin(30)` etc. match the
//! geometry path bit-for-bit. `rands` (non-deterministic) is deliberately NOT here — it needs the
//! seeded-RNG discipline (I.4.3). Names here MUST match [`is_builtin`].
//!
//! The list/string group (I.4.2) is the glue BOSL2 lives on: `len`/`concat`/`reverse` are vector
//! surgery, `chr`/`ord` bridge codepoints↔strings, `str` routes through the shared [`fmt`](super::fmt)
//! formatter (so echo at I.5 refines ONE place), and `lookup`/`search` are the table primitives —
//! `lookup` linear-interpolates + clamps at the ends, `search` follows `func.cc`'s per-match protocol
//! (`num_returns_per_match`: 1 = flat first-hits, 0 = all, n = up to n; `index_col_num` picks a column).
//!
//! Type predicates (I.4.3) are trivial variant tests. `version`/`version_num` report a PINNED constant
//! (last stable `2021.01`), NOT the host build — the oracle is nightly (a build-date version), but the
//! determinism doctrine forbids env-derived values, so we pin a release that clears BOSL2's minimum and
//! bucket the oracle's build-date `version()` as a known K divergence. `rands` is a DELIBERATE loud
//! defer (kept out of [`is_builtin`], so a call hits the unimplemented-builtin error): seedless it is
//! non-deterministic (banned), and seeded it would have to replicate boost's `mt19937` +
//! `uniform_real_distribution` bit-for-bit — a K divergence-bucket decision, not this leaf.

use super::fmt::format_value;
use super::trig;
use super::value::Value;
use super::{build_vector, iter_values_raw};

/// Is `name` a builtin we implement? Checked at a call site AFTER user functions, BEFORE "unknown"
/// (so a user function may shadow a builtin, per OpenSCAD).
pub(super) fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "sign"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "floor"
            | "ceil"
            | "round"
            | "ln"
            | "log"
            | "exp"
            | "pow"
            | "sqrt"
            | "min"
            | "max"
            | "norm"
            | "cross"
            // list + string (I.4.2)
            | "len"
            | "concat"
            | "str"
            | "chr"
            | "ord"
            | "reverse"
            | "lookup"
            | "search"
            | "rands"
            // type predicates + version (I.4.3)
            | "is_undef"
            | "is_bool"
            | "is_num"
            | "is_string"
            | "is_list"
            | "is_function"
            | "version"
            | "version_num"
            // module-instantiation stack introspection (control.cc) — stateful, routed via run_builtin
            | "parent_module"
    )
}

/// `parent_module(n)` (`control.cc`) — the NAME of the module `n` levels up the instantiation stack (0 =
/// the current module, 1 = its parent), or `undef` if `n` overruns the stack. `stack` is innermost-last
/// (the current module at the end), so index `len-1-n`. A non-integer / negative `n` → `undef`. Stateful
/// (reads the evaluator's module stack), so it's dispatched from [`run_builtin`](super::run_builtin), not
/// the pure `apply`. BOSL2's `deprecate()` echoes `parent_module(1)` to name the deprecated module.
pub(super) fn parent_module(pos: &[Value], stack: &[&str]) -> Value {
    let n = match pos.first() {
        None => 0,
        Some(v) => match as_index(v) {
            Some(n) => n,
            None => return Value::Undef,
        },
    };
    match stack.len().checked_sub(1 + n).and_then(|i| stack.get(i)) {
        Some(name) => Value::string((*name).to_string()),
        None => Value::Undef,
    }
}

/// Apply a builtin by name to its args. OpenSCAD builtins have no declared parameter names, so `pos` is
/// the WHOLE argument list in source order (a named arg's name is dropped upstream in [`run_builtin`]);
/// e.g. `search`'s `num_returns_per_match`/`index_col_num` are just positions 2 and 3 here.
pub(super) fn apply(name: &str, pos: &[Value]) -> Value {
    match name {
        "abs" => num1(pos, f64::abs),
        "sign" => num1(pos, sign),
        "sin" => num1(pos, trig::sin_degrees),
        "cos" => num1(pos, trig::cos_degrees),
        "tan" => num1(pos, trig::tan_degrees),
        // asin/acos snap to EXACT degrees at the nice sines/cosines (glibc's correctly-rounded value),
        // else libm — so `acos(-0.5)` is exactly 120, not `120.0000…01` (BOSL2's exact-`==` f_acos). atan/
        // atan2 stay on `to_degrees` (their nice angles are already exact here, and no test needs a snap).
        "asin" => num1(pos, trig::asin_degrees),
        "acos" => num1(pos, trig::acos_degrees),
        "atan" => num1(pos, |x| x.atan().to_degrees()),
        "atan2" => num2(pos, |y, x| y.atan2(x).to_degrees()),
        "floor" => num1(pos, f64::floor),
        "ceil" => num1(pos, f64::ceil),
        "round" => num1(pos, f64::round), // half AWAY from zero — same as OpenSCAD
        "ln" => num1(pos, f64::ln),
        "log" => num1(pos, f64::log10), // OpenSCAD `log` is base 10
        "exp" => num1(pos, f64::exp),
        "pow" => num2(pos, f64::powf),
        "sqrt" => num1(pos, f64::sqrt),
        "min" => min_max(pos, true),
        "max" => min_max(pos, false),
        "norm" => norm(pos),
        "cross" => cross(pos),
        "len" => len(pos),
        "concat" => concat(pos),
        "str" => str_concat(pos),
        "chr" => chr(pos),
        "ord" => ord(pos),
        "reverse" => reverse(pos),
        "lookup" => lookup(pos),
        "search" => search(pos),
        // `rands` is intercepted by `run_builtin` (it needs the evaluator's advancing RandStream for the
        // seedless case) and never reaches this pure dispatch.
        "is_undef" => pred(pos, |v| matches!(v, Value::Undef)),
        "is_bool" => pred(pos, |v| matches!(v, Value::Bool(_))),
        // A NaN is NOT `is_num` in OpenSCAD: `func.cc` guards `type()==NUMBER && !isnan(x)`, so
        // `is_num(0/0)` is `false` (BOSL2's `f_is_num` test pins `[NAN, false]`). `is_nan` catches those.
        "is_num" => pred(pos, |v| matches!(v, Value::Num(n) if !n.is_nan())),
        "is_string" => pred(pos, |v| matches!(v, Value::Str(_))),
        "is_list" => pred(pos, |v| matches!(v, Value::NumList(_) | Value::List(_))),
        "is_function" => pred(pos, |v| matches!(v, Value::Function { .. })),
        "version" => Value::num_list(vec![2021.0, 1.0, 0.0]),
        "version_num" => Value::Num(20_210_100.0),
        _ => Value::Undef,
    }
}

/// OpenSCAD `sign`: `-1`/`0`/`1` (unlike Rust's `signum`, which is `±1` at zero and `NaN` at `NaN`).
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0 // includes ±0 and NaN (both comparisons false), matching func.cc
    }
}

/// Apply a unary numeric function to the first arg; non-number / missing → `undef`.
fn num1(pos: &[Value], f: impl Fn(f64) -> f64) -> Value {
    match pos.first() {
        Some(&Value::Num(x)) => Value::Num(f(x)),
        _ => Value::Undef,
    }
}

/// Apply a binary numeric function to the first two args; non-numbers / missing → `undef`.
fn num2(pos: &[Value], f: impl Fn(f64, f64) -> f64) -> Value {
    match (pos.first(), pos.get(1)) {
        (Some(&Value::Num(a)), Some(&Value::Num(b))) => Value::Num(f(a, b)),
        _ => Value::Undef,
    }
}

/// `min`/`max`: either several numeric args, or a single numeric-list arg. Empty / ill-typed → `undef`.
fn min_max(pos: &[Value], is_min: bool) -> Value {
    let nums: Vec<f64> = match pos {
        [Value::NumList(xs)] => xs.to_vec(),
        [Value::Num(x)] => vec![*x],
        multi => {
            let mut v = Vec::with_capacity(multi.len());
            for value in multi {
                match value {
                    Value::Num(x) => v.push(*x),
                    _ => return Value::Undef,
                }
            }
            v
        }
    };
    match nums.split_first() {
        Some((&head, rest)) => Value::Num(
            rest.iter()
                .fold(head, |acc, &x| if is_min { acc.min(x) } else { acc.max(x) }),
        ),
        None => Value::Undef, // min()/max() with no numbers
    }
}

/// `norm(v)` — the Euclidean length of a numeric vector (sequential sum of squares, matching `func.cc`).
fn norm(pos: &[Value]) -> Value {
    match pos.first() {
        Some(Value::NumList(xs)) => Value::Num(xs.iter().map(|x| x * x).sum::<f64>().sqrt()),
        _ => Value::Undef,
    }
}

/// `cross(a, b)` — the 3D cross product (a 3-vector), or the 2D cross (a scalar). Anything else → `undef`.
fn cross(pos: &[Value]) -> Value {
    match (pos.first(), pos.get(1)) {
        (Some(Value::NumList(a)), Some(Value::NumList(b))) => {
            // A NaN/inf component is `undef` upstream (AH.2.1, cross-tests golden: "Invalid value
            // (NaN/INF) in parameter vector for cross()"), not a NaN-propagated vector.
            if a.iter().chain(b.iter()).any(|x| !x.is_finite()) {
                return Value::Undef;
            }
            match (&a[..], &b[..]) {
                ([a0, a1, a2], [b0, b1, b2]) => Value::num_list(vec![
                    a1 * b2 - a2 * b1,
                    a2 * b0 - a0 * b2,
                    a0 * b1 - a1 * b0,
                ]),
                ([a0, a1], [b0, b1]) => Value::Num(a0 * b1 - a1 * b0),
                _ => Value::Undef,
            }
        }
        _ => Value::Undef,
    }
}

// ─────────────────────────────── list + string group (I.4.2) ─────────────────────────────────────

/// A `usize` from a list index / length as an `f64` — indices and lengths are far below `2^53`, so
/// the conversion is exact (this is the one place the cast lives, so the `allow` lives here too).
#[allow(
    clippy::cast_precision_loss,
    reason = "list indices/lengths are far below 2^53; f64 is exact"
)]
fn count(n: usize) -> f64 {
    n as f64
}

/// A finite, non-negative `Value::Num` as a `usize` — the form of `search`'s `num_returns_per_match`
/// and `index_col_num` params. Anything else → `None` (caller supplies the default).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "checked finite and >= 0; `as usize` truncates the fraction (OpenSCAD casts to int too)"
)]
fn as_index(v: &Value) -> Option<usize> {
    match v {
        &Value::Num(n) if n.is_finite() && n >= 0.0 => Some(n as usize),
        _ => None,
    }
}

/// `len(x)` — element count of a list, or CHARACTER count of a string (Unicode scalars, not bytes).
/// A number / bool / undef / range / function has no length → `undef`.
fn len(pos: &[Value]) -> Value {
    // Exactly one argument — `len("test","upps")` is an arity warning + undef upstream (AH.2.1,
    // the isundef-test golden leans on it).
    match pos {
        [Value::NumList(xs)] => Value::Num(count(xs.len())),
        [Value::List(xs)] => Value::Num(count(xs.len())),
        [Value::Str(s)] => Value::Num(count(s.chars().count())),
        _ => Value::Undef,
    }
}

/// `concat(a, b, …)` — flatten ONE level: a list arg contributes its elements, anything else (number,
/// string, range, undef) is appended whole (`func.cc` expands vectors only). All-numeric → `NumList`.
fn concat(pos: &[Value]) -> Value {
    let mut out = Vec::new();
    for v in pos {
        match v {
            Value::NumList(xs) => out.extend(xs.iter().map(|&x| Value::Num(x))),
            Value::List(xs) => out.extend(xs.iter().cloned()),
            other => out.push(other.clone()),
        }
    }
    build_vector(out)
}

/// `str(a, b, …)` — concatenate each arg's string form. A TOP-LEVEL string is raw (`str("ab") == "ab"`);
/// everything else routes through the shared [`format_value`] (which quotes strings nested in lists).
fn str_concat(pos: &[Value]) -> Value {
    let mut s = String::new();
    for v in pos {
        match v {
            Value::Str(x) => s.push_str(x), // top-level string: raw, no quotes
            other => s.push_str(&format_value(other)),
        }
    }
    Value::string(s)
}

/// `chr(n | [n…] | range)` — Unicode codepoints → a string. Codepoints below `1`, non-finite, or not a
/// valid scalar value are SKIPPED (`func.cc`). A string / bool / undef arg → `undef` (chr wants numbers).
fn chr(pos: &[Value]) -> Value {
    // EVERY argument contributes (AH.2.1, chr-tests golden): `chr(90, 89, 88)` is `"ZYX"`, a
    // vector/range argument splices its elements — RECURSIVELY, `chr([65,[66,[67]]])` is `"ABC"` —
    // and anything unconvertible (a string, a bool, an out-of-range code) contributes NOTHING.
    // `chr()` with no args is `""`, not undef. Iterative walk: corpus nesting is shallow, but the
    // evaluator's no-host-recursion doctrine holds anyway.
    let mut s = String::new();
    let mut stack: Vec<Value> = pos.iter().rev().cloned().collect();
    while let Some(v) = stack.pop() {
        match v {
            Value::Num(n) => {
                if let Some(c) = code_to_char(n) {
                    s.push(c);
                }
            }
            Value::NumList(_) | Value::List(_) | Value::Range { .. } => {
                let kids = iter_values_raw(&v);
                stack.extend(kids.into_iter().rev());
            }
            _ => {}
        }
    }
    Value::string(s)
}

/// A codepoint `f64` → its `char`, or `None` when below `1`, non-finite, or not a valid Unicode scalar
/// (surrogate / above `U+10FFFF`). The fraction truncates (OpenSCAD casts to int).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "guarded finite and >= 1; `as u32` saturates a huge value, then from_u32 rejects it"
)]
fn code_to_char(code: f64) -> Option<char> {
    if !code.is_finite() || code < 1.0 {
        return None;
    }
    char::from_u32(code as u32)
}

/// `ord(s)` — the codepoint of a string's FIRST character. Empty string / non-string → `undef`.
fn ord(pos: &[Value]) -> Value {
    match pos.first() {
        Some(Value::Str(s)) => match s.chars().next() {
            Some(c) => Value::Num(f64::from(c as u32)),
            None => Value::Undef, // ord("") → undef
        },
        _ => Value::Undef,
    }
}

/// `reverse(x)` — a list or string reversed. Number / range / undef / function → `undef`.
fn reverse(pos: &[Value]) -> Value {
    match pos.first() {
        Some(Value::NumList(xs)) => Value::num_list(xs.iter().rev().copied().collect::<Vec<_>>()),
        Some(Value::List(xs)) => Value::list(xs.iter().rev().cloned().collect::<Vec<_>>()),
        Some(Value::Str(s)) => Value::string(s.chars().rev().collect::<String>()),
        _ => Value::Undef,
    }
}

/// `lookup(key, table)` — linear interpolation over a table of `[x, y]` pairs, CLAMPED at the ends
/// (below the lowest `x` → its `y`, above the highest → its `y`), matching `func.cc`. Non-numeric key
/// or no valid pairs → `undef`. The table need not be sorted: the bracketing pair is found by scan.
fn lookup(pos: &[Value]) -> Value {
    // Strict arity-2 (AH.2.8, the errors-warnings golden): `lookup(1, table, 3)` is a too-many-
    // arguments warning + undef upstream, not a computed interpolation.
    if pos.len() != 2 {
        return Value::Undef;
    }
    let key = match pos.first() {
        Some(&Value::Num(k)) if k.is_finite() => k,
        _ => return Value::Undef,
    };
    let table = match pos.get(1) {
        Some(t) => iter_values_raw(t),
        None => return Value::Undef,
    };
    // low = the pair with the largest x <= key; high = the smallest x >= key.
    let mut low: Option<(f64, f64)> = None;
    let mut high: Option<(f64, f64)> = None;
    for row in &table {
        if let Some((x, y)) = as_pair(row) {
            if x <= key && low.is_none_or(|(lx, _)| x > lx) {
                low = Some((x, y));
            }
            if x >= key && high.is_none_or(|(hx, _)| x < hx) {
                high = Some((x, y));
            }
        }
    }
    match (low, high) {
        (None, None) => Value::Undef,            // no valid pairs
        (Some((_, ly)), None) => Value::Num(ly), // key above all → clamp to last y
        (None, Some((_, hy))) => Value::Num(hy), // key below all → clamp to first y
        // low/high always bracket the key (lx <= key <= hx). `key <= lx` means key == lx — an exact
        // hit on a point (and, when lx == hx, the degenerate single-point case) → that y; it also
        // guards the divisor, since lx < key implies hx > key strictly (a point AT key would have set
        // lx == key). Otherwise interpolate. (`func.cc` writes the two end-clamps as separate `>=`/`<=`
        // guards; here the bracket invariant collapses the high clamp into the exact-hit case.)
        (Some((lx, ly)), Some((hx, hy))) => {
            if key <= lx {
                Value::Num(ly)
            } else {
                let f = (key - lx) / (hx - lx);
                Value::Num(ly * (1.0 - f) + hy * f)
            }
        }
    }
}

/// A table row as an `[x, y]` numeric pair (extra columns ignored), else `None`.
fn as_pair(row: &Value) -> Option<(f64, f64)> {
    match row {
        Value::NumList(xs) => match &xs[..] {
            [x, y, ..] => Some((*x, *y)),
            _ => None,
        },
        Value::List(xs) => match &xs[..] {
            [Value::Num(x), Value::Num(y), ..] => Some((*x, *y)),
            _ => None,
        },
        _ => None,
    }
}

/// `search(find, table, num_returns_per_match = 1, index_col_num = 0)` — `func.cc`'s find-indices
/// primitive. A NUMBER `find` returns a FLAT list of the matching indices (capped by `num_returns`,
/// `0` = all). A STRING or LIST `find` searches PER element/char: with `num_returns == 1` each hit is
/// its first index flattened in (a miss contributes nothing); otherwise each contributes a SUB-list of
/// up to `num_returns` indices (`0` = all), so misses show as `[]`. `index_col_num` compares against
/// `row[index_col_num]` when the table rows are lists.
fn search(pos: &[Value]) -> Value {
    let (Some(find), Some(table)) = (pos.first(), pos.get(1)) else {
        return Value::Undef;
    };
    let num_returns = pos.get(2).and_then(as_index).unwrap_or(1);
    let index_col = pos.get(3).and_then(as_index).unwrap_or(0);
    let rows = iter_values_raw(table);
    match find {
        // a numeric search is always a flat list of hit indices, capped by num_returns (0 = all).
        Value::Num(_) | Value::Bool(_) => build_vector(hits(find, &rows, num_returns, index_col)),
        // A STRING match drops misses (`search("abe","abc",1)` = `[0,1]` — 'e' vanishes)…
        Value::Str(s) => {
            // …and over a LIST table, a malformed (non-list) row invalidates the WHOLE call:
            // upstream warns per bad row and returns [] overall (AH.2.2, search-tests golden —
            // `search("a", [["a",1],123], num_returns_per_match=0)` is `[]`, not `[[0]]`).
            if matches!(table, Value::NumList(_) | Value::List(_))
                && rows
                    .iter()
                    .any(|r| !matches!(r, Value::NumList(_) | Value::List(_)))
            {
                return build_vector(Vec::new());
            }
            let keys: Vec<Value> = s.chars().map(|c| Value::string(c.to_string())).collect();
            build_vector(per_key_search(&keys, &rows, num_returns, index_col, false))
        }
        // …but a LIST match KEEPS them as `[]` in place (`search([0,1,2,3],[1],1)` = `[[],0,[],[]]`).
        // That asymmetry is an OpenSCAD quirk (verified vs the oracle), and BOSL2's `list_remove` leans on
        // it — `if (sres[i] == [])` needs the misses positional. Dropping them broke list_remove → str_split.
        Value::NumList(_) | Value::List(_) => build_vector(per_key_search(
            &iter_values_raw(find),
            &rows,
            num_returns,
            index_col,
            true,
        )),
        _ => Value::Undef,
    }
}

/// The indices in `rows` matching `key` (via [`matches_at`]), capped at `num_returns` (`0` = all),
/// as `Value::Num`s.
fn hits(key: &Value, rows: &[Value], num_returns: usize, index_col: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for (j, elem) in rows.iter().enumerate() {
        if matches_at(key, elem, index_col) {
            out.push(Value::Num(count(j)));
            if num_returns != 0 && out.len() >= num_returns {
                break;
            }
        }
    }
    out
}

/// The per-key half of `search` for STRING/LIST `find`. For `num_returns == 1` each key yields its FIRST
/// hit as a scalar; a MISS either drops out (`keep_misses == false`, the string-match rule) or stays as `[]`
/// in place (`keep_misses == true`, the list-match rule — the OpenSCAD asymmetry `list_remove` depends on).
/// Otherwise (`num_returns != 1`) each key contributes a sub-list (misses → `[]`) regardless.
fn per_key_search(
    keys: &[Value],
    rows: &[Value],
    num_returns: usize,
    index_col: usize,
    keep_misses: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    for key in keys {
        let found = hits(key, rows, num_returns, index_col);
        if num_returns == 1 {
            match found.into_iter().next() {
                Some(hit) => out.push(hit),
                None if keep_misses => out.push(build_vector(Vec::new())), // `[]` kept positional
                None => {}                                                 // miss dropped
            }
        } else {
            out.push(build_vector(found));
        }
    }
    out
}

/// Does `key` match table row `elem`? Directly when `index_col == 0`, else against `elem[index_col]`
/// (when `elem` is a list long enough). `NaN` never matches (IEEE), like OpenSCAD.
fn matches_at(key: &Value, elem: &Value, index_col: usize) -> bool {
    (index_col == 0 && key == elem) || column(elem, index_col).as_ref() == Some(key)
}

/// The `i`-th column of a list row, else `None` (scalar row, or too short).
fn column(elem: &Value, i: usize) -> Option<Value> {
    match elem {
        Value::NumList(xs) => xs.get(i).map(|&n| Value::Num(n)),
        Value::List(xs) => xs.get(i).cloned(),
        _ => None,
    }
}

/// `rands(min, max, count, [seed])` → `count` uniform draws in `[min, max)`, bug-for-bug vs OpenSCAD's
/// boost MT19937 + `uniform_real_distribution` (see [`super::rng`]). Non-numeric `min`/`max` or a bad
/// `count` → `undef`. With an explicit `seed`, a fresh engine → a pure function of the args (oracle-exact).
/// WITHOUT a seed, draws from the evaluator's ONE advancing [`RandStream`](super::rng::RandStream), so
/// consecutive seedless calls DIFFER (OpenSCAD draws seedless from a single global engine — BOSL2 needs
/// two `rands()` calls to make a non-degenerate line). Called via the [`run_builtin`](super::run_builtin)
/// seam that holds the stream, not the pure `apply` dispatch.
pub(super) fn rands(pos: &[Value], stream: &mut super::rng::RandStream) -> Value {
    let (Some(&Value::Num(min)), Some(&Value::Num(max))) = (pos.first(), pos.get(1)) else {
        return Value::Undef;
    };
    let Some(count) = pos.get(2).and_then(as_index) else {
        return Value::Undef;
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "boost seeds mt19937 with the value cast to the engine's 32-bit result_type; the BOSL2 \
                  seeds are small positive ints"
    )]
    let seed = match pos.get(3) {
        Some(&Value::Num(s)) if s.is_finite() => Some(s as u32),
        _ => None,
    };
    let draws = match seed {
        Some(s) => super::rng::rands(min, max, count, Some(s)), // seeded → fresh engine, oracle-exact
        None => stream.draw(min, max, count), // seedless → advance the eval's stream
    };
    Value::num_list(draws)
}

// ─────────────────────────────── type predicates + version (I.4.3) ────────────────────────────────

/// A positive type predicate (`is_bool`/`is_num`/…): the first arg is present AND satisfies `f`. A
/// missing arg → `false` (there is no value of that type). `is_undef` is the one that treats absence
/// as `undef` (→ `true`), so it doesn't go through here.
/// A type predicate over exactly ONE argument. An arity mismatch — no args, or extras — is `undef`
/// upstream (AH.2.1, the is*-test goldens: `is_num()` AND `is_num(1,2,3)` both warn + undef),
/// distinct from `is_*(undef)` = a real answer over a real (undefined) value.
fn pred(pos: &[Value], f: impl Fn(&Value) -> bool) -> Value {
    match pos {
        [v] => Value::Bool(f(v)),
        _ => Value::Undef,
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, apply};

    #[test]
    fn unknown_name_is_undef() {
        // `apply` is gated by `is_builtin` at every call site, so this fallback is reachable only here.
        assert_eq!(apply("not_a_builtin", &[]), Value::Undef);
    }
}
