//! The GUI's bridge to fab — drives geometry in-process via the shared `fab_scad` lib
//! (no subprocess, same code `fab slice` runs). Renders/slices at PREVIEW quality: it wraps
//! the source in `$preview = true; include <source>;` so models that gate detail on
//! `$fn = $preview ? low : high` render fast (nail_cure: 2.4s vs 43s at full $fn). Final,
//! full-quality output is `fab`'s job; the GUI just needs a quick, responsive preview.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};

use fab_scad::bambu::{self, PieceToPlace};
use fab_scad::manifest::{Connector, Cut, PieceOrient, Slicing};
use fab_scad::num::Num;
use fab_scad::openscad::Openscad;
use fab_scad::slicing;

// The shared geometry types the planner/auto-slice/orient APIs take (J.6 unified everything on `fab_lang`'s
// Vec3). Aliased `FVec3` so it doesn't collide with Bevy's `Vec3` used throughout the scene code.
use fab_lang::{Dims, Vec3 as FVec3};

use crate::stl::{self, StlMesh};

const TIMEOUT: Duration = Duration::from_secs(120);

/// A `[slicing]` spec carrying only the cuts — the shared base for per-piece rendering and the
/// orientation sweep (connectors/orientation are layered on by the specific caller).
fn cuts_to_spec(cuts: &[(char, f64)]) -> Slicing {
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut {
            axis: axis.to_string(),
            at: Num::Float(at),
        })
        .collect();
    Slicing {
        printer: None,
        cut,
        connector: vec![],
        orient: vec![],
        parts: vec![],
    }
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
            comp: 0,
            up: [
                Num::Float(o.up[0]),
                Num::Float(o.up[1]),
                Num::Float(o.up[2]),
            ],
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

/// Render the source whole at PREVIEW quality via SCAD-RS (in-process — no OpenSCAD subprocess), returning
/// the STL. Switched from the OpenSCAD binary as the live-preview loop's engine (Q.1 dogfooding): an edit now
/// re-renders in pure Rust + Manifold in milliseconds, so the GUI's `watch_source` gives a genuinely live
/// preview off `fab_scad`, not a subprocess round-trip. The `$preview = true` wrapper still makes
/// `$fn = $preview ? low : high` models render at low detail — scad-rs honors `$preview` like any variable.
/// The `Solid` is built AND consumed here (written to STL); it's `!Send` and never crosses the async-task
/// boundary the caller spawns this on. (The slice/export path still uses OpenSCAD — switched separately.)
pub fn render_whole(root: Option<&Path>, source: &Path, out_dir: &Path) -> Result<PathBuf> {
    use fab_scad::backend::{build_geo, ManifoldBackend};
    // Wrap in `$preview = true; include <ABSOLUTE source>;` (so `$fn = $preview ? lo : hi` takes the fast
    // path) but evaluate that wrapper against the SOURCE's OWN directory — NOT `out_dir`. scad-rs threads a
    // single base dir for every `import()`, so a wrapper written into the temp `out_dir` made a relative
    // `import("../FamilyLogo.svg")` resolve next to the temp file (→ ENOENT) instead of beside the model.
    // `resolve_geometry_with_base` lets us pass the model's dir explicitly; the absolute `include` still
    // resolves the source regardless. (The OpenSCAD slice path resolves imports per-containing-file, so it
    // was never bitten — this is a scad-rs-only base-dir seam.)
    let abs = source
        .canonicalize()
        .with_context(|| format!("resolving {}", source.display()))?;
    let base = abs.parent().unwrap_or_else(|| Path::new("."));
    let wrap_src = format!("$preview = true;\ninclude <{}>;\n", abs.display());
    std::fs::create_dir_all(out_dir)?;
    let out = out_dir.join(format!("{}.stl", stem_of(source)));
    let libs = preview_libs(root);
    let tree = fab_scad::import::resolve_geometry_with_base(
        &wrap_src,
        base,
        &libs,
        fab_lang::Config::from_env(),
    )
    .with_context(|| format!("scad-rs eval of {}", source.display()))?;
    let solid = build_geo(&tree, &ManifoldBackend)
        .filter(|s| !s.is_empty())
        .with_context(|| {
            format!(
                "scad-rs rendered EMPTY geometry (no faces) for {}",
                source.display()
            )
        })?;
    std::fs::write(&out, solid.to_stl_bytes())
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(out)
}

