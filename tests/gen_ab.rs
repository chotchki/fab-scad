//! AJ.7 — the Config A/B soak over GENERATED programs: evaluating the same fab-gen program with
//! the caches OFF (`Config::default`) and ON must be BIT-IDENTICAL — same message stream, same
//! STL bytes. This is the Config contract ("every field is bit-identity-preserving") used as an
//! oracle against the fattened AJ grammar, in CI on every `cargo test`.
//!
//! A failure names the seed; `cargo run -p fab-gen -- --replay <seed>` prints the exact program.

use fab_scad::backend::{ManifoldBackend, build_geo};

fn eval_ab(seed: u32) -> (String, String) {
    let src = fab_gen::generate(seed);
    let tmp = std::env::temp_dir();
    let run = |config: fab_lang::Config| -> String {
        let (tree, messages) =
            fab_scad::import::resolve_geometry_with_base_full(&src, &tmp, &[], config)
                .unwrap_or_else(|e| panic!("seed {seed} errored under {config:?}: {e}\n{src}"));
        let stl = build_geo(&tree, &ManifoldBackend)
            .map(|s| s.to_stl_bytes())
            .unwrap_or_default();
        let msgs: Vec<String> = messages.iter().map(fab_lang::Message::render).collect();
        // digest = messages + STL byte length + a cheap content fold (full bytes would bloat the
        // panic output; the fold still catches any byte flip).
        let mut fold = 0u64;
        for (i, b) in stl.iter().enumerate() {
            fold = fold
                .wrapping_mul(1_000_003)
                .wrapping_add(u64::from(*b) ^ i as u64);
        }
        format!(
            "{}\nstl_len={} stl_fold={fold:x}",
            msgs.join("\n"),
            stl.len()
        )
    };
    let off = run(fab_lang::Config::default());
    let on = run(fab_lang::Config {
        eval_cache: true,
        csg_cache: true,
        ..fab_lang::Config::default()
    });
    (off, on)
}

/// The soak: every seed evaluates identically with caches off and on.
#[test]
fn generated_programs_are_config_bit_identical() {
    for seed in 0..300u32 {
        let (off, on) = eval_ab(seed);
        assert_eq!(
            off, on,
            "seed {seed}: caches-on diverged from caches-off (replay: cargo run -p fab-gen -- --replay {seed})"
        );
    }
}
