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
#[cfg(feature = "kernel")]
pub mod bambu;
// BOSL2 test corpus runner (K.1 tier 2): needs fab-lang (eval) + toml + std::fs — a native dev/CI tool.
#[cfg(feature = "native")]
pub mod corpus;
pub mod cross_section;
pub mod deps;
#[cfg(all(feature = "native", feature = "kernel"))]
pub mod differ;
pub mod geomsg;
#[cfg(feature = "kernel")]
pub mod geomsvc;
// import()/surface() mesh readers (M.5): needs stl (native) + threemf_in (kernel) + fab-lang, same gate
// as differ. The impure side of fab-lang's needs fixpoint.
#[cfg(all(feature = "native", feature = "kernel"))]
pub mod import;
#[cfg(feature = "kernel")]
pub mod kernel;
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
#[cfg(feature = "native")]
pub mod publish;
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
