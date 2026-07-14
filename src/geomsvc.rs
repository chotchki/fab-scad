//! The geometry SERVICE (C.2): `geomsg::Request` in, `geomsg::Response` out — every kernel op
//! the web app needs behind one seam. Runs in the fab-geom worker on wasm, on a task pool
//! natively. Solids never cross the boundary (the !Send contract): bytes in, bytes out.

use anyhow::{Context, Result, anyhow, ensure};
use std::collections::HashMap;

use crate::auto_orient;
use crate::geomsg::*;
use crate::kernel::Solid;
use crate::manifest::{Connector, Cut, PieceOrient, Slicing};
use crate::num::Num;
use crate::{auto, auto_slice, slicing, stl, threemf_in};
use fab_lang::{Affine, Dims, Tri, Vec3};

/// Base solids held by the service, addressed by [`SolidId`] — the stateful home the `!Send` `Solid`
/// never leaves (bytes/handles cross, the Solid stays). ONE store per execution context (kernel thread
/// natively, Worker on wasm); its `shard` tags every id it mints so the pool routes ops back to it.
// `mint` (+ shard/next/cap) is exercised by the render arms, which are native-only until the wasm
// source-bytes render lands (W.3.6) — so in the kernel-without-native (wasm worker) config they read
// as dead until then. Silence ONLY that config, not native.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub struct SolidStore {
    shard: u16,
    next: u32,
    cap: usize,
    map: HashMap<SolidId, Solid>,
}

impl SolidStore {
    pub fn new(shard: u16) -> Self {
        Self {
            shard,
            next: 0,
            cap: 64,
            map: HashMap::new(),
        }
    }

