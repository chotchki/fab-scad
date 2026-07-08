//! M.3/M.4 — the `import`/`surface` needs fixpoint, from the NATIVE driver's side. `import`/`surface`
//! reference a mesh file by a RUNTIME path (resolvable only by EXECUTING to the call, unlike `use`/`include`
//! whose paths are literal tokens); fab-lang stays PURE and hands each one to a caller-supplied `mesh_reader`
//! (path in → [`Mesh`] out) that fab-scad will back with real STL/3MF readers (M.5). The `io` shell loops
//! the pure inner step, reading `use`/`include` sources + calling the reader for meshes, until the run
//! closes. These tests pin the driver's contract; the pure need-surfacing itself is unit-tested in
//! `eval::mod` (`resolve_source` is crate-private). A differential against real mesh files is M.5/M.6.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::cell::RefCell;
use std::path::Path;

use fab_lang::{
    Error, Geo, GeoNode, Mesh, Scope, SourceNeed, evaluate, eval_program, evaluate_geometry, parse,
    resolve_geometry_file, resolve_geometry_with_base,
};

/// A real mesh to stand in for a read STL — `cube(2)` tessellated through our own engine.
fn a_mesh() -> Mesh {
    evaluate("cube(2);").expect("cube tessellates")
}

/// Resolve in-memory `src` (CWD base, no library paths) with `reader` fulfilling File needs.
fn resolve<R: FnMut(&str) -> Result<Mesh, Error>>(src: &str, reader: R) -> Result<Geo, Error> {
    resolve_geometry_with_base(src, Path::new("."), &[], reader)
}

#[test]
fn a_supplied_mesh_flows_through() {
    // The reader serves the file, so `import` returns that mesh and the run CLOSES to a single 3D leaf.
    let mesh = a_mesh();
    let served = mesh.clone();
    let geo = resolve("import(\"a.stl\");", move |raw| {
        assert_eq!(raw, "a.stl", "reader gets the literal file= path");
        Ok(served.clone())
    })
    .expect("resolves");
    match geo {
        Geo::D3(GeoNode::Leaf(ref leaf)) => {
            assert_eq!(*leaf, mesh, "the imported mesh is the reader's")
        }
        other => panic!("expected a single 3D leaf, got {other:?}"),
    }
}

#[test]
fn every_import_reaches_the_reader_in_one_round() {
    // The placeholder-continue proof from the driver's side: an empty placeholder mesh doesn't gate the
    // control flow reaching the later imports, so ONE resolve pass hands the reader BOTH files (sorted +
    // deduped by the need set), then closes. `surface` rides the same channel.
    let calls = RefCell::new(Vec::new());
    let geo = resolve(
        "import(\"a.stl\"); import(\"b.stl\"); surface(\"c.dat\"); import(\"a.stl\");",
        |raw| {
            calls.borrow_mut().push(raw.to_string());
            Ok(a_mesh())
        },
    )
    .expect("resolves");
    assert!(matches!(geo, Geo::D3(_)));
    assert_eq!(*calls.borrow(), vec!["a.stl", "b.stl", "c.dat"]);
}

#[test]
fn a_reader_error_fails_loud() {
    // The reader can't produce the file → the driver propagates it LOUD (never a silently-empty mesh).
    let err = resolve("import(\"gone.stl\");", |raw| {
        Err(Error::Load(format!("cannot read {raw}")))
    })
    .unwrap_err();
    assert!(
        matches!(&err, Error::Load(m) if m.contains("gone.stl")),
        "got {err:?}"
    );
}

#[test]
fn a_bad_file_path_never_calls_the_reader() {
    // No resolvable path to name (an `undef`/non-string `file=`) → an empty result with NO need, so the
    // reader is never invoked, matching the oracle's warn-and-render on a bad path (warning TEXT is #94).
    let calls = RefCell::new(0usize);
    for src in ["import(undef);", "import(5);", "import();"] {
        let geo = resolve(src, |_| {
            *calls.borrow_mut() += 1;
            Ok(a_mesh())
        })
        .expect("resolves");
        match geo {
            Geo::D3(GeoNode::Leaf(ref leaf)) => {
                assert_eq!(leaf.tri_count(), 0, "{src}: empty placeholder")
            }
            other => panic!("{src}: expected an empty 3D leaf, got {other:?}"),
        }
    }
    assert_eq!(*calls.borrow(), 0, "no file named → reader never called");
}

