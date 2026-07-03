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
- Payload today: the 18.5 probe app (feathers + picking scene) — a REAL artifact in the real
  contract shape so the site integration can land first; the slicer port swaps in at build-out
  without touching the contract or the pipeline.

## Archive layout (flat — extract straight into a version-keyed dir)

| file                  | what                                                              |
| --------------------- | ----------------------------------------------------------------- |
| `fab_web.js`          | wasm-bindgen glue, `--target web` (ES module, default-export `init`) |
| `fab_web_bg.wasm`     | the app, wasm-opt'd                                               |
| `fab_web_bg.wasm.br`  | brotli -q 11 of the above — serve precompressed, NEVER recompress |
| `fab_web_bg.wasm.gz`  | gzip -9 fallback variant                                          |
| `manifest.json`       | `{version, entry, wasm, sha256:{...}}` — assert the contract at build time |
| `index.reference.html`| a working loader page to CRIB FROM — the site owns the real document |

## Rules the bundle promises (server side counts on these)

- **The document must provide `<canvas id="fab-web">`** — the app binds to it (panics if
  missing) and `fit_canvas_to_parent` tracks the parent's size, so the page owns layout.
  `index.reference.html` shows the minimal working shape.
- **Page chrome clearance:** the app's panel sits top-left inside the canvas; a page whose
  chrome (back button etc.) overlays that corner declares its height once —
  `<canvas id="fab-web" data-inset-top="44">` — and the panel starts below it. Default 44 px.

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
