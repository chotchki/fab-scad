//! The differential metric (G.3.7) — scad-rs's mesh vs the OpenSCAD oracle, across tiers of
//! increasing strictness, to find the STRICTEST tier that holds per model class.
//!
//! Tiers (loose → strict):
//!   1. quantized vertex-MULTISET — do the vertex SETS agree on a grid? Order-independent (the
//!      oracle reindexes through Manifold), and the grid absorbs the export's ~1e-6 quantization.
//!   2. bulk metrics — volume + surface area (relative tolerance), genus (exact). Size + topology,
//!      triangulation-independent.
//!   3. boolean residual — `vol((A−B) ∪ (B−A)) / vol(A)`, the actual solid symmetric difference:
//!      ~0 means "the SAME solid" no matter how each was triangulated. The strongest tier.
//!
//! Both engines' meshes become a `kernel::Solid` (Manifold) — this IS the downstream
//! `Mesh → Solid::from_indexed` hand-off the G.3.5 architecture deferred to here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::kernel::Solid;
use crate::oracle;
use fab_lang::Vec3;

/// One source compared across every tier.
#[derive(Debug, Clone)]
pub struct Comparison {
    pub scad_verts: usize,
    pub oracle_verts: usize,
    pub scad_tris: usize,
    pub oracle_tris: usize,
    /// Smallest grid `eps` (from [`EPS_LADDER`]) at which the vertex multisets match; None if none did.
    pub vertex_match_eps: Option<f64>,
    pub volume_rel_err: f64,
    pub area_rel_err: f64,
    pub genus_scad: i32,
    pub genus_oracle: i32,
    /// `vol((A−B) ∪ (B−A)) / vol(A)` — the symmetric-difference ratio (0 = identical solids).
    pub boolean_residual: f64,
}

/// Grid epsilons probed for the vertex-multiset tier (strict → loose). The smallest that matches is
/// the tier's achieved strictness.
pub const EPS_LADDER: [f64; 6] = [1e-9, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3];

/// Compare scad-rs and the oracle on `source`. `timeout` bounds the oracle render.
///
/// # Errors
/// Fails if either engine errors, or a mesh can't be realized as a manifold `Solid`.
pub fn compare(source: &str, timeout: Duration) -> Result<Comparison> {
    // scad-rs engine → Solid.
    let mesh = fab_lang::evaluate(source).context("scad-rs evaluate")?;
    let scad_solid =
        Solid::from_indexed(&mesh.verts, &mesh.tris).context("scad-rs mesh → Solid")?;

    // oracle engine → Solid.
    let run = oracle::run(source, timeout).context("oracle run")?;
    let oracle_tris = run.mesh.tris();
    let oracle_solid =
        Solid::from_indexed(&run.mesh.verts, &oracle_tris).context("oracle mesh → Solid")?;

    // Tier 1: the smallest grid at which the vertex multisets agree.
    let vertex_match_eps = EPS_LADDER
        .iter()
        .copied()
        .find(|&eps| vertex_multiset_matches(&mesh.verts, &run.mesh.verts, eps));

    // Tier 2: bulk metrics.
    let volume_rel_err = rel_err(scad_solid.volume(), oracle_solid.volume());
    let area_rel_err = rel_err(scad_solid.surface_area(), oracle_solid.surface_area());

    // Tier 3: boolean residual — the symmetric difference's volume, normalized.
    let boolean_residual = sym_diff_ratio(&scad_solid, &oracle_solid);

    Ok(Comparison {
        scad_verts: mesh.vert_count(),
        oracle_verts: run.mesh.vert_count(),
        scad_tris: mesh.tri_count(),
        oracle_tris: oracle_tris.len(),
        vertex_match_eps,
        volume_rel_err,
        area_rel_err,
        genus_scad: scad_solid.genus(),
        genus_oracle: oracle_solid.genus(),
        boolean_residual,
    })
}

