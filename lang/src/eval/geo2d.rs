//! The 2D geometry tree — fab-lang's 2D geometry OUTPUT (J.3), the strongly-typed sibling of
//! [`GeoNode`](super::geo::GeoNode).
//!
//! 2D and 3D are DIFFERENT TYPES here, on purpose. OpenSCAD lets you *write* a mixed tree (a `square`
//! next to a `cube`), but mixing dimensions is a WARNING, not a valid operation — so we encode the
//! dimension in the type system rather than as a runtime flag, and well-formed input (all of BOSL2)
//! becomes impossible to mis-lower at compile time. See SPEC "2D subsystem (DECIDED J.3.1)".
//!
//! The two trees are mutually recursive across the two DIMENSION BRIDGES, and only those:
//! - 2D→3D: [`GeoNode::Extrude`](super::geo::GeoNode::Extrude) holds a [`Shape2D`]
//!   (`linear_extrude` / `rotate_extrude`).
//! - 3D→2D: [`Shape2D::Projection`] holds a [`GeoNode`](super::geo::GeoNode) (`projection`).
//!
//! Evaluation produces a dimension-tagged [`Geo`]; each sub-tree under a tag is homogeneous. Everything
//! here is backend-agnostic (like `GeoNode`): the fab-scad backend lowers a `Shape2D` to a Manifold
//! `CrossSection`. This module is the TYPE scaffolding + bridge shape (J.3.1). The evaluator wiring that
//! PRODUCES these nodes lands incrementally: the 2D primitives + transforms + booleans + the 2D/3D
//! dimension-mixing rules are J.3.2.1 (`eval/mod.rs` + `eval/module.rs`); `offset` and the `extrude` /
//! `projection` bridge modules are J.3.3 onward.

use super::geo::GeoNode;
use crate::geom::{Affine2, Rgba, Vec2};

/// A closed 2D contour — a ring of points. One or more contours (an outer boundary plus holes) make a
/// [`Shape2D::Polygon`], resolved by the backend's fill rule.
pub type Contour = Vec<Vec2>;

/// A node in the 2D CSG tree — the strongly-typed 2D geometry the evaluator emits.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape2D {
    /// No 2D geometry (an empty result, a degenerate primitive).
    Empty,
    /// A 2D leaf — closed contours (outer boundary + holes). `square` / `circle` / `polygon` lower
    /// here (J.3.2); the backend resolves overlap/holes via its fill rule.
    Polygon(Vec<Contour>),
    /// Union of 2D children (also the implicit union of multiple 2D objects).
    Union(Vec<Shape2D>),
    /// `difference()` over 2D children — the first minus the rest.
    Difference(Vec<Shape2D>),
    /// `intersection()` over 2D children — the common area.
    Intersection(Vec<Shape2D>),
    /// `offset()` — grow (`delta > 0`) or shrink the child by `delta`, with a corner-[`Join2D`] style.
    Offset {
        /// Signed distance to inflate the outline by.
        delta: f64,
        /// How convex corners are finished.
        join: Join2D,
        /// Facet count for a [`Join2D::Round`] corner, resolved from `$fn` at eval time (ignored by
        /// the Miter/Bevel joins) — baked in like a `circle`'s segment count, so lowering is `$fn`-free.
        segments: u32,
        /// The offset subtree.
        child: Box<Shape2D>,
    },
    /// An affine transform of a 2D subtree (`translate` / `rotate` / `scale` / `mirror` on a 2D shape).
    Transform {
        /// The 2×3 affine.
        matrix: Affine2,
        /// The transformed subtree.
        child: Box<Shape2D>,
    },
    /// `projection()` — the 3D→2D bridge. `cut = true` slices the solid at `z = 0`; `cut = false` is
    /// the shadow (the whole solid flattened onto the XY plane).
    Projection {
        /// Whether to slice at `z = 0` (`true`) or project the full shadow (`false`).
        cut: bool,
        /// The 3D subtree being flattened.
        child: Box<GeoNode>,
    },
    /// `color()` over a 2D subtree — records the display color WITHOUT touching the geometry. The 2D backend
    /// (Manifold `CrossSection`) carries no vertex properties, so lowering passes the child through unchanged
    /// and the boolean-residual differential never sees it; the color rides the TREE so the GUI (Phase 5/7)
    /// can read it. The 3D analog is [`GeoNode::Color`], which the kernel DOES apply (Solids have properties).
    Color {
        /// The RGBA color.
        color: Rgba,
        /// The colored 2D subtree.
        child: Box<Shape2D>,
    },
}

