//! Pillar 1 — the deterministic parallel seam.
//!
//! The ONLY parallelism door in the crate (rayon is clippy-banned everywhere else) — a thin wrapper
//! over the Manifold `parallel.h` primitive set, chosen so determinism is achievable BY CONSTRUCTION:
//! disjoint-write ops → indexed collect (deterministic free); reductions → type-gated by a
//! [`CommutativeAssociative`] marker so a non-associative float reduce WON'T COMPILE (float-add that
//! feeds geometry goes through a fixed-order serial Kahan path); sorts → total-order comparators only.
//! Result: native-Par == native-Seq == wasm, bit-for-bit.
//!
//! With the `par` feature OFF (the default) every primitive is a plain serial loop — bit-identical
//! to the parallel path by construction (that's the whole point of the marker). With `par` ON the
//! rayon path is live wherever `par_live` holds (build.rs) AND the batch beats `SEQ_THRESHOLD`
//! (C++ `kSeqThreshold` parity — small batches stay serial, fork-join overhead isn't worth it):
//! native OS threads, and — since M.6.1 —
//! browser wasm (`wasm32-unknown-unknown`), where rayon runs over `wasm-bindgen-rayon`'s
//! Web-Worker + SharedArrayBuffer pool. That build is nightly `-Zbuild-std` + `+atomics`
//! (`scripts/wasm-par-check.sh` is the compile gate) and the APP owns the runtime discipline:
//! `await initThreadPool(...)` (the `init_thread_pool` re-export in lib.rs) BEFORE the first kernel
//! call, on a cross-origin-isolated page (COOP/COEP — kept alive at W.3.7.4 for exactly this).
//! `wasm32-wasip1` — the wasmtime differential lane — stays serial always. Because every parallel
//! primitive here is order-preserving or CA-gated, par-wasm == serial-wasm == native bit-for-bit by
//! the same construction K.D proved seq==par with; M.6 verifies the corpus on the serial lane and
//! the construction carries the threaded one.
//!
//! THREADED-WASM⟷BEVY COEXISTENCE (SPEC risk #5) remains the app-level question it always was:
//! whether the worker pool plays nicely with Bevy's wasm runtime is decided where the GUI turns
//! threads on (W phase), not here — the kernel only guarantees the pool it computes on.
//!
//! M.0.7 was the SPIKE: the seam + the marker + the compile-time proof, validated against the serial
//! reference. The real swap-in (replacing the kernel's serial loops) was M.4.

// `par_live` (build.rs): `par` AND (native OR browser-wasm) — see the module doc.
#[cfg(par_live)]
use rayon::prelude::*;

/// C++-parity sequential threshold (`parallel.h` `kSeqThreshold = 1e4`): at `len() <= SEQ_THRESHOLD`
/// every entry point takes the serial path even with `par` live — fork-join overhead dominates small
/// batches (`autoPolicy(size)` makes the same call, comparison `<=` matched exactly). Safe for bit
/// identity by construction: `map_collect` is order-preserving and `reduce` is CA-gated, so serial
/// and parallel already produce identical bytes — this only moves the crossover.
///
/// NOTE: C++ hot boolean loops (`boolean3.cpp` Intersect12/Winding03) use this default, but its
/// sort.cpp/face_op.cpp/impl.cpp sites pass a CUSTOM 1e5 — the seam's uniform 10k is *more* parallel
/// than C++ at those sites, not less.
#[cfg_attr(not(par_live), allow(dead_code))] // only read on the par_live path; doc-visible always
const SEQ_THRESHOLD: usize = 10_000;

/// A reduction's binary combine + identity, kept as a TYPE (not a closure) so the
/// [`CommutativeAssociative`] marker can gate it. `combine(identity, x) == x` must hold.
pub trait Reducer {
    /// The value being reduced.
    type Item: Copy;
    /// The identity element (`combine(identity, x) == x`).
    fn identity(&self) -> Self::Item;
    /// Fold two values together.
    fn combine(&self, a: Self::Item, b: Self::Item) -> Self::Item;
}

/// Marker: this reducer's `combine` is mathematically commutative AND associative, so a parallel
/// reduction — which regroups and reorders the folds — yields the SAME result as the serial left-fold.
///
/// This is the determinism gate. [`reduce`] REQUIRES it; a float-sum reducer must NOT implement it
/// (IEEE `+` is not associative — `(a+b)+c != a+(b+c)` in general), so the only way to get a
/// nondeterministic parallel float sum is to write a LYING impl, which is a visible, reviewable act.
/// The honest path for float sums is [`reduce_serial`] (fixed left-to-right order) — e.g. the Kahan
/// volume sum in [`crate::mesh`].
pub trait CommutativeAssociative: Reducer {}

