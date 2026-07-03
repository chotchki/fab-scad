//! Slicer parity (Track C 11.5): the in-process kernel slice must carve the SAME pieces OpenSCAD's
//! `slice()` does. Opt-in — needs OpenSCAD, so `#[ignore]`d; run with:
//!   cargo test --test slicer_parity -- --ignored
//! Renders a base fixture once, slices it both in-process and through slicer.scad, and compares each
//! piece's bbox (exact) and triangle count (loose — cut-face triangulation differs by a few tris).

#![cfg(feature = "kernel")]

use fab_scad::cross_section;
use fab_scad::kernel::Solid;
use fab_scad::manifest::Manifest;
use fab_scad::openscad::Openscad;
use fab_scad::slicing::{piece_driver, slice_solid};
use std::path::PathBuf;
use std::time::Duration;

/// The OpenSCAD driver for one piece — `slice(only=)` per cut axis around the base mesh. This is
/// exactly `slicing::piece_driver` minus connectors, so it exercises fab's real slice() module.
fn os_piece_driver(cuts: &[Vec<f64>; 3], piece: [usize; 3], base_stl: &str) -> String {
    let axis = ["RIGHT", "BACK", "UP"];
    let mut s = String::from("include <slicer.scad>\n");
    for a in 0..3 {
        if cuts[a].is_empty() {
            continue;
        }
        let list = cuts[a]
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        s += &format!("slice([{list}], axis = {}, only = {})\n", axis[a], piece[a]);
    }
    s += &format!("import(\"{base_stl}\");\n");
    s
}

fn approx(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < tol)
}