    /// Register a base solid, returning its handle. Bounded: a missed `Free` evicts the oldest rather
    /// than growing without limit (an op on an evicted id returns `Failed` → the GUI re-renders).
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    fn mint(&mut self, s: Solid) -> SolidId {
        let id = SolidId {
            shard: self.shard,
            idx: self.next,
        };
        self.next += 1;
        self.map.insert(id, s);
        while self.map.len() > self.cap {
            if let Some(old) = self.map.keys().min_by_key(|k| k.idx).copied() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        id
    }

    /// Read a held base (RETAIN — reads never free). Unknown id → error → `Failed{"unknown handle"}`.
    fn get(&self, id: SolidId) -> Result<&Solid> {
        self.map
            .get(&id)
            .ok_or_else(|| anyhow!("unknown handle {}:{}", id.shard, id.idx))
    }

    fn free(&mut self, ids: &[SolidId]) {
        for id in ids {
            self.map.remove(id);
        }
    }
}

/// Compat entry for the STATELESS arms (fab-web's upload flow) — a throwaway store the 4 original arms
/// ignore. The stateful fab-gui ops go through [`handle_with_store`] on a persistent store.
pub fn handle(req: Request) -> Response {
    handle_with_store(&mut SolidStore::new(0), req)
}

/// The service: never panics outward, never errors the transport — failures are a Response.
pub fn handle_with_store(store: &mut SolidStore, req: Request) -> Response {
    let run = |store: &mut SolidStore| -> Result<Response> {
        match req {
            Request::Analyze { name, bytes, bed } => analyze(&name, &bytes, Dims::from_array(bed)),
            Request::Slice {
                objects,
                cuts,
                connectors,
                with_connectors,
            } => slice(&objects, &cuts, &connectors, with_connectors),
            Request::Export {
                objects,
                cuts,
                connectors,
                bed,
                gap,
            } => export(&objects, &cuts, &connectors, Dims::from_array(bed), gap),
            Request::Section { objects, axis, at } => Ok(Response::Sectioned {
                loops: rotated_union(&objects)?.cross_section(axis, at),
            }),
            Request::RenderWhole { source, root } => {
                render_whole_svc(store, &source, root.as_deref())
            }
            Request::RenderParts { source, root } => {
                render_parts_svc(store, &source, root.as_deref())
            }
            Request::Reslice {
                base,
                cuts,
                connectors,
                orient,
                spread,
            } => reslice_svc(store, base, &cuts, &connectors, &orient, spread),
            Request::CrossSection { base, axis, at } => Ok(Response::Sectioned {
                loops: store.get(base)?.cross_section(axis, at),
            }),
            Request::AutoPlan {
                base,
                min,
                max,
                bed,
            } => auto_plan_svc(store, base, min, max, bed),
            Request::PrintLayout {
                base,
                cuts,
                connectors,
            } => print_layout_svc(store, base, &cuts, &connectors),
            Request::Free { ids } => {
                store.free(&ids);
                Ok(Response::Freed)
            }
        }
    };
    run(store).unwrap_or_else(|e| Response::Failed {
        error: format!("{e:#}"),
    })
}

fn analyze(name: &str, bytes: &[u8], bed: Dims) -> Result<Response> {
    // Per object: (display-fallback bytes, maybe a Solid, color, tri count).
    let mut solids: Vec<Solid> = Vec::new();
    let mut raw: Vec<GeomObject> = Vec::new();
    let mut colors: Vec<Option<[f32; 4]>> = Vec::new();
    let mut tris = 0usize;
    let mut all_solid = true;
    if name.to_ascii_lowercase().ends_with(".3mf") {
        for o in threemf_in::parse_3mf(bytes)? {
            tris += o.tris.len();
            // The 3mf reader speaks raw [f64;3]/[u32;3]; lift to the kernel's Vec3/Tri at the boundary.
            let verts: Vec<Vec3> = o.verts.iter().map(|&v| Vec3::from_array(v)).collect();
            let faces: Vec<Tri> = o.tris.iter().map(|&t| Tri(t)).collect();
            match Solid::from_indexed(&verts, &faces) {
                Ok(s) => solids.push(s),
                Err(_) => all_solid = false,
            }
            raw.push(GeomObject {
                stl: stl::binary_from_indexed(&o.verts, &o.tris),
                color: o.color,
            });
            colors.push(o.color);
        }
    } else {
        tris += stl::load_stl_bytes(bytes)?.positions.len() / 3;
        match Solid::from_stl_bytes(bytes) {
            Ok(s) => solids.push(s),
            Err(_) => all_solid = false,
        }
        raw.push(GeomObject {
            stl: bytes.to_vec(),
            color: None,
        });
        colors.push(None);
    }
    if !all_solid || solids.is_empty() {
        return Ok(Response::Analyzed {
            objects: raw,
            plan: None,
            tris,
        });
    }

    let union = match solids.len() {
        1 => solids[0].transform(&Affine::IDENTITY),
        _ => Solid::batch_union(&solids),
    };
    let fit = auto_slice::best_fit_rotation(&union, bed);
    let planned = auto::plan(&union.transform(&fit.rot), fit.min, fit.max, bed)?;
    let objects = solids
        .iter()
        .zip(&colors)
        .map(|(s, c)| GeomObject {
            stl: s.transform(&fit.rot).to_stl_bytes(),
            color: *c,
        })
        .collect();
    Ok(Response::Analyzed {
        objects,
        plan: Some(PlanOut {
            rot: fit.rot.to_column_major(), // the wire carries the column-major matrix, as before
            min: fit.min.to_array(),
            max: fit.max.to_array(),
            cuts: planned.cuts,
            connectors: planned.connectors.iter().map(WireConn::from).collect(),
        }),
        tris,
    })
}

fn spec(cuts: &[(char, f64)], connectors: Vec<Connector>) -> Slicing {
    Slicing {
        printer: None,
        cut: cuts
            .iter()
            .map(|&(ax, at)| Cut {
                axis: ax.to_string(),
                at: Num::Float(at),
            })
            .collect(),
        connector: connectors,
        orient: vec![],
        parts: vec![],
    }
}

fn slice(
    objects: &[GeomObject],
    cuts: &[(char, f64)],
    connectors: &[WireConn],
    with_connectors: bool,
) -> Result<Response> {
    let conns = if with_connectors {
        connectors.iter().map(Connector::from).collect()
    } else {
        vec![]
    };
    let s = spec(cuts, conns);
    let mut pieces = Vec::new();
    for o in objects {
        let solid = Solid::from_stl_bytes(&o.stl).context("piece source")?;
        for (idx, cell) in slicing::slice_solid(&s, &solid)? {
            pieces.push(Piece {
                idx,
                stl: cell.to_stl_bytes(),
                color: o.color,
            });
        }
    }
    Ok(Response::Sliced { pieces })
}

fn export(
    objects: &[GeomObject],
    cuts: &[(char, f64)],
    connectors: &[WireConn],
    bed: Dims,
    gap: f64,
) -> Result<Response> {
    let base = rotated_union(objects)?;
    let mut buf = std::io::Cursor::new(Vec::new());
    let sum = auto::make_planned(
        base,
        cuts,
        connectors.iter().map(Connector::from).collect(),
        bed,
        &mut buf,
        gap,
    )?;
    Ok(Response::Exported {
        threemf: buf.into_inner(),
        pieces: sum.pieces,
        plates: sum.plates,
    })
}

fn rotated_union(objects: &[GeomObject]) -> Result<Solid> {
    let solids: Vec<Solid> = objects
        .iter()
        .map(|o| Solid::from_stl_bytes(&o.stl))
        .collect::<Result<_>>()?;
    Ok(match solids.len() {
        0 => Solid::batch_union(&[]),
        1 => solids.into_iter().next().expect("len checked"),
        _ => Solid::batch_union(&solids),
    })
}

// --- fab-gui ops (W.3): base solids MINTED into / READ from the store. Render is fs-coupled (the
// scad-rs `import` loader), hence native-gated; the wasm source-bytes eval lands at W.3.6. ---

/// Eval the source at PREVIEW quality (the `$preview=true` wrapper takes `$fn=$preview?lo:hi`'s fast
/// path). `Source::Path` uses the native fs loader (`crate::import`, which is JIT-coupled → native);
/// `Source::Bytes` evals IN-MEMORY via `fab_lang` directly — no fs, no JIT — the fs-less wasm-worker
/// render path (W.3.6). Returns the tree + the source PATH when there is one (bytes have none → `None`,
/// so provenance names are skipped downstream).
#[cfg(feature = "kernel")]
fn eval_preview(
    source: &Source,
    root: Option<&str>,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>)> {
    match source {
        Source::Path(path) => eval_path(path, root),
        Source::Bytes { main, libs } => {
            // The wasm worker has no fs — `use`/`include` resolve against the IN-MEMORY lib closure the
            // app gathered (W.3.6 Stage 2), keyed by normalized relative path. Empty = a no-include
            // model. import()/surface() from bytes still isn't wired.
            let _ = root;
            let src = String::from_utf8_lossy(main);
            let wrap = format!("$preview = true;\n{src}\n");
            let sources: std::collections::BTreeMap<std::path::PathBuf, String> = libs
                .iter()
                .filter_map(|(p, b)| {
                    Some((
                        std::path::PathBuf::from(p),
                        String::from_utf8(b.clone()).ok()?,
                    ))
                })
                .collect();
            let tree = fab_lang::resolve_geometry_from_sources(
                &wrap,
                &sources,
                None, // no JIT on the wasm worker — interp only (the web execution tier)
                fab_lang::Config::from_env(),
                |raw| {
                    Err(fab_lang::Error::Load(format!(
                        "import(\"{raw}\") is not supported on the wasm worker yet"
                    )))
                },
            )
            .context("scad-rs eval of source bytes")?;
            Ok((tree, None))
        }
    }
}

/// `Source::Path` eval — native fs loader (canonicalize + `include <abs>` + the workspace lib search).
#[cfg(all(feature = "kernel", feature = "native"))]
fn eval_path(
    path: &str,
    root: Option<&str>,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>)> {
    let src = std::path::PathBuf::from(path);
    let abs = src
        .canonicalize()
        .with_context(|| format!("resolving {path}"))?;
    let base = abs
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let wrap = format!("$preview = true;\ninclude <{}>;\n", abs.display());
    let libs: Vec<std::path::PathBuf> = root
        .map(|r| {
            let r = std::path::Path::new(r);
            vec![r.join("libs"), r.join("scad-lib")]
        })
        .unwrap_or_default();
    let tree = crate::import::resolve_geometry_with_base(
        &wrap,
        &base,
        &libs,
        fab_lang::Config::from_env(),
    )
    .with_context(|| format!("scad-rs eval of {path}"))?;
    Ok((tree, Some(src)))
}

/// wasm worker: no fs, so a `Source::Path` render can't resolve — the app sends `Source::Bytes` there.
#[cfg(all(feature = "kernel", not(feature = "native")))]
fn eval_path(
    _path: &str,
    _root: Option<&str>,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>)> {
    anyhow::bail!(
        "Path source needs the native fs loader; the wasm worker renders from Source::Bytes"
    )
}

#[cfg(feature = "kernel")]
fn render_whole_svc(
    store: &mut SolidStore,
    source: &Source,
    root: Option<&str>,
) -> Result<Response> {
    use crate::backend::{ManifoldBackend, build_geo};
    let (tree, _src) = eval_preview(source, root)?;
    let solid = build_geo(&tree, &ManifoldBackend)
        .filter(|s| !s.is_empty())
        .context("scad-rs rendered EMPTY geometry (no faces)")?;
    let (mn, mx) = solid.bbox().context("rendered solid has no bbox")?;
    let stl = solid.to_stl_bytes();
    let id = store.mint(solid);
    Ok(Response::Rendered {
        id,
        stl,
        min: mn.to_array(),
        max: mx.to_array(),
    })
}

/// A rendered part staged for the wire response: its held `SolidId`, STL bytes, and bbox min/max.
#[cfg(feature = "kernel")]
type StagedPart = (SolidId, Vec<u8>, [f64; 3], [f64; 3]);

#[cfg(feature = "kernel")]
fn render_parts_svc(
    store: &mut SolidStore,
    source: &Source,
    root: Option<&str>,
) -> Result<Response> {
    use crate::backend::{ManifoldBackend, build_geo_parts};
    let (tree, src) = eval_preview(source, root)?;
    let mut staged: Vec<StagedPart> = Vec::new();
    for solid in build_geo_parts(&tree, &ManifoldBackend)
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
    {
        let Some((mn, mx)) = solid.bbox() else {
            continue;
        };
        let stl = solid.to_stl_bytes();
        let id = store.mint(solid);
        staged.push((id, stl, mn.to_array(), mx.to_array()));
    }
    ensure!(
        !staged.is_empty(),
        "scad-rs rendered EMPTY geometry (no parts)"
    );
    let names = part_names_for(&src, staged.len());
    Ok(Response::PartsRendered {
        parts: staged
            .into_iter()
            .zip(names)
            .map(|((id, stl, min, max), name)| WirePart {
                id,
                stl,
                min,
                max,
                name,
            })
            .collect(),
    })
}

/// Per-part provenance names, but ONLY from a real source file (native fs) whose AST count matches the
/// split (a wrong label is worse than none). `Source::Bytes` (the wasm worker) has no path → all `None`.
#[cfg(feature = "kernel")]
fn part_names_for(src: &Option<std::path::PathBuf>, n: usize) -> Vec<Option<String>> {
    #[cfg(feature = "native")]
    if let Some(p) = src {
        let names = crate::backend::part_names(p);
        if names.len() == n {
            return names;
        }
    }
    let _ = src;
    vec![None; n]
}

fn reslice_svc(
    store: &SolidStore,
    base: SolidId,
    cuts: &[(char, f64)],
    connectors: &[WireConn],
    orient: &[WireOrient],
    spread: f64,
) -> Result<Response> {
    let solid = store.get(base)?;
    let mut spec = cuts_spec(cuts);
    spec.connector = connectors.iter().map(Connector::from).collect();
    spec.orient = orient_spec(orient);
    let pieces = slicing::slice_solid(&spec, solid)?;
    ensure!(!pieces.is_empty(), "slice produced no pieces");
    let laid: Vec<Solid> = pieces
        .iter()
        .map(|(i, s)| {
            s.translate(Vec3::new(
                i[0] as f64 * spread,
                i[1] as f64 * spread,
                i[2] as f64 * spread,
            ))
        })
        .collect();
    Ok(Response::Resliced {
        stl: Solid::batch_union(&laid).to_stl_bytes(),
    })
}

fn auto_plan_svc(
    store: &SolidStore,
    base: SolidId,
    min: [f64; 3],
    max: [f64; 3],
    bed: [f64; 3],
) -> Result<Response> {
    let solid = store.get(base)?;
    let comps = solid.components();
    let pieces = comps.len().max(1);
    // Presliced-and-already-fits → no cuts (double-slicing the spread-out bbox would re-cut a
    // pre-sliced model). Otherwise the real fit-to-bed plan.
    let plan = if comps.len() > 1
        && comps.iter().all(|c| {
            c.bbox().is_none_or(|(mn, mx)| {
                auto_slice::auto_slice(mn, mx, Dims::from_array(bed)).is_empty()
            })
        }) {
        auto::AutoPlan {
            cuts: Vec::new(),
            connectors: Vec::new(),
        }
    } else {
        auto::plan(
            solid,
            Vec3::from_array(min),
            Vec3::from_array(max),
            Dims::from_array(bed),
        )?
    };
    Ok(Response::Planned {
        cuts: plan.cuts,
        connectors: plan.connectors.iter().map(WireConn::from).collect(),
        pieces,
    })
}

fn print_layout_svc(
    store: &SolidStore,
    base: SolidId,
    cuts: &[(char, f64)],
    connectors: &[WireConn],
) -> Result<Response> {
    let solid = store.get(base)?;
    // Pass 1: BARE slice → least-support build-up per (slab, connected-component). A presliced blob is
    // one uncut slab of many disjoint sub-solids, so orient each component on its own (T.1/T.2a).
    let mut ups: HashMap<([usize; 3], usize), [f64; 3]> = HashMap::new();
    for (piece, cell) in slicing::slice_solid(&cuts_spec(cuts), solid)? {
        for (comp, csolid) in cell.components().into_iter().enumerate() {
            let mesh = stl::load_stl_bytes(&csolid.to_stl_bytes())?;
            if mesh.positions.is_empty() {
                continue;
            }
            ups.insert(
                (piece, comp),
                auto_orient::best_up(&mesh_tris(&mesh), &[]).to_array(),
            );
        }
    }
    // Pass 2: carve with the onions, gated by each slab's build-up; re-split into pieces.
    let mut spec = cuts_spec(cuts);
    spec.connector = connectors.iter().map(Connector::from).collect();
    spec.orient = slab_orients(&ups);
    let mut out = Vec::new();
    for (piece, cell) in slicing::slice_solid(&spec, solid)? {
        for (comp, csolid) in cell.components().into_iter().enumerate() {
            let bytes = csolid.to_stl_bytes();
            let mesh = stl::load_stl_bytes(&bytes)?;
            if mesh.positions.is_empty() {
                continue;
            }
            let up = ups.get(&(piece, comp)).copied().unwrap_or([0.0, 0.0, 1.0]);
            out.push(WirePiece {
                piece,
                comp,
                stl: bytes,
                up: [up[0] as f32, up[1] as f32, up[2] as f32],
            });
        }
    }
    Ok(Response::LaidOut { pieces: out })
}

/// A cuts-only `[slicing]` spec — the shared base for the per-piece render + orientation sweep.
fn cuts_spec(cuts: &[(char, f64)]) -> Slicing {
    Slicing {
        printer: None,
        cut: cuts
            .iter()
            .map(|&(axis, at)| Cut {
                axis: axis.to_string(),
                at: Num::Float(at),
            })
            .collect(),
        connector: vec![],
        orient: vec![],
        parts: vec![],
    }
}

/// Wire orientations → manifest `[slicing.orient]`.
fn orient_spec(orient: &[WireOrient]) -> Vec<PieceOrient> {
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

/// Project per-(slab, component) build-ups to ONE per SLAB (component 0) for the slice codegen.
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

/// `StlMesh` positions (flat, 3 verts/tri) → `[Vec3; 3]` triangles for the orientation math.
fn mesh_tris(m: &stl::StlMesh) -> Vec<[Vec3; 3]> {
    m.positions
        .chunks_exact(3)
        .map(|t| std::array::from_fn(|i| Vec3::new(t[i][0] as f64, t[i][1] as f64, t[i][2] as f64)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_cube() -> (Vec<GeomObject>, PlanOut) {
        let cube = Solid::cube(30.0, 30.0, 60.0, false).to_stl_bytes();
        match handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: cube,
            bed: [40.0; 3],
        }) {
            Response::Analyzed {
                objects,
                plan: Some(p),
                ..
            } => (objects, p),
            other => panic!("expected planned analyze, got {}", label(&other)),
        }
    }

    fn label(r: &Response) -> &'static str {
        match r {
            Response::Analyzed { .. } => "Analyzed",
            Response::Sliced { .. } => "Sliced",
            Response::Exported { .. } => "Exported",
            Response::Sectioned { .. } => "Sectioned",
            Response::Rendered { .. } => "Rendered",
            Response::PartsRendered { .. } => "PartsRendered",
            Response::Resliced { .. } => "Resliced",
            Response::Planned { .. } => "Planned",
            Response::LaidOut { .. } => "LaidOut",
            Response::Freed => "Freed",
            Response::Failed { .. } => "Failed",
        }
    }

    #[test]
    fn analyze_slice_export_round_trip_through_the_wire() {
        let (objects, plan) = analyze_cube();
        assert_eq!(plan.cuts.len(), 1, "60mm on a 40 bed → one cut");

        // Through the CODEC, like the worker sees it.
        let req = decode_request(&encode_request(&Request::Slice {
            objects: objects.clone(),
            cuts: plan.cuts.clone(),
            connectors: plan.connectors.clone(),
            with_connectors: true,
        }))
        .unwrap();
        let Response::Sliced { pieces } = handle(req) else {
            panic!("slice failed")
        };
        assert_eq!(pieces.len(), 2);

        let Response::Exported {
            threemf, pieces, ..
        } = handle(Request::Export {
            objects,
            cuts: plan.cuts,
            connectors: plan.connectors,
            bed: [40.0; 3],
            gap: 5.0,
        })
        else {
            panic!("export failed")
        };
        assert_eq!(pieces, 2);
        assert!(threemf.len() > 1000);
    }

    #[test]
    fn analyze_reports_view_only_for_soup() {
        // Open-shell soup: one triangle. Displays, never welds.
        let soup = stl::binary_from_indexed(
            &[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            &[[0, 1, 2]],
        );
        match handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: soup,
            bed: [40.0; 3],
        }) {
            Response::Analyzed {
                plan: None, tris, ..
            } => assert_eq!(tris, 1),
            other => panic!("expected view-only, got {}", label(&other)),
        }
    }

