//! BOSL2 parse smoke test — the pinned BOSL2 (`libs/BOSL2` submodule) is real-world OpenSCAD, so it's a
//! standing regression net for the parser. Skips when the submodule isn't checked out (CI must
//! `submodule update --init`). NOT the differential (that's Phase K) — just "does our grammar accept the
//! library everyone uses". Evaluation of BOSL2 is a separate, deeper effort (see PLAN — deep recursion +
//! the expression-depth cap surface here first).

use std::path::PathBuf;

fn bosl2() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libs/BOSL2")
}

#[test]
fn bosl2_parses() {
    let dir = bosl2();
    if !dir.join("std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 submodule not checked out");
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "scad"))
        .collect();
    files.sort();

    let mut failures = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        if let Err(e) = fab_lang::parse(&src) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let reason = format!("{e}")
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_string();
            failures.push((name, reason));
        }
    }

    let ok = files.len() - failures.len();
    eprintln!("BOSL2 parse: {ok}/{} ok", files.len());
    for (name, reason) in &failures {
        eprintln!("  FAIL {name}: {reason}");
    }
    // A floor, not exact — BOSL2 is a moving target, but a big regression (e.g. a broken statement rule)
    // must fail the gate. Known holdouts are deeply-nested expression bodies (the parser's depth cap).
    assert!(
        ok >= 50,
        "BOSL2 parse rate regressed: only {ok}/{} parse",
        files.len()
    );
}
