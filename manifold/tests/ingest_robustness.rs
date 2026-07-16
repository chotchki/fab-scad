//! REGRESSION (M.7.3 flip): real welded imports (OpenSCAD STLs) carry two shapes the clean-mesh
//! ingest rejected where the C++ accepted — 4 of 15 corpus models failed the flip's import lane.
//!
//! 1. OPPOSED interior walls: the same vertex triple wound both ways (a union's coincident faces).
//!    The C++ `CreateHalfedges` cancels such pairs (`removed`); ours is the `cancel_opposed_tris`
//!    pre-pass in `from_mesh_gl`.
//! 2. PINCHED edges: one edge shared by 2k triangles (two solids touching along a line). The C++'s
//!    global sort threads k-th forward with k-th backward and `SplitPinchedVerts` untangles; ours is
//!    the threaded pairing in `create_halfedges` + the already-ported `cleanup_topology`.

use fab_manifold::mesh::{Mesh, MeshGl};

fn gl(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> MeshGl {
    MeshGl {
        num_prop: 3,
        vert_properties: verts.iter().flatten().copied().collect(),
        tri_verts: tris.iter().flatten().copied().collect(),
        ..Default::default()
    }
}

/// Two unit cubes stacked in z, soup-imported WITH the coincident interior wall present from both
/// sides (each cube keeps its own copy of the z=1 face, opposite windings). Cancellation must
/// dissolve the wall: one manifold box of volume 2.
#[test]
fn opposed_interior_walls_cancel() {
    let cube_tris: [[u32; 3]; 12] = [
        [0, 2, 1],
        [0, 3, 2], // bottom (z=lo), -z
        [4, 5, 6],
        [4, 6, 7], // top (z=hi), +z
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    let ring =
        |z: f64| -> Vec<[f64; 3]> { vec![[0., 0., z], [1., 0., z], [1., 1., z], [0., 1., z]] };
    // Cube A: verts 0..8 (z 0..1). Cube B: reuses A's top ring as its bottom (verts 4..8) plus a new
    // ring at z=2 (verts 8..12) — exact-bit welded, like the STL importer produces.
    let mut verts = ring(0.0);
    verts.extend(ring(1.0));
    verts.extend(ring(2.0));
    let mut tris: Vec<[u32; 3]> = cube_tris.to_vec();
    tris.extend(cube_tris.iter().map(|t| t.map(|v| v + 4)));
    let m = Mesh::from_mesh_gl(&gl(&verts, &tris)).expect("opposed walls cancel");
    assert!(m.is_manifold());
    assert!(
        (m.volume() - 2.0).abs() < 1e-12,
        "volume {} != 2",
        m.volume()
    );
    assert_eq!(fab_manifold::check::genus(&m), 0);
}

/// Two tetrahedra sharing ONE edge (a pinched edge: 4 triangles on it, 2 wound each way). The
/// threaded pairing + `split_pinched_verts` must yield a valid 2-component solid, not a rejection.
#[test]
fn pinched_edge_threads_and_splits() {
    let verts: Vec<[f64; 3]> = vec![
        [0., 0., 0.],     // 0 (shared edge)
        [1., 0., 0.],     // 1 (shared edge)
        [0.5, 1., 0.],    // 2 tet A
        [0.5, 0.5, 1.],   // 3 tet A apex
        [0.5, -1., 0.],   // 4 tet B
        [0.5, -0.5, -1.], // 5 tet B apex
    ];
    let tet = |a: u32, b: u32, c: u32, d: u32| -> [[u32; 3]; 4] {
        [[a, c, b], [a, b, d], [b, c, d], [c, a, d]]
    };
    let mut tris: Vec<[u32; 3]> = tet(0, 1, 2, 3).to_vec();
    tris.extend(tet(0, 1, 4, 5)); // apex below the z=0 plane — same helper, swapped base order

    let m = Mesh::from_mesh_gl(&gl(&verts, &tris)).expect("pinched edge accepted");
    assert!(m.is_manifold());
    assert!(m.volume() > 0.0);
    assert_eq!(m.decompose().len(), 2, "two tets, split at the pinch");
}

/// A repeated-vert DEGENERATE triangle in the soup (what an exact-bit weld of an f32 STL collapses
/// slivers into) is dropped before pairing — the C++ ctor's `triV[0] != triV[1] && ...` filter. Its
/// halfedges would otherwise poison the edge multiplicity and reject the whole import.
#[test]
fn degenerate_triangles_drop_at_ingest() {
    // A valid tetrahedron plus a (0, 1, 1) sliver.
    let verts: Vec<[f64; 3]> = vec![[0., 0., 0.], [1., 0., 0.], [0.5, 1., 0.], [0.5, 0.5, 1.]];
    let tris: Vec<[u32; 3]> = vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3], [0, 1, 1]];
    let m = Mesh::from_mesh_gl(&gl(&verts, &tris)).expect("degenerate dropped, tet accepted");
    assert!(m.is_manifold());
    assert_eq!(m.num_tri(), 4, "the sliver is gone");
    assert!(m.volume() > 0.0);
}
