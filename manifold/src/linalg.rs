//! The linalg layer — RE-EXPORTED from `fab-types` (M.0.2.1: lifted once R0–R6 proved the op order
//! bit-clean vs the C++ oracle; the byte-goldens held unchanged through the move). What stays here
//! is exactly the kernel's TRIG dialect: the degree-rotate builders feed `mathf`'s deterministic
//! `sind`/`cosd` (exact-quadrant, `math.h`-reference) into the type crate's sincos-parameterized
//! constructors — fab-lang's OpenSCAD-reference trig never mixes with it.

pub use fab_types::*;

use crate::mathf::{cosd, sind};

/// Euler rotation about x, then y, then z in DEGREES (`CsgNode::Rotate` — was `Mat3x4::rotate`):
/// mathf sincos into the verbatim `csg_tree.cpp` composition in [`Mat3x4::rotate_from_sincos`].
pub fn rotate_xyz_degrees(x_degrees: f64, y_degrees: f64, z_degrees: f64) -> Mat3x4 {
    Mat3x4::rotate_from_sincos([
        [sind(x_degrees), cosd(x_degrees)],
        [sind(y_degrees), cosd(y_degrees)],
        [sind(z_degrees), cosd(z_degrees)],
    ])
}

/// Z-axis rotation in DEGREES (`CrossSection::Rotate`'s matrix — was `Mat2x3::rotate_degrees`).
#[inline]
pub fn rotate2_degrees(degrees: f64) -> Mat2x3 {
    Mat2x3::rotate_from_sincos(sind(degrees), cosd(degrees))
}
