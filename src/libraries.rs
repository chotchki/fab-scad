//! SZ.2/SZ.4 — WHICH LIBRARIES THIS PRODUCT CARRIES, as one declaration.
//!
//! Its own module rather than a corner of `import` because three different consumers need it and
//! only one of them has the kernel: the registry builder (`import::registry`), the source packer
//! (`pack_libs`), and the wasm GUI's worker routing, which reads the prefixes to decide whether a
//! render needs the full worker or the lean one. `import` is `kernel`-gated and the GUI's wasm build
//! is not, so a declaration living there was reachable by two of the three.

/// One library this product carries: its transpiled ROWS and the directory its SOURCE lives in,
/// bound together (SZ.2).
///
/// They were two independent lists and they disagreed. The rows came from cargo features; the
/// source came from a python glob in `packaging/web/pack_scad_libs.py` that named BOSL2 and
/// scad-lib by hand. Adding MCAD to the `kernel` feature therefore compiled its natives into the
/// browser bundle while its `.scad` stayed out of `libs.json` — so `include <MCAD/units.scad>`
/// resolved natively and silently rendered NOTHING on the web, because a missing import costs a
/// PART rather than an error.
///
/// One struct, one list. A library that cannot say where its source lives cannot be declared, and
/// the packer reads the same list the registry does — so the two halves cannot drift apart again.
pub struct Library {
    /// The library's own name, as its `LibrarySurface` reports it.
    pub name: &'static str,
    /// Where its `.scad` files live, relative to the repo root.
    pub source_dir: &'static str,
    /// The prefix an `include <...>` uses — `BOSL2/std.scad` is `BOSL2/`, and scad-lib's own
    /// modules are referenced bare, so its prefix is empty.
    pub prefix: &'static str,
}

/// THE LIBRARIES THIS PRODUCT CARRIES, source side — the list `libs.json` is packed from and the
/// list the parity gate compares the registry against.
///
/// `Natives` is absent on purpose: fab-lang's own rows are compiled-in with no `.scad` behind them,
/// so there is nothing to pack. scad-lib IS here — it ships as source on both platforms and has no
/// transpiled crate, which is the mirror case.
///
/// UNCONDITIONAL, and SZ.4 is why. The first cut cfg-gated each entry on its transpiled crate, which
/// read as "one declaration" but quietly answered the wrong question: a LEAN build (kernel without
/// the `libraries` feature) has no BOSL2 rows and still has to RENDER BOSL2 — interpreted — so it
/// needs the source just as much. Gating the source on the rows would have shipped a lean worker
/// that cannot resolve `include <BOSL2/std.scad>` at all, turning a speed difference into a missing
/// part. Source and rows are genuinely different questions; the invariant that matters runs ONE way
/// (anything with rows must have source), and `library_declaration_is_one_list.rs` asserts exactly
/// that direction and no more.
#[must_use]
pub fn libraries() -> &'static [Library] {
    &[
        Library {
            name: "BOSL2",
            source_dir: "libs/BOSL2",
            prefix: "BOSL2/",
        },
        Library {
            name: "MCAD",
            source_dir: "libs/MCAD",
            prefix: "MCAD/",
        },
        Library {
            name: "machineblocks",
            source_dir: "libs/machineblocks/lib",
            prefix: "machineblocks/",
        },
        // fab-scad's OWN library: source-only, no transpiled crate, referenced bare.
        Library {
            name: "scad-lib",
            source_dir: "scad-lib",
            prefix: "",
        },
    ]
}
