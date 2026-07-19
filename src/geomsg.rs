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
    },
    /// Render the source into TOP-LEVEL parts (T.2b) → mints N handles.
    RenderParts {
        source: Source,
        root: Option<String>,
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
    /// The save-back mesh pair (W.5.6). `low`/`high` are the same format, named by `ext` ("stl" |
    /// "3mf") — which is also the multipart filename extension the site classifies the variant by.
    SavedMeshes {
        low: Vec<u8>,
        high: Vec<u8>,
        ext: String,
    },
    Freed,

    Failed {
        error: String,
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
