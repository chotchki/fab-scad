//! fab-scad as a library: the reusable workflow logic — the project manifest + slicing spec,
//! printer beds + cut planning, the slicing-driver codegen, and the OpenSCAD wrap. Shared by
//! the `fab` CLI (src/main.rs) and the Bevy GUI (gui/), so the slicing-spec and printer types
//! have ONE definition, not a mirror per front-end.

pub mod auto;
pub mod auto_orient;
pub mod auto_slice;
// The geometry backend trait (J.1): the CSG op vocabulary the geometry lowering targets, with a mock
// (miri-tested) + the real Manifold impl (ASAN-tested). Needs only fab-lang's Mesh, hence `geometry`
// (NOT `native`) so miri can run the mock without the OS-heavy native deps.
#[cfg(feature = "geometry")]
pub mod backend;
#[cfg(feature = "kernel")]
pub mod bambu;
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
pub mod slicing;
#[cfg(feature = "native")]
pub mod smoke;
pub mod stl;
// surface() heightmap → mesh (M.5.2, DAT-only): needs fab-lang (Mesh); called by the import reader.
#[cfg(feature = "geometry")]
pub mod surface;
#[cfg(feature = "kernel")]
pub mod threemf_in;
