//! I.2.6 — the `use`/`include` LOADER corpus. Every case drives a real multi-file `.scad` graph off
//! disk and asserts the loader's behavior against the INLINED equivalent through our own engine: if
//! `use <lib>; sphere(r(), …)` doesn't produce the same mesh as `sphere(8, …)`, the loader is wrong.
//! That equivalence is the self-contained comparison test — a differential against the real OpenSCAD
//! binary is K's job (the harness owns the oracle; fab-lang stays pure).
//!
//! Fixtures are materialized once into `CARGO_TARGET_TMPDIR` (deterministic path, CI-safe, no repo
//! clutter) from the literals below — so the corpus reads as ONE file even though it's a file graph.
//!
//! Semantics pinned here, all verified against the OpenSCAD source (parsersettings.cc / lexer.l /
//! ScopeContext.cc / LocalScope.cc):
//! - `include` splices the file's statements (vars + geometry) at the include point, in the shared
//!   scope; `use` imports only its function defs, executing NOTHING.
//! - precedence: local/include beats `use` (position-independent); last-def-wins within local/include;
//!   last-`use`-wins across `use`s.
//! - resolution: including file's dir first, then library paths in order.
//! - a cycle breaks (no hang); a diamond re-splices (parse-once, no error); a missing file is LOUD.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test harness: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fab_lang::{Error, Mesh, evaluate, evaluate_file, evaluate_with_base};

