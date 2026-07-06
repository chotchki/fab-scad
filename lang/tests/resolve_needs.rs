//! M.3 — the eval-time File-needs fixpoint. `import`/`surface` reference a mesh file by a RUNTIME path,
//! so they can't be resolved statically like `use`/`include` (whose paths are literal tokens) — only by
//! EXECUTING to the call. fab-lang stays PURE: a path the caller's [`FileTable`] lacks comes back as a
//! [`Resolution::Incomplete`] naming it (and the run substitutes an empty placeholder + keeps going, so ONE
//! pass surfaces EVERY missing file), the impure caller reads the meshes + re-runs, iterating to
//! [`Resolution::Complete`]. These tests pin that loop's shape; a differential against the real STL/3MF
//! readers is M.5/M.6's job (this corpus supplies its own meshes, staying pure).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::path::Path;

use fab_lang::{
    Error, FileTable, Geo, GeoNode, Mesh, Resolution, Scope, SourceNeed, evaluate, eval_program,
    evaluate_geometry, parse, resolve_geometry_file, resolve_geometry_with_base,
};

/// Resolve in-memory `src` against `files`, CWD-based (the tests never reference `use`/`include`).
fn resolve(src: &str, files: &FileTable) -> Resolution {
    resolve_geometry_with_base(src, Path::new("."), &[], files).expect("resolves")
}

/// The File needs of an [`Resolution::Incomplete`] — panics if it CLOSED (a test that expected a miss).
fn needs(res: Resolution) -> Vec<SourceNeed> {
    match res {
        Resolution::Incomplete { needs } => needs,
        Resolution::Complete { .. } => panic!("expected Incomplete, the run closed"),
    }
}

/// The geometry tree of a [`Resolution::Complete`] — panics if it still needed something.
fn complete(res: Resolution) -> Geo {
    match res {
        Resolution::Complete { geo, .. } => geo,
        Resolution::Incomplete { needs } => panic!("expected Complete, still needs {needs:?}"),
    }
}

/// A real mesh to stand in for a read STL — `cube(2)` tessellated through our own engine.
fn a_mesh() -> Mesh {
    evaluate("cube(2);").expect("cube tessellates")
}

#[test]
fn a_missing_import_surfaces_a_file_need() {
    // No table, so `import` can't resolve — the run reports the file it wants instead of erroring.
    assert_eq!(
        needs(resolve("import(\"a.stl\");", &FileTable::new())),
        vec![SourceNeed::File {
            raw: "a.stl".to_string()
        }]
    );
    // `surface` rides the same channel (its heightmap is a File need too).
    assert_eq!(
        needs(resolve("surface(\"h.dat\");", &FileTable::new())),
        vec![SourceNeed::File {
            raw: "h.dat".to_string()
        }]
    );
}

#[test]
fn every_need_surfaces_in_one_round() {
    // The placeholder-continue proof: three files, ONE pass names all three (an empty mesh doesn't gate the
    // control flow that reaches the later imports). Deduped + deterministically ordered (the BTreeSet).
    let got = needs(resolve(
        "import(\"a.stl\"); import(\"b.stl\"); surface(\"c.dat\"); import(\"a.stl\");",
        &FileTable::new(),
    ));
    assert_eq!(
        got,
        vec![
            SourceNeed::File {
                raw: "a.stl".to_string()
            },
            SourceNeed::File {
                raw: "b.stl".to_string()
            },
            SourceNeed::File {
                raw: "c.dat".to_string()
            },
        ]
    );
}

#[test]
fn a_supplied_mesh_flows_through() {
    // The closing round: the table has the file, so `import` returns that mesh and the run CLOSES.
    let mesh = a_mesh();
    let mut files = FileTable::new();
    files.insert("a.stl".to_string(), mesh.clone());
    match complete(resolve("import(\"a.stl\");", &files)) {
        Geo::D3(GeoNode::Leaf(leaf)) => assert_eq!(leaf, mesh, "the imported mesh is the table's"),
        other => panic!("expected a single 3D leaf, got {other:?}"),
    }
}

#[test]
fn a_partial_table_reports_only_the_rest() {
    // The fixpoint CONVERGES: round 1 wanted a+b, the caller supplied a, round 2 wants only b. A present
    // file no longer re-needs; the missing one still does.
    let mut files = FileTable::new();
    files.insert("a.stl".to_string(), a_mesh());
    assert_eq!(
        needs(resolve("import(\"a.stl\"); import(\"b.stl\");", &files)),
        vec![SourceNeed::File {
            raw: "b.stl".to_string()
        }]
    );
}

#[test]
fn a_bad_file_path_is_empty_not_a_need() {
    // No resolvable path to name (an `undef`/non-string `file=`) → an empty result with NO need, matching
    // the oracle's warn-and-render on a bad path (the warning TEXT is #94/M.6). Complete, so the fixpoint
    // doesn't spin forever asking for a file that was never named.
    for src in ["import(undef);", "import(5);", "import();"] {
        match complete(resolve(src, &FileTable::new())) {
            Geo::D3(GeoNode::Leaf(leaf)) => {
                assert_eq!(leaf.tri_count(), 0, "{src}: empty placeholder");
            }
            other => panic!("{src}: expected an empty 3D leaf, got {other:?}"),
        }
    }
}

#[test]
fn resolve_geometry_file_reports_then_closes() {
    // The `.scad`-FILE entry (the shell's real driver): the root is read from disk; the imported mesh comes
    // from the table. Round 1 names the file, round 2 (table filled) closes.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join("imports_one.scad");
    std::fs::write(&root, "import(\"part.stl\");\n").expect("write root");

    assert_eq!(
        needs(resolve_geometry_file(&root, &[], &FileTable::new()).expect("resolves")),
        vec![SourceNeed::File {
            raw: "part.stl".to_string()
        }]
    );

    let mut files = FileTable::new();
    files.insert("part.stl".to_string(), a_mesh());
    assert!(matches!(
        complete(resolve_geometry_file(&root, &[], &files).expect("resolves")),
        Geo::D3(GeoNode::Leaf(_))
    ));
}

#[test]
fn the_no_table_paths_are_loud_not_silent() {
    // The convenience entries carry no table, so an import through them is unfulfillable — they fail LOUD
    // naming the file (never a silently-empty mesh), the M.3 upgrade over the old "import()" Unimplemented.
    for res in [
        evaluate_geometry("import(\"a.stl\");"),
        evaluate("import(\"a.stl\");").map(|_| Geo::D3(GeoNode::Empty)),
    ] {
        assert!(
            matches!(&res, Err(Error::Load(m)) if m.contains("a.stl")),
            "expected a LOUD Load error naming a.stl, got {res:?}"
        );
    }
    // The raw AST path (`eval_program`) guards it too — no eval path swallows a need into empty geometry.
    let prog = parse("import(\"a.stl\");").expect("parses");
    assert!(matches!(
        eval_program(&prog, &Scope::new()),
        Err(Error::Load(m)) if m.contains("a.stl")
    ));
}

#[test]
fn source_need_variants_are_distinct() {
    // The M.1 contract type carries both discovery phases: a Scad ref (loader, static) and a File ref (eval,
    // dynamic). M.3 only emits File; this locks the enum's Debug/Clone/eq over BOTH arms so M.4 inherits a
    // proven type when it folds the loader's own channel in here.
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
