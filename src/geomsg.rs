//! The geometry WIRE (C.2): request/response types + bincode codec for the fab-geom worker.
//! Ungated — the web app links these WITHOUT the kernel; `geomsvc::handle` is the kernel-side
//! implementation. Bincode-safe by construction: plain f64s, no untagged enums (`Num` maps at
//! the edge), meshes as raw `Vec<u8>`.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::manifest::Connector;
use crate::num::Num;

/// One display/geometry object: binary STL bytes + its 3mf color.
#[derive(Serialize, Deserialize, Clone)]
pub struct GeomObject {
    pub stl: Vec<u8>,
    pub color: Option<[f32; 4]>,
}

/// `manifest::Connector` flattened for the wire (plain f64 pos — no `Num`).
#[derive(Serialize, Deserialize, Clone)]
pub struct WireConn {
    pub cut: usize,
    pub kind: String,
    pub screw: Option<String>,
    pub pos: [f64; 2],
    pub through: Option<f64>,
    pub size: Option<f64>,
}

impl From<&Connector> for WireConn {
    fn from(c: &Connector) -> Self {
        WireConn {
            cut: c.cut,
            kind: c.kind.clone(),
            screw: c.screw.clone(),
            pos: [c.pos[0].f(), c.pos[1].f()],
            through: c.through,
            size: c.size,
        }
    }
}

impl From<&WireConn> for Connector {
    fn from(w: &WireConn) -> Self {
        Connector {
            cut: w.cut,
            kind: w.kind.clone(),
            screw: w.screw.clone(),
            pos: [Num::Float(w.pos[0]), Num::Float(w.pos[1])],
            through: w.through,
            size: w.size,
        }
    }
}

/// The auto-plan in the rotated frame — what the app displays and edits.
#[derive(Serialize, Deserialize, Clone)]
pub struct PlanOut {
    pub rot: [f64; 12],
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub cuts: Vec<(char, f64)>,
    pub connectors: Vec<WireConn>,
}

/// A base solid held IN the service, addressed across the byte boundary by an opaque, shard-tagged
/// id. The `!Send` `Solid` never crosses — this plain-data handle does. `shard` routes the request to
/// the execution context that owns the solid (W.3: pool of threads native / Workers wasm; N=1 to start).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SolidId {
    pub shard: u16,
    pub idx: u32,
}

/// One rendered top-level PART: its minted handle, display STL, bbox, and provenance name (T.2b).
#[derive(Serialize, Deserialize, Clone)]
pub struct WirePart {
    pub id: SolidId,
    pub stl: Vec<u8>,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub name: Option<String>,
}

/// One printable piece: slab multi-index, connected-component index within the slab, mesh STL, and
/// the least-support build-up direction.
#[derive(Serialize, Deserialize, Clone)]
pub struct WirePiece {
    pub piece: [usize; 3],
    pub comp: usize,
    pub stl: Vec<u8>,
    pub up: [f32; 3],
}

/// A per-piece print orientation for the slice codegen (`[slicing.orient]`).
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct WireOrient {
    pub piece: [usize; 3],
    pub up: [f64; 3],
}

/// The source a render evaluates. `Path` (native fs loader) is what desktop sends today; `Bytes`
/// carries the main file + its import/lib closure in-memory for the fs-less wasm Worker (wired at W.3.6).
#[derive(Serialize, Deserialize, Clone)]
pub enum Source {
    Path(String),
    Bytes {
        main: Vec<u8>,
        libs: Vec<(String, Vec<u8>)>,
    },
}

/// Tessellation quality fab OWNS (W.3.25): injected as `$fa`/`$fs` (adaptive — `$fn` stays 0) at the top
/// of the render wrap, so models drop the `$fn = $preview ? draft : final` boilerplate. Draft = coarse
/// (interactive), Final = fine (export + the "Final" preview toggle). Injected BEFORE the model, so a
/// model's own `$fn`/`$fa` still overrides — graceful migration; local overrides win.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Draft,
    Final,
}