/// Parallel-safe reduction. Because `R` is [`CommutativeAssociative`], the result is identical whether
/// this runs the rayon tree-reduce (native + `par`) or the serial fold (default / wasm) — determinism
/// by construction. For non-associative ops the type system routes you to [`reduce_serial`] instead.
///
/// The determinism gate is a COMPILE error, not a runtime check — a non-`CommutativeAssociative`
/// reducer can't be handed to `reduce`. If this example ever compiled, the marker would be toothless:
///
/// ```compile_fail
/// use fab_manifold::par::{reduce, NaiveSum};
/// // NaiveSum: Reducer but NOT CommutativeAssociative → `reduce`'s bound is unsatisfied.
/// let _ = reduce(&[1.0_f64, 2.0, 3.0], &NaiveSum);
/// ```
pub fn reduce<R>(items: &[R::Item], reducer: &R) -> R::Item
where
    R: CommutativeAssociative + Sync,
    R::Item: Send + Sync,
{
    #[cfg(par_live)]
    {
        if items.len() <= SEQ_THRESHOLD {
            return reduce_serial(items, reducer);
        }
        items
            .par_iter()
            .copied()
            .reduce(|| reducer.identity(), |a, b| reducer.combine(a, b))
    }
    #[cfg(not(par_live))]
    {
        reduce_serial(items, reducer)
    }
}

/// Always-serial, fixed left-to-right fold: `identity ⊕ x0 ⊕ x1 ⊕ …`. Accepts ANY [`Reducer`],
/// associative or not — the determinism comes from the FIXED order, so this is the correct home for
/// float sums (naive or Kahan) that must not be reordered.
pub fn reduce_serial<R: Reducer>(items: &[R::Item], reducer: &R) -> R::Item {
    let mut acc = reducer.identity();
    for &x in items {
        acc = reducer.combine(acc, x);
    }
    acc
}

/// Order-preserving parallel map → `Vec`. Output index `i` is always `f(&items[i])` regardless of
/// scheduling, so the result is deterministic without any associativity requirement.
pub fn map_collect<T, U, F>(items: &[T], f: F) -> Vec<U>
where
    T: Sync,
    U: Send,
    F: Fn(&T) -> U + Sync + Send,
{
    #[cfg(par_live)]
    {
        if items.len() <= SEQ_THRESHOLD {
            return items.iter().map(f).collect();
        }
        items.par_iter().map(f).collect()
    }
    #[cfg(not(par_live))]
    {
        items.iter().map(f).collect()
    }
}

/// Run `f` on each element for its side effects. SAFE for determinism only when the effects are
/// independent (disjoint writes / pure reads) — the seam can't enforce that, so callers own it.
pub fn for_each<T, F>(items: &[T], f: F)
where
    T: Sync,
    F: Fn(&T) + Sync + Send,
{
    #[cfg(par_live)]
    {
        if items.len() <= SEQ_THRESHOLD {
            items.iter().for_each(f);
            return;
        }
        items.par_iter().for_each(f);
    }
    #[cfg(not(par_live))]
    {
        items.iter().for_each(f);
    }
}

// -----------------------------------------------------------------------------
// Built-in reducers. min/max/union are commutative + associative (CA); naive float sum is NOT.
// -----------------------------------------------------------------------------

use crate::linalg::{Box3, Vec3};

/// Bounding-box reduction: fold point-boxes (or sub-boxes) into their union. Componentwise min/max is
/// commutative + associative ⇒ CA ⇒ parallel-safe. Identity is the inverted-infinity empty box.
pub struct BoxUnion;

impl Reducer for BoxUnion {
    type Item = Box3;
    fn identity(&self) -> Box3 {
        Box3::default()
    }
    fn combine(&self, a: Box3, b: Box3) -> Box3 {
        a.union(b)
    }
}
impl CommutativeAssociative for BoxUnion {}

/// A NAIVE float sum — a [`Reducer`] that is deliberately NOT [`CommutativeAssociative`] (IEEE `+`
/// isn't associative). It exists to (a) exercise [`reduce_serial`]'s any-op path and (b) prove the
/// compile-time gate: `reduce(&[..], &NaiveSum)` fails to type-check. See the module test.
pub struct NaiveSum;

