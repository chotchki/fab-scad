//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Renders/slices at PREVIEW quality: it wraps
//! the source in `$preview = true; include <source>;` so models that gate detail on
//! `$fn = $preview ? low : high` render fast (nail_cure: 2.4s vs 43s at full $fn). Final,
//! full-quality output is `fab`'s job; the GUI just needs a quick, responsive preview.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};

use fab_scad::bambu::{self, PieceToPlace};
use fab_scad::manifest::{Connector, Cut, PieceOrient, Slicing};
use fab_scad::num::Num;
use fab_scad::openscad::Openscad;
use fab_scad::slicing;

use crate::stl::{self, StlMesh};

const TIMEOUT: Duration = Duration::from_secs(120);

/// A `[slicing]` spec carrying only the cuts — the shared base for per-piece rendering and the
/// orientation sweep (connectors/orientation are layered on by the specific caller).
fn cuts_to_spec(cuts: &[(char, f64)]) -> Slicing {
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut { axis: axis.to_string(), at: Num::Float(at) })
        .collect();
    Slicing { printer: None, cut, connector: vec![], orient: vec![] }
}

/// GUI placements → manifest connectors, per kind: an onion carries its auto-sized diameter; a bolt
/// carries its screw size (and lets the slicer default `through`). The slicer consumes both.
fn to_connectors(connectors: &[Conn]) -> Vec<Connector> {
    connectors
        .iter()
        .map(|c| {
            let pos = [Num::Float(c.pos[0]), Num::Float(c.pos[1])];
            match c.kind {
                ConnKind::Onion => Connector {
                    cut: c.cut,
                    kind: "onion".to_string(),
                    screw: None,
                    pos,
                    through: None,
                    size: Some(c.size),
                },
                ConnKind::Bolt => Connector {
                    cut: c.cut,
                    kind: "bolt".to_string(),
                    screw: Some(c.screw.to_string()),
                    pos,
                    through: None, // slicer defaults through-depth (12mm) until we expose it
                    size: None,
                },
            }
        })
        .collect()
}

/// GUI per-piece orientations → manifest `[slicing.orient]` entries.
fn to_orient(orient: &[Orient3]) -> Vec<PieceOrient> {
    orient
        .iter()
        .map(|o| PieceOrient {
            piece: o.piece,
            up: [Num::Float(o.up[0]), Num::Float(o.up[1]), Num::Float(o.up[2])],
        })
        .collect()
}

/// Walk up to the fab-scad root (the dir with `printers.toml` + `scad-lib/`) for OPENSCADPATH.
pub fn find_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("printers.toml").exists() && dir.join("scad-lib").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Render the source whole at PREVIEW quality, returning the STL.
pub fn render_whole(root: Option<&Path>, source: &Path, out_dir: &Path) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let wrap = preview_wrapper(source, out_dir)?;
    let out = out_dir.join(format!("{}.stl", stem_of(source)));
    let r = oscad.render(&wrap, &out, TIMEOUT)?;
    ensure!(r.ok, "render of {} failed", source.display());
    Ok(out)
}

/// The preview STL `render_whole` writes for `source` (reused by the cross-section, no re-render).
pub fn whole_stl(source: &Path, out_dir: &Path) -> PathBuf {
    out_dir.join(format!("{}.stl", stem_of(source)))
}

/// One piece, rendered + auto-oriented for the print-orientation preview: its slab multi-index, mesh
/// (WITH its joints carved — peg/socket), and the least-support build-up (`auto_orient::best_up`).
/// Empty slabs are dropped upstream.
pub struct PiecePrint {
    pub piece: [usize; 3],
    pub mesh: StlMesh,
    pub up: [f32; 3],
}

