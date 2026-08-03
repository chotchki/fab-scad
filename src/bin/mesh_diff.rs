//! SZ.9.3 — do two meshes describe the same solid?
//!
//!   cargo run --bin mesh_diff -- <a.stl|a.3mf> <b.stl|b.3mf> [--eps 1e-6]
//!   cargo run --bin mesh_diff -- <browser.3mf> --against <model.scad> [--root DIR]
//!
//! Built for ONE question that nothing in this tree could previously answer: does the geometry the
//! BROWSER produces match the geometry the DESKTOP produces? Both run the same evaluator and the
//! same kernel — `handle_with_store` is one function — so the answer ought to be trivially yes, and
//! "ought to" is exactly the kind of claim that had already been wrong twice this phase. What
//! actually differs between the two is the target: wasm gets Rust's own libm for the transcendentals
//! instead of the system one, rayon runs over wasm-bindgen-rayon rather than native threads, and
//! wasm-opt rewrites the module after the compiler is done with it. Any of those could move a
//! vertex; none of them would announce it.
//!
//! # `--against` renders the DESKTOP side here, rather than shelling out to `fab render`
//!
//! Because the request has to match. The browser's save-back issues
//! `RenderWhole { preview: false, quality: Final }` then `SaveMeshes { budget: None }` and uploads
//! the `high` part; `fab render --engine scad-rs` calls `build_geo` directly with the CLI's own
//! config. Those are different renders of the same file, so comparing them would fold a
//! quality-settings difference into a platform comparison and report divergence that means nothing.
//! `--against` issues the browser's EXACT request against `Source::Path` — the desktop's real door —
//! so the only variable left between the two sides is the target.
//!
//! Format-agnostic on purpose. The web save path emits 3MF unconditionally (`save_meshes_svc`)
//! while `fab render` writes STL by default, so a comparator that took one format would force the
//! harness to pick a lane and lose whichever check it did not pick.
//!
//! # What it compares, weakest claim first
//!
//! - **Triangle count** — exact. Different tessellation is a different mesh, full stop.
//! - **Vertex multiset** — snapped to a grid of `eps`, compared as a MULTISET. Order-independent,
//!   because neither format promises one and the exporters do not agree on it.
//! - **Bounding box** — per axis, within `eps`. Catches a uniform shift the multiset would also
//!   catch, but reports it in mm, which is the number a human can act on.
//! - **Volume** — signed, by the divergence theorem, relative-compared. Computed HERE from the
//!   triangles rather than asked of the kernel, so a kernel bug cannot make two meshes agree.
//!
//! # Why eps is not zero
//!
//! It could nearly be. Both sides are IEEE-754 f64 running identical Rust, so the arithmetic itself
//! is bit-reproducible — but `sin`/`cos`/`pow` are not, and a BOSL2 model is made of them. A
//! last-ulp difference in a rotation propagates to roughly 1e-15 relative, which at model scale is
//! ~1e-13 mm. The 1e-6 default is nine orders of magnitude above that and still four orders below
//! any tessellation change worth the name, so it separates "the same solid" from "a different
//! solid" without adjudicating float noise. STL's f32 storage is the real floor: it quantizes to
//! ~1e-7 relative on its own, which is why the default sits above it.

#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a diagnostic CLI: stdout/stderr ARE its interface"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// An indexed mesh, however it arrived.
struct Mesh {
    verts: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
}

impl Mesh {
    /// Read `.stl` or `.3mf` by extension. A 3MF holding several build objects is CONCATENATED into
    /// one mesh: the save path emits one object, and a comparator that silently dropped the rest
    /// would report agreement on a fraction of the model.
    fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("3mf") => {
                let objects = fab_scad::threemf_in::parse_3mf(&bytes)
                    .with_context(|| format!("parsing {} as 3MF", path.display()))?;
                let (mut verts, mut tris) = (Vec::new(), Vec::new());
                for o in objects {
                    let base =
                        u32::try_from(verts.len()).context("3MF vertex count overflows u32")?;
                    verts.extend(o.verts);
                    tris.extend(
                        o.tris
                            .iter()
                            .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
                    );
                }
                Ok(Self { verts, tris })
            }
            _ => {
                // STL is a triangle SOUP — no shared vertices — so every triangle contributes three
                // vertices. That is fine for both comparisons here: a multiset of soup vertices and
                // a multiset of indexed ones agree iff the surfaces do, provided BOTH sides are read
                // the same way, which is why the indexed path below is expanded to soup too.
                let mesh = fab_scad::stl::load_stl_bytes(&bytes)
                    .with_context(|| format!("parsing {} as STL", path.display()))?;
                let verts: Vec<[f64; 3]> = mesh
                    .positions
                    .iter()
                    .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
                    .collect();
                let tris = (0..verts.len() / 3)
                    .map(|i| {
                        let b = u32::try_from(i * 3).unwrap_or(u32::MAX);
                        [b, b + 1, b + 2]
                    })
                    .collect();
                Ok(Self { verts, tris })
            }
        }
    }

    /// Every triangle's three corners, in triangle order — the SOUP form. Both sides reduce to this
    /// before comparing, so an indexed mesh and a soup mesh of the same surface compare equal
    /// instead of differing by however many vertices the exporter chose to share.
    fn corners(&self) -> Vec<[f64; 3]> {
        self.tris
            .iter()
            .flat_map(|t| {
                t.iter()
                    .map(|&i| self.verts[i as usize])
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Signed volume by the divergence theorem: ⅙ Σ (a × b) · c over the triangles. Independent of
    /// the kernel — this is the check that still means something if the kernel is what is wrong.
    fn volume(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let (a, b, c) = (
                    self.verts[t[0] as usize],
                    self.verts[t[1] as usize],
                    self.verts[t[2] as usize],
                );
                let cross = [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ];
                cross[0].mul_add(c[0], cross[1].mul_add(c[1], cross[2] * c[2]))
            })
            .sum::<f64>()
            / 6.0
    }

    /// `[min, max]` per axis. `None` for an empty mesh.
    fn bbox(&self) -> Option<[[f64; 3]; 2]> {
        let first = *self.verts.first()?;
        let mut bb = [first, first];
        for v in &self.verts {
            for a in 0..3 {
                bb[0][a] = bb[0][a].min(v[a]);
                bb[1][a] = bb[1][a].max(v[a]);
            }
        }
        Some(bb)
    }
}

