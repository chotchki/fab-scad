//! The OpenSCAD oracle runner (G.3.6) — drives the CLI, captures the exported mesh + echo for the
//! differential harness (scad-rs vs OpenSCAD). `native`-gated (spawns a process).
//!
//! Mesh capture is via OFF export (shared vertices + polygon faces, unlike STL's f32 triangle-soup),
//! through the Manifold backend so it matches scad-rs's Manifold kernel path.
//!
//! **Determinism (spec Q7):** OpenSCAD's export is byte-identical run-to-run WITHOUT any flag — the
//! `--render`/export path is deterministic by default. There is no "sort the output" flag: vertex
//! and face order is GENERATION order (ring-major for a sphere, matching scad-rs), not canonicalized.
//! So the harness compares vertices as a MULTISET (order-independent), not positionally.
//!
//! **Precision floor:** the export quantizes (OFF ~6 significant digits, STL f32). Exact-f64
//! comparison is therefore impossible through a file — the metric tiers are tolerance-based (G.3.7).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use fab_lang::{Rgba, Tri, Vec3};

use crate::openscad::Openscad;

/// A mesh from the oracle: shared vertices + polygon faces (OFF preserves OpenSCAD's face structure).
#[derive(Debug, Clone)]
pub struct OracleMesh {
    pub verts: Vec<Vec3>,
    pub faces: Vec<Vec<u32>>,
    /// Per-face color (parallel to `faces`) — `Some` when the OFF face line carried a trailing
    /// `r g b [a]`, `None` for an uncolored face. OpenSCAD emits color per-face on CSG results (J.2.9).
    pub face_colors: Vec<Option<Rgba>>,
}

/// Quantize a color to 0-255 RGBA — the tessellation/representation-independent key the color
/// differential compares (our per-vertex f64 vs the oracle's per-face bytes both reduce to this).
#[must_use]
pub fn quantize_color(c: Rgba) -> [u8; 4] {
    let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c.r), q(c.g), q(c.b), q(c.a)]
}

impl OracleMesh {
    /// Vertex count.
    #[must_use]
    pub fn vert_count(&self) -> usize {
        self.verts.len()
    }

    /// The distinct per-face colors present, quantized + sorted + deduped — RAW (includes OpenSCAD's
    /// colorscheme DEFAULT color, which it leaks onto uncolored CSG results). The differential subtracts
    /// that default (probed dynamically, since it's `249 215 44` on one install, `157 203 81` on another
    /// — a preference, not semantic).
    #[must_use]
    pub fn distinct_colors(&self) -> Vec<[u8; 4]> {
        let set: std::collections::BTreeSet<[u8; 4]> = self
            .face_colors
            .iter()
            .flatten()
            .map(|&c| quantize_color(c))
            .collect();
        set.into_iter().collect()
    }

    /// Fan-triangulate the polygon faces into a triangle list (for Manifold / `from_indexed`).
    #[must_use]
    pub fn tris(&self) -> Vec<Tri> {
        let mut out = Vec::new();
        for face in &self.faces {
            for i in 1..face.len().saturating_sub(1) {
                out.push(Tri::new(face[0], face[i], face[i + 1]));
            }
        }
        out
    }
}

/// One oracle run: the exported mesh, the `ECHO:` console lines, warnings, and the tool version.
#[derive(Debug)]
pub struct OracleRun {
    pub mesh: OracleMesh,
    pub echo: Vec<String>,
    pub warnings: Vec<String>,
    pub version: Option<String>,
    /// The oracle process's wall time (AJ.8 timing comparison — subtract an empty-program
    /// baseline to remove startup cost).
    pub duration: Duration,
}

/// Per-process temp-file discriminator, so parallel test threads (same pid) don't clobber each other.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Run OpenSCAD on `source`, exporting an OFF mesh and capturing echo. `timeout` bounds the render.
///
/// # Errors
/// Fails if OpenSCAD can't be located, the render times out / errors, or the OFF can't be parsed.
pub fn run(source: &str, timeout: Duration) -> Result<OracleRun> {
    run_with_flags(source, timeout, &[])
}

