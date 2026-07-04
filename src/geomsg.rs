//! The geometry WIRE (C.2): request/response types + bincode codec for the fab-geom worker.
//! Ungated — the web app links these WITHOUT the kernel; `geomsvc::handle` is the kernel-side
//! implementation. Bincode-safe by construction: plain f64s, no untagged enums (`Num` maps at
//! the edge), meshes as raw `Vec<u8>`.

use anyhow::{anyhow, Result};
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