/// The library search path scad-rs's loader resolves `<lib.scad>` against — the workspace `libs/` (BOSL2) +
/// `scad-lib`, matching the `OPENSCADPATH` the oracle path uses. The preview wrapper's `include <ABSOLUTE
/// source>` resolves the source file itself; the source's own same-dir includes resolve against its parent
/// inside the loader, so they need no entry here.
fn preview_libs(root: Option<&Path>) -> Vec<PathBuf> {
    root.map(|r| vec![r.join("libs"), r.join("scad-lib")])
        .unwrap_or_default()
}

/// Render the source into its TOP-LEVEL PARTS (T.2b) — one preview STL per implicit-union child at
/// the model root (the `wall_sliced()` / `frame_sliced()` / … calls), written to `{stem}-part{i}.stl`
/// beside `render_whole`'s output, each paired with its bbox. Same `$preview` wrapper + own-dir eval
/// as `render_whole`; the split is `backend::build_geo_parts`. Each part Solid is built AND consumed
/// here (written to STL) — none crosses the async boundary (`!Send`). Empty parts are dropped; the
/// returned order is authored order (index `i` in the filename tracks it). The GUI's `kick_render`
/// consumes this: `poll_job` seeds one `Part` + one `Model` entity per returned STL.
/// One rendered top-level part: its whole STL path, its `(min, max)` bbox, and its provenance name
/// (the top-level module/function that produced it, or `None` when anonymous / ambiguous) — T.2b.
pub type PartRender = (PathBuf, (FVec3, FVec3), Option<String>);

pub fn render_parts(root: Option<&Path>, source: &Path, out_dir: &Path) -> Result<Vec<PartRender>> {
    use fab_scad::backend::{build_geo_parts, ManifoldBackend};
    let abs = source
        .canonicalize()
        .with_context(|| format!("resolving {}", source.display()))?;
    let base = abs.parent().unwrap_or_else(|| Path::new("."));
    let wrap_src = format!("$preview = true;\ninclude <{}>;\n", abs.display());
    std::fs::create_dir_all(out_dir)?;
    let libs = preview_libs(root);
    let tree = fab_scad::import::resolve_geometry_with_base(
        &wrap_src,
        base,
        &libs,
        fab_lang::Config::from_env(),
    )
    .with_context(|| format!("scad-rs eval of {}", source.display()))?;
    let stem = stem_of(source);
    let mut out = Vec::new();
    // ManifoldBackend's Solid is `Option<Solid>` (the empty algebra is `None`) — flatten drops empty
    // parts, then guard the rare Some(empty) too.
    for (i, solid) in build_geo_parts(&tree, &ManifoldBackend)
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let Some(bbox) = solid.bbox() else { continue };
        let path = out_dir.join(format!("{stem}-part{i}.stl"));
        std::fs::write(&path, solid.to_stl_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        out.push((path, bbox));
    }
    ensure!(
        !out.is_empty(),
        "scad-rs rendered EMPTY geometry (no parts) for {}",
        source.display()
    );
    // Attach provenance names, but ONLY when the AST-derived count matches the actual part split —
    // otherwise the alignment is ambiguous and labels stay generic (a wrong name is worse than none).
    let names = fab_scad::backend::part_names(source);
    let names = if names.len() == out.len() {
        names
    } else {
        vec![None; out.len()]
    };
    Ok(out
        .into_iter()
        .zip(names)
        .map(|((path, bbox), name)| (path, bbox, name))
        .collect())
}

