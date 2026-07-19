# Browser perf head-to-head — fab-scad-wasm vs OpenSCAD-wasm (W.3.17)

The native harness (`tests/models_harness.rs`) compares fab-scad against the OpenSCAD *binary*. This
bench answers a different question the blog needed: how do the two compare **in the browser**, where
OpenSCAD ships single-threaded WASM and fab keeps its rayon pool over `wasm-bindgen-rayon`? Both engines
render the same models in the same headless Chrome, wall-clocked the SAME way.

## Methodology (why the numbers are fair)

- **Render-only, cold, in a worker.** Each timed span is source-in → mesh-out. fab renders through a
  bench-gated `render_scad_stl` export (the same `handle_with_store` → `RenderWhole` full-res path the
  app uses) with a **fresh `SolidStore` per call** — so no cross-render CSG cache, a cold render every
  time. OpenSCAD makes a fresh Emscripten instance per render (no cache either). Cold-vs-cold, matching
  the native harness's one-render-per-model.
- **Warm ≠ this.** fab's content-addressed CSG cache makes a repeated render near-instant (the slider
  win) — that's real, but it's fab's *interactive* path, not a first render, so it's deliberately OFF
  here. An early run left it on and inflated fab 8-20× on heavy models; the fresh-store fix removed it.
- **Threads on.** Served under COOP/COEP (`bench-server.py`) so SharedArrayBuffer is available and
  `initThreadPool` gives fab its threads (16 on this box). OpenSCAD's WASM is single-threaded as it ships.
- **Instantiation is noise.** OpenSCAD's per-render round-trip (`osc_rt_ms`) sits ~30ms above render-only
  — Chrome caches the compiled 10MB module, so re-instantiation is cheap. Reported render-only regardless.
- **bowtie is skipped.** `bowtie/second_approach.scad` `import()`s an external SVG asset the bench
  doesn't mount, so BOTH engines fail on it. It stays the honest loss in the *native* table.

## Results (cold, 2026-07-19, 16 threads, Chrome headless)

| model | fab-scad | OpenSCAD-wasm | ratio | note |
|---|---|---|---|---|
| corner_brace | 113 ms | 1,980 ms | 17.5× | |
| angled_laptop_holder | 253 ms | 4,436 ms | 17.5× | |
| ashtray | 966 ms | 3,184 ms | 3.3× | |
| garage_door | 11.2 s | 36.7 s | 3.3× | |
| pill_holder | 6.3 s | **stack overflow** | — | OpenSCAD renders it fine NATIVELY (14s) — the browser-only failure |
| traced_holder | 3.9 s | **timeout (>90 s)** | — | OpenSCAD times out natively too (>30s); too slow for it everywhere |

Read: WASM taxes both engines (fab is 4-5× slower in-browser than native on heavy models — no SIMD,
float-heavy booleans). The gap is widest on light models (fab stays fast while OpenSCAD-wasm pays its
single-thread tax → ~17×) and compresses on heavy shared ones (~3×, both dragging the same anchor). The
real story is the bottom two rows: OpenSCAD-wasm stops finishing while fab renders. Raw data in
`results-cold.json`.

## Reproduce

```sh
# 1. recover OpenSCAD-wasm (GPL snapshot, SHA-pinned; gitignored) + the MIT worker glue
curl -fsSL -o /tmp/osc.zip https://files.openscad.org/snapshots/OpenSCAD-2026.07.01-WebAssembly-web.zip
echo "10e0937dd2627116a1f7ab6fb1ed75762b66572f21f4e5054c7dc00526cdd1ff  /tmp/osc.zip" | shasum -a 256 -c -
unzip -o /tmp/osc.zip -d perf/browser/vendor          # openscad.js + openscad.wasm
python3 perf/browser/pack_libs.py perf/browser/vendor/osc-libs.json

# 2. build fab's threaded + bench-gated worker
bash perf/browser/build-fab-wasm.sh

# 3. run (headless Chrome; ONLY=<name> to smoke one model)
bash perf/browser/run.sh
```
