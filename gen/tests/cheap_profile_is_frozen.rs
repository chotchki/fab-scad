//! AO.1 — the `cheap` generator profile is FROZEN, byte-for-byte.
//!
//! AO splits the generator's bounds into profiles so a `heavy` lane can emit geometry-dominated programs
//! worth timing. `cheap` is the one that must not move: it feeds the `gen_diff`/`jit_dispatch_diff` fuzz
//! corpora, the AJ.1 grammar-coverage gate, and `gen_ab`'s config bit-identity check. If a seed starts
//! producing different text, every one of those silently changes what it is testing — nothing goes red,
//! the corpora just drift onto a different program space.
//!
//! A whole-corpus FNV hash rather than a handful of golden strings: 2000 seeds is enough to catch a
//! probability shift in any single grammar arm, which spot-checking a few programs would sail past. The
//! constant was captured from the generator BEFORE the profile refactor and must survive it.
//!
//! If this fails after an INTENTIONAL grammar change, re-capture it — but re-capturing is a decision
//! about the fuzz corpora, not a formality: the seeded programs those campaigns have been exploring stop
//! being the programs they explore next.

/// Seeds hashed. Enough that a shifted probability in one arm moves the digest.
const SEEDS: u32 = 2000;

/// FNV-1a over every seed's program text, in seed order.
fn corpus_digest() -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for seed in 0..SEEDS {
        for b in fab_gen::generate(seed).bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

#[test]
fn the_cheap_corpus_has_not_moved() {
    assert_eq!(
        corpus_digest(),
        0x52ae_3841_853d_9513,
        "the `cheap` profile's output changed — every gen_diff / jit_dispatch_diff corpus entry and the \
         AJ.1 coverage gate now describe a different program space. Intentional? re-capture. Otherwise \
         the profile refactor leaked."
    );
}

#[test]
fn generation_is_deterministic_within_a_run() {
    // The freeze above is worthless if a seed isn't stable to begin with — this separates "the corpus
    // moved" from "the generator is nondeterministic", which would otherwise look identical.
    for seed in [0u32, 1, 42, 1999] {
        assert_eq!(fab_gen::generate(seed), fab_gen::generate(seed), "seed {seed}");
    }
}
