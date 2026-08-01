//! AR.26.4.2 — the MODULE tier differential, re-homed from fab-lang along with the band it tests.
//!
//! These three tests followed the 402 generated BOSL2 modules out of `fab-lang` and into the crate
//! that now produces them. They could not stay: fab-lang cannot depend on fab-bosl2 (the dependency
//! runs the other way — that is the whole reason AR.26.1 inverted the registry), so once
//! `generated_bosl2_modules/` stopped being checked into fab-lang there was no BOSL2 module tier
//! there to compare against, and a test whose subject has left the crate passes by rendering
//! interpreted twice.
//!
//! EVERY CASE PROVES A NATIVE ACTUALLY RAN, via `native_module_runs()`. Armed and RAN are different
//! facts and the band-1 postmortem is why: every transform native resolved, then declined at its
//! first child-forwarding call, and no tier test in the tree could see it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, native_module_runs};

/// The pinned BOSL2 checkout — the base dir these programs `include <std.scad>` against, so the
/// definitions the natives arm on are the library's OWN rather than transcribed copies.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs/BOSL2")
}

fn have_bosl2() -> bool {
    fab_bosl2::transpiled() && root().join("std.scad").exists()
}

fn registry() -> Registry {
    Registry::new().with(fab_bosl2::Bosl2.rows())
}

/// Render `src` with the compiled tier on or off. Returns the geometry tree's structural dump and
/// the console, both as strings, because a tier difference shows up in either.
fn run(src: &str, reg: &Registry, intrinsics: bool) -> (String, String) {
    let config = Config {
        intrinsics,
        ..Config::default()
    };
    let (geo, msgs) =
        fab_lang::evaluate_geometry_with_registry(src, &root(), &[], reg, config).expect("renders");
    (format!("{geo:?}"), format!("{msgs:?}"))
}

/// REAL BOSL2 modules through the compiled path (AR.14.4 bands 1+2): `down`, `zrot` and `hexagon`
/// armed by fingerprint match against the include'd definitions themselves, plus `top_half` and
/// `upcube`, which BAKE direction vectors and therefore arm only because `std.scad`'s own
/// `TOP`/`BOT`/`CENTER` match what the native compiled in.
#[test]
fn armed_bosl2_band_modules_match_the_interpreter() {
    if !have_bosl2() {
        return;
    }
    let reg = registry();
    let src = "include <std.scad>\ndown(3) cube(2);\nzrot(45) cube(1);\nhexagon(r=4);\n\
               top_half() sphere(r=3);\nupcube([2,3,4]);\n\
               arc_copies(d=40, n=6) sphere(1);\ncuboid([10,20,30]);\n\
               diff() cuboid([8,8,8]) { tag(\"remove\") cuboid([4,4,10]); };";

    let before = native_module_runs();
    let (geo_on, msgs_on) = run(src, &reg, true);
    let ran = native_module_runs() - before;
    let (geo_off, msgs_off) = run(src, &reg, false);

    assert_eq!(
        geo_on, geo_off,
        "band: compiled BOSL2 modules built different geometry than interpreting them"
    );
    assert_eq!(msgs_on, msgs_off, "band: different console");
    assert!(ran > 0, "no native module ran — the comparison is vacuous");

    // BAND 2's CONST GUARD, live on a real library row rather than the poc. `top_half`
    // (partitions.scad) bakes `BACK` and `TOP`; it must wire against a scope carrying the library's
    // own values and REFUSE one that rebinds either, because the fingerprint proves the module's
    // source and says nothing about the constants it names.
    let partitions = std::fs::read_to_string(root().join("partitions.scad")).expect("reads");
    let program = fab_lang::parse(&partitions).expect("parses");
    let (params, body) = program
        .stmts
        .iter()
        .find_map(|s| match &s.kind {
            fab_lang::StmtKind::ModuleDef { name, params, body } if &**name == "top_half" => {
                Some((params, body))
            }
            _ => None,
        })
        .expect("partitions.scad defines top_half");
    let mut good = fab_lang::Scope::new();
    good.bind("BACK", fab_lang::Value::num_list(vec![0.0, 1.0, 0.0]));
    good.bind("TOP", fab_lang::Value::num_list(vec![0.0, 0.0, 1.0]));
    assert!(
        reg.resolve_module("top_half", params, body, &good).is_some(),
        "`top_half` with the library's own `BACK`/`TOP` did not arm"
    );
    let mut rebound = fab_lang::Scope::new();
    rebound.bind("BACK", fab_lang::Value::num_list(vec![0.0, 1.0, 0.0]));
    rebound.bind("TOP", fab_lang::Value::num_list(vec![0.0, 0.0, -1.0]));
    assert!(
        reg.resolve_module("top_half", params, body, &rebound)
            .is_none(),
        "`top_half` with a rebound `TOP` must not wire"
    );
}

