//! The mesh spine — Manifold's `Impl` (a half-edge mesh).
//!
//! The structure everything mutates: vertices, the half-edge connectivity (`CreateHalfedges`), the
//! per-mesh tolerance/epsilon, and the property (color) channels threaded through booleans. Round-trips
//! to/from `MeshGL` (the flat vert+index+property buffer fab-scad's `to_mesh_f64`/`from_mesh_f64` speak,
//! stride 3=xyz / 7=xyz+RGBA), and answers `IsManifold` (the validity gate). No booleans here — this is
//! the spine the boolean reassembly writes onto.
//!
//! TODO(M.0.5).