/// The colors each engine produced for a program, quantized to 0-255 RGBA + deduped — the J.2.9 color
/// differential. Both sides reduce to the same distinct-color SET: our per-face colors vs the oracle's
/// per-face colors. That's tessellation- AND representation-independent (our kernel stores color
/// per-VERTEX, the oracle exports per-FACE — a face whose 3 verts agree is that color). The oracle's
/// default gold normalizes to uncolored. REGION-exact color matching (which face is which color, not
/// just which colors appear) is a documented future refinement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorComparison {
    /// scad-rs's distinct colors (sorted).
    pub scad_colors: Vec<[u8; 4]>,
    /// The oracle's distinct explicit colors (sorted, default gold excluded).
    pub oracle_colors: Vec<[u8; 4]>,
}

impl ColorComparison {
    /// Do both engines agree on the SET of colors present?
    #[must_use]
    pub fn matches(&self) -> bool {
        self.scad_colors == self.oracle_colors
    }
}

/// Compare the COLORS scad-rs and the oracle produce for `source` (J.2.9). `timeout` bounds the render.
///
/// # Errors
/// Fails if the oracle errors or scad-rs can't parse/evaluate the program.
pub fn compare_colors(source: &str, timeout: Duration) -> Result<ColorComparison> {
    let tree = fab_lang::evaluate_geometry(source).context("scad-rs evaluate_geometry")?;
    // Colors are a 3D display property; a 2D result has no 3D faces, so `build_geo` lowers it to the
    // empty solid → no distinct colors (compared equal to the oracle's colorless 2D output).
    let scad_colors = match crate::backend::build_geo(&tree, &crate::backend::ManifoldBackend) {
        Some(solid) => distinct_face_colors(&solid),
        None => Vec::new(),
    };
    let run = oracle::run(source, timeout).context("oracle run")?;
    let default = oracle_default_colors(timeout);
    let oracle_colors = run
        .mesh
        .distinct_colors()
        .into_iter()
        .filter(|q| !default.contains(q)) // drop OpenSCAD's colorscheme default (uncolored ⇒ no color)
        .collect();
    Ok(ColorComparison {
        scad_colors,
        oracle_colors,
    })
}

/// OpenSCAD leaks its colorscheme's DEFAULT object color onto an UNCOLORED CSG result's faces (`249 215
/// 44` on Cornfield, `157 203 81` on another scheme — a user preference, not geometry). Probe it once by
/// rendering a known-uncolored difference, so the differential can subtract it and an uncolored result
/// compares equal to our (colorless) one. Cached — it's constant per OpenSCAD install.
fn oracle_default_colors(timeout: Duration) -> Vec<[u8; 4]> {
    static DEFAULT: std::sync::OnceLock<Vec<[u8; 4]>> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            oracle::run(
                "difference() { cube(2); translate([0.5, 0.5, 0.5]) cube(2); }",
                timeout,
            )
            .map(|r| r.mesh.distinct_colors())
            .unwrap_or_default()
        })
        .clone()
}

/// The distinct colors of a solid's UNIFORMLY-colored faces (a triangle whose 3 verts quantize equal),
/// sorted + deduped. Seam triangles — whose verts carry Manifold's linear-blended props from a boolean
/// between differently-colored solids — have mixed vertex colors and are SKIPPED, matching OpenSCAD's
/// per-face (unblended) export. An uncolored solid → `[]`.
fn distinct_face_colors(solid: &Solid) -> Vec<[u8; 4]> {
    let Some(colors) = solid.vertex_colors() else {
        return Vec::new();
    };
    let (_verts, tris) = solid.to_indexed();
    let mut set: std::collections::BTreeSet<[u8; 4]> = std::collections::BTreeSet::new();
    for t in &tris {
        let [a, b, c] = t.indices();
        let q = |i: u32| oracle::quantize_color(colors[i as usize]);
        let (qa, qb, qc) = (q(a), q(b), q(c));
        if qa == qb && qb == qc {
            set.insert(qa);
        }
    }
    set.into_iter().collect()
}

/// Relative error `|a−b| / max(|a|,|b|)`.
fn rel_err(a: f64, b: f64) -> f64 {
    let denom = a.abs().max(b.abs()).max(f64::MIN_POSITIVE);
    (a - b).abs() / denom
}

