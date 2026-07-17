//! Geometry errors — a port of Manifold's `Manifold::Error`, minus its `NoError` variant.
//!
//! WHY NOT A `status_` FIELD: the C++ carries `Error status_` ON the `Impl` and surfaces it through
//! `Manifold::Status()`, because a `Manifold` is a node in a LAZY CSG tree — an op can't return a
//! `Result`, so it stashes the error on the (empty) result object and lets it surface at the leaf. That
//! forces every op to open with `if (status_ != NoError) propagate;`. M.3 FLATTENS the csg_tree to eager
//! (see the M.3 plan line), which dissolves that constraint: at the eager `Mesh`-op layer, failure is a
//! `Result<Mesh, Error>` and propagation is `?`. This makes the "a Mesh is silently carrying a latent
//! error" state — the C++ misuse vector — unrepresentable, and drops the propagation branch entirely.
//!
//! `NoError` doesn't survive the translation: the healthy state is `Ok`/absence, not an enum variant. An
//! error path's GEOMETRY output is the empty mesh either way, so the differential oracle (which compares
//! volume/genus/mesh) still sees "both empty" — the swap is invisible to the verbatim-port thesis, which
//! binds the geometry algorithms, not the error skin.

use thiserror::Error;

/// The reason a geometry op produced no valid mesh — Manifold's `Manifold::Error` (variants verbatim,
/// `NoError` dropped since that's `Ok`). Only a subset is reachable today; the rest are ported ahead of
/// the ops that raise them (set_properties → `PropertiesWrongLength`, merge import → the `Merge*`
/// variants, …) so the enum is complete and the mapping to C++ stays 1:1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum Error {
    /// A vertex position (or a transform matrix entry) is NaN or infinite.
    #[error("non-finite vertex position (NaN or infinite)")]
    NonFiniteVertex,
    /// The result is not an oriented 2-manifold (half-edge pairing inconsistent).
    #[error("not an oriented 2-manifold")]
    NotManifold,
    /// A triangle references a vertex index outside `vert_pos`.
    #[error("vertex index out of bounds")]
    VertexOutOfBounds,
    /// The vertex-property buffer isn't a whole number of `num_prop`-wide rows.
    #[error("vertex properties are not a whole number of rows")]
    PropertiesWrongLength,
    /// `num_prop < 3` — the position channel is missing.
    #[error("missing position properties (num_prop < 3)")]
    MissingPositionProperties,
    /// The two merge-index vectors (`from`/`to`) have different lengths.
    #[error("merge index vectors have different lengths")]
    MergeVectorsDifferentLengths,
    /// A merge index points outside the vertex range.
    #[error("merge index out of bounds")]
    MergeIndexOutOfBounds,
    /// The per-instance transform vector has the wrong length.
    #[error("transform vector has the wrong length")]
    TransformWrongLength,
    /// The run-index vector has the wrong length.
    #[error("run-index vector has the wrong length")]
    RunIndexWrongLength,
    /// The face-ID vector has the wrong length.
    #[error("face-ID vector has the wrong length")]
    FaceIdWrongLength,
    /// The construction inputs are structurally invalid.
    #[error("invalid construction")]
    InvalidConstruction,
    /// The result exceeds the maximum representable size.
    #[error("result too large")]
    ResultTooLarge,
    /// The supplied half-edge tangents are invalid.
    #[error("invalid tangents")]
    InvalidTangents,
    /// The operation was cancelled.
    #[error("operation cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_and_compare() {
        // thiserror `Display` renders the `#[error("…")]` message.
        assert_eq!(
            Error::NonFiniteVertex.to_string(),
            "non-finite vertex position (NaN or infinite)"
        );
        // Copy + Eq: the enum is a plain value type (fits in a `Result<Mesh, Error>` return cheaply).
        let e = Error::NotManifold;
        assert_eq!(e, e);
        assert_ne!(Error::NotManifold, Error::VertexOutOfBounds);
    }
}
