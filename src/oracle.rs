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

use crate::openscad::Openscad;

/// A mesh from the oracle: shared vertices + polygon faces (OFF preserves OpenSCAD's face structure).
#[derive(Debug, Clone)]
pub struct OracleMesh {
    pub verts: Vec<[f64; 3]>,
    pub faces: Vec<Vec<u32>>,
}

impl OracleMesh {
    /// Vertex count.
    #[must_use]
    pub fn vert_count(&self) -> usize {
        self.verts.len()
    }

    /// Fan-triangulate the polygon faces into a triangle list (for Manifold / `from_indexed`).
    #[must_use]
    pub fn tris(&self) -> Vec<[u32; 3]> {
        let mut out = Vec::new();
        for face in &self.faces {
            for i in 1..face.len().saturating_sub(1) {
                out.push([face[0], face[i], face[i + 1]]);
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
}

/// Per-process temp-file discriminator, so parallel test threads (same pid) don't clobber each other.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Run OpenSCAD on `source`, exporting an OFF mesh and capturing echo. `timeout` bounds the render.
///
/// # Errors
/// Fails if OpenSCAD can't be located, the render times out / errors, or the OFF can't be parsed.
pub fn run(source: &str, timeout: Duration) -> Result<OracleRun> {
    let osc = Openscad::discover(None)?;
    let version = osc.tool_version();

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir();
    let stem = format!("fab-oracle-{}-{seq}", std::process::id());
    let scad = dir.join(format!("{stem}.scad"));
    let off = dir.join(format!("{stem}.off"));

    std::fs::write(&scad, source).with_context(|| format!("writing {}", scad.display()))?;
    let report = osc.render(&scad, &off, timeout);
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
    })
}

/// Parse a plain OFF file: `OFF`, then `nverts nfaces nedges`, then vertices (`x y z`), then faces
/// (`n i0 … i(n-1)` + OPTIONAL trailing per-face color). LINE-BASED (see the body): counts may sit on
/// the `OFF` line or the next; per-face color is ignored.
fn parse_off(text: &str) -> Result<OracleMesh> {
    // LINE-BASED, because a face line may carry trailing per-face COLOR (`n i0 i1 i2 r g b`) — OpenSCAD
    // colors a CSG result (a boolean/multi-object export gets `249 215 44`, plain primitives don't). A
    // whole-file tokenizer would read that color as the NEXT face's arity and derail. So we read each
    // face's arity + indices from its own line and ignore the rest. Blank / `#`-comment lines skipped.
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
        verts.push([
            next_f64(&mut t, "vertex x")?,
            next_f64(&mut t, "vertex y")?,
            next_f64(&mut t, "vertex z")?,
        ]);
    }
    let mut faces = Vec::with_capacity(nfaces);
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
        faces.push(face); // trailing per-face color (if any) is left unread
    }
    Ok(OracleMesh { verts, faces })
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
        assert_eq!(m.verts[1], [1.0, 0.0, 0.0]);
        assert_eq!(m.tris().len(), 3); // triangle → 1, quad → 2
    }

    #[test]
    fn rejects_non_off() {
        assert!(parse_off("solid foo\n").is_err());
    }

    #[test]
    fn parse_off_ignores_per_face_color() {
        // A boolean-result OFF carries trailing per-face RGB (OpenSCAD colors CSG output); the parser
        // must read only the arity + indices, or the color derails the next face (J.2.7.1).
        let off = "OFF 3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2 249 215 44\n";
        let m = parse_off(off).unwrap();
        assert_eq!(m.vert_count(), 3);
        assert_eq!(m.tris(), vec![[0u32, 1, 2]]); // color dropped
    }

    #[test]
    fn parse_off_accepts_counts_on_the_next_line() {
        let off = "OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
        assert_eq!(parse_off(off).unwrap().tris(), vec![[0u32, 1, 2]]);
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
