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

// the robustness core: boolean3 + boolean_result + face_op + edge_op.
pub mod boolean;
// Oracle B: the manifold-invariant checker (test.h port), reference-free.
pub mod check;
// R5 — the 2D `CrossSection` subsystem, over the i_overlay 2D boolean engine (area-residual gated, NOT
// bit-exact — the one layer where the verbatim thesis relaxes; SPEC [OPEN #4]).
pub mod cross_section;
// R5 — the 2D↔3D bridges (Extrude/Revolve/Project/Slice); unblocks M.3.8.
pub mod bridge;
// The internal linalg (linalg.h subset) — vec2/3/4, mat3x4, Box; op order matches the C++ oracle.
pub mod linalg;
// Pillar 2: the libm seam — every transcendental routes through here.
pub mod mathf;
// the halfedge `Impl` (the mesh spine); MeshGL <-> Impl, IsManifold.
pub mod mesh;
// Minkowski sum (`Manifold::MinkowskiSum`) — the tiered hull+union over M.3.6's convex hull.
pub mod minkowski;
// typed mesh indices (VertId/HalfedgeId/TriId) — the misuse-resistance layer over raw i32.
pub mod mesh_ids;
// 2D offset — the verbatim Clipper2 `ClipperOffset` polygon walk (M.5.4.1): join-corner geometry is
// engine-DEFINED, so K.6's OpenSCAD-area parity forced a port; i_overlay only finishes the union.
pub mod offset;
// Pillar 1: the deterministic parallel seam (serial when `par` is off).
pub mod par;

/// Browser-wasm thread-pool initializer (M.6.1): re-exported so ANY final cdylib linking this crate
/// with `par` on `wasm32-unknown-unknown` carries wasm-bindgen-rayon's `initThreadPool` JS export.
/// The app MUST `await initThreadPool(navigator.hardwareConcurrency)` before the first kernel call —
/// rayon's pool doesn't exist until then — on a cross-origin-isolated (COOP/COEP) page.
#[cfg(all(feature = "par", target_arch = "wasm32", target_os = "unknown"))]
pub use wasm_bindgen_rayon::init_thread_pool;
// the robust 2D triangulator the boolean reassembly leans on.
pub mod polygon;
// convex hull (`Manifold::Hull`) — the QuickHull port; unblocks minkowski + fab-scad `hull()`.
pub mod quickhull;
// Morton-code geometry reindex — the boolean's final canonicalization (`SortGeometry`), so a chained
// op's intermediate order matches C++ bit-for-bit.
pub mod sort;
// Typed geometry errors (`Manifold::Error`) surfaced as `Result` by the eager ops — NOT the C++ lazy
// `status_` field. See the module doc for why the flattening dissolves that pattern.
pub mod status;

/// The C++ Manifold differential oracle (Oracle A) — a `KernelDriver` trait with a Rust and a C++
/// backend, plus the triangulation-independent boolean-residual metric. Scaffolds R0..R.X; gone at
/// R.X. Native-only (needs the C++ toolchain), so it's behind BOTH the feature and a non-wasm cfg.
#[cfg(all(feature = "oracle", not(target_arch = "wasm32")))]
pub mod oracle;