/// AR.14.4.5 on REAL content: `cuboid` — the most-instantiated module in BOSL2, and the band's
/// prize — runs COMPILED with its five nested module defs registered (`corner_shape` reads the
/// post-reassignment `size`/`chamfer`/`rounding`, `xtcyl`/`tsphere` read `teardrop`, all through the
/// materialized frame). Three argument shapes drive the plain, chamfered and rounded paths;
/// `half_of` rides along as the real-content DECLINE shape — its nested defs take children, so
/// equality is the assertion there and `ran` is not.
#[test]
fn bosl2_cuboid_runs_compiled_with_its_nested_defs() {
    if !have_bosl2() {
        return;
    }
    let reg = registry();
    let programs = [
        ("plain", "include <std.scad>\ncuboid([8, 6, 4]);"),
        (
            "chamfered",
            "include <std.scad>\ncuboid([8, 6, 4], chamfer=1);",
        ),
        (
            "rounded",
            "include <std.scad>\ncuboid([8, 6, 4], rounding=2, $fn=16);",
        ),
        (
            "half_of",
            "include <std.scad>\nhalf_of(UP) sphere(d=8, $fn=16);",
        ),
    ];
    for (label, src) in programs {
        let before = native_module_runs();
        let (geo_on, msgs_on) = run(src, &reg, true);
        let ran = native_module_runs() - before;
        let (geo_off, msgs_off) = run(src, &reg, false);
        assert_eq!(geo_on, geo_off, "{label}: compiled BOSL2 render diverged");
        assert_eq!(msgs_on, msgs_off, "{label}: different console");
        assert!(geo_on.contains("Leaf"), "{label}: no geometry");
        if label != "half_of" {
            assert!(ran > 0, "{label}: no module native completed a run");
        }
    }
}

/// AR.14.4.2 — a program that SHADOWS a builtin function must not have the armed module answer with
/// the REAL builtin. Dispatch resolves user functions first (BOSL2 itself shadows `reverse`), so
/// interpreting `down`'s `assert(is_undef(p), …)` reaches the program's `is_undef` — here `false`,
/// so the interpreted render ERRORS — while a native body that compiled the call to
/// `rt::bi::is_undef` sails past the assert and renders geometry.
///
/// Compared at the RESULT level because the divergence is error-versus-render.
#[test]
fn a_shadowed_builtin_function_does_not_leak_into_an_armed_module() {
    if !have_bosl2() {
        return;
    }
    let reg = registry();
    let go = |intrinsics: bool| {
        let config = Config {
            intrinsics,
            ..Config::default()
        };
        match fab_lang::evaluate_geometry_with_registry(
            "include <std.scad>\nfunction is_undef(x) = false;\ndown(3) cube(1);",
            &root(),
            &[],
            &reg,
            config,
        ) {
            Ok((geo, msgs)) => format!("ok geo={geo:?} msgs={msgs:?}"),
            Err(e) => format!("err {e:?}"),
        }
    };
    assert_eq!(
        go(true),
        go(false),
        "shadowed `is_undef`: the armed module answered with the REAL builtin"
    );
}

/// The `$`-frame bridge, the way it actually gets exercised: `diff()`/`tag()` create geometry
/// THUNKS that render later, and running a thunk against its CREATOR's dynamic context drops every
/// `$`-frame between creator and renderer — `hide()`'s `$tags_hidden` never reached the cuboid and
/// `diff()` UNIONED what it should have subtracted. Render-point scope over creator structure is
/// what this pins.
#[test]
fn the_tag_family_renders_through_compiled_children_like_the_interpreter() {
    if !have_bosl2() {
        return;
    }
    let reg = registry();
    let src = "include <std.scad>\ndiff() cuboid([40, 25, 80]) \
               { tag(\"remove\") left(5) cuboid([10, 10, 90]); };";

    let before = native_module_runs();
    let (geo_on, msgs_on) = run(src, &reg, true);
    let ran = native_module_runs() - before;
    let (geo_off, msgs_off) = run(src, &reg, false);

    assert_eq!(
        geo_on, geo_off,
        "tag family: a $-set was dropped between a thunk's creator and its renderer"
    );
    assert_eq!(msgs_on, msgs_off, "tag family: different console");
    assert!(
        ran > 0,
        "no native ran — the tag pipeline never exercised the bridge"
    );
}
