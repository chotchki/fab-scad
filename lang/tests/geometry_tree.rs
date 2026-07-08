//! J.2.1/J.2.2 — the CSG geometry tree fab-lang builds. These pin the tree STRUCTURE (the shape the
//! downstream backend walks); the geometry itself (that the tree renders the same solid as OpenSCAD)
//! is the differential's job, in fab-scad. A single primitive stays a `Leaf` [`evaluate`] can flatten
//! without a backend; transforms, booleans, and multi-object programs build real internal nodes.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration test: unwrap/panic ARE the assertions; translate/scale matrices are EXACT (the literal args, no trig)"
)]

use fab_lang::{Geo, GeoNode, Rgba, evaluate, evaluate_geometry};

/// Unwrap a 3D geometry result to its [`GeoNode`] — every case in this file is 3D (the 2D subsystem's
/// tree structure is pinned in `geometry_tree_2d.rs`), so the `Geo::D3` tag is just noise to strip.
fn d3(g: Geo) -> GeoNode {
    match g {
        Geo::D3(node) => node,
        Geo::D2(shape) => panic!("expected a 3D result, got 2D: {shape:?}"),
    }
}

#[test]
fn primitive_is_a_leaf() {
    assert!(matches!(
        d3(evaluate_geometry("cube(10);").unwrap()),
        GeoNode::Leaf(_)
    ));
    // ...and a single Leaf still flattens to a mesh with no backend (the evaluate() path).
    assert!(evaluate("cube(10);").unwrap().tri_count() > 0);
}

#[test]
fn empty_program_is_empty() {
    assert_eq!(d3(evaluate_geometry("x = 5;").unwrap()), GeoNode::Empty);
    assert_eq!(evaluate("x = 5;").unwrap().tri_count(), 0); // Empty flattens to an empty mesh
}

#[test]
fn transform_wraps_its_child_with_the_matrix() {
    match d3(evaluate_geometry("translate([5, 2, 9]) cube(10);").unwrap()) {
        GeoNode::Transform { ref matrix, ref child } => {
            let m = matrix.as_row_major();
            assert_eq!([m[3], m[7], m[11]], [5.0, 2.0, 9.0]); // translation column
            assert!(matches!(**child, GeoNode::Leaf(_)));
        }
        other => panic!("expected Transform, got {other:?}"),
    }
    // scale is a diagonal; multmatrix passes through; rotate(0) is identity.
    assert!(matches!(
        d3(evaluate_geometry("scale([2, 3, 4]) cube(1);").unwrap()),
        GeoNode::Transform { ref matrix, .. } if {
            let m = matrix.as_row_major();
            [m[0], m[5], m[10]] == [2.0, 3.0, 4.0]
        }
    ));
}

#[test]
fn multiple_objects_are_an_implicit_union() {
    match d3(evaluate_geometry("cube(10); sphere(5, $fn = 8);").unwrap()) {
        GeoNode::Union(ref children) => assert_eq!(children.len(), 2),
        other => panic!("expected Union, got {other:?}"),
    }
    // ...and a CSG tree can't flatten without a backend — evaluate() is LOUD.
    assert!(evaluate("cube(10); sphere(5);").is_err());
}

#[test]
fn booleans_build_their_nodes_over_children() {
    let two = |src| match d3(evaluate_geometry(src).unwrap()) {
        GeoNode::Difference(ref c) | GeoNode::Intersection(ref c) | GeoNode::Union(ref c) => c.len(),
        other => panic!("expected a boolean node, got {other:?}"),
    };
    assert_eq!(two("difference() { cube(10); sphere(5, $fn = 8); }"), 2);
    assert_eq!(two("intersection() { cube(10); sphere(5, $fn = 8); }"), 2);
    assert_eq!(two("union() { cube(10); sphere(5, $fn = 8); }"), 2);
}

#[test]
fn hull_builds_an_n_ary_node_over_children() {
    match d3(
        evaluate_geometry("hull() { cube(2); translate([5, 0, 0]) sphere(1, $fn = 8); }").unwrap(),
    ) {
        GeoNode::Hull(ref children) => assert_eq!(children.len(), 2),
        other => panic!("expected a Hull node, got {other:?}"),
    }
    // hull() of a single child is still a Hull (of one) — the backend hulls it to its convex hull.
    assert!(
        matches!(d3(evaluate_geometry("hull() cube(2);").unwrap()), GeoNode::Hull(ref c) if c.len() == 1)
    );
}

#[test]
fn nested_transform_over_a_boolean() {
    match d3(
        evaluate_geometry("translate([1, 0, 0]) union() { cube(2); sphere(1, $fn = 8); }").unwrap(),
    ) {
        GeoNode::Transform { ref child, .. } => {
            assert!(matches!(**child, GeoNode::Union(ref c) if c.len() == 2));
        }
        other => panic!("expected Transform, got {other:?}"),
    }
}