/// Every fixture in the graph, as `(relative path, contents)`. Subdirectories (e.g. `lib/`) are created
/// on demand. Kept together so the whole multi-file corpus is reviewable in one place.
const FIXTURES: &[(&str, &str)] = &[
    // shared leaves
    ("consts.scad", "size = 3;\n"),
    (
        "lib_math.scad",
        "function r() = 8;\nfunction dbl(x) = x * 2;\n",
    ),
    ("lib_r5.scad", "function r() = 5;\n"),
    // a used file exporting a MODULE (not just functions) — I.2.4's use-imported-module path
    ("lib_mod.scad", "module libbox(s) cube(s);\n"),
    ("use_module.scad", "use <lib_mod.scad>\nlibbox(4);\n"),
    // a lib that ALSO has a top-level var + geometry — `use` must run NEITHER
    (
        "lib_with_geom.scad",
        "noise = 99;\nsphere(50, $fn = 64);\nfunction gr() = 6;\n",
    ),
    // include splices a var into the shared scope
    (
        "include_var.scad",
        "include <consts.scad>\nsphere(size, $fn = 8);\n",
    ),
    // use imports a function
    (
        "use_fn.scad",
        "use <lib_math.scad>\nsphere(r(), $fn = 16);\n",
    ),
    // local def beats the used def (position-independent — the use is FIRST here)
    (
        "local_wins.scad",
        "use <lib_math.scad>\nfunction r() = 2;\nsphere(r(), $fn = 8);\n",
    ),
    // two uses define r(): the LAST use wins (lib_r5 → 5, over lib_math → 8)
    (
        "use_order.scad",
        "use <lib_math.scad>\nuse <lib_r5.scad>\nsphere(r(), $fn = 8);\n",
    ),
    // use imports only defs — the lib's own sphere(50) must not fire (else 2 objects → error)
    (
        "use_no_exec.scad",
        "use <lib_with_geom.scad>\nsphere(gr(), $fn = 8);\n",
    ),
    // cycle: a includes b, b includes a → breaks; a's own sphere still renders
    (
        "cycle_a.scad",
        "include <cycle_b.scad>\nsphere(2, $fn = 8);\n",
    ),
    ("cycle_b.scad", "include <cycle_a.scad>\nx = 1;\n"),
    // diamond: top includes left+right, both include shared (v=7) → parse-once, re-spliced, no error
    (
        "d_top.scad",
        "include <d_left.scad>\ninclude <d_right.scad>\nsphere(v, $fn = 8);\n",
    ),
    ("d_left.scad", "include <d_shared.scad>\n"),
    ("d_right.scad", "include <d_shared.scad>\n"),
    ("d_shared.scad", "v = 7;\n"),
    // a used file that itself INCLUDEs another → the include's defs fold into the export (transitive
    // through include, though `use` itself is not transitive)
    ("lib_inner.scad", "function inner_r() = 3;\n"),
    (
        "lib_outer.scad",
        "include <lib_inner.scad>\nfunction outer_r() = 9;\n",
    ),
    (
        "use_transitive_include.scad",
        "use <lib_outer.scad>\nsphere(inner_r(), $fn = 8);\n",
    ),
    // a used file whose include graph CYCLES on itself — export still terminates + collects its def
    (
        "lib_selfcycle.scad",
        "include <lib_selfcycle.scad>\nfunction sc_r() = 7;\n",
    ),
    (
        "use_selfcycle.scad",
        "use <lib_selfcycle.scad>\nsphere(sc_r(), $fn = 8);\n",
    ),
    // a used file that itself `use`s another → the inner `use` is NOT re-exported (non-transitive):
    // lu_r() is visible through lib_uses_inner, but inner_r() is not.
    (
        "lib_uses_inner.scad",
        "use <lib_inner.scad>\nfunction lu_r() = 2;\n",
    ),
    (
        "use_nontransitive.scad",
        "use <lib_uses_inner.scad>\nsphere(lu_r(), $fn = 8);\n",
    ),
    (
        "use_nontransitive_reach.scad",
        "use <lib_uses_inner.scad>\nsphere(inner_r(), $fn = 8);\n",
    ),
    // OBSERVABLE diamond: a shared file with GEOMETRY reached via BOTH arms splices TWICE → two
    // top-level objects. (A counter can't prove it — hoisting makes `n = n + 1` twice → undef, per the
    // oracle; and a constant is dedup-invariant. Geometry duplication is the only echo-free signal.)
    ("dup_geom_shared.scad", "cube(1);\n"),
    ("dup_geom_left.scad", "include <dup_geom_shared.scad>\n"),
    ("dup_geom_right.scad", "include <dup_geom_shared.scad>\n"),
    (
        "dup_geom_top.scad",
        "include <dup_geom_left.scad>\ninclude <dup_geom_right.scad>\n",
    ),
    // use imports FUNCTIONS only — a used file's top-level var (noise = 99 in lib_with_geom) is NOT
    // imported, so reading it in the using scope yields undef.
    (
        "use_var.scad",
        "use <lib_with_geom.scad>\nr = noise;\nsphere(r, $fn = 8);\n",
    ),
    // library-path resolution: pathlib lives in lib/, reachable only via a library path
    ("lib/pathlib.scad", "function pr() = 4;\n"),
    (
        "use_via_libpath.scad",
        "use <pathlib.scad>\nsphere(pr(), $fn = 8);\n",
    ),
    // I.9.5 — per-file MODULE scope islands. A `use`d file's module resolves ITS body against ITS OWN
    // island, so a name the INCLUDING program redefines still reaches the BUILTIN here. This is BOSL2's
    // `builtins.scad` `module _cube(size) cube(size);` trick reduced to one pair: `cube(4)` calls the
    // root's `cube`, whose body calls `_cube`, whose body's `cube` must be the BUILTIN — not the root's
    // redefinition. A global module store resolves that inner `cube` back to the redefinition →
    // unbounded `cube → _cube → cube → …` recursion (the exact I.9.5 attachable-path symptom).
    ("lib_shadow.scad", "module _cube(size) cube(size);\n"),
    (
        "use_shadow_builtin.scad",
        "use <lib_shadow.scad>\nmodule cube(size) { _cube(size); }\ncube(4);\n",
    ),
    // I.9.5 — TWO included files both `use` the SAME lib → the lib gets ONE island (dedup), reached from
    // both include arms. Mirrors real BOSL2, where color/shapes2d/shapes3d all `use <builtins.scad>`.
    // The shared `_sq` still resolves `cube` in ITS island → the builtin, from either arm.
    ("lib_shared_use.scad", "module _sq(s) cube(s);\n"),
    ("shared_use_a.scad", "use <lib_shared_use.scad>\n"),
    ("shared_use_b.scad", "use <lib_shared_use.scad>\n"),
    (
        "diamond_use.scad",
        "include <shared_use_a.scad>\ninclude <shared_use_b.scad>\nmodule cube(s) { _sq(s); }\ncube(2);\n",
    ),
];

