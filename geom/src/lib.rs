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

use wasm_bindgen::prelude::*;

use fab_scad::geomsvc::{SolidStore, handle_with_store};

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
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
    fab_scad::geomsg::encode_response(&response)
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
        assert!(matches!(
            geomsg::decode_response(&out).unwrap(),
            Response::Failed { .. }
        ));
    }
}
