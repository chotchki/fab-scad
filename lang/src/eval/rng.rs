//! `rands()` bug-for-bug — a boost-compatible MT19937 + `uniform_real_distribution`.
//!
//! OpenSCAD's `rands(min, max, count, [seed])` draws from `boost::random::mt19937` fed to
//! `boost::random::uniform_real_distribution<double>`. To match its output EXACTLY (verified vs the
//! 2026.06.12 oracle: `rands(0,1,3,seed=42)` = `[0.796543, 0.183435, 0.779691]` to every printed digit), two
//! pieces must be the reference algorithm, not a look-alike:
//!   - SEEDING is `init_genrand` (the 2002 linear recurrence, `mt[i] = 1812433253·(mt[i-1] ^ mt[i-1]>>30) +
//!     i`) — what `boost::mt19937::seed(uint32)` does. NOT `init_by_array` (the CPython/numpy seeding most
//!     MT crates expose), which gives a different stream.
//!   - the [0,1) draw is boost's `generate_canonical`: two 32-bit words combined as `(e0 + e1·2³²) / 2⁶⁴`.
//!     NOT the reference `genrand_res53` (`(a·2²⁶ + b)/2⁵³`), which most MT crates ship.
//!
//! Why bug-for-bug MATTERS here even though BOSL2's rands tests only assert geometric INVARIANTS (a random
//! plane contains its defining line, three random points form a valid triangle): the seeds were chosen so
//! OpenSCAD's SPECIFIC values dodge degeneracy (collinear points, zero-area). A different stream could land on
//! a degenerate case the seed was picked to avoid — so matching the stream is the safe path.
//!
//! Determinism doctrine: integer state + the fixed float formula → bit-identical every platform. Non-seeded
//! `rands` is non-deterministic in OpenSCAD (time-seeded); we use a FIXED default seed instead — the tests
//! that omit a seed assert invariants that hold for ANY random input, so a deterministic stand-in is correct
//! AND keeps us reproducible.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER: u32 = 0x8000_0000;
const LOWER: u32 = 0x7fff_ffff;

/// The default seed for a `rands` call with no explicit seed — a fixed constant so our output stays
/// deterministic (OpenSCAD would time-seed here; see the module note on why a stand-in is correct).
const DEFAULT_SEED: u32 = 0;

/// A reference MT19937 seeded boost's way (`init_genrand`). Holds the 624-word state + read index.
struct Mt19937 {
    mt: [u32; N],
    mti: usize,
}