/// One printable piece for the print-orientation preview: its slab multi-index, its
/// connected-COMPONENT index within that slab (0 when the slab is one solid; >0 splits a presliced
/// blob into its real pieces — T.2a), the mesh (WITH its joints carved — peg/socket), and the
/// least-support build-up (`auto_orient::best_up`). Empty slabs are dropped upstream.
pub struct PiecePrint {
    pub piece: [usize; 3],
    pub comp: usize,
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
    part_stl: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
) -> Result<Vec<PiecePrint>> {
    use fab_scad::kernel::Solid;

    // Slice off this PART's cached whole STL (`render_parts` wrote it on load) — one part per call,
    // so the GUI fans this over every top-level part and co-packs the pieces (T.2b.4). Both passes
    // reuse the one loaded base.
    let base = Solid::from_stl_file(part_stl)?;

    // Pass 1: BARE slice (no connectors) → least-support orientation per non-empty piece. Each slab
    // is split into its CONNECTED COMPONENTS first: a presliced part is one uncut slab holding many
    // disjoint sub-solids (BOSL2 `partition`), and scoring the whole blob picks one meaningless 45°
    // build-up (T.1) — so orient each component on its own. (Axis-aligned cuts only today, so the
    // cut-face normals are already in `best_up`'s base set — none.)
    let mut ups: HashMap<([usize; 3], usize), [f64; 3]> = HashMap::new();
    for (piece, solid) in slicing::slice_solid(&cuts_to_spec(cuts), &base)? {
        for (comp, csolid) in solid.components().into_iter().enumerate() {
            let mesh = stl::load_stl_bytes(&csolid.to_stl_bytes())?;
            if mesh.positions.is_empty() {
                continue; // a degenerate component — nothing to print
            }
            ups.insert(
                (piece, comp),
                fab_scad::auto_orient::best_up(&to_tris(&mesh), &[]).to_array(),
            );
        }
    }

    // Pass 2: carve each slab with the onions, gated by its per-SLAB orientation (the first
    // component's build-up — connectors only exist in the cut case, where a slab is a single
    // component, so this is that component's own up). Then re-split into components for the pieces.
    let mut spec = cuts_to_spec(cuts);
    spec.connector = to_connectors(connectors);
    spec.orient = slab_orients(&ups);

    let mut out = Vec::new();
    for (piece, solid) in slicing::slice_solid(&spec, &base)? {
        for (comp, csolid) in solid.components().into_iter().enumerate() {
            let mesh = stl::load_stl_bytes(&csolid.to_stl_bytes())?;
            if mesh.positions.is_empty() {
                continue;
            }
            // The build-up this component was oriented to in pass 1 (default +Z if a connector diff
            // dropped a bare component that reappears carved — shouldn't happen with axis cuts).
            let up = ups.get(&(piece, comp)).copied().unwrap_or([0.0, 0.0, 1.0]);
            out.push(PiecePrint {
                piece,
                comp,
                mesh,
                up: [up[0] as f32, up[1] as f32, up[2] as f32],
            });
        }
    }
    Ok(out)
}

/// Project the per-(slab, component) build-ups down to ONE build-up per SLAB for the slice codegen
/// (`slicing`'s onion gating + bolt teardrop read `[slicing.orient]` by slab index). Uses each
/// slab's component 0 — a multi-component slab only occurs presliced (no connectors), where this
/// gates nothing; a single-component slab (the cut case) is exactly that component's own build-up.
fn slab_orients(ups: &HashMap<([usize; 3], usize), [f64; 3]>) -> Vec<PieceOrient> {
    ups.iter()
        .filter(|((_, comp), _)| *comp == 0)
        .map(|((piece, _), up)| PieceOrient {
            piece: *piece,
            comp: 0,
            up: [Num::Float(up[0]), Num::Float(up[1]), Num::Float(up[2])],
        })
        .collect()
}

/// `StlMesh` positions (flat, 3 verts per tri) → `[[f64; 3]; 3]` triangles for the orientation math.
fn to_tris(m: &StlMesh) -> Vec<[FVec3; 3]> {
    m.positions
        .chunks_exact(3)
        .map(|t| {
            std::array::from_fn(|i| FVec3::new(t[i][0] as f64, t[i][1] as f64, t[i][2] as f64))
        })
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
    fab_scad::auto::plan(
        &base,
        FVec3::from_array(min),
        FVec3::from_array(max),
        Dims::from_array(bed),
    )
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
        parts: vec![],
    };
    slicing::slice_part(&oscad, &wrap, &spec, spread, out_dir, TIMEOUT)
}