/// Do the two vertex sets agree as MULTISETS when snapped to a grid of `eps`?
///
/// Caveat: a vertex sitting exactly on a grid boundary can round either way, so two near-equal
/// points straddling a cell edge quantize apart. Harmless for well-separated tessellation vertices
/// at a sane `eps`; a boundary-tolerant snap is the fix if a pathological corpus needs it.
#[must_use]
pub fn vertex_multiset_matches(a: &[Vec3], b: &[Vec3], eps: f64) -> bool {
    a.len() == b.len() && quantized_multiset(a, eps) == quantized_multiset(b, eps)
}

fn quantized_multiset(verts: &[Vec3], eps: f64) -> BTreeMap<[i64; 3], u32> {
    let mut m = BTreeMap::new();
    for v in verts {
        *m.entry(quantize(*v, eps)).or_insert(0) += 1;
    }
    m
}

/// Snap a vertex to an integer grid of `eps` units so near-equal floats collapse to one key.
fn quantize(v: Vec3, eps: f64) -> [i64; 3] {
    [
        (v[0] / eps).round() as i64,
        (v[1] / eps).round() as i64,
        (v[2] / eps).round() as i64,
    ]
}

// ───────────────── the two-driver differential (recon-gen / quicksight pattern) ──────────────────
//
// A `Driver` turns .scad SOURCE into a comparable `Outcome`, sealing the backend away so a test body
// stays engine-agnostic: `diff(scad)` runs it through EVERY registered driver and reports the first
// disagreement. Adding a driver (Phase-L's JIT — fast==JIT is the same discipline) makes every
// differential case check it too, for free. The rule "no test reaches a backend except through a
// Driver" is enforced in `tests/differential.rs` (a no-leak source lint + a both-drivers gate).

/// The comparable result of running a program through ONE engine.
pub enum Outcome {
    /// A realized manifold solid — compared by boolean residual (tessellation-independent).
    Solid(Solid),
    /// A valid program with NO geometry (both engines should agree it renders nothing).
    Empty,
    /// The engine rejected the program, or its mesh wasn't a manifold solid — an agreement axis of
    /// its own (do BOTH engines reject?).
    Rejected,
}

impl Outcome {
    fn kind(&self) -> &'static str {
        match self {
            Outcome::Solid(_) => "solid",
            Outcome::Empty => "empty",
            Outcome::Rejected => "rejected",
        }
    }
}

/// A differential driver — the test vocabulary. `name` discriminates it (quicksight's `dialect`).
pub trait Driver {
    /// The driver's short name, for divergence messages + the enforcement gate.
    fn name(&self) -> &'static str;
    /// Evaluate `.scad` source to a comparable [`Outcome`].
    fn eval(&self, scad: &str) -> Outcome;
    /// Evaluate a `.scad` FILE (resolving its `use`/`include` graph against `library_paths` after the
    /// file's own dir) to a comparable [`Outcome`] — the `use`/`include` differential's entry point.
    fn eval_file(&self, root: &Path, library_paths: &[PathBuf]) -> Outcome;
    /// The `ECHO:` console lines `scad` produces, trimmed, in order — the I.5 string-equal channel.
    fn echo(&self, scad: &str) -> Vec<String>;
}

/// scad-rs's own pure-Rust evaluator — the baseline.
pub struct FabLang;

