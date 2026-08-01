//! AR.28 — the tier differential aimed at the TRANSPILED band: one generated program, compiled tier
//! OFF and ON, against BOSL2's own declared surface.
//!
//! `intrinsics_dispatch_diff` (lang/fuzz) is the same comparison over the BUILTIN surface. It is
//! what caught the AN binding family and it stays, but it generates `sin`/`len`/`concat` and
//! therefore never emits a call to a single BOSL2 function — zero of the 1260 transpiled natives
//! were ever under it. AR.1 and AR.2 both name that target as the load-bearing deliverable for
//! AR.21, which deletes the hand-written tiers and leaves the transpiler as the ONLY implementation.
//! A suite pointed at the band doing the deleting rather than the band being deleted cannot carry
//! that weight; this is the one that can.
//!
//! WHY THE LIBRARY SURFACE IS DIFFERENT IN KIND, not just bigger: `names_bind` is TRUE on every one
//! of BOSL2's 1329 decls and FALSE on every builtin, because these stand in for user functions where
//! parameter names really do bind. The whole named-argument family — a duplicate name resolving to
//! the wrong slot, an unfilled parameter falling through to a like-named global, a positional arg
//! clobbering an earlier named one — is unreachable from a builtin-only generator by construction.
//!
//! DECLARED, NOT COMPILED. The surface is all 1329 including the 69 the emitter declines: those must
//! interpret identically too, and generating calls to them is how a decline that quietly stopped
//! being a decline would surface.
//!
//! The oracle is the interpreter (`Config::intrinsics = false`) against the SAME registry, so the
//! only variable is which tier ran. Comparison is over `Message::render` — the whole console,
//! warnings included, because a tier that computes the right number while swallowing a diagnostic is
//! still a divergence and a value-only channel is blind to it by construction.
//!
//! Skips when the submodule is absent: with no BOSL2 to include, both legs fail identically on every
//! seed and the target would report agreement forever.
#![no_main]

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;

use fab_gen::{NativeSurface, Profile, generate_against};
use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, Message};

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("libs")
}

/// Built ONCE. Indexing parses and fingerprints 1329 references plus 402 module ones; doing that per
/// input would make the fuzzer measure registry construction instead of the tier.
static HARNESS: LazyLock<Option<(Registry, NativeSurface)>> = LazyLock::new(|| {
    if !fab_bosl2::transpiled() || !libs().join("BOSL2/std.scad").exists() {
        return None;
    }
    Some((
        Registry::new().with(fab_bosl2::Bosl2.rows()),
        NativeSurface::from_library(&fab_bosl2::Bosl2),
    ))
});

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let Some((registry, surface)) = HARNESS.as_ref() else {
        return; // no submodule: every seed would agree vacuously
    };
    let seed = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let src = generate_against(seed, Profile::AB, surface);

    let run = |intrinsics: bool| {
        let config = Config {
            intrinsics,
            ..Config::default()
        };
        fab_lang::evaluate_geometry_with_registry(
            &src,
            Path::new("."),
            &[libs()],
            registry,
            config,
        )
    };
    match (run(false), run(true)) {
        (Ok((_, m_off)), Ok((_, m_on))) => {
            let interp: Vec<String> = m_off.iter().map(Message::render).collect();
            let native: Vec<String> = m_on.iter().map(Message::render).collect();
            assert_eq!(
                interp, native,
                "seed {seed}: interp != TRANSPILED through the real dispatch\n{src}"
            );
        }
        // Both erred identically is fine — with 1329 callables driven by arbitrary arguments, most
        // generated programs legitimately fail on a BOSL2 `assert`. What is NOT fine is the two
        // tiers disagreeing about whether it fails at all: a native that swallows an assert its
        // reference raises is exactly what the fallible ABI exists to prevent.
        (Err(a), Err(b)) => assert_eq!(
            a.to_string(),
            b.to_string(),
            "seed {seed}: both tiers erred, but differently\n{src}"
        ),
        (off, on) => panic!(
            "seed {seed}: one tier errored and the other didn't (off_ok={}, on_ok={})\n{src}",
            off.is_ok(),
            on.is_ok()
        ),
    }
});
