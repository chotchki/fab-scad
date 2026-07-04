//! fab-geom: the geometry worker's wasm — `geomsvc::handle` behind one byte-envelope export.
//! No bevy, no app, no side effects beyond the panic hook: safe to instantiate inside a Web
//! Worker, and ~1 MB instead of the 44 MB app wasm. Failures never cross as exceptions — the
//! service encodes them as `Response::Failed`.

use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub fn handle(request: &[u8]) -> Vec<u8> {
    let response = match fab_scad::geomsg::decode_request(request) {
        Ok(req) => fab_scad::geomsvc::handle(req),
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
