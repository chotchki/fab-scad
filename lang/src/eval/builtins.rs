//! OpenSCAD builtin FUNCTIONS (`func.cc`) — declared ONCE, derived everywhere (AS.2/AS.3).
//!
//! A builtin is a leaf operation: its arguments evaluate on the explicit stack, then this dispatches
//! by name. Ill-typed / missing args yield `undef` (OpenSCAD's undef-propagation), never an error.
//! Trig is in DEGREES and reuses `trig`'s exact-quadrant `sin`/`cos` so `sin(30)` etc. match the
//! geometry path bit-for-bit.
//!
//! THE ONE DECLARATION: [`declare_builtins!`] is the single answer to "what is a builtin". Each row
//! carries the full [`Decl`] (name, return domain, parameter names + domains) AND the
//! implementation, grouped by CAPABILITY ([`BuiltinCapability`]) — and [`is_builtin`], [`apply`],
//! [`context_impl`] and [`BUILTIN_SURFACE`] are all generated from the same rows, so membership,
//! dispatch and the declared surface cannot drift (the AR.20.10 family — a name `is_builtin`
//! accepts that `apply` answers with silent `undef` — is unrepresentable). `pure` rows are
//! functions of their argument values, exposed one-per-name in [`bi`], the only builtin surface
//! generated code may call. `context` rows need what `apply` never receives — argument NAMES
//! (`textmetrics`/`fontmetrics`/`object`), the run's advancing
//! [`RandStream`](super::rng::RandStream) (seedless `rands`, the ONE impure builtin), or the live
//! module-instantiation stack (`parent_module`) — and exist only behind [`context_impl`], so the
//! pure dispatch simply has no function to hand out for them.
//!
//! The list/string group (I.4.2) is the glue BOSL2 lives on: `len`/`concat` are vector
//! surgery, `chr`/`ord` bridge codepoints↔strings, `str` routes through the shared [`fmt`](super::fmt)
//! formatter (so echo at I.5 refines ONE place), and `lookup`/`search` are the table primitives —
//! `lookup` linear-interpolates + clamps at the ends, `search` follows `func.cc`'s per-match protocol
//! (`num_returns_per_match`: 1 = flat first-hits, 0 = all, n = up to n; `index_col_num` picks a column).
//!
//! Type predicates (I.4.3) are trivial variant tests. `version`/`version_num` report a PINNED constant
//! (last stable `2021.01`), NOT the host build — the oracle is nightly (a build-date version), but the
//! determinism doctrine forbids env-derived values, so we pin a release that clears BOSL2's minimum and
//! bucket the oracle's build-date `version()` as a known K divergence.

use super::fmt::format_value;
use super::trig;
use super::value::Value;
use super::{build_vector, iter_values_raw};
use crate::surface::{BuiltinCapability, BuiltinDecl, Decl, Domain, Kind, Param};

/// A required [`Param`] — row shorthand for [`declare_builtins!`].
const fn p(name: &'static str, domain: Domain) -> Param {
    Param {
        name,
        domain,
        required: true,
    }
}

/// An optional [`Param`] (an upstream default fills it) — row shorthand for [`declare_builtins!`].
const fn opt(name: &'static str, domain: Domain) -> Param {
    Param {
        name,
        domain,
        required: false,
    }
}

/// A context builtin's implementation, tagged by the capability it needs — the runtime half of
/// [`BuiltinCapability`]. `run_builtin` matches on THIS (the one place the evaluator supplies
/// context), so a sixth context builtin is a new row in [`declare_builtins!`], not a name check to
/// remember in N places.
#[derive(Clone, Copy)]
pub(super) enum ContextImpl {
    /// Needs the argument NAMES (and may warn through the console). Takes the names as a bare
    /// slice rather than AST `Arg`s so a VALUE-shaped caller — `ModuleCtx::call_fn`'s dispatch
    /// (AR.22), which has no AST — shares the one implementation with `run_builtin`.
    Named(fn(&[Option<&str>], &[Value], &mut Vec<super::Message>) -> Value),
    /// Draws from the evaluator's ONE advancing seedless stream.
    Stream(fn(&[Value], &mut super::rng::RandStream) -> Value),
    /// Reads the live module-instantiation name stack (innermost last).
    Stack(fn(&[Value], &[&str]) -> Value),
}

