//! # Semantics: circle/sphere fragment count (`$fn` / `$fa` / `$fs`)
//!
//! Provenance: OpenSCAD `src/core/CurveDiscretizer.cc` `getCircularSegmentCount` (master); the
//! legacy `Calc::get_fragments_from_r` is byte-identical for the `$fn == 0` branch.
//! Oracle: the installed nightly (2026.06.12 = master) rounds `$fn > 0` UP (ceil) — G.3.7's sphere
//! sweep confirmed the resulting vertex counts match the oracle EXACTLY (32 → 32768).

use fab_lang::fragments;

/// FACT: `$fn > 0` yields `ceil(max($fn, 3))` — master rounds UP (2021.01 truncated). It only
/// diverges for a NON-INTEGER `$fn`; the usual integer case is identical either way.
#[test]
fn positive_fn_rounds_up() {
    assert_eq!(fragments(5.0, 8.0, 12.0, 2.0), 8); // integer $fn
    assert_eq!(fragments(5.0, 6.5, 12.0, 2.0), 7); // non-integer → ceil, not trunc
    assert_eq!(fragments(5.0, 2.0, 12.0, 2.0), 3); // floored at 3
}

/// FACT: `$fn == 0` uses `ceil(max(min(360/$fa, 2·π·r/$fs), 5))` — the finer of the angle bound and
/// the size bound, never fewer than 5.
#[test]
fn zero_fn_uses_fa_and_fs() {
    assert_eq!(fragments(10.0, 0.0, 12.0, 2.0), 30); // $fa bound wins: 360/12 = 30
    assert_eq!(fragments(1.0, 0.0, 12.0, 2.0), 5); // tiny circle floored at 5
}

/// FACT: a curve below `GRID_FINE` (2⁻²⁰) radius degenerates to the fallback count 3.
#[test]
fn degenerate_radius_is_three() {
    assert_eq!(fragments(0.0, 0.0, 12.0, 2.0), 3);
    assert_eq!(fragments(f64::NAN, 0.0, 12.0, 2.0), 3); // non-finite too
}

/// FACT: the clamps applied BEFORE the formula — `$fn < 0 → 0`, and `$fa`/`$fs` floored at 0.01.
#[test]
fn clamps_precede_the_formula() {
    // $fn < 0 collapses to the $fn == 0 branch.
    assert_eq!(
        fragments(5.0, -1.0, 12.0, 2.0),
        fragments(5.0, 0.0, 12.0, 2.0)
    );
    // $fa/$fs = 0 floored to 0.01 → a very large (but finite) count.
    assert!(fragments(5.0, 0.0, 0.0, 0.0) > 30);
}
