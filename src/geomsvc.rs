//! The geometry SERVICE (C.2): `geomsg::Request` in, `geomsg::Response` out — every kernel op
//! the web app needs behind one seam. Runs in the fab-geom worker on wasm, on a task pool
//! natively. Solids never cross the boundary (the !Send contract): bytes in, bytes out.

use anyhow::{Context, Result, anyhow, ensure};
use std::collections::HashMap;

use crate::auto_orient;
use crate::geomsg::*;
use crate::kernel::Solid;

thread_local! {
    /// The EDITOR line a stamped eval error mapped to (W.3.37), stashed by `record_err_line` and drained by
    /// `handle_with_store` into `Response::Failed.line`. A thread-local because the service is single-threaded
    /// per store (kernel thread natively, Worker on wasm) and the error flattens to a String at ONE central
    /// catch — this rides the line there without threading a return through every render arm. Cleared per
    /// request so a stale line from a prior render never leaks onto a different failure.
    static ERR_LINE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}
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
    /// X.1: the persistent cross-render geometry cache — lives HERE (per execution context) so it
    /// survives across renders, unlike the per-build P.2 memo. The render arms thread it into
    /// `build_geo_cached`, so a live customizer reuses subtrees unchanged since the last render.
    geo_cache: crate::backend::GeoCache<Option<Solid>>,
}

