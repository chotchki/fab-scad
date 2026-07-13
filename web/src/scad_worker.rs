//! .scad → STL bytes through the OpenSCAD worker (Phase B). Everything here is LAZY: the
//! worker script, the 10.7 MB OpenSCAD wasm and the lib pack (BOSL2 + scad-lib, baked at the
//! repo's pins) are only fetched when the first .scad opens — STL/3mf users never pay. The
//! GPL module runs UNMODIFIED in its own worker; this file and the worker glue are the
//! arm's-length seam (see docs/web-bundle.md for the licensing stance). One render in flight
//! at a time — matches the app's single-flight picker.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use bevy::log::info;
use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::worker_rpc::Rpc;

thread_local! {
    static WORKER: RefCell<Option<(web_sys::Worker, Rc<Rpc>)>> = const { RefCell::new(None) };
    static LIBS: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Render `source` to binary STL bytes. Errors carry OpenSCAD's last log lines.
pub async fn render(source: String) -> Result<Vec<u8>> {
    let err = |w: String| anyhow!("openscad: {w}");
    let libs = libs_object().await?;
    let (worker, rpc) = get_worker()?;
    let (id, promise) = rpc.register();

    let msg = Object::new();
    let set = |k: &str, v: &JsValue| Reflect::set(&msg, &JsValue::from_str(k), v).ok();
    set("id", &JsValue::from_f64(id as f64));
    set("source", &JsValue::from_str(&source));
    set("files", &libs);
    set("args", &js_sys::Array::new());
    worker
        .post_message(&msg)
        .map_err(|_| err("postMessage failed".into()))?;

    let data = JsFuture::from(promise)
        .await
        .map_err(|_| err("worker died".into()))?;
    let get = |k: &str| Reflect::get(&data, &JsValue::from_str(k)).ok();
    let ok = get("ok").map(|v| v.is_truthy()).unwrap_or(false);
    if !ok {
        let e = get("error")
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "unknown".into());
        let logs = get("logs")
            .map(|l| js_sys::Array::from(&l))
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_string())
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .unwrap_or_default();
        // JSC (Safari) blows its engine stack on deeply recursive models (BOSL2 attachable
        // trees) that V8 renders fine — same wasm, different engine headroom. Say so.
        let hint = if e.contains("Maximum call stack") {
            " - this model's recursion exceeds this browser's WebAssembly stack; deep BOSL2 models currently need Chrome/Edge/Firefox"
        } else {
            ""
        };
        return Err(err(format!("{e}{hint} ({logs})")));
    }
    let stl = get("stl").ok_or_else(|| err("no stl in reply".into()))?;
    let bytes = Uint8Array::new(&stl).to_vec();
    info!("openscad rendered {} bytes", bytes.len());
    Ok(bytes)
}

/// The lib pack, fetched + parsed once: a JS object of path → text the worker writes into its
/// virtual FS before every render.
async fn libs_object() -> Result<JsValue> {
    if let Some(libs) = LIBS.with(|l| l.borrow().clone()) {
        return Ok(libs);
    }
    let bytes = crate::fetch_bytes(&format!("{}openscad/libs.json", crate::bundle_base())).await?;
    let text = String::from_utf8(bytes).map_err(|_| anyhow!("libs.json not utf-8"))?;
    let parsed = js_sys::JSON::parse(&text).map_err(|_| anyhow!("libs.json: bad json"))?;
    LIBS.with(|l| *l.borrow_mut() = Some(parsed.clone()));
    Ok(parsed)
}

/// The worker, created once (document-relative URL — the bundle contract serves members next
/// to the page). The heavy fetches (worker script + openscad.js + wasm) happen HERE, on first
/// use only.
fn get_worker() -> Result<(web_sys::Worker, Rc<Rpc>)> {
    if let Some(w) = WORKER.with(|w| w.borrow().clone()) {
        return Ok(w);
    }
    let opts = web_sys::WorkerOptions::new();
    opts.set_type(web_sys::WorkerType::Module);
    let url = format!("{}openscad/openscad-worker.js", crate::bundle_base());
    let worker = web_sys::Worker::new_with_options(&url, &opts)
        .map_err(|_| anyhow!("openscad worker failed to start ({url})"))?;
    let rpc = Rpc::attach(
        &worker,
        "openscad worker failed to load - is openscad/ deployed and data-base right?",
    );
    WORKER.with(|w| *w.borrow_mut() = Some((worker.clone(), rpc.clone())));
    Ok((worker, rpc))
}
