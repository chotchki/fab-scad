//! Codegen for the slicing driver (5.2): turn a `[slicing]` spec into a `slice()`/connector
//! `.scad` that `fab slice` renders. Pure string-building — the IO (freeze source, render)
//! lives in `slice_cmd`. This is the GUI ↔ fab contract: the GUI edits the spec, this
//! reproduces the same SCAD headlessly, so preview and `fab slice` are one path.

#[cfg(feature = "native")]
use std::path::{Path, PathBuf};
#[cfg(feature = "native")]
use std::time::Duration;

use anyhow::{Context, Result, bail};

#[cfg(feature = "kernel")]
use crate::kernel::Solid;
use crate::manifest::{Connector, PartSlicing, Slicing};
#[cfg(feature = "native")]
use crate::openscad::Openscad;
use fab_lang::{Affine, Vec3};

const AXIS: [&str; 3] = ["RIGHT", "BACK", "UP"];

// --- onion orientation gate (#40) -----------------------------------------------------------
// Tunable; the geometric ideal is 45°, refined by a printed coupon (Phase A). See the
// connector-orientation design memory for the derivation.
const SUPPORT_ANGLE: f64 = 45.0; // overhang threshold, degrees from vertical
const CAP_ANG_MIN: f64 = 20.0; // pointiest onion cap we'll print (BOSL2 ang is from vertical)
const CAP_SAFETY: f64 = 0.0; // extra socket margin; 0 keeps the aligned case at today's ang=45

/// The shared onion cap axis + cap angle for one joint, or Infeasible (→ downgrade to a bolt).
#[derive(Debug, Clone, Copy, PartialEq)]
enum OnionAxis {
    Feasible { cap: Vec3, ang: f64 },
    Infeasible,
}

/// Derive the ONE shared onion cap axis + angle from the two bordering pieces' build-ups `u_lo`
/// (peg piece, below the cut) and `u_up` (socket piece, above). The onion is CUT IN HALF by the cut
/// plane — one half stands proud as the peg, the matching half is carved as the socket — so the CUT
/// axis is irrelevant to printability; only the cap-vs-build angle is. (chotchki: a Y-cut onion with
/// the cap along +Z, sliced in half on the Y plane, is the best possible use of the onion.)
/// - PEG (proud bump, lower piece): the cap follows `u_lo`, so the teardrop narrows going up and
///   prints support-free in ANY orientation. That fixes the cap; the peg never limits feasibility.
/// - SOCKET (cavity, upper piece): the cap is the void's CEILING. Fine when the cap tilts little off
///   +u_up (steepen `ang` to clear it); fine again when it points well AWAY (the cavity opens up — a
///   bowl, no ceiling). The band between is where the ceiling overhangs → downgrade to a bolt.
fn onion_axis(u_lo: Vec3, u_up: Vec3) -> OnionAxis {
    let cap = u_lo; // peg-priority: the proud bump follows the lower build, always support-free
    let tilt = cap.angle_deg(u_up); // socket-ceiling tilt off the upper build
    let budget = SUPPORT_ANGLE - CAP_ANG_MIN - CAP_SAFETY; // tilt the steepest printable cap absorbs
    if tilt >= 180.0 - budget {
        return OnionAxis::Feasible {
            cap,
            ang: SUPPORT_ANGLE,
        }; // cap points away → bowl, no ceiling
    }
    if tilt > budget {
        return OnionAxis::Infeasible; // ceiling overhangs even at the steepest printable cap
    }
    let ang = (SUPPORT_ANGLE - tilt - CAP_SAFETY).clamp(CAP_ANG_MIN, SUPPORT_ANGLE);
    OnionAxis::Feasible { cap, ang }
}

