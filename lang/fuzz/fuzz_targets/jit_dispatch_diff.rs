//! AN.16 — the DISPATCH-level tier differential: the same generated program evaluated with the JIT OFF
//! and ON must produce the identical console, verbatim.
//!
//! This exists because `jit_diff` has a STRUCTURAL blind spot. That target calls `compile_function`
//! directly and binds parameters POSITIONALLY, which skips the interpreter's real call machinery
//! entirely — so for a whole family of binding bugs both tiers land on the same wrong answer and agree
//! with each other. Phase AN found four that way, every one invisible to `jit_diff` and needing a human:
//! a duplicate parameter name resolving to the wrong slot, an unfilled parameter falling through to a
//! like-named global, a positional arg clobbering an earlier named one, and a root constant overriding a
//! `use`d library's own. All four only exist once a call goes through `Task::Apply`, `push_call`, and the
//! registry — i.e. through THIS target and not that one.
//!
//! The oracle is the interpreter (jit=off), the same relationship `gen_diff` has with its cache configs.
//! Comparing `Message::render` means WARNINGS count too, not just echoed values — a tier that computes
//! the right number while swallowing a diagnostic is still a divergence, and the value-only channel is
//! blind to it by construction (the AN.12/AN.13 lesson).
#![no_main]

use libfuzzer_sys::fuzz_target;

use fab_jit::JitFactory;
use fab_lang::NumericJitFactory;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let seed = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let src = fab_gen::generate_ab(seed); // AB surface: cheap + seedless rands (AS.5)
    let tmp = std::env::temp_dir();
    let factory = JitFactory;
    let run = |jit: bool| {
        let hook: Option<&dyn NumericJitFactory> = jit.then_some(&factory);
        let config = fab_lang::Config {
            jit,
            ..fab_lang::Config::default()
        };
        fab_lang::resolve_geometry_with_base_full(&src, &tmp, &[], hook, config, |raw: &str| {
            Err(fab_lang::Error::Load(format!("no reader for '{raw}'")))
        })
    };
    match (run(false), run(true)) {
        (Ok((_, m_off)), Ok((_, m_on))) => {
            let interp: Vec<String> = m_off.iter().map(fab_lang::Message::render).collect();
            let jitted: Vec<String> = m_on.iter().map(fab_lang::Message::render).collect();
            assert_eq!(
                interp, jitted,
                "seed {seed}: interp != JIT through the real dispatch\n{src}"
            );
        }
        // Both erred identically is fine — a generated program can legitimately fail (an assert, a
        // budget). What is NOT fine is the two tiers DISAGREEING about whether it fails at all.
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