/// Print-orientation layout IN-PROCESS via the Manifold kernel (Track C 11.12) — the kernel twin of
/// `print_layout`. OpenSCAD renders the base mesh ONCE (the front-door); both passes then run in
/// Manifold off the cached base: a BARE slice picks each piece's least-support build-up
/// (`auto_orient::best_up`), then a CARVED slice gated by those orientations makes the preview's
/// onion joints match what the real slice produces. No per-piece spawn — the `slice_solid` twin of
/// `piece_driver` does both.
///
/// Thread-safety: every `Solid` is built AND consumed here; only the piece MESHES (`StlMesh`, Send)
/// leave. A `Solid` is `!Send` and never crosses the task boundary — the compiler enforces it. See
/// `docs/manifold-thread-safety.md`.
pub fn print_layout_kernel(
    root: Option<&Path>,
    source: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    out_dir: &Path,
) -> Result<Vec<PiecePrint>> {
    use fab_scad::kernel::Solid;

    // Cache the base: render the whole model once (front-door), reuse across both passes.
    let base_stl = whole_stl(source, out_dir);
    if !base_stl.exists() {
        render_whole(root, source, out_dir)?;
    }
    let base = Solid::from_stl_file(&base_stl)?;

    // Pass 1: BARE slice (no connectors) → least-support orientation per non-empty piece. (Axis-
    // aligned cuts only today, so the cut-face normals are already in `best_up`'s base set — none.)
    let mut ups: Vec<([usize; 3], [f64; 3])> = Vec::new();
    for (piece, solid) in slicing::slice_solid(&cuts_to_spec(cuts), &base)? {
        let mesh = stl::load_stl_bytes(&solid.to_stl_bytes())?;
        if mesh.positions.is_empty() {
            continue; // an empty slab (L-shaped gap) — nothing to print
        }
        ups.push((piece, fab_scad::auto_orient::best_up(&to_tris(&mesh), &[])));
    }

    // Pass 2: carve each piece with the onions, gated by the orientations just picked.
    let mut spec = cuts_to_spec(cuts);
    spec.connector = to_connectors(connectors);
    spec.orient = ups
        .iter()
        .map(|&(piece, up)| PieceOrient {
            piece,
            up: [Num::Float(up[0]), Num::Float(up[1]), Num::Float(up[2])],
        })
        .collect();

    let mut out = Vec::new();
    for (piece, solid) in slicing::slice_solid(&spec, &base)? {
        let mesh = stl::load_stl_bytes(&solid.to_stl_bytes())?;
        if mesh.positions.is_empty() {
            continue;
        }
        // The build-up this piece was oriented to in pass 1 (default +Z if a connector diff dropped
        // a bare piece that reappears carved — shouldn't happen with axis-aligned cuts).
        let up = ups.iter().find(|(p, _)| *p == piece).map(|(_, u)| *u).unwrap_or([0.0, 0.0, 1.0]);
        out.push(PiecePrint { piece, mesh, up: [up[0] as f32, up[1] as f32, up[2] as f32] });
    }
    Ok(out)
}

/// `StlMesh` positions (flat, 3 verts per tri) → `[[f64; 3]; 3]` triangles for the orientation math.
fn to_tris(m: &StlMesh) -> Vec<[[f64; 3]; 3]> {
    m.positions
        .chunks_exact(3)
        .map(|t| std::array::from_fn(|i| [t[i][0] as f64, t[i][1] as f64, t[i][2] as f64]))
        .collect()
}

/// The cut's 2D cross-section profile (loops in connector-pos coords), from the already-rendered
/// preview STL — for the per-cut connector editor. IN-PROCESS via the kernel (no OpenSCAD spawn);
/// the Solid lives + dies here (it's !Send).
pub fn cross_section(stl: &Path, axis: usize, at: f64) -> Result<Vec<Vec<[f64; 2]>>> {
    use fab_scad::kernel::Solid;
    Ok(Solid::from_stl_file(stl)?.cross_section(axis, at))
}

/// Auto-plan cuts + onion connectors for a too-big model — loads the base solid from the rendered
/// STL and runs the in-process planner ([`fab_scad::auto::plan`], no per-cut OpenSCAD spawn). The
/// Solid lives + dies here (it's !Send); only the plain-data plan crosses back.
pub fn auto_plan(
    base_stl: &Path,
    min: [f64; 3],
    max: [f64; 3],
    bed: [f64; 3],
) -> Result<fab_scad::auto::AutoPlan> {
    use fab_scad::kernel::Solid;
    let base = Solid::from_stl_file(base_stl)?;
    fab_scad::auto::plan(&base, min, max, bed)
}

/// Slice the source at the given cuts — each `(axis, at)` with axis in `'x' | 'y' | 'z'` (preview
/// quality), returning the sliced STL. A pure function of (source, cuts) — the DAG-cache unit.
pub fn reslice(
    root: Option<&Path>,
    source: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    let oscad = Openscad::discover(root)?;
    let wrap = preview_wrapper(source, out_dir)?;
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut {
            axis: axis.to_string(),
            at: Num::Float(at),
        })
        .collect();
    // Per-piece print orientations (auto-picked, seeded by the print-orientation preview). They
    // GATE the onions — a piece oriented off its cut axis downgrades that joint to a bolt. Empty =
    // every piece defaults to +Z (`slicing::piece_up`), which is the pre-orientation behaviour.
    let spec = Slicing {
        printer: None,
        cut,
        connector: to_connectors(connectors),
        orient: to_orient(orient),
    };
    slicing::slice_part(&oscad, &wrap, &spec, spread, out_dir, TIMEOUT)
}

