//! AR.10 — the depth budget + decline-to-interpreter runtime under the generated natives.
//!
//! Generated natives recurse on the RUST stack (`approx` per list-nesting level, and the
//! approx↔idx↔posmod cycle is real), reintroducing the class the interpreter designed out with
//! its explicit stack. The refactor chotchki called for: every generated native counts entry
//! depth through a thread-local, and past [`MAX_NATIVE_DEPTH`] it DECLINES — interpreting its own
//! reference (plus baked constants) from the batch-emitted `FALLBACK_SOURCES` via the PURE oracle
//! instead. Both paths are proven equal by `fast_eq`, so the fallback is a missed speedup, never a
//! wrong answer; real BOSL2 data nests 2-3 deep and never sees it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::eval::value::Value;

/// The native tier's recursion allowance, and since AR.26.4.4 a BACKSTOP rather than the mechanism.
///
/// Frames here are HEAVY: a native→`call_value`→nested-machine→native ladder (the BOSL2-idiomatic
/// recursive fold) measured ~200 KiB of DEBUG stack per level and ~28 KiB in release, on
/// `window_light_blocker.scad`. A counter denominated in LEVELS cannot bound that on its own — what
/// overflows is bytes, the per-level cost is a property of whatever the emitter wrote, and by the
/// time the counter fires its frames are already spent.
///
/// WHAT ACTUALLY BOUNDS IT NOW is the shape of the emitted call graph. The emitter drops each
/// subject's CYCLE GROUP from its sibling table, so a cycle-internal call compiles to
/// `fx.call_named` and lands in the live evaluator's explicit stack instead of recursing here; what
/// is left is a DAG whose height is a build-time constant, asserted at 22-of-24 by fab-lib's
/// `the_static_call_graph_is_measured`. So the static worst case is known before the program runs.
///
/// This budget still guards what the DAG cannot: re-entry through a CLOSURE (`call_value`), where
/// the ladder alternates tiers and its depth follows the user's data. 32 sits above the measured DAG
/// height — below it and legitimate deep chains would decline for no reason — and everything past it
/// runs on the interpreter (AR.24: the LIVE evaluator, one machine), so it stays a speed knob, never
/// an answer.
pub(super) const MAX_NATIVE_DEPTH: u32 = 32;

/// One parsed island per distinct `FALLBACK_SOURCES` identity — keyed by the `&'static str`'s
/// (ptr, len), `None` for an island whose parse failed (cached so it doesn't re-parse per decline).
type IslandCache = Vec<((usize, usize), Option<Rc<crate::Program>>)>;

/// The deepest native ladder this thread has built, ever.
///
/// [`MAX_NATIVE_DEPTH`] is a stack-size budget, and a budget nobody can measure against is a guess:
/// the AR.10 note that "real BOSL2 data nests 2-3 deep and never sees it" was true of the 66 rows
/// fab-lang shipped and is a claim about the LIBRARY, which changed the moment a consumer could load
/// 1260. Monotonic per thread, so a caller reads it after a render.
#[must_use]
pub fn peak_native_depth() -> u32 {
    PEAK.with(Cell::get)
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static PEAK: Cell<u32> = const { Cell::new(0) };
    // Parsed fallback programs, keyed by the SOURCES' identity (ptr+len of the `&'static str`) —
    // NOT first-comer-wins. Each generated module carries its own `FALLBACK_SOURCES` island, and a
    // declining native must interpret ITS island: an unkeyed cache would pin whichever module
    // declined first on the thread and serve its program to every other module's natives.
    static FALLBACK: RefCell<IslandCache> = const { RefCell::new(Vec::new()) };
}

/// RAII depth ticket: `enter` refuses past the budget, `Drop` gives the level back — early
/// returns (assert raises, `?` on sibling calls) can't leak depth.
pub struct DepthGuard;

