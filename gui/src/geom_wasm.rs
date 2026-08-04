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
use bevy::prelude::{Resource, info};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use fab_scad::geomsg::{self, Request, Response};

use crate::worker_rpc::Rpc;

thread_local! {
    static WORKER: RefCell<Option<(web_sys::Worker, Rc<Rpc>, Variant)>> = const { RefCell::new(None) };
}

/// WHICH WORKER WASM to run (SZ.4). Two builds of fab-geom: the same kernel and the same evaluator,
/// differing only in whether the transpiled band is linked in. Same answers either way — a native is
/// bit-identical to interpreting its reference by construction — so this is purely how much wasm the
/// browser downloads.
///
///     lean  1.3 MB brotli   BOSL2/MCAD calls INTERPRET
///     full  5.4 MB brotli   they dispatch to compiled natives
///
/// Worth 4.1 MB to a `cube(10);` user, and worth nothing to a BOSL2 user — of 122 real models there
/// are nine distinct include-closures and every one pulls 67-76% of BOSL2, so there is no middle
/// case to split more finely for. That measurement is why this is a variant rather than a loader.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Lean,
    Full,
}

impl Variant {
    /// The bundle subdirectory this variant's worker lives in.
    fn dir(self) -> &'static str {
        match self {
            Self::Lean => "geom-lean",
            Self::Full => "geom",
        }
    }

    /// The name this variant announces itself by. Logged at every worker creation, because the
    /// routing decision is otherwise INVISIBLE: both variants render the same geometry, so a router
    /// stuck on one of them looks exactly like a working one from the outside. This string is what
    /// `packaging/web/e2e-full-worker.sh` asserts on, and what tells you from a user's console
    /// whether they paid for the band.
    fn label(self) -> &'static str {
        match self {
            Self::Lean => "lean",
            Self::Full => "full",
        }
    }
}

/// Does this request name a library that has a transpiled band?
///
/// Read off the REQUEST rather than plumbed down from the app: a web render carries its whole
/// include closure as `Source::Bytes.libs`, so the request already knows. The prefixes come from
/// `fab_scad::libraries::libraries()` — the same declaration the source pack is built from — so a new
/// library cannot be added to one and forgotten in the other.
///
/// Only a RENDER can answer. Analyze/slice/export operate on handles a previous render minted, so
/// they must never trigger a switch: that would drop the store those handles live in.
fn wanted_variant(req: &Request) -> Option<Variant> {
    let libs = match req {
        Request::RenderWhole { source, .. } | Request::RenderParts { source, .. } => match source {
            geomsg::Source::Bytes { libs, .. } => libs,
            _ => return None,
        },
        _ => return None,
    };
    let banded: Vec<&str> = fab_scad::libraries::libraries()
        .iter()
        .filter(|l| !l.prefix.is_empty())
        .map(|l| l.prefix)
        .collect();
    let needs = libs
        .iter()
        .any(|(path, _)| banded.iter().any(|p| path.starts_with(p)));
    Some(if needs { Variant::Full } else { Variant::Lean })
}

/// Where the bundle's members live — the page declares it via `<canvas id="fab-gui" data-base=…>`;
/// document-relative by default. The geom worker + its wasm live under `{base}geom/`, libs.json at `{base}`.
pub(crate) fn bundle_base() -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("fab-gui"))
        .and_then(|c| c.get_attribute("data-base"))
        .map(|mut b| {
            if !b.ends_with('/') {
                b.push('/');
            }
            b
        })
        .unwrap_or_default()
}

/// The one worker, lazily created (its wasm fetches on first use). Cached in the thread_local;
/// nulled on a crash so this re-creates it.
///
/// UPGRADE-ONLY (SZ.4). A live LEAN worker is torn down and replaced when a render first needs the
/// band, because the alternative is silently interpreting a model the user could have had compiled.
/// The reverse never happens: downgrading a full worker would save no download (it is already
/// fetched and cached) and would throw away the `SolidStore` every held handle lives in. So the
/// variant only ever ratchets up, at most once per page.
fn get_worker(want: Option<Variant>) -> Result<(web_sys::Worker, Rc<Rpc>)> {
    if let Some((worker, rpc, live)) = WORKER.with(|w| w.borrow().clone()) {
        let upgrade = want == Some(Variant::Full) && live == Variant::Lean;
        if !upgrade {
            return Ok((worker, rpc));
        }
        // Same teardown the crash path uses — the app re-renders and the store rebuilds.
        worker.terminate();
        WORKER.with(|w| *w.borrow_mut() = None);
    }
    let variant = want.unwrap_or(Variant::Full);
    let opts = web_sys::WorkerOptions::new();
    opts.set_type(web_sys::WorkerType::Module);
    let url = format!("{}{}/geom-worker.js", bundle_base(), variant.dir());
    let worker = web_sys::Worker::new_with_options(&url, &opts)
        .map_err(|_| anyhow!("geometry worker failed to start ({url})"))?;
    let rpc = Rpc::attach(
        &worker,
        "geometry worker failed to load — is the worker directory deployed and data-base right?",
    );
    info!("fab-gui geom worker: {}", variant.label());
    WORKER.with(|w| *w.borrow_mut() = Some((worker.clone(), rpc.clone(), variant)));
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
        let (worker, rpc) = get_worker(wanted_variant(&req))?;
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
        let (response, logs) = geomsg::decode_reply(&js_sys::Uint8Array::new(&out).to_vec())?;
        // W.3.16: the worker's captured tracing → the Full console (the main-thread subscriber can't
        // see the worker's separate wasm context).
        for line in logs {
            crate::console::push(crate::console::Kind::Log, line);
        }
        Ok(response)
    }
}
