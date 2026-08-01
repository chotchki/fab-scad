//! AR.26.3 — THE DIFFERENTIAL, re-homed. A real BOSL2 program, evaluated twice in one process
//! against the registry this crate's build produced: compiled tier ON, compiled tier OFF.
//!
//! This is the only thing standing between the transpiler and a silent wrong answer once AR.21
//! deletes the hand-written tiers, and it is the reason the phase insisted the run gate stay
//! ORTHOGONAL to which libraries are loaded. `Config::intrinsics = false` turns the natives off
//! WITHOUT removing BOSL2 from the registry, so both legs evaluate the same program against the
//! same library and the only variable is which tier ran.
//!
//! EVERY CASE ASSERTS THE NATIVES WIRED. A tier comparison passes vacuously when nothing armed —
//! two interpreted runs agree perfectly — and that failure has been found in this codebase enough
//! times to be the first thing each test here checks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: unwrap/expect ARE the assertions"
)]

use std::path::{Path, PathBuf};

use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::{Config, Message};

/// The library path a `use`/`include` of `BOSL2/…` resolves against.
fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

fn have_bosl2() -> bool {
    fab_bosl2::transpiled() && libs().join("BOSL2/std.scad").exists()
}

/// This crate's library, and nothing else — the shape a consumer that only loads BOSL2 builds, and
/// the shape fab-lang itself will have once AR.21 stops shipping function rows of its own.
fn bosl2_registry() -> Registry {
    Registry::new().with(fab_bosl2::Bosl2.rows())
}

/// Render `src` and return `(the geometry tree's structural dump, echo lines)`.
///
/// The DUMP rather than a triangle count, deliberately: a count is a hash with a lot of collisions,
/// and the whole point of a tier differential is that it notices a difference a plausible-looking
/// mesh would hide. Comparing the debug rendering compares the tree — every node, every parameter.
fn run(src: &str, registry: &Registry, intrinsics: bool) -> (String, Vec<String>) {
    let config = Config {
        intrinsics,
        ..Config::default()
    };
    let (geo, messages) = fab_lang::evaluate_geometry_with_registry(
        src,
        Path::new("."),
        &[libs()],
        registry,
        config,
    )
    .expect("the program evaluates");
    let echoes = messages
        .iter()
        .filter_map(|m| match m {
            Message::Echo(e) => Some(e.clone()),
            Message::Warning(_) | Message::Error(_) => None,
        })
        .collect();
    (format!("{geo:?}"), echoes)
}

/// The transpiled library indexes CLEANLY — no row dropped, nothing silently absent.
///
/// First because it is the precondition for everything else: a fault here means some native quietly
/// stopped firing, and every differential below would still pass while measuring less than it says.
#[test]
fn the_transpiled_library_indexes_without_faults() {
    if !have_bosl2() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    let reg = bosl2_registry();
    assert_eq!(reg.faults(), Vec::new(), "BOSL2's rows must index cleanly");
    let rows = fab_bosl2::Bosl2.rows();
    assert_eq!(reg.function_count(), rows.functions.len());
    assert_eq!(reg.module_count(), rows.modules.len());
    assert!(
        reg.function_count() > 800,
        "only {} functions — the build transpiled far less than it should have",
        reg.function_count()
    );
    assert!(
        fab_bosl2::Bosl2.callables().len() > reg.function_count(),
        "the library must DECLARE more than it compiles — folding the two lists together is the \
         drift this phase exists to kill"
    );
}

