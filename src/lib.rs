//! fab-scad as a library: the reusable workflow logic — the project manifest + slicing spec,
//! printer beds + cut planning, the slicing-driver codegen, and the OpenSCAD wrap. Shared by
//! the `fab` CLI (src/main.rs) and the Bevy GUI (gui/), so the slicing-spec and printer types
//! have ONE definition, not a mirror per front-end.

// Print planning + slicing: all need geometry, and auto/auto_slice call the kernel — so they're
// gated (NOT ungated) to keep the no-default-features build wasm-safe. auto_orient is pure geom math.
#[cfg(feature = "kernel")]
pub mod auto;
#[cfg(feature = "geometry")]
pub mod auto_orient;
#[cfg(feature = "kernel")]
pub mod auto_slice;
// The geometry backend trait (J.1): the CSG op vocabulary the geometry lowering targets, with a mock
// (miri-tested) + the real Manifold impl (ASAN-tested). Needs only fab-lang's Mesh, hence `geometry`
// (NOT `native`) so miri can run the mock without the OS-heavy native deps.
#[cfg(feature = "geometry")]
pub mod backend;
// BU.7 dev probe: GeoNode->Solid redundancy (FAB_GEO_REDUNDANCY=1) — the P.2 kernel-cache ceiling.
mod geo_redundancy;
// Shared GeoNode/Shape2D content hashing (the probe + the P.2 memo).
mod geo_hash;
// The Bambu `.3mf` writer — mesh-only (no Solid), so it rides `mesh-io` not `kernel`, reachable on
// the wasm app for in-process co-pack + export (W.3.4). `kernel` implies `mesh-io`, so native is unchanged.
#[cfg(feature = "mesh-io")]
pub mod bambu;
// Standard 3MF (core + basematerials color) whole-model writer — the web save-back's mesh variant
// (W.5). Mesh-only (no Solid), so it rides `mesh-io` like `bambu`, reachable on the wasm worker.
#[cfg(feature = "mesh-io")]
pub mod threemf_out;
// The `.scadproj` project container (Phase Z): a multi-file OpenSCAD project as one stored zip, so the
// fs-less web app opens a project like the desktop opens a folder. Byte-based reader/writer, so it rides
// `mesh-io` like `bambu`/`threemf_out` and runs on the wasm worker.
#[cfg(feature = "mesh-io")]
pub mod scadproj;
// BOSL2 test corpus runner (K.1 tier 2): needs fab-lang (eval) + toml + std::fs — a native dev/CI tool.
#[cfg(feature = "native")]
pub mod corpus;
// Upstream-expectation filter for the sustain sweep (AE.1): classifies corpus failures against
// openscad's own must-fail verdicts. Report-side companion to `corpus`, same native gate.
#[cfg(feature = "native")]
pub mod sweep_expect;
// The desktop publish credential (W.3.27): env-then-file resolution + a 0600 config store. Native only —
// the web publishes via the site session cookie, no API key.
#[cfg(feature = "native")]
pub mod credentials;
pub mod cross_section;
// QEM mesh decimation (W.5) — the web save-back's low-res mesh variant. Mesh-only (no Solid), so it
// rides `mesh-io` like `threemf_out` and runs on the wasm geom worker.
#[cfg(feature = "mesh-io")]
pub mod decimate;
pub mod deps;
// Pure onion-joint feasibility + slab math (W.3.4) — no Solid, so it rides `geometry` (reachable on
// the wasm app for the live joint-downgrade flag). Extracted from `slicing`; the kernel slicer reuses it.
#[cfg(all(feature = "native", feature = "kernel"))]
pub mod differ;
#[cfg(feature = "geometry")]
pub mod feasibility;
pub mod geomsg;
#[cfg(feature = "kernel")]
pub mod geomsvc;
// import()/surface() mesh readers (M.5): the impure side of fab-lang's needs fixpoint. Kernel-gated —
// its deps (stl/threemf_in/svg/surface) are all kernel; the fs `read_import` is native-USED, and
// `read_import_bytes` serves the wasm worker's Source::Bytes imports (W.3.24).
#[cfg(feature = "kernel")]
pub mod import;
#[cfg(feature = "kernel")]
pub mod kernel;
// Browser-wasm thread-pool initializer (W.6): re-exported up the crate chain so the geom worker's final
// cdylib (fab-geom) carries wasm-bindgen-rayon's `initThreadPool` JS export. The worker MUST
// `await initThreadPool(navigator.hardwareConcurrency)` before the first kernel call, on a
// cross-origin-isolated (COOP/COEP) page. Native + serial wasm: absent (OS threads / serial).
#[cfg(all(feature = "kernel-par", target_arch = "wasm32", target_os = "unknown"))]
pub use fab_manifold::init_thread_pool;
pub mod manifest;
pub mod num;
#[cfg(feature = "native")]
pub mod openscad;
#[cfg(feature = "native")]
pub mod oracle;
pub mod pack;
pub mod printers;
#[cfg(feature = "native")]
pub mod project;
// The publish CONTRACT (W.3.29.1) — pure endpoint/field/slug/markdown knowledge, UNGATED so the native
// reqwest client AND the wasm fetch client (fab-gui) share it and can't drift.
#[cfg(feature = "native")]
pub mod publish;
pub mod publish_contract;
#[cfg(feature = "kernel")]
pub mod slicing;
#[cfg(feature = "native")]
pub mod smoke;
pub mod stl;
// SVG (2D vector) import (Q.4): usvg → contours, the fab-scad side of the 2D import path. kernel gate
// (usvg lives there); reached only via `import`, which also needs kernel + native.
#[cfg(feature = "kernel")]
pub mod svg;

/// A modest stack reserve for the render/eval harness threads. As of M.3, geometry EVAL is HEAP-bounded (the
/// explicit-stack driver — no host recursion; proven at `module_recursion_bound.rs` on a 512 KiB stack), joining
/// the expression machine (Phase I) and tree `Drop` (M.1/M.1b). So eval no longer needs a reserve at all — this
/// remains only as courtesy headroom for the NATIVE geometry backend (the Manifold tree-lowering + CSG render,
/// a separate subsystem the harness threads run right after eval). Dropped from the old 1 GiB (M.2's guard for
/// the then-host-recursive eval, now obsolete) to 64 MiB — ample for any real render, and eval itself would be
/// fine on the default stack.
pub const EVAL_STACK: usize = 64 * 1024 * 1024;
// surface() heightmap → mesh (M.5.2, DAT-only): needs fab-lang (Mesh); called by the import reader.
#[cfg(feature = "geometry")]
pub mod surface;
#[cfg(feature = "kernel")]
pub mod threemf_in;