impl Shape2D {
    /// The maximum x-coordinate over the profile's points, transforms applied — the revolve RADIUS for
    /// `rotate_extrude` (its farthest point from the +Z axis, feeding the fragment count). `None` for an
    /// empty profile or a subtree whose extent isn't statically known (a `Projection`); the caller falls
    /// back to the minimum fragment count. `difference`/`intersection` use the union bound (an over-
    /// estimate — a few extra segments, harmless — and those profiles are rare in a revolve); an
    /// `offset` grows the child's extent by its positive delta.
    #[must_use]
    pub fn max_x(&self) -> Option<f64> {
        fn walk(s: &Shape2D, m: &Affine2) -> Option<f64> {
            let fold = |it: &mut dyn Iterator<Item = f64>| {
                it.fold(None, |acc: Option<f64>, x| {
                    Some(acc.map_or(x, |a| a.max(x)))
                })
            };
            match s {
                Shape2D::Polygon(contours) => {
                    fold(&mut contours.iter().flatten().map(|p| m.apply(*p).x))
                }
                Shape2D::Transform { matrix, child } => walk(child, &m.compose(matrix)),
                Shape2D::Union(kids) | Shape2D::Difference(kids) | Shape2D::Intersection(kids) => {
                    fold(&mut kids.iter().filter_map(|c| walk(c, m)))
                }
                Shape2D::Offset { delta, child, .. } => walk(child, m).map(|x| x + delta.max(0.0)),
                Shape2D::Color { child, .. } => walk(child, m), // color moves no point
                Shape2D::Projection { .. } | Shape2D::Empty => None,
            }
        }
        walk(self, &Affine2::IDENTITY)
    }
}

/// The corner-join style for [`Shape2D::Offset`] — how an inflated outline finishes its convex corners.
/// The three OpenSCAD `offset()` reaches; maps onto Manifold/Clipper2 join types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join2D {
    /// Rounded corners (`$fn`-faceted) — OpenSCAD `offset(r = …)`.
    Round,
    /// Sharp mitered corners — OpenSCAD `offset(delta = …)` (the default).
    Miter,
    /// Beveled/chamfered corners — OpenSCAD `offset(delta = …, chamfer = true)`.
    Bevel,
}

/// How a [`Shape2D`] is swept into 3D by [`GeoNode::Extrude`](super::geo::GeoNode::Extrude). The
/// parameters map 1:1 onto Manifold's `extrude_with_options` / `revolve` (J.3.4 / J.3.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExtrudeKind {
    /// `linear_extrude` — sweep up `height` along +Z, twisting `twist` degrees over the run, tapering
    /// to per-axis end `scale`, across `slices` intermediate layers. `center` shifts the result to
    /// `z ∈ [−height/2, height/2]`.
    Linear {
        /// Extrusion height.
        height: f64,
        /// Total twist in degrees, applied linearly along Z.
        twist: f64,
        /// Per-axis `[x, y]` scale of the top face relative to the bottom.
        scale: [f64; 2],
        /// Number of intermediate layers (subdivisions along Z).
        slices: u32,
        /// Profile-perimeter fragment count (`$fn`) used to RESAMPLE the 2D outline when `twist != 0`
        /// — OpenSCAD subdivides each edge into `round(edge_len / perimeter · facets)` segments so the
        /// twisted walls follow the helix (J.3.4.1). Ignored when `twist == 0` (the raw profile extrudes).
        facets: u32,
        /// Center the result on `z = 0` instead of resting on it.
        center: bool,
    },
    /// `rotate_extrude` — revolve the child `angle` degrees about the +Z axis with `segments` facets.
    Rotate {
        /// Sweep angle in degrees (`360` = a full solid of revolution).
        angle: f64,
        /// Number of angular facets around the sweep.
        segments: u32,
    },
}

/// A dimension-tagged geometry result — what evaluation produces once 2D exists. Each variant's subtree
/// is homogeneous (all-2D or all-3D); the tag is how a module call reports which dimension it built, and
/// the point where a dimension-mixing warning fires. The evaluator's return type migrates to this at
/// J.3.2, when the first 2D primitive can be produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Geo {
    /// A 2D result.
    D2(Shape2D),
    /// A 3D result.
    D3(GeoNode),
}