/// Cut positions grouped by axis, each ascending — the shared prep for the driver, per-piece
/// codegen, and the feasibility query (`slice()` and the slab math both need sorted cuts).
fn axes_sorted(s: &Slicing) -> Result<[Vec<f64>; 3]> {
    let mut by_axis: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for c in &s.cut {
        by_axis[c.axis_index()?].push(c.at());
    }
    for v in by_axis.iter_mut() {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    Ok(by_axis)
}

/// Per-connector onion feasibility under the spec's orientations, index-aligned with `s.connector`:
/// `true` = the onion prints support-free for both bordering pieces, `false` = its orientation gate
/// failed and `driver_scad` downgrades it to a bolt. Non-onion connectors are `true` (nothing to
/// downgrade). The GUI's joint-downgrade flag runs through THIS — same gate the slice applies, so
/// the flag the user sees and the joint the slice carves never disagree.
pub fn onion_feasibility(s: &Slicing) -> Result<Vec<bool>> {
    let by_axis = axes_sorted(s)?;
    s.connector
        .iter()
        .map(|c| {
            if c.kind != "onion" {
                return Ok(true);
            }
            Ok(matches!(
                onion_resolution(s, &by_axis, c)?,
                OnionAxis::Feasible { .. }
            ))
        })
        .collect()
}

/// Slab index of `coord` among `sorted_cuts` (cuts strictly below it). For a cut's own position
/// this is the LOWER piece's index on that axis; the upper piece is +1.
fn slab_index(sorted_cuts: &[f64], coord: f64) -> usize {
    sorted_cuts.iter().filter(|&&x| x < coord - 1e-6).count()
}

/// A piece's build-up, keyed by (slab, connected-COMPONENT) — U.3.14 Phase G. A component-specific
/// override wins (a presliced blob's comp `k` orients on its own); else the slab-level orient (comp 0)
/// applies to every component of that slab; else +Z (as-modeled). The onion/bolt carve queries comp 0
/// (a cut slab is one component), the 3mf co-pack queries each real component.
fn piece_up(s: &Slicing, mi: [usize; 3], comp: usize) -> Vec3 {
    let at = |c: usize| s.orient.iter().find(|p| p.piece == mi && p.comp == c);
    at(comp)
        .or_else(|| at(0))
        .map(|p| Vec3::new(p.up[0].f(), p.up[1].f(), p.up[2].f()).normalize())
        .unwrap_or(Vec3::new(0.0, 0.0, 1.0))
}

/// Resolve one onion connector to its cap axis/angle (or Infeasible) from its two bordering
/// pieces' orientations. `by_axis` holds the sorted enabled cuts per axis (for slab lookup).
fn onion_resolution(s: &Slicing, by_axis: &[Vec<f64>; 3], c: &Connector) -> Result<OnionAxis> {
    let cut = s.cut.get(c.cut).with_context(|| {
        format!(
            "connector references cut {}, but there are {} cut(s)",
            c.cut,
            s.cut.len()
        )
    })?;
    let axis = cut.axis_index()?;
    let others: Vec<usize> = (0..3).filter(|&x| x != axis).collect();
    let k = slab_index(&by_axis[axis], cut.at()); // lower piece's index on the cut axis
    let mut lo = [0usize; 3];
    lo[axis] = k;
    lo[others[0]] = slab_index(&by_axis[others[0]], c.pos[0].f());
    lo[others[1]] = slab_index(&by_axis[others[1]], c.pos[1].f());
    let mut up = lo;
    up[axis] = k + 1;
    // The onion joins two CUT slabs — each a single component — so gate on comp 0.
    Ok(onion_axis(piece_up(s, lo, 0), piece_up(s, up, 0)))
}

/// Freeze `source` to a mesh, generate the slicer driver from `spec`, render the pieces.
/// Returns the sliced STL path. The shared slice flow — `fab slice` and the GUI both call it.
#[cfg(feature = "native")]
pub fn slice_part(
    oscad: &Openscad,
    source: &Path,
    spec: &Slicing,
    spread: f64,
    out_dir: &Path,
    timeout: Duration,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // Freeze the source to a mesh (slicing the frozen STL stays linear — no 2^N).
    let source_stl = out_dir.join(format!("{stem}.stl"));
    let f = oscad.render(source, &source_stl, timeout)?;
    if !f.ok {
        bail!("source render failed: {}", source.display());
    }

    // Generate the driver from the spec (imports the frozen mesh by name) and render it.
    let driver = driver_scad(spec, &format!("{stem}.stl"), spread)?;
    let driver_path = out_dir.join(format!("{stem}-sliced.scad"));
    std::fs::write(&driver_path, driver)
        .with_context(|| format!("writing {}", driver_path.display()))?;
    let sliced = out_dir.join(format!("{stem}-sliced.stl"));
    let r = oscad.render(&driver_path, &sliced, timeout)?;
    if !r.ok {
        bail!("slice render failed");
    }
    Ok(sliced)
}

/// Like `slice_part`, but emits the pieces as SEPARATE objects in a multi-object `.3mf` (6.3) — the
/// printable plate. Same frozen mesh; a multipart driver rendered with lazy-union.
#[cfg(feature = "native")]
pub fn slice_part_3mf(
    oscad: &Openscad,
    source: &Path,
    spec: &Slicing,
    spread: f64,
    out_dir: &Path,
    timeout: Duration,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    let source_stl = out_dir.join(format!("{stem}.stl"));
    let f = oscad.render(source, &source_stl, timeout)?;
    if !f.ok {
        bail!("source render failed: {}", source.display());
    }

    let driver = multipart_driver_scad(spec, &format!("{stem}.stl"), spread)?;
    let driver_path = out_dir.join(format!("{stem}-multipart.scad"));
    std::fs::write(&driver_path, driver)
        .with_context(|| format!("writing {}", driver_path.display()))?;
    let out_3mf = out_dir.join(format!("{stem}.3mf"));
    let r = oscad.render_multipart(&driver_path, &out_3mf, timeout)?;
    if !r.ok {
        bail!("3mf render failed");
    }
    Ok(out_3mf)
}

/// Slice IN-PROCESS via the Manifold kernel (Track C 11.9) instead of the scad driver. OpenSCAD is
/// still the front-door — it renders the base model to a mesh ONCE — then import, slice, connectors and
/// export all happen in-process (no per-piece spawn). `plate = Some((bed, gap))` bin-packs the pieces
/// onto `bed`-sized plates and writes a Bambu 3mf (U.3.14 Phase E); `None` fans each piece by its slab
/// index × `spread` and writes one merged STL.
#[cfg(all(feature = "kernel", feature = "native"))]
pub fn slice_part_kernel(
    oscad: &Openscad,
    source: &Path,
    spec: &Slicing,
    spread: f64,
    out_dir: &Path,
    timeout: Duration,
    plate: Option<([f64; 2], f64)>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // The one OpenSCAD spawn: freeze the base model to a mesh.
    let source_stl = out_dir.join(format!("{stem}.stl"));
    let f = oscad.render(source, &source_stl, timeout)?;
    if !f.ok {
        bail!("source render failed: {}", source.display());
    }

    let base = Solid::from_stl_file(&source_stl)?;
    let pieces = slice_solid(spec, &base)?;
    if pieces.is_empty() {
        bail!("slice produced no pieces");
    }

    match plate {
        Some((bed, gap)) => {
            let mut to_place = Vec::new();
            collect_plate_pieces(&pieces, spec, &mut to_place);
            let out = out_dir.join(format!("{stem}.3mf"));
            crate::bambu::export_plates(&out, to_place, bed, gap)?;
            Ok(out)
        }
        None => {
            // Fan each piece by its slab multi-index × spread (0 = assembled in place), merge to one STL.
            let laid: Vec<Solid> = pieces
                .iter()
                .map(|(idx, s)| {
                    s.translate(Vec3::new(
                        idx[0] as f64 * spread,
                        idx[1] as f64 * spread,
                        idx[2] as f64 * spread,
                    ))
                })
                .collect();
            let out = out_dir.join(format!("{stem}-sliced.stl"));
            Solid::batch_union(&laid).write_stl(&out)?;
            Ok(out)
        }
    }
}

/// Slice pieces → [`bambu::PieceToPlace`](crate::bambu::PieceToPlace) for the bed-packer (U.3.14 Phase
/// E): each piece splits into its connected COMPONENTS (a presliced blob is many disjoint sub-solids —
/// pack each on its own), and each takes its build-up from `spec`'s per-piece orientation (a manual
/// override, else +Z as-modeled — `fab slice` HONORS the spec, it does not auto-orient; that's `fab
/// make`'s job).
#[cfg(all(feature = "kernel", feature = "native"))]
fn collect_plate_pieces(
    pieces: &[([usize; 3], Solid)],
    spec: &Slicing,
    to_place: &mut Vec<crate::bambu::PieceToPlace>,
) {
    for (slab, solid) in pieces {
        for (comp, csolid) in solid.components().into_iter().enumerate() {
            // Phase G: each component orients by its OWN (slab, comp) build-up — a per-component manual
            // override wins over the slab default, so a presliced blob's parts each lay flat on their own.
            let up = piece_up(spec, *slab, comp).to_array();
            let (v, t) = csolid.to_indexed();
            to_place.push(crate::bambu::PieceToPlace {
                mesh: crate::bambu::Mesh {
                    verts: v.iter().map(|p| p.to_array()).collect(),
                    tris: t.iter().map(|f| f.indices()).collect(),
                },
                up,
            });
        }
    }
}

/// One part's slice output for the co-pack: its part-local spec (drives per-piece orientation) + the
/// `(slab-index, solid)` pieces it cut into. Kept per part so the 3mf path orients each piece by ITS
/// part's own spec, not the model's.
#[cfg(all(feature = "kernel", feature = "native"))]
type PartPieces = (Slicing, Vec<([usize; 3], Solid)>);

/// PER-PART slice IN-PROCESS (U.3.14 Phase D) — the CLI twin of the GUI's per-part Parts workflow. The
/// split needs the evaluated tree (not one OpenSCAD-rendered whole mesh), so this is the scad-rs eval
/// path: evaluate `source` → [`build_geo_parts`] → bind each `[[slicing.part]]` block to a part via
/// [`resolve_part`] → `slice_solid` that part with its OWN cuts/connectors. A part with no block stays
/// whole (one piece). `plate = Some((bed, gap))` bin-packs the pieces onto `bed`-sized plates (a 3mf,
/// Phase E); `None` fans each by its slab index × `spread` to one merged STL — same shape as
/// [`slice_part_kernel`]. An unresolvable block is a hard error (a silent mis-slice is worse); a name
/// that misses but resolves by index WARNS.
#[cfg(all(feature = "kernel", feature = "native"))]
pub fn slice_model_parts(
    source: &Path,
    libs: &[PathBuf],
    spec: &Slicing,
    spread: f64,
    out_dir: &Path,
    plate: Option<([f64; 2], f64)>,
) -> Result<PathBuf> {
    use crate::backend::{ManifoldBackend, build_geo_parts, part_names, resolve_part};

    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    // Evaluate + split, exactly as the GUI's `render_parts` does (ManifoldBackend's solid is
    // `Option<Solid>` — the empty algebra — so flatten drops empties, then guard the rare Some(empty)).
    let tree = crate::import::resolve_geometry_file(source, libs, fab_lang::Config::from_env())
        .map_err(|e| anyhow::anyhow!("scad-rs eval of {}: {e}", source.display()))?;
    let parts: Vec<Solid> = build_geo_parts(&tree, &ManifoldBackend)
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    anyhow::ensure!(
        !parts.is_empty(),
        "scad-rs rendered EMPTY geometry (no parts) for {}",
        source.display()
    );

    // Provenance names, applied ONLY when the count matches the split (else the bind is ambiguous —
    // resolve_part then falls back to index against the nulled names).
    let names = part_names(source);
    let names = if names.len() == parts.len() {
        names
    } else {
        vec![None; parts.len()]
    };

    // Bind each block to a part index. Unresolvable → bail; name-miss-resolved-by-index → warn.
    let mut block_of: Vec<Option<&PartSlicing>> = vec![None; parts.len()];
    for ps in &spec.parts {
        let i = resolve_part(&names, &ps.key).with_context(|| {
            format!(
                "[[slicing.part]] key {:?} matches no part in {}",
                ps.key,
                source.display()
            )
        })?;
        if let Some(name) = &ps.key.name {
            let bound_by_name = names.get(i).and_then(|n| n.as_deref()) == Some(name.as_str());
            if !bound_by_name {
                eprintln!(
                    "warning: slicing part '{name}' not found by name — bound to part {i} by index"
                );
            }
        }
        block_of[i] = Some(ps);
    }

    // Slice each part with its own block (part-local cuts/connectors/orient), or keep it whole. Keep
    // each part's (spec, pieces) so the 3mf path can orient every piece by ITS OWN part's spec.
    let mut sliced: Vec<PartPieces> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        match block_of[i] {
            Some(ps) => {
                let part_spec = Slicing {
                    printer: None,
                    cut: ps.cut.clone(),
                    connector: ps.connector.clone(),
                    orient: ps.orient.clone(),
                    parts: vec![],
                };
                let pieces = slice_solid(&part_spec, part)?;
                sliced.push((part_spec, pieces));
            }
            // No block → the whole part is one piece (slab origin), sliced with an empty spec.
            None => sliced.push((
                Slicing::default(),
                vec![([0, 0, 0], part.transform(&Affine::IDENTITY))],
            )),
        }
    }

    match plate {
        Some((bed, gap)) => {
            // Bed-pack: orient each piece (per component) by its part's spec, co-pack all parts' pieces.
            let mut to_place = Vec::new();
            for (part_spec, pieces) in &sliced {
                collect_plate_pieces(pieces, part_spec, &mut to_place);
            }
            anyhow::ensure!(!to_place.is_empty(), "slice produced no pieces");
            let out = out_dir.join(format!("{stem}.3mf"));
            crate::bambu::export_plates(&out, to_place, bed, gap)?;
            Ok(out)
        }
        None => {
            // Merged STL: fan each piece by its slab multi-index × spread, pooled across all parts.
            let mut laid: Vec<Solid> = Vec::new();
            for (_, pieces) in &sliced {
                for (idx, s) in pieces {
                    laid.push(s.translate(Vec3::new(
                        idx[0] as f64 * spread,
                        idx[1] as f64 * spread,
                        idx[2] as f64 * spread,
                    )));
                }
            }
            anyhow::ensure!(!laid.is_empty(), "slice produced no pieces");
            let out = out_dir.join(format!("{stem}-sliced.stl"));
            Solid::batch_union(&laid).write_stl(&out)?;
            Ok(out)
        }
    }
}