/// Materialize the fixture graph once into `CARGO_TARGET_TMPDIR/loader` and hand back its path.
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("loader");
        for (rel, contents) in FIXTURES {
            let path = base.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        base
    })
}

/// Evaluate a fixture file (relative to the graph root) with the given library paths.
fn file(name: &str, libs: &[PathBuf]) -> Mesh {
    evaluate_file(&root().join(name), libs).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// FULL mesh equality — verts AND tris, positions included (`Mesh` is `PartialEq`, tessellation is
/// deterministic). Counts alone can't distinguish a sphere's radius (topology is radius-independent),
/// so a value flowing through wrong would pass a counts check; equality actually pins the VALUE.
fn same_mesh(a: &Mesh, b: &Mesh) -> bool {
    a == b
}

/// The heart of the corpus: a loader graph must render EXACTLY its inlined single-file equivalent.
#[test]
fn loader_matches_the_inlined_equivalent() {
    let libdir = root().join("lib");
    for (fixture, libs, equivalent) in [
        // include splices `size = 3` → sphere r = 3
        ("include_var.scad", vec![], "sphere(3, $fn = 8);"),
        // use imports r() = 8 → sphere r = 8
        ("use_fn.scad", vec![], "sphere(8, $fn = 16);"),
        // use imports a MODULE def → libbox(4) renders exactly cube(4)
        ("use_module.scad", vec![], "cube(4);"),
        // local r() = 2 beats the used r() = 8
        ("local_wins.scad", vec![], "sphere(2, $fn = 8);"),
        // last use (lib_r5) wins → r() = 5
        ("use_order.scad", vec![], "sphere(5, $fn = 8);"),
        // use runs no geometry → only the root sphere, r = gr() = 6
        ("use_no_exec.scad", vec![], "sphere(6, $fn = 8);"),
        // cycle breaks, a's sphere(2) still renders
        ("cycle_a.scad", vec![], "sphere(2, $fn = 8);"),
        // diamond: shared v = 7 spliced via both arms → sphere r = 7 (no error)
        ("d_top.scad", vec![], "sphere(7, $fn = 8);"),
        // use pulls inner_r() in through lib_outer's own include (transitive via include)
        ("use_transitive_include.scad", vec![], "sphere(3, $fn = 8);"),
        // a used file whose includes cycle still exports its function → sc_r() = 7
        ("use_selfcycle.scad", vec![], "sphere(7, $fn = 8);"),
        // lu_r() reaches through one `use`; the used file's OWN `use` is not re-exported → lu_r() = 2
        ("use_nontransitive.scad", vec![], "sphere(2, $fn = 8);"),
        // use imports functions only → noise stays undef in the using scope → sphere(undef)
        ("use_var.scad", vec![], "sphere(undef, $fn = 8);"),
        // I.9.5: the used `_cube` resolves `cube` in ITS island → the BUILTIN, not the root's redefine.
        // Renders exactly `cube(4)`; before per-file islands this recursed to the depth-guard error.
        ("use_shadow_builtin.scad", vec![], "cube(4);"),
        // I.9.5: the same lib `use`d via two include arms dedups to one island → `cube(2)` still renders.
        ("diamond_use.scad", vec![], "cube(2);"),
        // pathlib reachable only via the library path → pr() = 4
        (
            "use_via_libpath.scad",
            vec![libdir.clone()],
            "sphere(4, $fn = 8);",
        ),
    ] {
        let got = file(fixture, &libs);
        let want = evaluate(equivalent).expect("inline equivalent evaluates");
        assert!(
            same_mesh(&got, &want),
            "{fixture}: got {}v/{}t, inlined {equivalent:?} is {}v/{}t",
            got.vert_count(),
            got.tri_count(),
            want.vert_count(),
            want.tri_count(),
        );
    }
}

#[test]
fn a_diamond_re_splices_shared_geometry() {
    // A shared file with geometry, reached via both diamond arms, splices TWICE → two top-level
    // objects → the implicit-union defer (J.2). A dedup regression would give ONE object → a mesh, no
    // error — so this fails LOUD if the loader ever stops re-splicing. (Flip to a 2-object union
    // assertion when J.2 lands.)
    let err = evaluate_file(&root().join("dup_geom_top.scad"), &[]).unwrap_err();
    assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
}

#[test]
fn missing_include_is_loud() {
    // OpenSCAD warns + renders on; we fail LOUD (a missing lib in a correct corpus is our bug).
    let err = evaluate_with_base("include <does_not_exist.scad>\n", root(), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
    // …and the same for a missing `use`.
    let err = evaluate_with_base("use <no_such_lib.scad>\n", root(), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}

#[test]
fn library_path_is_only_searched_after_the_local_dir() {
    // Without the lib path, pathlib.scad is unreachable from the root dir → LOUD (proves the local dir
    // is tried first + the lib path is what makes it resolve).
    let err = evaluate_file(&root().join("use_via_libpath.scad"), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}

#[test]
fn a_plain_program_still_round_trips_through_the_loader() {
    // The `evaluate` sugar now routes through the loader; an include-free program must be unaffected.
    let via_loader = evaluate("x = 4; sphere(x, $fn = 8);").expect("evaluates");
    let direct = evaluate("sphere(4, $fn = 8);").expect("evaluates");
    assert!(same_mesh(&via_loader, &direct));
}

#[test]
fn an_absolute_path_reference_resolves() {
    // An absolute `<…>` reference is used as-is (OpenSCAD's is_absolute branch) — no dir/lib search.
    let abs = root().join("consts.scad");
    let src = format!("include <{}>\nsphere(size, $fn = 8);\n", abs.display());
    let got = evaluate_with_base(&src, root(), &[]).expect("absolute include resolves");
    let want = evaluate("sphere(3, $fn = 8);").expect("evaluates");
    assert!(same_mesh(&got, &want));
}

#[test]
fn a_missing_root_file_is_loud() {
    // evaluate_file on a path that doesn't exist → LOUD at the root read (before any resolution).
    let err = evaluate_file(&root().join("no_such_root.scad"), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}

#[test]
fn use_is_not_transitive() {
    // lib_uses_inner `use`s lib_inner; a file that `use`s lib_uses_inner sees lu_r() but NOT inner_r()
    // (`use` doesn't re-export). Reaching for inner_r() is an unknown function → LOUD.
    let err = evaluate_file(&root().join("use_nontransitive_reach.scad"), &[]).unwrap_err();
    assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
}

/// Write a chain of `count` files `chain_0..chain_{count-1}` under a fresh subdir: each links the next
/// `fanout` times (1 = a deep linear chain, 2 = a fan-out bomb), the leaf is geometry. Returns the root.
fn write_chain(subdir: &str, count: usize, fanout: usize) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(subdir);
    std::fs::create_dir_all(&base).unwrap();
    for k in 0..count {
        let body = if k + 1 < count {
            format!("include <chain_{}.scad>\n", k + 1).repeat(fanout)
        } else {
            "sphere(1, $fn = 8);\n".to_string()
        };
        std::fs::write(base.join(format!("chain_{k}.scad")), body).unwrap();
    }
    base.join("chain_0.scad")
}

#[test]
fn a_deep_include_chain_is_loud() {
    // A 300-deep chain (> MAX_INCLUDE_DEPTH) must fail LOUD, not silently truncate the leaf's geometry.
    let err = evaluate_file(&write_chain("loader_deep", 300, 1), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}

#[test]
fn an_include_fan_out_bomb_is_loud() {
    // Each file includes the next TWICE → 2^N splices at only N depth. The total-work budget must trip
    // LOUD (fast) — the depth cap alone never fires here (depth stays 40, well under 256).
    let err = evaluate_file(&write_chain("loader_bomb", 40, 2), &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}

#[test]
fn a_deep_chain_behind_a_use_is_loud() {
    // The depth cap must also fire when the deep chain hangs off a `use` (collect_exported's guard),
    // not just a direct include.
    let dir = write_chain("loader_deep_use", 300, 1);
    let dir = dir.parent().expect("chain root has a parent dir");
    let err =
        evaluate_with_base("use <chain_0.scad>\nsphere(1, $fn = 8);\n", dir, &[]).unwrap_err();
    assert!(matches!(err, Error::Load(_)), "got {err:?}");
}
