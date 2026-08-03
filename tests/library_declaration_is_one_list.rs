//! SZ.5 — NATIVE AND WEB AGREE ABOUT WHICH LIBRARIES EXIST.
//!
//! A library reaches a render through two independent channels, and both must carry it or the
//! result is a platform split that nothing errors on:
//!
//!   - its ROWS, so calls into it dispatch to compiled natives rather than interpreting;
//!   - its SOURCE, so `include <BOSL2/std.scad>` resolves to text at all.
//!
//! Natively the source half is free — the loader walks `libs/` off disk. In the browser there is no
//! disk, so the source has to be PACKED into `libs.json` ahead of time, and for three libraries it
//! was packed by a python glob naming directories by hand while the rows came from cargo features.
//! They drifted the moment MCAD joined `kernel`: its natives compiled into the bundle, its `.scad`
//! never entered the pack, and `include <MCAD/units.scad>` rendered correctly on the desktop and
//! silently produced NOTHING in the browser — a missing import costs a PART, not an error.
//!
//! `import::libraries()` is now the single declaration and `pack_libs` reads it, so the two cannot
//! diverge by construction. This asserts the construction actually holds, because "cannot diverge"
//! is a claim about code that a refactor can quietly falsify.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::Path;

#[cfg(any(feature = "bosl2", feature = "mcad", feature = "machineblocks"))]
use fab_lang::surface::LibrarySurface;

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every declared library's source directory exists and holds `.scad`. A declaration pointing at
/// nothing is the shape of the MCAD bug from the other side — the pack would come up short and the
/// browser would be missing a library the registry insists it has.
#[test]
fn every_declared_library_has_source_on_disk() {
    for lib in fab_scad::import::libraries() {
        let dir = repo().join(lib.source_dir);
        if !dir.exists() {
            // A shallow clone without the submodule — skip, as the sibling gates do. CI checks out
            // recursively, which is where this assertion has teeth.
            continue;
        }
        let scads = std::fs::read_dir(&dir)
            .expect("the directory reads")
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "scad"))
            .count();
        assert!(
            scads > 0,
            "library `{}` declares {} but it holds no .scad at all — an empty submodule looks \
             exactly like this and packs silently",
            lib.name,
            dir.display()
        );
    }
}

/// Every library whose ROWS the product registers is also declared as SOURCE. This is the direction
/// that actually broke: MCAD had rows and no source, so it dispatched natively and vanished on web.
///
/// `Natives` is exempt and named explicitly rather than skipped by a rule — fab-lang's own rows are
/// compiled in with no `.scad` behind them, so there is nothing to pack and never will be.
// Built by pushing under `cfg`: which libraries register rows is a COMPILE-TIME question, and a
// `vec!` literal cannot hold conditionally-present elements.
#[allow(
    clippy::vec_init_then_push,
    reason = "each entry is cfg-gated; a vec! literal cannot express that"
)]
#[test]
fn every_registered_library_is_also_declared_as_source() {
    let declared: Vec<&str> = fab_scad::import::libraries()
        .iter()
        .map(|l| l.name)
        .collect();

    #[allow(
        unused_mut,
        reason = "a lean build enables none of the cfg arms below, so nothing pushes"
    )]
    let mut registered: Vec<&str> = Vec::new();
    #[cfg(feature = "bosl2")]
    registered.push(fab_bosl2::Bosl2.name());
    #[cfg(feature = "mcad")]
    registered.push(fab_mcad::Mcad.name());
    #[cfg(feature = "machineblocks")]
    registered.push(fab_machineblocks::MachineBlocks.name());

    for name in &registered {
        assert!(
            declared.contains(name),
            "`{name}` contributes ROWS to the product registry but is not in \
             `import::libraries()`, so its source never reaches `libs.json`. Native renders would \
             resolve it off disk and the browser would render NOTHING for it, silently. Declared: \
             {declared:?}"
        );
    }
    // Two-sided, for the same reason as `libraries_are_not_empty`: a lean build legitimately
    // registers nothing, and asserting non-empty there would fail a correct configuration while
    // asserting nothing about it.
    if cfg!(feature = "libraries") {
        assert!(
            !registered.is_empty(),
            "the `libraries` feature is ON but no library registered rows, so this compared nothing"
        );
    } else {
        assert!(
            registered.is_empty(),
            "the `libraries` feature is OFF but {registered:?} registered rows anyway"
        );
    }
}

/// Prefixes are distinct. Two libraries claiming the same `include <...>` prefix would have one
/// silently overwrite the other's files in the flat pack, and whichever lost would be missing in
/// the browser only.
#[test]
fn declared_prefixes_do_not_collide() {
    let libs = fab_scad::import::libraries();
    for (i, a) in libs.iter().enumerate() {
        for b in libs.iter().skip(i + 1) {
            assert_ne!(
                a.prefix, b.prefix,
                "`{}` and `{}` both claim the include prefix {:?} — the pack is a flat map, so one \
                 would overwrite the other's files and the loser disappears on web only",
                a.name, b.name, a.prefix
            );
        }
    }
}