/// Reslice IN-PROCESS via the Manifold kernel (Track C 11.10) — the reactive hot path. OpenSCAD
/// renders the base mesh only on a CACHE MISS (`whole_stl` absent); every cut/connector/orientation
/// edit after that is a pure in-process slice off the cached base, no spawn. Same signature +
/// merged-STL output as `reslice`, so `poll_job` is unchanged.
///
/// Thread-safety: the `Solid` is built AND consumed here; only the output STL PATH leaves this
/// function. A `Solid` is `!Send` and never crosses the task boundary — the compiler enforces it.
/// See `docs/manifold-thread-safety.md`.
pub fn reslice_kernel(
    root: Option<&Path>,
    source: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    use fab_scad::kernel::Solid;

    // Cache the base: render the whole model once; reuse it across every reslice.
    let base_stl = whole_stl(source, out_dir);
    if !base_stl.exists() {
        render_whole(root, source, out_dir)?;
    }
    let base = Solid::from_stl_file(&base_stl)?;

    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut { axis: axis.to_string(), at: Num::Float(at) })
        .collect();
    let spec = Slicing {
        printer: None,
        cut,
        connector: to_connectors(connectors),
        orient: to_orient(orient),
    };

    let pieces = slicing::slice_solid(&spec, &base)?;
    ensure!(!pieces.is_empty(), "slice produced no pieces");
    let laid: Vec<Solid> = pieces
        .iter()
        .map(|(i, s)| s.translate(i[0] as f64 * spread, i[1] as f64 * spread, i[2] as f64 * spread))
        .collect();
    let out = out_dir.join(format!("{}-sliced.stl", stem_of(source)));
    Solid::batch_union(&laid).write_stl(&out)?;
    Ok(out)
}

/// Per-connector onion feasibility under the current cuts + orientations, index-aligned with
/// `connectors`: `true` = prints support-free, `false` = downgrades to a bolt. Pure (no render),
/// so the GUI can flag joints live as cuts/orientations change. Same gate `reslice` carves with.
pub fn conn_feasibility(
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
) -> Result<Vec<bool>> {
    let mut spec = cuts_to_spec(cuts);
    spec.connector = to_connectors(connectors);
    spec.orient = to_orient(orient);
    slicing::onion_feasibility(&spec)
}

/// The two connector kinds the GUI places (both consumed by the slicer): Onion = support-free
/// peg/socket, auto-sized from the cross-section; Bolt = heat-set pocket + machine screw across the
/// cut. An onion that can't print support-free downgrades to a bolt in the slice regardless.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnKind {
    Onion,
    Bolt,
}

/// A connector to place, resolved for slicing: `cut` is the index into the cuts slice passed
/// alongside, `pos` the two coords in the cut plane's non-axis dims. `size` is the onion diameter
/// (auto-sized from the cross-section, ignored for a bolt); `screw` the bolt size ("M3"/"M4"/"M5",
/// ignored for an onion). `kind` picks which.
#[derive(Clone, Copy)]
pub struct Conn {
    pub cut: usize,
    pub pos: [f64; 2],
    pub size: f64,
    pub kind: ConnKind,
    pub screw: &'static str,
}

/// A per-piece print orientation, resolved for slicing: the slab multi-index and its build-up
/// direction (model space, unit). Threaded into `reslice` as `[slicing.orient]` so the slice
/// honours the auto-picked / manual print orientation (and gates the onions accordingly).
#[derive(Clone, Copy)]
pub struct Orient3 {
    pub piece: [usize; 3],
    pub up: [f64; 3],
}

/// Write a `$preview = true; include <source>;` wrapper so the source's
/// `$fn = $preview ? low : high` resolves to the low (fast) path. Returns the wrapper path.
fn preview_wrapper(source: &Path, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let abs = source.canonicalize()?;
    let wrap = out_dir.join(format!("{}-preview.scad", stem_of(source)));
    std::fs::write(&wrap, format!("$preview = true;\ninclude <{}>;\n", abs.display()))?;
    Ok(wrap)
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into())
}