/// Format a coordinate without a trailing `.0` for whole numbers (tidy generated SCAD).
fn n(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Generate the driver: nested `slice()` per axis around a `diff()` of the imported source
/// minus the connectors. `source` is the import path, relative to the driver file.
pub fn driver_scad(s: &Slicing, source: &str, spread: f64) -> Result<String> {
    let by_axis = axes_sorted(s)?; // slice() requires ascending cuts
    let chain = slice_chain(s, &by_axis, spread, None)?;
    if chain.is_empty() {
        bail!("[slicing] has no cuts");
    }
    let body = part_body(s, source, &by_axis)?;
    // slice() calls have no trailing `;`, so `body` (the diff) is the innermost CHILD — the geometry
    // every piece is carved from. slice()'s for-loop unions the pieces into one manifold → one STL.
    Ok(format!(
        "// generated by `fab slice` from project.toml [slicing] — edits go in the spec, not here.\n\
         include <slicer.scad>\n\
         include <connectors.scad>\n\n\
         {chain}{body}\n"
    ))
}

/// The multipart-3mf driver (6.3): each piece emitted as its OWN top-level `slice(..., only = i)`
/// statement so lazy-union keeps them SEPARATE objects on the plate — vs `driver_scad`, where slice()
/// unions the whole for-loop into one manifold. Renders with `--enable=lazy-union`.
pub fn multipart_driver_scad(s: &Slicing, source: &str, spread: f64) -> Result<String> {
    let by_axis = axes_sorted(s)?;
    let counts: Vec<usize> = by_axis
        .iter()
        .filter(|c| !c.is_empty())
        .map(|c| c.len() + 1)
        .collect();
    if counts.is_empty() {
        bail!("[slicing] has no cuts");
    }
    let body = part_body(s, source, &by_axis)?;
    // One statement per piece: the nested slice() chain with `only=` fixed per axis, carving `_part()`.
    let mut pieces = String::new();
    for combo in piece_combos(&counts) {
        pieces += &slice_chain(s, &by_axis, spread, Some(&combo))?;
        pieces += "    _part();\n";
    }
    Ok(format!(
        "// generated by `fab slice --3mf` — pieces as SEPARATE objects on the plate (lazy-union).\n\
         include <slicer.scad>\n\
         include <connectors.scad>\n\n\
         module _part() {body}\n\n\
         {pieces}"
    ))
}

/// The geometry every piece is carved from: `tag_scope() diff() { import(frozen mesh); bolts }`.
/// `force_tag()` pulls the raw `import()` mesh into BOSL2's tag system — without it `diff()` doesn't
/// see the import as keep-geometry and the connectors don't carve (BOSL2 primitives self-tag; import
/// does not). Feasible onions ride slice()'s `connectors` param, so only bolts (and downgraded onions,
/// which `connector_line` renders as bolts) land here.
fn part_body(s: &Slicing, source: &str, by_axis: &[Vec<f64>; 3]) -> Result<String> {
    let mut body = String::from("tag_scope() diff() {\n");
    body += &format!("    force_tag() import(\"{source}\");\n");
    for c in &s.connector {
        let feasible_onion = c.kind == "onion"
            && matches!(onion_resolution(s, by_axis, c)?, OnionAxis::Feasible { .. });
        if !feasible_onion {
            body += &connector_line(s, c)?;
        }
    }
    body += "}";
    Ok(body)
}

/// The nested `slice(...)` chain over the axes that have cuts. `only = Some(idx)` fixes one piece per
/// axis (a single multipart object, `idx` indexed over active axes in order); `None` slices every
/// piece at once (the unioned STL driver). Ends with a newline; the caller appends the child geometry.
fn slice_chain(
    s: &Slicing,
    by_axis: &[Vec<f64>; 3],
    spread: f64,
    only: Option<&[usize]>,
) -> Result<String> {
    let mut chain = String::new();
    let mut active = 0; // index into `only`, counting only axes that have cuts
    for (ax, cuts) in by_axis.iter().enumerate() {
        if cuts.is_empty() {
            continue;
        }
        let list = cuts.iter().map(|&x| n(x)).collect::<Vec<_>>().join(", ");
        let onions = onion_param(s, ax, by_axis)?;
        let only_arg = match only {
            Some(idx) => format!(", only = {}", idx[active]),
            None => String::new(),
        };
        chain += &format!(
            "slice([{list}], axis = {}, spread = {}, connectors = {onions}{only_arg})\n",
            AXIS[ax],
            n(spread)
        );
        active += 1;
    }
    Ok(chain)
}

/// The cartesian product of per-axis piece indices — every `[i]`, `[i,j]`, … multi-index across the
/// axes that have cuts. `[2, 3]` (2 pieces on one axis, 3 on another) → the 6 grid pieces.
fn piece_combos(counts: &[usize]) -> Vec<Vec<usize>> {
    let mut combos = vec![vec![]];
    for &c in counts {
        combos = combos
            .iter()
            .flat_map(|combo| {
                (0..c).map(move |i| {
                    let mut e = combo.clone();
                    e.push(i);
                    e
                })
            })
            .collect();
    }
    combos
}

/// Every piece's slab multi-index: the cartesian product of `0..(cuts_on_axis + 1)` per axis (an
/// axis with no cuts contributes only index 0). Iteration is x-outer → z-inner, and each axis runs
/// in ascending-cut order — the SAME order `piece_driver` sorts by — so a returned `[ix, iy, iz]`
/// selects exactly the slab that `piece_driver`/`slice(only=)` would carve. Feeds the per-piece
/// render sweep (auto-orient #42, print-orientation preview).
pub fn piece_indices(s: &Slicing) -> Result<Vec<[usize; 3]>> {
    let mut slabs = [1usize; 3]; // an uncut axis is one slab
    for c in &s.cut {
        slabs[c.axis_index()?] += 1;
    }
    let mut out = Vec::with_capacity(slabs[0] * slabs[1] * slabs[2]);
    for ix in 0..slabs[0] {
        for iy in 0..slabs[1] {
            for iz in 0..slabs[2] {
                out.push([ix, iy, iz]);
            }
        }
    }
    Ok(out)
}

/// Codegen for ONE piece (no spread): nested `slice(only=)` per axis around the imported source,
/// each carrying its axis's FEASIBLE onions so the piece shows its real joints — the peg unioned in
/// when this piece is below a connector's cut, the socket diffed out when above (the slicer decides
/// per piece). `s` with no connectors gives the bare piece (auto-orient overhang scoring #42); `s`
/// with onions gives the print-orientation preview's joined piece. `piece` is the slab multi-index;
/// an axis with no cuts must be index 0.
pub fn piece_driver(s: &Slicing, source: &str, piece: [usize; 3]) -> Result<String> {
    let by_axis = axes_sorted(s)?;
    let mut slices = String::new();
    for (ax, cuts) in by_axis.iter().enumerate() {
        if cuts.is_empty() {
            if piece[ax] != 0 {
                bail!(
                    "piece index {} on axis {ax} but that axis has no cuts",
                    piece[ax]
                );
            }
            continue;
        }
        let list = cuts.iter().map(|&x| n(x)).collect::<Vec<_>>().join(", ");
        let onions = onion_param(s, ax, &by_axis)?;
        slices += &format!(
            "slice([{list}], axis = {}, only = {}, connectors = {onions})\n",
            AXIS[ax], piece[ax]
        );
    }
    if slices.is_empty() {
        bail!("[slicing] has no cuts");
    }
    Ok(format!(
        "// generated by `fab` for a single piece (orientation / print preview render).\n\
         include <slicer.scad>\n\n\
         {slices}import(\"{source}\");\n"
    ))
}

/// The FEASIBLE onion connectors on `axis` as a SCAD list `[[cut_pos, a, b, d, ox, oy, oz, ang],
/// ...]` for `slice()`'s `connectors` param — applied per piece (peg into the lower, socket out of
/// the upper). `(ox,oy,oz)` is the cap axis + `ang` its cap angle, DERIVED per joint from the two
/// bordering pieces' print orientations (`onion_axis`). Infeasible onions are omitted here and
/// downgraded to a bolt in the diff body. `by_axis` = sorted enabled cuts per axis, for piece lookup.
fn onion_param(s: &Slicing, axis: usize, by_axis: &[Vec<f64>; 3]) -> Result<String> {
    let mut items = Vec::new();
    for c in s.connector.iter().filter(|c| c.kind == "onion") {
        let cut = s.cut.get(c.cut).with_context(|| {
            format!(
                "connector references cut {}, but there are {} cut(s)",
                c.cut,
                s.cut.len()
            )
        })?;
        if cut.axis_index()? != axis {
            continue;
        }
        if let OnionAxis::Feasible { cap, ang } = onion_resolution(s, by_axis, c)? {
            items.push(format!(
                "[{}, {}, {}, {}, {}, {}, {}, {}]",
                n(cut.at()),
                n(c.pos[0].f()),
                n(c.pos[1].f()),
                n(c.size.unwrap_or(10.0)),
                n(cap[0]),
                n(cap[1]),
                n(cap[2]),
                n(ang)
            ));
        }
    }
    Ok(format!("[{}]", items.join(", ")))
}

/// One `tag("remove") <connector>` line, positioned on its cut plane and oriented along the
/// cut axis (so it slices into both pieces correctly).
fn connector_line(s: &Slicing, c: &Connector) -> Result<String> {
    let cut = s.cut.get(c.cut).with_context(|| {
        format!(
            "connector references cut {}, but there are {} cut(s)",
            c.cut,
            s.cut.len()
        )
    })?;
    let ai = cut.axis_index()?;
    // Point = `at` along the cut axis, `pos` in the two perpendicular dims.
    let mut p = [0.0_f64; 3];
    p[ai] = cut.at();
    let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();
    p[others[0]] = c.pos[0].f();
    p[others[1]] = c.pos[1].f();

    let conn = match c.kind.as_str() {
        // An onion that can't print support-free for both pieces downgrades to a bolt here
        // (its halves orient independently) — chotchki's pick for the infeasible case.
        "bolt" | "onion" => format!(
            "bolt_joint(\"{}\", through = {}, orient = {})",
            c.screw.as_deref().unwrap_or("M3"),
            n(c.through.unwrap_or(12.0)),
            AXIS[ai]
        ),
        other => bail!("connector type must be 'bolt' or 'onion', got '{other}'"),
    };
    Ok(format!(
        "    translate([{}, {}, {}]) tag(\"remove\") {conn};\n",
        n(p[0]),
        n(p[1]),
        n(p[2])
    ))
}

/// Bolt-clearance dims by screw, mirroring connectors.scad `_insert_spec` (+ a shaft clearance and
/// head counterbore): `(clearance_d, counterbore_d, counterbore_h, insert_d, insert_depth)`. The
/// through-depth is NOT here — `slice_solid` binds it to the above-slab thickness per placement.
#[cfg(feature = "kernel")]
fn bolt_dims(screw: Option<&str>) -> (f64, f64, f64, f64, f64) {
    match screw.unwrap_or("M3") {
        "M4" => (4.5, 8.0, 4.0, 6.0, 6.0),
        "M5" => (5.5, 10.0, 5.0, 7.0, 10.0),
        _ => (3.4, 6.0, 3.0, 5.0, 6.0), // M3 default
    }
}

/// Slice `base` into finished pieces IN-PROCESS (Track C 11.7) — the kernel twin of `piece_driver`.
/// Each cell is carved by the slab slicer, then its connectors are applied: a feasible onion UNIONs
/// its peg into the below-cell and DIFFs its slop-grown socket from the above-cell; an infeasible
/// onion (and any bolt) DIFFs a bolt clearance from both. A connector only touches the two cells it
/// actually borders — matched by cut-axis slab AND perpendicular slab index — so the two-axis onion
/// floater the scad path grew CAN'T happen and nothing is trimmed. Returns each non-empty piece with
/// its slab multi-index (piece_indices order).
#[cfg(feature = "kernel")]
pub fn slice_solid(s: &Slicing, base: &Solid) -> Result<Vec<([usize; 3], Solid)>> {
    const SEG: i32 = 48; // connector circular resolution
    const SLOP: f64 = 0.2; // socket grows the onion by this (matches onion_socket)

    let by_axis = axes_sorted(s)?;
    let (_bmin, bmax) = base.bbox().context("slicing an empty base solid")?;

    // A connector's shape + the two cells it bridges.
    enum Shape {
        Onion {
            cap: Vec3,
            ang: f64,
            d: f64,
        },
        Bolt {
            axis: Vec3,
            screw: Option<String>,
            through: f64,
        },
    }
    struct Placed {
        below: [usize; 3],
        above: [usize; 3],
        point: Vec3,
        shape: Shape,
    }
    // Slab index on `axis` that a coordinate falls into = cuts strictly below it.
    let slab_of =
        |axis: usize, coord: f64| by_axis[axis].iter().filter(|&&x| x < coord - 1e-9).count();

    let mut placed = Vec::with_capacity(s.connector.len());
    for c in &s.connector {
        let cut = s.cut.get(c.cut).with_context(|| {
            format!(
                "connector references cut {}, but there are {} cut(s)",
                c.cut,
                s.cut.len()
            )
        })?;
        let ai = cut.axis_index()?;
        let at = cut.at();
        let others: Vec<usize> = (0..3).filter(|&a| a != ai).collect();

        let mut point = [0.0; 3];
        point[ai] = at;
        point[others[0]] = c.pos[0].f();
        point[others[1]] = c.pos[1].f();

        // Below/above cells: same perpendicular slab, adjacent across the cut on `ai`.
        let mut below = [0usize; 3];
        for (m, &aj) in others.iter().enumerate() {
            below[aj] = slab_of(aj, c.pos[m].f());
        }
        below[ai] = slab_of(ai, at);
        let mut above = below;
        above[ai] = below[ai] + 1;

        let d = c.size.unwrap_or(10.0);
        let mut axis_unit = [0.0; 3];
        axis_unit[ai] = 1.0;
        // Above-slab thickness on the cut axis (+ a hair for a clean boolean exit): the shaft spans
        // exactly this piece so the head counterbore seats at its outer face, not a fixed depth.
        let above_top = by_axis[ai]
            .iter()
            .copied()
            .find(|&x| x > at + 1e-9)
            .unwrap_or(bmax[ai]);
        let through = above_top - at + 0.02;
        let shape = if c.kind == "onion" {
            match onion_resolution(s, &by_axis, c)? {
                OnionAxis::Feasible { cap, ang } => Shape::Onion { cap, ang, d },
                OnionAxis::Infeasible => Shape::Bolt {
                    axis: Vec3::from_array(axis_unit),
                    screw: c.screw.clone(),
                    through,
                },
            }
        } else {
            Shape::Bolt {
                axis: Vec3::from_array(axis_unit),
                screw: c.screw.clone(),
                through,
            }
        };
        placed.push(Placed {
            below,
            above,
            point: Vec3::from_array(point),
            shape,
        });
    }

    let mut out = Vec::new();
    for (piece, mut cell) in base.slab_pieces(&by_axis) {
        for p in &placed {
            let at = |sol: Solid| sol.translate(p.point);
            match &p.shape {
                Shape::Onion { cap, ang, d } => {
                    if piece == p.below {
                        cell = cell.union(&at(Solid::onion(*d, *ang, SEG).align_z_to(*cap)));
                    } else if piece == p.above {
                        let socket = Solid::onion(*d + 2.0 * SLOP, *ang, SEG).align_z_to(*cap);
                        cell = cell.difference(&at(socket));
                    }
                }
                Shape::Bolt {
                    axis,
                    screw,
                    through,
                } => {
                    if piece == p.below || piece == p.above {
                        let (cl, cb_d, cb_h, ins_d, ins_h) = bolt_dims(screw.as_deref());
                        // Teardrop the hole when THIS piece prints it >45° off vertical (the build-up's
                        // component ⟂ the bolt axis), so the ceiling self-supports; aim the peak at the
                        // build-up via a full basis. A near-vertical hole needs none → plain cylinder.
                        let up = piece_up(s, piece, 0); // a bolt sits on a cut slab (one component)
                        let peak = up - (*axis * up.dot(*axis));
                        let teardrop = peak.length() > 0.707;
                        let bolt = Solid::bolt_clearance(
                            cl, *through, cb_d, cb_h, ins_d, ins_h, SEG, teardrop,
                        );
                        let oriented = if teardrop {
                            let zc = axis.normalize();
                            let yc = peak.normalize();
                            let xc = yc.cross(zc);
                            bolt.transform(&Affine::from_column_major([
                                xc[0], xc[1], xc[2], yc[0], yc[1], yc[2], zc[0], zc[1], zc[2], 0.0,
                                0.0, 0.0,
                            ]))
                        } else {
                            bolt.align_z_to(*axis)
                        };
                        cell = cell.difference(&at(oriented));
                    }
                }
            }
        }
        if !cell.is_empty() {
            out.push((piece, cell));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn spec(toml: &str) -> Slicing {
        let m: Manifest = ::toml::from_str(toml).unwrap();
        m.slicing.unwrap()
    }

    #[test]
    fn cuts_group_by_axis_and_sort() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        assert!(
            d.contains("slice([-10, 25], axis = RIGHT, spread = 0, connectors = [])"),
            "{d}"
        );
        // force_tag() is load-bearing: without it diff() won't carve connectors from the import.
        assert!(d.contains("force_tag() import(\"t.stl\")"), "{d}");
        assert!(d.contains("tag_scope() diff()"));
    }

    #[test]
    fn piece_combos_are_the_cartesian_product() {
        assert_eq!(piece_combos(&[2]), vec![vec![0], vec![1]]);
        assert_eq!(piece_combos(&[2, 3]).len(), 6); // a 2×3 grid
        assert!(piece_combos(&[2, 3]).contains(&vec![1, 2]));
    }

    #[test]
    fn multipart_driver_emits_one_object_per_piece() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.cut]]\naxis=\"x\"\nat=40\n",
        );
        let d = multipart_driver_scad(&s, "t.stl", 30.0).unwrap();
        // 2 cuts → 3 pieces → 3 top-level `slice(..., only = i) _part();` statements (separate objects).
        assert_eq!(d.matches("_part();").count(), 3, "{d}");
        assert!(
            d.contains("only = 0") && d.contains("only = 1") && d.contains("only = 2"),
            "{d}"
        );
        assert!(d.contains("module _part()"), "{d}");
    }

    #[test]
    fn piece_up_prefers_component_then_slab_then_z() {
        use crate::manifest::PieceOrient;
        use crate::num::Num;
        let f = |x: f64| Num::Float(x);
        let s = Slicing {
            printer: None,
            cut: vec![],
            connector: vec![],
            orient: vec![
                PieceOrient {
                    piece: [0, 0, 0],
                    comp: 0,
                    up: [f(0.0), f(1.0), f(0.0)],
                }, // slab default: +Y
                PieceOrient {
                    piece: [0, 0, 0],
                    comp: 2,
                    up: [f(1.0), f(0.0), f(0.0)],
                }, // comp 2 override: +X
            ],
            parts: vec![],
        };
        assert_eq!(piece_up(&s, [0, 0, 0], 2).to_array(), [1.0, 0.0, 0.0]); // comp-specific wins
        assert_eq!(piece_up(&s, [0, 0, 0], 1).to_array(), [0.0, 1.0, 0.0]); // no comp 1 → slab default
        assert_eq!(piece_up(&s, [9, 9, 9], 0).to_array(), [0.0, 0.0, 1.0]); // no orient → +Z
    }

    #[cfg(all(feature = "kernel", feature = "native"))]
    #[test]
    fn slice_model_parts_cuts_the_bound_part_and_keeps_the_rest_whole() {
        use crate::manifest::{Cut, PartKey, PartSlicing};
        use crate::num::Num;
        let dir = std::env::temp_dir().join("fab_slice_model_parts_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("m.scad");
        // Two top-level parts (implicit union → 2 parts): a tall box to cut, a small cube left whole.
        std::fs::write(&src, "cube([20,20,60]);\ntranslate([50,0,0]) cube(10);\n").unwrap();

        let block = |parts| Slicing {
            printer: None,
            cut: vec![],
            connector: vec![],
            orient: vec![],
            parts,
        };
        let spec = block(vec![PartSlicing {
            key: PartKey {
                name: None,
                nth: 0,
                index: 0, // bind part 0 (the tall box) by index
            },
            cut: vec![Cut {
                axis: "z".into(),
                at: Num::Float(30.0), // midway → 2 pieces
            }],
            connector: vec![],
            orient: vec![],
        }]);
        let out = slice_model_parts(&src, &[], &spec, 0.0, &dir.join("out"), None).unwrap();
        assert!(out.exists(), "produced a sliced STL");
        assert_eq!(out.extension().unwrap(), "stl");
        assert!(
            std::fs::metadata(&out).unwrap().len() > 100,
            "non-trivial STL"
        );

        // --3mf plate: same parts bin-packed onto a bed → a 3mf (Phase E). 256mm bed fits both pieces.
        let out3mf = slice_model_parts(
            &src,
            &[],
            &spec,
            0.0,
            &dir.join("out"),
            Some(([256.0, 256.0], 5.0)),
        )
        .unwrap();
        assert_eq!(out3mf.extension().unwrap(), "3mf");
        assert!(
            std::fs::metadata(&out3mf).unwrap().len() > 100,
            "non-trivial 3mf"
        );

        // An unresolvable block (bad name AND out-of-range index) is a HARD error — never a silent skip.
        let bad = block(vec![PartSlicing {
            key: PartKey {
                name: Some("nope".into()),
                nth: 0,
                index: 99,
            },
            cut: vec![],
            connector: vec![],
            orient: vec![],
        }]);
        assert!(slice_model_parts(&src, &[], &bad, 0.0, &dir.join("out"), None).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(feature = "kernel")]
    #[test]
    fn slice_solid_places_pegs_only_on_owning_cells() {
        // The two-axis floater regression: cut X@0 and Y@0, one onion on the X-cut at y=+15.
        let base = Solid::cube(40.0, 40.0, 40.0, true);
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.cut]]\naxis=\"y\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[15,0]\nsize=10\n",
        );
        let pieces = slice_solid(&s, &base).unwrap();
        assert_eq!(pieces.len(), 4, "2×2 cells");
        let get = |idx: [usize; 3]| {
            pieces
                .iter()
                .find(|(p, _)| *p == idx)
                .map(|(_, s)| s)
                .unwrap()
        };

        // The owning below-cell (x<0, y>0) grows the peg — it stands proud past the cut on +X.
        let owning = get([0, 1, 0]);
        owning.check().unwrap();
        assert!(
            owning.bbox().unwrap().1[0] > 1.0,
            "peg should stand proud +X on the owning cell"
        );

        // The non-owning cell (x<0, y<0) must NOT get a floating peg — its +X edge stays at the cut.
        let other = get([0, 0, 0]);
        other.check().unwrap();
        assert!(
            other.bbox().unwrap().1[0] < 0.01,
            "floater leaked: {:?}",
            other.bbox().unwrap()
        );
    }

    #[cfg(feature = "kernel")]
    #[test]
    fn bolt_teardrop_carves_valid_pieces() {
        // Cut X@0 with a bolt on it. The default build-up (+Z) is ⟂ the X bolt axis, so the hole
        // prints horizontal → teardropped (peak +Z) and through-depth bound to the 20mm half-slab.
        // Both pieces must survive the teardrop + basis-transform + diff as valid manifolds.
        let base = Solid::cube(40.0, 40.0, 40.0, true);
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"bolt\"\nscrew=\"M4\"\npos=[0,0]\n",
        );
        let pieces = slice_solid(&s, &base).unwrap();
        assert_eq!(pieces.len(), 2, "one cut → two pieces");
        for (idx, p) in &pieces {
            p.check()
                .unwrap_or_else(|e| panic!("piece {idx:?} not manifold after bolt carve: {e}"));
        }
        // The bolt carved material out of each piece (a solid 20×40×40 half is 32000 mm³).
        let vol = |idx: [usize; 3]| pieces.iter().find(|(p, _)| *p == idx).unwrap().1.num_tri();
        assert!(
            vol([0, 0, 0]) > 12 && vol([1, 0, 0]) > 12,
            "both halves carved (not bare boxes)"
        );
    }

    #[test]
    fn bolt_connector_positioned_and_oriented() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"bolt\"\nscrew=\"M4\"\npos=[5,-3]\nthrough=15\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        // cut on Z -> point is (pos.x, pos.y, at), oriented UP
        assert!(
            d.contains("translate([5, -3, 0]) tag(\"remove\") bolt_joint(\"M4\", through = 15, orient = UP)"),
            "{d}"
        );
    }

    fn deg(d: f64) -> f64 {
        d.to_radians()
    }

    #[test]
    fn onion_axis_aligned_case_matches_today() {
        // both pieces build +Z: cap = +Z, ang = 45 (identical to pre-orientation output).
        match onion_axis(Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 1.0)) {
            OnionAxis::Feasible { cap, ang } => {
                assert!((cap[2] - 1.0).abs() < 1e-9 && cap[0].abs() < 1e-9);
                assert!((ang - 45.0).abs() < 1e-9);
            }
            _ => panic!("aligned case must be feasible"),
        }
    }

    #[test]
    fn onion_infeasible_when_the_two_pieces_build_90_apart() {
        // peg piece builds +X, socket piece builds +Z: no single cap serves both — the socket
        // ceiling sits at a 90° overhang. The CUT axis is irrelevant; only the build mismatch is.
        assert_eq!(
            onion_axis(Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            OnionAxis::Infeasible
        );
    }

    #[test]
    fn onion_socket_steepens_cap_for_a_tilted_upper_piece() {
        // upper piece tilted 20° from the lower's +Z. cap stays +Z (the peg), ang shrinks to clear it.
        let u_up = Vec3::new(deg(20.0).sin(), 0.0, deg(20.0).cos());
        match onion_axis(Vec3::new(0.0, 0.0, 1.0), u_up) {
            OnionAxis::Feasible { ang, .. } => assert!((ang - 25.0).abs() < 0.5, "ang {ang}"),
            _ => panic!("20° upper tilt should be feasible with a steeper cap"),
        }
        // 30° upper tilt exceeds the cap budget (45-CAP_ANG_MIN=25) -> infeasible.
        let steep = Vec3::new(deg(30.0).sin(), 0.0, deg(30.0).cos());
        assert_eq!(
            onion_axis(Vec3::new(0.0, 0.0, 1.0), steep),
            OnionAxis::Infeasible
        );
    }

    #[test]
    fn onion_socket_bowl_up_is_always_feasible() {
        // upper piece builds opposite the lower (-Z vs +Z): the socket opens upward, no ceiling.
        match onion_axis(Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0)) {
            OnionAxis::Feasible { ang, .. } => assert!((ang - 45.0).abs() < 1e-9),
            _ => panic!("bowl-up socket must be feasible"),
        }
    }

    #[test]
    fn onion_on_x_cut_default_up_is_feasible() {
        // X cut, both pieces default +Z: the onion is sliced in half on the cut plane, cap +Z, and
        // prints support-free — the cut axis doesn't matter. It rides slice()'s connectors, no bolt.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = driver_scad(&s, "t.stl", 30.0).unwrap();
        assert!(
            d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"),
            "onion rides the slice: {d}"
        );
        assert!(
            !d.contains("bolt_joint("),
            "feasible onion is NOT downgraded: {d}"
        );
    }

    #[test]
    fn orientation_override_on_the_lower_piece_can_force_a_downgrade() {
        // Z cut; override the LOWER piece [0,0,0] to build +X while the upper stays +Z -> the two
        // pieces build 90° apart, no shared cap -> downgrade. Exercises the override -> piece_up ->
        // slab-lookup -> gate path.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=12\n\
             [[slicing.orient]]\npiece=[0,0,0]\nup=[1,0,0]\n",
        );
        let d = driver_scad(&s, "t.stl", 0.0).unwrap();
        assert!(
            d.contains("connectors = []"),
            "override forces infeasible: {d}"
        );
        assert!(d.contains("bolt_joint("), "downgraded to bolt: {d}");
    }

    #[test]
    fn onion_rides_the_slice_param_not_the_diff() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = driver_scad(&s, "t.stl", 30.0).unwrap();
        // Z cut -> others (x,y); onion enters slice()'s connectors param as [at,a,b,d,ox,oy,oz,ang],
        // cap axis = the cut axis (+Z) at Phase B.
        assert!(
            d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"),
            "{d}"
        );
        // ...and is NOT emitted as a pre-slice remove in the diff body.
        assert!(!d.contains("onion_"), "{d}");
    }

    #[test]
    fn piece_driver_isolates_one_slab_per_axis() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n",
        );
        let d = piece_driver(&s, "m.stl", [1, 0, 1]).unwrap();
        // bare spec -> per-axis slice(only=) with no onions
        assert!(
            d.contains("slice([-10, 25], axis = RIGHT, only = 1, connectors = [])"),
            "{d}"
        );
        assert!(
            d.contains("slice([0], axis = UP, only = 1, connectors = [])"),
            "{d}"
        );
        assert!(d.contains("import(\"m.stl\")"), "{d}");
        // an axis with no cuts must be index 0
        assert!(piece_driver(&s, "m.stl", [0, 1, 0]).is_err());
    }

    #[test]
    fn piece_driver_carves_a_feasible_onion_into_the_piece() {
        // Z cut + onion, both pieces default +Z -> feasible. The piece slice carries the onion so
        // the slicer unions the peg (lower) / diffs the socket (upper); the preview shows the joint.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[5,-3]\nsize=12\n",
        );
        let d = piece_driver(&s, "m.stl", [0, 0, 0]).unwrap();
        assert!(
            d.contains("connectors = [[0, 5, -3, 12, 0, 0, 1, 45]]"),
            "{d}"
        );
    }

    #[test]
    fn onion_feasibility_flags_the_downgrade() {
        // Both pieces default +Z, so an onion is feasible on ANY cut (Z and X alike) — sliced in
        // half on the cut plane, cap +Z, support-free -> [true, true], index-aligned with the list.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=10\n\
             [[slicing.connector]]\ncut=1\ntype=\"onion\"\npos=[0,0]\nsize=10\n",
        );
        assert_eq!(onion_feasibility(&s).unwrap(), vec![true, true]);
        // The downgrade now comes from a build MISMATCH: an override that tilts the lower piece 90°
        // off the upper leaves no shared cap.
        let tilted = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"onion\"\npos=[0,0]\nsize=10\n\
             [[slicing.orient]]\npiece=[0,0,0]\nup=[1,0,0]\n",
        );
        assert_eq!(onion_feasibility(&tilted).unwrap(), vec![false]);
    }

    #[test]
    fn piece_indices_are_the_axis_slab_product() {
        // 2 X cuts (3 X slabs) + 1 Z cut (2 Z slabs), no Y cuts (1 Y slab) -> 3*1*2 = 6 pieces.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=-10\n\
             [[slicing.cut]]\naxis=\"x\"\nat=25\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n",
        );
        let pieces = piece_indices(&s).unwrap();
        assert_eq!(pieces.len(), 6);
        assert!(pieces.contains(&[0, 0, 0]) && pieces.contains(&[2, 0, 1]));
        // every Y index is 0 (no Y cuts), and no index exceeds its axis's slab count
        assert!(pieces.iter().all(|p| p[1] == 0 && p[0] < 3 && p[2] < 2));
        // an uncut model is a single piece
        let none = spec("[project]\nname=\"t\"\n[slicing]\n");
        assert_eq!(piece_indices(&none).unwrap(), vec![[0, 0, 0]]);
    }

    #[test]
    fn spread_threads_through() {
        let s = spec("[project]\nname=\"t\"\n[slicing]\n[[slicing.cut]]\naxis=\"y\"\nat=0\n");
        let d = driver_scad(&s, "t.stl", 40.0).unwrap();
        assert!(d.contains("axis = BACK, spread = 40"), "{d}");
    }

    #[test]
    fn no_cuts_errors() {
        let s = spec("[project]\nname=\"t\"\n[slicing]\n");
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }

    #[test]
    fn bad_connector_cut_index_errors() {
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"x\"\nat=0\n\
             [[slicing.connector]]\ncut=5\ntype=\"bolt\"\n",
        );
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }

    #[test]
    fn retired_pin_connector_type_errors() {
        // pin/dowel was retired (the onion replaced the glued peg); bolt + onion remain.
        let s = spec(
            "[project]\nname=\"t\"\n[slicing]\n\
             [[slicing.cut]]\naxis=\"z\"\nat=0\n\
             [[slicing.connector]]\ncut=0\ntype=\"pin\"\n",
        );
        assert!(driver_scad(&s, "t.stl", 0.0).is_err());
    }
}
