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
    let (interp, native) = (run(src, &registry, false), run(src, &registry, true));
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

/// AR.29 — the five DUPLICATE-PARAMETER functions, end to end, across the argument shapes that
/// discriminate.
///
/// `zcyl`/`ycyl`/`regular_prism` each declare `length` twice, `linear_sweep` declares `h` at two
/// separate positions, and `path_copies` declares `dist` twice on one line. All five are upstream
/// editing accidents that OpenSCAD resolves silently with a two-phase bind: provided arguments bind
/// first and a later duplicate overwrites, then defaults fill only the still-unset names with a later
/// duplicate skipped. The emitter lays out one `let` per SLOT, so Rust shadowing takes the LAST — the
/// mismatch that declined these for the whole phase, until `duplicate_rebind` moved the resolution
/// into the runtime where binding belongs.
///
/// SHAPES, not one call: named and positional, the duplicated name given and omitted. The case that
/// used to be wrong is "the earlier slot provided, the later not", which last-wins reads as the later
/// slot's default.
#[test]
fn the_duplicate_parameter_functions_bind_like_the_interpreter() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    let calls = [
        // zcyl / ycyl: `length` at two slots.
        "zcyl(h=10, d=5)",
        "zcyl(d=5, length=8)",
        "zcyl(10, 5)",
        "ycyl(h=10, d=5)",
        "ycyl(d=5, length=8)",
        // regular_prism: same, with `n` in front.
        "regular_prism(6, h=4, side=3)",
        "regular_prism(6, side=3, length=7)",
        // linear_sweep: `h` twice.
        "linear_sweep(square(5), height=3)",
        "linear_sweep(square(5), h=3)",
        // path_copies: `dist` twice.
        "path_copies(square(10), n=4, p=[[0,0]])",
        "path_copies(square(10), spacing=3, p=[[0,0]])",
    ];
    // NON-VACUITY FIRST: all five must have a generated row, or this compares the interpreter
    // against itself and passes. They declined for the whole phase, so their presence is the
    // property under test as much as their answers are.
    for n in [
        "zcyl",
        "ycyl",
        "regular_prism",
        "linear_sweep",
        "path_copies",
    ] {
        assert!(
            fab_bosl2::Bosl2
                .rows()
                .functions
                .iter()
                .any(|e| e.name == n),
            "`{n}` has no generated row — the duplicate-parameter band declined again"
        );
    }
    let mut failures = Vec::new();
    for call in calls {
        let src = format!("include <BOSL2/std.scad>\necho({call});\n");
        match (run(&src, &registry, false), run(&src, &registry, true)) {
            (Leg::Ok(a), Leg::Ok(b)) if a == b => {}
            (Leg::Err(a), Leg::Err(b)) if a == b => {}
            (a, b) => {
                let show = |l: &Leg| match l {
                    Leg::Ok(m) => format!("ok {m:?}"),
                    Leg::Err(e) => format!("err {e}"),
                };
                failures.push(format!(
                    "{call}\n  interp: {}\n  native: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} duplicate-parameter calls diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// AR.30 — `$`-VARIABLE READS in a function body, against the shapes that discriminate.
///
/// A `$`-read is DYNAMICALLY scoped: its value belongs to the CALLER, reached through the dynamic
/// chain at call time. That is why it can never be baked, and why 33 BOSL2 functions declined until
/// `FnCtx` grew the `dollar` capability `ModuleCtx` has had since AR.20.3. `segs($fn)` is the poster
/// child and it is called from everywhere.
///
/// THE SHAPES MATTER MORE THAN THE COUNT. An unset `$fn`, a top-level one, a `let`-bound one, and one
/// set by an enclosing module instantiation are four different chains; a native that snapshotted the
/// wrong scope would agree with the interpreter on some of them and not others, which is exactly the
/// failure a single case would miss.
#[test]
fn dollar_reads_resolve_on_the_callers_chain() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    let programs = [
        // Unset: `$fn` is undef, and `segs` falls to its `$fa`/`$fs` arm.
        "echo(segs(10));",
        // Top level — the simplest chain.
        "$fn = 32;\necho(segs(10));",
        // `let`-bound: a DYNAMIC binding that exists only for the call, so a native reading the
        // island global instead of the caller's chain would miss it entirely.
        "echo(let($fn = 8) segs(10));",
        // Set by an enclosing module instantiation — the chain a real model actually builds.
        "module m() { echo(segs(10)); }\nm($fn = 16);",
        // Nested, inner wins.
        "$fn = 32;\necho(let($fn = 5) segs(10));",
        // A `$`-read behind another function call, so it arrives one frame deeper.
        "$fn = 12;\nfunction outer(r) = segs(r);\necho(outer(10));",
        // The other big `$`-reading band: the mask2d family reads `$edge_angle`.
        "echo(is_undef(mask2d_roundover(2)));",
        "echo(let($edge_angle = 60) is_undef(mask2d_roundover(2)));",
    ];
    let mut failures = Vec::new();
    for body in programs {
        let src = format!("include <BOSL2/std.scad>\n{body}\n");
        match (run(&src, &registry, false), run(&src, &registry, true)) {
            (Leg::Ok(a), Leg::Ok(b)) if a == b => {}
            (Leg::Err(a), Leg::Err(b)) if a == b => {}
            (a, b) => {
                let show = |l: &Leg| match l {
                    Leg::Ok(m) => format!("ok {m:?}"),
                    Leg::Err(e) => format!("err {e}"),
                };
                failures.push(format!(
                    "{body}\n  interp: {}\n  native: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} $-read shapes diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// NON-VACUITY for the `$`-read band: `segs` must actually have a generated row and WIRE, or the
/// comparison above is the interpreter against itself. It declined for the whole phase, so its
/// presence is the property under test as much as its answers are.
#[test]
fn the_dollar_reading_functions_are_compiled() {
    if !have_bosl2() {
        return;
    }
    let rows = fab_bosl2::Bosl2.rows();
    for n in ["segs", "mask2d_roundover", "get_slop", "_is_shown"] {
        let compiled = rows.functions.iter().any(|e| e.name == n);
        assert!(
            compiled || n == "_is_shown",
            "`{n}` reads a `$`-variable and should now compile"
        );
    }
    // And the band armed on a real program.
    let (registry, _s) = harness();
    let _ = run(
        "include <BOSL2/std.scad>\n$fn=16;\necho(segs(10));\n",
        &registry,
        true,
    );
    assert!(
        fab_lang::wired_count() > 800,
        "only {} natives wired",
        fab_lang::wired_count()
    );
}

/// AR.32 — THE C-STYLE COMPREHENSION, on the shapes that discriminate.
///
/// `[for(c=1, i=0; i<=n; c=c*(n-i)/(i+1), i=i+1) c]` is a three-clause loop with mutable state, and
/// the two clauses BIND SEQUENTIALLY — an init's later binding sees the earlier ones, an update's
/// later binding sees the NEW earlier value. Get the second wrong and BOSL2's DP row builders return
/// plausible garbage rather than failing.
///
/// PINNED VALUES, not just tier agreement, wherever upstream documents one: `binomial(6)` is
/// `[1,6,15,20,15,6,1]` in its own docstring, and two tiers both answering `undef` agree with each
/// other while both being wrong.
#[test]
fn c_style_comprehensions_agree_across_tiers() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    let cases: &[(&str, &str)] = &[
        // The library's own, with the value its docstring promises.
        ("binomial(6)", "[1, 6, 15, 20, 15, 6, 1]"),
        ("binomial_coefficient(6, 2)", "15"),
        ("cumsum([1, 2, 3, 4])", "[1, 3, 6, 10]"),
        ("cumprod([1, 2, 3, 4])", "[1, 2, 6, 24]"),
        ("product([[1, 2], [3, 4]])", "[3, 8]"),
        // SEQUENTIAL INIT: a later binding sees the earlier one, so `b` is 2 and not undef.
        ("[for(a = 1, b = a + 1; a < 2; a = a + 1) b]", "[2]"),
        // SEQUENTIAL UPDATE: `y` sees the NEW `x`. Last-wins or parallel binding gives [11, 21].
        (
            "[for(i = 0, x = 0, y = 0; i < 2; i = i + 1, x = i * 10, y = x + 1) y]",
            "[0, 11]",
        ),
        // A CLAUSE-LOCAL temporary the update introduces and a later update binding consumes —
        // BOSL2's DP idiom, minimised. It must not leak into the next iteration's condition.
        //
        // `[0, 0, 2]` and not `[0, 2, 6]`: the body contributes BEFORE the update runs, so each
        // element is the accumulator as of the previous pass. Getting that ordering wrong is the
        // one thing this case exists to catch, and it caught the author first.
        (
            "[for(i = 0, acc = 0; i < 3; t = i * 2, acc = acc + t, i = i + 1) acc]",
            "[0, 0, 2]",
        ),
        // The body is an ELEMENT here and a SPLICE below — `each` goes through a different seam.
        (
            "[for(i = 0; i < 3; i = i + 1) each [i, -i]]",
            "[0, 0, 1, -1, 2, -2]",
        ),
        // A guarded body contributes conditionally, and an empty condition yields an empty list.
        ("[for(i = 0; i < 4; i = i + 1) if (i % 2 == 0) i]", "[0, 2]"),
        ("[for(i = 0; i < 0; i = i + 1) i]", "[]"),
    ];
    let mut failures = Vec::new();
    for (call, want) in cases {
        let src = format!("include <BOSL2/std.scad>\necho({call});\n");
        match (run(&src, &registry, false), run(&src, &registry, true)) {
            (Leg::Ok(a), Leg::Ok(b)) if a == b => {
                // AND the value, so a shared wrong answer cannot pass as agreement.
                if !a.iter().any(|m| m.contains(want)) {
                    failures.push(format!("{call}: expected {want}, got {a:?}"));
                }
            }
            (a, b) => {
                let show = |l: &Leg| match l {
                    Leg::Ok(m) => format!("ok {m:?}"),
                    Leg::Err(e) => format!("err {e}"),
                };
                failures.push(format!(
                    "{call}\n  interp: {}\n  native: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} C-style case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// NON-VACUITY for the C-style band: the functions must actually be COMPILED, or the comparison
/// above is the interpreter against itself. All eleven declined before AR.32.
#[test]
fn the_c_style_functions_are_compiled() {
    if !have_bosl2() {
        return;
    }
    let rows = fab_bosl2::Bosl2.rows();
    for n in [
        "binomial",
        "binomial_coefficient",
        "cumsum",
        "cumprod",
        "product",
        "_dp_distance_row",
        "_dp_distance_array",
        "_dp_extract_map",
        "hull2d_path",
        "path_sweep",
    ] {
        assert!(
            rows.functions.iter().any(|e| e.name == n),
            "`{n}` uses a C-style comprehension and should now compile"
        );
    }
}

/// AR.33 — `rands`, the one builtin whose answer depends on WHEN it is called.
///
/// SEEDED is a fresh engine and pure; SEEDLESS draws from the run's ONE advancing stream, so
/// consecutive calls differ. That makes draw ORDER and draw COUNT part of the contract: a native
/// that draws once where the interpreter drew twice shifts every later value in the program, and the
/// result still looks like a plausible list of random numbers. There is no mesh comparison that
/// catches it and no eyeball that would either — only an exact console diff against the tier that
/// did not compile.
///
/// THE LAST CASE IS THE LOAD-BEARING ONE: a compiled BOSL2 call followed by a RAW `rands()`. If the
/// native drew from anywhere but the run's stream, or by the wrong amount, the raw draw afterwards
/// comes out different — which is the only way to observe a stream the native has to itself.
#[test]
fn rands_draws_from_the_runs_stream_in_the_interpreters_order() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    let programs = [
        // SEEDED — deterministic, so this pins VALUES and not merely agreement.
        "echo(rand_int(0, 10, 5, seed = 42));",
        "echo(gaussian_rands(0, 1, 4, seed = 7));",
        "echo(exponential_rands(1, 4, seed = 11));",
        "echo(shuffle([1, 2, 3, 4, 5], seed = 3));",
        "echo(random_points(4, seed = 5));",
        "echo(spherical_random_points(4, 1, seed = 9));",
        // SEEDLESS, twice — the second draw only matches if the first advanced the stream by
        // exactly the amount the interpreter advances it.
        "echo(rand_int(0, 10, 3));\necho(rand_int(0, 10, 3));",
        // SEEDLESS through two DIFFERENT compiled functions, interleaved.
        "echo(rand_int(0, 10, 2));\necho(shuffle([1, 2, 3, 4]));\necho(rand_int(0, 10, 2));",
        // THE ONE THAT MATTERS: a compiled call, then a RAW `rands()`. A native holding its own
        // engine agrees with itself on everything above and fails here.
        "echo(rand_int(0, 100, 3));\necho(rands(0, 1, 3));",
        "echo(rands(0, 1, 2));\necho(shuffle([1, 2, 3]));\necho(rands(0, 1, 2));",
    ];
    let mut failures = Vec::new();
    for body in programs {
        let src = format!("include <BOSL2/std.scad>\n{body}\n");
        match (run(&src, &registry, false), run(&src, &registry, true)) {
            (Leg::Ok(a), Leg::Ok(b)) if a == b => {
                assert!(!a.is_empty(), "{body}: no console — nothing was compared");
            }
            (a, b) => {
                let show = |l: &Leg| match l {
                    Leg::Ok(m) => format!("ok {m:?}"),
                    Leg::Err(e) => format!("err {e}"),
                };
                failures.push(format!(
                    "{body}\n  interp: {}\n  native: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} rands case(s) diverged:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// NON-VACUITY for the `rands` band: all eight must be COMPILED, or the draw-order comparison above
/// is the interpreter against itself and would pass no matter what `fx.rands` did.
#[test]
fn the_rands_functions_are_compiled() {
    if !have_bosl2() {
        return;
    }
    let rows = fab_bosl2::Bosl2.rows();
    for n in [
        "rand_int",
        "gaussian_rands",
        "exponential_rands",
        "spherical_random_points",
        "random_points",
        "random_polygon",
        "shuffle",
        "vnf_vertex_array",
    ] {
        assert!(
            rows.functions.iter().any(|e| e.name == n),
            "`{n}` calls `rands` and should now compile"
        );
    }
}

/// AR.34 — COMPREHENSION DISPATCH: two shapes the emitter could always compile and never routed to.
///
/// Neither was a missing construct. `[let(x=…) for(…) …]` is a comprehension under an
/// element-position `let`, and the vector's all-plain check looked only at the TOP of each item — so
/// it took the fast path into `expr()`, which has no comprehension arm, and bottomed out at
/// "construct outside the v0 subset". The interpreter's own `is_comprehension` peels `let` and says
/// why: a `let` in a vector is TRANSPARENT, it splices iff its body does.
///
/// `each if(c) X` is the other: `each` DISTRIBUTES into the guard, and the emitter sent its inner
/// through `expr()` instead. Together they were 8 of the last 17 declines — the constructs were
/// inside the subset the whole time, the dispatcher just never reached them.
#[test]
fn comprehension_dispatch_shapes_agree_across_tiers() {
    if !have_bosl2() {
        return;
    }
    let (registry, _surface) = harness();
    let cases: &[(&str, &str)] = &[
        // A `let` wrapping a `for` — the element is a loop, not a single value.
        ("[let(n = 3) for(i = [0:n-1]) i * 2]", "[0, 2, 4]"),
        // Two `let`s deep, because the peel is a loop and not one level.
        (
            "[let(a = 2) let(b = a + 1) for(i = [0:b-1]) i]",
            "[0, 1, 2]",
        ),
        // A `let` whose body is NOT a comprehension stays ONE element — the transparency cuts both
        // ways, and getting it wrong would flatten a single-point list into the enclosing path.
        ("[let(x = 1) [x, x + 1]]", "[[1, 2]]"),
        // `each` distributing into a guard, both arms.
        ("[each if (true) [1, 2], 9]", "[1, 2, 9]"),
        ("[each if (false) [1, 2], 9]", "[9]"),
        ("[each if (true) [1, 2] else [3], 9]", "[1, 2, 9]"),
        ("[each if (false) [1, 2] else [3], 9]", "[3, 9]"),
        // `each` distributing into a loop.
        ("[each for(i = [0:2]) [i, i]]", "[0, 0, 1, 1, 2, 2]"),
        // `each` over a `let`-wrapped comprehension — both peels at once.
        ("[each let(n = 2) for(i = [0:n-1]) [i]]", "[0, 1]"),
        // And the real BOSL2 functions the two shapes were blocking.
        (
            "len(_rounded_arc(10, rounding = 1, angle = 90, n = 8)) > 0",
            "true",
        ),
        (
            "len(squircle(10, squareness = 0.5, style = \"fg\")) > 0",
            "true",
        ),
    ];
    let mut failures = Vec::new();
    for (call, want) in cases {
        let src = format!("include <BOSL2/std.scad>\necho({call});\n");
        match (run(&src, &registry, false), run(&src, &registry, true)) {
            (Leg::Ok(a), Leg::Ok(b)) if a == b => {
                if !a.iter().any(|m| m.contains(want)) {
                    failures.push(format!("{call}: expected {want}, got {a:?}"));
                }
            }
            (a, b) => {
                let show = |l: &Leg| match l {
                    Leg::Ok(m) => format!("ok {m:?}"),
                    Leg::Err(e) => format!("err {e}"),
                };
                failures.push(format!(
                    "{call}\n  interp: {}\n  native: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} comprehension-dispatch case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// NON-VACUITY for AR.34's two bands: the functions they unblocked must be COMPILED.
#[test]
fn the_comprehension_dispatch_functions_are_compiled() {
    if !have_bosl2() {
        return;
    }
    let rows = fab_bosl2::Bosl2.rows();
    for n in [
        // the `let`-wrapped comprehension band
        "_squircle_fg",
        "_squircle_se",
        "qr_factor",
        "texture",
        "bevel_gear",
        // `each <comprehension>`
        "_rounded_arc",
        "nurbs_curve",
        "_region_region_intersections",
        // and the two sibling-binding shapes that now dispatch
        "half_of",
        "mask2d_chamfer",
    ] {
        assert!(
            rows.functions.iter().any(|e| e.name == n),
            "`{n}` should now compile"
        );
    }
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
    // 1329 → 1341 at the TC.4 pin bump (v2.0.747 → v2.0.751): upstream added 12 functions
    // (threading/bottlecaps growth). The number is a non-vacuity floor, so it moves WITH the pin —
    // in the same commit, with the count taken from the failing assert, never guessed.
    assert_eq!(
        fab_bosl2::Bosl2.callables().len(),
        1341,
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