/// A preview `StlMesh` (triangle soup) → an indexed `bambu::Mesh`, deduping shared vertices by exact
/// bits (the kernel emits bit-identical coords for shared verts, so the weld is exact).
fn stlmesh_to_bambu(m: &StlMesh) -> bambu::Mesh {
    use std::collections::HashMap;
    let mut map: HashMap<[u32; 3], u32> = HashMap::new();
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut cur = [0u32; 3];
    for (k, p) in m.positions.iter().enumerate() {
        let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
        let idx = *map.entry(key).or_insert_with(|| {
            verts.push([p[0] as f64, p[1] as f64, p[2] as f64]);
            (verts.len() - 1) as u32
        });
        cur[k % 3] = idx;
        if k % 3 == 2 {
            tris.push(cur);
        }
    }
    bambu::Mesh { verts, tris }
}

/// Export the print-oriented preview pieces as a Bambu multi-plate project `.3mf` at `out`. `ups[i]`
/// is the resolved build-up for `pieces[i]` (auto-pick or the user's manual override). Bin-packs onto
/// the fewest `bed`-sized plates, `gap` mm apart. Pure mesh work — no `Solid` — so the caller can run
/// it wherever; it's cheap enough to call inline on a click.
pub fn export_plates(
    pieces: &[PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    gap: f64,
    out: &Path,
) -> Result<bambu::ExportSummary> {
    let to_place: Vec<PieceToPlace> = pieces
        .iter()
        .zip(ups)
        .map(|(pp, &up)| PieceToPlace { mesh: stlmesh_to_bambu(&pp.mesh), up })
        .collect();
    bambu::export_plates(out, to_place, bed, gap)
}

/// Open `source` in the OpenSCAD GUI (detached, non-blocking) — the same binary fab renders with
/// (`$OPENSCAD` / the macOS .app / `$PATH`). Sets `OPENSCADPATH` from `root` so the model's includes
/// (scad-lib, BOSL2) resolve. Closes the dogfooding loop: edit + save in OpenSCAD, and the GUI's
/// file-watch re-renders. Errors only if OpenSCAD can't be found or the spawn fails.
pub fn open_in_openscad(root: Option<&Path>, source: &Path) -> Result<()> {
    let bin = fab_scad::openscad::find_bin()
        .context("OpenSCAD not found — set $OPENSCAD or install it")?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg(source); // no -o → OpenSCAD opens the editor GUI rather than rendering headless
    if let Some(r) = root {
        cmd.env(
            "OPENSCADPATH",
            format!("{}:{}", r.join("libs").display(), r.join("scad-lib").display()),
        );
    }
    cmd.spawn().with_context(|| format!("launching OpenSCAD ({})", bin.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn reslice_kernel_caches_the_base() {
        let tmp = std::env::temp_dir().join(format!("gui_reslice_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();

        let conns = [Conn { cut: 0, pos: [12.0, 0.0], size: 12.0, kind: ConnKind::Onion, screw: "M3" }];
        // Two-axis cut + onion — the floater case — sliced in-process off the cached base.
        let out = reslice_kernel(None, &src, &[('x', 0.0), ('y', 0.0)], &conns, &[], 40.0, &tmp)
            .expect("first reslice");
        let base = whole_stl(&src, &tmp);
        assert!(base.exists(), "base STL should be cached after the first reslice");
        assert!(!stl::load_stl(&out).unwrap().positions.is_empty(), "sliced mesh has geometry");

        // A second reslice (different cut) must NOT re-render the base — that's the reactivity win.
        let mtime0 = std::fs::metadata(&base).unwrap().modified().unwrap();
        reslice_kernel(None, &src, &[('x', 5.0), ('y', 0.0)], &conns, &[], 40.0, &tmp)
            .expect("second reslice");
        let mtime1 = std::fs::metadata(&base).unwrap().modified().unwrap();
        assert_eq!(mtime0, mtime1, "second reslice re-rendered the base (cache miss)");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn print_layout_kernel_orients_every_piece() {
        let tmp = std::env::temp_dir().join(format!("gui_printlayout_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();

        // One X cut → two pieces; both lay out with a unit build-up and real geometry.
        let pieces = print_layout_kernel(None, &src, &[('x', 0.0)], &[], &tmp).expect("print layout");
        assert_eq!(pieces.len(), 2, "one cut on a box makes two pieces");
        for p in &pieces {
            assert!(!p.mesh.positions.is_empty(), "piece {:?} has geometry", p.piece);
            let n = (p.up[0] * p.up[0] + p.up[1] * p.up[1] + p.up[2] * p.up[2]).sqrt();
            assert!((n - 1.0).abs() < 1e-3, "up {:?} should be a unit vector", p.up);
        }
        assert!(whole_stl(&src, &tmp).exists(), "base STL cached (front-door rendered once)");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