impl Mt19937 {
    /// Seed with `init_genrand(seed)` — the boost `mt19937::seed(uint32)` recurrence.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the loop index is < N (624), so `i as u32` never truncates"
    )]
    fn new(seed: u32) -> Self {
        let mut mt = [0u32; N];
        mt[0] = seed;
        for i in 1..N {
            mt[i] = 1_812_433_253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Mt19937 { mt, mti: N }
    }

    /// The next 32-bit output (generate + temper), regenerating the block when exhausted.
    fn next_u32(&mut self) -> u32 {
        if self.mti >= N {
            for i in 0..N {
                let y = (self.mt[i] & UPPER) | (self.mt[(i + 1) % N] & LOWER);
                self.mt[i] =
                    self.mt[(i + M) % N] ^ (y >> 1) ^ (if y & 1 != 0 { MATRIX_A } else { 0 });
            }
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// A double in `[0, 1)` — boost's `generate_canonical<double>`: two words as `(e0 + e1·2³²) / 2⁶⁴`.
    fn canonical(&mut self) -> f64 {
        let e0 = f64::from(self.next_u32());
        let e1 = f64::from(self.next_u32());
        (e0 + e1 * 4_294_967_296.0) / 18_446_744_073_709_551_616.0
    }
}

/// `count` draws uniformly in `[min, max)` from a boost-compatible MT19937 seeded with `seed` (or
/// [`DEFAULT_SEED`] when `None`). The stream matches OpenSCAD's `rands()` word-for-word. This is the
/// SEEDED path — a fresh engine per call, so `rands(…, seed=k)` is a pure function of its args.
#[must_use]
pub fn rands(min: f64, max: f64, count: usize, seed: Option<u32>) -> Vec<f64> {
    let mut rng = Mt19937::new(seed.unwrap_or(DEFAULT_SEED));
    let span = max - min;
    (0..count).map(|_| min + span * rng.canonical()).collect()
}

/// A LIVE MT19937 for SEEDLESS `rands`. OpenSCAD draws every seedless call from ONE global engine, so
/// consecutive `rands()` calls DIFFER (`rands(-1,1,3)` twice gives two distinct points — BOSL2 leans on
/// this to build a non-degenerate random line/triangle). A fresh engine per call (what the seeded path
/// does) would return the SAME values every time and collapse those to a degenerate case. So the
/// evaluator holds ONE stream, seeded once with [`DEFAULT_SEED`] and ADVANCED per seedless call. It's
/// reset per evaluation (→ reproducible, bit-identical) but eval-order-STATEFUL within a run — the one
/// deliberately-impure builtin (seedless `rands` is non-reproducible in OpenSCAD anyway). Any future
/// parallelism must draw in the fixed eval order, same as the buffered echo/warning log.
pub struct RandStream {
    rng: Mt19937,
    /// Monotonic count of values DRAWN — the impurity signal the eval-memo cache (N.2c) reads. A user
    /// function whose evaluation advances this is NOT a pure function of its args (seedless `rands` depends
    /// on stream position), so it can't be memoized; the cache snapshots this before/after a call and only
    /// caches when it's unchanged. Never reset except by `new()` (a fresh per-eval stream starts at 0).
    draws: u64,
}

impl Default for RandStream {
    /// A fresh stream at the fixed default seed — lets `Ctx` keep its derived `Default`.
    fn default() -> Self {
        Self::new()
    }
}

impl RandStream {
    /// A fresh stream at the fixed default seed — one per evaluation.
    pub(crate) fn new() -> Self {
        Self {
            rng: Mt19937::new(DEFAULT_SEED),
            draws: 0,
        }
    }

    /// ONE draw in `[min, max)`, ADVANCING the stream (bumps `draws` by 1). The single source of truth for a
    /// seedless draw — both the batch [`RandStream::draw`] and the JIT's [`jit_rand_next`] helper route through
    /// it, so the draw SEQUENCE and the `draws` fence counter are bit-identical whether a `rands()` runs
    /// interpreted or JIT'd (P.1.6 rung-D piece 1). Recomputing `max - min` per draw is deterministic, so it
    /// equals the old batch's compute-span-once form value-for-value. `pub` so the native JIT's `jit_rand_next`
    /// helper (in `fab-jit`, where the `unsafe` deref lives — this crate is `unsafe_code = forbid`) can advance
    /// the woven stream through the SAME primitive.
    pub fn next_one(&mut self, min: f64, max: f64) -> f64 {
        self.draws += 1;
        min + (max - min) * self.rng.canonical()
    }

    /// `count` draws in `[min, max)`, ADVANCING the stream so the next call continues the sequence.
    pub(crate) fn draw(&mut self, min: f64, max: f64, count: usize) -> Vec<f64> {
        (0..count).map(|_| self.next_one(min, max)).collect()
    }

    /// The monotonic draw count — the cache's impurity probe (see the `draws` field).
    #[allow(dead_code, reason = "consumed by the N.2c eval-cache purity fence, landing next")]
    pub(crate) fn draws(&self) -> u64 {
        self.draws
    }
}


#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "rands is byte-exact vs the oracle — exact float asserts ARE the bug-for-bug proof"
)]
mod tests {
    use super::rands;

    /// Byte-exact vs the OpenSCAD 2026.06.12 oracle (echo prints 6 sig figs; we match every one).
    #[test]
    fn matches_the_oracle() {
        let a = rands(0.0, 1.0, 3, Some(42));
        assert_eq!(format!("{:.6} {:.6} {:.6}", a[0], a[1], a[2]), "0.796543 0.183435 0.779691");
        let b = rands(0.0, 10.0, 4, Some(42));
        assert_eq!(
            format!("{:.5} {:.5} {:.5} {:.5}", b[0], b[1], b[2], b[3]),
            "7.96543 1.83435 7.79691 5.96850"
        );
        let c = rands(-1.0, 1.0, 3, Some(3));
        assert_eq!(format!("{:.6} {:.6} {:.6}", c[0], c[1], c[2]), "-0.858550 0.679898 -0.757343");
    }

    #[test]
    fn count_and_range() {
        let v = rands(2.0, 5.0, 100, Some(7));
        assert_eq!(v.len(), 100);
        assert!(v.iter().all(|&x| (2.0..5.0).contains(&x)));
        // deterministic: same seed → same stream
        assert_eq!(rands(0.0, 1.0, 5, Some(1)), rands(0.0, 1.0, 5, Some(1)));
    }
}
