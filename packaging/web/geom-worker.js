// fab's geometry worker (MIT, ours): the kernel-only fab-geom wasm behind a byte envelope —
// {id, buf} in, {id, ok, buf|error} out. Requests queue behind init; the app keeps one call
// in flight. Buffers transfer, not copy.
import init, { handle, initThreadPool } from "./fab_geom.js";
// Re-arm init on failure: a transient wasm-fetch error must not poison the worker for good.
// W.6: after the module inits, spin up fab-manifold's rayon pool (wasm-bindgen-rayon) so the kernel's
// booleans run threaded. Needs a cross-origin-isolated (COOP/COEP) page for SharedArrayBuffer.
let ready = null;
const ensure = () =>
  (ready ??= init()
    .then(() => initThreadPool(navigator.hardwareConcurrency))
    .catch((e) => {
      ready = null;
      throw e;
    }));
self.onmessage = async (e) => {
  const { id, buf } = e.data;
  try {
    await ensure();
    const out = handle(new Uint8Array(buf));
    self.postMessage({ id, ok: true, buf: out.buffer }, [out.buffer]);
  } catch (err) {
    self.postMessage({ id, ok: false, error: `${err}` });
  }
};
