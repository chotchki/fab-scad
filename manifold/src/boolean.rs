//! The robustness core — the entire bet.
//!
//! boolean3 (the 3-way vert/edge/face intersection classification), boolean_result (reassembling a
//! watertight manifold from the classified pieces — where the EdgePos total-order comparator lives, the
//! fix for the C++ non-determinism), face_op + edge_op (the manifold-preserving topology surgery at
//! every coplanar/degenerate seam). union/difference/intersection all fall out of boolean3's op
//! parameter. Robustness comes from Manifold's tolerance model (tracked epsilon + operation-dependent
//! symbolic perturbation of exact-equal ties), NOT exact arithmetic — see [`crate::mathf`]. A 95%-right
//! version passes cubes and fails the nasty corpus; there is no partial credit here.
//!
//! SERIAL through R3 (the C++ reference stays exactly comparable); [`crate::par`] swaps in at R4.
//!
//! M.1.0 landed the FOUNDATIONS:
//! - [`predicates`] — the symbolic-perturbation primitives (`shadows`/`interpolate`/`intersect`/`ccw`/
//!   `get_axis_aligned_projection`/…), ported verbatim, no FMA.
//! - [`vocab`] — the value-style records the assembly passes around (`Halfedge`/`TriRef`/`TmpEdge`/
//!   `Intersections`).
//!
//! The perturbation INPUTS those consume (`face_normal`/`epsilon`/`tolerance`, the `for_vert` orbit)
//! live on [`crate::mesh::Mesh`]. Next: M.1.1 broad phase → M.1.2 boolean3 cascade.

pub mod boolean3;
pub mod collider;
pub mod disjoint_sets;
pub mod predicates;
pub mod vocab;

/// The boolean operation (`manifold.h` `OpType`). Only `Add` (union) is exercised by the R1 tracer;
/// `Subtract`/`Intersect` fall out of the same `boolean3` with `expandP = false`, ported in R2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpType {
    /// Union — the R1 tracer op. `expandP = true`: symbolic-perturbation expands BOTH inputs.
    Add,
    /// Difference (A − B). `expandP = false`.
    Subtract,
    /// Intersection (A ∩ B). `expandP = false`.
    Intersect,
}
