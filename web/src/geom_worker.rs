//! geomsvc calls from the app: on wasm, bincode over postMessage to the fab-geom worker
//! (lazy-created, `data-base`-aware — the worker script + its ~1 MB wasm fetch on first use);
//! natively, `geomsvc::handle` runs right on the task-pool thread. Either way the caller
//! awaits a `Response`, and Solids never enter this crate — bytes in, bytes out.

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use fab_scad::geomsg::{Request, Response};

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::{anyhow, Result};
    use fab_scad::geomsg::{self, Request, Response};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    use crate::worker_rpc::Rpc;

    thread_local! {
        static WORKER: RefCell<Option<(web_sys::Worker, Rc<Rpc>)>> = const { RefCell::new(None) };
    }

    fn get_worker() -> Result<(web_sys::Worker, Rc<Rpc>)> {
        if let Some(w) = WORKER.with(|w| w.borrow().clone()) {
            return Ok(w);
        }
        let opts = web_sys::WorkerOptions::new();
        opts.set_type(web_sys::WorkerType::Module);
        let url = format!("{}geom/geom-worker.js", crate::bundle_base());
        let worker = web_sys::Worker::new_with_options(&url, &opts)
            .map_err(|_| anyhow!("geometry worker failed to start ({url})"))?;
        let rpc = Rpc::attach(
            &worker,
            "geometry worker failed to load - is geom/ deployed and data-base right?",
        );
        WORKER.with(|w| *w.borrow_mut() = Some((worker.clone(), rpc.clone())));
        Ok((worker, rpc))
    }

    pub async fn call(req: Request) -> Result<Response> {
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
            let e = get("error")
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "unknown".into());
            return Err(anyhow!("geometry worker: {e}"));
        }
        let out = get("buf").ok_or_else(|| anyhow!("geometry worker: empty reply"))?;
        geomsg::decode_response(&js_sys::Uint8Array::new(&out).to_vec())
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::call;

/// Native twin: same seam, no worker — the kernel runs on the pool thread this future lands on.
#[cfg(not(target_arch = "wasm32"))]
pub async fn call(req: Request) -> Result<Response> {
    Ok(fab_scad::geomsvc::handle(req))
}