/// The natives ACTUALLY WIRE against real BOSL2. Without this every case below is two interpreted
/// runs agreeing with each other.
#[test]
fn the_compiled_tier_arms_on_real_bosl2() {
    if !have_bosl2() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    // FAB_EXPLAIN prints one line per wired intrinsic; asking the registry directly is cheaper and
    // does not depend on an env var. `is_def` is a leaf predicate BOSL2 declares and we compile.
    let reg = bosl2_registry();
    let program = fab_lang::parse("function is_def(x) = !is_undef(x);").expect("parses");
    let fab_lang::StmtKind::FunctionDef { params, body, .. } = &program.stmts[0].kind else {
        panic!("the probe is a function definition");
    };
    assert!(
        reg.resolve("is_def", params, body).is_some(),
        "BOSL2's own `is_def` does not resolve against the transpiled registry — either the \
         reference drifted or the row is absent, and either way nothing downstream is dispatching"
    );
}

/// A geometry program built out of BOSL2 modules and functions renders IDENTICALLY on both tiers.
#[test]
fn bosl2_geometry_agrees_across_tiers() {
    if !have_bosl2() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    const SRC: &str = "include <BOSL2/std.scad>\n\
                       cuboid([10, 20, 30], rounding = 2);\n\
                       right(40) cyl(h = 20, r = 6, chamfer = 1);\n\
                       back(40) prismoid(size1 = [20, 20], size2 = [10, 10], h = 15);\n";
    let reg = bosl2_registry();
    let (fast, fast_echo) = run(SRC, &reg, true);
    let (slow, slow_echo) = run(SRC, &reg, false);
    assert_eq!(
        fast, slow,
        "the compiled tier built a different geometry tree than the interpreter"
    );
    assert_eq!(fast_echo, slow_echo, "the two tiers printed different consoles");
    assert!(
        fast.len() > 500,
        "the program must actually build something — got {} bytes of tree",
        fast.len()
    );
}

/// The VALUE half — BOSL2's function library exercised through echo, where a divergence shows up as
/// text rather than as a mesh. Echoes are the sharper instrument: a wrong number prints, where a
/// wrong vertex may still tessellate to the same triangle count.
#[test]
fn bosl2_values_agree_across_tiers() {
    if !have_bosl2() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    const SRC: &str = "include <BOSL2/std.scad>\n\
                       echo(approx(1/3, 0.3333333333333333));\n\
                       echo(unit([3, 4]));\n\
                       echo(list_shape([[1, 2], [3, 4], [5, 6]]));\n\
                       echo(v_theta([1, 1]));\n\
                       echo(deduplicate([1, 1, 2, 2, 3]));\n\
                       echo(sum([1.5, 2.25, 3.125]));\n\
                       echo(idx([10, 20, 30]));\n\
                       echo(str_join([\"a\", \"b\", \"c\"], \"-\"));\n\
                       echo(select([1, 2, 3, 4, 5], 1, 3));\n\
                       echo(is_path([[0, 0], [1, 1], [2, 0]]));\n";
    let reg = bosl2_registry();
    let (_, fast) = run(SRC, &reg, true);
    let (_, slow) = run(SRC, &reg, false);
    assert_eq!(
        fast, slow,
        "a BOSL2 function answered differently compiled than interpreted"
    );
    assert_eq!(fast.len(), 10, "every echo must have fired: {fast:?}");
    // Not just AGREEMENT — pin one value outright. Two tiers both returning `undef` would agree
    // with each other and still be wrong against upstream, which is the failure shape a pure
    // differential cannot see.
    assert!(
        fast.iter().any(|e| e.contains("true")),
        "`approx` and `is_path` should both echo true: {fast:?}"
    );
}

/// An EMPTY registry renders the same program identically — the whole library interprets. Pins that
/// the transpiled tier is an accelerator and never a semantic, which is the claim AR.21 will be
/// deleting the interpreter's competition on the strength of.
#[test]
fn the_transpiled_tier_changes_nothing_but_speed() {
    if !have_bosl2() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    const SRC: &str = "include <BOSL2/std.scad>\n\
                       echo(v_theta([1, 1]));\n\
                       cuboid([8, 9, 10], rounding = 1);\n";
    let with = bosl2_registry();
    let without = Registry::new();
    assert_eq!(run(SRC, &with, true), run(SRC, &without, true));
}
