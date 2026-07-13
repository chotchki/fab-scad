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

