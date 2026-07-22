## 2026-06-28

## Phase 0 - Safety net (no deletions until this is green)
- [x] 0.1 - Confirm NAS reachable + capacity; pick cold-archive root (hotchkiss.io:/Volumes/NAS/3d_print/_cold_archive)
- [x] 0.2 - Full immutable snapshot of current ~7.6 G to NAS (done manually)
- [x] 0.3 - Verify the manual archive is complete + intact (checksums); record an inventory of what's where


## Phase 1 - Git backup (fab-scad MIT tool repo + scad-models designs repo)
- [x] 1.1 - Author .gitignore (out/, target/, BOSL2.wiki, downloaded models, .DS_Store)
- [x] 1.2 - git init / first commit of source into scad-models (git@github.com:chotchki/scad-models.git)
- [x] 1.3 - Push both repos (fab-scad MIT done; scad-models stays PRIVATE for now); verify a fresh clone is small/fast
- [x] 1.4 - Apply CC BY-NC-SA 4.0 to scad-models (LICENSE + headers + README terms); then decide public
- [x] 1.5 - Root README (workflow overview, how the pieces fit)


## Phase 2 - fab-scad skeleton + BOSL2 pin + standardization + shared SCAD lib
- [x] 2.1 - Stand up fab-scad superproject skeleton (clone repo, layout: libs/ scad-lib/ printers.toml, MIT)
- [x] 2.2 - Pin BOSL2 submodule under fab-scad/libs to latest tag (v2.0.746); track tagged releases, bump deliberately
- [x] 2.3 - Canonical include mechanism (OPENSCADPATH up into fab-scad so BOSL2/std.scad resolves); document it
- [x] 2.4 - Standardize include paths across projects to canonical <BOSL2/...> form (scripted)
- [x] 2.5 - Inventory pass: which projects fail to compile at the pin via .csg eval (report only)
- [x] 2.6 - Pin other third-party libs as submodules (gridfinity_extended, machineblocks)
- [x] 2.7 - Stand up scad-lib in fab-scad (MIT): version-stamp + part-numbering modules


---

## 2026-06-28

## Phase 3 - fab foundation (workflow layer + OpenSCAD wrap spike)
- [x] 3.1 - fab repo + CI + clap skeleton; `fab doctor` (openscad/manifold/NAS/pins/submodules)
- [x] 3.2 - Dogfood the OpenSCAD integration pattern: headless render, preview, geometry I/O (the wrap fab + GUI build on)
- [x] 3.3 - `fab focus <project>` active-project context (no name on every command)
- [x] 3.4 - Parse a MINIMAL project.toml (name/title/part); schema grows by dogfooding
- [x] 3.5 - `fab new <name>` scaffolds from the template
- [x] 3.6 - Wire scad-models + libs + scad-lib as pinned submodules under fab-scad


---

## 2026-06-28

## Phase 4 - Linear slicing (SCAD lib; kill the 2^N blowup)
- [x] 4.1 - Characterize blowup: confirm nested partition() ~2^N; quantify on window_light_blocker + shoe_holder
- [x] 4.2 - Linear slicer module in scad-lib: planar slab cuts, piece = source ∩ slab (child once per piece)
- [x] 4.3 - Cut minimization: fit by rotation/diagonal against printer bed before cutting (printers.toml)
- [x] 4.4 - Connector lib: heat-set insert + M bolt (default) and teardrop pin + glue; reuse BOSL2 screw_hole/nut_trap; harvest insert specs
- [x] 4.5 - Per-piece print orientation drives connector orientation so all features print support-free
- [x] 4.6 - Auto-place connectors on a face + manual override (shared across connector types)
- [x] 4.7 - Test coupons: emit a sample joint to tune slop before full prints
- [x] 4.8 - Dimensional-integrity check: reassembled pieces == original within tolerance (no shrink)

- [x] 4.9 - Family logo stamp module (scad-lib, BOSL2 attachable)

---

## 2026-07-01

## Phase 10 - Manifold in-process kernel — spike (go/no-go)
- [x] 10.1 - Vet a Rust Manifold binding: build + boolean + STL export
- [x] 10.2 - Import the real Underdesk STL into Manifold; check robustness
- [x] 10.3 - In-process slab slice: parity + latency vs OpenSCAD
- [x] 10.4 - Multi-object 3mf export from Manifold meshes
- [x] 10.5 - Go/no-go writeup + scope Track C or park

---

## 2026-07-01

## Phase 11 - Track C: in-process Manifold geometry kernel
- [x] 11.1 - kernel module scaffold: manifold3d dep + typed Solid wrapper
- [x] 11.2 - STL import: weld to a valid manifold Solid
- [x] 11.3 - Export: Solid to binary STL + multi-object 3mf
- [x] 11.4 - In-process slab slicer: piece by multi-index
- [x] 11.5 - Slicer parity harness vs OpenSCAD
- [x] 11.6 - Connector solids in Rust: onion + bolt clearance
- [x] 11.7 - Apply connectors per piece, floater-free by construction
- [x] 11.8 - Connector parity vs the scad path
- [x] 11.9 - fab render/slice: in-process kernel path with OpenSCAD fallback
- [x] 11.10 - GUI reactive DAG on the cached base mesh
- [x] 11.11 - Corpus parity + latency validation; demote scad codegen; docs
- [x] 11.12 - Port the print-orientation preview to the kernel

---

## 2026-07-01

## Phase 12 - Bambu multi-plate export
- [x] 12.1 - fab_scad::bambu writer — multi-plate project .3mf
- [x] 12.2 - 2D bin-packer — oriented footprints to fewest bed plates
- [x] 12.3 - export_plates orchestration — orient, seat, pack, place, write
- [x] 12.4 - GUI "Export plates" action in the print view
- [x] 12.5 - Round-trip test + Bambu Studio validation

---

## 2026-07-01

## Phase 13 - Auto-slice & orient
- [x] 13.1 - auto_orient: stability tie-break — largest face down when overhang allows
- [x] 13.2 - auto_slice: bbox partition into bed-fit cells (equal division, overflowing axes only)
- [x] 13.3 - GUI Auto-slice button — seed cuts from model bbox + printer bed

---

## 2026-07-02

## Phase 14 - fab make: the one-shot auto pipeline
- [x] 14.1 - fab_scad::auto — plan() chaining auto_slice + connector auto-place (lib extraction)
- [x] 14.2 - GUI: auto-on-open for unsliced too-big models + Auto button on auto::plan
- [x] 14.3 - CLI: fab make <model> — render, plan, slice, orient, export a Bambu project
- [x] 14.4 - fab make end-to-end dogfood + docs
- [x] 14.5 - Adaptive onion placement — a few alignment guides, spread + scaled to face

---

## 2026-07-02

## Phase 15 - Publish to hotchkiss.io
- [x] 15.1 - Manifest [publish] section + fab_scad::publish client (auth, slugify, upload, page CRUD, retry)
- [x] 15.2 - publish orchestration: gather artifacts (cover, low-fn viewer STL, full STL, 3mf), compose markdown, idempotent create+update
- [x] 15.3 - fab publish CLI command — API key (env/flag) + base URL, render artifacts, publish
- [x] 15.4 - GUI Publish button
- [x] 15.5 - Publish end-to-end dogfood against hotchkiss-io + docs

---

## 2026-07-02

## Phase 16 - Bolt: bound through-depth + teardrop shaft
- [x] 16.1 - Bolt: bound through-depth to the above-slab thickness (kernel slice_solid)
- [x] 16.2 - Bolt: teardrop the shaft + counterbore for support-free horizontal holes (build-up aimed)
- [x] 16.3 - Bolt: tests (through-depth spans slab, teardrop self-supports) + scad bolt_joint parity

---

## 2026-07-03

## Phase A - fab-web build-out: the browser slicer
- [x] A.1 - fab-web crate (workspace member web/): canvas-bound app skeleton + STL upload→view (rfd pick_file → bytes → mesh, bed-seated, auto-framed camera); repoint dev.sh + release-web.yml payloads off the probe
- [x] A.2 - Slice in the browser: fab-scad kernel dep (kernel, no native) + rotate-to-fit + auto::plan on upload → cut planes + piece preview; CI needs LLVM 20+ & lld for the wasm kernel build (ubuntu-24.04 clang 18 too old)
- [x] A.3 - Connector editor subset: per-cut cross-section view, auto-placed onions visible, add/remove/resize — lift the desktop editor's hot path
- [x] A.4 - Export: pack → Bambu multi-plate 3mf via Cursor<Vec<u8>> seam → browser blob download (zero server-side outputs)
- [x] A.5 - Share don't fork: unify stl.rs + scene helpers duplicated between gui/ and web/ (duplicates drift)
- [x] A.6 - Size trim: prune bevy default features (audio/gltf/animation/scene formats) + wasm-opt parity in dev; budget ≤7 MiB brotli on the wire
- [x] A.7 - Ship web-v0.3.0 (real slicer payload: plan/slice/export in-browser), retire spikes/wasm-gui, hotchkiss-io pin bump
- [x] A.8 - Perf gate: 100k+ tri STL upload/slice on the main thread — measure jank; if bad, geometry web worker over mesh-bytes postMessage (the !Send Solid contract maps 1:1)
- [x] A.9 - 3MF upload alongside STL (color carry-through): parse 3mf meshes + material/color groups → per-object colored meshes; picker filter grows to [stl, 3mf]; keep colors through slice → export

---

## 2026-07-03

## Phase B - openscad-wasm: render .scad in the browser (BOSL2 + scad-lib)
- [x] B.1 - Worker spike: pinned official openscad-wasm snapshot (files.openscad.org) in a web worker — write .scad + includes into the Emscripten FS, callMain (Manifold backend; --backend=manifold on older pins), read STL bytes back; own ~100-line glue from the README, do NOT fork the playground's GPL runner
- [x] B.2 - Bake tagged lib pins INTO the bundle: release CI packs BOSL2 (the libs/ submodule pin, v2.0.746 today) + scad-lib (same commit as the app) as zip members of the fab-web artifact; worker mounts them at /libraries so any .scad hits include <BOSL2/std.scad> / <slicer.scad> with ZERO setup; prove screw_hole/onion/teardrop render
- [x] B.3 - fab-web integration: picker accepts .scad → worker render (progress in the panel) → STL bytes → the SAME present_model path (plan/slice/export just work)
- [x] B.4 - Lazy delivery + licensing: openscad wasm (~13 MB) + library zips as separate bundle members fetched only when a .scad opens; GPL done consciously — unmodified module in its own worker, notice + source link on the page (page-level combo conveys GPL, MIT files stay MIT)
- [x] B.5 - Dogfood a real models/ part end to end in the browser: .scad with scad-lib + BOSL2 includes → worker render → auto-slice → export; the baked pins (B.2) must resolve everything with no manual mounting
- [>] B.6 - Customizer stretch: expose the .scad's top-level params in the panel, tweak → worker re-render (defer if B.1-B.5 drag)
- [x] B.7 - Viewer controls: orbit (left-drag), pan (middle-drag / shift+left), zoom (wheel) on the fab-web 3D view — Z-up like the desktop; clicks still pick (drag-guard suppresses click-after-orbit); input yields over the panel

---

## 2026-07-04

## Phase 5 - Slicer / workflow GUI (EARLY; dogfood the OpenSCAD wrap)
- [>] 5.1 - GUI MVP: load model, set cut planes, click a face to place pegs/connectors, preview piece-vs-bed + orientation
  - [x] 5.1.1 - Sim-interaction test harness: scripted input → real systems → screenshot
  - [x] 5.1.2 - Multi-cut + per-cut axis: set cut lines, rotate/pick the plane
  - [x] 5.1.3 - Face-pick connector placement: click model → drop bolt/pin on the cut
    - [x] 5.1.3.1 - Manual face-pick: click model → drop a connector on the nearest cut (build first)
    - [x] 5.1.3.2 - BOSL2 onion connector (support-free), replacing pin/dowel
    - [x] 5.1.3.3 - Per-piece print-orientation UI → derive connector orientation
    - [x] 5.1.3.4 - Cross-section-driven auto-size + auto-place connectors
    - [x] 5.1.3.5 - Per-cut 2D cross-section connector editor: button on a cut → see its profile → pick connectors on it
