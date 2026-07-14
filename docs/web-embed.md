# fab-gui web embedding contract (W.3.7)

The runtime interface between the **fab-gui wasm bundle** (this repo owns + releases it) and its
**host page** (hotchkiss.io, `/3d/editor`). The build-artifact side — tar.gz layout, tag scheme,
sha256 — lives at the bottom under [The bundle](#the-bundle); this doc leads with the RUNTIME contract
because that's the part both sides have to agree on frame-by-frame: what the page hands the wasm, and
what the wasm hands back.

Supersedes the fab-web contract in `web-bundle.md` (fab-web is retired at W.3.11). The shape is the
same — a pinned GitHub release the site's `build.rs` fetches — but the payload is the ONE fab-gui app
(desktop + web from one codebase, W.3), and the OpenSCAD side-module is GONE (scad-rs+Manifold render
everywhere now).

## The boundary in one picture

```
  hotchkiss.io page  (owns: outer chrome, layout, headers, boot splash)
  │
  ├─ <canvas id="fab-gui" data-base="/3d/editor/" data-inset-top="64">
  │     └─ init() from fab_gui.js  ── binds Bevy to the canvas, boots the app
  │
  ├─ serves the bundle tree from data-base (fab_gui_bg.wasm[.br], geom/, libs.json)
  │
  └─ listens for  document → CustomEvent "fab-gui:ready"  ── removes the splash
        ▲                                              │
        └──────────────  the wasm  ────────────────────┘
             renders the tool INTO the canvas · reads data-* config ·
             emits ready + console health · owns file-open + download (Blob)
```

## What the HOST must provide

### 1. The mount — one canvas, declared before `init()`

```html
<canvas id="fab-gui"
        data-base="/3d/editor/"
        data-inset-top="64"></canvas>
```

- **`id="fab-gui"`** — Bevy's `WindowPlugin` binds to this exact selector (`scene.rs::window_plugin`);
  a missing/misnamed canvas is a boot panic, not a blank page. (Renamed from `fab-web` — the id is a
  contract symbol, and a fab-gui app answering to `#fab-web` is a trap for the next reader.)
- **`data-base`** *(required)* — the URL prefix every bundle member resolves against: the app wasm,
  the `geom/` worker + its wasm, and `libs.json`. Document-relative if omitted, but the site should set
  it explicitly so the app and its lazily-fetched worker agree even under a rewritten path. MUST end in
  `/` (the app appends one if not).
- **`data-inset-top`** *(reserved, px)* — declared for a host that wants the app to inset its own top
  chrome under an OVERLAY header. The app does NOT read it today; the preferred framing sizes the canvas
  PARENT to sit below the site header (see below), so the app never needs to inset itself. Kept in the
  contract so an overlay-chrome host has a hook when one implements the app-side inset.

The canvas PARENT sizes the app: `fit_canvas_to_parent` tracks the parent's box, so the site controls
layout by sizing the wrapper, not the canvas. The clean frame is a flex column — a fixed site header,
then a `flex:1` stage holding the canvas (give it `min-height:0`); the app fills the stage, its tab-bar
landing directly under the header with no overlap and no inset attribute.

### 2. Load the module

```html
<script type="module">
  const base = document.getElementById('fab-gui').dataset.base ?? './';
  const { default: init } = await import(`${base}fab_gui.js`);
  init().catch((e) => {
    // winit exits App::run via a thrown JS control-flow exception — expected, NOT a crash.
    if (!`${e}`.includes('Using exceptions for control flow')) console.error('fab-gui init:', e);
  });
</script>
```

### 3. Serve the whole bundle tree — with the right headers

Everything under `data-base` must be reachable; a missing `geom/` breaks ALL geometry, visibly.

| header | value | why |
| --- | --- | --- |
| `Content-Type` (`.wasm`) | `application/wasm` | streaming compile (`instantiateStreaming`) |
| `Content-Encoding` (`.wasm.br`) | `br` | serve the precompressed variant — NEVER recompress at the edge |
| `Cross-Origin-Opener-Policy` | `same-origin` | **kept** — no threads today, but the manifold-rs rewrite (multithreaded Manifold on web via `SharedArrayBuffer`) needs cross-origin isolation, and it's free to keep now |
| `Cross-Origin-Embedder-Policy` | `require-corp` | same |
| `Cache-Control` | immutable, long max-age | the wasm is content-hashed per release; the version-keyed path makes it safe to cache hard |

The bundle is self-contained: fonts are `include_bytes!`, no runtime asset fetch beyond the members
above. No CDN, no external origin.

### 4. Own the outer chrome + the boot splash

The site owns the header, the back-nav, and the page document. The app draws ONLY the tool surface into
the canvas (its wordmark is dropped in embed mode — the site already says where you are).

The **boot splash** is the site's, not the app's: an ~8 MiB bevy bundle takes real seconds to download +
instantiate, and a blank canvas reads as broken. Show a themed (navy/gold) splash over the canvas from
page load, and remove it on the ready signal below. This is the condition for accepting the bundle size
([[gui-reactive-standard]] — never a blank page).

## What the WASM provides back

### 1. Renders the whole tool into the canvas

The full app — Model / Parts / Orientation / Export tabs + the 3D viewport — identical to desktop
(that's the W.3 bet). Geometry runs in the `geom/` Web Worker; the UI stays live through it.

### 2. Reads its config from the canvas `data-*`

`data-base` (member root) is the whole config surface the app reads today (`data-inset-top` is reserved,
above). Query params (`?demo`, `?model=`) and `localStorage` (last file, camera) are the natural next
knobs but are OUT of scope until the site asks for them; the contract stays small on purpose.

### 3. Emits three signals

| signal | channel | when | for |
| --- | --- | --- | --- |
| **`fab-gui:ready`** | `document` `CustomEvent` | the FIRST egui frame paints | the host removes its boot splash |
| **`fab-gui render complete: N part(s)`** | `console.info` | every whole-render finishes (N ≥ 1) | the release boot gate + a health heartbeat |
| Rust panic | `console.error` | on any panic | `console_error_panic_hook` — a bare wasm trap is otherwise opaque |

`fab-gui:ready` fires at "the app is VISIBLE" (first frame), NOT "geometry is done" — the worker
round-trip takes a second or two after, and the app shows its OWN loading pulse for that (the reactive
standard). So the splash lifts the moment the UI is up; geometry filling in is the app's job to signal
in-canvas. The host just:

```js
document.addEventListener('fab-gui:ready', () => splash.remove(), { once: true });
```

### 4. Owns file-open and download — the host provides nothing

- **Open**: an `<input type=file>` the app creates (rfd on wasm) → reads BYTES (no filesystem). `.scad`
  in, and its `fab:config` block (W.3.8) rehydrates the slicing state.
- **Save / Export**: the app builds a `Blob` and triggers an anchor-click download — the browser's own
  save dialog. Save writes the `.scad` with the live `fab:config` block baked in; Export writes the
  Bambu multi-plate `.3mf`. The host wires up nothing; it just must not sandbox downloads away.

## The bundle

Pinned GitHub release, fetched + sha256-verified + RustEmbedded by hotchkiss.io's `build.rs` — the same
mechanism as the Tailwind CLI fetch, one tar.gz because the bundle is many files.

- **Tag**: `web-vX.Y.Z` (continues the fab-web scheme — this IS the web bundle, just a new payload; the
  site pin is a one-line bump). Pushing the tag runs `.github/workflows/release-web.yml`.
- **Asset**: `https://github.com/chotchki/fab-scad/releases/download/web-vX.Y.Z/fab-gui-X.Y.Z.tar.gz`
- **Arch-independent** (wasm), one asset per release.
- **Size** (first fab-gui build, W.3.7): the app is **8.69 MiB brotli** (47.4 MB raw, wasm-opt `-Oz`)
  plus a 2.83 MB geom worker — comparable to fab-web's 8.13 MiB, and it DROPS the separate OpenSCAD
  module fab-web shipped. The ~8 MiB is the accepted cost; the host's boot splash covers the download.

### Archive layout (extract straight into a version-keyed dir)

| path | what |
| --- | --- |
| `fab_gui.js` | wasm-bindgen glue, `--target web` (ES module, default-export `init`) |
| `fab_gui_bg.wasm` | the app, wasm-opt `-Oz` |
| `fab_gui_bg.wasm.br` / `.gz` | brotli-11 / gzip-9 precompressed — serve one, never recompress |
| `geom/fab_geom.js` | the kernel worker's wasm-bindgen glue |
| `geom/fab_geom_bg.wasm` (`.br`/`.gz`) | the Manifold kernel wasm (~1.7 MB), run in the worker |
| `geom/geom-worker.js` | the worker entry (GPL, ours) — bincode byte envelope over the seam |
| `libs.json` | BOSL2 + scad-lib + the demo, packed once; the app computes each model's include closure from it |
| `manifest.json` | `{version, entry, wasm, sha256:{path→hash}}` — the build asserts this contract |
| `index.reference.html` | a WORKING loader to crib from — the site owns the real document |

No `openscad/` dir — scad-rs renders `.scad` in-process (in the worker). That's the payload cut vs
fab-web.

### The boot gate (non-negotiable)

`release-web.yml` never ships an artifact nobody executed (the `web-v0.1.0` lesson). It serves the exact
staged bundle, loads `index.reference.html?demo` in headless Chrome, and requires:

- NO `panicked` / `RuntimeError` / `could not grow` in the console (hard fail), AND
- the `fab-gui render complete: N part(s)` heartbeat with N ≥ 1 (the demo rendered end-to-end THROUGH
  the worker — the whole point of the web build).

Real wall-clock poll, not `--virtual-time-budget`: the demo plans through the geom worker, and virtual
time races real worker threads (relearned the hard way on fab-web 0.11.0).

## Decisions (settled)

1. **Canvas id is `fab-gui`** (renamed from `fab-web`) — the id outlives the fab-web project, so it
   names the app it actually mounts. App side: `window_plugin` + `geom_wasm::bundle_base`.
2. **`fab-gui:ready` fires on the first themed egui frame** — the splash lifts the moment the UI is
   visible; the worker round-trip that fills in geometry is covered by the app's own in-canvas loading
   pulse (the reactive standard), not by holding the splash.
