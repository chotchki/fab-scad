//! AR.2 — the DISPATCH-level differential for the NATIVE tier: the same generated program evaluated
//! with intrinsics OFF and ON must produce the identical console, verbatim.
//!
//! The twin of `jit_dispatch_diff`, aimed at the other compiled tier. Both exist for the same reason,
//! and it is worth stating once more because it is the whole argument for the transpiler's acceptance
//! suite: a harness that calls an implementation DIRECTLY and binds parameters positionally skips the
//! interpreter's real call machinery, so an entire family of BINDING bugs lands on the same wrong
//! answer on both sides and the two agree with each other. Phase AN found four that way — a duplicate
//! parameter name resolving to the wrong slot, an unfilled parameter falling through to a like-named
//! global, a positional arg clobbering an earlier named one, and a root constant overriding a `use`d
//! library's own. Every one of them only exists once a call goes through `Task::Apply`, `push_call`
//! and the registry, i.e. through THIS target and not through a positional one.
//!
//! Why it could not exist before AR.2: there was no way to turn intrinsics off. `Config` carried
//! `jit`, `eval_cache`, `csg_cache` and `eval_budget` — every accelerator had an off-switch except the
//! one that has been dispatching ~55 hand-written natives since O.5. Without the switch there is no
//! INTERPRETER ORACLE, and "compiled tier == interpreter" — which is precisely the transpiler's
//! contract, not just the intrinsics' — is unfalsifiable at the dispatch level.
//!
//! The oracle is the interpreter (`intrinsics=false`), the same relationship `jit_dispatch_diff` and
//! `gen_diff` have with their configs. Comparing `Message::render` means WARNINGS count too: a tier
//! that computes the right number while swallowing a diagnostic is still a divergence, and a
//! value-only channel is blind to it by construction (the AN.12/AN.13 lesson).
//!
//! AR.5a is the standing evidence that this is not hypothetical — three of the registry's
//! hand-maintained guard lists were WRONG, and a guard is exactly what decides whether a native is
//! allowed to wire at a real call site.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let seed = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let src = fab_gen::generate_ab(seed); // AB surface: cheap + seedless rands (AS.5)
    let tmp = std::env::temp_dir();
    let run = |intrinsics: bool| {
        let config = fab_lang::Config {
            intrinsics,
            ..fab_lang::Config::default()
        };
        fab_lang::resolve_geometry_with_base_full(&src, &tmp, &[], None, config, |raw: &str| {
            Err(fab_lang::Error::Load(format!("no reader for '{raw}'")))
        })
    };
    match (run(false), run(true)) {
        (Ok((_, m_off)), Ok((_, m_on))) => {
            let interp: Vec<String> = m_off.iter().map(fab_lang::Message::render).collect();
            let native: Vec<String> = m_on.iter().map(fab_lang::Message::render).collect();
            assert_eq!(
                interp, native,
                "seed {seed}: interp != NATIVE through the real dispatch\n{src}"
            );
        }
        // Both erred identically is fine — a generated program can legitimately fail (an assert, a
        // budget). What is NOT fine is the two tiers DISAGREEING about whether it fails at all: an
        // intrinsic that swallows an assert its reference raises is exactly the shape of bug the
        // fallible-ABI contract exists to prevent.
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
