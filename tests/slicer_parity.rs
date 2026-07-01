//! Slicer parity (Track C 11.5): the in-process kernel slice must carve the SAME pieces OpenSCAD's
//! `slice()` does. Opt-in — needs OpenSCAD, so `#[ignore]`d; run with:
//!   cargo test --test slicer_parity -- --ignored
//! Renders a base fixture once, slices it both in-process and through slicer.scad, and compares each
//! piece's bbox (exact) and triangle count (loose — cut-face triangulation differs by a few tris).

#![cfg(feature = "kernel")]

use fab_scad::kernel::Solid;
use fab_scad::openscad::Openscad;
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
        let list = cuts[a].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ");
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
    os.render(&fixture, &base_stl, Duration::from_secs(120)).expect("render base");
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
            std::fs::write(&drv, os_piece_driver(&cuts, *idx, base_stl.to_str().unwrap())).unwrap();
            let ostl = t("piece.stl");
            os.render(&drv, &ostl, Duration::from_secs(120)).expect("render piece");
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
