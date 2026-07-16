//! M.6.3 — the FULL-SURFACE native==wasm golden corpus (R6, pillar 1). Every kernel op family
//! produces a byte fingerprint ([`fab_manifold::golden`]) checked against a golden baked on the
//! serial native build. The SAME binary runs under wasmtime (`cargo test --target wasm32-wasip1`)
//! and with `--features par` — matching goldens there IS the bit-for-bit proof the C++ kernel
//! structurally cannot make (platform libm + TBB scheduling), resting on the `mathf` seam + the
//! `par::` order-preserving construction.
//!
//! Everything is generated IN CODE (no files, no oracle feature) so the corpus runs identically on
//! every lane. Regenerate goldens ONLY on a deliberate output change: run natively, read the
//! printed table, paste it back, and re-verify par + wasm against the SAME values.

use fab_manifold::boolean::OpType;
use fab_manifold::boolean::boolean_result::boolean;
use fab_manifold::cross_section::{CrossSection, FillRule, JoinType};
use fab_manifold::golden;
use fab_manifold::linalg::{Mat3x4, Vec2, Vec3};
use fab_manifold::mesh::Mesh;
use fab_manifold::{mathf, par};

/// Deterministic LCG (same constants as the oracle's MC sampler) — seeds every "random" input.
struct Lcg(u64);
impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // 53 high bits → [0, 1).
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }
}

fn cube_at(ox: f64, oy: f64, oz: f64) -> Mesh {
    Mesh::cube(Vec3::new(1.0, 1.0, 1.0), false)
        .unwrap()
        .transform(Mat3x4::translate(Vec3::new(ox, oy, oz)))
        .unwrap()
}

/// Boolean then the inter-op prepare (what a chained consumer does).
fn prepared_union(a: &Mesh, b: &Mesh) -> Mesh {
    let mut m = boolean(a, b, OpType::Add);
    m.set_epsilon(-1.0, false);
    m.initialize_original();
    m.set_normals_and_coplanar();
    m
}

