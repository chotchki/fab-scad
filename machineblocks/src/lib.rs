//! machineblocks, transpiled to Rust at BUILD time (AR.37) — the THIRD generated library crate.
//!
//! [`fab-mcad`] is the generality CONTROL (never a target, inverted shape, and it produced a real
//! emitter bug). This one is the breadth: a third-party parametric-brick library by somebody with
//! no connection to either of the other two, written in whatever OpenSCAD style its author likes.
//! It transpiles whole — 57 of 57 functions, 27 of 28 modules — which is the boring result and the
//! one worth having, because two libraries agreeing could still be a coincidence of style.
//!
//! ONLY `lib/` IS READ. machineblocks ships 529 `.scad` files and roughly 500 of them are generated
//! part variants under `examples/` and `templates/`; `lib/` is the 16 that declare anything. That is
//! a fact about this repository's layout, not a limit of the transpiler.
//!
//! [`fab-mcad`]: https://github.com/openscad/MCAD
//!
//! # What it declares versus what it compiled
//!
//! Two different lists, deliberately kept apart — same contract as fab-bosl2.
//! [`MachineBlocks::callables`] is DECLARATION, [`MachineBlocks::rows`] is IMPLEMENTATION, and
//! folding them together would assert they are the same set.

#![doc(html_no_source)]

/// The row type the generated MODULE spine names. It spells it `super::ModuleEntry`, matching the
/// layout it was written for in fab-lang — and a generated file that has to be rewritten to move is
/// a file nobody can diff, so the name is provided here rather than the text changed there.
pub(crate) use fab_lang::rt::ModuleEntry;

// THE TRANSPILED LIBRARY, assembled by the build into one wrapper file. Included at the CRATE ROOT
// rather than inside a `mod` of our own, because both halves open with inner attributes and
// `include!` will not carry those into a module body.
include!(concat!(env!("OUT_DIR"), "/machineblocks.rs"));

/// machineblocks as a library a consumer can load.
///
/// Zero-sized: everything it serves is static data the build wrote. Hand it to a `Registry` and the
/// compiled tier can dispatch to it; ask it for `callables` and the fuzzer can generate against it.
#[derive(Clone, Copy, Debug, Default)]
pub struct MachineBlocks;

impl fab_lang::surface::LibrarySurface for MachineBlocks {
    fn name(&self) -> &'static str {
        "machineblocks"
    }

    /// machineblocks' own files reach each other with `use <block.scad>`, so there is no umbrella and
    /// no include that brings the library in wholesale. `block.scad` is the entry a user actually
    /// starts from.
    fn preamble(&self) -> &'static str {
        "use <block.scad>\n"
    }

    fn rows(&self) -> fab_lang::registry::Rows {
        fab_lang::registry::Rows {
            name: "machineblocks",
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

/// Was a machineblocks actually transpiled into this build? `false` means the submodule was absent and the
/// crate declares nothing — worth being able to ASK, because a silently empty library is the
/// failure this codebase keeps finding (a missing asset costs a part, not an error).
#[must_use]
pub fn transpiled() -> bool {
    TRANSPILED
}