/// [`run`] with extra `--enable=…` oracle flags (AJ.8's gen-diff runs the oracle with the
/// experimental features our evaluator ships always-on).
pub fn run_with_flags(source: &str, timeout: Duration, flags: &[&str]) -> Result<OracleRun> {
    let osc = Openscad::discover(None)?;
    let version = osc.tool_version();

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir();
    let stem = format!("fab-oracle-{}-{seq}", std::process::id());
    let scad = dir.join(format!("{stem}.scad"));
    let off = dir.join(format!("{stem}.off"));

    std::fs::write(&scad, source).with_context(|| format!("writing {}", scad.display()))?;
    let report = osc.render_with_flags(&scad, &off, timeout, flags);
    let _ = std::fs::remove_file(&scad);
    let report = report?;
    ensure!(
        report.ok,
        "OpenSCAD render failed (timed_out={}): {:?}",
        report.timed_out,
        report.warnings
    );

    let off_text =
        std::fs::read_to_string(&off).with_context(|| format!("reading {}", off.display()))?;
    let _ = std::fs::remove_file(&off);
    let mesh = parse_off(&off_text).context("parsing oracle OFF export")?;

    Ok(OracleRun {
        mesh,
        echo: report.echo,
        warnings: report.warnings,
        version,
        duration: report.duration,
    })
}

/// Parse a plain OFF file: `OFF`, then `nverts nfaces nedges`, then vertices (`x y z`), then faces
/// (`n i0 … i(n-1)` + OPTIONAL trailing per-face color). LINE-BASED (see the body): counts may sit on
/// the `OFF` line or the next; per-face color is CAPTURED into `face_colors` (J.2.9).
fn parse_off(text: &str) -> Result<OracleMesh> {
    // LINE-BASED, because a face line may carry trailing per-face COLOR (`n i0 i1 i2 r g b`) — OpenSCAD
    // colors a CSG result (a boolean/multi-object export; plain primitives don't) with the applied
    // color or, when uncolored, its colorscheme default. A whole-file tokenizer would read that color as
    // the NEXT face's arity and derail, so we read each face's arity + indices from its own line and take
    // the rest as the color. Blank / `#`-comment lines skipped.
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));

    let header: Vec<&str> = lines
        .next()
        .context("OFF: empty file")?
        .split_whitespace()
        .collect();
    ensure!(
        header.first() == Some(&"OFF"),
        "not an OFF file (missing OFF magic)"
    );
    // Counts sit on the `OFF` line (OpenSCAD's style) or the next line.
    let counts: Vec<&str> = if header.len() >= 4 {
        header[1..].to_vec()
    } else {
        lines
            .next()
            .context("OFF: missing counts line")?
            .split_whitespace()
            .collect()
    };
    let mut cs = counts.into_iter();
    let nverts = next_usize(&mut cs, "vertex count")?;
    let nfaces = next_usize(&mut cs, "face count")?;

    let mut verts = Vec::with_capacity(nverts);
    for _ in 0..nverts {
        let mut t = lines
            .next()
            .context("OFF: missing vertex line")?
            .split_whitespace();
        verts.push(Vec3::new(
            next_f64(&mut t, "vertex x")?,
            next_f64(&mut t, "vertex y")?,
            next_f64(&mut t, "vertex z")?,
        ));
    }
    let mut faces = Vec::with_capacity(nfaces);
    let mut face_colors = Vec::with_capacity(nfaces);
    for _ in 0..nfaces {
        let mut t = lines
            .next()
            .context("OFF: missing face line")?
            .split_whitespace();
        let arity = next_usize(&mut t, "face arity")?;
        let mut face = Vec::with_capacity(arity);
        for _ in 0..arity {
            face.push(next_u32(&mut t, "face index")?);
        }
        // Whatever trails the indices is the per-face color: `r g b` or `r g b a`, ints 0-255 (J.2.9).
        face_colors.push(parse_face_color(&t.collect::<Vec<_>>()));
        faces.push(face);
    }
    Ok(OracleMesh {
        verts,
        faces,
        face_colors,
    })
}

/// The trailing tokens of an OFF face line → its color: `[r, g, b]` or `[r, g, b, a]` (0-255) → an
/// [`Rgba`]; anything else (no trailing tokens, or an unparseable count) → `None` (uncolored).
fn parse_face_color(rest: &[&str]) -> Option<Rgba> {
    let byte = |s: &str| s.parse::<u8>().ok().map(f64::from);
    match rest {
        [r, g, b] => Some(Rgba::opaque(
            byte(r)? / 255.0,
            byte(g)? / 255.0,
            byte(b)? / 255.0,
        )),
        [r, g, b, a] => Some(Rgba::new(
            byte(r)? / 255.0,
            byte(g)? / 255.0,
            byte(b)? / 255.0,
            byte(a)? / 255.0,
        )),
        _ => None,
    }
}

