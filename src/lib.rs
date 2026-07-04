//! fab-scad as a library: the reusable workflow logic — the project manifest + slicing spec,
//! printer beds + cut planning, the slicing-driver codegen, and the OpenSCAD wrap. Shared by
//! the `fab` CLI (src/main.rs) and the Bevy GUI (gui/), so the slicing-spec and printer types
//! have ONE definition, not a mirror per front-end.

pub mod auto;
pub mod auto_orient;
pub mod auto_slice;
#[cfg(feature = "kernel")]
pub mod bambu;
pub mod cross_section;
pub mod deps;
pub mod geom;
pub mod geomsg;
#[cfg(feature = "kernel")]
pub mod geomsvc;
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
#[cfg(feature = "kernel")]
pub mod threemf_in;
