//! AR.28 — THE ACCEPTANCE SUITE, pointed at the band it is supposed to protect.
//!
//! AR.1 and AR.2 both name `intrinsics_dispatch_diff` as the load-bearing deliverable for AR.21:
//! once both hand-written tiers are gone there is no second implementation left to disagree with,
//! so a tier differential is the ONLY thing between the transpiler and a silent wrong answer. That
//! target generates against the BUILTIN surface — `sin`, `len`, `concat` — so it exercises the
//! dispatch MACHINERY (which is real: it is what caught the AN binding family) and never emits a
//! call to a single BOSL2 function. Zero of the 1260 transpiled natives were under it.
//!
//! This is that differential aimed at BOSL2's own 1329 declared callables, through
//! `NativeSurface::from_library`. The generated programs call `vector_angle` and `_GJK_collide`
//! with NAMED arguments — `names_bind` is true on every library decl and false on every builtin,
//! which is precisely why a generator pointed at builtins can never reach the binding family the
//! natives have to get right.
//!
//! DECLARED, NOT COMPILED, and that is deliberate: the surface is all 1329, including the 69 the
//! emitter declines. Those must interpret identically too, and generating calls to them is how a
//! decline that quietly stopped being a decline would show up.
//!
//! THE ORACLE IS THE INTERPRETER (`Config::intrinsics = false`) against the SAME registry, so the
//! only variable is which tier ran — the AR.2 contract. Comparison is over `Message::render`, the
//! whole console: a tier that computes the right number while swallowing a warning is still a
//! divergence, and this phase found three of those in one night.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_gen::{NativeSurface, Profile, generate_against};
use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, Message};

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

fn have_bosl2() -> bool {
    fab_bosl2::transpiled() && libs().join("BOSL2/std.scad").exists()
}

/// One seed's verdict, as a string, so a divergence prints what actually differed.
enum Leg {
    Ok(Vec<String>),
    Err(String),
}

fn run(src: &str, registry: &Registry, intrinsics: bool) -> Leg {
    let config = Config {
        intrinsics,
        ..Config::default()
    };
    match fab_lang::evaluate_geometry_with_registry(
        src,
        Path::new("."),
        &[libs()],
        registry,
        config,
    ) {
        Ok((_geo, msgs)) => Leg::Ok(msgs.iter().map(Message::render).collect()),
        Err(e) => Leg::Err(e.to_string()),
    }
}

/// Compare both tiers on one seed. Returns `Err(explanation)` on a divergence.
///
/// BOTH-ERRED-IDENTICALLY IS A PASS: a generated program can legitimately fail (a BOSL2 `assert`,
/// an eval budget), and with 1329 callables driven by arbitrary arguments most of them will. What
/// is not a pass is the two tiers disagreeing about WHETHER it fails — a native that swallows an
/// assert its reference raises is exactly what the fallible ABI exists to prevent.
fn diff_seed(seed: u32, registry: &Registry, surface: &NativeSurface) -> Result<(), String> {
    let src = generate_against(seed, Profile::AB, surface);
    match (run(&src, registry, false), run(&src, registry, true)) {
        (Leg::Ok(interp), Leg::Ok(native)) if interp == native => Ok(()),
        (Leg::Ok(interp), Leg::Ok(native)) => Err(format!(
            "seed {seed}: interp != NATIVE console\n  interp: {interp:?}\n  native: {native:?}\n{src}"
        )),
        (Leg::Err(a), Leg::Err(b)) if a == b => Ok(()),
        (Leg::Err(a), Leg::Err(b)) => Err(format!(
            "seed {seed}: both tiers erred, differently\n  interp: {a}\n  native: {b}\n{src}"
        )),
        (a, b) => Err(format!(
            "seed {seed}: one tier errored and the other did not (interp_ok={}, native_ok={})\n{src}",
            matches!(a, Leg::Ok(_)),
            matches!(b, Leg::Ok(_))
        )),
    }
}

/// The surface a consumer that loaded BOSL2 would generate against, and the registry that consumer
/// would evaluate with.
fn harness() -> (Registry, NativeSurface) {
    (
        Registry::new().with(fab_bosl2::Bosl2.rows()),
        NativeSurface::from_library(&fab_bosl2::Bosl2),
    )
}

