//! The robust 2D polygon triangulator (Manifold's `polygon.cpp`) the boolean reassembly leans on.
//!
//! Retriangulates the cut faces boolean_result produces — the ear-clipping/monotone-decomposition that
//! must stay watertight through slivers and near-degenerate polygons. Uses the centered-shoelace
//! `SignedArea` ([`crate::mathf`]) to kill catastrophic cancellation. Gated in test by
//! area-conservation and edge-coverage invariants (strengthened PAST Manifold's tri-count-only checks),
//! fuzzed by `polygon_fuzz`. It's also the shared triangulator the 2D subsystem reuses (R5).
//!
//! TODO(M.1.1).