#[test]
fn bare_block_groups_its_children_into_a_union() {
    match d3(evaluate_geometry("{ cube(2); sphere(1, $fn = 8); }").unwrap()) {
        GeoNode::Union(ref children) => assert_eq!(children.len(), 2),
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn if_contributes_the_taken_branch() {
    assert!(matches!(
        d3(evaluate_geometry("if (true) cube(1);").unwrap()),
        GeoNode::Leaf(_)
    ));
    assert_eq!(
        d3(evaluate_geometry("if (false) cube(1);").unwrap()),
        GeoNode::Empty
    );
    assert!(matches!(
        d3(evaluate_geometry("if (1 > 2) cube(1); else sphere(1, $fn = 8);").unwrap()),
        GeoNode::Leaf(_) // the else branch
    ));
}

#[test]
fn for_iterates_and_unions() {
    let count = |src| match d3(evaluate_geometry(src).unwrap()) {
        GeoNode::Union(ref c) | GeoNode::Intersection(ref c) => c.len(),
        other => panic!("expected a fold node, got {other:?}"),
    };
    assert_eq!(
        count("for (i = [0:2]) translate([i * 5, 0, 0]) cube(2);"),
        3
    ); // range 0,1,2
    assert_eq!(
        count("for (x = [1, 5, 9]) translate([x, 0, 0]) cube(1);"),
        3
    ); // number list
    assert_eq!(
        count("for (p = [[0, 0, 0], [5, 0, 0]]) translate(p) cube(1);"),
        2
    ); // list of vecs
    assert_eq!(
        count("for (i = [0:1], j = [0:2]) translate([i, j, 0]) cube(1);"),
        6
    ); // product 2×3
    // scalar → one iteration: union_of collapses a single iteration to the child itself (a Transform).
    assert!(matches!(
        d3(evaluate_geometry("for (i = 5) translate([i, 0, 0]) cube(1);").unwrap()),
        GeoNode::Transform { .. }
    ));
    assert_eq!(
        count("intersection_for (i = [0:2]) rotate([0, 0, i * 30]) cube(10);"),
        3
    );
}

#[test]
fn color_wraps_its_subtree() {
    // A named color → GeoNode::Color with the resolved Rgba; child is the primitive.
    match d3(evaluate_geometry("color(\"red\") cube(10);").unwrap()) {
        GeoNode::Color { ref color, ref child } => {
            assert_eq!(*color, Rgba::opaque(1.0, 0.0, 0.0));
            assert!(matches!(**child, GeoNode::Leaf(_)));
        }
        other => panic!("expected Color, got {other:?}"),
    }
    // rgb vector + alpha override; hex; case-insensitive name.
    assert!(matches!(
        d3(evaluate_geometry("color([0, 1, 0], 0.5) cube(1);").unwrap()),
        GeoNode::Color { ref color, .. } if *color == Rgba::new(0.0, 1.0, 0.0, 0.5)
    ));
    assert!(matches!(
        d3(evaluate_geometry("color(\"#0000ff\") sphere(1, $fn = 8);").unwrap()),
        GeoNode::Color { ref color, .. } if *color == Rgba::opaque(0.0, 0.0, 1.0)
    ));
    // color OVER a boolean: the whole difference is the colored subtree.
    assert!(matches!(
        d3(evaluate_geometry("color(\"red\") difference() { cube(10); sphere(6, $fn = 8); }").unwrap()),
        GeoNode::Color { ref child, .. } if matches!(**child, GeoNode::Difference(_))
    ));
    // INVALID color inherits — NO Color node, just the child (OpenSCAD's -1 sentinel).
    assert!(matches!(
        d3(evaluate_geometry("color(\"notacolor\") cube(1);").unwrap()),
        GeoNode::Leaf(_)
    ));
    // a non-string / non-vector color arg (a number) is also invalid → inherit.
    assert!(matches!(
        d3(evaluate_geometry("color(5) cube(1);").unwrap()),
        GeoNode::Leaf(_)
    ));
    // nested: the OUTER node wraps the inner (the backend resolves outer-wins at J.2.9).
    assert!(matches!(
        d3(evaluate_geometry("color(\"red\") color(\"blue\") cube(1);").unwrap()),
        GeoNode::Color { ref color, ref child }
            if *color == Rgba::opaque(1.0, 0.0, 0.0) && matches!(**child, GeoNode::Color { .. })
    ));
    // ...and a single colored primitive still flattens with no backend (color dropped from the mesh).
    assert!(evaluate("color(\"red\") cube(10);").unwrap().tri_count() > 0);
}

#[test]
fn evaluate_geometry_file_reads_and_builds_a_tree() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("geometry_tree_file.scad");
    std::fs::write(&path, "translate([1, 0, 0]) cube(2);").unwrap();
    assert!(matches!(
        d3(fab_lang::evaluate_geometry_file(&path, &[]).unwrap()),
        GeoNode::Transform { .. }
    ));
    // an unreadable path → Error::Load.
    assert!(fab_lang::evaluate_geometry_file(&dir.join("nope.scad"), &[]).is_err());
}