impl Reducer for NaiveSum {
    type Item = f64;
    fn identity(&self) -> f64 {
        0.0
    }
    fn combine(&self, a: f64, b: f64) -> f64 {
        a + b
    }
}
// NOTE: intentionally NO `impl CommutativeAssociative for NaiveSum` — that omission is the safety net.

/// The bounding box of a point cloud, via the parallel-safe [`reduce`] over [`BoxUnion`]. Deterministic
/// on every target. (The kernel's own `calculate_bbox` keeps its NaN-skipping serial loop until M.4
/// swaps this in — this is the spike demonstrating the seam produces the same box.)
pub fn bbox_of(points: &[Vec3]) -> Box3 {
    let boxes = map_collect(points, |&p| Box3::from_points(p, p));
    reduce(&boxes, &BoxUnion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_union_reduce_matches_serial() {
        let pts: Vec<Vec3> = (0..1000)
            .map(|i| {
                let f = i as f64;
                Vec3::new(
                    (f * 1.3) % 7.0 - 3.0,
                    (f * 2.7) % 11.0 - 5.0,
                    (f * 0.7) % 5.0 - 2.0,
                )
            })
            .collect();
        let boxes = map_collect(&pts, |&p| Box3::from_points(p, p));
        // The CA reducer: parallel-capable `reduce` and the fixed-order `reduce_serial` agree — the
        // property that makes the parallel path safe to swap in at M.4.
        let par = reduce(&boxes, &BoxUnion);
        let ser = reduce_serial(&boxes, &BoxUnion);
        assert_eq!(par, ser);
    }

    #[test]
    fn bbox_of_matches_manual() {
        let pts = vec![
            Vec3::new(1.0, -2.0, 3.0),
            Vec3::new(-4.0, 5.0, 0.0),
            Vec3::new(2.0, 2.0, -1.0),
        ];
        let bb = bbox_of(&pts);
        assert_eq!(bb.min, Vec3::new(-4.0, -2.0, -1.0));
        assert_eq!(bb.max, Vec3::new(2.0, 5.0, 3.0));
    }

    #[test]
    fn map_collect_preserves_order() {
        let items: Vec<i32> = (0..500).collect();
        let doubled = map_collect(&items, |&x| x * 2);
        assert_eq!(doubled, (0..500).map(|x| x * 2).collect::<Vec<_>>());
    }

    #[test]
    fn naive_sum_serial_is_fixed_order() {
        // reduce_serial accepts the non-CA NaiveSum and folds left-to-right, deterministically.
        let xs = vec![0.1, 0.2, 0.3, 0.4];
        let s = reduce_serial(&xs, &NaiveSum);
        // exactly the left-fold ((((0+0.1)+0.2)+0.3)+0.4), whatever its last bit is.
        let mut acc = 0.0;
        for &x in &xs {
            acc += x;
        }
        assert_eq!(s.to_bits(), acc.to_bits());
    }

    #[test]
    fn map_collect_preserves_order_above_seq_threshold() {
        // 20k items > SEQ_THRESHOLD — under `par` this is the rayon path (the sub-threshold tests
        // above now exercise the serial early-out); either way the output must be index-exact.
        let items: Vec<i32> = (0..20_000).collect();
        let doubled = map_collect(&items, |&x| x * 2);
        assert_eq!(doubled, (0..20_000).map(|x| x * 2).collect::<Vec<_>>());
    }

    #[test]
    fn reduce_above_seq_threshold_matches_serial() {
        let boxes: Vec<Box3> = (0..20_000)
            .map(|i| {
                let f = i as f64;
                let p = Vec3::new(
                    (f * 1.3) % 7.0 - 3.0,
                    (f * 2.7) % 11.0 - 5.0,
                    (f * 0.7) % 5.0 - 2.0,
                );
                Box3::from_points(p, p)
            })
            .collect();
        assert_eq!(reduce(&boxes, &BoxUnion), reduce_serial(&boxes, &BoxUnion));
    }

    #[test]
    fn for_each_visits_every_element() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let sum = AtomicUsize::new(0);
        for_each(&[1usize, 2, 3, 4, 5], |&x| {
            sum.fetch_add(x, Ordering::Relaxed);
        });
        assert_eq!(sum.load(Ordering::Relaxed), 15);
    }
}
