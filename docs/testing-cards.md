# Testing cards — the deck and the receipts

**The point of playing EVERY testing card on one codebase is the receipts.** Each card catches a
bug class the others structurally can't — so when a real bug lands, log WHICH card caught it and
why the rest missed. That ledger (below) is the asset; what it buys is at the bottom. scad-rs is
deliberately the dry run: FeOphant inherits this whole playbook when I go back
(differential-vs-Postgres is the same two-driver card as differential-vs-openscad).

## The deck

| Card | What it proves | What it structurally can't see | Status |
|---|---|---|---|
| Example tests @ 100% coverage | Known semantics pinned. Every divergence found later lands NEXT to an existing test — an assert edit, not a coverage gap | Truth of the expectations themselves (coverage measures execution, not correctness) | Live — the floor since day 1 |
| Property tests (proptest) | Invariants over generated inputs — fast==slow BITWISE is the flagship | Properties I didn't think to state | Live |
| cargo-fuzz | Input-space corners no hand-written case reaches (panics, OOM, parser crashes) | Wrong-but-calm answers — no oracle, it only sees crashes | Live |
| Differential vs the openscad oracle | The SPEC itself: real OpenSCAD's behavior, echo string-equal + mesh via Manifold XOR → residual volume (order-insensitive, so float sequencing noise degrades into sliver volume instead of a binary fail) | Places the oracle itself is wrong or undefined | Live — two-driver harness landed 04b8f1d; corpus tiers + the ChaCha8 grammar-directed generator are Phase K |
| cargo-mutants | The tests CATCH bugs, not just execute lines (kills tests that run everything and assert nothing) | Bug classes outside the mutation operators | Backlog — wire at the H.5 / I test phases |
| Kani proofs | Bounded state space CLOSED on the small kernels: push/pop discipline, range-iteration termination, indices in bounds — panic-freedom on the exact loop that runs untrusted SCAD | Anything above the bounded kernel (the whole evaluator is too big a state space) | Live — I.7: 9 proofs in `cargo kani` (range termination, stack pop-N, dot + sphere/cylinder/fan index bounds, guarded shift). A workflow panic-audit swept the eval first: zero unguarded panic sites |
| Tri-OS CI matrix | The determinism doctrine's "bit-identical, every platform" claim — cross-OS float-order/hasher divergence surfaces as a mismatch | It's a proof harness for the other cards, not a bug-finder itself | Backlog — fab-lang lane first |

The Backlog owns the wiring details for the unplayed cards; this doc owns the why.

## The receipts ledger

When a real bug is caught, one row. The honest record of which card earned its keep:

| Date | Bug (one line) | Caught by | Why the other cards missed it |
|---|---|---|---|
| | | | |

## What the receipts buy

1. **The blog series.** Each row is a post beat: a real bug + the one card that caught it + why
   the rest couldn't. Feeds the formal-methods outline already drafted in career-portfolio — the
   receipts make the argument so I don't have to.
2. **The browser-safe claim, upgraded.** With Kani proofs on the VM kernels, "browser-safe" stops
   meaning "it's in a WASM sandbox" and becomes "the loop executing untrusted SCAD is PROVEN
   panic-free" — a claim no other web playground can print.
3. **Interview evidence.** Kani-on-production-Rust is AWS's public engineering identity (ARG). A
   receipts ledger beats "familiar with formal methods" as an artifact.
4. **The FeOphant playbook.** A database wants exactly this deck (differential vs Postgres,
   property tests on the storage engine, Kani on page/buffer arithmetic). Playing every card here
   first means FeOphant restarts with a proven harness design instead of a theory.

## Why Kani and not Z3 (the recon-gen scar)

I tried Z3 with recon-gen; Kani's tutorial is WAY more approachable. The reason isn't sugar — Z3
approaches from the formal-math side: you re-state your problem in ITS language (sorts,
quantifiers, SMT-LIB), which means a steep on-ramp AND the hand translation is itself a bug
surface — you end up proving the MODEL, not the code, with full confidence. Kani approaches from
the programming-testing side: a proof harness looks like a unit test (`#[kani::proof]` plus the
asserts you already write) and it compiles the SHIPPED Rust (MIR → CBMC → SAT), so there's no
model to drift. The formal math is still underneath — someone else just wrote the translation
layer once, correctly.

The on-ramp is a gradient, not a cliff: an example test pins ONE input, proptest samples MANY,
`kani::any()` quantifies over ALL of them (bounded). Same harness shape at every rung, only the
quantifier gets stronger — which is exactly why it feels learnable coming from testing.
