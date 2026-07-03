# Deployment spike dossier: DMG/winget vs wasm on hotchkiss.io (Phase 18)

Research notes, 2026-07-03 — written BEFORE the spike, to scope it. Two candidate deployment
modes: native installers (DMG + winget) and a browser build hosted at hotchkiss.io. Both are
plausible; each has exactly one gate that decides it, and the spike's job is to hit those gates
cheaply before committing a build-out phase. Every claim below was adversarially fact-checked
against primary sources (one headline recommendation did NOT survive — see the target-triple
trap); re-verify the fast-moving ones (bevy#22620, manifold-csg) at spike time.

## Where the codebase already stands (audit)

The Track C kernel split did most of the wasm prep without trying:

- `manifold3d` lives in exactly ONE file (`src/kernel.rs`) behind the `kernel` feature; every
  consumer talks to the `Solid` facade. Swapping the kernel backend is a one-file rewrite.
- OpenSCAD is spawned for exactly ONE hot-path job now: `.scad` → base mesh, once per source
  change (`gui/src/fab.rs` render_whole, `src/auto.rs` front-door). Booleans, slab slicing,
  cross-sections, connectors, feasibility, orientation, packing — all in-process already.
  (Also publish artifacts, the legacy codegen slice path and `Edit in OpenSCAD` — all
  cfg-gateable, none load-bearing for a browser subset.)
- Byte twins exist (`from_stl_bytes`/`to_stl_bytes`/`load_stl_bytes`) — the temp-file STL bus
  between stages can become `Vec<u8>` mechanically. The 3mf/Bambu writers need a
  `Cursor<Vec<u8>>` seam (zip already supports it) + a browser download trigger.
- The `!Send Solid` discipline (mesh bytes cross threads, Solids don't) is EXACTLY the web-worker
  postMessage contract. The architecture is already worker-shaped.
- One compile-breaker: `reqwest` blocking is an UNCONDITIONAL lib dep — wasm builds die before
  any of the above matters. Needs a `native` feature seam (mirror of `kernel`) gating
  openscad.rs/publish.rs/project.rs/smoke.rs.
- The minimal browser app needs NO OpenSCAD at all: upload STL → `from_stl_bytes` →
  rotate-to-fit + auto-slice → connector editor on kernel cross-sections → pack → download 3mf.
  That's the whole GUI hot loop, scad-free.

## Wasm path — the two real gates

> **Spike results (2026-07-03). BOTH gates are a GO — the wasm mode is real.**
> Gate 1: the lib gained a `native` feature (18.3 — wasm32 build excludes openscad/publish/
> reqwest, whole native matrix + 106 tests stay green), and `kernel::Solid` booleans +
> cross-sections RAN in headless Chrome on wasm32-unknown-unknown via manifold-csg
> `unstable-wasm-uu` + wasm-bindgen (18.4, spikes/wasm-kernel/ — needs Homebrew `lld` for
> wasm-ld; the facade forwards the feature; `parallel` off on wasm via a target-split dep).
> Release cdylib: 387 KB — the kernel is noise next to Bevy.
> Gate 2 (18.5, spikes/wasm-gui/): bevy#22620 does NOT reproduce on our stack — Bevy 0.19
> feathers + mesh_picking render and fire on BOTH WebGL2 and WebGPU in headless Chrome, no
> flags, no RefCell panic, no uniform-alignment failure. Wire size untrimmed: 55.9 MiB raw /
> 8.5 MiB brotli (ALL default bevy features, no wasm-opt) — build-out must feature-prune bevy
> (drop bevy_audio etc.) + wasm-opt; expect ~6-7 MiB brotli shipping. Port notes (canvas
> binding, init() control-flow-exception catch, bsn! quirks) in the 18.5 agent report +
> spikes/wasm-gui/. A verified spike bundle in the web-bundle.md contract shape sits at
> target/fab-web/fab-web-spike-18.5.tar.gz for the hotchkiss-io gate.
> Bonus channel (18.9): the `fab-scad` crates.io name is free; with the new `include` list the
> package is 103 KiB / verifies clean — `cargo install fab-scad` becomes the from-source
> channel whenever we push the button.

**Gate 1: the kernel target triple.** The naive read ("Manifold has official emscripten wasm,
compile everything to emscripten") is WRONG for us: Bevy supports `wasm32-unknown-unknown`
ONLY — no public evidence anywhere (July 2026) of winit/wgpu/Bevy running under wasm-bindgen's
new `--target emscripten` mode. The realistic options:

1. `manifold-csg` (the crate behind manifold3d 0.3.x) has a provisional `unstable-wasm-uu`
   feature: C++ Manifold on `wasm32-unknown-unknown` via wasm-cxx-shim (LLVM 20+,
   -fno-exceptions, no threads). CI builds it and asserts zero unexpected imports — but the
   author's own design doc names wasm-bindgen end-to-end integration as "the most important
   remaining gap". We would be the ones closing it.
   [design doc](https://github.com/zmerlynn/manifold-csg/blob/main/docs/plans/wasm-unknown-unknown.md)
2. Split modules: Bevy on unknown-unknown + the official `manifold-3d` npm wasm (532 KB kernel,
   maintained by upstream, full API incl. slice/splitByPlane) bridged through JS with typed-array
   mesh copies. Works today, costs a JS glue layer and per-op mesh copies.
3. No pure-Rust fallback: csgrs is self-described experimental, fidget is SDF-not-mesh-CSG,
   truck is stale. Manifold or bust.

Option 1 is the spike target (keeps kernel.rs single-backend); option 2 is the documented
fallback. Bus-factor note: manifold-csg is single-maintainer (~15 stars) — mitigated by upstream
Manifold's health and patches flowing upstream.

**Gate 2: feathers on the web.** `bevy_feathers` is BROKEN in-browser as of Jan 2026
([bevy#22620](https://github.com/bevyengine/bevy/issues/22620), open): winit RefCell panic +
WebGL2 16-byte uniform-alignment pipeline failure. A WebGPU build may sidestep the alignment
half (82% global browser support now, WebGL2 still the compatibility default). If feathers won't
render, the wasm mode is dead until upstream moves — find out in an afternoon, not after a port.

Known costs if both gates pass: single-threaded (Bevy's wasm scheduler is single-threaded
regardless; rayon-on-wasm is nightly-only via wasm-bindgen-rayon — skip it, our rayon is
CLI-only anyway), ~15 MB wasm / ~5-8 MB brotli on the wire (order-of-magnitude, unmeasured),
rfd `pick_file`→bytes replaces the folder picker (directory access is Chromium-only, not v1),
mtime watching dies (content-hash instead — browser sources are in-memory anyway).

**openscad-wasm (stretch, NOT v1):** viable if we ever want .scad-in-browser — official
OpenSCAD-org build, ~13 MB wasm (~3 MB compressed), Manifold backend default since Aug 2025
(use `--backend=manifold` against older pins; `--enable=manifold` is a stale README flag that
silently drops you to CGAL), single-threaded, playground-proven worker + virtual-FS pattern
(~100 lines of glue from the README — don't fork the playground's runner, it's GPL). Licensing:
FSF calls the callMain-a-module pattern "a borderline case"; both real embedders (playground,
CADAM) ship the combination as GPL. MIT is GPL-compatible so worst case the page-level combo
conveys under GPL while our code stays MIT — acceptable, decide consciously in the memo.

## Native path — well-lit, costs money

- **Tool: cargo-packager** (CrabNebula). The ONLY maintained non-tauri tool emitting macOS
  .app+DMG AND Windows NSIS/MSI from one config, multi-binary (`fab` + `fab-gui` in one bundle).
  Slow-release but alive (v0.11.8 2025-11, security commits into 2026). Floor if it stalls:
  hdiutil/create-dmg + cargo-wix scripts. cargo-dist is alive too (v0.32, 2026-05) but does
  archives/MSI only — no .app/DMG, fine for the CLI alone, not the GUI.
- **macOS:** unsigned DMGs are effectively DEAD (Sequoia killed right-click-open; Tahoe reportedly
  time-boxes "Open Anyway" to ~1 h — secondary sources, verify on a real box). Real path:
  Apple Developer Program ($99/yr) + `rcodesign` (pure Rust, runs on Linux CI, notarize via App
  Store Connect API key). Bevy gotcha: assets resolve relative to the EXECUTABLE
  (Contents/MacOS/assets, not Resources) — fix with `AssetPlugin file_path` or a post-bundle copy
  ([bevy#15618](https://github.com/bevyengine/bevy/issues/15618), closed-with-workaround).
- **Windows/winget:** winget does NOT require Authenticode (SHA256-pinned manifests; only MSIX
  must be signed) — an unsigned NSIS installer can ship TODAY, SmartScreen warnings and all.
  Signing later: Azure Artifact Signing, $9.99/mo, US/Canada individuals, needs a PAID Azure
  sub; EV certs lost their SmartScreen bypass in 2024, so never pay the EV premium. winget
  installs partially sidestep SmartScreen (no browser download prompt) — winget-first helps.
  Decide before writing manifests: one installer with PATH registration for the CLI, or `fab` as
  a separate winget "portable" package (renames in winget-pkgs are painful).
- **Do NOT bundle OpenSCAD.** Subprocess exec is arm's-length (no GPL exposure) but bundling
  makes us a GPL distributor (source-serving obligation for the exact nightly). The old
  re-signing argument is DEAD — OpenSCAD macOS nightlies are signed + notarized since ~2025
  ([#5421](https://github.com/openscad/openscad/issues/5421)), and a 2026.01 stable is brewing
  (tag exists, unreleased) — but detect-and-guide (brew/winget install, pin a minimum snapshot,
  probe at startup — `Openscad::discover` already does most of this) stays the cheap clean
  answer.
- **Updates:** no self-updater. winget-releaser/komac automate manifest PRs; Homebrew cask same
  model; an in-app "new version" check against GitHub Releases is a version compare + a link.

## hotchkiss.io hosting — no blockers, ~1-2 days of server work

axum 0.8 + tower-http, headers are per-route, TLS + h2 + no timeouts already there. The plan:

- New nested `/apps` router (the established feature-router pattern) serving a per-app ON-DISK
  directory: `ServeDir` with `.precompressed_br().precompressed_gzip()` (compress at publish
  time, keeps Content-Length + range requests), versioned-immutable caching via the existing
  `?cb=` convention.
- `SetResponseHeaderLayer` adding COOP `same-origin` + COEP `require-corp` on the app document
  route only — needed for SharedArrayBuffer IF threads ever light up; the app must be a
  full-page document (crossOriginIsolated requires the TOP-LEVEL document to carry the headers,
  so link it from a /projects page, don't iframe it).
- Add `NotForContentType("application/wasm")` to the global CompressionLayer predicate —
  on-the-fly compression of a big wasm burns CPU and drops Content-Length/ranges.
- Bundle consumption (DECIDED, both sides in motion): hotchkiss-io's build.rs fetches a pinned
  fab-scad GitHub-release artifact — the exact Tailwind-CLI pattern it already runs (version
  const → release asset URL → version-keyed OUT_DIR cache) — and bakes it into its own output
  as a special page. The bundle never enters the site's git history; a fab release goes live
  via pin-bump + git push, buying atomic deploy + rollback through the existing pipeline. The
  artifact shape is the CONTRACT — see [web-bundle.md](web-bundle.md) (tar.gz: ES-module glue,
  wasm-opt'd wasm, .br/.gz precompressed variants, manifest.json). Iteration point unchanged:
  the rust-embed static path has no precompressed-variant serving and no 206s today — teach it
  .br negotiation or extract-to-disk + ServeDir. Measure before choosing.

**Product shape (decided 2026-07-03).** The site work is TWO concerns, deliberately split:
the model SHOWCASE (the existing publish pipeline, Phases 7/15) and the auto-slicer TOOL. The
tool ships as a standalone full-page app that hotchkiss-io embeds as a special page, with a hard
constraint: ZERO outputs stored server-side. The server serves static app bytes and nothing
else — a visitor's model goes File → browser memory, the 3mf comes back as a blob download,
nothing ever lands on the server (no storage, no retention, no server compute — and "your model
never leaves your browser" is a trust line worth printing on the page). The split also decouples
release cadences: models change when something gets printed, the app bundle changes when fab
ships. One shape rule: "embed" must NOT mean iframe — crossOriginIsolated requires the TOP-LEVEL
document to carry COOP/COEP, so the special page IS the app document (site chrome, if any,
rendered around the canvas server-side); /projects links to it. v1 is single-threaded anyway
(Bevy wasm scheduler + Manifold-wasm both), so the headers are future-proofing, not a v1
dependency. Stretch for later: showcase pages deep-linking a published model's STL INTO the
slicer (same-origin media fetch, works under COEP `require-corp`).

## What the spike must answer (→ PLAN.md Phase 18)

1. Native: does cargo-packager produce a working .app+DMG and a Windows installer for our
   two-binary workspace? (Expect yes — this spike is mostly turning the crank + writing down
   the signing bill.)
2. Wasm gate 1 (GO/NO-GO): does `Solid` boolean + `slice_at_z` run under wasm-bindgen on
   `wasm32-unknown-unknown` via manifold-csg `unstable-wasm-uu`? If no: is the npm-bridge
   fallback tolerable, or is wasm dead?
3. Wasm gate 2: does our feathers UI render in a browser (WebGL2? WebGPU?) on Bevy 0.19?
4. Hosting: /apps route on hotchkiss.io serving a crossOriginIsolated page.
5. Decision memo: pick the primary mode; the web shape is already decided (standalone
   client-only auto-slicer, zero server-side outputs, STL-upload-first, openscad-wasm as a
   stretch) — the memo scopes what's left and spawns the build-out phase.

The two modes are NOT mutually exclusive — native is the power-user tool, the browser build is
the public-facing slicer living next to the published projects it produced. The memo's real
question is sequencing, not either/or.
