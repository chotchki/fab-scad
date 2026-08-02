//! MCAD, transpiled to Rust at BUILD time (AR.37) — the SECOND generated library crate, and the
//! reason the first one's numbers can be trusted.
//!
//! Everything the transpiler had ever been measured on was BOSL2, which it was built against for a
//! whole phase. "99.5% of BOSL2" is a claim about BOSL2. This crate is the control: MCAD is the
//! library that ships WITH OpenSCAD, it was never a target, and its shape is INVERTED — 167 modules
//! to 39 functions, where BOSL2 is 414 to 1329. If the emitter's subset were BOSL2-shaped rather
//! than OpenSCAD-shaped, this is where it would show.
//!
//! It does not. MCAD transpiles whole — 39 of 39 functions, 167 of 167 modules — once one real bug
//! was out of the way, and that bug is the interesting part: OpenSCAD identifiers may begin with a
//! digit and Rust identifiers may not. `3dtri_draw`, `8bit_polyfont` and `12ptStar` emitted bare
//! and the whole band failed to compile. BOSL2 declares no such name, so no amount of work on it
//! could have found this. A second library is a different KIND of test, not more of the same one.
//!
//! # What it declares versus what it compiled
//!
//! Two different lists, deliberately kept apart — same contract as fab-bosl2. [`Mcad::callables`]
//! is DECLARATION, [`Mcad::rows`] is IMPLEMENTATION, and folding them together would assert they
//! are the same set. They happen to match here; that is a fact about MCAD, not a guarantee.

#![doc(html_no_source)]

/// The row type the generated MODULE spine names. It spells it `super::ModuleEntry`, matching the
/// layout it was written for in fab-lang — and a generated file that has to be rewritten to move is
/// a file nobody can diff, so the name is provided here rather than the text changed there.
pub(crate) use fab_lang::rt::ModuleEntry;

// THE TRANSPILED LIBRARY, assembled by the build into one wrapper file. Included at the CRATE ROOT
// rather than inside a `mod` of our own, because both halves open with inner attributes and
// `include!` will not carry those into a module body.
include!(concat!(env!("OUT_DIR"), "/mcad.rs"));

/// MCAD as a library a consumer can load.
///
/// Zero-sized: everything it serves is static data the build wrote. Hand it to a `Registry` and the
/// compiled tier can dispatch to it; ask it for `callables` and the fuzzer can generate against it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mcad;

impl fab_lang::surface::LibrarySurface for Mcad {
    fn name(&self) -> &'static str {
        "MCAD"
    }

    /// MCAD has no umbrella include — a user pulls in the one file they want. `units.scad` is the
    /// closest thing to a base: it defines the unit constants (`mm`, `inch`, `cm`) that the rest of
    /// the library measures in, and several files include it themselves.
    fn preamble(&self) -> &'static str {
        "include <MCAD/units.scad>\n"
    }

    fn rows(&self) -> fab_lang::registry::Rows {
        fab_lang::registry::Rows {
            name: "MCAD",
            functions: functions::REGISTRY,
            modules: modules::REGISTRY,
            // No PINS, same reason as BOSL2: `function_band` drops any function calling something
            // it did not emit, so every dep is already a row.
            pins: &[],
        }
    }

    fn callables(&self) -> &'static [fab_lang::rt::Decl] {
        functions::SURFACE
    }
}

/// Was an MCAD actually transpiled into this build? `false` means the submodule was absent and the
/// crate declares nothing — worth being able to ASK, because a silently empty library is the
/// failure this codebase keeps finding (a missing asset costs a part, not an error).
#[must_use]
pub fn transpiled() -> bool {
    TRANSPILED
}
