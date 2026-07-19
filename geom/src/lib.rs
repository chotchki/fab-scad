//! fab-geom: the geometry worker's wasm — `geomsvc` behind one byte-envelope export. No bevy, no
//! app, no side effects beyond the panic hook: safe to instantiate inside a Web Worker, and ~1 MB
//! instead of the 44 MB app wasm. Failures never cross as exceptions — the service encodes them as
//! `Response::Failed`.
//!
//! STATEFUL (W.3.6): one persistent `SolidStore` per worker instance, so fab-gui's handle flow works
//! across postMessage calls — `RenderParts` MINTS base handles that a later `Reslice`/`CrossSection`/
//! `AutoPlan`/`PrintLayout` READS. fab-web's stateless `Analyze`/`Slice`/`Export` arms simply never
//! touch the store, so this is backward-compatible. `!Send` Solids stay in the store, never crossing
//! the byte envelope. A hard C++ `bad_alloc` traps the worker (isolated here, never in the app);
//! recovery is a re-created worker (fresh store) — held handles then miss and the app re-renders.

use std::cell::RefCell;
use std::sync::Mutex;

use wasm_bindgen::prelude::*;

use fab_scad::geomsvc::{SolidStore, handle_with_store};

// W.3.16: the worker's captured `tracing` for the app's "Full" console. The wasm Worker is a separate
// context, so its logs can't reach the main-thread subscriber — `handle()` drains this into the reply
// envelope each call. A process global (not thread_local) so the `par` build's rayon threads land here
// too. Capped so a long session can't grow it unbounded.
static WORKER_LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A minimal `tracing::Subscriber` (events only, no spans) that mirrors each event into [`WORKER_LOGS`]
/// as `LEVEL target: message`, matching the app-side console format. Hand-rolled instead of pulling
/// `tracing-subscriber` into the ~1 MB worker wasm. TRACE is dropped (too chatty for a console).
#[cfg(target_arch = "wasm32")]
struct Capture;
#[cfg(target_arch = "wasm32")]
impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        if *meta.level() == tracing::Level::TRACE {
            return;
        }
        let mut grab = MessageGrab(String::new());
        event.record(&mut grab);
        if grab.0.is_empty() {
            return;
        }
        let line = format!("{} {}: {}", meta.level(), meta.target(), grab.0);
        let mut logs = WORKER_LOGS.lock().unwrap_or_else(|e| e.into_inner());
        logs.push(line);
        let over = logs.len().saturating_sub(4000);
        if over > 0 {
            logs.drain(0..over);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[cfg(target_arch = "wasm32")]
struct MessageGrab(String);
#[cfg(target_arch = "wasm32")]
impl tracing::field::Visit for MessageGrab {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

// W.6: carry wasm-bindgen-rayon's `initThreadPool` JS export into THIS cdylib (the worker's wasm) so
// the worker JS can `await initThreadPool(...)` before the first `handle()` call. Present only on the
// threaded browser build (`par`); native + serial wasm don't have it.
#[cfg(all(feature = "par", target_arch = "wasm32", target_os = "unknown"))]
pub use fab_scad::init_thread_pool;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    // W.3.16: capture this worker's tracing so `handle()` can ship it back for the Full console.
    let _ = tracing::subscriber::set_global_default(Capture);
}

thread_local! {
    // Shard 0 — this is the single geom worker (N=1). Persists for the worker's lifetime.
    static STORE: RefCell<SolidStore> = RefCell::new(SolidStore::new(0));
}

#[wasm_bindgen]
pub fn handle(request: &[u8]) -> Vec<u8> {
    let response = match fab_scad::geomsg::decode_request(request) {
        Ok(req) => STORE.with(|s| handle_with_store(&mut s.borrow_mut(), req)),
        Err(e) => fab_scad::geomsg::Response::Failed {
            error: format!("{e:#}"),
        },
    };
    // Drain the tracing this call captured → ship it back in the reply for the Full console (W.3.16).
    // Empty on native (no subscriber set there; native worker tracing rides the app's shared subscriber).
    let logs = std::mem::take(&mut *WORKER_LOGS.lock().unwrap_or_else(|e| e.into_inner()));
    fab_scad::geomsg::encode_reply(&response, &logs)
}

/// W.3.17 bench export (the browser perf head-to-head, blog p2): render one whole model to STL bytes
/// through the SAME path the app uses (`handle_with_store` → `RenderWhole`, full-res), but with a
/// JS-friendly signature — `main` is the .scad source, `libs_json` the `{path: text}` include pack
/// (fab's `libs.json`). Lets a harness time a render without hand-rolling the bincode `Request`. Runs on
/// the caller (drive it FROM a worker so the `par` build's rayon join can block). `bench`-gated: absent
/// from the shipped worker. Ok(stl) on success; a JS exception carrying the eval/kernel error otherwise.
#[cfg(feature = "bench")]
#[wasm_bindgen]
pub fn render_scad_stl(main: &str, libs_json: &str) -> Result<Vec<u8>, JsError> {
    use fab_scad::geomsg::{Request, Response, Source};
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(libs_json).map_err(|e| JsError::new(&format!("libs parse: {e}")))?;
    let libs = map.into_iter().map(|(k, v)| (k, v.into_bytes())).collect();
    let req = Request::RenderWhole {
        source: Source::Bytes {
            main: main.as_bytes().to_vec(),
            libs,
        },
        root: None,
        preview: false,
    };
    match STORE.with(|s| handle_with_store(&mut s.borrow_mut(), req)) {
        Response::Rendered { stl, .. } => Ok(stl),
        Response::Failed { error } => Err(JsError::new(&error)),
        _ => Err(JsError::new("render_scad_stl: unexpected response variant")),
    }
}

#[cfg(test)]
mod tests {
    use fab_scad::geomsg::{self, Request, Response};

    #[test]
    fn envelope_round_trips() {
        let req = geomsg::encode_request(&Request::Analyze {
            name: "t.stl".into(),
            bytes: vec![1, 2, 3],
            bed: [40.0; 3],
        });
        let out = super::handle(&req);
        let (resp, _logs) = geomsg::decode_reply(&out).unwrap();
        assert!(matches!(resp, Response::Failed { .. }));
    }

    #[test]
    fn reply_envelope_carries_logs() {
        // The Full-console path (W.3.16): the worker's captured tracing must survive the wire.
        let logs = vec![
            "ECHO: 5".to_string(),
            "INFO fab_lang: [csg-cache] …".to_string(),
        ];
        let bytes = geomsg::encode_reply(&Response::Freed, &logs);
        let (resp, back) = geomsg::decode_reply(&bytes).unwrap();
        assert!(matches!(resp, Response::Freed));
        assert_eq!(back, logs);
    }
}