/// THE ONE DECLARATION (AS.2): one row per builtin — its [`Decl`] and its implementation, grouped
/// by capability. From the SAME rows this derives [`is_builtin`] (membership), [`apply`] (pure
/// dispatch, one direct statically-dispatched arm per `pure` row — the hot path keeps today's
/// match-on-name shape), [`context_impl`] (the evaluator's capability dispatch), and
/// [`BUILTIN_SURFACE`] (the declared surface every other consumer reads). A builtin cannot appear
/// in one artifact and not the others.
macro_rules! declare_builtins {
    (
        pure {
            $( $pname:literal => $pf:path, ret $pret:expr, params $pparams:expr; )+
        }
        context {
            $( $cname:literal => $ccap:ident($cf:expr), ret $cret:expr, names_bind $cnb:expr, params $cparams:expr; )+
        }
    ) => {
        /// Is `name` a builtin we implement? Checked at a call site AFTER user functions, BEFORE
        /// "unknown" (so a user function may shadow a builtin, per OpenSCAD). Derived from
        /// [`declare_builtins!`].
        #[must_use]
        pub fn is_builtin(name: &str) -> bool {
            matches!(name, $( $pname )|+ $( | $cname )+)
        }

        /// Apply a PURE builtin by name to its args. OpenSCAD builtins have no declared parameter
        /// names, so `pos` is the WHOLE argument list in source order (a named arg's name is dropped
        /// upstream in `run_builtin`); e.g. `search`'s `num_returns_per_match`/`index_col_num` are
        /// just positions 2 and 3 here. A CONTEXT builtin has no arm — its implementation exists
        /// only behind [`context_impl`] — and an unknown name is `undef` (this dispatch is gated by
        /// [`is_builtin`] at every call site).
        pub fn apply(name: &str, pos: &[Value]) -> Value {
            match name {
                $( $pname => $pf(pos), )+
                _ => Value::Undef,
            }
        }

        /// A context builtin's implementation — `None` for a pure (or unknown) name. The evaluator
        /// matches the capability here instead of comparing names, which is what stops a new
        /// context builtin being added in one place and forgotten in another.
        pub(super) fn context_impl(name: &str) -> Option<ContextImpl> {
            match name {
                $( $cname => Some(ContextImpl::$ccap($cf)), )+
                _ => None,
            }
        }

        /// The declared builtin surface, one entry per row of [`declare_builtins!`]: membership,
        /// capability, and the call shape (return domain, parameter names + domains). The emitter
        /// consults it for callability, the fuzzer for generation, the conformance suite for probe
        /// domains — all reading the same rows the evaluator dispatches from.
        pub const BUILTIN_SURFACE: &[BuiltinDecl] = &[
            $(
                BuiltinDecl {
                    decl: Decl {
                        name: $pname,
                        kind: Kind::Function,
                        ret: $pret,
                        names_bind: false,
                        params: $pparams,
                    },
                    capability: BuiltinCapability::Pure,
                },
            )+
            $(
                BuiltinDecl {
                    decl: Decl {
                        name: $cname,
                        kind: Kind::Function,
                        ret: $cret,
                        names_bind: $cnb,
                        params: $cparams,
                    },
                    capability: BuiltinCapability::$ccap,
                },
            )+
        ];
    };
}