#[test]
fn resolve_geometry_file_reads_the_root_then_the_mesh() {
    // The `.scad`-FILE driver: the root is read from disk; the imported mesh comes from the reader.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join("imports_one.scad");
    std::fs::write(&root, "import(\"part.stl\");\n").expect("write root");

    let geo = resolve_geometry_file(&root, &[], |raw| {
        assert_eq!(raw, "part.stl");
        Ok(a_mesh())
    })
    .expect("resolves");
    assert!(matches!(geo, Geo::D3(GeoNode::Leaf(_))));
}

#[test]
fn the_no_reader_entries_are_loud_not_silent() {
    // The convenience entries carry NO reader, so an import through them fails LOUD naming the file (never
    // a silently-empty mesh) — the M.4 behavior over the old blanket "import() Unimplemented".
    for res in [
        evaluate_geometry("import(\"a.stl\");"),
        evaluate("import(\"a.stl\");").map(|_| Geo::D3(GeoNode::Empty)),
    ] {
        assert!(
            matches!(&res, Err(Error::Load(m)) if m.contains("a.stl")),
            "expected a LOUD Load error naming a.stl, got {res:?}"
        );
    }
    // The raw AST path (`eval_program`) guards it too — no eval path swallows an import into empty geometry.
    let prog = parse("import(\"a.stl\");").expect("parses");
    assert!(matches!(
        eval_program(&prog, &Scope::new()),
        Err(Error::Load(m)) if m.contains("a.stl")
    ));
}

#[test]
fn surface_center_translates_the_mesh_eval_side() {
    // center is applied EVAL-side (M.5.2): the path-only reader returns a natural-position mesh; surface's
    // `center` flag translates it to the XY origin. center_xy is a generic XY translate, so a cube(2) at
    // [0,2]³ stands in for a heightmap — plain stays at [0,2], centered shifts to [−1,1].
    let mesh = a_mesh(); // cube(2), XY bounds [0, 2]
    let x_range = |geo: Geo| match geo {
        Geo::D3(GeoNode::Leaf(ref m)) => {
            let lo = m.verts.iter().map(|v| v.x).fold(f64::INFINITY, f64::min);
            let hi = m.verts.iter().map(|v| v.x).fold(f64::NEG_INFINITY, f64::max);
            (lo, hi)
        }
        other => panic!("expected a 3D leaf, got {other:?}"),
    };
    let m1 = mesh.clone();
    let (plo, phi) = x_range(resolve("surface(file=\"h.dat\");", move |_| Ok(m1.clone())).unwrap());
    assert!(plo.abs() < 1e-9 && (phi - 2.0).abs() < 1e-9, "plain stays at [0,2], got [{plo},{phi}]");
    let m2 = mesh.clone();
    let (clo, chi) =
        x_range(resolve("surface(file=\"h.dat\", center=true);", move |_| Ok(m2.clone())).unwrap());
    assert!(
        (clo + 1.0).abs() < 1e-9 && (chi - 1.0).abs() < 1e-9,
        "center shifts XY bounds to the origin [−1,1], got [{clo},{chi}]"
    );
}

#[test]
fn source_need_variants_are_distinct() {
    // The M.1 contract type carries both discovery phases: a Scad ref (loader, static) and a File ref (eval,
    // dynamic). This locks the enum's Debug/Clone/eq over BOTH arms — the seam an async wasm host builds on.
    let scad = SourceNeed::Scad {
        from_dir: Path::new("/lib").to_path_buf(),
        raw: "part.scad".to_string(),
    };
    let file = SourceNeed::File {
        raw: "part.stl".to_string(),
    };
    assert_eq!(scad.clone(), scad);
    assert_eq!(file.clone(), file);
    assert_ne!(scad, file);
    assert!(format!("{scad:?} {file:?}").contains("part.stl"));
}
