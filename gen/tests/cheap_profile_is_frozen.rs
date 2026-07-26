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

/// The dial has to reach the MESH, not just the program text.
///
/// AO.12 measured a flat performance curve and the cause was here: `max_fn` was the documented "main
/// dial on curved geometry cost", but nothing put it on a primitive. The only `$fn` was a program-level
/// statement that fired in 39% of seeds and drew UNIFORM FROM ZERO, so its mean was half the cap and
/// the other 61% of programs tessellated at `$fa`/`$fs` defaults at every dial. Raising the knob moved
/// a number that mostly never reached geometry, and every test passed the whole time because they all
/// ran `cheap`.
///
/// So this asserts the MECHANISM, not the wall clock: heavy seeds must put `$fn` on curved primitives,
/// and those values must sit in the top half of the dial's cap.
///
/// MINKOWSKI operands are excluded on purpose. Their `$fn` is PINNED at `minkowski_fn` on every dial
/// (AO.3) because minkowski cost is multiplicative in its operands' vertex counts — the one place the
/// dial deliberately must NOT reach. A test that demanded a floor there would be demanding the blowup
/// this profile exists to prevent.
#[test]
fn the_dial_lands_on_the_primitives() {
    for dial in [1u32, 4, 16] {
        let profile = fab_gen::Profile::heavy(dial);
        let cap = profile.max_fn;
        let (mut curved, mut with_fn) = (0usize, 0usize);
        for seed in 0..200u32 {
            let src = fab_gen::generate_with(seed, profile);
            for line in src.split([';', '\n']) {
                if line.contains("minkowski") || line.contains("r = 1, $fn = 6") {
                    continue; // pinned by AO.3 — see the doc comment
                }
                if !(line.contains("sphere(")
                    || line.contains("cylinder(")
                    || line.contains("circle("))
                {
                    continue;
                }
                curved += 1;
                let Some(rest) = line.split("$fn = ").nth(1) else {
                    continue;
                };
                let val: i64 = rest
                    .trim_matches(|c: char| !c.is_ascii_digit())
                    .parse()
                    .unwrap_or(-1);
                assert!(
                    val >= cap / 2 && val <= cap,
                    "dial {dial}: $fn {val} outside the top half of cap {cap} — the dial is meant to \
                     be a FLOOR on facets, not an average of one: {line}"
                );
                with_fn += 1;
            }
        }
        assert!(
            curved > 0,
            "dial {dial}: no curved primitives generated at all"
        );
        assert_eq!(
            curved,
            with_fn,
            "dial {dial}: {} of {curved} curved primitives carry no $fn — those tessellate at the \
             $fa/$fs defaults however high the dial goes, which IS the AO.12 plateau",
            curved - with_fn
        );
    }
}

/// `cheap` must not grow a per-primitive `$fn`: `gen-diff`'s corpus is frozen, and the flag gates an
/// RNG DRAW as well as the emitted text, so flipping it would shift every subsequent value.
///
/// A CONST assertion, not a test — the field is a `const`, so this fails the BUILD rather than a run,
/// and no one can flip it and discover the consequence from a red digest later.
/// `the_cheap_corpus_has_not_moved` still guards the bytes; this names the knob.
const _: () = assert!(!fab_gen::Profile::CHEAP.prim_fn);

/// Same freeze, same reasoning, for AR.4's knob: `domains` gates RNG draws as well as text, so ON
/// in `cheap` would shift every subsequent value in the stream.
const _: () = assert!(!fab_gen::Profile::CHEAP.domains);

/// Heavy must keep `domains` ON at every dial — that lane's numbers are only meaningful while its
/// calls COMPUTE (`every_declared_call_computes`); off, it regresses to timing error handling.
#[test]
fn every_dial_generates_domain_typed_calls() {
    for dial in 1..=8u32 {
        assert!(fab_gen::Profile::heavy(dial).domains, "dial {dial}");
    }
}

/// The builtin surface's invariants (AR.3), each for a reason that has already bitten something.
///
/// LENGTH is frozen because `pick_builtin` indexes the table with the RNG: adding or removing an
/// entry moves every subsequent draw and rewrites the whole `cheap` corpus. The digest test would
/// catch that, but as a hash — this says WHICH change did it.
///
/// `names_bind` is FALSE for every builtin, and that is a verified fact about upstream rather than a
/// convention: `pow(exp=3, base=2)` is 9, not 8, because the names are discarded and the arguments
/// bind positionally (`builtin_argument_names_are_ignored` in fab-lang pins the behaviour). A future
/// entry flipping this to true would tell a call generator that named builtin calls exercise the
/// AN.14 diagnostic family, which they cannot.
#[test]
fn the_builtin_surface_holds_its_shape() {
    let decls = fab_gen::builtins();
    assert_eq!(
        decls.len(),
        33,
        "the builtin table is RNG-indexed, so its length is frozen — changing it moves every \
         subsequent draw and rewrites the cheap corpus"
    );
    for d in decls {
        assert!(
            !d.names_bind,
            "{}: builtin argument names do NOT bind upstream (pow(exp=3, base=2) == 9)",
            d.name
        );
        assert!(
            d.arity() > 0,
            "{}: a zero-arity entry would generate `f()`, which the corpus has never contained",
            d.name
        );
        assert_eq!(
            d.arity(),
            d.params.len(),
            "{}: arity is params.len()",
            d.name
        );
    }
}
