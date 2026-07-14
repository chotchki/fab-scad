//! Pillar 1 — the deterministic parallel seam.
//!
//! The ONLY parallelism door in the crate (rayon is clippy-banned everywhere else) — a thin wrapper over
//! Manifold's `parallel.h` primitive set, chosen so determinism is achievable BY CONSTRUCTION:
//! disjoint-write ops → indexed collect (deterministic free); reductions → type-gated by a
//! `CommutativeAssociative` marker so a non-associative float reduce WON'T COMPILE (float-add that feeds
//! geometry goes through a fixed-order serial Kahan path); scans → fixed-block-size (block = f(n), not
//! thread count), integer operands; sorts → total-order comparators only. Result: native-Par ==
//! native-Seq == wasm, bit-for-bit.
//!
//! With the `par` feature OFF (the default, and the wasm-safe path) every primitive is a plain serial
//! loop — bit-identical to the parallel path by construction, which is why serial-wasm can ship long
//! before threaded-wasm's nightly `-Zbuild-std` + `+atomics`. Swaps in for the serial reference at R4.
//!
//! TODO(M.0.7 spike, M.4 real).
