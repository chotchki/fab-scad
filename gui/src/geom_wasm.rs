//! The wasm geometry transport (W.3.6): `GeomPool` talks to the fab-geom Web Worker over bincode via
//! postMessage — the transport twin of the native kernel-thread pool (`geom.rs`). The `!Send` Worker +
//! `Rpc` live in a `thread_local` (wasm is single-threaded); `GeomPool` is a ZST `Resource` so the
//! shared render/slice systems drive it UNCHANGED — only the transport behind `call` differs by target.
//! The Manifold kernel runs OFF the main thread in the Worker, isolating the `-fno-exceptions`
//! bad_alloc trap: a crash comes back as `ok:false`, and the dead worker is NULLED so the next call
//! re-creates a fresh instance (+ fresh store) — held handles then miss and the app re-renders.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use bevy::prelude::Resource;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use fab_scad::geomsg::{self, Request, Response};

use crate::worker_rpc::Rpc;

thread_local! {
    static WORKER: RefCell<Option<(web_sys::Worker, Rc<Rpc>)>> = const { RefCell::new(None) };
}

/// Where the bundle's members live — the page declares it via `<canvas id="fab-web" data-base=…>`;
/// document-relative by default. The geom worker + its wasm live under `{base}geom/`, libs.json at `{base}`.
pub(crate) fn bundle_base() -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("fab-web"))
        .and_then(|c| c.get_attribute("data-base"))
        .map(|mut b| {
            if !b.ends_with('/') {
                b.push('/');
            }
            b
        })
        .unwrap_or_default()
}

/// The one worker, lazily created (its ~1.7 MB wasm fetches on first use). Cached in the thread_local;
/// nulled on a crash so this re-creates it.
fn get_worker() -> Result<(web_sys::Worker, Rc<Rpc>)> {
    if let Some(w) = WORKER.with(|w| w.borrow().clone()) {
        return Ok(w);
    }
    let opts = web_sys::WorkerOptions::new();
    opts.set_type(web_sys::WorkerType::Module);
    let url = format!("{}geom/geom-worker.js", bundle_base());
    let worker = web_sys::Worker::new_with_options(&url, &opts)
        .map_err(|_| anyhow!("geometry worker failed to start ({url})"))?;
    let rpc = Rpc::attach(
        &worker,
        "geometry worker failed to load — is geom/ deployed and data-base right?",
    );
    WORKER.with(|w| *w.borrow_mut() = Some((worker.clone(), rpc.clone())));
    Ok((worker, rpc))
}

/// The wasm transport. ZST + `Clone` + `Resource` mirror the native `GeomPool` so `Res<GeomPool>` and
/// the systems that drive it are identical on both targets; the Worker lives in the thread_local.
#[derive(Resource, Clone)]
pub struct GeomPool;

impl GeomPool {
    /// Match the native signature (`n` shards); the worker is created lazily on the first `call`.
    pub fn new(_n: u16) -> Self {
        GeomPool
    }

    /// Encode → postMessage (transfer the buffer) → await the id-matched reply → decode. `Err` =
    /// TRANSPORT failure (worker gone/crashed); domain failures arrive as `Ok(Response::Failed)`.
    pub async fn call(&self, req: Request) -> Result<Response> {
        let (worker, rpc) = get_worker()?;
        let (id, promise) = rpc.register();

        let bytes = geomsg::encode_request(&req);
        let buf = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
        let msg = js_sys::Object::new();
        js_sys::Reflect::set(&msg, &"id".into(), &JsValue::from_f64(id as f64)).ok();
        js_sys::Reflect::set(&msg, &"buf".into(), &buf).ok();
        worker
            .post_message_with_transfer(&msg, &js_sys::Array::of1(&buf))
            .map_err(|_| anyhow!("geometry worker: postMessage failed"))?;

        let data = JsFuture::from(promise)
            .await
            .map_err(|_| anyhow!("geometry worker died"))?;
        let get = |k: &str| js_sys::Reflect::get(&data, &JsValue::from_str(k)).ok();
        if !get("ok").map(|v| v.is_truthy()).unwrap_or(false) {
            // A wasm trap (bad_alloc under -fno-exceptions) poisons the instance — NULL it so the next
            // call re-creates a fresh worker + store; held handles then miss → the app re-renders.
            WORKER.with(|w| *w.borrow_mut() = None);
            let e = get("error")
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "unknown".into());
            return Err(anyhow!("geometry worker: {e}"));
        }
        let out = get("buf").ok_or_else(|| anyhow!("geometry worker: empty reply"))?;
        geomsg::decode_response(&js_sys::Uint8Array::new(&out).to_vec())
    }
}
