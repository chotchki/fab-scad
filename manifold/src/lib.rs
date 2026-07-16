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

// The whole point of the port is to LEAVE C++'s unsafety behind — this crate carries almost none of
// its own. `deny` (not `forbid`) because the boolean narrow phase carries a SMALL audited set of
// unchecked hot loads (C++ VecView release parity, BU.4.2): `boolean3::MeshView` VALIDATES the
// mesh tables in one O(halfedges) pass at Boolean3 entry — a violating mesh PANICS there — and
// only then reads them unchecked, through typed ids only; `collider::query_leaves` reads its own
// self-built arrays under a documented depth bound. Each site is an item-scoped
// `#[allow(unsafe_code)]` with a debug_assert! bound and a SAFETY invariant naming the check that
// justifies it. Any NEW unsafe anywhere else still fails the build.
#![deny(unsafe_code)]

// the robustness core: boolean3 + boolean_result + face_op + edge_op.
pub mod boolean;
// Oracle B: the manifold-invariant checker (test.h port), reference-free.
pub mod check;
// R5 — the 2D `CrossSection` subsystem, over the i_overlay 2D boolean engine (area-residual gated, NOT
// bit-exact — the one layer where the verbatim thesis relaxes; SPEC [OPEN #4]).
pub mod cross_section;
// R5 — the 2D↔3D bridges (Extrude/Revolve/Project/Slice); unblocks M.3.8.
pub mod bridge;
// Byte-fingerprinting for the golden-mode gates (M.6.3) — the M.7 freeze's vocabulary.
pub mod golden;
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

// The C++ Manifold differential oracle (Oracle A) lived here R0..M.7.3 — a `KernelDriver` A/B
// harness over manifold3d that gated every port stage. CUT at M.7.4 (the finish line): the frozen
// goldens (`goldens/`, tests/m7_golden_mode.rs) + the M.6 cross-lane corpus carry the correctness
// memory; the OpenSCAD-binary differential (fab-scad differ) remains as the external oracle.
