//! 18.4 GO/NO-GO: prove `kernel::Solid` (manifold-csg `unstable-wasm-uu`, C++ Manifold via
//! wasm-cxx-shim) instantiates and RUNS under wasm-bindgen — boolean + cross-section, the two
//! ops the whole slicing pipeline hangs off. Compile success means nothing here; the risk is
//! runtime (missing C-runtime imports, aborts from implicit STL throws).

use fab_scad::kernel::Solid;
use wasm_bindgen::prelude::*;

/// Boolean a through-hole out of a cube, cross-section it, report shape counts.
/// Expected: the z=0 profile of a holed cube is exactly 2 loops (outline + hole).
#[wasm_bindgen]
pub fn kernel_smoke() -> String {
    let outer = Solid::cube(20.0, 20.0, 20.0, true);
    let hole = Solid::cube(10.0, 10.0, 30.0, true);
    let diff = outer.difference(&hole);
    let loops = diff.cross_section(2, 0.0);
    format!("tris={} loops={}", diff.num_tri(), loops.len())
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::*;

    // Run in a real browser (chromedriver); comment out to fall back to Node.
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn kernel_boolean_and_cross_section_run_on_wasm() {
        let s = super::kernel_smoke();
        assert!(s.contains("loops=2"), "holed cube must section to outline+hole: {s}");
    }
}
