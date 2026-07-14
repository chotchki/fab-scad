//! The GUI's pure geometry vocabulary + the in-process (non-Solid) helpers. Every Solid-TOUCHING op
//! (render / reslice / cross-section / auto-plan / print-layout) now goes through the geometry SERVICE
//! (`crate::geom::GeomPool` → `fab_scad::geomsvc`, W.3.3): the `!Send` Solid lives on the service
//! thread and never crosses a task boundary — only bytes/handles do. What stays HERE is the connector/
//! orientation TYPES the panel edits, the wire converters, and the three ops that need no Solid at all
//! (onion feasibility is a pure predicate on the spec; co-pack + 3mf export are indexed-MESH work).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use fab_scad::bambu::{self, PieceToPlace};
use fab_scad::geomsg::{WireConn, WireOrient};
use fab_scad::manifest::{Connector, Cut, PieceOrient, Slicing};
use fab_scad::num::Num;
use fab_scad::slicing;

use crate::stl::StlMesh;

/// A `[slicing]` spec carrying only the cuts — the shared base the feasibility predicate layers
/// connectors + orientation onto.
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
/// carries its screw size (and lets the slicer default `through`). The feasibility predicate consumes
/// these (the render/slice path sends [`to_wire_conns`] over the wire instead).
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

/// GUI connector placements → WIRE connectors for the geometry service (W.3.3). Same per-kind mapping
/// as [`to_connectors`], but flattened to the bincode-safe `WireConn` (plain f64 pos, no `Num`); the
/// service maps them back to manifest connectors the slicer consumes.
pub fn to_wire_conns(connectors: &[Conn]) -> Vec<WireConn> {
    connectors
        .iter()
        .map(|c| match c.kind {
            ConnKind::Onion => WireConn {
                cut: c.cut,
                kind: "onion".to_string(),
                screw: None,
                pos: c.pos,
                through: None,
                size: Some(c.size),
            },
            ConnKind::Bolt => WireConn {
                cut: c.cut,
                kind: "bolt".to_string(),
                screw: Some(c.screw.to_string()),
                pos: c.pos,
                through: None, // slicer defaults through-depth (12mm) until we expose it
                size: None,
            },
        })
        .collect()
}

/// GUI per-piece orientations → WIRE orientations for the geometry service. The slice codegen gates
/// onions/teardrops per SLAB, so `Reslice` carries these so the service honours the print-up.
pub fn to_wire_orient(orient: &[Orient3]) -> Vec<WireOrient> {
    orient
        .iter()
        .map(|o| WireOrient {
            piece: o.piece,
            up: o.up,
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

/// One printable piece for the print-orientation preview, rebuilt from the service's `WirePiece`
/// (W.3.3): its slab multi-index, its connected-COMPONENT index within that slab (0 when the slab is
/// one solid; >0 splits a presliced blob into its real pieces — T.2a), the mesh (WITH its joints
/// carved — peg/socket), and the least-support build-up the service picked (`auto_orient::best_up`).
pub struct PiecePrint {
    pub piece: [usize; 3],
    pub comp: usize,
    pub mesh: StlMesh,
    pub up: [f32; 3],
}

/// Per-connector onion feasibility under the current cuts + orientations, index-aligned with
/// `connectors`: `true` = prints support-free, `false` = downgrades to a bolt. PURE — a predicate on
/// the spec, no Solid — so it stays in-process (the GUI flags joints live as cuts/orientations change,
/// no service round-trip). Same gate the slice carves with.
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
/// direction (model space, unit). Threaded into `Reslice` as `[slicing.orient]` so the slice honours
/// the auto-picked / manual print orientation (and gates the onions accordingly).
#[derive(Clone, Copy)]
pub struct Orient3 {
    pub piece: [usize; 3],
    pub up: [f64; 3],
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
/// the fewest `bed`-sized plates, `gap` mm apart. Pure indexed-MESH work — no `Solid` — so it stays
/// in-process; it's cheap enough to call inline on a click.
pub fn export_plates(
    pieces: &[&PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    plate: [f64; 2],
    gap: f64,
    preset: Option<&fab_scad::printers::BambuPreset>,
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
    bambu::export_plates(out, to_place, bed, plate, gap, preset)
}

/// Plate-count / fill summary of co-packing `pieces` (U.3.5) — the cheap reactive twin of
/// [`export_plates`]: same orient→footprint→bin-pack, but it writes no 3mf, so the panel can show a
/// live `plates · pieces · fits WxH` metric as orientations change.
pub fn copack_summary(
    pieces: &[&PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    gap: f64,
) -> Result<bambu::ExportSummary> {
    let to_place: Vec<PieceToPlace> = pieces
        .iter()
        .zip(ups)
        .map(|(pp, &up)| PieceToPlace {
            mesh: stlmesh_to_bambu(&pp.mesh),
            up,
        })
        .collect();
    bambu::pack_summary(&to_place, bed, gap)
}
