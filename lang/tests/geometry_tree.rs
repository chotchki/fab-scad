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

use fab_lang::{GeoNode, evaluate, evaluate_geometry};

#[test]
fn primitive_is_a_leaf() {
    assert!(matches!(
        evaluate_geometry("cube(10);").unwrap(),
        GeoNode::Leaf(_)
    ));
    // ...and a single Leaf still flattens to a mesh with no backend (the evaluate() path).
    assert!(evaluate("cube(10);").unwrap().tri_count() > 0);
}

#[test]
fn empty_program_is_empty() {
    assert_eq!(evaluate_geometry("x = 5;").unwrap(), GeoNode::Empty);
    assert_eq!(evaluate("x = 5;").unwrap().tri_count(), 0); // Empty flattens to an empty mesh
}

#[test]
fn transform_wraps_its_child_with_the_matrix() {
    match evaluate_geometry("translate([5, 2, 9]) cube(10);").unwrap() {
        GeoNode::Transform { matrix, child } => {
            let m = matrix.as_row_major();
            assert_eq!([m[3], m[7], m[11]], [5.0, 2.0, 9.0]); // translation column
            assert!(matches!(*child, GeoNode::Leaf(_)));
        }
        other => panic!("expected Transform, got {other:?}"),
    }
    // scale is a diagonal; multmatrix passes through; rotate(0) is identity.
    assert!(matches!(
        evaluate_geometry("scale([2, 3, 4]) cube(1);").unwrap(),
        GeoNode::Transform { matrix, .. } if {
            let m = matrix.as_row_major();
            [m[0], m[5], m[10]] == [2.0, 3.0, 4.0]
        }
    ));
}

#[test]
fn multiple_objects_are_an_implicit_union() {
    match evaluate_geometry("cube(10); sphere(5, $fn = 8);").unwrap() {
        GeoNode::Union(children) => assert_eq!(children.len(), 2),
        other => panic!("expected Union, got {other:?}"),
    }
    // ...and a CSG tree can't flatten without a backend — evaluate() is LOUD.
    assert!(evaluate("cube(10); sphere(5);").is_err());
}

#[test]
fn booleans_build_their_nodes_over_children() {
    let two = |src| match evaluate_geometry(src).unwrap() {
        GeoNode::Difference(c) | GeoNode::Intersection(c) | GeoNode::Union(c) => c.len(),
        other => panic!("expected a boolean node, got {other:?}"),
    };
    assert_eq!(two("difference() { cube(10); sphere(5, $fn = 8); }"), 2);
    assert_eq!(two("intersection() { cube(10); sphere(5, $fn = 8); }"), 2);
    assert_eq!(two("union() { cube(10); sphere(5, $fn = 8); }"), 2);
}

#[test]
fn nested_transform_over_a_boolean() {
    match evaluate_geometry("translate([1, 0, 0]) union() { cube(2); sphere(1, $fn = 8); }")
        .unwrap()
    {
        GeoNode::Transform { child, .. } => {
            assert!(matches!(*child, GeoNode::Union(ref c) if c.len() == 2));
        }
        other => panic!("expected Transform, got {other:?}"),
    }
}

#[test]
fn bare_block_groups_its_children_into_a_union() {
    match evaluate_geometry("{ cube(2); sphere(1, $fn = 8); }").unwrap() {
        GeoNode::Union(children) => assert_eq!(children.len(), 2),
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn if_contributes_the_taken_branch() {
    assert!(matches!(
        evaluate_geometry("if (true) cube(1);").unwrap(),
        GeoNode::Leaf(_)
    ));
    assert_eq!(
        evaluate_geometry("if (false) cube(1);").unwrap(),
        GeoNode::Empty
    );
    assert!(matches!(
        evaluate_geometry("if (1 > 2) cube(1); else sphere(1, $fn = 8);").unwrap(),
        GeoNode::Leaf(_) // the else branch
    ));
}

#[test]
fn for_iterates_and_unions() {
    let count = |src| match evaluate_geometry(src).unwrap() {
        GeoNode::Union(c) | GeoNode::Intersection(c) => c.len(),
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
    assert_eq!(count("for (i = 5) translate([i, 0, 0]) cube(1);"), 1); // scalar → one iteration
    assert_eq!(
        count("intersection_for (i = [0:2]) rotate([0, 0, i * 30]) cube(10);"),
        3
    );
}

#[test]
fn evaluate_geometry_file_reads_and_builds_a_tree() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("geometry_tree_file.scad");
    std::fs::write(&path, "translate([1, 0, 0]) cube(2);").unwrap();
    assert!(matches!(
        fab_lang::evaluate_geometry_file(&path, &[]).unwrap(),
        GeoNode::Transform { .. }
    ));
    // an unreadable path → Error::Load.
    assert!(fab_lang::evaluate_geometry_file(&dir.join("nope.scad"), &[]).is_err());
}