- [x] 5.2 - Emit the slicing spec that scad-lib/fab consume; round-trip it through `fab render`
- [>] 5.3 - Grow into a friendly workflow front-end (cut the verb-memorization tax)
  - [x] 5.3.1 - Directory/file picker: rfd "Open" button → choose a project dir or .scad (retire CLI-arg-only entry)
  - [x] 5.3.2 - File-list side panel: FileList resource (Vec<PathBuf> + active); click a row to switch (SceneCfg.source stays the scalar active pointer — lower blast radius)
  - [x] 5.3.3 - File-watch: mtime-poll on the active .scad → auto re-render on save (open-file only; include-graph gap → 6.6)
  - [x] 5.3.4 - Panel UX pass (dogfooding): full-focus mode-aware panel (View / Connectors / Print — hide controls that don't apply, in-mode Done); scroll-bound the file list + orbit yields over the panel; fix cross-section Y-flip (OpenSCAD negates SVG Y → auto-place scattered connectors below the model)
  - [x] 5.3.5 - Connector type picker (onion/bolt) in GUI editor
  - [x] 5.3.6 - Split view: dock panel, inset 3D camera viewport
  - [x] 5.3.7 - Onion feasibility follows build axis, not cut axis
  - [x] 5.3.8 - Bound onions to teardrop tip, not sphere radius
  - [x] 5.3.9 - Bolt: bound through-depth + wire teardrop shaft in slicer
  - [x] 5.3.10 - Seat the loaded model on the bed (Z-floor)
- [x] 5.4 - Track B: reactive render DAG (dogfood-driven)
  - [x] 5.4.1 - Include/use dependency resolver (fab_scad::deps)
  - [x] 5.4.2 - Include-graph file-watch (closes 5.3.3 gap)
  - [x] 5.4.3 - Reactive auto-reslice: no Re-slice button
  - [x] 5.4.4 - Loading pulse while recomputing


## Phase 6 - fab render + output (3mf, magnets, Bambu)
- [>] 6.1 - Render engine: enumerate targets → parallel (rayon) render → report; a "target" is any .scad→out unit (pieces/parts/projects collapse to target sets); per-target thumbnail + N/M progress
- [x] 6.2 - Incremental rebuild: skip pieces whose inputs are unchanged (content hash)
- [x] 6.3 - Multipart 3mf: export pieces as SEPARATE objects on a plate (lazy-union); verify separation
- [>] 6.4 - Embedded magnets: clean split around cavities + pause-at-layer in the 3mf
- [>] 6.5 - Investigate Bambu 3mf settings embedding (plate/material/pause) for one-click print; adopt only if clean
- [>] 6.6 - Demote import() crutch to optional freeze-source-once; DAG resolver as fallback only
- [x] 6.7 - Smoke oracle: a render "passes" iff OpenSCAD exits 0 AND mesh face-count > 0 (fast, no goldens; parity-vs-archived deferred to 8.4)
- [x] 6.8 - `fab render --all [PATH]`: walk the tree, enumerate every .scad, parallel smoke-render, print a pass/fail summary (the correctness sweep; needs no manifests)
- [x] 6.9 - Wire `.fab/focus`: `fab render` with no arg renders the focused project's parts (needs one minimal project.toml; scaffold from the existing template)


## Phase 17 - auto-slice/pack v2: kernel cross-sections + rotate-to-fit
- [x] 17.1 - Kernel cross-section: Solid::cross_section(axis, at) via slice_at_z (drop the OpenSCAD spawn)
- [x] 17.2 - Swap auto::plan + GUI connector editor onto the kernel cross-section (keep OpenSCAD as parity oracle)
- [x] 17.3 - Rotate-to-fit auto-slice: score candidate rotations (incl 45°) by piece count, pick fewest
- [x] 17.4 - Wire rotate-to-fit into auto::plan / fab make / GUI auto-on-open
- [x] 17.5 - Phase 17 tests + parity (kernel vs OpenSCAD cross-section; rotate-to-fit reduces pieces) + dogfood
- [>] 17.6 - GUI auto-on-open rotate-to-fit: re-orient loaded model + thread rotation through reslice/export


## Phase 18 - Deployment spike: DMG/winget vs wasm on hotchkiss.io
- [x] 18.1 - Native: cargo-packager multi-binary config → local unsigned .app + DMG; Bevy asset-path fix; app launches from /Applications
- [x] 18.2 - Native: drafted then PARKED (web-first, 2026-07-03) — release-native.yml kept, manual-dispatch only; winget manifests drafted; signing bill in docs/packaging.md; resume via backlog
- [x] 18.3 - Wasm: `native` feature seam — lib compiles on wasm32-unknown-unknown with openscad/publish/reqwest gated off (pure modules + STL bytes green)
- [x] 18.4 - Wasm kernel gate (GO/NO-GO): manifold-csg `unstable-wasm-uu` — Solid boolean + slice_at_z under wasm-bindgen in a browser; npm-bridge fallback assessment if no
- [x] 18.5 - Wasm GUI gate: fab-gui via bevy_cli on wasm — feathers render (bevy#22620: WebGL2 vs WebGPU), mesh_picking, rfd pick_file→bytes
- [x] 18.6 - Wasm hosting gate: hotchkiss-io special page serving the fab wasm bundle as a build-time artifact (full-page document, NOT an iframe) — COOP/COEP on the app document, precompressed bytes, wasm out of CompressionLayer; crossOriginIsolated proven
- [x] 18.7 - Decision memo → SPEC.md: pick primary mode; web = standalone client-only auto-slicer, zero server-side outputs (decided 2026-07-03; STL-upload-first, openscad-wasm stretch); spawn the build-out phase
- [x] 18.8 - fab-web bundle contract: GitHub-release artifact (tar.gz: ES-module glue + wasm + br/gz + manifest.json; tailwind-style pinned fetch) — contract doc + spike bundle handed to the 18.6 gate
- [>] 18.9 - crates.io channel: claim the free `fab-scad` name — fix package contents (exclude models/spikes/docs), `cargo publish --dry-run` clean, then publish 0.1.0 (cargo install = third distribution channel, source-build tradeoff documented)


## Phase C - fab-web beta feedback
- [x] C.1 - Busy pulse + staged sync work: animated "rendering {name} (OpenSCAD)" while the worker runs; "slicing…"/"packing…" labels armed 2 frames ahead so they PAINT before the main-thread block; all completions clear to a real status (the desktop loading-pulse standard, ported)
- [x] C.2 - Geometry worker (fab-geom): a second SMALL wasm (kernel-only, no bevy, ~1 MB) in its own web worker runs weld/plan/slice/export over mesh-bytes postMessage — the !Send Solid contract as designed; makes the C.1 slice/export labels a LIVE pulse instead of a painted-then-frozen one (A.8 measured 5-10 s block on a 119k-tri part)
- [x] C.3 - Printer selection: preset cycle button (A1 mini / P1-X1 / MK4 / Ender 3 / Voron 350) + localStorage persistence (fab-web.bed) — no hardcoded 256³; changing printer re-plans the loaded part in the background (reactive standard, live pulse); ?bed= deep-link still wins at startup
- [x] C.4 - Adversarial review of C.2/C.3 (40-agent workflow: 4 lenses × 2-skeptic verify) + fixes: pick/render polls queue behind in-flight geometry (single-flight bypass = crossed worker replies), id-matched persistent worker transport + onerror (404'd worker script errored visibly instead of eternal pulse), Part.raw commits only on Analyze success, worker-init retry, ?bed= clamp, queued printer clicks

---

## 2026-07-05

## Phase G - scad-rs bootstrap: pivot + spec + tracer bullet
- [x] G.1 - Relicense + pivot mechanics: GPL-2.0-or-later (OpenSCAD's EXACT license, chosen for zero-friction upstreaming) across LICENSE + 4 crate manifests + README/NOTICE/web-bundle docs; SPEC.md → SPEC_workflow.md; PLAN restructured — all non-G work backlogged with provenance, phases 5/6/17/18/C archived
- [x] G.2 - SPEC.md rounds 1-2 (drafted WITH chotchki): mission + license stance, architecture, BOSL2 rungs, determinism doctrine, testing/verification layers — all open questions resolved or scheduled (winnow, enum values, Kani-low-level, semantics/ segmented, lang/ sibling, tracing full-trace)
- [x] G.3 - Tracer bullet: sphere-vs-oracle end to end, metric gate chosen from data
  - [x] G.3.1 - lang/ crate scaffold: workspace sibling, error type, tracing dep (compiled-out default), clippy-pedantic baseline, CI lane (fmt/clippy/test)
  - [x] G.3.2 - winnow lexer: tokens, numbers/strings/identifiers, comments PRESERVED (customizer needs them later); every named parser wrapped in winnow trace() from day one (debug-feature-gated, zero cost off); lexer fuzz seed corpus started
  - [x] G.3.3 - parser core: expression precedence, module instantiation, argument lists incl. $-args; AST with source spans (LocatingSlice + .with_span()); winnow-native errors from production one — StrContext label+expected everywhere, cut_err at commit points, caret rendering from the context stack
  - [x] G.3.4 - evaluator skeleton: explicit-stack machine over the subset; Value v0 (Num/Bool/Str/NumList/Undef); $fn/$fa/$fs resolution
  - [x] G.3.5 - lower sphere()/cube()/cylinder() to kernel::Solid — tessellation EXACTLY matching src/core primitives (ring/segment math ported, provenance noted)
  - [x] G.3.6 - oracle runner: drive the openscad CLI, capture mesh + echo; VERIFY the deterministic-output flag (spec Q7) — what it sorts, what it doesn't
  - [x] G.3.7 - metric experiment: implement the comparison tiers (quantized vertex-multiset, vol/area/Euler, boolean residual); sphere $fn=8→256 matrix; DOCUMENT the gate per model class back into SPEC.md
  - [x] G.3.8 - first semantics/ tests land (provenance-annotated from G.3.5's port)


---

## 2026-07-05

## Phase H - scad-rs: the whole grammar
- [x] H.1 - Grammar inventory: bison file → conformance checklist doc (every production accounted for)
  - [x] H.1.1 - grammar-inventory.md: every parser.y production + lexer.l rule → {AST node, parser fn, status, conformance anchor}; the matrix H.5's suite derives from
  - [x] H.1.2 - Lexer completeness audit vs lexer.l: confirm hex/float/escapes/unicode/$-idents/digit-idents/EOT/operators all covered; document the DELIBERATE divergences (comments preserved, zero file-IO in the lexer)
- [x] H.2 - Statements/items (parse-only): module def, function def, if/else, use/include → AST — the 4 genuinely-new constructs; for/intersection_for/let/each/assert/echo ALREADY parse as module calls (their semantics are I.2/I.3)
  - [x] H.2.1 - Parameter type + params-list parser (id | id=default, trailing comma) — shared by module def, function def, and the function-literal expr
  - [x] H.2.2 - Module def: `module id(params) statement` → StmtKind::ModuleDef (body is one statement, usually a block)
  - [x] H.2.3 - Function def: `function id(params) = expr;` → StmtKind::FunctionDef
  - [x] H.2.4 - if/else in the module_instantiation path: dangling-else (%prec NO_ELSE), else-if chains, works in child position for free (translate() if(x) cube();)
  - [x] H.2.5 - use/include → AST nodes (parse-only, zero-IO); resolution/splice is I.2's loader; the evaluator stays LOUD-deferred on these nodes until then
  - [x] H.2.6 - Conformance nicety: child_statements ⊂ inner_input (module/function DEFS illegal inside a module-call child block) — tighten block() or consciously defer
- [x] H.3 - Expressions complete: list comprehensions (every form), ranges, function literals, ternary, string escapes/unicode
  - [x] H.3.1 - Extend the non-recursive Drop + MAX_DEPTH guards for every new recursive node (the Safari-cliff discipline — do the pattern once, here)
  - [x] H.3.2 - List-comprehension elements: LcFor, LcForC (C-style for(init;cond;next)), LcEach, LcLet, LcIf/else, parenthesized _p, arbitrary nesting
  - [x] H.3.3 - Function-literal expr: `function(params) expr` → ExprKind::FunctionLiteral
  - [x] H.3.4 - let-expression: `let(args) expr` → ExprKind::Let
  - [x] H.3.5 - assert/echo expressions with OPTIONAL trailing expr (expr_or_empty): assert(args) expr?, echo(args) expr?
  - [x] H.3.6 - Ranges + string-escape/unicode: audit + pin with tests (already implemented in G.3.3 + the lexer — confirm, don't rebuild)
- [x] H.4 - Customizer annotations survive: parameter comments/groups/ranges in the AST (lossless-enough)
  - [x] H.4.1 - Customizer annotation model: group / description / widget-constraint (range, step, dropdown k:v, string maxlen) types in the AST
  - [x] H.4.2 - Trivia-association pass: walk Lexed::all, bind trailing line-comment + active group header to each top-level assignment (top-of-file scope, per OpenSCAD)
  - [x] H.4.3 - Constraint mini-grammar parser: [min:max], [min:step:max], [v,…], [k:label,…], [maxlen]; group headers incl. [Hidden]/[Global]
  - [x] H.4.4 - Customizer lossless-enough roundtrip test: annotations survive parse → (edit a value) → emit
- [x] H.5 - proptest print/parse roundtrip + the bison-derived conformance suite green
  - [x] H.5.1 - Pretty-printer: AST → canonical OpenSCAD source (Display over the whole AST) — the missing prerequisite for the roundtrip property
  - [x] H.5.2 - proptest strategy over the AST + print→parse→assert-equal property (structural eq modulo spans)
  - [x] H.5.3 - Bison-derived conformance suite: one+ example per production from grammar-inventory.md, all green — fills the doc's H.5.3 anchor holes
  - [x] H.5.4 - cargo-mutants gate on the parser (backlog #37) — prove the tests CATCH bugs, kill survivors
- [x] H.6 - cargo-fuzz target + SCHEDULED CI fuzz job + persisted/minimized corpus + trophy log (fuzz-from-first-commit doctrine starts here, not later)

  - [x] H.6.1 - cargo-fuzz target: parse(arbitrary bytes) never panics/hangs/OOMs — wire the fuzz crate + the parse harness
  - [x] H.6.2 - Fuzz seed corpus: extend the lexer seed set to the parser + a structure-aware corpus from H.5's generator
  - [x] H.6.3 - Scheduled CI fuzz job + persisted/minimized corpus artifact (the fuzz-from-first-commit doctrine)
  - [x] H.6.4 - TROPHIES.md doctrine: every fuzz-found bug logged + regression-pinned as a test

---

## 2026-07-06

## Phase M - scad-rs: pure source-provider (fab-lang zero-IO; caller fulfills a needs fixpoint)
- [x] M.1 - M.1 - The pure source-provider contract
- [x] M.2 - M.2 - Pure loader: source table in, Scad needs out (static parse-time fixpoint)
- [x] M.3 - M.3 - Eval-time File needs: import/surface emit needs + placeholder-continue
- [x] M.4 - M.4 - The IO shell: the outer fixpoint loop (the one place std::fs lives)
  - [x] M.4.2 - M.4.2 - The io module: outer fixpoint driver + thin wrappers
  - [x] M.4.3 - M.4.1 - Loader → pure: excise std::fs, surface Scad needs
- [x] M.5 - M.5 - import()/surface() backend: readers fulfill File needs → Mesh
  - [x] M.5.1 - M.5.1 - import() reader: STL/3MF → fab_lang::Mesh + driver
  - [x] M.5.2 - M.5.2 - surface() heightmap: DAT/PNG → Mesh + center/invert eval-threading
- [x] M.6 - M.6 - Differential + coverage close-out
  - [x] M.6.1 - M.6.1 - Tolerant loader: missing/broken use/include → warn+render
  - [x] M.6.2 - M.6.2 - Differential: import() STL matches the oracle
  - [x] M.6.3 - M.6.3 - Coverage close-out: verify functions-100 + lcov-DA-lines-100

---

## 2026-07-08

## Phase M - M - Heap-bounded eval (last recursion removal)
- [x] M.1 - M.1 - Iterative Drop for deep GeoNode/Shape2D/Value trees
- [x] M.1b - M.1b - Value deep-list Drop: ValueList newtype (heap-bounded, no arithmetic-hot-path cost)
- [x] M.2 - M.2 - Assess eval-assembly recursion + correct the reserve rationale (EVAL_STACK); fix split to M.3
- [x] M.3 - M.3 - Explicit-stack eval assembly (remove host recursion; default + wasm stack)

---

## 2026-07-08

## Phase Q - Dogfooding + hardening
- [x] Q.1 - Dogfood: fab render --engine scad-rs [--check] (eval→Manifold→STL, + oracle diff)
- [x] Q.2 - Q.2 - GUI live preview via scad-rs: swap render_whole off OpenSCAD (edit-in-Zed → live 3D)
- [x] Q.3 - Dogfood bug: BOSL2 constants (UP/CENTER/_EPSILON) undef in module defaults via transitive `use`
- [x] Q.4 - Q.4 - SVG import LANDED via usvg: import(x.svg) → even-odd Shape2D::Polygon; oracle-matched (8/8 icons + FamilyLogo bbox exact), unblocks remindwall
  - [x] Q.4.1 - Q.4.1 - Widen the import seam to 2D-or-3D (Imported enum)
  - [x] Q.4.2 - Q.4.2 - usvg parser → contours (scale 25.4/72 @ dpi=72, Y-flip about size height)
  - [x] Q.4.3 - Q.4.3 - Oracle-match validation across the SVG corpus (differ); document v1 simplifications
  - [x] Q.4.4 - Q.4.4 - Tests + docs/svg-import-design.md + remindwall FamilyLogo end-to-end

---

## 2026-07-12

## Phase W - W - workspace hygiene: rustfmt/clippy burn-down to the existing -D warnings CI gates (branch is 277 commits ahead of origin — the gates never ran)
- [x] W.1 - W.1 - rustfmt sweep: cargo fmt --all (18 drifted files) → `cargo fmt --all -- --check` green
- [x] W.2 - W.2 - clippy mechanical tier: cargo clippy --fix per crate + hand tail (doc backticks/doc-valid-idents, semicolons, type_complexity aliases, too_many_args, if-let, inline format args, derive, sort_unstable, must_use msg) across lang/gui/web
- [x] W.3 - W.3 - determinism-policy sites: 17 HashMap + 2 HashSet in eval_cache/mod_cache/mod_redundancy/redundancy → IndexMap/BTreeMap per-site; N.2c hazard = gate overhead, so perf sanity-check after the swap
- [x] W.4 - W.4 - no-panic doctrine sites: geo_stack unreachable×2 + mod_cache panic×1 → typed paths; seed_fuzz_from_bosl2 example → Result main (no bare expect)
- [x] W.5 - W.5 - precision casts: u64/usize→f64 stats ratios (13 sites, same 4 cache files) → one ratio helper with a reasoned allow; eval_cache:377 u64→u32 truncation read + fix or justify
- [x] W.6 - W.6 - exit gate: run the ci.yml lane commands verbatim locally — fmt --check, clippy -D warnings (root + fab-lang --all-features + fab-jit), tests — all green, zero allows without reasons
- [x] W.7 - W.7 - test-lane segmentation: default `cargo test` = seconds (unit + smoke); heavy suites (bosl2_scout, conformance, eval/geometry corpus, models_harness e2e) → #[ignore = "corpus lane"] + a dedicated CI lane running --ignored under [profile.test] opt-level 2; kill the fab-lang double-run (root --workspace already includes lang — ci.yml's "not a default member" comment is stale); consider cargo-nextest for per-test timings

---

## 2026-07-13

## Phase U - U - GUI: feathers → egui migration (unblocks rich-text, tabs, resizable panels)
- [x] U.1 - U.1 - egui migration: feathers → bevy_egui 0.41 (Bevy 3D stays); panel layer only
  - [x] U.1.1 - U.1.1 - bevy_egui integration: dep + EguiPlugin + minimal SidePanel rendering alongside Bevy 3D
  - [x] U.1.2 - U.1.2 - port all panels (view/connectors/print) to egui immediate-mode + rewire the 2 seams + icon font
  - [x] U.1.3 - U.1.3 - delete feathers: UI builders + retained-mode reconciliation systems + drop the feature
  - [x] U.1.4 - U.1.4 - harness modes (windowed/screenshot/scripted) render egui + full gui verify (test + clippy)
- [x] U.2 - U.2 - egui panel polish: Material Symbols icons + active-row alignment + optional Nudge flash
  - [x] U.2.1 - build.rs Material Symbols font pipeline: manifest-keyed download+cache+subset+cache; committed subset = CI/offline fallback; egui set_fonts registration
- [ ] U.3 - U.3 - Workflow tabs: app-wide top-tab restructure (Model/Parts/Orientation/Export) — see docs/workflow-tabs-mockup.html
  - [x] U.3.1 - U.3.1 - Top-tab shell + bottom status bar: app-wide Tab resource, full-width bar, route existing blocks, retire derived PanelMode
  - [x] U.3.2 - U.3.2 - Model tab: egui editor from debounced buffer + explicit desktop Save + unsliced 3D + file inner-tabs with ＋-reopens-folder (reuse FileList/SwitchFile); active file drives downstream
  - [x] U.3.3 - U.3.3 - Parts tab: left-panel 3-level drill part→cut→connectors inline; fold today's Connectors mode in
  - [x] U.3.4 - U.3.4 - Orientation tab: promote Print mode; per-piece flat/auto list across all parts
  - [x] U.3.5 - U.3.5 - Export tab: co-pack preview + Export 3MF + Publish merged
  - [x] U.3.6 - U.3.6 - Entry-point gating: web (single presupplied file, no ＋, editor landing) vs desktop (full picker + ＋); platform gate
  - [x] U.3.7 - U.3.7 - Feedback: per-node DAG dirty flags → amber tab dots (stale) + spinner motion on rendering tab + bottom status-bar detail; background jobs clear
  - [x] U.3.8 - U.3.8 - Harness + tests: script verbs (tab-switch, editor-edit), screenshot each tab, full gui verify
  - [x] U.3.9 - U.3.9 - panel-inset layout bug: egui layer offset by seam on HiDPI window (egui context rect ↔ split_viewport 3D-camera inset collision); root-cause via bevy_egui-0.41 source + real-window diag, fix + verify on 2× display
  - [x] U.3.10 - U.3.10 - real-window screenshot harness: windowed `--shot <path>` captures the TRUE winit/HiDPI window surface at a settled frame (+ camera/egui-context ownership dump, self-exit) — the offscreen harness renders a different pipeline and is blind to windowed-only wiring bugs
  - [x] U.3.11 - GUI integration tests: script-driven state assertions (ScheduleRunner harness → drive tab/addcut/edit/autoplace → assert edit.0/cuts/conns/active_part/Tab)
  - [x] U.3.12 - Dogfood fixes: Parts Auto-slice/Explode no-op + Model-editor scroll zooms 3D view + ＋ file-tab glyph (Material Symbols)
  - [x] U.3.13 - Model tab: SCAD syntax highlighting in the code editor (egui layouter / LayoutJob)
  - [x] U.3.14 - Config-driven Parts: GUI ↔ project.toml [slicing] shared with the CLI — load-if-present / auto-derive-if-absent, save-on-edit, reset-to-auto (both cuts+connectors), complete derive for all parts, Explode→view-toggle
    - [x] U.3.14.1 - Phase A — manifest schema types (Slicing.parts, PartSlicing, PartKey{name,nth,index}, PieceOrient.comp) + shared resolve_part in backend; flat back-compat + serde round-trip tests
    - [x] U.3.14.2 - Phase B — inverse bridge (manifest→GUI: Cut→CutDef, Connector→PlacedConn reversing enabled↔stack idx, PieceOrient→Orient) + GUI load hook in poll_job (before auto-plan stands down)
    - [x] U.3.14.3 - Phase C — GUI save: debounced format-preserving autosave (toml_edit) writing [[slicing.part]], migrate-on-save strips flat fields, baseline-seeded so bare open never churns the file
    - [x] U.3.14.4 - Phase D — CLI part-aware slice: slice_model_parts (build_geo_parts + resolve_part bind + per-part slice_solid), XOR-bail on flat+per-part mix, legacy flat unchanged, bind-by-index+warn on name miss
    - [x] U.3.14.5 - Phase E — printer wiring: read Slicing.printer (dead field today) + --printer on Slice subcommand, precedence CLI>spec>default
    - [x] U.3.14.6 - Phase G — slicer honors (slab, comp) orientation [chotchki D2]: re-key slice_solid/piece_up from [usize;3] slab to PieceKey=(slab,comp) so a manually-oriented component orients in the actual sliced geometry (GUI reslice + CLI slice)
  - [x] U.3.15 - Reactive Parts UX (no config dep): complete+consistent auto-derive for ALL parts (fit-to-bed cuts + auto-placed connectors), Explode→persistent view toggle, Reset-to-auto (cuts+connectors)
  - [x] U.3.16 - Dogfood fixes (slice_parts drive): editor h-scroll (ScrollArea::both + left-aligned ui.add, was blowing the panel open on a long line) + multi-plate grid preview (unify onto pack::pack + promoted grid_cols/plate_origin so preview == panel count == exported 3mf, one bed slab per plate in a near-square grid, was one plate in a line)
  - [x] U.3.17 - Feedback accuracy (slice_parts dogfood): status pulsed a stale "ready" mid-render + no loading badge on the FIRST compute (badges gated on `dirty`, empty before first compute). sync_pipeline now derives per-tab `loading` from IN-FLIGHT jobs (not `dirty`) → spinner on the computing stage even on initial load, and an accurate `activity` label ("rebuilding geometry…"/"auto-planning part N…"/"orienting pieces…") the status bar pulses instead of the imperative Status (which can lag terminal). Wired sync_pipeline+AutoJob into the scripted harness (was windowed-only). Unit-tested derive_loading + busy_activity; caught+verified a busy frame (spinner + pulse)
  - [x] U.3.18 - Tofu fixes (wall_screen dogfood): the stale-tab badge + "unsaved" (`●`), the Publish button + export/publish status (`→`), and "flat ✓" all rendered as tofu — the egui font stack (defaults + Material Symbols subset) covers none of those glyphs. Added DOT (fiber_manual_record) + CHECK to the build.rs manifest (regenerated the subset) and switched arrows to ASCII `->`; audited the whole gui for other raw non-ASCII (·…—× are safe), and wrote the no-raw-non-ASCII-glyph rule into gui/CLAUDE.md
  - [x] U.3.19 - Dogfood fixes (presliced wall_sliced): (1) DRAG-to-move cut planes was dead — the opaque Model mesh has no `Pickable`, so it BLOCKS the pick ray (bevy "entities block by default") and the cut plane sits inside it; DragStart landed on the Model, the observers bailed. Fix: `Pickable::IGNORE` on the Model spawns (split-viewport was correctly ruled out; needs live-window confirm — can't script a drag gesture). (2) presliced model DOUBLE-SLICED — auto-slice keyed on the whole spread-out bbox and re-cut an already-sliced blob. Fix (Option A): `fab::auto_plan` is now connected-component aware — if EVERY component already fits the bed it returns an empty plan (0 cuts) and the T.2a print pipeline fans the uncut blob into its pieces; the Parts header maxes the cut estimate with the stored component count so a presliced part reads "N pcs". Unit-tested the gate; end-to-end confirmed via log (presliced → 0 cuts, connected-oversized → 2 cuts)
  - [x] U.3.20 - Dogfood fixes (window_light_blocker): (1) Orientation view "fought" the user — could orbit but not ZOOM. `sync_orientation` re-framed the camera (o.target/o.radius) on EVERY `parts.is_changed()`, which fires every frame (panel_ui derefs parts), stomping the wheel-zoom's radius next frame. Fix: frame only on `cache.is_changed()` (pieces freshly (re)laid), so a re-orient re-packs but doesn't yank the camera. (2) embedded MAGNET VOIDS pulled out as separate pieces (103 pcs). Confirmed NOT a diff()/tag() bug — fab-lang subtracts correctly (differential AGREES with the oracle). Root cause: `Solid::components()` (src/kernel.rs) is a surface-vertex union-find, so a fully-enclosed cavity's inner shell (shares no verts with the outer) splits off as a phantom inverted-normal solid AND erases the pocket from the host. Fix: classify each shell by SIGNED VOLUME (negative = internal cavity) and fold each cavity into the smallest outer shell whose bbox contains it → a solid-with-void is ONE piece, cavity intact. Regression test (cube − enclosed sphere → 1 comp; + a floating island inside the void → 2, parity-correct). Both need live-window confirm (interactive zoom; 27s model render exceeds the harness --shot window)
  - [x] U.3.21 - Bambu multi-plate .3mf opened as ONE plate — FIXED + VERIFIED (chotchki: 4 plates each with its cube appeared, correctly positioned). Root cause: BambuStudio force-clears `load_config` without a parseable `Metadata/project_settings.config` → discards the `<plate>` blocks → one plate (the Application gate only RECOGNIZES the file). Fix: `bambu::write_project_to` emits a minimal non-empty `project_settings.config` (5th zip entry) with `printable_area` = the packed `bed` (configured 325, per chotchki). Confirmed Bambu HONORS our printable_area (cubes landed on the right 325-grid plates) → no 350-skew, the self-consistent 325 approach was right. Extracted the key-set from chotchki's real H2D "Save Project" reference; bambu tests + doc + `bambu-3mf-multiplate` memory updated. RESIDUAL papercut → U.3.22
  - [x] U.3.22 - Bambu import "customized filament/printer presets: -" prompt — FIXED + VERIFIED (chotchki: "it opened clean!").  Root cause: two things — (a) our config set printable_area = the usable 325 bed ≠ the H2D preset (350), and (b) it NAMED no presets (the "-") and emitted no filament settings → BambuStudio flags customized, unnamed presets. FIX SHIPPED in two parts: (1) real plate size — printer profile now separates USABLE `bed` (325, pieces pack within) from the real `plate` (350, printable_area); optional `plate` field in printers.toml (H2D `[350,320,320]`, defaults to bed), threaded through SceneCfg → export_plates → write_project_to (grid+printable_area = plate; pack = bed) AND the 3D preview (`sync_orientation` tiles on plate) so preview == export. (2) named presets (chotchki: "strong default is fine, Bambu makes it easy to swap") — optional `[printer.bambu]` block in printers.toml (printer/process/filament ids + nozzles + bed_type, from chotchki's H2D "Save Project" reference); `BambuPreset` struct threaded via `default_bambu_preset()` → export path → `project_settings_config` emits the NAMED presets when present (else minimal). Sample regen'd at ~/Desktop/fab-multiplate-test.3mf (now names Bambu Lab H2D 0.8 nozzle + 0.40mm Standard + PLA filaments). PENDING chotchki verify: does the named config import prompt-free?
- [x] U.4 - U.4 - gui module split: break gui/src/main.rs (4.6k lines) into cohesive modules (behavior-preserving moves, no logic changes)

---

## 2026-07-16

## Phase M - manifold-rs: the geometry kernel in Rust (fab-manifold)
Own the kernel — reimplement Manifold's ~4.4K robustness core in pure Rust so fab-scad drops the C++/emsdk toolchain and gets bit-identical determinism native==wasm (what a binding STRUCTURALLY can't give — [[onetbb-wasm-determinism]]). Full scope + decisions in **SPEC_manifold-rs.md**. TEST-FIRST: the oracle harness lands BEFORE the first boolean. **R0+R1 (mesh spine + serial union bit-clean vs the C++ oracle on Manifold's own fuzzer) is the TRACER / GO-NO-GO — if R1 isn't clean, STOP.** New crate `fab-manifold` in `manifold/`. Subsumes Phase S (cross-platform determinism) for geometry. Chotchki: "we're doing this" (2026-07-14).
- [x] M.0 - R0 COMPLETE: crate + oracle harness + mesh spine + invariant checker + par seam, all 100%-covered. Gate K.0 GREEN (Rust spine == C++ on volume/area/genus/bbox across a genus-0/1 primitive+boolean corpus). The correctness thesis is proven; R1 (the tracer boolean) is the next go/no-go.
  - [x] M.0.1 - Scaffolded `fab-manifold` (manifold/): Cargo.toml (libm dep; rayon behind `par`; manifold3d behind `oracle`, off-by-default), lib.rs (`#![forbid(unsafe_code)]` — leaving C++'s unsafety behind) with the module skeleton (mathf/mesh/boolean/polygon/check/par + the oracle harness behind feature+non-wasm-cfg), each stub documenting its role + phase. Workspace member wired. `manifold/clippy.toml` = the Pillar-2 determinism door (f64::sin-etc + mul_add banned → forced through mathf/libm). GREEN: default (serial) + wasm + `--features par` + `--features oracle` all build, clippy `-D warnings` clean.
  - [x] M.0.2 - `mathf.rs` seam + `linalg.rs` vec/mat layer. DECISION CHANGE vs SPEC: the linked v3.5.1 ships its OWN deterministic trig (`math.h` = a musl/msun transliteration), and the C++ oracle uses it — so DON'T "adopt libm" (an independent port, unproven bit-match); VERBATIM-port `math.h` (sin/cos/tan/acos/asin/atan/atan2 + rem_pio2), bit-identical by construction (straight-line f64, no FMA, `to_bits`/`from_bits` punning keeps `#![forbid(unsafe_code)]`). Cross-check tests: 200k pts bit-identical to the `libm` crate. sind/cosd exact-quadrant snap (ONE dialect [[libm-transcendental-divergence]]). linalg = hand-rolled concrete-typed (NOT glam — determinism control), op order read from `linalg.h` (cross/dot/normalize/col-major mat3x4·vec4); componentwise via macro, ordered ops explicit; Box3 rides along. 11 tests, fmt+clippy+native+wasm green.
    - [x] M.0.2.1 - `fab-types` extraction (DEFERRED to post-R1, chotchki's ask): lift `manifold/src/linalg.rs` to a `fab-types` leaf crate ONCE R0/R1 proves the op order bit-clean vs the oracle, then migrate fab-lang `geom.rs` (Vec2/Vec3/Vec4/Tri/Affine, today trapped inside the whole evaluator) onto it — kills the duplicate Vec3 + the "kernel would need fab-lang for a vector" layering inversion. Sequenced late on purpose: lift VALIDATED code, don't guess against geom.rs's evaluator-era ops.
      - [x] M.0.2.1.1 - M.0.2.1.1 - fab-types leaf crate: linalg.rs lifted VERBATIM minus trig (rotate constructors become sincos-parameterized; manifold's linalg.rs shims mathf sind/cosd into them as free fns) + geom-parity additions (from/to_array, Index). Kernel gate: the M.6 byte-goldens must hold UNCHANGED through the move
      - [x] M.0.2.1.2 - M.0.2.1.2 - fab-lang geom.rs onto fab-types: Vec2/Vec3 re-exported; the evaluator dialect preserved bit-exact as a geom-local extension (normalize_or_self keeps the zero-guard + reciprocal-mul rounding; angle_deg keeps platform acos) — 11 call sites across auto_orient/bambu/feasibility/slicing/cuts renamed. Gates: fab-lang suite + workspace suite green
  - [x] M.0.3 - `KernelDriver` differential harness (`manifold/src/oracle.rs`, behind `oracle` feat + non-wasm): trait + RustKernel (our `Mesh`) + CppKernel (manifold3d); `cpp_to_mesh_gl` lets the C++ kernel GENERATE test geometry both engines re-ingest. GREEN + STRONG: unit cube volume+area BIT-IDENTICAL to C++ (`to_bits()==`); sphere(64)/cylinder/cube + a boolean result (sphere−cube, ~thousands of irregular tris) all match within 1e-9 rel; sphere scale sweep r=0.5→500 clean. = the R0 thesis in miniature (mathf+linalg+Kahan-volume reproduce C++ on real curved geometry). Boolean-residual metric (G.3.7) parked to M.1 (needs Rust booleans); genus/component backstops join as the crate grows.
  - [x] M.0.4 - The invariant `check` module (`manifold/src/check.rs`): finite / euler_characteristic (χ=V−E+F) / genus (1−χ/2) / euler_consistent (χ even), `strictly` composite gate (is_manifold+finite+parity, first-failure named), `intermediate_check(KernelParams)` hook for R1 booleans (panics when on, verified both ways). Deferred LOUD: self-intersection (collider→R2), `related` provenance (booleans→R1). Genus wired into the oracle differential too = exact-integer backstop vs C++ (sphere/cyl/cube/boolean all agree). 27 default + 30 oracle tests.
  - [x] M.0.5 - The mesh spine (`manifold/src/mesh.rs`): `Mesh` (= Manifold's `Impl`) with halfedge stored `(start,paired,prop)` + DERIVED end (mirrors the `Halfedges` SoA so `CheckHalfedges` transliterates 1:1). `create_halfedges` (deterministic clean-mesh edge pairing — opposed-tri REMOVAL deferred to R1), `MeshGl`↔`Mesh` round-trip (pos-only AND extra props e.g. RGBA), `is_manifold` (verbatim), Kahan `volume`/`surface_area`, NaN-skip bbox. 11 tests (Euler check, exact unit-cube vol/area, offset-cube pins the FP cancellation the C++ shares → why K.0 compares engines not analytics, non-manifold rejection). Scope gaps LOUD-deferred (dedup→R3, epsilon/normals later).
  - [x] M.0.6 - GATE K.0 PASSED (`oracle::tests::k0_gate`): on identical buffers the Rust spine (1) accepts-iff-C++-accepts, (2) agrees with C++ on volume/area/genus/bbox to 1e-9 rel (breaks invariant-circularity — volume/genus trustworthy because calibrated vs C++ before check.rs asserts on them), (3) round-trips volume bit-exact. Corpus spans genus 0+1, primitives+booleans: sphere-32/128 (up to 8192 tris), cylinder, box, sphere∪box, tunnel-block(genus 1) — ALL rust==cpp ✓. Unit cube volume+area literally bit-identical. R0 correctness thesis PROVEN end-to-end.
  - [x] M.0.7 - `par::` spike (`manifold/src/par.rs`): Reducer + `CommutativeAssociative` MARKER — `reduce` requires it so a non-associative float sum WON'T COMPILE (proven by a `compile_fail` doctest); float sums route to `reduce_serial` (fixed order, where Kahan volume lives). `map_collect` order-preserving, `BoxUnion` CA reducer, `bbox_of` demo. Dual-gated (`par` AND `not(wasm)`) so wasm is ALWAYS serial → serial-wasm ships without nightly atomics; `par==serial` asserted. Threaded-wasm⟷Bevy coexistence (risk #5) DEFERRED w/ rationale (serial-wasm sidesteps it). Real swap-in = M.4. Green across default/par/oracle/par,oracle/wasm.
  - [x] M.0.8 - 100% line + function coverage of the R0 primitives (merged default+par+oracle runs): mathf/linalg/mesh/check/par AND the oracle scaffold all at functions-100 + lines-100 (4 residual REGIONS = short-circuit sub-exprs in the verbatim mathf, region≠line). Hard branches got real work: precision-searched rem_pio2 triggers (2915.397982531328 → 3rd step; 8.639…/13.351… → ∓1 corrections; exact π/2-multiples), direct rem_pio2(inf) for the unreachable-via-callers arm, dangling-vertex mesh for `strictly`'s Euler guard + the oracle genus-push, NaN-vert + opposed-flap meshes for both asymmetric-validity arms. Cross-checks refactored to assert-inline (no dead branches).
- [x] M.1 - R1 **COMPLETE** (last box M.1.5's fuzz lane closed 2026-07-15; the go/no-go itself went GREEN at M.1.6 back on 07-14). R1: the TRACER boolean (union, serial) ★ GO/NO-GO. ~4030 NCLOC. Full scope + risk register in **`SPEC_manifold-rs_R1.md`**. STRATEGY = offset-first: an OFFSET cube∪cube is general-position → `Shadows p==q` never fires → the perturbation normals (the libm-acos hazard) are never consulted, isolating the CORE pipeline; THEN harden for coincident/shared-face. DEFERRED (not needed for a residual-clean union): all of `edge_op` (~750 — IsManifold passes BEFORE SimplifyTopology runs), the LBVH (serial brute-force stands in), 2D holes/keyhole, every parallel path, Slice/Project.
  - [x] M.1.0 - Foundations: the shared boolean vocabulary + perturbation inputs. value-`Halfedge{start,end,pair,prop}` + `TriRef` + `TmpEdge`; mutable `Halfedges` accessors (setters, `ForVert` orbit); `vec2`/`vec4`; `Intersections{p1q2,x12,v12}`. Perturbation predicates VERBATIM (shared.h): `Shadows`(`p==q?dir<0:p<q`), `withSign`, `Interpolate`, `Intersect`; `CCW`(`area²·4≤base²·tol²` — literal 4 + ≤ + squared form), `GetAxisAlignedProjection`, `determinant2x2`. Inputs: `faceNormal_`(cross+normalize, NaN→(0,0,1)), `SetEpsilon`/`tolerance`(MaxEpsilon,kPrecision). `vertNormal_` DEFERRED to M.1.4. Unit-diff each vs C++; NO mul_add anywhere.
  - [x] M.1.1 - Broad phase (serial): brute-force O(n·leaves) `Collisions` emitting Box×Box + XY-projected-vec3 overlap pairs (empty-box early-out, selfCollision=false); LBVH deferred. `SortGeometry` (Morton sort verts+faces + reindex) — DECISION: needed for bit-match, but for the OFFSET tracer (no coord ties) the result is order-independent → start WITHOUT it, add only if the residual diverges.
  - [x] M.1.2 - boolean3 (the robustness core): cascade `Shadow01→Kernel02→Kernel11→Kernel12` (expandP=true, forward∈{t,f}); `Intersect12` (serial recorder localStore + stable_sort/Permute); `Winding03` (`DisjointSets` rank+lower-index tie rule + Kernel02 flood fill); `Boolean3` ctor Add-path → the four tables `xv12_/xv21_/w03_/w30_`. Snapshot-compare all four vs C++ (serial `MANIFOLD_PAR=NONE` oracle as debug aid).
  - [x] M.1.3 - boolean_result (assembly) + face_op (retriangulation) + polygon (EarClip subset): winding→inclusion, `exclusive_scan`+AbsSum vert remaps, `DuplicateVerts`, `AddNewEdgeVerts` (serial **BTreeMap not HashMap** — iteration order load-bearing), `SizeOutput`, `EdgePos`/`PairUp`, `AppendPartial/New/WholeEdges` → faceHalfedges. Then `Face2Tri` serial + `AssembleHalfedges` (FIFO multimap equal-key) + `WriteLocalTriangles` (tri + quad-diagonal via CCW/length2); polygon `IsConvex`+`TriangulateConvex`(alternating fan) + the simple-polygon EarClip core for >4-gon cut faces (tree2d + holes deferred). **★ GATE-A (offset go/no-go): OFFSET cube∪cube residual-clean <1e-5 vs C++, IsManifold + exact-genus + analytic-volume. CLEAN → the core is PROVEN. NOT clean → diagnose the four tables, or STOP.**
    - **✅ GATE-A GREEN (2026-07-14): residual = 0.000e0** (bit-identical to C++, not just <1e-5) across offset cube∪cube (50 tris, vol 1.79) AND a 4-config general-position box∪box sweep (vol 8.895/7.0/25.0). IsManifold + genus 0 + analytic volume all pass; genus/volume also match C++ exactly. **THE CORE IS PROVEN.** Face2Tri UNIFIED past the C++ tri/quad/general split (all faces: assemble→earclip→generalized-WriteLocalTriangles); ear-clip is a no-BVH no-holes core (verbatim EarClip = tree2d+holes+Delaunay-cost = later determinism task, legit because the residual is triangulation-independent). Deferred (don't change the covered solid): provenance, SimplifyTopology, SortGeometry, ReorderHalfedges; RemoveUnreferencedVerts kept as a COMPACTION (keeps genus exact absent SortGeometry). Commits 98fd80fd (impl) + b150ebf6 (gate).
  - [x] M.1.3.1 - Misuse-resistant typing (chotchki, 2026-07-14 — "make it hard to use the APIs wrong"; the boolean surface was full of interchangeable `&[i32]`/`i32` args). Introduced `mesh_ids` newtypes `VertId`/`HalfedgeId`/`TriId` (`#[repr(transparent)]` over i32, zero-cost, derive Ord/Eq, `-1` = `NONE` sentinel + `is_none()`), with EXPLICIT NAMED conversions replacing bare index arithmetic (`HalfedgeId::tri()`=`/3`, `TriId::halfedge(i)`=`3t+i`, `.next()`/`.prev()`, `.u()`→usize). Threaded through the API SURFACE: mesh.rs accessors (`start(HalfedgeId)->VertId`, setters) + value `Halfedge`/`TmpEdge` + every boolean fn signature. Remaps→`Vec<VertId>`, inclusion COUNTS stay `Vec<i32>`, `edgesNew` keys `(TriId,TriId)`, `edgesP` keys `HalfedgeId`. PRAGMATIC boundary held: `Intersections.p1q2` stays raw `[i32;2]` (the runtime `[index]` forward/reverse trick), `face_halfedges` buffer index stays i32 (a distinct local space, named `LocalEdge`), `TriRef` stays i32 (mesh-instance namespace). Single id per axis, NO phantom P/Q/R. GATE-A stayed residual=0.000e0 through the change (zero-cost proven). 83 default + 90 oracle green, clippy -D clean default+par+oracle, wasm builds. Commit 1f6b8695.
    - **DECISION (chotchki, 2026-07-14): struct-grouping the many-arg assembly helpers is DEFERRED to a dedicated builder-refactor phase AFTER the port works** — foundation-first: lock in a validated, C++-faithful transliteration through the risky R1→R2 robustness work (long-arg free fns stay 1:1 with the reference for auditing), THEN refactor ergonomics onto a green baseline. Tracked below as the post-R2 builder pass. The typed args already deliver the "hard to use wrong" win now.
  - [x] M.1.4 - Perturbation hardening (coincident): **GATE-B validation only — the port already shipped in M.1.2.** DISCOVERY: Manifold's `CalculateVertNormals` uses its OWN `math::acos` (impl.cpp:748), NOT platform `acos`, and `mathf::acos` is already that bit-exact transliteration — so `calculate_vert_normals` (angle-weighted pseudonormal, `ForVert` order) landed in M.1.2 bit-exact WITHOUT the `libm` crate. The "pull libm forward" plan is moot. This task = drive the axis-aligned face-sharing cube∪cube (coincident coords → `p==q` fires → the normals ARE consulted) to residual-clean, confirming the shared-coordinate tie-break bit-matches C++. **GATE-B: axis-aligned face-sharing cube∪cube residual-clean.**
    - **✅ GATE-B GREEN (2026-07-14, commit b38b1296): residual = 0.000e0 + genus match vs C++** across 6 coincident configs — shared y,z planes / shared z plane / face-touching / shared x,y / shared x,z / Q-contained-sharing-corner. `p==q` fires (normals consulted for the FIRST time, inert through all GATE-A) and the shared-coordinate tie-break bit-matches the reference. Validation-only confirmed — nothing new needed in the kernel. (Rust tri counts run higher than C++, e.g. 44 vs 28, from skipping SimplifyTopology's coplanar merge — topologically identical, residual-independent.)
    - **⚠→✅ TRIPWIRE RESOLVED — `edge_op` STAYS R2 (does NOT pull forward):** GATE-B's face-sharing IsManifold all PASSED. Only the FULLY-COPLANAR extreme (identical cubes, every face coincident — beyond face-sharing scope) doubles faces → genus −1, because there's no coplanar-face merge (edge_op/SimplifyTopology). NOT a tie-break bug (partial-coincidence is residual-0, so the cascade is correct) — the missing CLEANUP. Captured as `oracle::identical_cubes_need_coplanar_merge_r2` (`#[ignore]`d with the R2 acceptance criterion baked in; un-ignoring = the R2 fix-check). So R1 covers realistic coincidence; fully-coplanar merge is legitimately R2.
  - [x] M.1.5 - Fuzzer: ONE `#[derive(Arbitrary)]` structure-aware CSG-tree generator (up to 100 transformed cubes, random union) — proptest fast-gate + cargo-fuzz/ASan continuous, `KernelParams.intermediate_checks=true`. Port `polygon_fuzz` too.
    - **DESIGN NOTE (from GATE-B): the generator must use CONTINUOUS random transforms (float translate/rotate/scale), NOT an integer/grid lattice.** GATE-B proved fully-coplanar-face unions (identical/grid-aligned cubes) need edge_op coplanar-merge = R2 — so a grid generator would flood M.1.6 with R2-deferred cases as FALSE thesis failures. With continuous transforms exact coplanarity is measure-zero, keeping the thesis gate honest to R1's actual scope. (If a grid mode is ever wanted, gate it behind the R2 edge_op landing.)
    - **✅ PROPTEST FAST-GATES DONE:** (1) `oracle::prop_cube_fold_unions_match_cpp` (64 cases, commit 3e3efd76) — boolean fold-union differential, shrinks to the minimal diverging box set. (2) `boolean::polygon::ear_clip_tiles_any_star_polygon` (commit 622d57b1) — `polygon_fuzz` for the ear-clip (the one NON-verbatim component): simple star polygons by construction, 50000-case stress clean. It caught a real GENERATOR bug (a 196° angular gap → self-intersecting input, outside the ear-clip's simple-polygon contract) — fixed by constructing valid inputs.
    - **✅ M.1.5 TAIL BUILT + SOAKED (decision: chotchki 2026-07-15 — `manifold/fuzz/`, corpus gitignored, nightly cargo-fuzz).** Two ASan targets: `csg_tree` (up to 100 CONTINUOUSLY-transformed cubes fold-unioned, `intermediate_checks=true` = strictly-manifold after EVERY op) + `polygon` (valid-by-construction star polygons through the ear clip, n−2 law). First 5-min soaks CLEAN against expectation: csg_tree 263,589 runs / polygon 1,616,082 runs, 0 crashes, 0 trophies, corpora 2,243/690 entries. Fuzz crate excluded from the stable workspace (own root); trophies-if-found land as MINIMIZED regression tests, never the corpus. **K.5's 24h clause: `cargo +nightly fuzz run csg_tree -- -max_total_time=86400` (chotchki's launch; fuzz/README.md has both commands).**
  - [x] M.1.6 - GATE (THESIS): union of random transformed cubes boolean-residual-clean <1e-5 vs C++ AND IsManifold/exact-genus/analytic-Volume over the fuzzer + polygon_fuzz 1h ASan-clean. **CLEAN → thesis PROVEN** (tail is execution risk). **NOT clean → STOP at R1.** — **CLOSED on correct-SOLID (chotchki, 2026-07-14): the go/no-go is GREEN.** Core geometry PROVEN (Monte-Carlo 0/100000 + bit-identical volume, all-manifold, rotated folds included). "exact-genus" + the 1h ASan-continuous move to R2/tail (see the corrected sub-note above); SimplifyTopology leads R2.
    - **✅ SOLID THESIS PROVEN — the go/no-go is GREEN, R1 does NOT stop (core is correct). ⚠ CORRECTED 2026-07-14: "exact-genus" is NOT met on folds — it needs SimplifyTopology = R2.** Two-part status:
      - **CORE / GEOMETRY: PROVEN (commit 981a14ed).** Random multi-cube FOLD-unions (boolean run on its OWN output, axis-aligned 120-trial + 600×8 stress, AND rotated 120-trial) are all WATERTIGHT + volume BIT-IDENTICAL + Monte-Carlo containment 0/100000 vs C++. The four-table intersection + assembly produce the geometrically correct solid, general position or coincident. This is the actual go/no-go and it's GREEN.
      - **CLEANLINESS / TOPOLOGY: needs R2.** The earlier residual-only "thesis proven" (commit fc242ff0) OVERCLAIMED — the C++-boolean residual silently tolerated un-clean topology. Reality: un-simplified R1 folds accumulate INTERNAL degenerate structure (coincident/doubled walls at seams) → wrong genus (−2 vs 0 by the 5th rotated fold) + inflated area, WITHOUT changing the solid (Monte-Carlo proves it). Appears on ~7.5% of single rotated unions too — so SimplifyTopology/edge_op is needed for the GENERAL case, not just exact-coincident inputs. **`genus`/`area`/residual are cleanliness-sensitive → NOT valid gates for un-simplified meshes; only volume + bbox + Monte-Carlo containment are** (chotchki's methodology, commit 981a14ed). Every output stays MANIFOLD throughout.
      - **⬜ OPEN SCOPING (chotchki): does R1 close on correct-SOLID (exact-genus → R2), or does SimplifyTopology come FORWARD into R1?** Plus the M.1.5 tail (cargo-fuzz/ASan + polygon_fuzz).
    - **⚠ TRIPWIRES (deferrals that may pull forward here, both gate-protected — LOUD not silent): (1) `edge_op`/`SimplifyTopology` (R2/M.2) if random-cube unions leave degenerate topology IsManifold rejects. (2) The EarClip HOLES/keyhole path (R2/M.2 polygon hardening): the M.1.3 ear-clip has NO keyholing, so if a union-of-many-cubes ever produces a cut face with an interior hole loop — unproven for convex primitives (intersection curves cross face boundaries rather than forming islands, but a nested arrangement is not ruled out) — it fails non-manifold and keyholing pulls forward from R2.**
- [x] M.2 - R2 **COMPLETE modulo the 24h soak** (all children done; the corpus runs all three ops solid-clean + component-structure-matched vs C++, M.2.4a fixed the two verbatim-port gaps it exposed). Gate K.5 **CLOSED (chotchki's call, 2026-07-15)**: 0-divergence GREEN + the ASan soak run and CALLED at ~2h clean post-fix csg_tree (178,076 execs, strictly-manifold after every op) + ~4.5h polygon (29.5M execs, clean throughout) — 1 trophy found AND fixed during the campaign (M.2.4b). Two slow-unit artifacts noted (heavy near-coincident folds — the big-twin effect, not crashes). R2: full robustness core — difference/intersection (boolean3 op param) + edge_op cleanup + polygon hardening + the 17-model nasty corpus.
  - **LEADS WITH `SimplifyTopology`/edge_op (M.1.6 finding, confirmed-needed-for-general):** un-simplified R1 folds accumulate internal degenerate structure → wrong genus + inflated area (solid still correct + manifold). Needed for the GENERAL case, not just exact-coincident. Landing it flips `oracle::identical_cubes_need_coplanar_merge_r2` (`#[ignore]`d, R2 criterion baked in) to green AND lets the fold gates re-add exact-genus (currently vol+bbox+Monte-Carlo only, per the un-simplified-mesh methodology). The ✅ metric to watch: rotated folds hit genus-0 + the residual drops to ~0.
  - [x] M.2.1 - difference + intersection (the `op` param). VALIDATED (commit 4fab687e): the c1/c2/c3 inclusion transforms + invertQ face-flip were ported in M.1.3, never exercised; now offset P−Q (0.79) + P∩Q (0.21) analytic + a 4-config general-position sweep of both ops all watertight + volume-matched + solid-oracle-clean vs C++. Port was correct first try.
  - [x] M.2.2 - `SimplifyTopology`/edge_op — DONE (commits 07ab049f safe-subset, 8e619176 Stage C). All 5 stages ported verbatim (SplitPinchedVerts + DedupeEdges + CollapseShortEdges + CollapseColinearEdges + SwapDegenerates) + CollapseEdge/FormLoop/CollapseTri/RemoveIfFolded/UpdateVert/DedupeEdge/RecursiveEdgeSwap, the mark-then-compact removal, and mesh ops (set_end/set_halfedge/remove_dead_triangles). **`identical_cubes_need_coplanar_merge_r2` un-ignored GREEN.** KEY ORDERING: CollapseColinearEdges MUST run before SwapDegenerates (swap-without-colinear mis-collapses real geometry, −1.16e-3 measured) — the dependency that forced M.2.2.1 forward.
    - **⚠ DEPENDENCY FINDING: `CollapseColinearEdges` NEEDS `triRef`/coplanar-ID (provenance), which R1 DEFERRED.** Its "colinear" predicate is `SameFace(triRef)`-based (coplanar_id set by SetNormalsAndCoplanar's flooding — also deferred). The OTHER four stages are provenance-FREE: SplitPinchedVerts/DedupeEdges/SwapDegenerates/CollapseShortEdges don't need triRef (short-edge CollapseEdge skips the triRef redundancy check at `if(!shortEdge)`; triRef reads/writes elsewhere are prop-guarded or skippable). So the FORK (biggest R2 decision): **(A) minimal-subset-first** — port the 4 provenance-free stages, TEST if they fix the fold genus (DedupeEdges kills duplicate edges from coincident faces, SwapDegenerates kills slivers), add CollapseColinearEdges + provenance only if needed; vs **(B) provenance-first** — port triRef/coplanar-ID (MapTriRef/UpdateReference + SetNormalsAndCoplanar flooding) THEN full SimplifyTopology. A cleans genus with less code IF the internal structure is dup-edges/slivers not coplanar-redundant; B is complete but pulls the deferred provenance forward.
  - [x] M.2.2.1 - Provenance: `triRef` + coplanar-ID — DONE (commits 550bbc7a Stage A, 8d640c3c Stage B). Pulled forward (chotchki: "not a fan of multiple defers" — colinear AND swap both needed it). Shipped: (a) `set_normals_and_coplanar` area-sorted coplanar FLOOD + normal-snap + `initialize_original` + the global `MESH_ID_COUNTER`/`reserve_ids`; (b) the temp-`halfedgeRef` (`{meshID:0|1, faceID:srcTri}`) threaded through AppendPartial/New/WholeEdges; (c) Face2Tri's `WriteTriRefs` (face's first-halfedge ref per output tri); (d) `update_reference`/MapTriRef (Q's meshID offset by the counter). `vocab::TriRef`/`same_face` (M.1.0) wired through; the color/UV `CreateProperties` half stays R3.
  - [x] M.2.2.2 - Determinism canonicalization: `ReorderHalfedges` + `SortGeometry` + Delaunay-cost `EarClip` — DONE (commits 8e619176, 411743d6). Got rotated folds from accumulating divergence to **112/120 byte-identical vs C++**; all 120 Monte-Carlo-clean. `ReorderHalfedges` (within-face order) + `SortGeometry` (Morton vert/face reindex → chained-op intermediates match C++'s order) + `earclip` (Delaunay-cost ear selection, simple-poly, BTreeSet `(cost,seq)` queue — replaces the textbook clip in Face2Tri).
  - [x] M.2.2.3 - **Determinism close-out — DONE (commit b5228c95), and the diagnosis was WRONG.** The residual was NOT "SimplifyTopology collapse-order divergence" — it was FILLED-OVER HOLES. The old per-loop Face2Tri filled interior hole loops (zero-volume internal walls → the "wrong genus" cases) and inverted CW loops (→ the "~8 volume outliers"). M.2.3's multi-loop keyhole EarClip fixed both. Re-measured post-keyhole: all 120 rotated folds byte-identical to C++ in VOLUME (rel < 1e-12) + genus-matched + MC-clean. So no collapse-cascade chase was needed. Shipped: `solid_divergence` now checks genus-match on every differential; rotated thesis + nasty corpus gates tightened 2e-2 → 1e-9 (both hold, self-intersecting corpus included); `keyhole_boolean_holes_match_cpp` (256-case boolean-level hole fuzzer vs C++, chotchki's ask — the hole case is now fuzz-produced, not hand-built). The earlier "genus-invariant to ear-clip" finding only tested the single-loop START (output-irrelevant); it never varied the HOLE handling, which is where the divergence actually lived.
  - [x] M.2.3 - polygon hardening: keyhole HOLES — DONE (commit ee73171b). Rewrote `polygon.rs` as the verbatim multi-loop `EarClip::Triangulate`: `Initialize` (all loops → one arena) + `FindStart` (signed-area Kahan classification → outers/holes + rightmost-reflex start) + `CutKeyhole`/`FindCloserBridge`/`JoinPolygons` (ray-cast right via `InterpY2X`, refine, splice via 2 duplicated verts) + `InsideEdge`/`IsReflex`/`InterpY2X` line-by-line. Index-based arena (no C++ iterator-invalidation dance). `ear_cost` brute force scoped to the CURRENT loop (`self.active`) to match `VertCollider`'s scope — the collider is perf-only (proven M.2.2.2). `face2tri` now hands ALL loops of a face to `triangulate()` together (was per-loop earclip → filled holes + inverted CW loops). Gate MET: annulus tiling (square+hole, offset hole) + a 2048-case keyhole fuzz + the integration test `r2_tunnel_difference_holed_face_vs_cpp` (box drilled through = genus-1, vol 840, solid-clean vs C++). 108 oracle / 91 default green, rotated-fold thesis + nasty corpus unchanged through the rewrite.
  - [x] M.2.4 - the NASTY corpus **COMPLETE**: every pair (Havocglass8, Cray, Generic_Twin ×2 incl. the big twin, self_intersectA/B) through ALL THREE ops, solid-oracle-clean + component-structure-matched vs C++ (the M.2.4a un-blinding: component count + per-component volumes at each component's OWN scale — e13 garbage can't hide under an e116 total again). The self_intersect diff/intersect genus/components gates are LOUDLY waived (ε-invalid inputs; upstream only ever UNIONS them with processOverlaps). K.5's 0-divergence half is GREEN; the 24h-ASan half rides M.1.5's open cargo-fuzz tail (chotchki's corpus/toolchain decision).
    - **STARTED (commit 6c7f8b5e): the SMALL pairs are GREEN** — `nasty_corpus_union_vs_cpp` (.obj loader + build-dir path resolver) unions Havocglass8 (self-intersecting), Cray (extreme coords, verts at ±3.4e38 = f32::MAX, vol ~1.5e116), Generic_Twin_7863 → all manifold + solid-divergence-clean vs C++. So my boolean handles real self-intersecting / extreme-coordinate geometry.
    - **BIG models UNBLOCKED by M.2.4.1 (commit 4727e1d9): both correct + MC-clean vs C++.** `self_intersectA ∪ self_intersectB` (17K+17K tri, 33ms release) EXACT-matches C++ (33542 tri, identical volume) → now IN `nasty_corpus_union_vs_cpp` (800 MC samples). `Generic_Twin_7081` (20K, `m1 + m2`) is correct but ~15s release / ~190s debug — it's a near-COINCIDENT twin whose face boxes overlap almost everywhere, so `intersect12` emits ~64.5M candidate box overlaps for ~1024 real hits; the LBVH finds them fine, the cost is the SERIAL narrow phase (~124M `kernel12`) → RESOLVED by M.4.1 (the parallel narrow phase, commit b88a55e6): 15s→1.2s (12.8×), now EXACT vs C++. `big_twin_union_vs_cpp` stays `#[ignore]`d only for the debug+serial default lane (~190s there; ~3s release+par). Diagnosed via the `manifold::boolean` tracing target.
    - self_intersectA/B are `m1 + m2` UNION in `boolean_complex_test.cpp` (with `processOverlaps=true`, which we match WITHOUT it). Add explicit difference/intersection corpus cases (M.2.1 already exercises those on offset cubes).
  - [x] M.2.4.1 - **LBVH broad phase** — DONE (commit 40401288). Ported `collider.h`'s Karras Morton-BVH verbatim into `boolean/collider.rs`: `CreateRadixTree` (split via `PrefixLength`+CLZ, ties→leaf index), `BuildInternalBoxes` (bottom-up union), `FindCollision` (stack traversal). SERIAL — a plain counter reproduces `BuildInternalBoxes` bit-for-bit (box union is exact min/max, computed once per node), dropping the parallel/atomic build. Reuses `morton_code`/`K_NO_CODE` from `sort.rs` (now `pub(crate)`). Karras needs Morton-SORTED leaves; rather than mutate the caller's mesh, `from_mesh` sorts internally and keeps a `leaf2face` remap applied at the record boundary (callers still see face indices). PERF-ONLY gate MET: `collisions_brute` retained as the differential oracle, two new unit tests assert BVH == brute-force SET (overlapping cubes + a dense 27-cube grid); full differential suite unchanged through the BVH (104 oracle / 88 default green). Both query modes kept (Box3 + Vec3-XY-projected). Skipped: parallel/atomic build, `Transform` variants.
  - [x] M.2.4a - M.2.4a - Cray mixed-scale REGRESSION (caught by the new diff/intersect corpus gate): L−R emits garbage inverted-shell component pairs (±e13, larger than L itself) vs C++ 4.91e10; union hid the same garbage under the 1e-9-RELATIVE volume gate vs R's 1.58e116. Diagnose stage where garbage enters (raw tables vs simplify/epsilon at max(eps_P, eps_Q)~3.4e29), fix verbatim, un-blind the union gate
  - [x] M.2.4b - M.2.4b - FUZZER TROPHY #1 (csg_tree, ~2h in): non-terminating Vec&lt;HalfedgeId&gt; push in split_pinched_verts (the M.3.9 class — an orbit that never closes) on a fuzzed cube-fold union → 2GB grow_one OOM. Diagnosis agent on it: decode → minimize → root-cause vs C++ SplitPinchedVerts → verbatim fix + regression; goldens must hold
- [x] M.3 - R3 **COMPLETE** (every child [x]; M.3.8's kernel ops delivered via M.5.3 — the residual fab-scad `Solid`/`CrossSection` WIRING is the scad-rs integration seam, tracked outside this standalone-kernel phase). R3: 3D completion — constructors + Decompose (the W.4 cavity contract), manifold.cpp (split/trim/slice/project), Volume/Area/Genus, csg_tree FLATTENED (~200 LOC eager — dissolves !Send, SPEC [OPEN #3]), quickhull, minkowski (volume-residual), transforms, color/set_properties (the BOSL2 color gate). Gate: fab-scad's whole `Solid` surface green vs C++ on the models/ sweep. Surface (from scad-rs `kernel.rs`): booleans DONE; transforms, split/trim/decompose/project, set_properties, minkowski, queries (status) TODO. `project`/`extrude`/`revolve` are 2D-blocked → R5 (LOUD-defer).
  - [x] M.3.1 - Transforms: `Mesh::transform(Mat3x4)` — DONE (commit 8022a8a5). Verbatim `Manifold::Impl::Transform`: positions through the affine, normals through the inverse-transpose (`Mat3::normal_transform` = `(b×c,c×a,a×b)/det`), mirror (det<0) → `flip_tris` (FlipTris/FlipHalfedge) so the surface stays outward, bbox recomputed, epsilon ×= spectral norm. New linalg `Mat3` + `Mat3x4::linear/is_finite`. SHORTCUT (tracked, task #4 + Backlog): spectral norm via power iteration on MᵀM, not the Jacobi SVD (epsilon-only, invisible to output geometry). Gate: `transform_moves_scales_and_mirrors` (mirror stays +volume/manifold/genus-0 = the flip_tris gate) + a Mat3 unit test. NOTE: status carry (non-finite → empty) deferred to M.3.2.
  - [x] M.3.2 - Status + trivial queries — DONE (commit 4e24ee7e). Ported `Manifold::Error` (thiserror, 14 variants; `NoError` DROPPED — that's `Ok`). DECISION (chotchki, 2026-07-14): eager ops surface failure as `Result<Mesh, Error>`, NOT the C++ lazy-CSG `status_` field / `status()`. The field only exists because a `Manifold` is a lazy-tree node that can't return a `Result`; M.3 flattens csg_tree to eager, so propagation becomes `?` and the "Mesh silently carrying a latent error" misuse state is unrepresentable. `Result<Option<Mesh>>` rejected (empty Mesh already a valid value → `Ok(None)` would be a redundant second encoding). Geometry of an error path is the empty mesh either way, so the differential oracle is unaffected. `transform` → `Result` (non-finite → `Err(NonFiniteVertex)`). Added query surface `Mesh::is_finite` (`Impl::IsFinite`) + `Mesh::bounding_box`; is_empty/num_tri/num_vert/volume/surface_area/genus already oracle-gated vs C++.
  - [x] M.3.3 - Decompose — DONE (commit 3e17227f). `Mesh::decompose`: union-find over forward halfedges → per-vert labels → each component extracted (vert subset + owned tris, re-paired via `create_halfedges` + canonicalized by `sort_geometry`, inherits epsilon/tolerance). Added `DisjointSets::connected_components`. Single component → clone; enclosed cavities stay separate (W.4). Gate: two-disjoint-cubes → 2 manifold parts vol 1 (total preserved) + single→1.
  - [x] M.3.4a - set_properties API — DONE (commit b7868178). `Mesh::set_properties(num_prop, prop_fn)` (Manifold's `SetProperties`): recompute each vertex's EXTRA properties from a `(new, position, old)` callback. `num_prop` = extras EXCLUDING position (C++ semantics; matches fab-scad `with_color`'s `set_properties(4, …)` → `Mesh::num_prop` 7; 0 strips to position-only). Position stays in `vert_pos`, never touched. Per-corner, idempotent, serial (deterministic). This is the `color()` OVERWRITE half. Gate: `set_properties_stamps_color_and_reads_old` (stride 3→7, reads old, strips to 3).
  - [x] M.3.4b - CreateProperties (the boolean COLOR-survival half) — DONE FAITHFUL, **ZERO CARVE-OUTS** (chotchki's fidelity call; commits 1d650aea→5c2fffb4). A coloured/UV/normal subtree keeps its properties across union/difference/intersection, byte-comparable to C++ (`∫ prop dA` per channel + tri-count match), survives a MeshGL serialization round-trip both ways vs C++ (merge-vectors, 4b.7), AND world-frame normals sign-flip correctly through Subtract (4b.8, gated vs C++). The decoupled prop-vert model (4b.1) is the keystone. Barycentric-interpolate source properties onto the boolean's NEW intersection verts so a colored subtree keeps its color through union/difference/intersection (the boolean hardcodes `num_prop = 3` today at `boolean_result.rs:620`). Slot the port between `reorder_halfedges` and `update_reference` (mirrors C++ 947→950 — reads `tri_ref` in its TEMP `{0|1, srcTri}` form). RE-SCOPED (scout 2026-07-14): NOT a self-contained 117-LOC port. The 117 lines (`boolean_result.cpp:571-687`) + `Barycentric` (540-569) + `GetBarycentric` (`shared.h:123-160`) are the easy part; the hard part is that C++ `CreateProperties` MANUFACTURES split prop-verts (a `properties_` array keyed by prop-vert, growing independently of geometric verts via the `propIdx`/`propMissIdx`/`idx++` dedup), while our `Mesh` is hardwired 1:1 (`prop_vert == start_vert`, no `properties_`/`NumPropVert`). Upstream boolean state is all present at the seam; the gap is entirely the Mesh property model. **DECISION FORK (needs chotchki):** (a) FAITHFUL — decouple `prop_vert` from `start_vert` across `from_mesh_gl`/`to_mesh_gl`/`sort_geometry`/`simplify_topology`/`remove_unreferenced_verts` (Mesh surgery, ≫117 LOC) — the only path that matches C++ `RelatedGL` at property SEAMS; or (b) 1:1 color-survival approximation (~150 LOC, interpolate per new geometric vert, skip seam-split) — delivers "color survives a boolean" but DIVERGES from C++ wherever it splits a prop-vert at a seam = a 95%-right port the verbatim thesis forbids. Gate: `properties`/`RelatedGL` differential vs C++ (path (b) can't pass it byte-identical on UV/normal meshes). Biggest risk: the prop-vert decoupling.
    - **DECISION (chotchki, 2026-07-15): FAITHFUL — path (a).** "Otherwise we can't prove we didn't diverge too much." The differential oracle IS the proof, and it can only prove parity against a reference we didn't deliberately diverge from — a 1:1 shortcut diverges exactly at property seams, the one place left unprovable. Scoped by the 2026-07-15 surgery scout into 6 tasks + 1 deferral. Keystone finding: C++ `CollapseEdge` doesn't merely re-point prop-verts, it INTERPOLATES-AND-GROWS `properties_` (edge_op.cpp:658-672) — so props feed back into the GEOMETRY pass (`SimplifyTopology` must be prop-aware, and a property-carrying boolean can triangulate differently than position-only). Mercy that keeps the gate cheap: C++ `Impl` ctor position-dedups MeshGL rows when merge-vectors are absent, so a per-face-colored cube (24 rows / 8 geo verts) round-trips with NO merge-vector encoding — exact-position dedup in `from_mesh_gl` matches it. Invariant across all 6: position-only degenerates to the current 1:1 model (`num_prop_ == 0` ⟹ `num_prop_vert == num_vert`, `prop_vert == start_vert`), so the 111 existing tests stay byte-identical.
    - [x] M.3.4b.1 - Decouple the Mesh property model — the keystone. DONE. `props_extra` → prop-vert-indexed `properties` (stride = C++ `numProp_`, position excluded); `Mesh::num_prop` FLIPPED to `numProp_` semantics (extra-count, `0` = position-only — matches C++ `Impl` exactly, so the downstream property ports read verbatim; `MeshGl::num_prop` stays interchange, `+3`). Added `num_prop_vert()`. `Halfedge.prop_vert` doc'd as a genuine decoupled index. Rippled the position-only `Mesh` literals `3`→`0` (boolean output at `boolean_result.rs:620` — the "hardcoded num_prop" the fork flagged — plus cube/decompose/quickhull/minkowski); `from_mesh_gl`/`to_mesh_gl` translate `∓3` at the interchange boundary (still 1:1 ingest/emit — decouple is 4b.2/4b.3). Gate MET: 112 default (+`decoupled_prop_verts_index_their_own_space`, a hand-built `num_prop_vert=4 > num_vert=3` seam) / 133 oracle green vs C++, zero regression; clippy clean (incl. a drive-by on the M.3.9 octagon test).
    - [x] M.3.4b.2 - `from_mesh_gl` ingest — DONE BY CORRECTION (scout premise was wrong). Reading the C++ `Impl` MeshGL ctor (impl.h:356) settled it: C++ does NOT position-dedup on ingest — when `mergeFromVert`/`mergeToVert` are ABSENT, `prop2vert` stays empty and geometric vert == prop-vert == MeshGL row (strict 1:1); coincident positions merge ONLY via explicit merge-vectors. So our current 1:1 `from_mesh_gl` (post-4b.1, translating `−3`) ALREADY matches C++ for merge-vector-absent inputs — which is precisely the single-boolean gate's per-vertex-colored inputs. The output's prop-vert decoupling comes entirely from `CreateProperties`/`CollapseEdge`, not ingest. The merge-vector ingest path (per-face colors, 24 rows → 8 geo verts) folds into 4b.7 (it needs the `MeshGl` merge fields anyway). No new ingest code.
    - [x] M.3.4b.3 - `to_mesh_gl`: emit prop-verts — DONE (commit eaab4ee7). One interchange row per prop-vert (position from each prop-vert's geo vert via a half-edge scan, extras from `properties`), `tri_verts` = prop-verts; position-only keeps the 1:1 fast path. NOT re-mergeable without mergeFromVert/To (→ 4b.7). Exercised by the gate.
    - [x] M.3.4b.4 - CreateProperties port — DONE (commit 41948a85). `boolean_result.cpp:540-687` + `GetBarycentric` (`shared.h:123-168`, in predicates.rs) + the `propIdx`/`propMissIdx`/`idx++` split-prop-vert dedup, slotted between `reorder_halfedges` and `update_reference` (reads `tri_ref` in TEMP `{0|1,srcTri}` form). DEVIATION (loud): `negateNormals` hardwired off (no `TriHasNormals` provenance) — exact for colour/UV, diverges only for world-frame normals-as-properties through Subtract.
    - [x] M.3.4b.5 - Prop-vert maintenance — DONE (commit eaab4ee7). `edge_op` collapse_edge repoint (579-587) + swap_edge interpolate-and-grow (657-673); `sort_geometry` CompactProps (independent prop-vert reindex) + `!has_prop` guard on `ReindexVerts`. **THE maintenance bug** was `remove_unreferenced_verts` remapping `prop_vert` through the GEOMETRIC remap — C++ never touches props there (CompactProps owns them), so now skipped when `num_prop > 0`. NOTE: properties DON'T change collapse/swap decisions (only values carried), so geometry stays byte-identical — my earlier "props change the triangulation" worry was wrong (DedupePropVerts, which would, isn't in the boolean path). Driven out by a self-checking test (colour = position ⟹ every corner is zero-from-B or its-own-position-from-A).
    - [x] M.3.4b.6 - THE GATE — DONE (commit 89927ea9). `m3_4b_properties_vs_cpp`: colour cube A by position, A−B in both engines, compare the area-weighted surface integral `∫ prop dA` per RGBA channel (triangulation-independent, like the volume residual) + tri-count match. Passes — properties carry across the seam identically to C++.
    - [x] M.3.4b.8 - negateNormals (chotchki 2026-07-15: "implement it now", close the carve-out) — DONE (commit 5c2fffb4). World-frame vertex normals carried as properties sign-flip through Subtract. `Mesh.mesh_id_has_normals` (BTreeSet — the per-meshID `hasNormals` flag, the only bit read; full `meshIDtransform` Relation not ported) + `mark_has_normals`/`tri_has_normals`; the `create_properties` negateNormals branch is now LIVE + verbatim; `update_reference` threads the provenance (P + Q-offset, C++ 530-535). SetNormals (COMPUTING normals w/ sharp-angle split) is the orthogonal "make normals" feature, separate box — this is the "normals SURVIVE a boolean" half. Gates: `subtract_flips_q_world_frame_normals` (self-check: flagged-vs-not must exactly negate each channel's ∫ integral) + `m3_4b_negate_normals_vs_cpp` (differential: real normals from C++ `calculate_normals`, ∫normal dA per channel matches).
    - [x] M.3.4b.7 - merge-vector MeshGL round-trip — DONE (commits 4db6846e + d00ecd54). `MeshGl` gains `merge_from_vert`/`merge_to_vert`; `to_mesh_gl` emits per (geo-vert, prop-vert) PAIR (Manifold `GetMeshGL` 620-683 — a prop-vert can be shared across many geo verts, e.g. the zero-prop row, so per-prop-vert gave an ambiguous position; that was the round-trip bug); `from_mesh_gl` merge branch (`prop2vert[from]=to`, C++ `Impl` ctor 355-368) folds coincident rows into a shared geo vert. 7a gate: colour-by-position output → serialize (WITH merge-vectors) → re-import → manifold + volume + colour preserved + a CHAINED further boolean carries colour. 7b: bridge extended (manifold-csg DOES expose merge-vectors via `MeshGL64Options.merge_vertices` / `to_meshgl64`), `m3_4b_merge_vector_round_trip_vs_cpp` gates BOTH directions — ours→C++ (C++ reconstructs the same solid) and C++→ours (re-import C++'s merge-encoded output). So a property-carrying output survives serialization (save/load, cross-subsystem) AND round-trips vs C++.
  - [x] M.3.5 - split/trim by plane — DONE (commits efcfc7d0, 07930761, 0f08467c). `Manifold::SplitByPlane`/`TrimByPlane`: `Mesh::half_space` (big cutting cuboid, Cube(2,centered) slab folded through Translate/Scale/Rotate into ONE `Mat3x4` — bit-faithful to lazy C++'s single Transform, epsilon scales by the product's spectral norm), `boolean_result::split` (Manifold's `Split`: one shared `Boolean3(Subtract)` → Result(Intersect)=+side + Result(Subtract)=−side), `Mesh::split_by_plane`/`trim_by_plane`. Scaffolding shipped: `Mat3::mul_mat3` + `Mat3x4::{translate,scale,rotate,Mul}` builders; `Mesh::cube` primitive (exact `Manifold::Cube`). Gate: `m3_5_split_trim_by_plane_vs_cpp` — 4 planes (axis-aligned + tilted) each solid-oracle-clean vs C++, conservation, trim==+side.
  - [x] M.3.6 - quickhull (`Manifold::Hull` via the QuickHull impl) — DONE (commits 3277c129, 6efb8c93). Verbatim port of `quickhull.cpp`/`.h` (Kuukka QuickHull) in `quickhull.rs` (~950 LOC): seed tetrahedron from extreme points, then iteratively extrude each face's farthest outside point onto its horizon loop. `Mesh::hull_of_points`/`hull` wrap it with the C++ `Impl::Hull` post-pass sequence. Two documented deviations, both invisible to the solid oracle: owned `Vec<Vec3>` replacing the `VecView` rebind to `planarPointCloudTemp` (self-ref Rust won't allow; single-buffer reproduces the aliasing incl the planar back=front reset), and a SERIAL reorder tail replacing the parallel `for_each`/`AtomicAdd`/`exclusive_scan` (only RENUMBERS — shape is fixed by the serial iteration — so bit-faithful to the C++ sequential policy; M.4 owns parallel determinism). Cancellation (ctx) dropped (none in this kernel yet). Gate: `m3_6_hull_vs_cpp` — 5 clouds (cube, cube+interior, tetra, Fibonacci-sphere-60, random box-cloud) each solid-oracle-clean vs C++ `hull_pts` (volume/genus/6k point-in-mesh) + 5 unit tests. Sealed by an adversarial 5-lens line-by-line verification (control-flow / index-arithmetic / numeric-ops / degenerate-paths / deviation-soundness): 0 confirmed divergences.
  - [x] M.3.7a - minkowski Tier 0 (convex×convex) — DONE (commit c3df2984). Port of `minkowski.cpp` (PR #666's tiered hull+union): the mesh's own triangle faces ARE the convex decomposition (no general convex-decomposition algorithm needed). `Mesh::minkowski_sum` dispatches on `Mesh::is_convex` (ported `Impl::IsConvex` — Euler χ + all-dihedrals-same-sign, self-contained via new `Mesh::tri_normal`); empty = identity (A⊕∅=A). Tier 0 = one hull of every vertex-sum. Gate `m3_7_minkowski_vs_cpp`: cube⊕cube + octahedron⊕cube solid-oracle-clean vs C++ `minkowski_sum`; unit tests add analytic cube(10)⊕cube(2)=box-1728 + empty-identity. Volume-residual (algorithm-independent, like J.4.4).
  - [x] M.3.7b - minkowski Tier 1/2 (nonconvex) — DONE (commit 676c6ace, unblocked by M.3.9). Tier 1 (nonconvex×convex, sweep the convex operand along each face) + Tier 2 (nonconvex×nonconvex, per-face-pair 9-point hulls, coplanar-skip), sequential-union fold. `m3_7_minkowski_vs_cpp` now gates all three tiers vs C++ (t0 cube⊕cube + octa⊕cube, t1 concave⊕cube, t2 concave⊕concave), 2.2s. `minkowski_sum` is complete for all convexity combos. STILL deferred: inset/`MinkowskiDifference` (erode) — its own later box, no stub, no caller.
  - [x] M.3.9 - BOOLEAN robustness: coplanar-union infinite loop — FIXED (commit 347601fb; found via M.3.7). Root cause: the ear-clip's `ring()` (polygon.rs) collapsed C++ `Loop()`'s PER-VERT re-anchor into a one-shot clipped-start check, so on a self-touching degenerate coplanar seam face it dropped a triangle → `face2tri` emitted spurious non-contour boundary edges → an output half-edge left UNPAIRED → `for_vert` (split_pinched_verts) walked off the NONE pair forever (Rust `-1 % 3 == -1` turns C++'s would-be OOB crash into a hang). Fix = mirror C++ `Loop()` verbatim (polygon.cpp:544-566), re-anchoring on every clipped vert. A PORT TRANSCRIPTION bug, not a new guard. Regressions: `self_touching_octagon_triangulation_is_contour_valid` (root-cause 8-gon) + `coplanar_slab_union_terminates_and_is_manifold` (the captured two slabs). Independently verified: 111 lib + 132 oracle differential, zero regression.
  - [x] M.3.8 - project/extrude/revolve/slice → **KERNEL OPS DELIVERED in M.5.3** (`bridge.rs`, gated vs C++). The `CrossSection` type + all four bridges now exist; the remaining piece is the fab-scad `Solid`/`CrossSection` wiring (the scad-rs integration seam, not this standalone-kernel branch). Was DEFERRED to M.5 (hard 2D dependency). These are 2D↔3D bridges: `project`/`slice` return a 2D `CrossSection`, `extrude`/`revolve` consume one — and the `CrossSection` type + the 2D subsystem don't exist until M.5. On the standalone kernel branch there's NO caller and NO return type to stub against, so a "blows-up stub" now would be guessing against M.5's shape (the anti-pattern the verbatim thesis forbids). The loud-defer stub lands in M.5 WITH the CrossSection type, at the fab-scad integration seam where a caller exists. Not a blocker for M.3→M.4: M.4 (deterministic parallel) is a sweep over the EXISTING 3D loops, none of which is 2D.
- [x] M.4 - R4: deterministic parallel — **GATE K.D MET** (chotchki's "prove-deterministic, stop at perf" scope): Seq==Par bit-identical (M.4.4 golden holds in both par configs) AND native==wasm bit-identical (M.4.4-wasm, wasmtime) AND run1==run2 (narrow_phase test). The pillar-1 proof C++ structurally can't pass — DONE. Total-order comparators (M.4.2) + the parallel maps (M.4.1 narrow phase, M.4.3a/b) are in; M.4.3c (duplicate_verts scatter + face2tri earclip) stays serial by decision — perf-neutral, the hotspot already parallel. Original scope line: swap `par::` in; total-order comparators, fixed-shape reductions, deterministic ids. **SEQUENCING (chotchki 2026-07-14): M.3 comes FIRST.** M.4 is a HORIZONTAL loop-by-loop sweep — every new geometry loop M.3 adds (Decompose/split/trim/slice/project/quickhull/minkowski/transforms/color) would re-open it, so completing the loop set with M.3 before the M.4 sweep avoids doing parallelism twice. M.4.1 (narrow phase) stays a justified pull-forward (targeted perf win, deterministic, no redo).
  - [x] M.4.2 - Total-order sort comparators — DONE (commit 4091fdbb). The audit's recurring fix — prerequisite for ANY parallel sort.
  - [x] M.4.3 - Parallelize the disjoint per-element maps through the `par::` seam (each safe-by-construction: order-preserving map / disjoint output ranges → par == seq bit-for-bit). Worklist from [[m4-parallel-sweep-worklist]]: `calculate_vert_normals` phase-2 (per-vertex normal), `duplicate_verts` (exclusive-scan-disjoint ranges), the face-box/centroid computes, `Face2Tri` pass-1 (per-face earclip — the main remaining perf target, but tiny on the current corpus). Gate: the full oracle differential passes with `--features par` too (INCLUDING the rotated-fold + chained byte-identity tests), proving par == seq. — DONE: all three sub-boxes landed (face2tri pass-1 went par in the M.2.4-era work); the par gate held through every pre-cut oracle run and survives as the M.6 par-lane goldens.
    - [x] M.4.3a - `calculate_vert_normals` per-vertex map — DONE (commit 49d35a5b). Phase-2 (angle-weighted normal per vertex) now runs through `par::map_collect`: an independent pure function of each vertex's one-ring + face normals, order-preserving (result index i = vertex i) so par == seq. Proven: full 137-oracle differential green with `--features par,oracle` (the byte-identity tests included), clippy clean both configs.
    - [x] M.4.3b - `SortGeometry` Morton computes — DONE (commit bca1a2c8). `sort_verts` per-vertex code + `sort_faces` per-face centroid code map through `par::map_collect` (pure per-element, removed face → kNoCode). 114/137 par+oracle green.
    - [x] M.4.3c - remaining maps: `duplicate_verts` (a disjoint-range SCATTER, not a map — needs restructure or a checked split to stay unsafe-free), `Face2Tri` pass-1 (per-face earclip — the main perf target, but tiny on the corpus + the earclip's BTreeSet cost-queue is fiddly). Deferred — perf-neutral on the current corpus, and the narrow phase (the real hotspot) already ships parallel (M.4.1).
  - [x] M.4.4 - K.D determinism gate, in-crate half (seq==par) — DONE (commit 6d8153cd). `boolean_pipeline_bytes_match_golden`: FNV-1a fingerprint of the full-pipeline output (positions + per-corner start-verts + property rows, all bitwise) over 5 cases (union/difference/intersection/3-cube-fold/colored-diff). Golden holds in serial AND `--features par` ⇒ **seq==par BIT-IDENTICAL proven** over the corpus. Same test = the native==wasm harness (also the M.7 golden-mode foundation).
    - [x] M.4.4 - wasm - native==wasm PROVEN (commit 4df26682). The blocker was the test HARNESS, not the code: proptest → rusty-fork → `wait-timeout` doesn't build for wasm, so proptest is now a `cfg(not(wasm32))` dev-dep + its 3 polygon fuzz blocks gated (native still runs them). `.cargo/config.toml` wires `wasm32-wasip1` runner = wasmtime. `cargo test --target wasm32-wasip1`: the golden fingerprint matches the native-generated golden BYTE-FOR-BYTE ⇒ native==wasm bit-identical, AND the whole suite is wasm-clean (112/112 + doctest). Turned out simpler than the example-refactor — the real #[test] runs directly under wasmtime. Every parallel-bound sort today ties-breaks by stable-sort's original-index preservation; an UNSTABLE parallel sort would diverge. Add an explicit index tiebreak so the order is total (a NO-OP on current output — stable-sort already breaks ties by index — verified by the full C++ differential staying byte-identical). Sites (from the [[m4-parallel-sweep-worklist]] audit): **CRITICAL** `sort.rs::sort_verts`/`sort_faces` (Morton-tie → the canonical order fed to chained booleans, so instability here breaks bit-identity); `collider.rs::build` (Morton-tie, set-invariant so hygiene-only); `polygon.rs` holes sort (descending pos.x, mirrors C++ `multiset<MaxX>`); `mesh.rs::set_normals_and_coplanar` (area²-descending, seeds the coplanar flood); `boolean_result.rs::pair_up` (already serial-stable, flagged). Gate: full 137-oracle differential unchanged (proves no-op on output) + a note that each is now safe to parallelize.
  - [x] M.4.1 - Narrow phase (PULLED FORWARD, commit b88a55e6) — `intersect12`/`winding03` parallelized through the `par::map_collect` seam. Both are MAPs not reduces → deterministic BY CONSTRUCTION (index-preserving map + the existing `stable_sort` on the unique `(edge,face)` key + integer winding sums; Rust's `F: Fn+Sync+Send` + zero unsafe makes a race a compile error). Factored `Collider::query_leaves` (per-query BVH traversal) as the unit map_collect drives; `collisions` is that in a serial loop. PROVEN: par-off + par-on both pass the full C++ differential at 1e-9 + genus (110 oracle each) + a `narrow_phase_is_run_to_run_deterministic` byte-identity test. PERF: big-twin narrow phase **15,000ms → 1,168ms on 16 cores (12.8×)**, now EXACT vs C++ (33230 tri). Named-struct strong-typing pass (Hit12/QueryHits/RepWinding). REMAINING M.4: the rest of the kernel's serial loops (Face2Tri, SimplifyTopology, SortGeometry, the collider BUILD) + total-order sort comparators + the full K.D wasm≡native gate.
- [x] M.5 - R5: 2D subsystem **COMPLETE** (K.6 green at M.5.4; the one carve-out — join-corner geometry — resolved by porting Clipper2's offset walk verbatim while everything else stays i_overlay + region-residual; 2D surface is panic-free with a typed-`Result` boundary per M.5.4.5). **DECISION (chotchki 2026-07-15, SPEC [OPEN #4] resolved): `i_overlay` + AREA-residual oracle** — NOT a Clipper2 port. The linked v3.5.1's 2D IS Clipper2 (verified: `cross_section.cpp` includes `clipper2/clipper.h`), but 2D determinism is by integer-coords EITHER way, so area-match (like minkowski's volume-residual) beats a ~10-15K bit-faithful port. This is the inflection where the bit-identity thesis RELAXES for the 2D layer only — the 3D core stays byte-exact. i_overlay 7.0.2 (union/difference/intersection/xor/self-intersect). Gate K.6: `cross_section_test` ported + 2D area-residual <1e-5 vs Clipper2-via-Manifold + offset area-by-area vs OpenSCAD (the 78.2548 canary).
  - [x] M.5.0 - i_overlay ROBUSTNESS SPIKE — **PASS, bet validated** (commit f9f09729, `tests/m5_0_i_overlay_spike.rs`). Every decision-critical path checked vs ANALYTIC truth: booleans under Positive-fill (union/intersect/difference/holed all exact), OFFSET/round-joins (the flagged risk — round=32+π dead-on, miter/bevel/inset correct; `Miter(Angle)` is a min-sharp-angle threshold not a ratio), bit-deterministic run-to-run (booleans + offset), robust on self-intersecting bowtie + near-degenerate sliver, AND builds+runs on wasm32-wasip1 (fab-scad web). Port-Clipper2 fallback stays CLOSED. → M.5.1.
  - [x] M.5.1 - `CrossSection` type + 2D booleans — DONE (commit 1aab33d0). `cross_section.rs`: Positive-fill polygon set, `from_polygons`/union/difference/intersection/area/bounds/is_empty/num_contour/num_vert/to_polygons over i_overlay's f64 API (i_overlay owns the f64↔integer-grid determinism seam). i_overlay → real dep. Gate `m5_1_cross_section_area_vs_cpp`: area-residual <1e-5 vs C++ Clipper2 CrossSection (bridge exposes it) across 5 cases × 3 ops. 123 default / 148 oracle / wasm-clean / clippy clean.
  - [x] M.5.2 - offset / round-joins — DONE (commit 29af551d). `JoinType{Square,Round,Miter}` + `CrossSection::offset(delta, join, miter_limit, circular_segments)` over i_overlay `outline`. Round area-matches Clipper2 (gated `m5_2_offset_round_area_vs_cpp` < 1e-3 — the OpenSCAD `offset(r)` path); Miter/Square best-effort (corner geometry differs, documented, NOT gated). The 78.2548 OpenSCAD canary → M.5.4.
  - [x] M.5.3 - 2D↔3D bridges — DONE, all four (commits bfc7bc6c/408e8801/6e12c3d5/af09d678), `bridge.rs`. **extrude** (CrossSection→Mesh, C++ Extrude straight-wall: caps via our 3D triangulator + wall quads), **revolve** (full-360° Revolve: axis-clip + per-profile-vertex ring, on-axis vertex reuse — the intricate index port), **project** (Mesh→CrossSection silhouette: all tri XY-projections in one Positive-fill pass, holes survive), **slice_at_z** (marching-triangles contour trace, BTreeSet-deterministic). ALL gated vs C++ (solid-divergence for the 2D→3D, area-residual for the 3D→2D) + wasm-clean. Partial revolve (front/back caps) is the one follow-on. **This UNBLOCKS M.3.8 at the kernel level** (the ops now exist; the fab-scad `Solid`/`CrossSection` wiring is the integration seam).
  - [x] M.5.4 - gate K.6 **GREEN** — all four parts + the hardening pass that verification forced. `cross_section_test` ported 1:1 (15/15, incl. EXACT vertex counts — RoundOffset n+4, BatchBoolean 66/42 match Clipper2's arrangement); the sweep upgraded area-residual → symmetric-difference REGION match (<1e-5, flat joins land ~1e-9) across ctors/transforms/hull/batch/decompose/fill-rules + offset 4-joins × ±δ × 4 shapes; the 78.2548 canary holds in BOTH engines; bridges solid-clean over 2D-op outputs. KEY MOVE: `offset` became a verbatim Clipper2-walk port (M.5.4.1) because join-corner geometry is engine-DEFINED — the one spot where area-parity forced algorithm-parity; M.5.2's "Miter/Square NOT gated" carve-out is CLOSED. Adversarial 5-lens verification (skeptics compiled C++ probes): 8 confirmed findings, all fixed — the delta-abs arm for zero-area contours, bounds-on-empty = the ALL-ENCOMPASSING rect (C++'s sorted sentinel quirk, ported), ReverseSolution orientation retention, subnormal unit_normal underflow (→ Err), and a false test-doc premise (C++ Identical is 1e-4 + sorted tris; ours strictly tighter). TRAPS BANKED: Clipper2's 2-arg CrossProduct is the NEGATED standard cross; Manifold's segments→arcTol→acos∘cos round-trip is a ULP razor edge (we pass n directly); the C++ CrossSection ingest quantizes on a BINARY grid (~1.5e-9 on non-dyadic coords — dyadic test coords keep 1e-9 gates honest).
    - [x] M.5.4.1 - M.5.4.1 - Verbatim Clipper2 offset port (offset.rs): OffsetPoint/DoSquare/DoMiter/DoBevel/DoRound walk in f64 + finishing Positive union; JoinType += Bevel; kills the i_overlay outline approximation. Canary 78.2548 + all-join-type area gates vs C++
    - [x] M.5.4.2 - M.5.4.2 - CrossSection API completion: Rect + square/circle (+get_circular_segments) + translate/rotate/scale/mirror/transform/warp + fill-rule ctors + hull (monotone chain) + batch_boolean/compose/decompose; bounds()→Rect
    - [x] M.5.4.3 - M.5.4.3 - Port cross_section_test.cpp (15 tests) → tests/m5_4_cross_section_test.rs, default lane (analytic + our-extrude Identical comparisons)
    - [x] M.5.4.4 - M.5.4.4 - K.6 oracle sweep: area-residual <1e-5 across the full new 2D surface + offset corpus (all joins, ± deltas) vs Clipper2-via-Manifold; 78.2548 canary; bridges solid-clean vs C++ where 3D results
    - [x] M.5.4.5 - M.5.4.5 - 2D no-panic hardening (chotchki): CrossSection finiteness INVARIANT (contours pub(crate) + accessor) + Result<_, status::Error> on every coordinate-ingesting op (ctors/transforms/warp/offset/hull_of_points; bridge project/slice) — NaN/±inf → Err(NonFiniteVertex), never a dep panic; subnormal-offset + degenerate-delta + CW-retention regressions
- [x] M.6 - R6 **PROVEN** (commit f9582ba7): 33 byte-goldens over the FULL op surface (mathf sweep incl. rem_pio2 triggers, booleans in every shape, transforms, split/trim, decompose, quickhull, minkowski, properties, the whole 2D subsystem + all four bridges, the par seam) hold bit-for-bit across native-serial, native-par, AND wasm32-wasip1 under wasmtime — the pillar-1 claim on the whole surface, not the K.D 5-case sample. Discipline audit closed (M.6.2: zero std-float escapes, ban list completed to the full transcendental family, HashMap sites order-safe). Threaded browser wasm rides the same construction (M.6.1: wasm-bindgen-rayon behind `par`, nightly compile gate scripted + green). The `golden` module is M.7's freeze vocabulary.
  - [x] M.6.1 - M.6.1 - Threaded wasm enablement (chotchki, pre-M.6): wasm-bindgen-rayon behind `par` on wasm32-unknown-unknown (Web-Worker + SharedArrayBuffer pool; COOP/COEP already kept at W.3.7.4), `init_thread_pool` re-exported; wasip1 test lane stays serial; build.rs `par_live` cfg alias; nightly +atomics/-Zbuild-std compile gate scripted + run
  - [x] M.6.2 - M.6.2 - Discipline audit: clippy ban list extended to the full transcendental family (exp2/log2/log10/ln_1p/exp_m1/hyperbolics); zero std-float calls outside mathf verified; both HashMap sites audited order-safe (lookup-only / test counter); no FMA anywhere
  - [x] M.6.3 - M.6.3 - The native==wasm FULL-SURFACE golden corpus: `golden` module (FNV-1a mesh/cross-section/f64 fingerprints — the M.7 golden-mode vocabulary) + tests/m6_native_wasm_golden.rs covering mathf sweep, booleans (folds/rotated/coincident), transforms, split/trim, decompose, hull, minkowski, properties, 2D ops + all four bridges — baked goldens must match bit-for-bit under wasmtime
- [x] M.7 - R.X: CUT C++ — freeze `oracle_goldens.json` (vol/area/genus/bbox/status) + own byte-exact `mesh_snapshots/`, flip to golden-mode, drop manifold3d / the `kernel` feature (SPEC [OPEN #5]). Suite green with C++ GONE = the finish line. — **DONE 2026-07-16** (commits 77a85201/f7f8432d/e179088d + the cut): fab-scad runs on fab-manifold end to end, manifold3d + oracle.rs deleted (−3.5k lines), every lane green with the C++ GONE. Flip fallout fixed pre-cut: 2D ingest snap-back (i_overlay grid noise was topologically live — drill_guide), BatchBoolean heap + face2tri tri fast path (the outlet O(n²) runaway), decompose/set_properties/ingest-pairing port gaps. Models differential: 14/57 diverged vs 17/54 on the pre-flip C++ baseline (every survivor but one pre-existing). **M.7.5 final numbers: median per-model 1.55× FASTER than OpenSCAD; wall-total 0.96× (dead even — the heavy-boolean tail is the known M.7.1 kernel perf gap, the next perf frontier); 74/109 both-rendered, unchanged from baseline.**
  - [x] M.7.1 - M.7.1 - Pre-cut perf comparison (chotchki, run POST-fuzzing): Rust kernel vs C++ via the oracle bridge — identical MeshGL inputs, lazy-C++ forced with num_tri(), medians over repeats; booleans (spheres/folds/nasty corpus incl. big twin), minkowski, hull; ours-serial AND ours-par vs TBB-parallel C++
  - [x] M.7.2 - M.7.2 - The FREEZE (pre-cut, C++ still linked): goldens/ = oracle_goldens.json (C++ volume/area/genus/bbox per corpus case × op, bit-recorded) + frozen corpus inputs (10 nasty OBJs + C++-generated sphere/cylinder as .bin MeshGLs) + our-output fingerprints; capture is an #[ignore]d oracle-feature test (idempotent bytes), golden-mode lane runs on DEFAULT features (native; wasm keeps the in-code M.6 corpus) — the cut then only deletes
  - [x] M.7.3 - M.7.3 - THE FLIP: fab-scad's kernel.rs onto fab-manifold (replace the manifold3d wrapper, keep the Solid/Section API) — inventory the surface, fill kernel gaps first (twist/scale extrude the known one), then validate with the FULL fab-scad suites + models differential sweep vs OpenSCAD (which survives the cut — it's an external binary)
    - [x] M.7.3.1 - M.7.3.1 - Coincident-ring union genus divergence (drill_guide class): two coaxial tubes attached end-to-end union to genus 0 (tunnel CLOSED) vs C++ genus 1 — same volume; also charger_v3 (2 vs 0), seltzer_fix (9 vs 5). Reduce vs C++ at kernel level, fix pre-cut
    - [x] M.7.3.2 - M.7.3.2 - face2tri runaway on uncut_supported_outlet: write_general_triangulation grows to 4GB+ (grow_one hot, trophy-#1 class) where C++ renders in seconds — reduce to minimal face, fix pre-cut
  - [x] M.7.4 - M.7.4 - THE CUT: drop manifold3d + the oracle feature from fab-manifold (golden-mode carries the correctness memory) and from fab-scad's deps; delete oracle.rs; suite green with the C++ GONE — the finish line
  - [x] M.7.5 - M.7.5 - REMEASURE post-flip: K.1.2 models-tree sweep again — fab-scad-on-fab-manifold vs OpenSCAD-on-C++-Manifold — the bet's final number

---

## 2026-07-17

## Phase O - O - Intrinsics tier (AST-fingerprint, wasm-safe)
- [x] O.1 - O.1 - Intrinsic registry LANDED: AST-fingerprint gate (exact-match-or-interpret) + Task::Intrinsic dispatch + fast==slow harness; POC proves the chain, corpus 901/901
- [x] O.2 - O.2 - First hand-written BOSL2-function intrinsics from the release profile
- [x] O.3 - O.3 v1 - EXPLAIN report LANDED (FAB_EXPLAIN): per-function intrinsic plan WIRED/DRIFT/interpreted, so you can see if an intrinsic fires or silently interprets (library drift). Runtime fire-counts + JIT path ride with P.1
- [x] O.4 - O.4 - Targeted deep-profile: per-user-fn inclusive TIME (task-stack-aware fnprofile) + FAB_PROFILE_TARGETS harness leg; profile the eval-bound tail (window_air_cover 36s, shoe_holder, webcam_holder, pill_holder) → the ranked worklist
- [x] O.5 - O.5 - Next intrinsics band from the O.4 worklist (hand-written, wasm-safe, fast==slow gated) — concrete sub-tasks cut from the profile data
  - [x] O.5.1 - O.5.1 - Wire-time const guard: Entry.consts (name, expected bits) checked against the fn's home scope at build_intrinsics — mismatch doesn't wire. Unblocks the eps=_EPSILON family (is_vector, approx, _tri_class, unit, posmod...)
  - [x] O.5.2 - O.5.2 - Predicate/shape band (~19s): is_vector, approx, is_consistent+_list_pattern+same_shape, is_matrix, is_path, in_list, force_list, num_defined, constrain, posmod — each verbatim-reference + fast==slow battery
  - [x] O.5.3 - O.5.3 - Earcut band (~17s, window_air_cover's core): _tri_class (12.4s/3.9M), _none_inside (4.8s/1.6M, recursive w/ early exit; deps select/_tri_class/_pt_in_tri)
  - [x] O.5.4 - O.5.4 - Aggregate/affine band (~12s): sum/_sum, _apply, unit, idx, _bt_search, vector_angle — recursive accumulators + matrix×points
  - [x] O.5.5 - O.5.5 - Re-measure + docs: the four models under models_profile_targets + K.1.2 sweep vs baseline; models-profile.md updated; deferred monsters (_region_region_intersections, _point_dist, _find_anchor, _group_sort_by_index, rot ~22s) named for the next cut
- [x] O.6 - O.6 - Named-arg → positional rebind at intrinsic dispatch: BOSL2's is_vector(v, zero=)/unit(v, error=) calls fall past the v1 all-positional gate (~1.2s interpreted in wac alone) — rebind by the callee's param names at dispatch_call, extending every existing intrinsic
- [x] O.7 - O.7 - Residual band 5 (medium bodies, ~7s): _find_anchor? _group_sort_by_index, _vnf_centroid, rot, _get_ear, vector_axis, affine3d_rot_from_to + small fry (in_list, is_path, constrain, apply) — OR route to P.1.6 JIT list ABI; the monsters (_region_region_intersections+_point_dist 14.2s) decide the JIT-vs-intrinsic split
- [x] O.8 - O.8 - Value-const guard: Entry.consts_v (name, fn()->Value) bit-compared against the home-scope binding at arm time — unlocks the non-numeric-constant tier (_NO_ARG, UP, RIGHT) for hand intrinsics (wasm gets every win, unlike the JIT)
- [x] O.9 - O.9 - The unlocked band: vector_axis, affine3d_rot_from_to (+v_theta/v_abs/point2d/affine3d_identity deps), then apply (determinant/det2-4 + vnf_reverse_faces + BOSL2-reverse chain), then rot (move/rot_inverse/affine3d_rot_by_axis + _NO_ARG) — cut per dep-tree, each with battery + wire check
- [x] O.10 - O.10 - The region-monster band: _region_region_intersections + its full reachable closure as hand intrinsics (the P.1.6 resolution — ~9.7s/6 calls on shoe_holder)

## Phase P - P - Cranelift JIT + CSG cache (desktop)
added 2026-07-16.
- [x] P.1 - P.1 - Cranelift JIT for the numeric long tail (desktop)
  - [x] P.1.1 - P.1.1 - JIT registry + compile cache (one JITModule, keyed by fingerprint)
  - [x] P.1.2 - P.1.2 - Crate-boundary hook + dispatch integration
  - [x] P.1.3 - P.1.3 - fast==JIT differential over the corpus + EXPLAIN coverage
  - [x] P.1.3a - JIT $-global hazard (reviewer find, pre-existing, NOT 2b): loader.rs tagged_globals doesn't filter $-assignments — a top-level `$fn=32;` + a JIT'd fn reading $fn inlines 32 and diverges from the interpreter under dynamic shadowing. Needs a fast==JIT probe + fix — DONE (6c06b8af): probe watched failing end-to-end, then two-layer fix (Ident-arm $-decline + tagged_globals filter); corpus coverage unchanged
  - [x] P.1.4 - P.1.4 - Extend the numeric subset (ternary, comparisons, transcendental calls)
  - [x] P.1.5 - P.1.5 - Measure + coverage report
    - [x] P.1.5.1 - P.1.5.1 - LTO experiment: fat LTO + codegen-units=1 vs the default release profile (chotchki's ask) — measured on the heavies + mid models, vs-OpenSCAD implication from baseline oracle times
    - [x] P.1.5.2 - P.1.5.2 - Interpreter Geo-tree nondeterminism (pill_holder flake): bistable fingerprint on the PURE-interpreter side, doctrine #36 violation — hunt with FAB_GEO_DUMP, root-cause, fix
  - [x] P.1.6 - P.1.6 - JIT list/vector ABI (scalarize A/B/C, sink-return D)
- [x] P.2 - P.2 - Content-addressed CSG cache — DONE cf1ff16a as the kernel-level Solid memo (the rung BU.7's measurement picked): per-build content-addressed memo in build_geo/build_geo_parts (ONE memo spans parts — sliced models share the base between parts), prepass-counted so only will-recur content is retained, deep-eq verified per hit (collision = re-render, never a wrong mesh), FAB_GEO_CACHE=0 opt-out + =verify diagnosis mode. THE HUNT: silverwear diverged 140 tris — ops never MINT ids so update_reference's Q-offset is one constant per build; served copies sharing an id-set collide in union trees and same_face merges ACROSS copies; fixed with fresh_instance-on-serve (Mesh::as_fresh_instance re-mints instance ids, classes preserved). All four heavy models bitwise-identical on/off. Sweep: slice_parts 8.0s→0.59s (−92%), bowtie −77%, garage −53%, desktop_holder TIMEOUT→solid; wall-total 124.0s vs OpenSCAD 250.0s = 2.02× FASTER (day-start: 0.96×), median 2.69×, 75/109 both-rendered; baseline re-frozen

## Phase Q - Fuzzing the evaluator + JIT (miri/Kani can't execute native code — fuzzing runs it, ASan checks it)
- [x] Q.1 - Q.1 - eval fuzz target: parse→eval→geometry→mesh under ASan (the interpreter miri-substitute)
- [x] Q.2 - Q.2 - jit_diff fuzz target: interp vs JIT bit-identity, executes the JIT unsafe seam under ASan
- [x] Q.3 - Q.3 - wire eval + jit_diff into the fuzz.yml nightly campaign (corpus persist + crash upload)
- [x] Q.4 - Q.4 - overnight campaign run + triage; any crash → minimize + TROPHIES.md
- [x] Q.5 - Q.5 - global eval iteration/time budget (untrusted-input DoS hardening; a single 10M-element comprehension is bounded but 10s)
- [x] Q.6 - Q.6 - fix JIT/interp NaN divergence: resolved as NaN-CLASS convention (fab_lang::tier_eq). Real cause = Cranelift folding (-s)*(-s)→s*s, not fmul canonicalization; NaN payload unobservable + ISA-nondeterministic so waived. Doctrine #36 refined in SPEC.md.
- [x] Q.7 - Q.7 - JIT compile-complexity budget (fab-jit): bound the lowering's IR growth so compile_function declines a pathological body cheaply instead of OOMing

## Phase T - T - Slice/plate pipeline: multi-part models + print-orientation
- [x] T.1 - T.1 - BUG (dogfood): sliced plate pieces land ~45° from the bed in the print-orientation view instead of lying flat. Hypothesis: auto-orient/plate-placement using the wrong up-vector (slice-plane frame leaking into the bed frame)
- [x] T.2 - T.2 - treat separate TOP-LEVEL items as DISTINCT slice/place targets (partition the root union's children into independent parts, each sliced + oriented + packed on its own) — solves legacy presliced parts. The big one.
- [x] T.2a - T.2a - CC print-pipeline fix (kernel connected-components + per-component best_up); subsumes T.1
- [x] T.2b - T.2b - structural parts (build_geo_parts) + egui multi-part tabbed UI; co-pack shared plates
  - [x] T.2b.1 - T.2b.1 - lib keystone: build_geo_parts (split root Union into N part Solids) + per-part fab.rs pipeline
  - [x] T.2b.2 - T.2b.2 - GUI state model → per-part: Parts vec + ActivePart, part_id on entities, slice_hash/poll/sync
  - [x] T.2b.3 - T.2b.3 - multi-part tabbed UI (part switcher + per-part editing) — the design work
  - [x] T.2b.4 - T.2b.4 - co-pack all parts onto shared plates + full verify (headless screenshot/script + tests)
- [x] T.3 - T.3 - best_up prefer-flat policy: stop tilting structured pieces to 45° over a stable flat face

---

## 2026-07-17

## Phase I - scad-rs: evaluator core
  Meta - Cranelift is the NATIVE JIT rung (chotchki's find: VERY approachable, and it's determinism-friendly — no auto-FMA, transcendentals stay CALLS to our own math, so the fixed-accumulation doctrine survives). NOT a replacement for the interpreter: the wasm/browser target can't JIT in-sandbox (the bet's #1 differentiator needs ONE implementation everywhere), and the interpreter is the bit-identical baseline the JIT validates against (fast==slow extends to fast==JIT). Spiked at I.8 (one hot function, prove bit-identical); the JIT-vs-intrinsics PROMOTE decision lands at Phase L with data.
added 2026-07-06.
- [x] I.1 - Value model full: enum + NumList fast path + interned strings + lazy ranges; fast==slow BITWISE property via the shared fixed 4-lane accumulation order
  - [x] I.1.1 - Heterogeneous List(Rc<[Value]>) alongside the NumList fast path: nested lists, indexing, eq/order per Value.cc
  - [x] I.1.2 - Lazy Range value (start/step/end): inclusive-end iteration, element cap + warning, range-as-value
  - [x] I.1.3 - Function values / closures (params + body + captured env) — the currency I.2's calls spend
  - [x] I.1.4 - Interned strings (deterministic intern table) + string indexing / char access
  - [x] I.1.5 - Fixed 4-lane accumulation order + the fast==slow BITWISE proptest (NumList fast path == List slow path)
- [x] I.2 - Scoping engine: lexical envs, dynamic $-variables, children()/late binding, module+function call machinery on the explicit stack; + the use/include LOADER (file resolution + include-splice + use-import — parser stays zero-IO, this is where H's use/include AST nodes get resolved)
  - [x] I.2.1 - Lexical env chain (vars) + frame repr — DECISION: Rc<Frame> chain (correctness-first, single-threaded, the browser can't thread anyway; closures capture ONE Rc clone; $-scoping walks the chain). The frame-arena is a profiled I.6 opt, not now. PARALLELISM (captured 2026-07-04): it's not tree-vs-stack, it's a TREE OF STACK-MACHINES — fork independent units, each a sequential deterministic stack machine, join in FIXED order. Task-parallelism lives in the geometry DAG (6.1) + a parallel-comprehension MAP driver (fan iterations, assemble BY INDEX), NOT a rebuilt evaluator. Rc→Arc is the 3rd axis: parallel comprehensions need Send values+env (Arc taxes the sequential fast path for a benefit only they collect) — defer to a profiled I.6/intrinsics call; the swap is mechanical but crate-wide, internal to Value. Any parallelism MUST preserve a fixed reduction order (the 4-lane accumulation IS that) + buffered echo/warning order (else I.5's string-equal-vs-oracle breaks).
  - [x] I.2.2 - Dynamic $-variables: down-the-call-tree propagation + per-call override + the reaching-$-context
  - [x] I.2.3 - Function-call machinery ON THE EXPLICIT STACK: resolve + arg-match (positional/named/default) + body eval + return, no host recursion
    - [x] I.2.3.1 - Per-task scope + eval-context (function store) plumbing — Task carries its Scope so a call's body evals in the callee's scope while the caller's continuation waits; thread a Ctx (name→&'prog FunctionDef) through eval. Refactor only, all tests stay green.
    - [x] I.2.3.2 - User function calls on the explicit stack: resolve name→FunctionDef, arg-match (positional/named/default), push body eval in the call frame, return the value — no host recursion. The corner_brace-class deep-recursion (f(n)=f(n-1), 100k deep) proof lands here.
    - [x] I.2.3.3 - Function-literal VALUES / closures: Value::Function (params + body + captured Rc<Frame> env), function(x)body evaluates to it, calling a function value reuses I.2.3.2's machinery. Folds in I.1.3 (#70).
  - [x] I.2.4 - Module-call machinery on the explicit stack: resolve user module + arg-bind + children eval → geometry tree
    - [x] I.2.4.1 - Loader: collect module defs (ModStore) through use/include, like functions
    - [x] I.2.4.2 - Ctx.modules + thread global through the statement side (module bodies = global.child + params, OpenSCAD hygiene)
    - [x] I.2.4.3 - Module-call arm: resolve user module + arg-bind (positional/named/default/$-args) + depth-guarded body eval → GeoNode
  - [x] I.2.5 - children() / $children late binding (refers to the call-site children, late-bound)
  - [x] I.2.6 - use/include LOADER: path resolution + include-splice + use-import (resolves H's zero-IO AST nodes; parser stays zero-IO)
  - [x] I.2.7 - Whole-scope variable binding — hoist top-level assignments, last-assignment-wins (OpenSCAD), not sequential
  - [x] I.2.8 - Differential vs the OpenSCAD oracle: use/include file-based cases (two-driver harness landed 04b8f1d)
- [x] I.3 - Control flow + comprehensions + recursion bounded by memory — corner_brace-class deep recursion as the standing regression proof
  - [x] I.3.1 - let-expression `let(a=1,b=2) body` (ExprKind::Let): bind args left-to-right in a child scope, evaluate body there. Pure expression — deferred here from I.2.3.3. Reused by the comprehension `let`.
  - [x] I.3.2 - List comprehensions on the explicit stack: LcFor (iterate range/list), LcForC (C-style), LcEach (splice), LcIf/else (filter), lc-let — produce a List, nesting arbitrarily. Uses the I.1.2 range iterator; the element cap + warning ride here.
  - [x] I.3.3 - STATEMENT control flow (if/for producing geometry → the CSG tree) — GEOMETRY-COUPLED, deferred to sit with Phase J (needs transforms/booleans/multi-child union). The expression-level halves (I.3.1/I.3.2) land now; this is the statement half.
- [x] I.4 - Builtin function library (~80: math/list/string/type predicates), each landing with its semantics/ test
  - [x] I.4.1 - Math builtins: abs/sign, sin/cos/tan/asin/acos/atan/atan2 (DEGREES, reuse trig.rs), floor/ceil/round, ln/log/exp/pow/sqrt, min/max, norm/cross. Bug-for-bug func.cc. (rands is non-deterministic → deferred separately.)
  - [x] I.4.2 - List + string builtins: len, concat, str, chr, ord, lookup, search, reverse — the glue BOSL2 lives on.
  - [x] I.4.3 - Type-predicate builtins: is_undef, is_bool, is_num, is_string, is_list, is_function — + version/version_num. rands as a SEEDED deterministic builtin (or a loud defer if the seed threading isn't ready).
- [x] I.5 - undef propagation + warning/echo text bug-for-bug (string-equal vs oracle)
- [x] I.6 - tracing spans on the call path + aggregating benchmark layer; release builds compile it out; overhead measured
- [x] I.7 - Kani proofs: stack-machine push/pop discipline, range-iteration termination

- [x] I.8 - Cranelift JIT spike: after the interpreter core, JIT one hot numeric function, measure speedup vs interpreter, PROVE bit-identical (fast==JIT); bank the float-discipline recipe — de-risks the L JIT-vs-intrinsics decision
  - [x] I.8.1 - fab-jit crate scaffold: cranelift-jit deps, native-only, the single documented unsafe seam (fn-ptr call)
  - [x] I.8.2 - Expr → Cranelift IR compiler for the numeric subset, fixed left-to-right order matching the interpreter
  - [x] I.8.3 - Ops Cranelift lacks → external CALLS to our Rust math (% → a%b, ^ → a.powf(b)) — the determinism recipe
  - [x] I.8.4 - fast==JIT BITWISE differential (corpus + coeff-proptest) + the speedup benchmark
  - [x] I.8.5 - Bank the float-discipline recipe (doc) — feeds the Phase-L JIT-vs-intrinsics promote decision
- [x] I.9 - fixing BOSL2 — evaluator bring-up (parse ✓ 56/56; short-circuit ✓; burn down the eval divergences)
  - [x] I.9.1 - Member access .x/.y/.z on vectors (ExprKind::Member) — deferred at I.1, now the next BOSL2 eval blocker
  - [x] I.9.2 - BOSL2 cyl → "Invalid transformation matrix" — a matrix helper (down/skew/up/multmatrix chain) diverges
  - [x] I.9.3 - BOSL2 cuboid → "Input to sum is non-numeric or inconsistent" — a list-build feeds sum() a non-numeric
  - [x] I.9.4 - BOSL2 sphere → "Bad arguments" — an arg-normalization assert fires (spherical primitive / attachable)
  - [x] I.9.5 - BOSL2 sphere/cyl/cuboid → "user-module recursion too deep" — unbounded recursion on the attachable path
  - [x] I.9.6 - BOSL2 attachable → `let(...) children()` used as a STATEMENT (module-form let)

## Phase J - scad-rs: geometry surface + cache
added 2026-07-05.
- [x] J.1 - Geometry backend trait; interface suite runs miri-on-mock AND ASAN-on-real-Manifold in CI (the split that replaced raw miri-on-FFI)
  - [x] J.1.1 - GeometryBackend trait + MockBackend + ManifoldBackend + the generic interface suite (both green under cargo test)
  - [x] J.1.2 - Run the interface suite under miri (mock) + ASAN (real Manifold) in CI — the split that replaces miri-on-FFI
- [x] J.2 - 3D: primitives, multmatrix, booleans through Manifold; polyhedron with oracle-matching validation semantics
  - [x] J.2.1 - GeoNode CSG tree + evaluator produces it: primitives→Leaf, transforms→Transform, implicit top-level Union
  - [x] J.2.2 - Boolean modules union/difference/intersection → the boolean GeoNodes over children
  - [x] J.2.3 - fab-scad tree-walker: GeoNode → Solid via GeometryBackend; rewire the FabLang differential driver through it
  - [x] J.2.6 - polyhedron() primitive + oracle-matching validation semantics
    - [x] J.2.6.1 - polyhedron(points,faces,convexity) → Mesh Leaf in fab-lang: raw verts + fan-triangulated n-gon faces (OpenSCAD tessellation), no backend needed
    - [x] J.2.6.2 - polyhedron validation bug-for-bug: out-of-range face index / <3-vertex face / non-manifold → OpenSCAD warn-and-render vs error
    - [x] J.2.6.3 - Differential: spheroid + a VNF shape vs oracle (boolean-residual / vertex-multiset)
  - [x] J.2.7 - Differential: CSG programs (transforms/booleans/multi-object/polyhedron) vs the oracle via boolean-residual
    - [x] J.2.7.1 - Harness: oracle-side re-import uses f32 MeshGL → boolean-result meshes fail; blocks the boolean differential
  - [x] J.2.8 - color() module → GeoNode::Color + Rgba vocab + CSS named-color table (BOSL2-critical)
  - [x] J.2.9 - Color propagation through Manifold (vertex props survive booleans) + oracle capture + differential
- [x] J.3 - 2D subsystem on Clipper2: square/circle/polygon/offset/projection + linear/rotate_extrude bridging 2D→3D with tessellation parity
  - Comment: Is clipper2 the right library for this? could manifold do it?
  - [x] J.3.1 - DECISION + 2D backend seam: Manifold CrossSection for all 2D/hull/extrude/projection (zero new geometry deps — bundles Clipper2, the lib OpenSCAD 2021+ uses). GeoNode↔CrossSection; note in SPEC
  - [x] J.3.2 - 2D primitives square/circle/polygon → Shape2D node; circle uses our $fn fragment math for parity
    - [x] J.3.2.1 - J.3.2.1 - eval-wire: recognize 2D primitives + thread Geo{D2,D3} through the geometry pass
  - [x] J.3.3 - 2D booleans + offset over 2D children (CrossSection ops)
  - [x] J.3.4 - linear_extrude (height/twist/scale/slices) → 3D; tessellation parity MEASURED vs oracle (Manifold's if the metric tolerates, else our loft)
    - [x] J.3.4.1 - J.3.4.1 - twisted linear_extrude loft: match OpenSCAD's profile-resampling + slice interpolation
  - [x] J.3.5 - rotate_extrude (angle, $fn) → 3D; reuse the ring/segment math
  - [x] J.3.6 - projection(cut) 3D→2D via slice_to_cross_section
  - [x] J.3.7 - Differential: path/region-derived BOSL2 2D shapes vs oracle
- [x] J.4 - hull; import() via our STL/3MF readers; text/minkowski/surface = LOUD deferred stubs (blow up, complain, never silently wrong)
  - Comment: Text could be handled by https://github.com/pop-os/cosmic-text . I'm still researching minkowski.
  - [x] J.4.1 - hull() → Manifold hull/batch_hull over children (2D + 3D); unblocks cuboid chamfer/rounding + masks
  - [x] J.4.2 - import() via our STL/3MF readers (threemf/zip/quick-xml deps already present)
    - [>] J.4.2.1 - J.4.2.1 - import() eval + backend wiring (STL/3MF readers → Leaf)
    - [>] J.4.2.2 - J.4.2.2 - import() differential vs oracle (round-trip a known STL + 3MF)
  - [x] J.4.3 - text() LANDED via rustybuzz (shaping, the pure-Rust harfbuzz port — matches OpenSCAD's harfbuzz) + ttf-parser (glyph OUTLINES) over a BUNDLED Liberation Sans (OpenSCAD's default, SIL OFL, pinned at src/eval/fonts/). NOT cosmic-text — that rasterizes to pixels + does system-font lookup (fontconfig = non-deterministic, banned); we need vector contours from a pinned face. Pipeline: shape → per-glyph outline → $fn-flatten Béziers → placed/scaled contours → Shape2D::Polygon (even-odd fill, so glyph HOLES resolve for free). halign/valign/spacing/direction/script/language honored; `font=` accepts but ships one face (system fonts = a later opt-in). Deterministic (pure Rust + pinned font) + oracle-matchable (same glyphs as OpenSCAD → volume-residual). Validated: 'O' fills as a RING not a box; multi-glyph advance; empty→empty. Used across the models/ tree (part numbers, version stamps, labels) → unblocks L.3.
  - [x] J.4.4 - minkowski() LANDED via Manifold's NATIVE `minkowski_sum` (manifold3d 0.3.3 clean drop-in — same manifold-csg lineage, no migration; wraps Manifold C++ PR #666's tiered hull+union). `GeoNode::Minkowski` folds the binary sum with the empty-ANNIHILATOR rule (A⊕∅=∅); 2D LOUD-deferred to Clipper2 like 2D hull. Validated: box⊕box=summed box (1728 exact, oracle-free) + volume-residual for the rounding case; test_cyl clears → corpus 99.1%, 0 assertion / 0 unimplemented. Research + design writeup: docs/minkowski-design.md. (surface() stays a LOUD-deferred stub.)
  - [x] J.4.5 - DETERMINISM: native geometry runs Manifold with TBB (`parallel` feature ON) = non-deterministic parallel reduction; wasm is single-threaded. Doctrine #36 needs bit-identical output cross-platform — build native with `parallel` OFF (`MANIFOLD_PAR=NONE`, matching wasm) + re-baseline, OR prove TBB reduction is deterministic. Surfaced by the minkowski research (manifold#666 CI: non-convex² broke Mac/Windows on non-CCW triangulation even with `deterministic=true`). Affects ALL geometry, not just minkowski. — RESOLVED: the manifold-rs rewrite runs rayon, not C++ TBB, so this cross-platform TBB concern is moot; run-to-run same-platform determinism is tracked separately in S.4.
- [x] J.5 - Content-addressed CSG cache: node hash = subtree + resolved params + reaching $-context; in-memory tier + hit-rate counters (the on-disk tier stays a storage decision)

  - [x] J.5.1 - Module-redundancy probe: measure the CSG cache-hit ceiling
  - [x] J.5.2 - Module memo rung 2a: naive full-$-context (body, params, all-$ctx) → Geo, ~42% safe
  - [>] J.5.2b - Module memo rung 2b: read-set-precise $-context (key only $-vars each module reads), chase 42%→~99%
  - [x] J.5.3 - Correctness gate: cache-on==off differential + exclusion validation tests
- [x] J.6 - Unify fab-scad's geom::V3 ([f64;3] orientation helpers) + printer-domain [f64;3] into fab_lang::Vec3

---

## 2026-07-17

## Phase BU - Big-boolean kernel perf: close the heavy-mesh gap vs C++ (the M.7.1 tail — wall-total 0.96×)
deferred from J.5.2b on 2026-07-10.
- [x] BU.1 - Re-baseline the kernel gap: M.7.1 harness in a pre-cut worktree (e179088d) — fresh Rust(serial+par) vs C++(TBB) per-op ratios incl. million-tri booleans — DONE: PAR big_twin 0.52× / self_intersect 0.28× / minkowski-t1 0.15× (par LOSES 3× to serial on small cases) / hull WINS 1.73×
- [x] BU.2 - Profile the worst cases on the current tree (samply, release+debuginfo): wall time attributed per boolean pipeline stage (collider broad-phase, intersections/winding, result assembly, face2tri, simplify) + hot-function report — DONE: big_twin = 99.8% intersect12 (shadow01 ~50% self, runtime-bool cascade); self_intersect = 72% output-mesh rebuild (simplify 44%: dedupe SipHash-per-ring ~11% + Vec churn ~12% + per-op collider rebuild ~9%)
- [x] BU.3 - Parallel-coverage + algorithm map: every C++ TBB/par_for site in boolean3/boolean_result/collider vs our par:: seam; algorithmic deltas in the hot stages — DONE: full site table + 6 ranked gaps (1 threshold ✓, 2 simplify detection, 3 collider cache-on-Mesh+par, 4 assembly bulk passes, 5 face2tri pass2/stitch, 6 sort tails); sphere128 answered: only ~35-40% of wall behind par:: → Amdahl caps at 1.6×
- [x] BU.4 - Fix the top bottlenecks profile-first, byte-golden-gated: M.6 cross-lane + m7 golden-mode stay bit-identical, determinism doctrine holds (serial == par == wasm)
  - [x] BU.4.1 - Monomorphize the narrow-phase cascade: const-generic EXPAND_P/FORWARD through shadow01/kernel02/kernel11/kernel12 + inline, C++ template parity (the boolean3.rs header's own IOU)
  - [x] BU.4.2 - Unchecked hot loads in the cascade + collider leaf traversal (C++ VecView parity): LANDED as chotchki's VALIDATED-VIEW design (forbid→deny accepted): `MeshView::validate` proves the tables closed in one O(halfedges) pass at Boolean3 entry (violating mesh PANICS — release too, regression-tested), then the cascade + collider traversal read unchecked through typed ids only; 10 item-scoped allows, each debug_assert-guarded w/ SAFETY. big_twin serial 9.1s→7.26s, par 723→596ms = BEATS C++ TBB 628ms; goldens bit-identical, 149/149
  - [x] BU.4.3 - par:: sequential threshold (C++ kSeqThreshold=1e4 parity) in map_collect/for_each/reduce — kills the small-case par regressions (Havocglass 0.41→1.24ms class); bit-identical by construction, goldens gate anyway
  - [x] BU.4.4 - Minkowski par regression (11.2→33.2ms) — DONE in two halves: BU.4.3's threshold killed the regression; 5876ccef restored the COARSE grain (par per-face hulls via map_collect_min_len(100) = C++'s autoPolicy value; union_all → FIXED-SHAPE pairwise tree, deterministic vs C++'s timing-dependent BatchBoolean queue; M.3.9 unblocked the pairing, re-tested no-hang). t1: serial 13.7→7.5ms, par 4.7ms vs C++ TBB 5.1 — WE WIN. One documented m6 regen (t1 triangulation; volume bits identical)
  - [x] BU.4.5 - self_intersect class — DONE (c9e13dad + 3ab40218): EndVertMin ports C++'s endVerts linear-scan buffer (SipHash-per-ring dead), all ring walks fold IN PLACE (grow_one churn dead), colliders cached on Mesh eager-at-sort_geometry (C++ parity; lazy REJECTED on soundness — pub fields make invalidation unenumerable). self_intersect serial 19.8→14.8 / par 13.0→11.7; sphere128 serial 8.1→6.3 / par 8.4→6.1; big_twin par 589ms — BEATS C++ 628. Known: single-op small cases +0.18ms (unfused output-collider build → BU.4.7)
  - [x] BU.4.6 - Close the ranked par-coverage gaps from the C++-vs-ours map — DONE (6 clusters, commits 174f5f42..9d1dc05a, all byte-golden-gated both lanes): self_intersect par 16.4→12.98ms (edge_op parallel-detect/serial-apply −20%, face_op per-face chunks −3.6%, collider radix build); sort/mesh/boolean_result corpus-neutral BY MEASUREMENT (gates at C++'s 1e5 parity — the 1e4 gate measured NEGATIVE and is documented at each const; wins fire >100k halfedges, byte-equality unit-tested there); AtomicAdd race replaced w/ deterministic gather/emit; 3 measured rejections documented in-code (winding03 split DSU-bound, internal-boxes level-sweep, mesh-tail collect shapes 2-4× loss at corpus scale). sphere128 answered: its domains sit below the C++-parity gates — C++ runs them serial too; its residual gap is serial-quality (BU.4.5), not coverage
  - [x] BU.4.7 - Fuse the output collider build with sort_faces (Collider::from_sorted_leaves reusing faceBox/faceMorton, C++ GetFaceBoxMorton parity) — reclaims ~30-40% of each per-result build, the fix for big_twin serial's +0.3% and the residual sphere128 tenths
- [x] BU.5 - Re-measure — DONE 2026-07-16: kernel table (ours-par vs C++-TBB): big_twin 1.07× + minkowski-t1 1.09× WE WIN, sphere64 0.99×, sphere128− 0.77×, self_intersect 0.40× (was 0.28), Havocglass 0.51× (was 0.25), hull 1.73×. Models sweep: wall-total 1.12× FASTER (exit ≥1× MET; was 0.96×), median 1.74× (was 1.55×), silverwear_5part flipped both-TIMEOUT → fab 28.1s/oracle TIMEOUT, wall_screen flipped fab-slower → fab-faster (22.1 vs 27.0s). Outlet still >budget BOTH engines — the BU.7 cache rung's case, not a kernel gap
- [x] BU.6 - PERSISTENT perf harness — DONE: models sweep writes perf/runs/<epoch>.json + delta-reports vs committed perf/baseline.json (FROZEN 2026-07-16 post-BU.4, the trend-line anchor); boolean_perf driver (obj+bin, stage trace, samply-able) landed eae1dbd6
- [x] BU.7 - Cache leverage on the big-mesh tail — MEASURED (probe 9cb4456d, FAB_GEO_REDUNDANCY=1) and the rung is PICKED: (a)+(b) shipped as BU.8 (realized 40-98% eval-side hits); (c) P.2 kernel-level per-GeoNode Solid memo is the BUILD — the eval cache AMPLIFIES kernel-tree duplication (hits splice deep Geo clones): slice_parts 95% of build re-renders known content (53,382 nodes / 367 distinct), garage_door 65%, pill_holder 59%, silverwear 56%; (d) optimizer residual ~EMPTY (distinct counts tiny — content-addressing covers it, skip the rung). NON-cache tails named: the OUTLET completed for the first time (38.1s, was TIMEOUT; 0.6% redundancy = raw kernel throughput → BU.4.7 + narrow-phase work); window_air_cover/shoe_holder/webcam_holder are EVAL-bound (build 0.2-0.6s of 11-38s walls) → the O/P intrinsics/JIT tier
- [x] BU.8 - Module memo rung 2b: read-set-precise $-context — DONE 90f65fd6 (design docs/mod-cache-rung2b-design.md): capture stack on lookup_opt's walk (gen/kill/hit-merge = chotchki's leaves-up invariant at runtime). ADVERSARIAL REVIEW caught a confirmed wrong-hit pre-landing (Rc::make_mut COW replaces a shared capture-entry frame → empty read sets → vacuous hits; suite was blind, 7 killer programs now in the A/B) — fixed w/ COW-surviving Frame.boundary ids + child()-not-clone at hoist/for/$-args + panic-safe guards. Post-fix: slice_parts 21.1→7.5s (−64%, 71% hits, outer-module hits short-circuit subtrees), STLs BITWISE identical on/off ×3 models, all gates green cache-on. KILLED N.2c.3 (per-level specials walk WAS the pathology; recursion suite 0.05s on == off) → csg cache DEFAULTS ON (56bf9a64, chotchki's call; FAB_CSG_CACHE=0 opts out; eval-cache half of N.2c.2.3 stays open). Sweep at default: wall-total 1.42× FASTER than OpenSCAD (was 1.12×), median 2.65× (was 1.74×); kirby_holder + nail_cure flipped both-TIMEOUT → fab-renders; baseline re-frozen at the new default

## Phase N - N - Interpreter fast-paths (our builtins)
- [x] N.1 - N.1 - Re-profile a slow model on RELEASE with a sampling profiler
- [x] N.2 - N.2 - Cut eval allocation (profile-driven; builtin dispatch was <1%)
- [x] N.2a - N.2a - Cheap allocation wins: assert-formatting freebie + eval_with_global per-call allocs
- [x] N.2b - N.2b - Intern var/$-names as Rc<str> in the AST LANDED: Parameter/Assignment/Arg.name → Rc<str>, bind clones a refcount not a String; slice_parts eval 8517→8210ms (~3.6%; cum N.2d+N.2b ~8%), corpus 901/901
- [x] N.2c - N.2c - Eval-memo cache (the 82-92% lever) — reviewed design, ready to build
  - [x] N.2c.1 - N.2c step 1 — DynCtx: O(1) per-frame $-context identity in Scope
  - [x] N.2c.2 - N.2c.2 - Program-level auto-off: make the eval cache safe to default ON
    - [x] N.2c.2.1 - N.2c.2.1 - baseline: reproduce the release cache-on/off split (under_sink_guide ~-17%, pill_holder/corner_brace +win) via fab render --engine scad-rs, FAB_EVAL_CACHE 0 vs 1
    - [x] N.2c.2.2 - N.2c.2.2 - implement bounded-warmup program-level auto-off: measure key-cost vs hit-benefit over a fixed warmup window, one-time disable flag for net-negative programs (per-call cost → single branch once disabled)
    - [x] N.2c.2.3 - N.2c.2.3 - flip eval_cache/csg default ON — CSG HALF DONE (56bf9a64, BU.8 unblocked it; full suite + gauntlet + bitwise STL diffs were the gate per this task's own terms). REMAINING: the eval_cache half — model-dependent win profile (under_sink_guide −17% in N.2c.2.1), auto-off unrevalidated at default-on
  - [x] N.2c.3 - N.2c.3 - csg-cache + deep-recursion pathology — RESOLVED as a BU.8 side effect: rung 2b's params-only key deleted the per-level specials() walk + ~42-var hash that WAS the cost; module_recursion_bound cache-on 0.05s == off 0.06s. No guard-check needed
- [x] N.2d - N.2d - Vec-frame Scope LANDED: adaptive VarMap (Vec small / BTreeMap-spill for island globals); slice_parts eval -4.6% (8925→8517ms), corpus 901/901 (cleared spheroid+gaussian_rands); residual per-bind String-key alloc → N.2b
- [x] N.2e - N.2e - NumList COW buffer reuse LANDED (ceiling-verified): zip_reuse/map_reuse recycle a refcount-1 Rc<[f64]>; ~0% slice_parts (falsified the theory — its alloc is comprehension result-lists) but ~11% on vector-arithmetic-heavy; bit-identical, corpus 901/901

---

## 2026-07-19

## Phase X - Live customizer + content-addressed CSG cache
- [x] X.1 - X.1 - Persistent cross-render CSG cache (CONDITIONAL — build only if X.2.5 shows the live loop lags on a heavy model). GeoMemo (src/backend.rs) already dedupes duplicated items WITHIN a render default-on; net-new = lift its key(geo_hash)+serve/store into a persistent LRU on SolidStore so unchanged branches survive across slider ticks. Keep the fresh_instance provenance re-mint; owned-GeoNode-clone-or-128bit-hash for the cross-build verify.
  - [x] X.1.1 - X.1.1 - Cache key: op-tree content-hash carried AS geometry lowers (f(op, child_keys, transform, $fn/$fa/$fs)); NOT mesh-byte hashing (the eval_cache gate-overhead trap)
  - [x] X.1.2 - X.1.2 - Cache store worker-side: native GeomPool thread + wasm Web Worker (persist across render requests), LRU by total mesh bytes, wasm ~1GB-ceiling aware
  - [x] X.1.3 - X.1.3 - Correctness gate: byte-golden cache-on == cache-off (par==serial makes content-addressing sound); a deliberate stale-key/wrong-geometry test
  - [x] X.1.4 - X.1.4 - Validate on a RENDER-PATH warm-cache benchmark (render heavy parametric model, change 1 param, measure the recompute) — NOT slice_parts (that's a different path: auto_slice cut-booleans, GeoMemo never sees it; filed to backlog)
- [x] X.2 - X.2 - Customizer wire-up: the existing lang/src/customizer.rs → egui widgets + conditional tab — native + web
  - [x] X.2.1 - X.2.1 - Conditional Customize tab between Model and Parts (Tab enum + left-panel branch; appears only when customize(source) yields >=1 param)
  - [x] X.2.2 - X.2.2 - Widget mapping: CustomParam.constraint -> egui (Range=slider, Dropdown=combo, bool=checkbox, Num/Str=DragValue/text, vector=fields); group by /* [Group] */ into collapsing sections
  - [x] X.2.3 - X.2.3 - Source-splice re-render: replace_range(editor.text, value_span) -> set edited_at -> existing debounced preview_edited_buffer (native temp-file + wasm bytes paths both inherited); faithful value->source formatting (no float noise)
  - [x] X.2.4 - X.2.4 - Per-param reset-to-default (remember first-parse default) + reactive polish (loading pulse, no Apply button) per gui-reactive-standard
  - [x] X.2.5 - X.2.5 - Measure the live loop — SUPERSEDED by X.1.4: chotchki chose to build X.1 unconditionally ("push on with X.1"), and the X.1.4 kernel bench measured the cross-render win directly (cold 70ms → warm 15.2ms, 4.6× on a one-param-moved render). An end-to-end in-GUI slider-drag timing remains a nice-to-have but its gating decision is moot.
- [x] X.3 - X.3 - Persistence + native/web parity: customized values ride in the source (buffer saves free); verify round-trip through native Save + web save-back (PUT /variants); explicit native+web parity pass
- [x] X.4 - X.4 - 2D hull() so real models render (B): wire hull() over 2D children — the gap that blocks parametric_trinket_shelf (chotchki's wife's part)
  - [x] X.4.1 - X.4.1 - Recon: where "hull() over 2D children is not yet wired" originates (the eval→2D-backend seam), the Shape2D representation, and CRUCIALLY whether fab-manifold's CrossSection already exposes a hull op (decides small=wire-it vs medium=hand-roll a monotone-chain 2D convex hull over the union of children's contour points)
  - [x] X.4.2 - X.4.2 - Implement the 2D hull op: GeometryBackend 2D-hull method (CrossSection::hull if it exists, else Andrew's monotone-chain over the children's contour vertices) + a Shape2D::Hull node + the fab-lang lowering for hull() with 2D children; deterministic (total-order the hull points)
  - [x] X.4.3 - X.4.3 - Validate: parametric_trinket_shelf renders end-to-end (the wife's model, in the Customize tab), a 2D-hull unit test (square + offset circle → known hull), native + wasm compile + fmt/tests green

---

## 2026-07-22

## Phase Z - Multi-file SCAD projects on the web (the .scadproj zip container)
- [x] Z.1 - Z.1 - The .scadproj container: stored-zip schema (mimetype first-entry + fab-project.json manifest + entry-point resolution + path sanitize); reader/writer in fab-scad; native fab pack / fab open
- [x] Z.2 - Z.2 - Project VFS into the render pack: merge project files (relative-path keyed) into the include/asset pack; byte-clean the web producer so binary assets survive (subsumes the W.3.24 transport residual)
- [x] Z.3 - Z.3 - fab-gui web open/save: open a .scadproj -> in-memory project in the file list (FOLDER treatment, like native FileList), switch+edit any file; Save/publish re-zip; open-file UX says it's a zip
  - [x] Z.3.1 - Z.3.1 - Project document model: in-memory Project resource (files{relpath->buf} + entry + active + origin); EditorBuf becomes the active-file view; assemble->(entry_bytes, pack) render seam. A bare .scad = a 1-file project.
  - [x] Z.3.2 - Z.3.2 - Open funnels to a Project: single .scad -> 1-file project, .scadproj -> N-file (read_scadproj); wire every entry point (native launch/rfd, web ?model= fetch, paste, drag-drop)
  - [x] Z.3.3 - Z.3.3 - Project tab (first, always present): file list click-to-switch (folder treatment), entry marker + set-entry, add/rename/delete file, New; open-file UX says a .scadproj is a zip
  - [x] Z.3.4 - Z.3.4 - Render the ENTRY from project state (edit any file -> re-render entry): native (all project files reachable) + web (lib_fetch merges project files + byte-clean binary assets, closing the W.3.24 transport residual)
  - [x] Z.3.5 - Z.3.5 - Save/publish project-aware: 1 file -> .scad (today's path), >=2 -> .scadproj (write_scadproj); native fs write + web download + web save-back PUT variants + publish upload
  - [x] Z.3.6 - Z.3.6 - native render-source unification (one render-root per project)
  - [x] Z.3.7 - Z.3.7 - loose .scad grows into a project: clear promotion + Save-As .scadproj
  - [x] Z.3.8 - Z.3.8 - web .scadproj save-back (re-zip) — lift the Z.3.5 destructive-save guard
  - [x] Z.3.9 - Z.3.9 - web model NAME from the response, not the URL (Z.5 dogfood): `?model=` points at a media ITEM, so its URL leaf is an opaque `media_ref` hash — read `Content-Disposition` instead (header -> basename -> model.scad), strip publish's ` — model` title suffix, and name uploads/downloads off the DOCUMENT (`ProjectDoc::doc_stem`) not the active file
  - [x] Z.3.10 - Z.3.10 - web project file MANAGEMENT (rename/new/delete/set-entry/add): `project_files_action` was native-only, so a bad entry name inside a loaded `.scadproj` couldn't be fixed in the browser at all. Rules extracted to `file_ops` (cfg-free, unit-tested — the wasm handler is a shim, since the repo has no wasm test harness); browser `<input type=file>` replaces rfd for Add; `Platform::manages_files()` splits document editing from the fs-bound picker
  - [x] Z.3.11 - Z.3.11 - web plate export filename: `print.rs` hardcoded `plates.3mf` on wasm while native uses the source stem — every web export landed on the same name. Now `<doc_stem>-plates.3mf`, matching native
  - [x] Z.3.12 - Z.3.12 - rename the hotchkiss.io media ITEM from the editor: `PUT /media/<ref>` JSON `{title}` (the site's shipped DQ.4 metadata control, admin-gated by the same session cookie as the save-back). Re-applies publish's ` — model` suffix so the published trio stays grouped; adopts the new name locally so the header/download/publish stem agree without a reload
- [x] Z.4 - Z.4 - hotchkiss-io kind: MediaKind::OpenscadProject + probe extension + render_embed_html editor arm + ?format=project token + ext_for_mime (their repo, no migration)
- [x] Z.5 - Z.5 - Publish round-trip + validate: publish a project zip, re-open from the gallery; e2e; native+wasm+fmt/clippy/tests green; dogfood shower_holder end-to-end on the web (web-v0.27.0: upload + naming confirmed live by chotchki)

