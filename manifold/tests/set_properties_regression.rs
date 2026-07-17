//! REGRESSION (M.7.3 flip): recoloring a COLORED boolean output. A boolean keeps distinct property
//! rows for coincident seam verts, so its prop-vert count exceeds its vert count — `set_properties`
//! sized the new row table by `num_vert` (C++ sizes by `NumPropVert()`) and blew up on the first
//! recolor of a colored union. Sweeps overlap patterns × ops, asserting the row-index invariant and
//! exercising the recolor path.
#[test]
fn recolor_of_colored_boolean_outputs() {
    use fab_manifold::boolean::{OpType, boolean_result::boolean};
    use fab_manifold::linalg::{Mat3x4, Vec3};
    use fab_manifold::mesh::Mesh;

    let color = |m: &Mesh, rgba: [f64; 4]| -> Mesh {
        m.set_properties(4, |new, _pos, _old| new.copy_from_slice(&rgba))
    };
    let cube = Mesh::cube(Vec3::new(10.0, 10.0, 10.0), false).unwrap();
    let a = color(&cube, [1.0, 0.0, 0.0, 1.0]);
    let mut worst = 0usize;
    for (dx, dy, dz) in [
        (5.0, 5.0, 5.0),
        (10.0, 0.0, 0.0),
        (3.0, 3.0, 0.0),
        (2.5, 0.0, 0.0),
        (0.0, 0.0, 9.5),
    ] {
        let b = color(
            &cube
                .transform(Mat3x4::translate(Vec3::new(dx, dy, dz)))
                .unwrap(),
            [0.0, 0.0, 1.0, 1.0],
        );
        for op in [OpType::Add, OpType::Subtract, OpType::Intersect] {
            let out = boolean(&a, &b, op);
            let rows = out.num_prop_vert();
            let max_pv = out
                .halfedge_ids()
                .map(|e| out.prop(e).u())
                .max()
                .unwrap_or(0);
            if max_pv >= rows {
                eprintln!(
                    "VIOLATION d=({dx},{dy},{dz}) op={op:?}: max prop_vert {max_pv} rows {rows}"
                );
                worst = worst.max(max_pv + 1 - rows);
            }
            // recolor exercises the OOB read
            let _ = color(&out, [0.0, 1.0, 0.0, 1.0]);
        }
    }
    assert_eq!(worst, 0, "prop_vert exceeded row count");
}