/// THE GATE, on stable, in the workspace, every run. The fuzz target below it is the continuous
/// lane, but cargo-fuzz is nightly-only and lives in a separate workspace, so a nightly-only
/// acceptance suite is one nobody's `cargo nextest` would notice breaking.
///
/// SEED COUNT IS A BUDGET, not a coverage claim. Each seed costs two full evaluations of a program
/// that `include`s BOSL2, which is where nearly all the time goes; the deep lane below runs the same
/// comparison over far more of them and is the one to reach for after touching the emitter.
#[test]
fn the_transpiled_band_agrees_with_the_interpreter_over_generated_programs() {
    if !have_bosl2() {
        return;
    }
    let (registry, surface) = harness();
    let mut failures = Vec::new();
    for seed in 0..24 {
        if let Err(why) = diff_seed(seed, &registry, &surface) {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 24 seeds diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The DEEP lane — same comparison, far more seeds. `--ignored`, because it is minutes rather than
/// seconds and the fast gate above is what protects an ordinary commit.
#[test]
#[ignore = "deep lane: run after touching the emitter"]
fn the_transpiled_band_agrees_over_many_generated_programs() {
    if !have_bosl2() {
        return;
    }
    let (registry, surface) = harness();
    let mut failures = Vec::new();
    for seed in 0..500 {
        if let Err(why) = diff_seed(seed, &registry, &surface) {
            failures.push(why);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 500 seeds diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// AR.28's FIRST FINDING, pinned as its own case so it cannot regress quietly.
///
/// `root_find(f, x0, x1)` calls through `f`, a PARAMETER — the AN.10 shadow shape. When the caller
/// passes a non-function, the local does not shadow, so the static resolution runs; and when nothing
/// resolves, the emitted else-half used to fall into the ordinary outward dispatch, which reports an
/// UNKNOWN name. The interpreter's rule for a name that is locally BOUND is `Task::CallValue`:
/// `undef`, and SILENT. So the compiled tier printed two `Ignoring unknown function 'f'` lines the
/// interpreter never printed — the right value with the wrong console, which is this tier's
/// characteristic failure and invisible to every mesh comparison.
///
/// Found by seed 207 of the deep lane's first run, minimised here.
#[test]
fn a_non_callable_local_is_silent_like_the_interpreter() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    // No `include` needed: the shape is about a call through a parameter, and keeping the program
    // this small is what makes the console comparison readable when it fails.
    let src = "function g(fnobody, x) = fnobody(x);\necho(g(5, 3));\n";
    let (interp, native) = (
        run(src, &registry, false),
        run(src, &registry, true),
    );
    let (Leg::Ok(interp), Leg::Ok(native)) = (interp, native) else {
        panic!("both tiers must render this")
    };
    assert_eq!(interp, native, "a non-callable local diverged");
    assert!(
        !native.iter().any(|m| m.contains("unknown function")),
        "the name is BOUND, just not callable — the interpreter says nothing: {native:?}"
    );
    assert!(
        native.iter().any(|m| m.contains("undef")),
        "and it answers undef: {native:?}"
    );
}

/// NON-VACUITY, and this file is worthless without it. Two interpreted runs agree perfectly, so a
/// tier differential that armed nothing — or generated programs that call nothing — passes while
/// testing nothing. Three separate ways it could be hollow, each checked.
#[test]
fn the_generated_programs_actually_reach_the_transpiled_band() {
    if !have_bosl2() {
        return;
    }
    let (registry, surface) = harness();

    // (1) The surface is the LIBRARY's, not fab-lang's 85 and not the builtins.
    assert_eq!(
        fab_bosl2::Bosl2.callables().len(),
        1329,
        "the generation surface is not BOSL2's declared set"
    );

    // (2) The generated text really does call library functions. Without the preamble riding along
    // from `from_library`, every one of them would be an unknown function on BOTH legs.
    let names: Vec<&str> = fab_bosl2::Bosl2
        .callables()
        .iter()
        .map(|d| d.name)
        .collect();
    let mut calling = 0;
    for seed in 0..24 {
        let src = generate_against(seed, Profile::AB, &surface);
        assert!(
            src.contains("include <BOSL2/std.scad>"),
            "seed {seed} lost the library's preamble"
        );
        if names.iter().any(|n| src.contains(&format!("{n}("))) {
            calling += 1;
        }
    }
    assert!(
        calling > 0,
        "no generated program called a single BOSL2 function — the surface is not being used"
    );

    // (3) The natives ARM for these programs. `wired_count` is a last-value, so read it after a run.
    let src = generate_against(0, Profile::AB, &surface);
    let _ = run(&src, &registry, true);
    assert!(
        fab_lang::wired_count() > 800,
        "only {} natives wired — the differential is comparing the interpreter against itself",
        fab_lang::wired_count()
    );
}
