// W.3.17 fab bench worker (browser perf head-to-head, blog p2). Inits the THREADED geom wasm once, then
// times `render_scad_stl` per request. The render runs IN this worker — NOT the page — because
// wasm-bindgen-rayon's rayon join blocks on Atomics.wait, which is illegal on the main thread. Reports
// render-only wall time (the warm-worker number a user feels on every render after the first).
import init, { initThreadPool, render_scad_stl } from "./vendor/geom/fab_geom.js";

(async () => {
  await init();
  let threads = 0;
  try {
    threads = navigator.hardwareConcurrency || 4;
    await initThreadPool(threads);
  } catch (e) {
    // No COOP/COEP (no SharedArrayBuffer) → falls back to serial. Flag it: the comparison wants threads on.
    postMessage({ boot: "warn", msg: "initThreadPool failed (serial fallback): " + e });
    threads = 0;
  }
  postMessage({ boot: "ready", threads });
})();

onmessage = (e) => {
  const { id, main, libs } = e.data;
  const t = performance.now();
  try {
    const stl = render_scad_stl(main, libs); // libs = fab's libs.json TEXT
    postMessage({ id, ok: true, ms: performance.now() - t, len: stl.length });
  } catch (err) {
    postMessage({ id, ok: false, ms: performance.now() - t, error: "" + ((err && err.message) || err) });
  }
};
