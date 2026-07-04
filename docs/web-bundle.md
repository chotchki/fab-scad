# fab-web bundle contract (18.8)

The interface between fab-scad releases and hotchkiss-io's build. Shape deliberately mirrors
hotchkiss-io's Tailwind CLI fetch (pinned version const → GitHub release asset URL →
version-keyed cache in OUT_DIR), with ONE difference: the bundle is multiple files, so the
asset is a single tar.gz the site build unpacks.

## The asset

- URL: `https://github.com/chotchki/fab-scad/releases/download/web-v<version>/fab-web-<version>.tar.gz`
- Tag scheme `web-vX.Y.Z` — web releases are DECOUPLED from native tags while the native channel
  is parked (18.2); pushing the tag runs `.github/workflows/release-web.yml` and attaches the
  bundle to a prerelease. Converge with native `v*` tags at build-out if it ever matters.
- Arch-independent (wasm), one asset per release.
- Payload: the fab-web slicer (upload STL/3mf/scad → auto-plan → edit connectors → slice →
  Bambu 3mf download, all client-side).

## Archive layout (flat — extract straight into a version-keyed dir)

| file                  | what                                                              |
| --------------------- | ----------------------------------------------------------------- |
| `fab_web.js`          | wasm-bindgen glue, `--target web` (ES module, default-export `init`) |
| `fab_web_bg.wasm`     | the app, wasm-opt'd                                               |
| `fab_web_bg.wasm.br`  | brotli -q 11 of the above — serve precompressed, NEVER recompress |
| `fab_web_bg.wasm.gz`  | gzip -9 fallback variant                                          |
| `manifest.json`       | `{version, entry, wasm, sha256:{...}}` — assert the contract at build time |
| `index.reference.html`| a working loader page to CRIB FROM — the site owns the real document |

## Known limitation: Safari + deeply recursive .scad

Safari's JavaScriptCore gives WebAssembly less call-stack headroom than V8. Ordinary models
(incl. multi-include BOSL2 parts) render fine; DEEPLY recursive ones (heavy attachable/
tag_diff nesting) throw `RangeError: Maximum call stack size exceeded` at render — the SAME
bundle renders them in Chrome/Edge/Firefox. Verified against byte-identical stages and three
snapshot pins (2026.06.12/06.21/07.01): not a deploy issue, not fixable by pin-shopping. The
app's error message names the workaround; a custom wasm build with a larger baked stack is
the open lead (backlog). Hosting pages may want a one-line hint near the app.

## The geometry worker (C.2 — live)

`geom/` ships fab-geom: the kernel (Manifold weld/plan/slice/export/section) as its OWN ~1 MB
wasm, run in a web worker over a bincode byte envelope. The APP wasm carries no kernel at all
— every geometry op is async, the UI pulses live through 100k-tri slices that used to freeze
the tab. Members: `geom/geom-worker.js` (MIT glue), `geom/fab_geom.js`, `geom/fab_geom_bg.wasm`
(+ .br/.gz). Lazily created on first model open; resolved against `data-base` like everything
else. Serve the whole tree — a missing `geom/` breaks ALL loading, visibly, in the panel.

## The OpenSCAD side-module (Phase B — live)

`.scad` files render in-browser: the bundle's `openscad/` dir carries the UNMODIFIED official
OpenSCAD wasm build (10.7 MB + `.br`), our MIT worker glue (`openscad-worker.js`), a GPL
notice, and `libs.json` — BOSL2 (the libs/ submodule pin, v2.0.746) + scad-lib (the app's
commit) as one path→text pack the worker writes into its virtual FS (`/libraries` is
OpenSCAD's default search path; `include <BOSL2/std.scad>` just works). Everything under
`openscad/` is fetched LAZILY on the first .scad open — STL/3mf users never download it.
Members are DOCUMENT-RELATIVE like the rest of the bundle (the app spawns
`openscad/openscad-worker.js` relative to the page).

**Licensing, decided consciously:** the GPL module ships unmodified in its own worker with
notice + source links (`openscad/OPENSCAD-NOTICE.txt`); the page-level combination conveys
under GPL terms while fab's own files stay MIT (GPL-compatible — the dossier's calculus). The
hosting page SHOULD show an "OpenSCAD (GPL)" attribution line near the app; measured worker
cost on a real part: ~10 s first render including the lazy fetch, seconds after.

## Rules the bundle promises (server side counts on these)

- **The document must provide `<canvas id="fab-web">`** — the app binds to it (panics if
  missing) and `fit_canvas_to_parent` tracks the parent's size, so the page owns layout.
  `index.reference.html` shows the minimal working shape.
- **Page chrome clearance:** the app's panel sits top-left inside the canvas; a page whose
  chrome (back button etc.) overlays that corner declares its height once —
  `<canvas id="fab-web" data-inset-top="44">` — and the panel starts below it. Default 44 px.
- **Bundle base:** runtime fetches (the lazy `openscad/` module) default to DOCUMENT-relative,
  which only works when the page lives inside the bundle dir. A page that mounts the bundle at
  a versioned path declares it once — `data-base="/3d/editor/<version>/"` — the same path the
  page already writes into its import statement.

- **Relative fetches only.** The glue resolves `fab_web_bg.wasm` off `import.meta.url`, so the
  bundle works under ANY mount path (`/apps/fab/...`). Nothing in the bundle assumes an origin.
- **No inline eval / no external hosts** — CSP-friendly, works under COEP `require-corp`.
- The wasm is served with `Content-Type: application/wasm` (the glue uses
  `WebAssembly.instantiateStreaming`; wrong MIME falls back slow or fails).

## Rules the server promises (the 18.6 gate proves them)

- COOP `same-origin` + COEP `require-corp` on the TOP-LEVEL app document (future wasm-threads
  headroom; the v1 bundle is single-threaded and doesn't need them to run).
- Precompressed `.br`/`.gz` served via content negotiation; `application/wasm` EXCLUDED from
  on-the-fly compression (keeps Content-Length + ranges).
- Immutable caching keyed by version (the site's `?cb=` convention or the versioned dir path).

## Local iteration (don't ship a release to test a change)

`./packaging/web/dev.sh` — build → bindgen → stage `target/fab-web/stage/` in the contract
shape → serve http://127.0.0.1:8787/ with PROD headers (COOP/COEP, `application/wasm`,
no-store). Verified: `crossOriginIsolated === true` and the app boots under `require-corp`,
so header-dependent behavior fails here, not on the special page. `--stage-only` refreshes the
dir and exits. Dev deltas from the released artifact, on purpose: no wasm-opt and fast
compression levels — the file SET still matches, so consumers need no special-casing.

For the hotchkiss-io side of the loop: give build.rs a local override, e.g.
`FAB_WEB_LOCAL=/Users/chotchki/workspace/fab-scad/target/fab-web/stage` copies that dir
instead of fetching the pinned release — iterate both repos at seconds-scale, drop the env
var to go back to the pin. (Same escape-hatch shape as pin-with-override everywhere else.)

## CI

`.github/workflows/release-web.yml`, live now: push a `web-v*` tag → build (wasm32) →
`wasm-bindgen --target web --out-name fab_web` (CLI pinned to the Cargo.lock crate version) →
`wasm-opt -Oz` → brotli -q 11 / gzip -9 → manifest.json (sha256s) → tar → GitHub prerelease.
Manual dispatch produces the same tar as a CI artifact without a release. Checkout skips
submodules on purpose — the wasm build needs none, and models/ is a private repo (the reason
ci.yml has been red since June).
