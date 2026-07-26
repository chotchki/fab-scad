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
        assert_eq!(
            fab_gen::generate(seed),
            fab_gen::generate(seed),
            "seed {seed}"
        );
    }
}

// ─────────────────────── the dial actually dials (AO.1/AO.3) ───────────────────────
//
// `heavy` was unverified code until it was MEASURED: the freeze test above only guards `cheap`, and the
// parse + coverage tests only ever run `cheap`. So `Profile::heavy` compiled, read plausibly, and emitted
// LIGHTER programs than `cheap` at dial 1 — `max_stmts: 6 * d` gives 6, under cheap's 9 — with nothing red.
// These pin the two properties the scaling curve depends on: every dial is at least cheap, and cost grows.

/// Bytes emitted across a fixed seed span — a proxy for how much program a profile produces.
fn corpus_bytes(profile: fab_gen::Profile) -> usize {
    (0..60u32)
        .map(|seed| fab_gen::generate_with(seed, profile).len())
        .sum()
}

#[test]
fn every_dial_is_at_least_as_heavy_as_cheap() {
    let cheap = corpus_bytes(fab_gen::Profile::CHEAP);
    for dial in 1..=8u32 {
        let heavy = corpus_bytes(fab_gen::Profile::heavy(dial));
        assert!(
            heavy >= cheap,
            "heavy({dial}) emitted {heavy} bytes vs cheap's {cheap} — a dial step that makes programs \
             SMALLER makes the scaling curve read backwards"
        );
    }
}

#[test]
fn the_dial_grows_monotonically() {
    let mut last = corpus_bytes(fab_gen::Profile::heavy(1));
    for dial in 2..=8u32 {
        let now = corpus_bytes(fab_gen::Profile::heavy(dial));
        assert!(
            now > last,
            "heavy({dial}) = {now} bytes is not above heavy({}) = {last}; the curve's x-axis has to be \
             ordered or the plot means nothing",
            dial - 1
        );
        last = now;
    }
}

#[test]
fn the_multiplicative_knob_never_moves() {
    // Minkowski cost is multiplicative in operand vertex counts, so it is pinned at EVERY dial (AO.3).
    // Asserted rather than trusted: the obvious refactor when raising `$fn` globally is to let this
    // follow, and one seed with minkowski at a high facet count can eat the whole nightly budget.
    for dial in 1..=8u32 {
        assert_eq!(
            fab_gen::Profile::heavy(dial).minkowski_fn,
            fab_gen::Profile::CHEAP.minkowski_fn,
            "minkowski operands must not scale (dial {dial})"
        );
    }
    // The depth cap is exponential in leaves (~3^depth), so it stays bounded however far the dial goes.
    assert_eq!(fab_gen::Profile::heavy(64).max_geo_depth, 6);
}
