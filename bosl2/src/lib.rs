//! BOSL2, transpiled to Rust at BUILD time (AR.26.3) — the first generated library crate, and the
//! shape every library after it takes.
//!
//! Nothing here is checked in. `build.rs` reads the pinned `libs/BOSL2` submodule, runs the
//! transpiler over it, and writes the natives into `OUT_DIR`; this file is the ~50 lines that
//! declare what came out. A BOSL2 bump moves the submodule pointer and the next build
//! re-transpiles — there is no generated artifact to keep current, and therefore no regen gate to
//! run and no 3.7 MB diff to not-read.
//!
//! # The dependency direction, and why the registry had to invert
//!
//! This crate depends on fab-lang for the runtime its natives are emitted against
//! (`fab_lang::rt`). fab-lang therefore CANNOT depend on it — which is exactly why dispatch stopped
//! consulting a table inside fab-lang and started consulting one a consumer hands in (AR.26.1). A
//! consumer accumulates [`Bosl2`]'s rows into a `Registry` and passes that to evaluation.
//!
//! # What it declares versus what it compiled
//!
//! Two different lists, deliberately different lengths. [`Bosl2::callables`] is DECLARATION — every
//! function BOSL2 hosts, whether or not we compile it — and [`Bosl2::rows`] is IMPLEMENTATION.
//! Folding them together would assert they are the same set, which is the drift this whole phase
//! exists to kill.

#![doc(html_no_source)]

/// The row type the generated MODULE spine names. It spells it `super::ModuleEntry`, matching the
/// layout it was written for in fab-lang — and a generated file that has to be rewritten to move is
/// a file nobody can diff, so the name is provided here rather than the text changed there. (The
/// function half needs no such binding: it says `rt::Entry` outright.)
pub(crate) use fab_lang::rt::ModuleEntry;

// THE TRANSPILED LIBRARY. One wrapper file the build assembled: `mod functions` (the natives, this
// library's declared SURFACE, and the AR.10 fallback island) and `mod modules` (one generated file
// per BOSL2 source behind a spine). Included at the CRATE ROOT rather than inside a `mod` of our
// own, because both halves open with inner attributes and `include!` will not carry those into a
// module body — build.rs hoists them onto the wrapper's `mod` declarations instead.
include!(concat!(env!("OUT_DIR"), "/bosl2.rs"));

/// BOSL2 as a library a consumer can load.
///
/// Zero-sized: everything it serves is static data the build wrote. Hand it to a `Registry` and the
/// compiled tier can dispatch to it; ask it for `callables` and the fuzzer can generate against it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bosl2;

impl fab_lang::surface::LibrarySurface for Bosl2 {
    fn name(&self) -> &'static str {
        "BOSL2"
    }

    fn preamble(&self) -> &'static str {
        "include <BOSL2/std.scad>\n"
    }

    fn rows(&self) -> fab_lang::registry::Rows {
        fab_lang::registry::Rows {
            name: "BOSL2",
            functions: functions::REGISTRY,
            modules: modules::REGISTRY,
            // No PINS. A pin exists so a row can anchor a dep it does not itself compile, and this
            // library's dep closures are closed OVER THE BAND by construction — `function_band`
            // drops any function that calls something it did not emit, so every dep is a row.
            pins: &[],
        }
    }

    fn callables(&self) -> &'static [fab_lang::rt::Decl] {
        functions::SURFACE
    }
}

/// Was a BOSL2 actually transpiled into this build? `false` means the submodule was absent and the
/// crate declares nothing — worth being able to ASK, because a silently empty library is the
/// failure this codebase keeps finding (a missing asset costs a part, not an error).
#[must_use]
pub fn transpiled() -> bool {
    TRANSPILED
}
