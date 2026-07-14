//! fab-manifold — the geometry kernel in Rust.
//!
//! A pure-Rust reimplementation of Manifold's robustness core (boolean3 / boolean_result / face_op /
//! edge_op / impl / polygon), deterministic bit-for-bit native == wasm — the thing a C++ binding
//! structurally cannot give us. Scope, phasing, and the two pillars (deterministic parallelism +
//! portable libm math) live in `SPEC_manifold-rs.md`.
//!
//! Built TEST-FIRST: the differential + invariant oracle ([`check`], and the `oracle` feature's
//! `KernelDriver`) gates every phase before the geometry that feeds it exists. Nothing here is load-
//! bearing yet — Phase M.0 is the crate skeleton + the oracle harness + the mesh spine, NO booleans.

// The whole point of the port is to LEAVE C++'s unsafety behind — this crate carries none of its own.
// (manifold3d, the differential oracle, has its own unsafe; that's a dep, not us, and it's off by
// default and gone at R.X.)
#![forbid(unsafe_code)]

pub mod mathf; // Pillar 2: the libm seam — every transcendental routes through here.
pub mod mesh; // the halfedge `Impl` (the mesh spine); MeshGL <-> Impl, IsManifold.
pub mod boolean; // the robustness core: boolean3 + boolean_result + face_op + edge_op.
pub mod polygon; // the robust 2D triangulator the boolean reassembly leans on.
pub mod check; // Oracle B: the manifold-invariant checker (test.h port), reference-free.
pub mod par; // Pillar 1: the deterministic parallel seam (serial when `par` is off).

/// The C++ Manifold differential oracle (Oracle A) — a `KernelDriver` trait with a Rust and a C++
/// backend, plus the triangulation-independent boolean-residual metric. Scaffolds R0..R.X; gone at
/// R.X. Native-only (needs the C++ toolchain), so it's behind BOTH the feature and a non-wasm cfg.
#[cfg(all(feature = "oracle", not(target_arch = "wasm32")))]
pub mod oracle;
