// fab's geometry worker (MIT, ours): the kernel-only fab-geom wasm behind a byte envelope —
// {id, buf} in, {id, ok, buf|error} out. Requests queue behind init; the app keeps one call
// in flight. Buffers transfer, not copy.
import init, { handle } from "./fab_geom.js";
// Re-arm init on failure: a transient wasm-fetch error must not poison the worker for good.
let ready = null;
const ensure = () => (ready ??= init().catch((e) => { ready = null; throw e; }));
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