declare_builtins! {
    pure {
        // ── math (func.cc). Return domains are conservative so calls COMPOSE (`sin` returns a
        // Unit, `asin` wants one); argument domains are GENERATION domains — what makes a call
        // compute rather than undef (AR.4) — which is also exactly what a conformance probe needs.
        "abs" => bi::abs, ret Domain::Num, params &[p("x", Domain::Num)];
        "sign" => bi::sign, ret Domain::Num, params &[p("x", Domain::Num)];
        "sin" => bi::sin, ret Domain::Unit, params &[p("x", Domain::Deg)];
        "cos" => bi::cos, ret Domain::Unit, params &[p("x", Domain::Deg)];
        "tan" => bi::tan, ret Domain::Num, params &[p("x", Domain::Deg)];
        "asin" => bi::asin, ret Domain::Deg, params &[p("x", Domain::Unit)];
        "acos" => bi::acos, ret Domain::Deg, params &[p("x", Domain::Unit)];
        "atan" => bi::atan, ret Domain::Deg, params &[p("x", Domain::Num)];
        "atan2" => bi::atan2, ret Domain::Deg, params &[p("y", Domain::Num), p("x", Domain::Num)];
        "floor" => bi::floor, ret Domain::Num, params &[p("x", Domain::Num)];
        "ceil" => bi::ceil, ret Domain::Num, params &[p("x", Domain::Num)];
        "round" => bi::round, ret Domain::Num, params &[p("x", Domain::Num)];
        // ln/log/sqrt take Pos: a negative argument is instant NaN.
        "ln" => bi::ln, ret Domain::Num, params &[p("x", Domain::Pos)];
        "log" => bi::log, ret Domain::Num, params &[p("x", Domain::Pos)];
        "exp" => bi::exp, ret Domain::Num, params &[p("x", Domain::Num)];
        // pow's base is Pos: a negative base under a fractional exponent is NaN.
        "pow" => bi::pow, ret Domain::Num, params &[p("base", Domain::Pos), p("exponent", Domain::Num)];
        "sqrt" => bi::sqrt, ret Domain::Num, params &[p("x", Domain::Pos)];
        // min/max (and str/concat below) are VARIADIC upstream, pinned at the arity the corpus has
        // always generated — a generation choice, not a language claim (see `Decl::arity`).
        "min" => bi::min, ret Domain::Num, params &[p("a", Domain::Num), p("b", Domain::Num)];
        "max" => bi::max, ret Domain::Num, params &[p("a", Domain::Num), p("b", Domain::Num)];
        "norm" => bi::norm, ret Domain::Num, params &[p("v", Domain::VecN)];
        "cross" => bi::cross, ret Domain::Vec3, params &[p("a", Domain::Vec3), p("b", Domain::Vec3)];
        // ── list + string (I.4.2) ──
        // len takes VecN, not Any: `len(5)` is undef upstream.
        "len" => bi::len, ret Domain::Num, params &[p("value", Domain::VecN)];
        "concat" => bi::concat, ret Domain::List, params &[p("a", Domain::Any), p("b", Domain::Any)];
        "str" => bi::str, ret Domain::Str, params &[p("a", Domain::Any), p("b", Domain::Any)];
        "chr" => bi::chr, ret Domain::Str, params &[p("n", Domain::Pos)];
        "ord" => bi::ord, ret Domain::Num, params &[p("c", Domain::Str)];
        "lookup" => bi::lookup, ret Domain::Num, params &[p("key", Domain::Num), p("pairs", Domain::Table)];
        // search's match_value is Num NOT Any: a string key searched over a non-string column
        // ABORTS the upstream oracle (openscad#5017; docs/openscad-search-crash.md).
        "search" => bi::search, ret Domain::List, params &[p("match_value", Domain::Num), p("string_or_vector", Domain::Table)];
        // ── objects (AF.4) ──
        "is_object" => bi::is_object, ret Domain::Bool, params &[p("value", Domain::Any)];
        "has_key" => bi::has_key, ret Domain::Bool, params &[p("object", Domain::Any), p("key", Domain::Str)];
        // ── type predicates + version (I.4.3) ──
        "is_undef" => bi::is_undef, ret Domain::Bool, params &[p("value", Domain::Any)];
        "is_bool" => bi::is_bool, ret Domain::Bool, params &[p("value", Domain::Any)];
        "is_num" => bi::is_num, ret Domain::Bool, params &[p("value", Domain::Any)];
        "is_string" => bi::is_string, ret Domain::Bool, params &[p("value", Domain::Any)];
        "is_list" => bi::is_list, ret Domain::Bool, params &[p("value", Domain::Any)];
        "is_function" => bi::is_function, ret Domain::Bool, params &[p("value", Domain::Any)];
        "version" => bi::version, ret Domain::VecN, params &[];
        "version_num" => bi::version_num, ret Domain::Num, params &[];
    }
    context {
        // The capability partition: each implementation needs something `apply` never receives, so
        // none of these has a `bi` function and generated code cannot name them.
        // Seedless `rands` draws from the evaluator's one advancing stream — the ONE impure builtin.
        "rands" => Stream(rands), ret Domain::VecN, names_bind false, params &[p("min_value", Domain::Num), p("max_value", Domain::Num), p("value_count", Domain::Pos), opt("seed", Domain::Num)];
        // `object`'s member names ARE the argument names (AF.4) — open-ended, so no declared params.
        "object" => Named(object_named), ret Domain::Any, names_bind true, params &[];
        // The metrics pair (AG): upstream's only builtins with DECLARED named parameters. Both
        // return an OBJECT (no Domain variant models that; `Any` is the honest widening).
        "textmetrics" => Named(textmetrics_named), ret Domain::Any, names_bind true, params &[opt("text", Domain::Str), opt("size", Domain::Num), opt("font", Domain::Str), opt("direction", Domain::Str), opt("language", Domain::Str), opt("script", Domain::Str), opt("halign", Domain::Str), opt("valign", Domain::Str), opt("spacing", Domain::Num)];
        "fontmetrics" => Named(fontmetrics_named), ret Domain::Any, names_bind true, params &[opt("size", Domain::Num), opt("font", Domain::Str)];
        // Module-instantiation stack introspection (control.cc) — impure to the eval memo (N.2c).
        "parent_module" => Stack(parent_module), ret Domain::Str, names_bind false, params &[opt("n", Domain::Num)];
    }
}