impl Geo {
    /// This result's dimension as `2` or `3` — the number OpenSCAD prints in its "Mixing 2D and 3D
    /// objects" / "Ignoring {n}D child object" warnings.
    #[must_use]
    pub fn dim(&self) -> u8 {
        match self {
            Geo::D2(_) => 2,
            Geo::D3(_) => 3,
        }
    }

    /// Whether this is a NULL result — the `Empty` variant, meaning "no geometry object at all" (a `{}`
    /// block, an `if` with no taken branch, a `for` that never ran). Null geometry is DIM-NEUTRAL: it
    /// neither fixes a group's dimension nor triggers a mixing warning, and it drops out of an operand
    /// list. This is DISTINCT from a present-but-empty primitive like `cube(0)` — that tessellates to an
    /// empty mesh but is still a 3D object (`GeoNode::Leaf`), and it DOES fix the dimension (verified vs
    /// OpenSCAD 2026.06.12: `union(){ cube(0); circle(5); }` warns "Ignoring 2D child" → the result is 3D).
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Geo::D2(Shape2D::Empty) | Geo::D3(GeoNode::Empty))
    }
}

#[cfg(test)]
mod tests {
    use super::{ExtrudeKind, Geo, Join2D, Shape2D};
    use crate::GeoNode;
    use crate::geom::{Affine2, Vec2};

    /// A unit square as a single-contour polygon leaf.
    fn unit_square() -> Shape2D {
        Shape2D::Polygon(vec![vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ]])
    }

    #[test]
    fn shape2d_tree_constructs_every_variant() {
        let sq = unit_square();
        // Booleans + offset + transform over the leaf.
        let u = Shape2D::Union(vec![sq.clone(), Shape2D::Empty]);
        let d = Shape2D::Difference(vec![sq.clone(), sq.clone()]);
        let i = Shape2D::Intersection(vec![sq.clone()]);
        let off = Shape2D::Offset {
            delta: 2.0,
            join: Join2D::Round,
            segments: 16,
            child: Box::new(sq.clone()),
        };
        let xf = Shape2D::Transform {
            matrix: Affine2::IDENTITY,
            child: Box::new(sq.clone()),
        };
        // The 3D→2D bridge holds a GeoNode.
        let proj = Shape2D::Projection {
            cut: true,
            child: Box::new(GeoNode::Empty),
        };
        // Debug + Clone + PartialEq are all exercised (the derives are covered code).
        for s in [&u, &d, &i, &off, &xf, &proj] {
            assert_eq!(s.clone(), *s);
            assert!(!format!("{s:?}").is_empty());
        }
        assert_ne!(u, d);
        assert_eq!(Shape2D::Empty, Shape2D::Empty);
    }

    #[test]
    fn join2d_variants_are_distinct() {
        assert_ne!(Join2D::Round, Join2D::Miter);
        assert_ne!(Join2D::Miter, Join2D::Bevel);
        assert_eq!(Join2D::Round, Join2D::Round);
        assert!(!format!("{:?}", Join2D::Bevel).is_empty());
    }

    #[test]
    fn extrude_kind_linear_and_rotate() {
        let lin = ExtrudeKind::Linear {
            height: 10.0,
            twist: 90.0,
            scale: [0.5, 0.5],
            slices: 8,
            facets: 32,
            center: false,
        };
        let rot = ExtrudeKind::Rotate {
            angle: 360.0,
            segments: 64,
        };
        assert_eq!(lin, lin); // Copy + PartialEq
        assert_ne!(lin, rot);
        assert!(!format!("{lin:?}{rot:?}").is_empty());
    }

    #[test]
    fn geonode_extrude_bridges_a_2d_child() {
        // The 2D→3D bridge lives on GeoNode; a fab-lang test exercises its derives from that side.
        let ex = GeoNode::Extrude {
            kind: ExtrudeKind::Rotate {
                angle: 270.0,
                segments: 48,
            },
            child: Box::new(unit_square()),
        };
        assert_eq!(ex.clone(), ex);
        assert!(matches!(ex, GeoNode::Extrude { .. }));
        assert!(!format!("{ex:?}").is_empty());
    }

    #[test]
    fn geo_tags_both_dimensions() {
        let d2 = Geo::D2(unit_square());
        let d3 = Geo::D3(GeoNode::Empty);
        assert_eq!(d2.clone(), d2);
        assert_ne!(d2, d3);
        // The tag is matchable — the whole point of the typed split.
        assert!(matches!(d2, Geo::D2(_)));
        assert!(matches!(d3, Geo::D3(_)));
        assert!(!format!("{d3:?}").is_empty());
    }
}