impl Driver for FabLang {
    fn name(&self) -> &'static str {
        "fab-lang"
    }
    fn eval(&self, scad: &str) -> Outcome {
        fab_geometry_outcome(fab_lang::evaluate_geometry(scad))
    }
    fn eval_file(&self, root: &Path, library_paths: &[PathBuf]) -> Outcome {
        // Through the import reader (M.5/M.6), so an `import()`/`surface()` in the file resolves its mesh
        // instead of failing LOUD — a no-import file is unaffected (the reader is never called).
        fab_geometry_outcome(crate::import::resolve_geometry_file(root, library_paths))
    }
    fn echo(&self, scad: &str) -> Vec<String> {
        fab_lang::evaluate_full(scad)
            .map(|e| {
                e.echos()
                    .into_iter()
                    .map(|c| format!("ECHO: {c}"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Map scad-rs's geometry TREE to a comparable [`Outcome`]: walk it through the Manifold backend (J.2
/// — the geometry lowering under test), then no geometry → `Empty`, a manifold solid → `Solid`, an
/// evaluation error → `Rejected`. Manifold's ops always yield a valid manifold, so a `Some` solid is
/// never rejected here (that distinction is the oracle's job on the OTHER leg). A 2D result lowers to
/// `Empty` on this 3D-mesh axis — the oracle's 3D export of a 2D program is likewise empty, so they agree;
/// the 2D geometry differential (region/area) is a separate leg (J.3.7).
fn fab_geometry_outcome(result: fab_lang::Result<fab_lang::Geo>) -> Outcome {
    match result {
        Ok(tree) => match crate::backend::build_geo(&tree, &crate::backend::ManifoldBackend) {
            Some(solid) if !solid.is_empty() => Outcome::Solid(solid),
            _ => Outcome::Empty,
        },
        Err(_) => Outcome::Rejected,
    }
}

/// The real OpenSCAD binary — the oracle.
pub struct OpenScad;

impl Driver for OpenScad {
    fn name(&self) -> &'static str {
        "openscad"
    }
    fn eval(&self, scad: &str) -> Outcome {
        match oracle::run(scad, Duration::from_secs(30)) {
            Ok(run) if run.mesh.verts.is_empty() => Outcome::Empty,
            Ok(run) => {
                let tris = run.mesh.tris();
                Solid::from_indexed(&run.mesh.verts, &tris)
                    .map_or(Outcome::Rejected, Outcome::Solid)
            }
            Err(_) => Outcome::Rejected,
        }
    }
    fn eval_file(&self, root: &Path, library_paths: &[PathBuf]) -> Outcome {
        oracle_file_outcome(root, library_paths)
    }
    fn echo(&self, scad: &str) -> Vec<String> {
        oracle::run(scad, Duration::from_secs(30))
            .map(|run| run.echo.iter().map(|l| l.trim().to_string()).collect())
            .unwrap_or_default()
    }
}

/// Render a `.scad` FILE through the OpenSCAD binary (`library_paths` → `OPENSCADPATH`) → STL → `Solid`.
/// A missing binary / render failure / non-manifold export → `Rejected`.
fn oracle_file_outcome(root: &Path, library_paths: &[PathBuf]) -> Outcome {
    let os = if library_paths.is_empty() {
        crate::openscad::Openscad::discover(None)
    } else {
        crate::openscad::Openscad::with_library_paths(library_paths)
    };
    let Ok(os) = os else { return Outcome::Rejected };
    let out = root.with_extension("oracle-render.stl");
    match os.render(root, &out, Duration::from_secs(30)) {
        Ok(r) if r.ok => Solid::from_stl_file(&out).map_or(Outcome::Rejected, Outcome::Solid),
        _ => Outcome::Rejected,
    }
}

/// Every registered driver, fab-lang FIRST (the baseline). OpenSCAD is OPTIONAL — omitted when the
/// binary isn't installed, so a machine without it runs the fab-lang leg only (the "optional not
/// required" gate). Phase-L's JIT slots in here and every case starts checking it automatically.
#[must_use]
pub fn drivers() -> Vec<Box<dyn Driver>> {
    let mut v: Vec<Box<dyn Driver>> = vec![Box::new(FabLang)];
    if crate::openscad::find_bin().is_some() {
        v.push(Box::new(OpenScad));
    }
    v
}

/// Run `scad` through every registered driver and check they AGREE (fab-lang is the baseline). `Ok`
/// when all agree — or when only fab-lang is present (no oracle → nothing to differ, a clean skip).
/// PURE: it renders + compares, it never panics (the test wrapper turns an `Err` into a failure).
///
/// # Errors
/// The first `(baseline vs driver)` disagreement, as a human-readable reason.
pub fn diff(scad: &str) -> std::result::Result<(), String> {
    diff_within(scad, 1e-3)
}

/// Like [`diff`], but with a caller-set residual ceiling — the twisted-extrude class leans on this
/// (a relaxed, DOCUMENTED tolerance): our profile-resample + Manifold's helix match OpenSCAD's SHAPE, but
/// differ by a small tessellation-phase artifact that vanishes with resolution (J.3.4.1). The gate stays
/// 1e-3 for everything else.
///
/// # Errors
/// The first `(baseline vs driver)` disagreement whose residual exceeds `max_residual`.
pub fn diff_within(scad: &str, max_residual: f64) -> std::result::Result<(), String> {
    let drivers = drivers();
    let base = drivers[0].eval(scad);
    for d in &drivers[1..] {
        outcomes_agree(&base, &d.eval(scad), max_residual)
            .map_err(|why| format!("{scad:?}: {} vs {}: {why}", drivers[0].name(), d.name()))?;
    }
    Ok(())
}

/// Run `scad` through every registered driver and check the ECHO output agrees line-for-line — the
/// I.5 string-equal-vs-oracle gate: number formatting (6 sig figs, scientific crossover), string
/// quoting/escaping, and named-arg rendering all match the real binary's console, not just my probes.
///
/// # Errors
/// The first driver whose echo lines differ from the baseline (fab-lang).
pub fn diff_echo(scad: &str) -> std::result::Result<(), String> {
    let drivers = drivers();
    let base = drivers[0].echo(scad);
    for d in &drivers[1..] {
        let other = d.echo(scad);
        if base != other {
            return Err(format!(
                "{scad:?}: {} echo {base:?} vs {} echo {other:?}",
                drivers[0].name(),
                d.name()
            ));
        }
    }
    Ok(())
}

/// Run a `.scad` FILE (its `use`/`include` graph, resolved against `library_paths`) through every
/// registered driver and check they AGREE — the file-based sibling of [`diff`], for the loader.
///
/// # Errors
/// The first `(baseline vs driver)` disagreement, as a human-readable reason.
pub fn diff_files(root: &Path, library_paths: &[PathBuf]) -> std::result::Result<(), String> {
    let drivers = drivers();
    let base = drivers[0].eval_file(root, library_paths);
    for d in &drivers[1..] {
        outcomes_agree(&base, &d.eval_file(root, library_paths), 1e-3).map_err(|why| {
            format!(
                "{}: {} vs {}: {why}",
                root.display(),
                drivers[0].name(),
                d.name()
            )
        })?;
    }
    Ok(())
}

/// Two outcomes agree iff both empty, both rejected, or two solids with equal genus + a negligible
/// symmetric difference — the strongest, tessellation-independent tier (same gate as [`compare`]).
fn outcomes_agree(a: &Outcome, b: &Outcome, max_residual: f64) -> std::result::Result<(), String> {
    match (a, b) {
        (Outcome::Empty, Outcome::Empty) | (Outcome::Rejected, Outcome::Rejected) => Ok(()),
        (Outcome::Solid(x), Outcome::Solid(y)) => {
            if x.genus() != y.genus() {
                return Err(format!(
                    "genus {} vs {} (vol {:.1} vs {:.1}, bbox {} vs {})",
                    x.genus(),
                    y.genus(),
                    x.volume(),
                    y.volume(),
                    bbox_str(x),
                    bbox_str(y)
                ));
            }
            let resid = sym_diff_ratio(x, y);
            if resid < max_residual {
                Ok(())
            } else {
                Err(format!(
                    "boolean residual {resid:.2e} exceeds {max_residual:.0e} \
                     (vol {:.1} vs {:.1}, bbox {} vs {})",
                    x.volume(),
                    y.volume(),
                    bbox_str(x),
                    bbox_str(y)
                ))
            }
        }
        (a, b) => Err(format!(
            "shape-class mismatch: {} vs {}",
            a.kind(),
            b.kind()
        )),
    }
}

/// A solid's bounding-box extents `[w×d×h]` — the debugging companion to genus/residual in a divergence
/// message (a size mismatch, e.g. the text 100/72 DPI bug, shows here as a clean ratio instead of a raw
/// volume number). `?` if the solid has no bbox (empty).
fn bbox_str(s: &Solid) -> String {
    s.bbox().map_or_else(
        || "?".to_string(),
        |(lo, hi)| {
            let (l, h) = (lo.to_array(), hi.to_array());
            format!("[{:.2}×{:.2}×{:.2}]", h[0] - l[0], h[1] - l[1], h[2] - l[2])
        },
    )
}

/// `vol((A−B) ∪ (B−A)) / vol(A)` — the symmetric-difference ratio (0 = identical solids).
fn sym_diff_ratio(a: &Solid, b: &Solid) -> f64 {
    let sym = a.difference(b).union(&b.difference(a));
    let ref_vol = a.volume().abs().max(f64::MIN_POSITIVE);
    sym.volume() / ref_vol
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openscad::find_bin;

    #[test]
    fn multiset_is_order_independent_and_eps_tolerant() {
        let a = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0)];
        let b = [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1e-9)]; // reordered + 1e-9 jitter
        assert!(vertex_multiset_matches(&a, &b, 1e-6)); // coarse grid: jitter absorbed
        assert!(!vertex_multiset_matches(&a, &b, 1e-12)); // fine grid: jitter separates
        assert!(!vertex_multiset_matches(
            &a,
            &[Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0)],
            1e-6
        )); // genuinely different set
        assert!(!vertex_multiset_matches(&a, &a[..1], 1e-6)); // length mismatch
    }

    fn skip_if_no_oracle() -> bool {
        if find_bin().is_none() {
            eprintln!("skipping: OpenSCAD not found");
            return true;
        }
        false
    }

    /// The J.2.9 color differential: scad-rs's colors vs the oracle's, across the well-defined cases —
    /// a named/hex color, an outer color() over a boolean (uniform), a disjoint two-color union, and the
    /// uncolored baselines (a bare primitive → no color; an uncolored CSG result → oracle's default gold,
    /// normalized to uncolored). Both engines must agree on the SET of colors present.
    #[test]
    fn color_differential_matches_the_oracle() {
        if skip_if_no_oracle() {
            return;
        }
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let cases: [(&str, Vec<[u8; 4]>); 5] = [
            ("color(\"red\") cube(10);", vec![red]),
            ("cube(10);", vec![]),
            (
                "color(\"red\") difference() { cube(10); translate([5, 5, 5]) cube(8); }",
                vec![red],
            ),
            (
                "difference() { cube(10); translate([5, 5, 5]) cube(8); }",
                vec![],
            ),
            (
                "color(\"red\") cube(5); color(\"blue\") translate([20, 0, 0]) cube(5);",
                vec![blue, red], // sorted: blue < red
            ),
        ];
        for (src, expected) in cases {
            let c = compare_colors(src, Duration::from_secs(60)).unwrap();
            assert!(
                c.matches(),
                "{src}\n  scad:   {:?}\n  oracle: {:?}",
                c.scad_colors,
                c.oracle_colors
            );
            assert_eq!(c.scad_colors, expected, "{src}: scad colors");
        }
    }

    /// The tracer bullet's payoff: run the sphere resolution sweep, print the tier matrix, and gate
    /// on the strongest tier (boolean residual ~0, genus exact).
    #[test]
    fn sphere_resolution_matrix() {
        if skip_if_no_oracle() {
            return;
        }
        eprintln!(
            "\n$fn | scad_v oracle_v | scad_t oracle_t | vtx_eps | vol_err  area_err | genus | bool_resid"
        );
        eprintln!("{}", "-".repeat(92));
        for fn_ in [8, 16, 32, 64, 128, 256] {
            let src = format!("sphere(10, $fn={fn_});");
            let c = compare(&src, Duration::from_secs(60)).unwrap();
            eprintln!(
                "{fn_:>3} | {:>6} {:>8} | {:>6} {:>8} | {:>7} | {:.1e}  {:.1e} | {}/{}   | {:.2e}",
                c.scad_verts,
                c.oracle_verts,
                c.scad_tris,
                c.oracle_tris,
                c.vertex_match_eps
                    .map_or_else(|| "none".to_string(), |e| format!("{e:.0e}")),
                c.volume_rel_err,
                c.area_rel_err,
                c.genus_scad,
                c.genus_oracle,
                c.boolean_residual,
            );
            // The gate: both are closed genus-0 solids and their symmetric difference is negligible.
            assert_eq!(c.genus_scad, 0, "$fn={fn_}: scad-rs not genus 0");
            assert_eq!(c.genus_oracle, 0, "$fn={fn_}: oracle not genus 0");
            assert!(
                c.boolean_residual < 1e-3,
                "$fn={fn_}: boolean residual {:.2e} exceeds 1e-3",
                c.boolean_residual
            );
        }
        eprintln!();
    }
}
