//! `$fn`/`$fa`/`$fs` → fragment count — the tessellation resolution at the heart of the tracer
//! bullet. Ported from OpenSCAD's fragment formula (master: `CurveDiscretizer::getCircularSegmentCount`,
//! `src/core/CurveDiscretizer.cc`; the legacy `Calc::get_fragments_from_r` is byte-identical for the
//! `$fn == 0` branch). The `$fn/$fa/$fs` clamps happen HERE (OpenSCAD applies them at discretizer
//! construction, before the formula).
//!
//! The `$fn > 0` rounding is version-sensitive (master `ceil()`, 2021.01 truncation — diverges only
//! for NON-INTEGER `$fn`: `$fn = 6.5` → 7 vs 6). RESOLVED at G.3.6: the installed oracle is nightly
//! (2026.06.12 = master), so we match `ceil()`.

use std::f64::consts::PI;

/// `GRID_FINE = 2^-20` — below this radius a curve degenerates to the fallback fragment count
/// (`src/geometry/Grid.h`).
const GRID_FINE: f64 = 0.000_000_953_674_316_406_25;
/// `F_MINIMUM = 0.01` — the floor `$fa` and `$fs` are clamped to (`CurveDiscretizer.cc`).
const F_MINIMUM: f64 = 0.01;
/// The fragment count for a degenerate curve — OpenSCAD's `.value_or(3)` at every call site.
const FALLBACK: u32 = 3;

/// The number of fragments a full circle of radius `r` is tessellated into, given `$fn`/`$fa`/`$fs`.
///
/// `$fn > 0` → `ceil(max($fn, 3))` (master/nightly rounds up). `$fn == 0` →
/// `ceil(max(min(360/$fa, 2·π·r/$fs), 5))`. Degenerate (`r < GRID_FINE`, or non-finite `$fn`/`r`) → 3.
#[must_use]
pub fn fragments(r: f64, fn_: f64, fa: f64, fs: f64) -> u32 {
    // Clamps applied before the formula (OpenSCAD does these at discretizer construction).
    let fn_ = if fn_ < 0.0 { 0.0 } else { fn_ }; // $fn < 0 → 0
    let fa = fa.max(F_MINIMUM);
    let fs = fs.max(F_MINIMUM);

    if !r.is_finite() || !fn_.is_finite() || r < GRID_FINE {
        return FALLBACK;
    }

    let result = if fn_ > 0.0 {
        // master/nightly rounds UP: ceil(max($fn, 3)). (2021.01 truncated; identical for integer $fn.)
        // The installed oracle is nightly (2026.06.12), so we match ceil. G.3.6-resolved.
        (if fn_ >= 3.0 { fn_ } else { 3.0 }).ceil()
    } else {
        // $fn == 0 branch — byte-identical master/legacy.
        (360.0 / fa).min(r * 2.0 * PI / fs).max(5.0).ceil()
    };

    // Cast is saturating in Rust (never UB); the value is a small non-negative integer here.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "result is a finite non-negative fragment count; f64->u32 saturates, never UB"
    )]
    let n = result as u32;
    n.max(FALLBACK)
}