/// Reslice ONE part IN-PROCESS via the Manifold kernel (Track C 11.10, T.2b) — the reactive hot
/// path. `render_parts` already wrote every part's `{stem}-part{i}.stl` on load, so the base is
/// right there; slicing part A off it never re-renders or touches part B. Output lands at
/// `{part_stem}-sliced.stl` (unique per part). !Send discipline: the Solid is built AND consumed
/// here — only the STL path leaves the function, never crossing the async-task boundary.
pub fn reslice_part_kernel(
    part_stl: &Path,
    cuts: &[(char, f64)],
    connectors: &[Conn],
    orient: &[Orient3],
    spread: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    use fab_scad::kernel::Solid;
    let base = Solid::from_stl_file(part_stl)?;
    let cut = cuts
        .iter()
        .map(|&(axis, at)| Cut {
            axis: axis.to_string(),
            at: Num::Float(at),
        })
        .collect();
    let spec = Slicing {
        printer: None,
        cut,
        connector: to_connectors(connectors),
        orient: to_orient(orient),
        parts: vec![],
    };
    let pieces = slicing::slice_solid(&spec, &base)?;
    ensure!(!pieces.is_empty(), "slice produced no pieces");
    let laid: Vec<Solid> = pieces
        .iter()
        .map(|(i, s)| {
            s.translate(FVec3::new(
                i[0] as f64 * spread,
                i[1] as f64 * spread,
                i[2] as f64 * spread,
            ))
        })
        .collect();
    let stem = part_stl
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let out = out_dir.join(format!("{stem}-sliced.stl"));
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    std::fs::write(
        &wrap,
        format!("$preview = true;\ninclude <{}>;\n", abs.display()),
    )?;
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
    pieces: &[&PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    gap: f64,
    out: &Path,
) -> Result<bambu::ExportSummary> {
    let to_place: Vec<PieceToPlace> = pieces
        .iter()
        .zip(ups)
        .map(|(pp, &up)| PieceToPlace {
            mesh: stlmesh_to_bambu(&pp.mesh),
            up,
        })
        .collect();
    bambu::export_plates(out, to_place, bed, gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn reslice_part_kernel_slices_off_a_prerendered_base() {
        let tmp = std::env::temp_dir().join(format!("gui_reslice_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();

        // The part STL is the load-time render (render_parts writes one per part; a single-object
        // model renders as ONE part). reslice_part_kernel slices straight off it — no re-render.
        let base = render_whole(None, &src, &tmp).expect("render base");

        let conns = [Conn {
            cut: 0,
            pos: [12.0, 0.0],
            size: 12.0,
            kind: ConnKind::Onion,
            screw: "M3",
        }];
        // Two-axis cut + onion — the floater case — sliced in-process off the pre-rendered base.
        let out = reslice_part_kernel(&base, &[('x', 0.0), ('y', 0.0)], &conns, &[], 40.0, &tmp)
            .expect("first reslice");
        assert!(
            !stl::load_stl(&out).unwrap().positions.is_empty(),
            "sliced mesh has geometry"
        );

        // A second reslice (different cut) must NOT touch the base STL — the reactivity win: the
        // base is rendered once at load and every edit slices off it in place.
        let mtime0 = std::fs::metadata(&base).unwrap().modified().unwrap();
        reslice_part_kernel(&base, &[('x', 5.0), ('y', 0.0)], &conns, &[], 40.0, &tmp)
            .expect("second reslice");
        let mtime1 = std::fs::metadata(&base).unwrap().modified().unwrap();
        assert_eq!(mtime0, mtime1, "second reslice rewrote the base STL");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_parts_splits_top_level_into_separate_stls() {
        // T.2b keystone at the GUI edge: two top-level cubes → two independent part STLs at their
        // authored positions (no libs, so root = None evaluates the cubes directly).
        let tmp = std::env::temp_dir().join(format!("gui_parts_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("twin.scad");
        std::fs::write(
            &src,
            "cube([10,10,10]); translate([40,0,0]) cube([10,10,10]);",
        )
        .unwrap();

        let parts = render_parts(None, &src, &tmp).expect("render parts");
        assert_eq!(parts.len(), 2, "two top-level cubes → two part STLs");
        assert!(
            parts.iter().all(|(p, _, _)| p.exists()),
            "each part STL written"
        );
        // The two parts keep their authored X positions (0..10 and 40..50), proving they're the
        // distinct top-level items, not one merged solid.
        let xs: Vec<f64> = parts.iter().map(|(_, (min, _), _)| min.x).collect();
        assert!(
            xs.iter().any(|&x| x.abs() < 1.0) && xs.iter().any(|&x| (x - 40.0).abs() < 1.0),
            "parts at authored positions: {xs:?}"
        );
        // Provenance (T.2b): both parts name to "cube" — the second descends past `translate`.
        let names: Vec<Option<&str>> = parts.iter().map(|(_, _, n)| n.as_deref()).collect();
        assert_eq!(
            names,
            vec![Some("cube"), Some("cube")],
            "top-level module names, wrapper-descended"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn print_layout_splits_a_presliced_blob_into_flat_pieces() {
        // T.2a regression: a "presliced" part is many disjoint solids unioned into one (BOSL2
        // `partition` with spread). With no cuts, slice_solid hands back the whole blob as ONE
        // slab — so print_layout_kernel must split it into connected components and orient EACH,
        // else best_up scores the blob and every piece tilts ~45° (the dogfood bug). Two cubes
        // 60mm apart stand in for the blob; pre-seed the base STL so no render is needed.
        use fab_scad::kernel::Solid;
        let tmp = std::env::temp_dir().join(format!("gui_presliced_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let part_stl = tmp.join("twin-part0.stl"); // the part's cached whole STL (render_parts output)
        let blob = Solid::cube(20.0, 20.0, 20.0, true)
            .union(&Solid::cube(20.0, 20.0, 20.0, true).translate(FVec3::new(60.0, 0.0, 0.0)));
        std::fs::write(&part_stl, blob.to_stl_bytes()).unwrap();

        let pieces = print_layout_kernel(&part_stl, &[], &[]).expect("print layout");
        assert_eq!(pieces.len(), 2, "the blob splits into its two components");
        // The two share the slab index [0,0,0] but get distinct component indices.
        assert_eq!(pieces[0].piece, [0, 0, 0]);
        let comps: std::collections::HashSet<usize> = pieces.iter().map(|p| p.comp).collect();
        assert_eq!(
            comps,
            [0, 1].into_iter().collect(),
            "distinct comp ids 0 and 1"
        );
        // Each cube lies FLAT: its build-up is an axis (a component ≈ ±1), never a 45° tilt (≈0.707).
        for p in &pieces {
            let m = p.up.iter().map(|c| c.abs()).fold(0.0_f32, f32::max);
            assert!(
                m > 0.99,
                "piece {:?}/{} up {:?} is a 45° tilt, not flat",
                p.piece,
                p.comp,
                p.up
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[ignore = "needs OpenSCAD; run with --ignored"]
    fn print_layout_kernel_orients_every_piece() {
        let tmp = std::env::temp_dir().join(format!("gui_printlayout_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();
        // The part's whole STL is the load-time render (render_parts writes one per part; a single-
        // object model renders as ONE part). print_layout_kernel slices straight off it.
        let base = render_whole(None, &src, &tmp).expect("render base");

        // One X cut → two pieces; both lay out with a unit build-up and real geometry.
        let pieces = print_layout_kernel(&base, &[('x', 0.0)], &[]).expect("print layout");
        assert_eq!(pieces.len(), 2, "one cut on a box makes two pieces");
        for p in &pieces {
            assert!(
                !p.mesh.positions.is_empty(),
                "piece {:?} has geometry",
                p.piece
            );
            let n = (p.up[0] * p.up[0] + p.up[1] * p.up[1] + p.up[2] * p.up[2]).sqrt();
            assert!(
                (n - 1.0).abs() < 1e-3,
                "up {:?} should be a unit vector",
                p.up
            );
        }
        assert!(base.exists(), "base STL present");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