/// Look up a builtin's [`Decl`] by name, usable in CONST context — a consumer-side table built as
/// `&[builtin_decl("sin"), …]` makes a typo'd (or upstream-vanished) name a COMPILE error instead
/// of a silently absent surface entry. This is how the fuzzer keeps its own seed-frozen ORDER while
/// the declaration owns the CONTENT (AS.5).
///
/// # Panics
/// On a name that is not a declared builtin — at compile time when used in const position.
#[must_use]
#[allow(
    clippy::panic,
    reason = "the panic IS the feature: in const position it is a compile error naming the typo"
)]
pub const fn builtin_decl(name: &str) -> Decl {
    let mut i = 0;
    while i < BUILTIN_SURFACE.len() {
        if str_eq(BUILTIN_SURFACE[i].decl.name, name) {
            return BUILTIN_SURFACE[i].decl;
        }
        i += 1;
    }
    panic!("not a declared builtin")
}

/// Byte-wise `str` equality in const context (`==` on `&str` is not const).
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The PURE builtins as directly-callable functions, one per `pure` row of [`declare_builtins!`] —
/// re-exported as `rt::bi`, the ONLY builtin surface generated code may reach. A context builtin
/// has no function here, so emitted code that named one would fail to COMPILE — the silent-`undef`
/// answer the old string-dispatched `rt::builtin` gave (the AR.20.10 bug class) is unrepresentable
/// rather than tested-against.
pub mod bi {
    use super::Value;

    /// `abs(x)`.
    #[inline]
    #[must_use]
    pub fn abs(pos: &[Value]) -> Value {
        super::num1(pos, f64::abs)
    }

    /// `sign(x)` — `-1`/`0`/`1` (zero includes ±0 and NaN, matching `func.cc`).
    #[inline]
    #[must_use]
    pub fn sign(pos: &[Value]) -> Value {
        super::num1(pos, super::sign)
    }

