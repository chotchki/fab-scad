//! The NATIVE geometry-service transport (W.3.3): a pool of kernel THREADS, each owning a
//! [`fab_scad::geomsvc::SolidStore`]. The `!Send` `Solid`s live on their shard's thread and never
//! cross — only `Request`/`Response` (both `Send`) travel the channel. Reads route to the shard that
//! owns the base handle; mints pick a shard (N=1 today → shard 0; per-part sharding is a knob — bump
//! `n` + the assignment policy, no protocol change). The wasm transport (a pool of Web Workers, same
//! routing) lands at W.3.6; a `Geom` enum will unify the two then.
//!
//! Crash model: a Rust panic in the kernel becomes a `Failed` response (the loop survives via
//! `catch_unwind`). A hard C++ `bad_alloc`/abort aborts the process — the honest limit of a THREAD
//! transport; the `SubprocessTransport` follow-up (and the wasm Worker) get true isolation.

use anyhow::{Context, Result, anyhow};
use bevy::prelude::Resource;

use fab_scad::geomsg::{Request, Response};
use fab_scad::geomsvc::{SolidStore, handle_with_store};

type Reply = async_channel::Sender<Response>;

/// One shard: a dedicated thread owning a `SolidStore`, fed `Request`s over a channel, replying
/// per-request over a fresh oneshot. `Clone` shares the send end (the thread + store are the shared
/// resource behind it) so the pool can hand a cheap handle to every async task body.
#[derive(Clone)]
struct Shard {
    tx: async_channel::Sender<(Request, Reply)>,
}

impl Shard {
    fn spawn(shard: u16) -> Self {
        let (tx, rx) = async_channel::unbounded::<(Request, Reply)>();
        std::thread::Builder::new()
            .name(format!("fab-geom-{shard}"))
            .spawn(move || {
                let mut store = SolidStore::new(shard);
                while let Ok((req, reply)) = rx.recv_blocking() {
                    let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handle_with_store(&mut store, req)
                    }))
                    .unwrap_or_else(|_| Response::Failed {
                        error: "geometry kernel panicked".into(),
                    });
                    // A superseded (coalesced) call dropped its receiver — harmless. NEVER unwrap here:
                    // that would kill the loop on the routine cut-drag-debounce case.
                    let _ = reply.send_blocking(resp);
                }
            })
            .expect("spawn fab-geom thread");
        Shard { tx }
    }

    async fn call(&self, req: Request) -> Result<Response> {
        let (rtx, rrx) = async_channel::bounded(1);
        self.tx
            .send((req, rtx))
            .await
            .map_err(|_| anyhow!("geometry shard gone"))?;
        rrx.recv()
            .await
            .map_err(|_| anyhow!("geometry shard dropped the reply"))
    }
}

/// The native transport: a pool of shards addressed by handle shard. A cloneable Bevy `Resource` —
/// clones share the shard threads (they're behind the `async_channel` send ends), so a system holds
/// `Res<GeomPool>` and every spawned task body moves its own cheap clone in.
#[derive(Resource, Clone)]
pub struct GeomPool {
    shards: Vec<Shard>,
}

impl GeomPool {
    /// Spawn `n` kernel-thread shards (clamped to ≥1). N=1 to start.
    pub fn new(n: u16) -> Self {
        GeomPool {
            shards: (0..n.max(1)).map(Shard::spawn).collect(),
        }
    }

    /// Which shard owns this request's work: reads route by the base handle; mints (+ the stateless
    /// upload arms) assign shard 0 for now (round-robin / per-part is the future knob).
    fn shard_for(req: &Request) -> u16 {
        match req {
            Request::Reslice { base, .. }
            | Request::CrossSection { base, .. }
            | Request::AutoPlan { base, .. }
            | Request::PrintLayout { base, .. } => base.shard,
            Request::Free { ids } => ids.first().map(|i| i.shard).unwrap_or(0),
            _ => 0,
        }
    }

    /// Send a request to its owning shard and await the reply. `Err` = TRANSPORT failure (shard gone);
    /// domain failures arrive as `Ok(Response::Failed)`.
    pub async fn call(&self, req: Request) -> Result<Response> {
        let s = Self::shard_for(&req) as usize;
        let shard = self
            .shards
            .get(s)
            .or_else(|| self.shards.first())
            .context("geometry pool has no shards")?;
        shard.call(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::tasks::block_on;
    use fab_scad::geomsg::{Request, Response, Source};

    #[test]
    fn pool_renders_reslices_off_the_held_handle_then_frees() {
        let tmp = std::env::temp_dir().join(format!("geompool_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("box.scad");
        std::fs::write(&src, "cube([60,40,30], center=true);").unwrap();

        let pool = GeomPool::new(1);
        let Response::PartsRendered { parts, .. } = block_on(pool.call(Request::RenderParts {
            source: Source::Path(src.to_string_lossy().into_owned()),
            root: None,
        }))
        .unwrap() else {
            panic!("render over the transport failed")
        };
        let id = parts[0].id;

        // A reslice reads the HELD base — no re-render — routed back to the same shard by id.shard.
        assert!(
            matches!(
                block_on(pool.call(Request::Reslice {
                    base: id,
                    cuts: vec![('x', 0.0)],
                    connectors: vec![],
                    orient: vec![],
                    spread: 40.0,
                }))
                .unwrap(),
                Response::Resliced { .. }
            ),
            "reslice off the held handle"
        );

        block_on(pool.call(Request::Free { ids: vec![id] })).unwrap();
        assert!(
            matches!(
                block_on(pool.call(Request::CrossSection {
                    base: id,
                    axis: 2,
                    at: 0.0,
                }))
                .unwrap(),
                Response::Failed { .. }
            ),
            "an op on a freed handle → Failed across the transport"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
