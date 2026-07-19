// fab's geometry worker (MIT, ours): the kernel-only fab-geom wasm behind a byte envelope —
// {id, buf} in, {id, ok, buf|error} out. Requests queue behind init; the app keeps one call
// in flight. Buffers transfer, not copy.
//
// NAMESPACE import (not `{ initThreadPool }`): wasm-bindgen-rayon exports `initThreadPool` ONLY in a
// threaded (`par`) build. A NAMED import of a missing export is a module-LOAD SyntaxError ("worker
// failed to load") — so we import the namespace and look the symbol up dynamically, tolerating its
// absence in a non-threaded build.
import init, * as glue from "./fab_geom.js";
// Re-arm init on failure: a transient wasm-fetch error must not poison the worker for good.
// W.6: after the module inits, spin up fab-manifold's rayon pool (wasm-bindgen-rayon) so the kernel's
// booleans run threaded — needs a cross-origin-isolated (COOP/COEP) page for SharedArrayBuffer.
// BEST-EFFORT (W.5.9 runtime finding): a non-threaded build has no `initThreadPool`, and a mis-shared
// memory makes the pool spin-up throw `DataCloneError` when it postMessages the wasm Memory to its
// rayon sub-workers. Either way, fall back to SERIAL geometry rather than bricking the worker — the
// module still `handle`s requests, just single-threaded. (Perf, not correctness; threading is W.6.2.)
let ready = null;
const ensure = () =>
  (ready ??= init()
    .then(async () => {
      const spin = glue.initThreadPool;
      if (typeof spin === "function") {
        try {
          await spin(navigator.hardwareConcurrency);
        } catch (e) {
          console.warn(`fab-geom: parallel pool unavailable, running serial: ${e}`);
        }
      }
    })
    .catch((e) => {
      ready = null;
      throw e;
    }));
self.onmessage = async (e) => {
  const { id, buf } = e.data;
  try {
    await ensure();
    const out = glue.handle(new Uint8Array(buf));
    self.postMessage({ id, ok: true, buf: out.buffer }, [out.buffer]);
  } catch (err) {
    self.postMessage({ id, ok: false, error: `${err}` });
  }
};