    /// `sin(x)` — degrees, exact at quadrant points.
    #[inline]
    #[must_use]
    pub fn sin(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::sin_degrees)
    }

    /// `cos(x)` — degrees, exact at quadrant points.
    #[inline]
    #[must_use]
    pub fn cos(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::cos_degrees)
    }

    /// `tan(x)` — degrees.
    #[inline]
    #[must_use]
    pub fn tan(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::tan_degrees)
    }

    // Inverse trig snaps by upstream's GENERAL whole-degree round-trip rule (degree_trig.cc,
    // AH.2.12 follow-through): any input that is exactly the sin/cos/tan of an integer angle
    // returns that integer — `acos(-0.5)` is exactly 120, and `asin(sin(n)) == n` for every
    // integer degree (the trig-tests inverse sweep). atan2 snaps within 3e-14 of a whole.

    /// `asin(x)` — degrees.
    #[inline]
    #[must_use]
    pub fn asin(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::asin_degrees)
    }

    /// `acos(x)` — degrees.
    #[inline]
    #[must_use]
    pub fn acos(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::acos_degrees)
    }

    /// `atan(x)` — degrees.
    #[inline]
    #[must_use]
    pub fn atan(pos: &[Value]) -> Value {
        super::num1(pos, super::trig::atan_degrees)
    }

    /// `atan2(y, x)` — degrees.
    #[inline]
    #[must_use]
    pub fn atan2(pos: &[Value]) -> Value {
        super::num2(pos, super::trig::atan2_degrees)
    }

    /// `floor(x)`.
    #[inline]
    #[must_use]
    pub fn floor(pos: &[Value]) -> Value {
        super::num1(pos, f64::floor)
    }

    /// `ceil(x)`.
    #[inline]
    #[must_use]
    pub fn ceil(pos: &[Value]) -> Value {
        super::num1(pos, f64::ceil)
    }

    /// `round(x)` — half AWAY from zero, same as OpenSCAD.
    #[inline]
    #[must_use]
    pub fn round(pos: &[Value]) -> Value {
        super::num1(pos, f64::round)
    }

    /// `ln(x)`.
    #[inline]
    #[must_use]
    pub fn ln(pos: &[Value]) -> Value {
        super::num1(pos, f64::ln)
    }

    /// `log(x)` — base 10 (OpenSCAD's `log`).
    #[inline]
    #[must_use]
    pub fn log(pos: &[Value]) -> Value {
        super::num1(pos, f64::log10)
    }

    /// `exp(x)`.
    #[inline]
    #[must_use]
    pub fn exp(pos: &[Value]) -> Value {
        super::num1(pos, f64::exp)
    }

    /// `pow(base, exponent)`.
    #[inline]
    #[must_use]
    pub fn pow(pos: &[Value]) -> Value {
        super::num2(pos, f64::powf)
    }

    /// `sqrt(x)`.
    #[inline]
    #[must_use]
    pub fn sqrt(pos: &[Value]) -> Value {
        super::num1(pos, f64::sqrt)
    }

    /// `min(…)` — several numeric args, or one numeric list.
    #[inline]
    #[must_use]
    pub fn min(pos: &[Value]) -> Value {
        super::min_max(pos, true)
    }

    /// `max(…)` — several numeric args, or one numeric list.
    #[inline]
    #[must_use]
    pub fn max(pos: &[Value]) -> Value {
        super::min_max(pos, false)
    }

    /// `norm(v)` — Euclidean length of a numeric vector.
    #[inline]
    #[must_use]
    pub fn norm(pos: &[Value]) -> Value {
        super::norm(pos)
    }

    /// `cross(a, b)` — 3D cross product, or the 2D scalar cross.
    #[inline]
    #[must_use]
    pub fn cross(pos: &[Value]) -> Value {
        super::cross(pos)
    }

    /// `len(x)` — element count of a list, character count of a string.
    #[inline]
    #[must_use]
    pub fn len(pos: &[Value]) -> Value {
        super::len(pos)
    }

    /// `concat(…)` — flatten ONE level.
    #[inline]
    #[must_use]
    pub fn concat(pos: &[Value]) -> Value {
        super::concat(pos)
    }

    /// `str(…)` — concatenate each arg's string form.
    #[inline]
    #[must_use]
    pub fn str(pos: &[Value]) -> Value {
        super::str_concat(pos)
    }

    /// `chr(…)` — codepoints → a string.
    #[inline]
    #[must_use]
    pub fn chr(pos: &[Value]) -> Value {
        super::chr(pos)
    }

    /// `ord(s)` — the first character's codepoint.
    #[inline]
    #[must_use]
    pub fn ord(pos: &[Value]) -> Value {
        super::ord(pos)
    }

    /// `lookup(key, pairs)` — linear interpolation, clamped at the ends.
    #[inline]
    #[must_use]
    pub fn lookup(pos: &[Value]) -> Value {
        super::lookup(pos)
    }

    /// `search(find, table, …)` — `func.cc`'s find-indices primitive.
    #[inline]
    #[must_use]
    pub fn search(pos: &[Value]) -> Value {
        super::search(pos)
    }

    /// `is_undef(x)`.
    #[inline]
    #[must_use]
    pub fn is_undef(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Undef))
    }

    /// `is_bool(x)`.
    #[inline]
    #[must_use]
    pub fn is_bool(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Bool(_)))
    }

    /// `is_num(x)` — a NaN is NOT a number: `func.cc` guards `type()==NUMBER && !isnan(x)`, so
    /// `is_num(0/0)` is `false` (BOSL2's `f_is_num` test pins `[NAN, false]`). `is_nan` catches those.
    #[inline]
    #[must_use]
    pub fn is_num(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Num(n) if !n.is_nan()))
    }

    /// `is_string(x)`.
    #[inline]
    #[must_use]
    pub fn is_string(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Str(_)))
    }

    /// `is_list(x)`.
    #[inline]
    #[must_use]
    pub fn is_list(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::NumList(_) | Value::List(_)))
    }

    /// `is_function(x)`.
    #[inline]
    #[must_use]
    pub fn is_function(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Function { .. }))
    }

    /// `is_object(x)`.
    #[inline]
    #[must_use]
    pub fn is_object(pos: &[Value]) -> Value {
        super::pred(pos, |v| matches!(v, Value::Object(_)))
    }

    /// `has_key(object, key)` — membership; a non-object/non-string pair is `false`, wrong arity
    /// `undef`.
    #[inline]
    #[must_use]
    pub fn has_key(pos: &[Value]) -> Value {
        match pos {
            [Value::Object(o), Value::Str(k)] => Value::Bool(o.has_key(k)),
            [_, _] => Value::Bool(false),
            _ => Value::Undef,
        }
    }

    /// `version()` — the PINNED `[2021, 1, 0]` (see the module doc: no env-derived values).
    #[inline]
    #[must_use]
    pub fn version(_pos: &[Value]) -> Value {
        Value::num_list(vec![2021.0, 1.0, 0.0])
    }

    /// `version_num()` — `20210100`, same pin as [`version`].
    #[inline]
    #[must_use]
    pub fn version_num(_pos: &[Value]) -> Value {
        Value::Num(20_210_100.0)
    }
}