/// Snap to an integer grid of `eps` so near-equal floats collapse to one key.
///
/// Caveat, inherited from `differ::vertex_multiset_matches` and true here for the same reason: a
/// vertex sitting exactly on a cell boundary can round either way, so two near-equal points
/// straddling an edge quantize apart. Harmless for well-separated tessellation vertices at a sane
/// eps — and a false ALARM rather than a false pass, which is the right way for it to be wrong.
fn key(v: [f64; 3], eps: f64) -> [i64; 3] {
    [
        (v[0] / eps).round() as i64,
        (v[1] / eps).round() as i64,
        (v[2] / eps).round() as i64,
    ]
}

/// Relative error `|a−b| / max(|a|,|b|)`, and 0 when both are 0.
fn rel(a: f64, b: f64) -> f64 {
    let scale = a.abs().max(b.abs());
    if scale == 0.0 {
        0.0
    } else {
        (a - b).abs() / scale
    }
}

/// Render `model` the way the BROWSER'S SAVE does, one platform over: `RenderWhole` at
/// `Quality::Final` with `preview: false`, then `SaveMeshes { budget: None }`, taking the `high`
/// part. Every knob matches `gui/src/jobs.rs`'s save job; the source door is `Source::Path` because
/// that IS the desktop's door, and the two doors agreeing is part of what this checks.
fn render_native(model: &Path, root: Option<&Path>) -> Result<Vec<u8>> {
    use fab_scad::geomsg::{Quality, Request, Response, Source};
    use fab_scad::geomsvc::{SolidStore, handle_with_store};

    let mut store = SolidStore::new(0);
    let id = match handle_with_store(
        &mut store,
        Request::RenderWhole {
            source: Source::Path(model.to_string_lossy().into_owned()),
            root: root.map(|r| r.to_string_lossy().into_owned()),
            preview: false,
            quality: Quality::Final,
        },
    ) {
        Response::Rendered { id, .. } => id,
        Response::Failed { error, line } => {
            let at = line.map(|l| format!("line {l}: ")).unwrap_or_default();
            bail!("native render of {} failed: {at}{error}", model.display())
        }
        _ => bail!("native render: unexpected service response"),
    };
    match handle_with_store(
        &mut store,
        Request::SaveMeshes {
            base: id,
            budget: None,
        },
    ) {
        Response::SavedMeshes { high, .. } => Ok(high),
        Response::Failed { error, .. } => bail!("native mesh export failed: {error}"),
        _ => bail!("save-meshes: unexpected service response"),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut eps = 1e-6_f64;
    let mut against: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    // A free function, not a closure: a closure over `args`/`i` would still be borrowing `i` when
    // the arms below advance it.
    fn need(args: &[String], i: usize, flag: &str) -> Result<String> {
        args.get(i + 1)
            .cloned()
            .with_context(|| format!("{flag} needs a value"))
    }
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--eps" => {
                eps = need(&args, i, "--eps")?
                    .parse()
                    .context("--eps must be a number")?;
                i += 2;
            }
            "--against" => {
                against = Some(PathBuf::from(need(&args, i, "--against")?));
                i += 2;
            }
            "--root" => {
                root = Some(PathBuf::from(need(&args, i, "--root")?));
                i += 2;
            }
            other => {
                paths.push(PathBuf::from(other));
                i += 1;
            }
        }
    }
    if eps <= 0.0 {
        bail!("--eps must be positive, got {eps}");
    }

    // Side B is either a second file or a native render made on the spot. `--against` writes its
    // 3MF beside side A rather than to a temp path, so a failure leaves the desktop's answer on disk
    // for a human to open — a mismatch is the moment you most want both meshes.
    let (a_path, b_path) = match (paths.as_slice(), &against) {
        ([a], Some(model)) => {
            let out = a.with_file_name(format!(
                "{}-native.3mf",
                a.file_stem().unwrap_or_default().to_string_lossy()
            ));
            let bytes = render_native(model, root.as_deref())?;
            std::fs::write(&out, &bytes)
                .with_context(|| format!("writing the native render to {}", out.display()))?;
            println!("rendered {} natively -> {}", model.display(), out.display());
            (a.clone(), out)
        }
        ([a, b], None) => (a.clone(), b.clone()),
        _ => bail!(
            "usage: mesh_diff <a> <b> [--eps E]  |  mesh_diff <a> --against <model.scad> [--root DIR] [--eps E]"
        ),
    };

    let a = Mesh::load(&a_path)?;
    let b = Mesh::load(&b_path)?;
    // Indices are checked ONCE, here, so `corners`/`volume` can index freely below. A malformed file
    // must report as a malformed file, not as a panic in a comparison.
    for (m, p) in [(&a, &a_path), (&b, &b_path)] {
        if let Some(bad) = m
            .tris
            .iter()
            .flatten()
            .find(|&&i| i as usize >= m.verts.len())
        {
            bail!(
                "{} has a triangle referencing vertex {bad}, past its {} vertices",
                p.display(),
                m.verts.len()
            );
        }
    }
    println!("A  {}  {} tris", a_path.display(), a.tris.len());
    println!("B  {}  {} tris", b_path.display(), b.tris.len());

    // An EMPTY mesh on either side is a failure, not a match. Two empty meshes agree on every
    // comparison below, so without this the gate passes loudest exactly when the render died.
    if a.tris.is_empty() || b.tris.is_empty() {
        bail!(
            "a mesh is EMPTY (A {} tris, B {} tris) — a render produced no geometry",
            a.tris.len(),
            b.tris.len()
        );
    }

    let mut problems: Vec<String> = Vec::new();

    if a.tris.len() != b.tris.len() {
        problems.push(format!(
            "triangle count differs: {} vs {} — the two sides tessellated differently, which is a \
             different mesh however close the surfaces sit",
            a.tris.len(),
            b.tris.len()
        ));
    }

    // The vertex multiset, at eps. Counted rather than set-compared so a duplicated vertex on one
    // side is a difference: a multiset is what a mesh actually is.
    let mut counts: BTreeMap<[i64; 3], i64> = BTreeMap::new();
    for v in a.corners() {
        *counts.entry(key(v, eps)).or_default() += 1;
    }
    for v in b.corners() {
        *counts.entry(key(v, eps)).or_default() -= 1;
    }
    let unmatched: i64 = counts.values().map(|n| n.abs()).sum();
    if unmatched > 0 {
        let sample: Vec<String> = counts
            .iter()
            .filter(|(_, n)| **n != 0)
            .take(4)
            .map(|(k, n)| {
                let side = if *n > 0 { "A only" } else { "B only" };
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "grid keys back to mm for a human-readable report"
                )]
                let mm = [k[0] as f64 * eps, k[1] as f64 * eps, k[2] as f64 * eps];
                format!(
                    "[{:.6}, {:.6}, {:.6}] ×{} {side}",
                    mm[0],
                    mm[1],
                    mm[2],
                    n.abs()
                )
            })
            .collect();
        problems.push(format!(
            "{unmatched} vertex slot(s) do not match at eps={eps:e}; first few: {}",
            sample.join(", ")
        ));
    }

    match (a.bbox(), b.bbox()) {
        (Some(ba), Some(bb)) => {
            for (axis, name) in ["x", "y", "z"].iter().enumerate() {
                for (end, label) in [(0, "min"), (1, "max")] {
                    let (va, vb) = (ba[end][axis], bb[end][axis]);
                    if (va - vb).abs() > eps {
                        problems.push(format!(
                            "bbox {name}.{label} differs by {:.9} mm ({va} vs {vb})",
                            (va - vb).abs()
                        ));
                    }
                }
            }
        }
        _ => problems.push("a mesh has no vertices at all".into()),
    }

    let (va, vb) = (a.volume(), b.volume());
    // Volume is an integral over every triangle, so it accumulates error across the whole mesh
    // rather than at one vertex — it gets a RELATIVE tolerance, and a looser one than eps, or a
    // large model fails on rounding that moved nothing.
    let vrel = rel(va, vb);
    if vrel > 1e-9 {
        problems.push(format!(
            "volume differs by {vrel:e} relative ({va:.9} vs {vb:.9} mm³)"
        ));
    }
    println!("volume  {va:.6} vs {vb:.6} mm³  (rel {vrel:e})");

    if problems.is_empty() {
        println!("MATCH — the two meshes describe the same solid (eps {eps:e})");
        return Ok(());
    }
    for p in &problems {
        eprintln!("::error::mesh_diff: {p}");
    }
    bail!(
        "{} difference(s) between {} and {}",
        problems.len(),
        a_path.display(),
        b_path.display()
    )
}
