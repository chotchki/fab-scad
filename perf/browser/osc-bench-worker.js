// W.3.17 OpenSCAD bench worker: the recovered fab MIT glue (vendor/openscad-worker.js) + render-only
// timing. A FRESH OpenSCAD instance per job (the shipped fab-web design — crash/OOM isolation, and
// exactly how the browser tool loaded it), so the round-trip the page times includes module
// instantiation. `renderMs` here is the callMain-only span, so the page can report render-only too.
// {id, source, files?} -> {id, ok, len?, renderMs?, error?, logs}.
import OpenSCAD from "./vendor/openscad.js";

self.onmessage = async (e) => {
  const { id, source, files = {}, args = [] } = e.data;
  const logs = [];
  const log = (t) => {
    logs.push(`${t}`);
    if (logs.length > 500) logs.shift();
  };
  try {
    const inst = await OpenSCAD({ noInitialRun: true, print: log, printErr: log });
    for (const [path, data] of Object.entries(files)) {
      mkdirs(inst, path);
      inst.FS.writeFile(path, data);
    }
    inst.FS.writeFile("/input.scad", source);
    const t0 = performance.now();
    const code = callMain(inst, ["/input.scad", "-o", "/out.stl", ...args]);
    const renderMs = performance.now() - t0;
    if (code !== 0) throw new Error(`openscad exited ${code}: ${logs.slice(-5).join(" | ")}`);
    const stl = inst.FS.readFile("/out.stl");
    self.postMessage({ id, ok: true, len: stl.length, renderMs, logs });
  } catch (err) {
    self.postMessage({ id, ok: false, error: `${err}`, logs });
  }
};

// Emscripten's exit() surfaces as a thrown ExitStatus even on success — normalize to a code.
function callMain(inst, args) {
  try {
    const r = inst.callMain(args);
    return r === undefined ? 0 : r;
  } catch (err) {
    if (err && err.name === "ExitStatus") return err.status;
    throw err;
  }
}

function mkdirs(inst, path) {
  const parts = path.split("/").filter(Boolean).slice(0, -1);
  let dir = "";
  for (const p of parts) {
    dir += "/" + p;
    try {
      inst.FS.mkdir(dir);
    } catch {
      /* exists */
    }
  }
}