/// `textmetrics(...)` in the [`ContextImpl::Named`] shape; [`metrics_call`] carries the binding
/// logic shared by the metrics pair.
fn textmetrics_named(
    names: &[Option<&str>],
    pos: &[Value],
    messages: &mut Vec<super::Message>,
) -> Value {
    metrics_call("textmetrics", names, pos, messages)
}

/// `fontmetrics(...)` in the [`ContextImpl::Named`] shape.
fn fontmetrics_named(
    names: &[Option<&str>],
    pos: &[Value],
    messages: &mut Vec<super::Message>,
) -> Value {
    metrics_call("fontmetrics", names, pos, messages)
}

/// `object(...)` in the [`ContextImpl::Named`] shape — it reads names but never warns; the unused
/// messages slot is the price of ONE `Named` signature instead of two.
fn object_named(
    names: &[Option<&str>],
    pos: &[Value],
    _messages: &mut Vec<super::Message>,
) -> Value {
    object(names, pos)
}

/// `parent_module(n)` (`control.cc`) — the NAME of the module `n` levels up the instantiation stack (0 =
/// the current module, 1 = its parent), or `undef` if `n` overruns the stack. `stack` is innermost-last
/// (the current module at the end), so index `len-1-n`. A non-integer / negative `n` → `undef`. Stateful
/// (reads the evaluator's module stack), so it's dispatched from [`run_builtin`](super::run_builtin), not
/// the pure `apply`. BOSL2's `deprecate()` echoes `parent_module(1)` to name the deprecated module.
fn parent_module(pos: &[Value], stack: &[&str]) -> Value {
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
        // Upstream folds with a COMPARISON (`func.cc`), not fmin/fmax — and the difference is
        // observable at NaN: every compare against NaN is false, so the accumulator never moves.
        // A NaN FIRST element therefore poisons the whole fold (`min(0/0, 1)` is nan) while a
        // later NaN is skipped (`min(1, 0/0)` is 1). `f64::min`/`max` are IEEE minNum/maxNum —
        // NaN-ignoring in BOTH positions — and answered 1 for both. Caught by AS.6's generated
        // conformance probes; the JIT's `jit_fmin`/`jit_fmax` fold step moved in the same commit.
        Some((&head, rest)) => Value::Num(rest.iter().fold(head, |acc, &x| {
            let take = if is_min { x < acc } else { x > acc };
            if take { x } else { acc }
        })),
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
        [Value::Object(o)] => Value::Num(count(o.len())), // member count (AF.4)
        _ => Value::Undef,
    }
}

