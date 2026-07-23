//! Fuzz the GENERATOR × CONFIG A/B contract (AJ.6): fuzzer bytes pick a seed, the seed generates a
//! valid-by-construction program (the WHOLE language surface — the AJ.1 coverage gate guards the
//! grammar), and the program must evaluate BIT-IDENTICALLY with the eval/CSG caches off and on —
//! same messages, same geometry tree shape. Where `eval` explores adversarial SYNTAX, this
//! explores deep VALID programs cheaply, with the Config contract as a semantic oracle: a caching
//! bug (a stale memo, a key collision, an impurity leak) trips the assert instead of hiding
//! behind "it didn't crash". Hermetic: generated programs name no real files.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let seed = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let src = fab_gen::generate(seed);
    let tmp = std::env::temp_dir();
    let run = |config: fab_lang::Config| {
        fab_lang::resolve_geometry_with_base_full(&src, &tmp, &[], None, config, |raw: &str| {
            Err(fab_lang::Error::Load(format!("no reader for '{raw}'")))
        })
    };
    let off = run(fab_lang::Config::default());
    let on = run(fab_lang::Config {
        eval_cache: true,
        csg_cache: true,
        ..fab_lang::Config::default()
    });
    match (off, on) {
        (Ok((_, m_off)), Ok((_, m_on))) => {
            let a: Vec<String> = m_off.iter().map(fab_lang::Message::render).collect();
            let b: Vec<String> = m_on.iter().map(fab_lang::Message::render).collect();
            assert_eq!(a, b, "seed {seed}: cache A/B message divergence\n{src}");
        }
        (Err(_), Err(_)) => {} // both erred — fine (import against a missing file, etc.)
        (off, on) => panic!(
            "seed {seed}: one config errored, the other didn't (off_ok={}, on_ok={})\n{src}",
            off.is_ok(),
            on.is_ok()
        ),
    }
});
