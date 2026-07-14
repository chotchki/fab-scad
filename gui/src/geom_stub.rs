//! Placeholder wasm geometry transport (W.3.4) — the SLOT the W.3.6 Web Worker fills. `GeomPool`'s
//! API matches the native kernel-thread transport (`geom.rs`), but until the Worker is wired every
//! op returns `Failed`, so the wasm app boots with UI + an EMPTY scene (renders fail → no geometry
//! shown, honestly, not a fake). W.3.6 replaces `call`'s body with real Worker RPC — a transport swap,
//! no change to the systems that drive it.

use anyhow::Result;
use bevy::prelude::Resource;

use fab_scad::geomsg::{Request, Response};

/// The wasm transport stub. `Clone`/`Resource` mirror the native `GeomPool` so the shared render/
/// slice systems compile + run unchanged; only the transport behind `call` differs by target.
#[derive(Resource, Clone)]
pub struct GeomPool;

impl GeomPool {
    /// Match the native signature (`n` shards); the stub holds nothing.
    pub fn new(_n: u16) -> Self {
        GeomPool
    }

    /// Every op fails until the W.3.6 Web Worker lands — the app surfaces the error + shows no
    /// geometry (empty scene), never a silent lie.
    pub async fn call(&self, _req: Request) -> Result<Response> {
        Ok(Response::Failed {
            error: "geometry runs in the Web Worker (W.3.6 — not wired yet)".into(),
        })
    }
}