    #[test]
    fn section_returns_the_editor_profile() {
        let (objects, plan) = analyze_cube();
        let (_, at) = plan.cuts[0];
        let Response::Sectioned { loops } = handle(Request::Section {
            objects,
            axis: 2,
            at,
        }) else {
            panic!("section failed")
        };
        assert_eq!(loops.len(), 1, "solid square profile: one outline");
    }

    #[test]
    fn failures_travel_as_responses_not_panics() {
        let r = handle(Request::Analyze {
            name: "t.stl".into(),
            bytes: vec![1, 2, 3],
            bed: [40.0; 3],
        });
        assert!(matches!(r, Response::Failed { .. }));
    }

    // The stateful fab-gui path (W.3): render MINTS a handle, reads REUSE it, Free drops it, and an op
    // on a freed handle self-heals as Failed. Render is native (the fs loader), so gate it.
    #[cfg(feature = "native")]
    #[test]
    fn render_mints_a_handle_reused_by_reslice_then_freed() {
        let tmp = std::env::temp_dir().join(format!("geomsvc_handle_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();

        let mut store = SolidStore::new(0);
        let Response::PartsRendered { parts } = handle_with_store(
            &mut store,
            Request::RenderParts {
                source: Source::Path(src.to_string_lossy().into_owned()),
                root: None,
            },
        ) else {
            panic!("render failed")
        };
        assert_eq!(parts.len(), 1, "one cube → one part");
        let id = parts[0].id;
        assert_eq!(id.shard, 0, "handle carries the store's shard");

        // Reslice reads the HELD base — through the codec, like the worker sees it — no re-render.
        let req = decode_request(&encode_request(&Request::Reslice {
            base: id,
            cuts: vec![('x', 0.0)],
            connectors: vec![],
            orient: vec![],
            spread: 40.0,
        }))
        .unwrap();
        let Response::Resliced { stl } = handle_with_store(&mut store, req) else {
            panic!("reslice failed")
        };
        assert!(stl.len() > 100, "sliced STL has geometry");

        // A second reslice reuses the SAME handle — retained, not consumed.
        assert!(
            matches!(
                handle_with_store(
                    &mut store,
                    Request::Reslice {
                        base: id,
                        cuts: vec![('y', 0.0)],
                        connectors: vec![],
                        orient: vec![],
                        spread: 40.0,
                    }
                ),
                Response::Resliced { .. }
            ),
            "the base handle is retained across reslices"
        );

        // Free drops it; an op on the freed id self-heals as Failed (→ the GUI re-renders).
        assert!(matches!(
            handle_with_store(&mut store, Request::Free { ids: vec![id] }),
            Response::Freed
        ));
        assert!(
            matches!(
                handle_with_store(
                    &mut store,
                    Request::CrossSection {
                        base: id,
                        axis: 2,
                        at: 0.0,
                    }
                ),
                Response::Failed { .. }
            ),
            "an op on a freed handle → Failed"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Print-layout + auto-plan seam behaviour, exercised by MINTING a blob straight into the store (no
    // .scad render — so these run under kernel-WITHOUT-native too, i.e. the wasm worker config). They
    // guard the two tricky compositions the fab-gui wrapper used to own before the seam swallowed them.

    #[test]
    fn print_layout_splits_a_presliced_blob_into_flat_pieces() {
        // T.2a regression: a "presliced" part is many disjoint solids unioned into one. With no cuts
        // slice_solid hands back the whole blob as ONE slab — so print_layout_svc must split it into
        // connected components and orient EACH, else best_up scores the blob and every piece tilts
        // ~45° (the dogfood bug). Two cubes 60mm apart stand in for the blob.
        let mut store = SolidStore::new(0);
        let blob = Solid::cube(20.0, 20.0, 20.0, true)
            .union(&Solid::cube(20.0, 20.0, 20.0, true).translate(Vec3::new(60.0, 0.0, 0.0)));
        let id = store.mint(blob);
        let Response::LaidOut { pieces } = handle_with_store(
            &mut store,
            Request::PrintLayout {
                base: id,
                cuts: vec![],
                connectors: vec![],
            },
        ) else {
            panic!("print layout failed")
        };
        assert_eq!(pieces.len(), 2, "the blob splits into its two components");
        assert!(
            pieces.iter().all(|p| p.piece == [0, 0, 0]),
            "one slab, split by connected component"
        );
        let comps: HashMap<usize, ()> = pieces.iter().map(|p| (p.comp, ())).collect();
        assert_eq!(comps.len(), 2, "distinct comp ids 0 and 1");
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
    }

    #[test]
    fn auto_plan_skips_a_presliced_blob_whose_pieces_fit() {
        // A presliced part (disjoint components) whose components EACH fit the bed needs NO cuts —
        // auto-slicing the whole SPREAD-OUT bbox would double-slice an already-sliced model. Two 200mm
        // cubes 500mm apart: the whole bbox (700mm in X) overflows a 256 bed, each cube fits.
        let mut store = SolidStore::new(0);
        let bed = [256.0, 256.0, 256.0];
        let blob = Solid::cube(200.0, 200.0, 200.0, true)
            .union(&Solid::cube(200.0, 200.0, 200.0, true).translate(Vec3::new(500.0, 0.0, 0.0)));
        let id = store.mint(blob);
        let Response::Planned { cuts, pieces, .. } = handle_with_store(
            &mut store,
            Request::AutoPlan {
                base: id,
                min: [-100.0, -100.0, -100.0],
                max: [600.0, 100.0, 100.0],
                bed,
            },
        ) else {
            panic!("auto-plan failed")
        };
        assert!(
            cuts.is_empty(),
            "presliced-and-fits → no cuts, got {cuts:?}"
        );
        assert_eq!(pieces, 2, "two disjoint components");

        // A single CONNECTED oversized part still gets a real cut plan (component-aware, not a blanket
        // skip).
        let big = store.mint(Solid::cube(700.0, 200.0, 200.0, true));
        let Response::Planned {
            cuts: cuts2,
            pieces: pieces2,
            ..
        } = handle_with_store(
            &mut store,
            Request::AutoPlan {
                base: big,
                min: [-350.0, -100.0, -100.0],
                max: [350.0, 100.0, 100.0],
                bed,
            },
        )
        else {
            panic!("auto-plan (connected) failed")
        };
        assert!(
            !cuts2.is_empty(),
            "one connected 700mm part still slices to fit the bed"
        );
        assert_eq!(pieces2, 1, "one connected component");
    }

    #[test]
    fn render_whole_from_source_bytes_no_fs() {
        // W.3.6 keystone: the fs-less wasm-worker render path. Eval a NO-INCLUDE .scad straight from
        // bytes (what the browser sends via Source::Bytes) — no path, no fs, no JIT — and prove
        // RenderWhole mints a solid with the right bbox. Runs under kernel-WITHOUT-native too (this is
        // exactly the geom worker's config), so it guards the branch the browser actually exercises.
        let mut store = SolidStore::new(0);
        let Response::Rendered {
            id, stl, min, max, ..
        } = handle_with_store(
            &mut store,
            Request::RenderWhole {
                source: Source::Bytes {
                    main: b"cube([60,40,30], center=true);".to_vec(),
                    libs: vec![],
                },
                root: None,
            },
        )
        else {
            panic!("render from source bytes failed")
        };
        assert!(stl.len() > 100, "cube STL has geometry");
        assert_eq!(id.shard, 0, "handle carries the store's shard");
        // 60×40×30 centered → bbox ≈ [-30,-20,-15]..[30,20,15].
        assert!(
            (max[0] - 30.0).abs() < 0.01 && (min[2] + 15.0).abs() < 0.01,
            "unexpected bbox {min:?}..{max:?}"
        );

        // The minted base is REUSED by a reslice off the held handle — the stateful flow the worker's
        // persistent store serves.
        assert!(
            matches!(
                handle_with_store(
                    &mut store,
                    Request::Reslice {
                        base: id,
                        cuts: vec![('x', 0.0)],
                        connectors: vec![],
                        orient: vec![],
                        spread: 40.0,
                    }
                ),
                Response::Resliced { .. }
            ),
            "reslice reads the bytes-rendered base"
        );
    }

    #[test]
    fn render_from_source_bytes_resolves_an_include_from_the_lib_map() {
        // W.3.6 Stage 2: a model that INCLUDEs a lib renders on the fs-less worker, resolving the
        // include from the in-memory lib map (what the app gathers + sends in Source::Bytes.libs).
        // Proves REAL (include-using) models work, not just no-include ones.
        let mut store = SolidStore::new(0);
        let main = b"include <lib/box.scad>;\nmybox(50, 30, 20);".to_vec();
        let lib = b"module mybox(x, y, z) { cube([x, y, z], center = true); }".to_vec();
        let Response::Rendered { stl, min, max, .. } = handle_with_store(
            &mut store,
            Request::RenderWhole {
                source: Source::Bytes {
                    main,
                    libs: vec![("lib/box.scad".to_string(), lib)],
                },
                root: None,
            },
        ) else {
            panic!("render with an include failed")
        };
        assert!(stl.len() > 100, "the included module produced geometry");
        // mybox(50,30,20) → cube centered → bbox ≈ [-25,-15,-10]..[25,15,10].
        assert!(
            (max[0] - 25.0).abs() < 0.01 && (min[1] + 15.0).abs() < 0.01,
            "unexpected bbox {min:?}..{max:?}"
        );
    }
}
