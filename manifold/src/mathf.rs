//! Pillar 2 — the libm math seam.
//!
//! Every transcendental (sin/cos/tan/asin/acos/atan/atan2/exp/ln/pow) routes through here and calls the
//! pure-Rust `libm` crate, so results are bit-identical native == wasm — platform `f64::sin`-etc are
//! 1-ULP-divergent AND cross-platform nondeterministic, so they're clippy-banned (`clippy.toml`) to
//! force everything through this module. Degree-trig gets exact-quadrant SNAPPING (`sin(180°)==0`,
//! `acos(-0.5)==120`) so the kernel and fab-lang speak ONE math dialect. `sqrt`/`floor`/`ceil`/`round`/
//! `trunc` stay hardware `f64::` (IEEE-exact — routing them through libm buys nothing).
//!
//! The exact geometric predicates (SignedArea centered-shoelace, operation-dependent perturbation) also
//! live near here — pure f64, NO exact-arithmetic crate, NO `mul_add` (FMA is the #1 determinism
//! hazard). See SPEC_manifold-rs.md Pillar 2.
//!
//! TODO(M.0.2).