fn next_usize<'a>(tok: &mut impl Iterator<Item = &'a str>, what: &str) -> Result<usize> {
    tok.next()
        .with_context(|| format!("OFF: missing {what}"))?
        .parse()
        .with_context(|| format!("OFF: bad {what}"))
}
fn next_u32<'a>(tok: &mut impl Iterator<Item = &'a str>, what: &str) -> Result<u32> {
    tok.next()
        .with_context(|| format!("OFF: missing {what}"))?
        .parse()
        .with_context(|| format!("OFF: bad {what}"))
}
fn next_f64<'a>(tok: &mut impl Iterator<Item = &'a str>, what: &str) -> Result<f64> {
    tok.next()
        .with_context(|| format!("OFF: missing {what}"))?
        .parse()
        .with_context(|| format!("OFF: bad {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openscad::find_bin;

    /// OFF parse is pure — testable without OpenSCAD installed.
    #[test]
    fn parses_off_verts_and_triangulates_faces() {
        // A unit tetrahedron: 4 verts, one triangle face + one quad face → 1 + 2 = 3 tris.
        let off = "OFF 4 2 0\n0 0 0\n1 0 0\n0 1 0\n0 0 1\n3 0 1 2\n4 0 1 3 2\n";
        let m = parse_off(off).unwrap();
        assert_eq!(m.vert_count(), 4);
        assert_eq!(m.verts[1].to_array(), [1.0, 0.0, 0.0]);
        assert_eq!(m.tris().len(), 3); // triangle → 1, quad → 2
    }

    #[test]
    fn rejects_non_off() {
        assert!(parse_off("solid foo\n").is_err());
    }

    #[test]
    fn parse_off_captures_per_face_color_off_the_indices() {
        // A face line's trailing color must NOT derail the arity/indices (J.2.7.1)...
        let off = "OFF 3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2 255 0 0\n";
        let m = parse_off(off).unwrap();
        assert_eq!(m.vert_count(), 3);
        assert_eq!(m.tris(), vec![Tri::new(0, 1, 2)]); // indices clean
        // ...and it's now CAPTURED as the face's color (J.2.9).
        assert_eq!(m.face_colors, vec![Some(Rgba::opaque(1.0, 0.0, 0.0))]);
        assert_eq!(m.distinct_colors(), vec![[255, 0, 0, 255]]);
    }

    #[test]
    fn distinct_colors_are_raw_and_uncolored_is_none() {
        // distinct_colors reports colors RAW — the default-color normalization is the differ's job.
        let m = parse_off("OFF 3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2 249 215 44\n").unwrap();
        assert_eq!(m.face_colors, vec![Some(Rgba::from_u8(249, 215, 44))]);
        assert_eq!(m.distinct_colors(), vec![[249, 215, 44, 255]]);
        // A plain (colorless) face → None, and an alpha'd color keeps its 4th channel.
        let plain = parse_off("OFF 3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n").unwrap();
        assert_eq!(plain.face_colors, vec![None]);
        assert!(plain.distinct_colors().is_empty());
        let rgba = parse_off("OFF 3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2 0 0 255 127\n").unwrap();
        assert_eq!(rgba.distinct_colors(), vec![[0, 0, 255, 127]]);
    }

    #[test]
    fn parse_off_accepts_counts_on_the_next_line() {
        let off = "OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
        assert_eq!(parse_off(off).unwrap().tris(), vec![Tri::new(0, 1, 2)]);
    }

    // The live-oracle tests skip when OpenSCAD isn't installed, so CI without it stays green.
    fn skip_if_no_oracle() -> bool {
        if find_bin().is_none() {
            eprintln!("skipping: OpenSCAD not found");
            return true;
        }
        false
    }

    #[test]
    fn sphere_fn8_is_32_verts() {
        if skip_if_no_oracle() {
            return;
        }
        let run = run("sphere(1, $fn=8);", Duration::from_secs(30)).unwrap();
        assert_eq!(run.mesh.vert_count(), 32, "sphere($fn=8) → 4 rings × 8");
        assert!(!run.mesh.tris().is_empty());
    }

    #[test]
    fn captures_echo() {
        if skip_if_no_oracle() {
            return;
        }
        let run = run("echo(\"hi\", n = 7); cube(1);", Duration::from_secs(30)).unwrap();
        assert!(
            run.echo.iter().any(|l| l.contains("hi") && l.contains('7')),
            "echo lines: {:?}",
            run.echo
        );
    }

    #[test]
    fn export_is_deterministic() {
        if skip_if_no_oracle() {
            return;
        }
        let a = run("sphere(3, $fn=16);", Duration::from_secs(30)).unwrap();
        let b = run("sphere(3, $fn=16);", Duration::from_secs(30)).unwrap();
        assert_eq!(a.mesh.verts, b.mesh.verts, "run-to-run vertex determinism");
    }
}
