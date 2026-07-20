//! The GUI's pure geometry vocabulary + the in-process (non-Solid) helpers. Every Solid-TOUCHING op
//! (render / reslice / cross-section / auto-plan / print-layout) now goes through the geometry SERVICE
//! (`crate::geom::GeomPool` → `fab_scad::geomsvc`, W.3.3): the `!Send` Solid lives on the service
//! thread and never crosses a task boundary — only bytes/handles do. What stays HERE is the connector/
//! orientation TYPES the panel edits, the wire converters, and the three ops that need no Solid at all
//! (onion feasibility is a pure predicate on the spec; co-pack + 3mf export are indexed-MESH work).

use std::collections::HashMap;
// `Path` feeds only the native `export_plates` (wasm delivers `.3mf` bytes as a Blob — W.3.13).
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;

use fab_scad::bambu::{self, PieceToPlace};
use fab_scad::feasibility;
use fab_scad::geomsg::{WireConn, WireOrient};
use fab_scad::manifest::{Connector, Cut, PieceOrient, Slicing};
use fab_scad::num::Num;

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
/// Walk up from `start` for the workspace root — the dir holding `printers.toml` + `scad-lib/`, which
/// carries the library search paths BOSL2 resolves against. `None` if no ancestor qualifies.
pub fn find_root_from(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("printers.toml").exists() && dir.join("scad-lib").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The workspace root from the CURRENT directory — right for the CLI/dev entry (cwd IS the workspace).
/// A double-clicked `.app` opens with cwd `/`, so `native_entry` walks up from the OPENED MODEL instead
/// (W.3.21) — without a root there are no library paths and every BOSL2 module goes undefined → empty.
pub fn find_root() -> Option<PathBuf> {
    find_root_from(&std::env::current_dir().ok()?)
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
    feasibility::onion_feasibility(&spec)
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

/// The shared piece-prep for the export/summary entries: preview meshes welded to indexed
/// [`bambu::Mesh`]es, paired with their resolved build-ups.
fn pieces_to_place(pieces: &[&PiecePrint], ups: &[[f64; 3]]) -> Vec<PieceToPlace> {
    pieces
        .iter()
        .zip(ups)
        .map(|(pp, &up)| PieceToPlace {
            mesh: stlmesh_to_bambu(&pp.mesh),
            up,
        })
        .collect()
}

/// Export the print-oriented preview pieces as a Bambu multi-plate project `.3mf` at `out`. `ups[i]`
/// is the resolved build-up for `pieces[i]` (auto-pick or the user's manual override). Bin-packs onto
/// the fewest `bed`-sized plates, `gap` mm apart. Pure indexed-MESH work — no `Solid` — so it stays
/// in-process; it's cheap enough to call inline on a click.
#[cfg(not(target_arch = "wasm32"))]
pub fn export_plates(
    pieces: &[&PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    plate: [f64; 2],
    gap: f64,
    preset: Option<&fab_scad::printers::BambuPreset>,
    out: &Path,
) -> Result<bambu::ExportSummary> {
    bambu::export_plates(out, pieces_to_place(pieces, ups), bed, plate, gap, preset)
}

/// [`export_plates`] into MEMORY (W.3.13) — the browser delivery: same layout/pack/emit through
/// `bambu::export_plates_to`, sunk into a `Cursor<Vec<u8>>` so the wasm app can hand the `.3mf`
/// bytes to a Blob download (there is no fs to write "next to the source" in a browser). Compiled
/// on native too (only the wasm export action CALLS it there → allow) so the unit test below covers
/// the byte path without a browser.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn export_plates_bytes(
    pieces: &[&PiecePrint],
    ups: &[[f64; 3]],
    bed: [f64; 2],
    plate: [f64; 2],
    gap: f64,
    preset: Option<&fab_scad::printers::BambuPreset>,
) -> Result<(bambu::ExportSummary, Vec<u8>)> {
    let mut sink = std::io::Cursor::new(Vec::new());
    let sum = bambu::export_plates_to(
        &mut sink,
        pieces_to_place(pieces, ups),
        bed,
        plate,
        gap,
        preset,
    )?;
    Ok((sum, sink.into_inner()))
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
    bambu::pack_summary(&pieces_to_place(pieces, ups), bed, gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_root_from_walks_up_to_the_workspace() {
        // W.3.21: a double-clicked .app has cwd `/`, so root must be found from the OPENED model's
        // location. A temp workspace (printers.toml + scad-lib/) with a nested model dir → find_root_from
        // a deep child returns the workspace root; a dir with no such ancestor → None.
        let tmp = std::env::temp_dir().join(format!("fab-root-test-{}", std::process::id()));
        let nested = tmp.join("models").join("thing");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(tmp.join("scad-lib")).unwrap();
        std::fs::write(tmp.join("printers.toml"), "").unwrap();
        assert_eq!(find_root_from(&nested).as_deref(), Some(tmp.as_path()));
        assert_eq!(find_root_from(std::path::Path::new("/")), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A unit tetrahedron as STL triangle soup — the smallest closed mesh with a real footprint.
    fn tet() -> StlMesh {
        let v = [
            [0.0f32, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
        ];
        let tris = [[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]];
        let positions: Vec<[f32; 3]> = tris.iter().flat_map(|t| t.map(|i| v[i])).collect();
        let normals = vec![[0.0f32, 0.0, 1.0]; positions.len()];
        StlMesh { positions, normals }
    }

    /// W.3.13's core: the in-memory `.3mf` export produces a real zip (the Blob download's bytes) with
    /// the same summary the path-writing export reports. Native-tested — the wasm side only differs in
    /// delivery (Blob vs file), never in bytes.
    #[test]
    fn export_plates_bytes_zips_in_memory() {
        let piece = PiecePrint {
            piece: [0, 0, 0],
            comp: 0,
            mesh: tet(),
            up: [0.0, 0.0, 1.0],
        };
        let (sum, bytes) = export_plates_bytes(
            &[&piece],
            &[[0.0, 0.0, 1.0]],
            [256.0, 256.0],
            [256.0, 256.0],
            5.0,
            None,
        )
        .expect("one tet packs onto one plate");
        assert_eq!(sum.pieces, 1);
        assert_eq!(sum.plates, 1);
        assert!(bytes.len() > 200, "a real 3mf zip, not a stub");
        assert_eq!(&bytes[..2], b"PK", "3mf is a zip container");
    }
}