impl SolidStore {
    pub fn new(shard: u16) -> Self {
        Self {
            shard,
            next: 0,
            cap: 64,
            map: HashMap::new(),
            geo_cache: crate::backend::GeoCache::new(),
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
    // Clear the per-request error-line carrier (W.3.37) so a stale line from a PRIOR render never rides a
    // later, different failure. Only an eval error sets it.
    ERR_LINE.set(None);
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
            Request::RenderWhole {
                source,
                root,
                preview,
                quality,
            } => render_whole_svc(store, &source, root.as_deref(), preview, quality),
            Request::RenderParts {
                source,
                root,
                quality,
            } => render_parts_svc(store, &source, root.as_deref(), quality),
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
            Request::SaveMeshes { base, budget } => save_meshes_svc(store, base, budget),
            Request::Free { ids } => {
                store.free(&ids);
                Ok(Response::Freed)
            }
        }
    };
    run(store).unwrap_or_else(|e| Response::Failed {
        error: format!("{e:#}"),
        line: ERR_LINE.take(),
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

/// Eval the source with `$preview` set to `preview` (so `$fn=$preview?lo:hi` takes the fast facet
/// path when `true`, the full-res path when `false` — the W.5.2 save-mesh render). `Source::Path` uses
/// the native fs loader (`crate::import`, which is JIT-coupled → native); `Source::Bytes` evals
/// The header fab injects before every model (W.3.25): `$preview` FIRST (fab-lang honors the leading
/// special-var config there), then the fab-OWNED tessellation quality (`$fa`/`$fs`, adaptive with
/// `$fn = 0`). Both precede the model, so a model's own `$fn`/`$fa` — or an un-migrated
/// `$fn = $preview ? …` — still overrides (graceful migration; local overrides win).
/// Stash the EDITOR line for a stamped eval error (W.3.37): map the error's source `span.start` through
/// `source` (the string eval actually parsed) and subtract `header_lines` — the fab-injected `$fa/$fs` wrap
/// prepends 2 lines on the Bytes path; the Path path `include`s the user file separately, so its spans are
/// file-local (0). No span (a parse error, a non-eval fault) ⇒ no line. Clamped to ≥ 1.
#[cfg(feature = "kernel")]
fn record_err_line(source: &str, header_lines: u32, err: &fab_lang::Error) {
    if let Some(span) = err.span() {
        let line = fab_lang::offset_to_line(source, span.start).saturating_sub(header_lines);
        ERR_LINE.set(Some(line.max(1)));
    }
}

#[cfg(feature = "kernel")]
fn wrap_header(preview: bool, quality: Quality) -> String {
    let (fa, fs) = quality.fa_fs();
    format!("$preview = {preview};\n$fn = 0; $fa = {fa}; $fs = {fs};\n")
}

/// IN-MEMORY via `fab_lang` directly — no fs, no JIT — the fs-less wasm-worker render path (W.3.6).
/// Returns the tree + the source PATH when there is one (bytes have none → `None`, so provenance names
/// are skipped downstream).
#[cfg(feature = "kernel")]
fn eval_source(
    source: &Source,
    root: Option<&str>,
    preview: bool,
    quality: Quality,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>, Vec<String>)> {
    match source {
        Source::Path(path) => eval_path(path, root, preview, quality),
        Source::Bytes { main, libs } => {
            // The wasm worker has no fs — `use`/`include` resolve against the IN-MEMORY lib closure the
            // app gathered (W.3.6 Stage 2), keyed by normalized relative path. Empty = a no-include
            // model. import()/surface() ASSETS ride the same closure (W.3.24), matched by basename.
            let _ = root;
            let src = String::from_utf8_lossy(main);
            let wrap = format!("{}{src}\n", wrap_header(preview, quality));
            let sources: std::collections::BTreeMap<std::path::PathBuf, String> = libs
                .iter()
                .filter_map(|(p, b)| {
                    Some((
                        std::path::PathBuf::from(p),
                        String::from_utf8(b.clone()).ok()?,
                    ))
                })
                .collect();
            let (tree, messages) = fab_lang::resolve_geometry_from_sources_full(
                &wrap,
                &sources,
                None, // no JIT on the wasm worker — interp only (the web execution tier)
                fab_lang::Config::from_env(),
                |raw| {
                    // import()/surface() asset (W.3.24): the app packs referenced assets into the closure
                    // (scad-lib's .svg etc); match by BASENAME since a model-relative "../x" can't
                    // normalize against a rootless main. A text pack ⇒ SVG works on the web.
                    let want = std::path::Path::new(raw).file_name().and_then(|n| n.to_str());
                    match libs.iter().find(|(p, _)| {
                        std::path::Path::new(p).file_name().and_then(|n| n.to_str()) == want
                    }) {
                        Some((_, bytes)) => crate::import::read_import_bytes(raw, bytes),
                        None => Err(fab_lang::Error::Load(format!(
                            "import(\"{raw}\"): asset not in the web pack (only scad-lib assets resolve on the web)"
                        ))),
                    }
                },
            )
            // Bytes: the user source is inline in `wrap`, after the 2-line `$fa/$fs` header (W.3.37).
            .inspect_err(|e| record_err_line(&wrap, 2, e))
            .context("scad-rs eval of source bytes")?;
            Ok((tree, None, messages.iter().map(|m| m.render()).collect()))
        }
    }
}

/// `Source::Path` eval — native fs loader (canonicalize + `include <abs>` + the workspace lib search).
#[cfg(all(feature = "kernel", feature = "native"))]
fn eval_path(
    path: &str,
    root: Option<&str>,
    preview: bool,
    quality: Quality,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>, Vec<String>)> {
    let src = std::path::PathBuf::from(path);
    let abs = src
        .canonicalize()
        .with_context(|| format!("resolving {path}"))?;
    let base = abs
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let wrap = format!(
        "{}include <{}>;\n",
        wrap_header(preview, quality),
        abs.display()
    );
    let libs: Vec<std::path::PathBuf> = root
        .map(|r| {
            let r = std::path::Path::new(r);
            vec![r.join("libs"), r.join("scad-lib")]
        })
        .unwrap_or_default();
    let (tree, messages) = crate::import::resolve_geometry_with_base_full(
        &wrap,
        &base,
        &libs,
        fab_lang::Config::from_env(),
    )
    // Path: the user file is `include`d separately, so a stamped span is file-LOCAL (header 0), mapped
    // against the file's own text — which for the GUI preview is the editor buffer written verbatim, so the
    // line lands exactly (W.3.37). Read lazily, only on error.
    .inspect_err(|e| {
        if e.span().is_some()
            && let Ok(txt) = std::fs::read_to_string(&abs)
        {
            record_err_line(&txt, 0, e);
        }
    })
    .with_context(|| format!("scad-rs eval of {path}"))?;
    Ok((
        tree,
        Some(src),
        messages.iter().map(|m| m.render()).collect(),
    ))
}

/// wasm worker: no fs, so a `Source::Path` render can't resolve — the app sends `Source::Bytes` there.
#[cfg(all(feature = "kernel", not(feature = "native")))]
fn eval_path(
    _path: &str,
    _root: Option<&str>,
    _preview: bool,
    _quality: Quality,
) -> Result<(fab_lang::Geo, Option<std::path::PathBuf>, Vec<String>)> {
    anyhow::bail!(
        "Path source needs the native fs loader; the wasm worker renders from Source::Bytes"
    )
}

/// A diagnostic suffix for an EMPTY render (W.3.22). The usual cause is unresolved libraries — a missing
/// workspace root ⇒ no search paths ⇒ every module undefined — which is far more actionable than a bare
/// "EMPTY". Pull the first "Can't open library" warning, else count the "Ignoring unknown module" ones.
#[cfg(feature = "kernel")]
fn empty_hint(messages: &[String]) -> String {
    if let Some(w) = messages.iter().find(|m| m.contains("Can't open library")) {
        return format!(
            " — libraries not found: {w} (is the workspace root — scad-lib + libs — reachable?)"
        );
    }
    let unknown = messages
        .iter()
        .filter(|m| m.contains("Ignoring unknown module"))
        .count();
    if unknown > 0 {
        return format!(" — {unknown} undefined module(s); likely a missing library or a typo");
    }
    String::new()
}

#[cfg(feature = "kernel")]
fn render_whole_svc(
    store: &mut SolidStore,
    source: &Source,
    root: Option<&str>,
    preview: bool,
    quality: Quality,
) -> Result<Response> {
    use crate::backend::{ManifoldBackend, build_geo_cached};
    let (tree, _src, messages) = eval_source(source, root, preview, quality)?;
    let solid = build_geo_cached(&tree, &ManifoldBackend, &mut store.geo_cache)
        .filter(|s| !s.is_empty())
        .with_context(|| {
            format!(
                "scad-rs rendered EMPTY geometry (no faces){}",
                empty_hint(&messages)
            )
        })?;
    let (mn, mx) = solid.bbox().context("rendered solid has no bbox")?;
    let stl = solid.to_stl_bytes();
    let id = store.mint(solid);
    Ok(Response::Rendered {
        id,
        stl,
        min: mn.to_array(),
        max: mx.to_array(),
        messages,
    })
}

/// Render a `.scad` source straight to a kernel [`Solid`] — the SHARED base-render core (W.3.30). This is
/// the same `eval_source` → `build_geo_cached` the GUI pool and web worker run through [`render_whole_svc`],
/// just handed back as a `Solid` instead of a stored handle + STL bytes. It's what the native CLI
/// (`fab make`/`slice`/`coupon`) renders through, so all three front-ends — GUI, web, CLI — share ONE
/// render path and OpenSCAD is gone from production (it survives only in the differential test oracle).
/// Always full-res (`preview=false`, `Quality::Final`): the headless CLI has no draft toggle. Fresh cache
/// per call — a one-shot render doesn't reuse subtrees.
#[cfg(feature = "kernel")]
pub fn render_source_to_solid(source: &Source, root: Option<&str>) -> Result<Solid> {
    use crate::backend::{GeoCache, ManifoldBackend, build_geo_cached};
    // W.3.37: the CLI (`fab make`) doesn't go through the service's Failed-response line plumbing, so read
    // the stamped editor line off the thread-local and prefix it here — `eval_source` set it on the record.
    ERR_LINE.set(None);
    let (tree, _src, messages) =
        eval_source(source, root, false, Quality::Final).map_err(|e| match ERR_LINE.take() {
            Some(l) => e.context(format!("line {l}")),
            None => e,
        })?;
    build_geo_cached(&tree, &ManifoldBackend, &mut GeoCache::new())
        .filter(|s| !s.is_empty())
        .with_context(|| {
            format!(
                "scad-rs rendered EMPTY geometry (no faces){}",
                empty_hint(&messages)
            )
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
    quality: Quality,
) -> Result<Response> {
    use crate::backend::{ManifoldBackend, build_geo_parts_cached};
    // Parts are the interactive view → always $preview=true; the quality (Draft/Final) rides the toggle.
    let (tree, src, messages) = eval_source(source, root, true, quality)?;
    let mut staged: Vec<StagedPart> = Vec::new();
    // Materialize the parts BEFORE the loop: the `&mut store.geo_cache` borrow must end before the body
    // touches `store.mint` (a for-loop's iterator temporaries otherwise live to the end of the loop).
    let parts = build_geo_parts_cached(&tree, &ManifoldBackend, &mut store.geo_cache);
    for solid in parts.into_iter().flatten().filter(|s| !s.is_empty()) {
        let Some((mn, mx)) = solid.bbox() else {
            continue;
        };
        let stl = solid.to_stl_bytes();
        let id = store.mint(solid);
        staged.push((id, stl, mn.to_array(), mx.to_array()));
    }
    ensure!(
        !staged.is_empty(),
        "scad-rs rendered EMPTY geometry (no parts){}",
        empty_hint(&messages)
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
        messages,
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

/// The web save-back's two mesh variants off a held FULL-RES base (W.5.6). ALWAYS 3MF (W.3.18): the
/// site wants a consistent 3MF variant, not STL-when-uncolored. Per-vertex color survives as a material
/// table when the solid carries it; an uncolored solid emits a plain-geometry 3MF. `high` is the whole
/// solid serialized as-is; `low` is a QEM-decimated copy at the `budget` triangle target (the
/// conditional skip inside `decimate_mesh` leaves an already-lean mesh alone). The whole model decimates
/// as ONE mesh — one coherent per-vertex color array, fully deterministic; the per-part rayon path
/// ([`crate::decimate::decimate_parts`]) is for callers that already hold disjoint parts.
fn save_meshes_svc(store: &SolidStore, base: SolidId, budget: u32) -> Result<Response> {
    let solid = store.get(base)?;
    let (verts, tris) = solid.to_indexed();
    let colors = solid.vertex_colors();
    let v: Vec<[f64; 3]> = verts.iter().map(|p| p.to_array()).collect();
    let t: Vec<[u32; 3]> = tris.iter().map(|tri| tri.indices()).collect();
    let c: Option<Vec<[f64; 4]>> =
        colors.map(|cs| cs.iter().map(|x| [x.r, x.g, x.b, x.a]).collect());
    let low_mesh = crate::decimate::decimate_mesh(&v, &t, c.as_deref(), budget as usize);

    Ok(Response::SavedMeshes {
        low: crate::threemf_out::to_3mf_bytes(
            &low_mesh.verts,
            &low_mesh.tris,
            low_mesh.colors.as_deref(),
        ),
        high: solid.to_3mf_bytes(),
        ext: "3mf".to_string(),
    })
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
            Response::SavedMeshes { .. } => "SavedMeshes",
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
        let Response::PartsRendered { parts, .. } = handle_with_store(
            &mut store,
            Request::RenderParts {
                source: Source::Path(src.to_string_lossy().into_owned()),
                root: None,
                quality: Quality::Draft,
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
                preview: true,
                quality: Quality::Draft,
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
                preview: true,
                quality: Quality::Draft,
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

    #[test]
    fn save_back_pipeline_through_the_wire() {
        // W.5.9 (in-process e2e): the EXACT worker path the browser Save drives — a full-res COLORED
        // render (preview=false, from source bytes) -> SaveMeshes off the held handle -> both 3MF —
        // through the bincode wire codec, on a persistent store like the worker's. The browser adds
        // only the multipart fetch on top of this (its own leg, W.5.9b). Runs under kernel-without-
        // native too (the wasm worker's config).
        let mut store = SolidStore::new(0);
        let src = b"$fn = $preview ? 6 : 40;\ncolor(\"red\") sphere(r = 10);".to_vec();

        let req = decode_request(&encode_request(&Request::RenderWhole {
            source: Source::Bytes {
                main: src,
                libs: vec![],
            },
            root: None,
            preview: false,
            quality: Quality::Final,
        }))
        .unwrap();
        let Response::Rendered { id, .. } = handle_with_store(&mut store, req) else {
            panic!("full-res render failed")
        };

        let req = decode_request(&encode_request(&Request::SaveMeshes {
            base: id,
            budget: 500,
        }))
        .unwrap();
        let Response::SavedMeshes { low, high, ext } = handle_with_store(&mut store, req) else {
            panic!("save-meshes failed")
        };
        assert_eq!(ext, "3mf", "a color()'d model saves BOTH variants as 3MF");
        assert_eq!(&high[..2], b"PK", "full-res is a 3MF OPC zip");
        assert_eq!(&low[..2], b"PK", "low-res is the SAME format");
        assert!(
            low.len() < high.len(),
            "decimated low-res ({}) < full-res ({})",
            low.len(),
            high.len()
        );
    }

    #[test]
    fn empty_render_names_the_missing_library() {
        // W.3.22: a model that includes BOSL2 with NO libs provided → every module undefined → empty.
        // The error must NAME the missing library, not just say "EMPTY" (the homepod_mount dogfood).
        let mut store = SolidStore::new(0);
        let src = Source::Bytes {
            main: b"include <BOSL2/std.scad>\ncyl(d=10, h=5);".to_vec(),
            libs: vec![],
        };
        let msg = match render_whole_svc(&mut store, &src, None, false, Quality::Draft) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("expected an empty-render error, got geometry"),
        };
        assert!(
            msg.contains("libraries not found") && msg.contains("BOSL2/std.scad"),
            "empty error should name the missing library, got: {msg}"
        );
    }

    #[test]
    fn bosl2_path_resolves_against_a_supplied_root() {
        // W.3.33 (inverse of `empty_render_names_the_missing_library`): the packed-lib-root fallback lets a
        // PASTED model render with no opened file. Prove the tail of that chain — a `Source::Path` sitting
        // OUTSIDE any workspace (temp dir), whose only libs come from `root/{libs,scad-lib}`, resolves BOSL2
        // and produces real geometry. `CARGO_MANIFEST_DIR` is the repo root here (fab_scad is the root
        // crate), the dev arm of what `fab::packed_lib_root` returns.
        let mut store = SolidStore::new(0);
        let dir = std::env::temp_dir().join(format!("fab-paste-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("paste.scad");
        std::fs::write(&path, b"include <BOSL2/std.scad>\ncyl(d=10, h=5);").unwrap();
        let src = Source::Path(path.to_string_lossy().into_owned());
        let ok = matches!(
            render_whole_svc(
                &mut store,
                &src,
                Some(env!("CARGO_MANIFEST_DIR")),
                false,
                Quality::Draft,
            ),
            Ok(Response::Rendered { .. })
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            ok,
            "BOSL2 cyl() should render when root supplies libs + scad-lib"
        );
    }

    #[test]
    fn web_svg_import_resolves_from_the_lib_pack() {
        // W.3.24: import("x.svg") on the bytes (web) path resolves the asset from the closure by BASENAME
        // (a model-relative "../x.svg" can't normalize against a rootless main). A 30x40 rect extruded →
        // a real solid, so the reader parsed the SVG rather than erroring as "not supported".
        let mut store = SolidStore::new(0);
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect x="10" y="20" width="30" height="40" fill="black"/></svg>"#;
        let src = Source::Bytes {
            main: b"linear_extrude(5) import(\"../FamilyLogo.svg\");".to_vec(),
            libs: vec![("FamilyLogo.svg".to_string(), svg.to_vec())],
        };
        let ok = matches!(
            render_whole_svc(&mut store, &src, None, false, Quality::Final),
            Ok(Response::Rendered { .. })
        );
        assert!(
            ok,
            "an SVG import resolved from the lib pack should render a solid"
        );
    }

    #[test]
    fn fab_owned_quality_drives_facet_count() {
        // W.3.25: with NO model $fn, fab's injected $fa/$fs sets tessellation — Final (fine) yields MORE
        // triangles than Draft (coarse) on a bare sphere. fab OWNS the quality.
        let src = Source::Bytes {
            main: b"sphere(r=10);".to_vec(),
            libs: vec![],
        };
        let tris = |q: Quality| -> u32 {
            let mut store = SolidStore::new(0);
            match render_whole_svc(&mut store, &src, None, true, q) {
                Ok(Response::Rendered { stl, .. }) => {
                    u32::from_le_bytes(stl[80..84].try_into().unwrap())
                }
                _ => panic!("render failed"),
            }
        };
        assert!(
            tris(Quality::Final) > tris(Quality::Draft),
            "Final ({}) should out-tessellate Draft ({})",
            tris(Quality::Final),
            tris(Quality::Draft)
        );
    }

    #[test]
    fn a_model_fn_overrides_fab_quality() {
        // W.3.25 graceful migration: a model that pins $fn wins over fab's injected default, so Draft and
        // Final tessellate IDENTICALLY — an un-migrated `$fn = …` model is unaffected.
        let src = Source::Bytes {
            main: b"$fn = 20;\nsphere(r=10);".to_vec(),
            libs: vec![],
        };
        let tris = |q: Quality| -> u32 {
            let mut store = SolidStore::new(0);
            match render_whole_svc(&mut store, &src, None, true, q) {
                Ok(Response::Rendered { stl, .. }) => {
                    u32::from_le_bytes(stl[80..84].try_into().unwrap())
                }
                _ => panic!("render failed"),
            }
        };
        assert_eq!(
            tris(Quality::Draft),
            tris(Quality::Final),
            "a model's own $fn overrides fab's quality"
        );
    }

    #[test]
    fn save_meshes_always_3mf_and_decimates_low() {
        // W.5.6 + W.3.18: ALWAYS 3MF now (color survives when present, geometry-only otherwise), low-res
        // decimated below the high-res. Runs by minting straight into the store (no .scad render), so it
        // holds under the wasm worker's kernel-without-native config too.
        use fab_lang::Rgba;
        let mut store = SolidStore::new(0);
        let sphere = Solid::sphere(10.0, 32).with_color(Rgba::opaque(1.0, 0.0, 0.0));
        let id = store.mint(sphere);
        let Response::SavedMeshes { low, high, ext } = handle_with_store(
            &mut store,
            Request::SaveMeshes {
                base: id,
                budget: 100,
            },
        ) else {
            panic!("save (colored) failed")
        };
        assert_eq!(ext, "3mf", "always 3MF");
        assert_eq!(&high[..2], b"PK", "3MF is an OPC zip");
        assert_eq!(&low[..2], b"PK", "low-res is the SAME format");
        assert!(
            low.len() < high.len(),
            "decimated low-res ({}) is smaller than full-res ({})",
            low.len(),
            high.len()
        );

        // An UNCOLORED base → 3MF too now (W.3.18, no more STL branch); a cube (12 tris) is under budget
        // → decimation skips → low is the same mesh as high (same serialized size).
        let cid = store.mint(Solid::cube(20.0, 20.0, 20.0, true));
        let Response::SavedMeshes { low, high, ext } = handle_with_store(
            &mut store,
            Request::SaveMeshes {
                base: cid,
                budget: 1000,
            },
        ) else {
            panic!("save (uncolored) failed")
        };
        assert_eq!(ext, "3mf", "uncolored is 3MF too now (W.3.18)");
        assert_eq!(&high[..2], b"PK", "uncolored 3MF is still an OPC zip");
        assert_eq!(&low[..2], b"PK", "low-res is 3MF too");
        assert_eq!(
            low.len(),
            high.len(),
            "under budget → decimation skipped → identical mesh, same 3MF size"
        );
    }

    #[test]
    fn render_whole_full_res_beats_preview() {
        // W.5.2: the same source, once at preview and once at full-res. A model that keys its facet
        // count off $preview (as BOSL2/library models do) must tessellate DENSER at full-res — that
        // heavier mesh is what the save-back serializes. Runs on the bytes path (no fs), so it also
        // guards the wasm worker's config.
        let src = b"$fn = $preview ? 6 : 48;\nsphere(r = 10);".to_vec();
        let render = |preview: bool| -> u32 {
            let mut store = SolidStore::new(0);
            let Response::Rendered { stl, .. } = handle_with_store(
                &mut store,
                Request::RenderWhole {
                    source: Source::Bytes {
                        main: src.clone(),
                        libs: vec![],
                    },
                    root: None,
                    preview,
                    quality: Quality::Draft,
                },
            ) else {
                panic!("render failed (preview={preview})")
            };
            // Binary STL triangle count lives in bytes 80..84.
            u32::from_le_bytes(stl[80..84].try_into().expect("stl header"))
        };
        let (lo, hi) = (render(true), render(false));
        assert!(lo > 0 && hi > 0, "both quality levels render ({lo}, {hi})");
        assert!(
            hi > lo,
            "full-res ({hi} tris) must exceed preview ({lo} tris)"
        );
    }
}
