//! Oracle A — the C++ Manifold differential harness.
//!
//! A `KernelDriver` trait with two backends — `RustKernel` (this crate) and `CppKernel` (`manifold3d`,
//! the linked C++ kernel fab-scad already ships) — run the SAME op through both and compare. The metric
//! is the G.3.7 boolean-residual: `vol((A−B) ∪ (B−A)) / vol(A) < 1e-5`, which is triangulation-
//! INDEPENDENT and therefore immune to exactly the C++ mesh-nondeterminism that motivates the port.
//! Backstop: vol/area (<1e-7 rel), genus (exact), bbox (exact), component count (exact). This is a
//! SCAFFOLD — it gates R0..R.X, then goes away at R.X when we freeze goldens and drop `manifold3d`.
//!
//! TODO(M.0.3).
