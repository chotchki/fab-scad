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
#[cfg(feature = "kernel")]
pub mod kernel;
pub mod manifest;
pub mod num;
pub mod openscad;
pub mod pack;
pub mod printers;
pub mod project;
pub mod slicing;
pub mod smoke;
