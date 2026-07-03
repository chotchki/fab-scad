// fab's OpenSCAD worker (MIT, ours) — drives the UNMODIFIED official OpenSCAD wasm build like
// a subprocess: scad text (+ include files) in, STL bytes out. The GPL module stays its own
// artifact in its own worker; this glue is the arm's-length seam. A fresh instance per job on
// purpose: a crashed/OOM'd render dies alone (the playground reinstantiates for the same
// reason). Message: {id, source, files?: {path: text|bytes}, args?: [..]} →
// {id, ok, stl?, error?, logs}.
import OpenSCAD from "./openscad.js";

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
    // Manifold is the default backend in current snapshots — no flag needed.
    const code = callMain(inst, ["/input.scad", "-o", "/out.stl", ...args]);
    if (code !== 0) throw new Error(`openscad exited ${code}: ${logs.slice(-5).join(" | ")}`);
    const stl = inst.FS.readFile("/out.stl");
    self.postMessage({ id, ok: true, stl, logs }, [stl.buffer]);
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