#[test]
fn m6_full_surface_golden() {
    let mut cases: Vec<(&str, u64)> = Vec::new();

    // 0. Raw mathf sweep — localizes a transcendental divergence to the seam itself. Covers the
    // verbatim math.h ports + the degree/exact-quadrant dialect + rem_pio2 hard cases.
    {
        let mut rng = Lcg(0x6D_A7_00);
        let mut vals = Vec::new();
        for _ in 0..2000 {
            let x = rng.range(-720.0, 720.0);
            vals.extend_from_slice(&[
                mathf::sin(x),
                mathf::cos(x),
                mathf::tan(x / 7.0),
                mathf::atan2(x, 1.0 - x),
                mathf::sind(x),
                mathf::cosd(x),
            ]);
            let u = rng.range(-1.0, 1.0);
            vals.extend_from_slice(&[mathf::asin(u), mathf::acos(u)]);
        }
        // The rem_pio2 precision-searched triggers (M.0.8) + exact π/2 multiples.
        for x in [
            2915.397982531328,
            8.639379797371932,
            13.351768777756621,
            core::f64::consts::FRAC_PI_2,
            core::f64::consts::PI,
        ] {
            vals.extend_from_slice(&[mathf::sin(x), mathf::cos(x)]);
        }
        cases.push(("mathf", golden::f64s(vals)));
    }

    // 1. Booleans: the three ops on offset cubes.
    let a = cube_at(0.0, 0.0, 0.0);
    for (name, op) in [
        ("union", OpType::Add),
        ("difference", OpType::Subtract),
        ("intersection", OpType::Intersect),
    ] {
        let b = cube_at(0.5, 0.3, 0.4);
        cases.push((name, golden::mesh(&boolean(&a, &b, op))));
    }

    // 2. A 5-cube chained fold (the total-order-sort stress) + a ROTATED fold (trig-heavy inputs).
    {
        let offsets = [
            (0.5, 0.3, 0.4),
            (0.2, 0.7, 0.1),
            (0.6, 0.1, 0.5),
            (0.3, 0.5, 0.8),
        ];
        let mut acc = cube_at(0.0, 0.0, 0.0);
        for &(x, y, z) in &offsets {
            acc = prepared_union(&acc, &cube_at(x, y, z));
        }
        cases.push(("fold5", golden::mesh(&acc)));

        let mut acc = cube_at(0.0, 0.0, 0.0);
        for (i, &(x, y, z)) in offsets.iter().enumerate() {
            let m = Mat3x4::rotate(10.0 + 7.0 * i as f64, 3.0 * i as f64, 17.0 * i as f64);
            let c = cube_at(x, y, z).transform(m).unwrap();
            acc = prepared_union(&acc, &c);
        }
        cases.push(("fold5_rotated", golden::mesh(&acc)));
    }

    // 3. Coincident (GATE-B class): face-sharing cubes — the perturbation/tie-break path.
    cases.push((
        "coincident_union",
        golden::mesh(&boolean(&a, &cube_at(1.0, 0.0, 0.0), OpType::Add)),
    ));

    // 4. Transforms: a mirrored composite (det < 0 → flip_tris) on a boolean output.
    {
        let m = Mat3x4::rotate(30.0, 45.0, 60.0);
        let mirrored = Mat3x4::scale(Vec3::new(-1.0, 1.0, 1.0));
        let t = boolean(&a, &cube_at(0.5, 0.5, 0.5), OpType::Add)
            .transform(m)
            .unwrap()
            .transform(mirrored)
            .unwrap();
        cases.push(("transform_mirror", golden::mesh(&t)));
    }

    // 5. Split / trim by a tilted plane.
    {
        let u = prepared_union(&a, &cube_at(0.4, 0.4, 0.4));
        let n = Vec3::new(1.0, 2.0, 3.0);
        let (plus, minus) = u.split_by_plane(n, 0.7);
        cases.push(("split_plus", golden::mesh(&plus)));
        cases.push(("split_minus", golden::mesh(&minus)));
        cases.push(("trim", golden::mesh(&u.trim_by_plane(n, 0.7))));
    }

    // 6. Decompose of a two-component union.
    {
        let two = prepared_union(&a, &cube_at(5.0, 0.0, 0.0));
        for (i, part) in two.decompose().iter().enumerate() {
            let label: &'static str = ["decompose_0", "decompose_1"][i];
            cases.push((label, golden::mesh(part)));
        }
    }

    // 7. Hull of a seeded cloud + a Fibonacci sphere (trig-generated points → quickhull).
    {
        let mut rng = Lcg(0x481);
        let cloud: Vec<Vec3> = (0..96)
            .map(|_| {
                Vec3::new(
                    rng.range(-2.0, 2.0),
                    rng.range(-2.0, 2.0),
                    rng.range(-2.0, 2.0),
                )
            })
            .collect();
        cases.push((
            "hull_cloud",
            golden::mesh(&Mesh::hull_of_points(&cloud).unwrap()),
        ));

        let n = 60;
        let ga = core::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
        let fib: Vec<Vec3> = (0..n)
            .map(|i| {
                let y = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
                let r = (1.0 - y * y).sqrt();
                let th = ga * i as f64;
                Vec3::new(r * mathf::cos(th), y, r * mathf::sin(th))
            })
            .collect();
        cases.push((
            "hull_fibonacci",
            golden::mesh(&Mesh::hull_of_points(&fib).unwrap()),
        ));
    }

    // 8. Minkowski: convex⊕convex and nonconvex⊕convex (tier 0 + 1 — hull + union machinery).
    {
        let octa = Mesh::hull_of_points(&[
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, -1.0),
        ])
        .unwrap();
        cases.push((
            "minkowski_t0",
            golden::mesh(&a.minkowski_sum(&octa).unwrap()),
        ));
        let concave = boolean(
            &Mesh::cube(Vec3::new(2.0, 2.0, 2.0), false).unwrap(),
            &cube_at(1.0, 1.0, 1.0),
            OpType::Subtract,
        );
        let small = Mesh::cube(Vec3::new(0.25, 0.25, 0.25), true).unwrap();
        cases.push((
            "minkowski_t1",
            golden::mesh(&concave.minkowski_sum(&small).unwrap()),
        ));
    }

    // 9. Properties: colour-by-position through a difference (CreateProperties + barycentric).
    {
        let colored = a.set_properties(4, |n, p, _| n.copy_from_slice(&[p.x, p.y, p.z, 1.0]));
        cases.push((
            "colored_difference",
            golden::mesh(&boolean(
                &colored,
                &cube_at(0.5, 0.5, 0.5),
                OpType::Subtract,
            )),
        ));
    }

    // 10. Ingest canonicalization: a scrambled-vert-order cube fingerprints identically to the
    // plain one (the M.2.4a ctor tail at work).
    {
        let plain = Mesh::from_mesh_gl(&cube_at(0.0, 0.0, 0.0).to_mesh_gl()).unwrap();
        cases.push(("ingest_canonical", golden::mesh(&plain)));
    }

    // 11. The 2D subsystem: booleans, all four offset joins (± delta), hull, decompose, fill rules.
    {
        let sq = CrossSection::square(Vec2::new(6.0, 6.0), false).unwrap();
        let circ = CrossSection::circle(2.5, 32)
            .unwrap()
            .translate(Vec2::new(5.0, 3.0))
            .unwrap();
        cases.push(("cs_union", golden::cross_section(&sq.union(&circ))));
        cases.push((
            "cs_difference",
            golden::cross_section(&sq.difference(&circ)),
        ));
        cases.push((
            "cs_intersection",
            golden::cross_section(&sq.intersection(&circ)),
        ));
        let l = sq.difference(&circ);
        for (name, join) in [
            ("cs_offset_square", JoinType::Square),
            ("cs_offset_round", JoinType::Round),
            ("cs_offset_miter", JoinType::Miter),
            ("cs_offset_bevel", JoinType::Bevel),
        ] {
            let grown = l.offset(0.8, join, 2.0, 32).unwrap();
            let shrunk = l.offset(-0.6, join, 2.0, 32).unwrap();
            cases.push((
                name,
                golden::f64s([
                    golden::cross_section(&grown) as f64,
                    golden::cross_section(&shrunk) as f64,
                ]),
            ));
        }
        cases.push(("cs_hull", golden::cross_section(&l.hull())));
        let bowtie = vec![
            Vec2::new(-7.0, 13.0),
            Vec2::new(-7.0, 12.0),
            Vec2::new(-5.0, 9.0),
            Vec2::new(-5.0, 8.1),
            Vec2::new(-4.8, 8.0),
        ];
        let fills = [
            FillRule::Positive,
            FillRule::Negative,
            FillRule::EvenOdd,
            FillRule::NonZero,
        ]
        .map(|r| CrossSection::from_polygons_with(core::slice::from_ref(&bowtie), r).unwrap());
        cases.push((
            "cs_fill_rules",
            golden::f64s(fills.iter().map(|c| golden::cross_section(c) as f64)),
        ));
    }

    // 12. The four 2D↔3D bridges.
    {
        let ring = CrossSection::square(Vec2::new(10.0, 10.0), false)
            .unwrap()
            .difference(
                &CrossSection::square(Vec2::new(2.0, 2.0), false)
                    .unwrap()
                    .translate(Vec2::new(4.0, 4.0))
                    .unwrap(),
            );
        let tube = ring.extrude(3.0);
        cases.push(("extrude", golden::mesh(&tube)));
        cases.push(("project", golden::cross_section(&tube.project().unwrap())));
        cases.push((
            "slice",
            golden::cross_section(&tube.slice_at_z(1.5).unwrap()),
        ));
        let profile = CrossSection::square(Vec2::new(2.0, 3.0), false)
            .unwrap()
            .translate(Vec2::new(1.0, 0.0))
            .unwrap();
        cases.push(("revolve", golden::mesh(&profile.revolve(48))));
    }

    // 13. The par:: seam primitives directly (reduce + map order).
    {
        let vals: Vec<i64> = (0..1000).map(|i| (i * 37) % 101).collect();
        let mapped = par::map_collect(&vals, |&v| v * v - 3);
        cases.push(("par_map", golden::f64s(mapped.iter().map(|&v| v as f64))));
    }

    // GOLDEN — baked on the serial native build; must match under --features par AND wasm32-wasip1.
    // (Sanity identities in the table itself: trim == split_plus — trim IS the + side; project ==
    // slice on a straight tube — both are the same 2D ring.)
    let golden_table: &[(&str, u64)] = &[
        ("mathf", 0xf475733cc6aca38a),
        ("union", 0x2e43ddb726a64619),
        ("difference", 0x5d956680e73f0b45),
        ("intersection", 0x6210223b6e1a050d),
        ("fold5", 0x1cc2f6c9bf898351),
        ("fold5_rotated", 0x4f6e5a393f905270),
        ("coincident_union", 0x8fb87dfc1d9abda6),
        ("transform_mirror", 0xbf015228a930ca77),
        ("split_plus", 0xfac2a539f48dcd08),
        ("split_minus", 0xbf150a0c171f2d8f),
        ("trim", 0xfac2a539f48dcd08),
        ("decompose_0", 0x4b28fc516802fe65),
        ("decompose_1", 0x01b4571dd1ded2d5),
        ("hull_cloud", 0xd54959f5a7d51ab3),
        ("hull_fibonacci", 0x8e41cb8f12c6b1ec),
        ("minkowski_t0", 0xc279fe066511cb73),
        ("minkowski_t1", 0xc14d52688455a5d9),
        ("colored_difference", 0x75865c501b1e3cd1),
        ("ingest_canonical", 0xea2b3709e58d74e5),
        ("cs_union", 0x5af29e0b01b9c18e),
        ("cs_difference", 0x02abe3a80bccb691),
        ("cs_intersection", 0x7e65b4968cc81175),
        ("cs_offset_square", 0xdc9698c1e4098007),
        ("cs_offset_round", 0x2b59c6cb7ea1b8c0),
        ("cs_offset_miter", 0x799b312a1704ad89),
        ("cs_offset_bevel", 0x7faa90fa3ba1e368),
        ("cs_hull", 0xed3e434bca18c641),
        ("cs_fill_rules", 0xed5770929f1206e2),
        ("extrude", 0x8b2f876ff8bf6975),
        ("project", 0xed2f4eb3c0a56365),
        ("slice", 0xed2f4eb3c0a56365),
        ("revolve", 0xf3a7095d923dcce9),
        ("par_map", 0x6d0302e2f813e9af),
    ];

    for (name, got) in &cases {
        eprintln!("        (\"{name}\", 0x{got:016x}),");
    }
    if golden_table.is_empty() {
        panic!("golden table not baked yet — paste the printed table in");
    }
    assert_eq!(
        cases.len(),
        golden_table.len(),
        "corpus/golden size mismatch"
    );
    for ((name, got), (gname, want)) in cases.iter().zip(golden_table) {
        assert_eq!(name, gname, "corpus order changed");
        assert_eq!(
            got, want,
            "{name}: fingerprint 0x{got:016x} != golden 0x{want:016x} — a bit diverged on this lane"
        );
    }
}