impl DepthGuard {
    /// Take one level of the budget, or `None` when it is spent. A generated native that gets
    /// `None` must DECLINE — see [`run_interpreted`] — rather than recurse anyway.
    #[must_use]
    pub fn enter() -> Option<Self> {
        DEPTH.with(|d| {
            if d.get() >= MAX_NATIVE_DEPTH {
                None
            } else {
                let now = d.get() + 1;
                d.set(now);
                PEAK.with(|p| p.set(p.get().max(now)));
                Some(DepthGuard)
            }
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Does this value CARRY a closure, at any depth? A `Value::Function`'s `closure_id` indexes the
/// evaluator it was minted in — it cannot cross into the fallback's throwaway ctx (inbound) or
/// out of it (outbound) without silently resolving against the WRONG closure table. Lists and
/// objects can hold closures; every other variant cannot. Explicit work stack, house doctrine.
fn carries_closure(v: &Value) -> bool {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Function { .. } => return true,
            Value::List(items) => stack.extend(items.iter()),
            Value::Object(map) => stack.extend(map.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    false
}

/// The decline path: interpret `name` from `sources` (parsed once per thread PER ISLAND) with
/// intrinsics DISABLED — one machine, explicit stack, no interior dispatch bouncing back to a
/// declining native. `sources` is `&'static str` BY CONTRACT, not convenience: the cache keys on
/// its (ptr, len), which only identifies a program if the bytes can never be freed and readdressed.
/// Parse failure is impossible by construction (the sources are the registry references that
/// already parsed for codegen), but the no-panic doctrine gets an error, not an unwrap.
///
/// # Errors
/// Whatever interpreting `name` raises — an `assert` in the reference propagates exactly as it
/// would from the native, which is the point of the fallback. Also errors if `sources` fails to
/// parse, which the caller's own codegen already ruled out — and, LOUDLY, when a closure would
/// cross the boundary in either direction (see below; AR.24 owns the real fix).
pub fn run_interpreted(sources: &'static str, name: &str, args: &[Value]) -> crate::Result<Value> {
    // AR.17.2 seam, refused LOUDLY in both directions: a closure crossing the fallback boundary
    // would carry a `closure_id` into a foreign table — inbound (`reduce`'s `func` at depth) the
    // throwaway ctx would invoke the wrong entry, outbound (`f_1arg`'s returned literal) the real
    // ctx would. Loud beats silently-wrong; the real fix — routing depth-declines through `fx`
    // so the reference interprets in the LIVE evaluator — is AR.24.
    if args.iter().any(carries_closure) {
        return Err(crate::Error::Unimplemented(
            "a depth-declined native was handed a closure — it cannot cross into the fallback evaluator (AR.24)",
        ));
    }
    let key = (sources.as_ptr() as usize, sources.len());
    let program = FALLBACK.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some((_, p)) = cache.iter().find(|(k, _)| *k == key) {
            p.clone()
        } else {
            let parsed = crate::parse(sources).ok().map(Rc::new);
            cache.push((key, parsed.clone()));
            parsed
        }
    });
    match program {
        Some(p) => {
            let out = crate::eval::interpret_fn_pure(&p, name, args)?;
            if carries_closure(&out) {
                return Err(crate::Error::Unimplemented(
                    "a depth-declined native produced a closure — it cannot cross out of the fallback evaluator (AR.24)",
                ));
            }
            Ok(out)
        }
        None => Err(super::bosl_assert(
            "generated fallback sources failed to parse",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test harness: expect IS the assertion")]
mod tests {
    use super::{DepthGuard, MAX_NATIVE_DEPTH};

    /// The budget holds exactly: `MAX` nested enters succeed, the next declines, and dropping
    /// gives levels back (the RAII contract early returns rely on).
    #[test]
    fn the_depth_budget_holds_and_returns() {
        let mut held = Vec::new();
        for _ in 0..MAX_NATIVE_DEPTH {
            held.push(DepthGuard::enter().expect("within budget"));
        }
        assert!(
            DepthGuard::enter().is_none(),
            "past the budget must decline"
        );
        held.pop();
        let again = DepthGuard::enter();
        assert!(again.is_some(), "a dropped guard returns its level");
        drop(again);
        drop(held);
        assert!(DepthGuard::enter().is_some(), "all levels returned");
    }

    /// AR.17.2 — closures REFUSE the fallback boundary in BOTH directions, loudly: a
    /// `closure_id` is only meaningful in the evaluator that minted it, so inbound (a closure
    /// argument at depth) the throwaway ctx would invoke the wrong table entry and outbound (a
    /// literal-returning native like `f_1arg`) the real ctx would. Silent corruption either
    /// way; the refusal is the interim seam until AR.24 routes declines through `fx`.
    #[test]
    fn closures_refuse_the_fallback_boundary() {
        use crate::eval::value::Value;
        let f = Value::Function {
            closure_id: 0,
            env: crate::eval::scope::Scope::new(),
            self_name: None,
            repr: "function() 1".into(),
            group: None,
            bound_this: None,
        };
        // inbound: a closure argument, bare and nested in a list.
        for arg in [f.clone(), Value::list(vec![Value::Num(1.0), f.clone()])] {
            let out = super::run_interpreted("function t(x) = 1;", "t", &[arg]);
            assert!(out.is_err(), "a closure argument must refuse the boundary");
        }
        // outbound: the interpreted reference RETURNS a literal.
        let out = super::run_interpreted("function t() = function(x) x;", "t", &[]);
        assert!(out.is_err(), "a returned closure must refuse the boundary");
        // and a plain value still flows.
        let out = super::run_interpreted("function u() = 41 + 1;", "u", &[]);
        assert_eq!(
            out.expect("closure-free fallback still answers"),
            Value::Num(42.0)
        );
    }

    /// Two distinct islands on ONE thread each interpret their OWN program — the cache keys on
    /// the sources' identity. A first-comer-wins cache (the reviewed hazard) would resolve island
    /// B's call against island A's program and fail it as an unknown function.
    #[test]
    fn the_fallback_cache_keys_on_the_island() {
        use crate::eval::value::Value;
        const A: &str = "function only_in_a() = 1;";
        const B: &str = "function only_in_b() = 2;";
        let a = super::run_interpreted(A, "only_in_a", &[]).expect("island A resolves");
        let b = super::run_interpreted(B, "only_in_b", &[])
            .expect("island B resolves on the same thread");
        assert!(matches!(a, Value::Num(x) if x.to_bits() == 1.0f64.to_bits()));
        assert!(matches!(b, Value::Num(x) if x.to_bits() == 2.0f64.to_bits()));
    }
}