#[test]
#[ignore = "needs OpenSCAD; run with --ignored"]
fn slab_pieces_match_openscad() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Ok(os) = Openscad::discover(Some(root.as_path())) else {
        eprintln!("skipping: OpenSCAD not found");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("fab_parity_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let t = |n: &str| tmp.join(n);

    // An ASYMMETRIC fixture (notched box) so cut faces aren't trivially identical under triangulation.
    let fixture = t("fixture.scad");
    std::fs::write(
        &fixture,
        "difference(){ cube([60,40,30],center=true); translate([12,6,0]) cylinder(h=40,d=16,center=true,$fn=48); }",
    )
    .unwrap();
    let base_stl = t("base.stl");
    os.render(&fixture, &base_stl, Duration::from_secs(120))
        .expect("render base");
    let base = Solid::from_stl_file(&base_stl).expect("import base");

    // One-axis (3 slabs) and two-axis (4 cells — the floater-prone case).
    for cuts in [
        [vec![-10.0, 10.0], vec![], vec![]],
        [vec![8.0], vec![0.0], vec![]],
    ] {
        let pieces = base.slab_pieces(&cuts);
        assert!(!pieces.is_empty(), "kernel produced no pieces for {cuts:?}");
        for (idx, kpiece) in &pieces {
            // OpenSCAD's version of this same piece.
            let drv = t("piece.scad");
            std::fs::write(
                &drv,
                os_piece_driver(&cuts, *idx, base_stl.to_str().unwrap()),
            )
            .unwrap();
            let ostl = t("piece.stl");
            os.render(&drv, &ostl, Duration::from_secs(120))
                .expect("render piece");
            let opiece = Solid::from_stl_file(&ostl).expect("import piece");

            let (kmin, kmax) = kpiece.bbox().unwrap();
            let (omin, omax) = opiece.bbox().unwrap();
            assert!(approx(kmin, omin, 1e-3) && approx(kmax, omax, 1e-3),
                "bbox mismatch cuts={cuts:?} piece={idx:?}: kernel {kmin:?}..{kmax:?} vs os {omin:?}..{omax:?}");

            let (kt, ot) = (kpiece.num_tri() as i64, opiece.num_tri() as i64);
            let tol = (ot / 10).max(24); // cut-face triangulation differs; allow ~10% or 24 tris
            assert!((kt - ot).abs() <= tol,
                "tri-count mismatch cuts={cuts:?} piece={idx:?}: kernel {kt} vs os {ot} (tol {tol})");
        }
        eprintln!("parity OK for cuts {cuts:?}: {} pieces", pieces.len());
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Corpus robustness (Track C 11.11): the spike's flagged risk was whether Manifold booleans hold up
/// on arbitrary real geometry (not just cubes). Sample real models across the tree, render each base,
/// import, and slab-slice on X+Y — every piece must come back a valid 2-manifold. Reports the ratio.
#[test]
#[ignore = "needs OpenSCAD + models/; run with --ignored"]
fn corpus_robustness() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("models");
    if !models.is_dir() {
        eprintln!("skipping: no models/ submodule");
        return;
    }
    let Ok(os) = Openscad::discover(Some(root.as_path())) else {
        eprintln!("skipping: OpenSCAD not found");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("fab_corpus_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let stl = tmp.join("base.stl");

    // Spread the sample across the (sorted) corpus rather than the first N alphabetically.
    let files = fab_scad::smoke::scad_files(&models);
    let step = (files.len() / 25).max(1);
    let (mut tested, mut ok) = (0, 0);
    let mut failures = Vec::new();
    for f in files.iter().step_by(step) {
        if tested >= 15 {
            break;
        }
        match os.render(f, &stl, Duration::from_secs(60)) {
            Ok(r) if r.ok => {}
            _ => continue, // base didn't render — not this test's concern
        }
        let Ok(base) = Solid::from_stl_file(&stl) else {
            tested += 1;
            failures.push(format!("{}: import", f.display()));
            continue;
        };
        let Some((min, max)) = base.bbox() else {
            continue;
        };
        let cuts = [
            vec![(min[0] + max[0]) / 2.0],
            vec![(min[1] + max[1]) / 2.0],
            vec![],
        ];
        let pieces = base.slab_pieces(&cuts);
        tested += 1;
        if !pieces.is_empty() && pieces.iter().all(|(_, s)| s.is_manifold()) {
            ok += 1;
        } else {
            failures.push(format!(
                "{}: {} pieces, non-manifold",
                f.display(),
                pieces.len()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!("corpus robustness: {ok}/{tested} models sliced to all-manifold pieces");
    for fail in &failures {
        eprintln!("  FAIL {fail}");
    }
    assert!(tested > 0, "no models rendered — corpus check ran nothing");
    assert!(
        failures.is_empty(),
        "{} corpus models failed the in-process slice",
        failures.len()
    );
}

#[test]
#[ignore = "needs OpenSCAD; run with --ignored"]
fn connectors_match_openscad() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Ok(os) = Openscad::discover(Some(root.as_path())) else {
        eprintln!("skipping: OpenSCAD not found");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("fab_conn_parity_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let t = |n: &str| tmp.join(n);

    // A plain box, one X-cut, one feasible onion on it (both pieces build +Z ⇒ cap +Z, support-free).
    let fixture = t("box.scad");
    std::fs::write(&fixture, "cube([60,40,30], center=true);").unwrap();
    let base_stl = t("box.stl");
    os.render(&fixture, &base_stl, Duration::from_secs(120))
        .expect("render base");
    let base = Solid::from_stl_file(&base_stl).expect("import base");

    let s = ::toml::from_str::<Manifest>(
        "[project]\nname=\"t\"\n[slicing]\n\
         [[slicing.cut]]\naxis=\"x\"\nat=0\n\
         [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=12\n",
    )
    .unwrap()
    .slicing
    .unwrap();

    let kpieces = slice_solid(&s, &base).expect("kernel slice");
    assert_eq!(kpieces.len(), 2, "one cut -> two pieces");
    for (idx, kpiece) in &kpieces {
        // scad's version of the SAME piece — piece_driver emits the feasible onion into slice().
        let drv = t("piece.scad");
        std::fs::write(
            &drv,
            piece_driver(&s, base_stl.to_str().unwrap(), *idx).unwrap(),
        )
        .unwrap();
        let ostl = t("piece.stl");
        os.render(&drv, &ostl, Duration::from_secs(120))
            .expect("render scad piece");
        let opiece = Solid::from_stl_file(&ostl).expect("import scad piece");

        // Same onion radius + cap in both engines ⇒ the peg extends the bbox the same amount. Allow
        // 0.5mm for the two tessellations (BOSL2 onion vs sphere∪cone).
        let (kmin, kmax) = kpiece.bbox().unwrap();
        let (omin, omax) = opiece.bbox().unwrap();
        assert!(approx(kmin, omin, 0.5) && approx(kmax, omax, 0.5),
            "connector bbox mismatch piece {idx:?}: kernel {kmin:?}..{kmax:?} vs os {omin:?}..{omax:?}");
        eprintln!(
            "connector parity OK piece {idx:?}: kernel {} tris, os {} tris",
            kpiece.num_tri(),
            opiece.num_tri()
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

fn approx2(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
    (0..2).all(|k| (a[k] - b[k]).abs() < tol)
}

fn bbox2(loops: &[Vec<[f64; 2]>]) -> ([f64; 2], [f64; 2]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for lp in loops {
        for p in lp {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    (lo, hi)
}

/// The in-process kernel cross-section must describe the SAME cut profile OpenSCAD's projection does:
/// same extent (coords + convention) and same loop count (a hole shows up in both). Fixture is a
/// 100×80×40 box with an off-centre Z through-hole.
#[test]
#[ignore = "needs OpenSCAD; run with --ignored"]
fn cross_section_matches_openscad() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Ok(os) = Openscad::discover(Some(root.as_path())) else {
        eprintln!("skipping: OpenSCAD not found");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("fab_xsec_parity_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let t = |n: &str| tmp.join(n);
    let fixture = t("fixture.scad");
    std::fs::write(
        &fixture,
        "difference(){ cube([100,80,40],center=true); translate([12,-6,0]) cylinder(h=60,d=24,center=true,$fn=64); }",
    )
    .unwrap();
    let base_stl = t("base.stl");
    assert!(
        os.render(&fixture, &base_stl, Duration::from_secs(60))
            .unwrap()
            .ok
    );
    let solid = Solid::from_stl_file(&base_stl).unwrap();

    // (axis, at, expected loop count, expected pos bbox). Z@0 slices THROUGH the hole (outer+hole=2);
    // X@-30 and Y@30 miss the hole (plain rectangle=1). Bboxes double as the coord-convention check:
    // Z→(x,y), X→(y,z), Y→(x,z).
    let cases = [
        (2usize, 0.0f64, 2, ([-50.0, -40.0], [50.0, 40.0])),
        (0, -30.0, 1, ([-40.0, -20.0], [40.0, 20.0])),
        (1, 30.0, 1, ([-50.0, -20.0], [50.0, 20.0])),
    ];
    for &(axis, at, want_loops, (want_lo, want_hi)) in &cases {
        let os_loops =
            cross_section::cross_section(&os, &base_stl, axis, at, &tmp, Duration::from_secs(60))
                .unwrap();
        let ks_loops = solid.cross_section(axis, at);
        assert_eq!(
            os_loops.len(),
            want_loops,
            "OpenSCAD axis {axis} loop count"
        );
        assert_eq!(ks_loops.len(), want_loops, "kernel axis {axis} loop count");
        let (klo, khi) = bbox2(&ks_loops);
        let (olo, ohi) = bbox2(&os_loops);
        assert!(
            approx2(klo, want_lo, 0.6) && approx2(khi, want_hi, 0.6),
            "kernel axis {axis} bbox {klo:?}..{khi:?}"
        );
        assert!(
            approx2(olo, want_lo, 0.6) && approx2(ohi, want_hi, 0.6),
            "os axis {axis} bbox {olo:?}..{ohi:?}"
        );
        eprintln!("xsec parity OK axis {axis}: {want_loops} loop(s), bbox {klo:?}..{khi:?}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
