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

/// The native tier's recursion allowance — kin to the parser's `MAX_DEPTH = 64` doctrine. Frames
/// here are HEAVY (locals + accumulator `Vec`s per level), so the budget is deliberately small;
/// everything past it runs on the interpreter's explicit stack.
pub(super) const MAX_NATIVE_DEPTH: u32 = 64;

/// One parsed island per distinct `FALLBACK_SOURCES` identity — keyed by the `&'static str`'s
/// (ptr, len), `None` for an island whose parse failed (cached so it doesn't re-parse per decline).
type IslandCache = Vec<((usize, usize), Option<Rc<crate::Program>>)>;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
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
                d.set(d.get() + 1);
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
/// parse, which the caller's own codegen already ruled out.
pub fn run_interpreted(sources: &'static str, name: &str, args: &[Value]) -> crate::Result<Value> {
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
        Some(p) => crate::eval::interpret_fn_pure(&p, name, args),
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
