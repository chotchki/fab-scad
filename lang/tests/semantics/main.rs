//! # `semantics/` — the executable OpenSCAD-conformance spec (segmented, provenance-annotated)
//!
//! Each segment pins one area of OpenSCAD's OBSERVABLE semantics as executable FACTS, each annotated
//! with (a) the behavior and (b) the `src/core` PROVENANCE it was translated from. The intent: this
//! suite reads as the OpenSCAD language spec (which doesn't exist upstream — writing it IS the
//! reimplementation), not merely as a coverage driver. Every test is a citable fact.
//!
//! This is the FIRST landing (G.3.8, from the G.3.5 geometry port). K.2 formalizes the naming +
//! provenance conventions and migrates the `*_corpus` suites in; until then this establishes the
//! shape:
//!   - a module per semantic area, `//! Provenance:` (OpenSCAD source) + `//! Oracle:` (how it was
//!     verified against the real oracle) in the module header,
//!   - `/// FACT:` on each test, stating the behavior in prose before asserting it.
//!
//! Segments verify through the PUBLIC API only (`evaluate`, `fragments`) — the internal port
//! (`trig`, tessellation loops) has its own unit tests; here we pin what an OpenSCAD user observes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "executable-spec tests: unwrap/expect ARE the assertions; exact geometry asserts are deterministic"
)]

mod fragment;
mod tessellation;
mod trig;