/// `textmetrics(...)` / `fontmetrics(...)` (AG) — the two builtins with DECLARED named parameters
/// upstream. Binding: positionals fill the declaration order, named args their slot, an unknown
/// name warns "not specified as parameter" and is ignored, and a WRONG-TYPED value warns
/// "Invalid type" and falls back to the default (the golden's all-zero textmetrics case is just
/// empty-text metrics). Returns an OBJECT (the golden shapes) — see [`super::metrics`].
fn metrics_call(
    name: &str,
    args: &[Option<&str>],
    pos: &[Value],
    messages: &mut Vec<super::Message>,
) -> Value {
    let params: &[&str] = if name == "textmetrics" {
        &[
            "text",
            "size",
            "font",
            "direction",
            "language",
            "script",
            "halign",
            "valign",
            "spacing",
        ]
    } else {
        &["size", "font"]
    };
    let mut bound: std::collections::BTreeMap<&str, &Value> = std::collections::BTreeMap::new();
    let mut next_positional = 0usize;
    for (arg, value) in args.iter().zip(pos) {
        match arg {
            None => {
                if let Some(&slot) = params.get(next_positional) {
                    bound.insert(slot, value);
                }
                next_positional += 1;
            }
            Some(n) => match params.iter().find(|&&p| p == *n) {
                Some(&slot) => {
                    bound.insert(slot, value);
                }
                None => messages.push(super::Message::Warning(format!(
                    "variable \"{n}\" not specified as parameter"
                ))),
            },
        }
    }
    // upstream's parameter type names ("vector", not "list").
    let upstream_type = |v: &Value| match v {
        Value::NumList(_) | Value::List(_) => "vector",
        other => other.type_name(),
    };
    let mut warn_bad = |param: &str, v: &Value, want: &str| {
        messages.push(super::Message::Warning(format!(
            "{name}(..., {param}={}) Invalid type: expected {want}, found {}",
            format_value(v),
            upstream_type(v)
        )));
    };
    let mut take_num = |param: &str, default: f64| match bound.get(param) {
        Some(Value::Num(n)) => *n,
        Some(v) => {
            warn_bad(param, v, "number");
            default
        }
        None => default,
    };
    let size = take_num("size", 10.0);
    if name == "fontmetrics" {
        // font is type-checked (string) but only the bundled face is honored.
        if let Some(v) = bound.get("font")
            && !matches!(v, Value::Str(_))
        {
            warn_bad("font", v, "string");
        }
        return super::metrics::fontmetrics(size);
    }
    let spacing = take_num("spacing", 1.0);
    let mut take_str = |param: &str, default: &str| match bound.get(param) {
        Some(Value::Str(s)) => s.to_string(),
        Some(v) => {
            warn_bad(param, v, "string");
            default.to_string()
        }
        None => default.to_string(),
    };
    let p = super::metrics::MetricsParams {
        text: take_str("text", ""),
        size,
        spacing,
        direction: take_str("direction", ""),
        language: take_str("language", "en"),
        script: take_str("script", ""),
        halign: take_str("halign", "default"),
        valign: take_str("valign", "default"),
    };
    if let Some(v) = bound.get("font")
        && !matches!(v, Value::Str(_))
    {
        warn_bad("font", v, "string");
    }
    super::metrics::textmetrics(&p)
}