impl Quality {
    /// The `($fa, $fs)` fab injects for this quality. Draft = OpenSCAD's own defaults (coarse, fast);
    /// Final = fine enough that facets don't show on a print.
    pub fn fa_fs(self) -> (f64, f64) {
        match self {
            Quality::Draft => (12.0, 2.0),
            Quality::Final => (6.0, 0.5),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Request {
    /// Parse an upload (.stl or .3mf by `name`), weld, rotate-to-fit, auto-plan.
    Analyze {
        name: String,
        bytes: Vec<u8>,
        bed: [f64; 3],
    },
    /// Cut every object with the shared spec (objects are already in the plan frame).
    Slice {
        objects: Vec<GeomObject>,
        cuts: Vec<(char, f64)>,
        connectors: Vec<WireConn>,
        with_connectors: bool,
    },
    /// The full make pipeline: union → orient → pack → Bambu 3mf bytes.
    Export {
        objects: Vec<GeomObject>,
        cuts: Vec<(char, f64)>,
        connectors: Vec<WireConn>,
        bed: [f64; 3],
        gap: f64,
    },
    /// Cross-section of the union at one cut (the editor's profile).
    Section {
        objects: Vec<GeomObject>,
        axis: usize,
        at: f64,
    },

    // --- fab-gui ops (W.3): base solids held by handle; only these touch a Solid. ---
    /// Render the source WHOLE → mints 1 handle. `preview` sets `$preview`: `true` = the fast
    /// low-facet path the interactive view uses; `false` = full-res (the web save-back's mesh source,
    /// W.5.2).
    RenderWhole {
        source: Source,
        root: Option<String>,
        preview: bool,
        quality: Quality,
    },
    /// Render the source into TOP-LEVEL parts (T.2b) → mints N handles.
    RenderParts {
        source: Source,
        root: Option<String>,
        quality: Quality,
    },
    /// Slice one part off its held base → the (spread, unioned) preview STL. Reads `base`.
    Reslice {
        base: SolidId,
        cuts: Vec<(char, f64)>,
        connectors: Vec<WireConn>,
        orient: Vec<WireOrient>,
        spread: f64,
    },
    /// The cut's 2D profile off the held base (connector editor). Reads `base`.
    CrossSection { base: SolidId, axis: usize, at: f64 },
    /// Fit-to-bed cut plan + onion connectors off the held base. Reads `base`.
    AutoPlan {
        base: SolidId,
        min: [f64; 3],
        max: [f64; 3],
        bed: [f64; 3],
    },
    /// Two-pass print layout (bare→orient, carved→pieces) off the held base. Reads `base`.
    PrintLayout {
        base: SolidId,
        cuts: Vec<(char, f64)>,
        connectors: Vec<WireConn>,
    },
    /// Produce the web save-back's two mesh variants off a held base (W.5.6): a full-res mesh + a
    /// decimated low-res mesh, BOTH in one format — 3MF if the solid is colored (so color survives),
    /// STL otherwise. `budget` is the low-res triangle target. Reads `base` (a FULL-RES render, W.5.2).
    SaveMeshes { base: SolidId, budget: u32 },
    /// Drop held base solids (reload / file-switch / part-count change).
    Free { ids: Vec<SolidId> },
}

#[derive(Serialize, Deserialize)]
pub struct Piece {
    pub idx: [usize; 3],
    pub stl: Vec<u8>,
    pub color: Option<[f32; 4]>,
}

#[derive(Serialize, Deserialize)]
pub enum Response {
    /// `plan: None` = view-only (didn't weld); objects are then the RAW frame, else rotated.
    Analyzed {
        objects: Vec<GeomObject>,
        plan: Option<PlanOut>,
        tris: usize,
    },
    Sliced {
        pieces: Vec<Piece>,
    },
    Exported {
        threemf: Vec<u8>,
        pieces: usize,
        plates: usize,
    },
    Sectioned {
        loops: Vec<Vec<[f64; 2]>>,
    },

    // --- fab-gui ops (W.3) ---
    Rendered {
        id: SolidId,
        stl: Vec<u8>,
        min: [f64; 3],
        max: [f64; 3],
        /// The eval's `echo`/warning console lines (already `ECHO: …` / `WARNING: …`), for the GUI
        /// console (W.3.16) — the only way to see them on web, where the eval runs in the worker.
        messages: Vec<String>,
    },
    PartsRendered {
        parts: Vec<WirePart>,
        /// `echo`/warning lines from the whole-model eval (W.3.16).
        messages: Vec<String>,
    },
    Resliced {
        stl: Vec<u8>,
    },
    Planned {
        cuts: Vec<(char, f64)>,
        connectors: Vec<WireConn>,
        pieces: usize,
    },
    LaidOut {
        pieces: Vec<WirePiece>,
    },
    /// The save-back mesh pair (W.5.6). `low`/`high` are always 3MF now (W.3.18 — color survives when
    /// present, geometry-only otherwise); `ext` is the multipart filename extension the site classifies
    /// the variant by (kept in the wire for forward-compat, currently always `"3mf"`).
    SavedMeshes {
        low: Vec<u8>,
        high: Vec<u8>,
        ext: String,
    },
    Freed,

    Failed {
        error: String,
        /// 1-based EDITOR line the fault maps to (W.3.37), when an eval error carried a source span — the
        /// GUI points the editor + status at it. `None` for non-eval failures (transport, unknown handle)
        /// or when the span couldn't be mapped. Additive bincode field (tolerated by older peers).
        #[serde(default)]
        line: Option<u32>,
    },
}

pub fn encode_request(r: &Request) -> Vec<u8> {
    bincode::serialize(r).expect("wire types are bincode-total")
}
pub fn decode_request(b: &[u8]) -> Result<Request> {
    bincode::deserialize(b).map_err(|e| anyhow!("bad request: {e}"))
}
pub fn encode_response(r: &Response) -> Vec<u8> {
    bincode::serialize(r).expect("wire types are bincode-total")
}
pub fn decode_response(b: &[u8]) -> Result<Response> {
    bincode::deserialize(b).map_err(|e| anyhow!("bad response: {e}"))
}

/// The wasm Worker's REPLY envelope (W.3.16): the [`Response`] PLUS the `tracing` lines the worker
/// captured during the call, for the app's "Full" console. On WEB the worker is a separate wasm context
/// so its logs can't reach the main-thread subscriber — this carries them back. (Native runs the worker
/// in-process, so its tracing already rides the shared subscriber; only the wasm transport uses this.)
pub fn encode_reply(response: &Response, logs: &[String]) -> Vec<u8> {
    bincode::serialize(&(response, logs)).expect("wire types are bincode-total")
}
pub fn decode_reply(b: &[u8]) -> Result<(Response, Vec<String>)> {
    bincode::deserialize(b).map_err(|e| anyhow!("bad reply: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W.3.25.3 parity pin: Draft MUST inject fab_lang's own unset defaults (`$fa=12`, `$fs=2`, the same
    /// numbers `eval::scope` seeds and OpenSCAD ships). That equality is the whole reason "fab owns quality"
    /// is a no-op for un-annotated geometry — scad-lib and every model that never set a facet var render
    /// byte-for-byte as they did before the wrap. Drift these and Draft silently re-tessellates the entire
    /// library (and breaks the oracle differential); this fails first and forces the reckoning.
    #[test]
    fn draft_quality_is_the_unset_default() {
        assert_eq!(
            Quality::Draft.fa_fs(),
            (12.0, 2.0),
            "Draft must stay == the fab_lang/OpenSCAD unset default, or it stops being a no-op"
        );
        // And Final must be strictly finer on both knobs, or the toggle does nothing useful.
        let (dfa, dfs) = Quality::Draft.fa_fs();
        let (ffa, ffs) = Quality::Final.fa_fs();
        assert!(
            ffa < dfa && ffs < dfs,
            "Final must be finer than Draft on both $fa and $fs"
        );
    }
}
