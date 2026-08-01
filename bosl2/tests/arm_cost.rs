//! AR.19's number, measurable at last: what does ARMING an 866-row library cost per evaluation?
//!
//! `build_intrinsics` fingerprints every defined function once at `Ctx` build, and
//! `arm_guarded_intrinsics` then re-resolves each const-guarded row and walks its dep list, doing a
//! full AST fingerprint per (row, dep) pair. At 83 hand rows nobody noticed. AR.26.2 measured the
//! shape at library scale — 437 of 866 rows need the post-hoist arm, ~4500 fingerprints and as many
//! bake-builder calls — and this is the wall-clock that shape actually costs.
//!
//! `#[ignore]`d: it is a measurement, not a gate. A gate would need a threshold, and a threshold on
//! a laptop's wall clock is a flake generator. Run it with
//! `cargo test -p fab-bosl2 --release --test arm_cost -- --ignored --nocapture`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "measurement harness: expect IS the assertion"
)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use fab_lang::registry::Registry;
use fab_lang::surface::LibrarySurface;
use fab_lang::Config;

fn libs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("libs")
}

/// One render, timed, against `registry`.
fn timed(src: &str, registry: &Registry, intrinsics: bool) -> f64 {
    let config = Config {
        intrinsics,
        ..Config::default()
    };
    let t = Instant::now();
    fab_lang::evaluate_geometry_with_registry(src, Path::new("."), &[libs()], registry, config)
        .expect("evaluates");
    t.elapsed().as_secs_f64() * 1000.0
}

/// Best of N — the arming cost is deterministic work, so the minimum is the honest number and the
/// spread is the machine's noise.
fn best(src: &str, registry: &Registry, intrinsics: bool, n: usize) -> f64 {
    (0..n)
        .map(|_| timed(src, registry, intrinsics))
        .fold(f64::INFINITY, f64::min)
}

#[test]
#[ignore = "measurement, not a gate"]
fn arming_an_866_row_library_costs() {
    if !fab_bosl2::transpiled() || !libs().join("BOSL2/std.scad").exists() {
        eprintln!("skipping: libs/BOSL2 not checked out");
        return;
    }
    // A program that INCLUDES the whole library but builds almost nothing, so what it measures is
    // ctx build and arming rather than geometry. That separation is the point: AR.0's whole finding
    // was that geometry dominates wall time, which would hide this.
    const THIN: &str = "include <BOSL2/std.scad>\ncube(1);\n";
    // And one that does real work, for the ratio that actually matters to a user.
    const FAT: &str = "include <BOSL2/std.scad>\n\
                       cuboid([10, 20, 30], rounding = 2);\n\
                       right(40) cyl(h = 20, r = 6, chamfer = 1, $fn = 64);\n";

    let bosl2 = Registry::new().with(fab_bosl2::Bosl2.rows());
    let empty = Registry::new();
    let builtin = Registry::builtin();
    // Prime every lazy index so the first timed run is not paying for one.
    let _ = (bosl2.function_count(), bosl2.module_count());
    let _ = (builtin.function_count(), builtin.module_count());

    println!("\n=== AR.19: what arming costs (best of 7, ms) ===");
    println!(
        "rows: bosl2 {} fns / {} mods, builtin {} fns / {} mods",
        bosl2.function_count(),
        bosl2.module_count(),
        builtin.function_count(),
        builtin.module_count()
    );
    for (label, src) in [("thin (include + cube)", THIN), ("fat (real geometry)", FAT)] {
        let off = best(src, &empty, false, 7);
        let none = best(src, &empty, true, 7);
        let hand = best(src, builtin, true, 7);
        let full = best(src, &bosl2, true, 7);
        println!("  {label}");
        println!("    interpreter only            {off:8.2}");
        println!("    empty registry, tier on     {none:8.2}");
        println!(
            "    builtin registry ({:4} rows) {:8.2}",
            builtin.function_count(),
            hand
        );
        println!(
            "    BOSL2 registry   ({:4} rows) {:8.2}   delta vs empty {:+.2}",
            bosl2.function_count(),
            full,
            full - none
        );
    }
}