/// `object(...)` — build an object, accumulating LEFT TO RIGHT (AF.4, upstream's experimental
/// `object()` always-on): a NAMED arg sets that member (`$`-named ones included — `$this=42` is a
/// member); a positional OBJECT merges its members in order (the copy form); a positional LIST is
/// an EDIT list — each element `[k]` removes `k`, `[k, v]` sets it; anything else contributes
/// nothing. `args` carries the names (this is why `run_builtin` routes here specially), `pos` the
/// evaluated values, index-aligned.
fn object(args: &[Option<&str>], pos: &[Value]) -> Value {
    let mut map = super::object::ObjectMap::new();
    for (arg, value) in args.iter().zip(pos) {
        match (arg, value) {
            (Some(name), v) => map.set(std::rc::Rc::from(*name), v.clone()),
            (None, Value::Object(o)) => {
                for (k, v) in o.iter() {
                    map.set(std::rc::Rc::clone(k), v.clone());
                }
            }
            (None, Value::NumList(xs)) if xs.is_empty() => {} // an empty edit list is a no-op
            (None, Value::List(edits)) => {
                for edit in edits.iter() {
                    // Every entry must be a `[key]` (remove) or `[key, value]` (set) pair with a
                    // STRING key — ANY malformed entry invalidates the WHOLE call (the
                    // object-warning-tests golden: bool/undef keys, over-long or empty pairs,
                    // undef entries all yield undef, not a partial object).
                    let Value::List(pair) = edit else {
                        return Value::Undef;
                    };
                    match (pair.first(), pair.get(1), pair.len()) {
                        (Some(Value::Str(k)), None, 1) => map.remove(k),
                        (Some(Value::Str(k)), Some(v), 2) => {
                            map.set(std::rc::Rc::clone(k), v.clone());
                        }
                        _ => return Value::Undef,
                    }
                }
            }
            _ => return Value::Undef, // a positional non-object/non-edit-list poisons the call
        }
    }
    Value::Object(std::rc::Rc::new(map))
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
fn rands(pos: &[Value], stream: &mut super::rng::RandStream) -> Value {
    // Upstream's argument treatment verbatim (AH.2.11, the rands golden's bizarro sweep):
    // arity 3-or-4, all numbers; a non-finite BOUND substitutes ±DBL_MAX/2 (warn + reset), then
    // min/max swap if reversed; count is |count|, non-finite → 1; the SEED is any double, mapped
    // through CPython's float hash ([`super::rng::seed_from_double`]) — never a truncation.
    if !(3..=4).contains(&pos.len()) {
        return Value::Undef;
    }
    let (Some(&Value::Num(min0)), Some(&Value::Num(max0)), Some(&Value::Num(count0))) =
        (pos.first(), pos.get(1), pos.get(2))
    else {
        return Value::Undef;
    };
    let seed = match pos.get(3) {
        Some(&Value::Num(s)) => Some(super::rng::seed_from_double(s)),
        Some(_) => return Value::Undef,
        None => None,
    };
    let mut min = if min0.is_finite() {
        min0
    } else {
        -f64::MAX / 2.0
    };
    let mut max = if max0.is_finite() {
        max0
    } else {
        f64::MAX / 2.0
    };
    if max < min {
        std::mem::swap(&mut min, &mut max);
    }
    let n = if count0.is_finite() {
        count0.abs()
    } else {
        1.0
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "n is finite and non-negative; a beyond-usize count saturates (upstream's \
                  boost_numeric_cast would throw — a count that size is pathological either way)"
    )]
    let count = n as usize;
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
    use super::{BUILTIN_SURFACE, ContextImpl, Value, apply, builtin_decl, context_impl, is_builtin};
    use crate::surface::BuiltinCapability;

    #[test]
    fn unknown_name_is_undef() {
        // `apply` is gated by `is_builtin` at every call site, so this fallback is reachable only here.
        assert_eq!(apply("not_a_builtin", &[]), Value::Undef);
    }

    #[test]
    fn the_declaration_is_the_membership_list() {
        // The count is a deliberate pin: a new builtin is a new row, and this number moving is the
        // reviewable diff (upstream func.cc + control.cc, AS.1's census).
        assert_eq!(BUILTIN_SURFACE.len(), 43);
        for b in BUILTIN_SURFACE {
            assert!(is_builtin(b.decl.name), "{} declared but not a member", b.decl.name);
        }
        let mut names: Vec<&str> = BUILTIN_SURFACE.iter().map(|b| b.decl.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), BUILTIN_SURFACE.len(), "duplicate declaration row");
        assert!(!is_builtin("not_a_builtin"));
    }

    #[test]
    fn capability_partitions_the_dispatch() {
        // Structurally: Pure ⇔ no context interception. (Both artifacts derive from the same rows,
        // so this can only fail if the macro itself regresses — that is what makes it cheap to keep.)
        for b in BUILTIN_SURFACE {
            match b.capability {
                BuiltinCapability::Pure => assert!(
                    context_impl(b.decl.name).is_none(),
                    "{} is Pure but intercepted",
                    b.decl.name
                ),
                _ => assert!(
                    context_impl(b.decl.name).is_some(),
                    "{} needs context but has no interception",
                    b.decl.name
                ),
            }
        }
        // The five context builtins under their exact capability — a row moved to the wrong group
        // would still compile, so pin the partition itself.
        assert!(matches!(context_impl("textmetrics"), Some(ContextImpl::Named(_))));
        assert!(matches!(context_impl("fontmetrics"), Some(ContextImpl::Named(_))));
        assert!(matches!(context_impl("object"), Some(ContextImpl::Named(_))));
        assert!(matches!(context_impl("rands"), Some(ContextImpl::Stream(_))));
        assert!(matches!(context_impl("parent_module"), Some(ContextImpl::Stack(_))));
        let context_rows = BUILTIN_SURFACE
            .iter()
            .filter(|b| b.capability != BuiltinCapability::Pure)
            .count();
        assert_eq!(context_rows, 5);
    }

    #[test]
    fn builtin_decl_is_const_usable() {
        // The consumer pattern (AS.5): a const table of lookups, where a typo is a COMPILE error.
        const SIN: crate::surface::Decl = builtin_decl("sin");
        assert_eq!(SIN.name, "sin");
        assert_eq!(SIN.arity(), 1);
        assert!(matches!(SIN.ret, crate::surface::Domain::Unit));
    }

    #[test]
    fn pure_rows_dispatch_through_the_declaration() {
        // Wiring smoke — semantics are pinned by eval_corpus; this only proves the macro's arms
        // reach the `bi` implementations.
        assert_eq!(apply("version_num", &[]), Value::Num(20_210_100.0));
        assert_eq!(apply("abs", &[Value::Num(-2.0)]), Value::Num(2.0));
    }
}
