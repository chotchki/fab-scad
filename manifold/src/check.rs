//! Oracle B — the manifold-invariant checker (a port of Manifold's `test.h`).
//!
//! Reference-free structural gates that survive the C++ oracle's removal at R.X: `strictly` (manifold,
//! no self-intersection), `finite` (no NaN/inf verts), `euler` (V−E+F genus consistency), `related`
//! (property/color provenance survives a boolean). A `KernelParams { intermediate_checks }` flag runs
//! them after EVERY internal op in test/fuzz builds (off in release) — Manifold's `intermediateChecks`
//! trick that catches a corruption at the op that caused it, not three ops later. The circularity ("is
//! Volume trustworthy enough to assert Volume==12") is broken by Gate K.0 calibrating against the C++
//! oracle on identical buffers first.
//!
//! TODO(M.0.4).
