# SPEC — 3D print workflow, backup, and showcase

Status: DRAFT, round 2 (chotchki redlines folded in). The PLAN (`PLAN.md`) is downstream
of this — don't start building until the decisions here are signed off.

## The problem

One folder, ~62 projects, 7.6 G, and five pains that all trace back to the same root:
there's no SYSTEM here, just an accreting pile. OpenSCAD is a great geometry engine; it
has ZERO workflow. Every pain below is a workflow gap wearing a different hat.

1. **Nothing is backed up.** Local-only. A dead disk takes out years of design work.
2. **BOSL2 upgrades itself and breaks old projects.** One floating unpinned shared copy
   (`./BOSL2`, currently `v2.0.743-12-gb5ff7d9b`), referenced two inconsistent ways —
   `include <../BOSL2/std.scad>` (82×) and `include <BOSL2/std.scad>` (80×). An upstream
   pull silently changes the meaning of code I wrote a year ago.
3. **Outputs are massive — because there's no workflow.** STL/3mf dominate the 7.6 G —
   shoe_holder alone keeps `uncut_supported` v1..v13 plus variants at 250–430 MB each.
   It's all REGENERABLE from tiny `.scad`, but with no build system I hoard outputs as if
   they were precious. Fix the workflow and the hoard stops being necessary.
4. **Multi-cut partitioning blows up exponentially.** Manifold is already my default and
   single cuts are fine — NOT a backend problem. BOSL2's `partition()` instantiates its
   `children()` TWICE (once per side), and my multi-cut approach NESTS `partition()` (7
   deep in window_light_blocker). So the child subtree is instantiated ~2^N times — 7 cuts
   ≈ 128 copies of the model, each intersected with a mask. The frozen-STL `import()` just
   makes each of those 128 evals cheap — it treats the symptom.
5. **Multipart OUTPUT is its own problem.** OpenSCAD now writes 3mf (good), but getting
   the pieces out as SEPARATE objects on a plate is fiddly — and it actively fights my
   embedded-magnet technique, where parts must split cleanly around magnet cavities (and
   the print pause that drops them in).
6. **None of it is shared.** I want the good stuff on https://hotchkiss.io/projects to
   showcase, and there's no path from "scad on disk" to "live on the site".

## Design drivers (the principles every decision answers to)

- **Mental burden is the primary metric.** The win isn't features, it's *less to hold in
  my head*. I want to FOCUS one project, lean on SHARED parts, and generate output —
  without remembering a pile of verbs or re-deriving settings (cf. recon-gen's
  `run_tests.sh`). A focused-project context and a GUI for the spatial stuff matter more
  than CLI surface area.
- **Dogfood over design.** Don't build elaborate config/schema up front "for the sake of
  config". Start minimal, run it on a REAL project, add a field only when that project
  proves it's needed.
- **Geometry in SCAD, workflow in `fab`.** OpenSCAD does geometry (it's great at it); the
  tool does everything OpenSCAD won't — orchestration, lifecycle, reproducibility,
  sharing. The tool never reimplements OpenSCAD.
- **Support-free, always.** I design around supports. Every generated feature (pins,
  pockets, counterbores) must print without them.

## What we're building

A REORG plus a Rust tool (`fab`, in the `fab-scad` repo) that turns this pile into a
reproducible pipeline: design → render → slice → print → publish. `fab` is a WORKFLOW
LAYER, not a geometry tool — same move as skylander-portal-controller wrapping an
emulator. The geometry (the linear slicer + the connector library) ships as a SHARED SCAD
lib, not Rust.

Infra-first: stand up the foundation, prove it end-to-end on 2–3 showcase projects
(including live on hotchkiss.io), THEN migrate the rest opportunistically. No big-bang
retrofit of all 62. (chotchki: agreed.)

## Decisions (locked)

- **Backup → GitHub, two repos.** `chotchki/fab-scad` (MIT) holds the tool + the shared
  SCAD toolkit. `chotchki/scad-models` holds my actual designs — PRIVATE for now until I
  work out how to apply CC BY-NC-SA 4.0 (protect the designs: attribution, non-commercial,
  share-alike), then it can go public. Off-site GitHub is the canonical backup either way;
  NAS is the COLD archive for big binaries.
- **Repo topology → `fab-scad` owns the workflow.** `~/workspace/fab-scad` (MIT) is the
  tool's repo AND the workspace root: it carries the Rust binary (`fab`), the shared SCAD
  toolkit (`scad-lib`), pinned third-party OpenSCAD deps (`libs/`, BOSL2 @ v2.0.746), and
  `printers.toml`. The designs (`scad-models`, private/CC) are pulled in UNDER it as a
  pinned submodule. So the workflow owns its toolchain; the designs repo stays pure source.
  Splitting MIT-tool from CC-designs along the repo line keeps the slicer upstreamable
  without entangling the models' license.
- **Big binaries → exclude, cold-archive, then prune ONLY after validation.** Source in
  git; generated STL/3mf gitignored and regenerated on demand. Before ANY deletion: a full
  immutable snapshot of today's 7.6 G lands on the NAS. Pruning old versions is a
  per-project, gated step after I've validated which version is RIGHT (and likely after the
  scad fixes that make it regenerable). No blanket nuke, ever.
- **BOSL2 → one pinned shared submodule**, includes standardized to a single mechanism.
  Track the latest TAGGED release (they finally tag — currently v2.0.746); bumping is a
  deliberate, tested step — never a silent floating pull.
- **Scope → infra first, pilot 2–3, then incremental.**
- **Full reorg is in-bounds.** Separate what's MINE from third-party libs, from downloaded
  models, from generated outputs.

## Target shape

### Folder — `fab-scad` owns it

```
~/workspace/fab-scad/            # repo: fab-scad (MIT) — Rust tool + SCAD toolkit; OWNS the workflow
  src/  Cargo.toml               # the fab binary (command: `fab`)
  printers.toml                  # bed sizes / printer profiles (shared)
  libs/                          # third-party OpenSCAD deps as PINNED submodules (BOSL2 @ v2.0.746, gridfinity, ...)
  scad-lib/                      # MY shared SCAD modules (MIT, upstreamable) — see below
  models/                        # repo: scad-models (private/CC, the designs) as a pinned submodule
    shoe_holder/
      project.toml               # MINIMAL manifest, grown by dogfooding
      src/*.scad                 # source — the only truly precious bytes
      out/                       # generated STL/3mf — GITIGNORED, regenerated
      renders/                   # small cover/thumbnail PNGs — kept
  SPEC.md  PLAN.md
```

Excluded from git (regenerable / live on NAS): `out/`, Rust `target/`, `BOSL2.wiki`,
downloaded third-party models (the Falcon STL et al.), `.DS_Store`. Models resolve
`scad-lib` and `libs/BOSL2` via OPENSCADPATH pointing up into `fab-scad` — the workflow
owns the pinned toolchain.

### Shared SCAD lib — `scad-lib` (lives in fab-scad, MIT)

The cross-cutting modules every design re-needs, in ONE place (the "leverage shared parts"
driver):

- **The linear slicer + connector library** (the core geometry deliverable, below).
- **Version stamping** — emboss/deboss a version onto a part.
- **Part numbering** — label pieces so a sliced set reassembles in order.
- Whatever else the pilots prove is genuinely shared (dogfood it).

MINE and MIT (so the slicer can be upstreamed to BOSL2), distinct from vendored
third-party (`libs/`) and from the CC-licensed designs (`models/`).

### The manifest — `project.toml` (start MINIMAL, grow by dogfooding)

Do NOT design a big schema up front. The manifest exists to capture what the pile loses —
a project's identity, how to build it, how to show it — but the real field set is
discovered by running it on the first pilot, not invented here. Start with roughly:

```toml
[project]
name = "shoe_holder"
title = "Entryway Shoe Holder"

[[part]]
src = "src/shoe_holder.scad"     # one render target; more as needed
```

Everything else — print settings, slicing config, web/showcase metadata, the build DAG
(only for whatever still genuinely stages an `import()`) — gets ADDED when a project
actually needs it. Three things we KNOW we'll want, validated by dogfooding before we
commit schema:

1. **Build DAG (only as fallback).** The `import()` intermediate was a perf crutch for the
   2^N blowup, not an inherent step. Once linear slicing lands, most projects go back to
   single-source; the DAG support survives only for whatever genuinely still stages — and
   there it must know which STLs are live intermediates that must NOT be pruned.
2. **Print settings → ideally INTO the 3mf.** Material/nozzle/orientation. I use Bambu
   Studio; the dream is these ride in the exported 3mf itself (plate, per-object settings,
   pause-for-magnet) so printing is one click — not a manifest I re-read. (Needs a look at
   Bambu's 3mf extensions; see Output.)
3. **Showcase metadata.** Title, summary, tags, which part feeds the web viewer, cover
   image — what hotchkiss.io's content model wants.

### The tool — `fab` (few verbs, a focus context, a GUI for the spatial stuff)

The verbs below are the scriptable ENGINE. But "too many verbs to remember" is itself a
mental-burden cost, so the primary UX is: **focus a project once**, then short commands
act on it — and the high-burden spatial work (cuts + peg placement) lives in a GUI, not
flags.

**How `fab` wraps OpenSCAD is itself an unknown to dogfood EARLY.** OpenSCAD has no API
for what we need (headless render, geometry round-trip, live preview a GUI can drive) — so
the integration pattern (CLI invocation? STL/3mf round-trip? a preview the GUI renders
itself?) has to be discovered by doing, not designed. That's why an integration spike
lands in the foundation (Phase 3) and the GUI comes EARLY (Phase 5, right after the slicer
engine) rather than as a capstone — building it is how we learn the wrap.

- `fab focus <project>` — set the active project; later commands need no name (cf.
  plan-bridge `activate`). Cuts the verb-memorization tax.
- `fab doctor` — env preflight: OpenSCAD + Manifold, NAS reachable, pins match, submodules.
- `fab new <name>` — scaffold from the template (minimal manifest + dirs).
- `fab render` — render `out/` + thumbnails; sliced projects render each piece as an
  INDEPENDENT parallel job; incremental (skip unchanged, content hash).
- `fab web` / `fab publish` — build the web bundle (decimated STL + cover + source) and
  push it live to hotchkiss.io (auth below).
- `fab archive` / `fab bosl2` — NAS cold-archive; report/manage the BOSL2 pin.
- **`fab gui`** (or a desktop app) — the headache-killer, and EARLY (Phase 5): set cut
  planes, click a face to place pegs/connectors, preview piece-vs-bed and print
  orientation, emit the slicing spec. Doubles as a friendly front-end so I'm not memorizing
  verbs. Gated only on the slicer engine (Phase 4); building it is how we dogfood the
  OpenSCAD wrap. PLANNED, not a maybe.

### Linear slicing — the core geometry fix (SCAD)

Make multi-cut LINEAR. Three pieces:

- **Don't nest `partition()`.** Each piece is `source ∩ region_i` (the slab between cut
  plane i and i+1). N+1 pieces = N+1 intersections, child evaluated ONCE per piece. Linear,
  not 2^N. Lives in `scad-lib`, replaces the nested-`slice_part()` idiom.
- **Minimize cuts FIRST.** Before cutting, try to fit the part on the bed by rotation /
  diagonal placement — a diagonal fit has saved a cut more than once. Needs printer bed
  size (`printers.toml`). Fewer cuts > clever cuts.
- **Render pieces independently.** Each piece is its own intersection, so `fab` fans the
  renders across cores → wall-clock SUBLINEAR.

**Joinery = planar cut + pluggable CONNECTORS** (not jigsaw/dovetail). Default to a flat
slab cut; register pieces with connectors on the mating faces:

- **Removable (default / favorite): heat-set insert + M bolt.** Insert pocket on one
  piece, bolt clearance hole + head counterbore on the other. Reuse BOSL2
  `screw_hole`/`nut_trap` for the bolt side; the heat-set pocket is custom (BOSL2 has
  NONE) — harvest specs from `ams_stackfix` / `ceiling_bracket` / `new_desk_v2`.
- **Permanent: teardrop alignment pin + glue.** Hole+peg for alignment only; clamp and
  superglue. Teardrop so it prints support-free.

**Placement + print orientation is the shared hard problem** (across both connector types).
Each piece carries a print orientation (manifest field; auto-suggest "largest flat face
down"); every connector derives its geometry orientation from its own piece so pockets,
counterbores AND teardrops print SUPPORT-FREE. Mating halves orient independently —
clearance + glue/bolt absorbs the slop. Placing connectors on a face and orienting each
half is miserable as SCAD numbers — that's what the GUI is for.

**Tolerances + integrity, tested not assumed.** Connector slop must be TUNED — generate
small test coupons (a sample joint) to dial clearance before committing a full print. And
slicing must not silently shrink parts: validate that the reassembled pieces equal the
original within tolerance (dimensional-integrity check, not just "it looked right").

This is what retires the `import()` crutch and the giant intermediate STLs — they only
ever existed to survive the exponential blowup.

### Output — multipart 3mf, Bambu, embedded magnets

A workflow-layer (`fab`) concern, distinct from slicing:

- **Separate objects in one 3mf.** Pieces must export as DISTINCT objects on a plate (not
  merged into one mesh) so Bambu treats them as parts — needs OpenSCAD's lazy-union /
  per-object export; verify it actually preserves separation.
- **Embedded magnets.** Parts must split cleanly around magnet cavities, and the print
  needs a pause-at-layer to drop magnets in. Ideally `fab` emits the 3mf with the pause /
  per-object metadata already set.
- **Bambu settings in the 3mf.** The aspiration from print-settings above: bake
  plate/material/pause into the exported 3mf so printing is one click. Needs investigation
  of Bambu's 3mf extensions — flagged as an unknown, not a commitment.

### Backup topology

- **Primary, off-site:** public GitHub (source + manifests + small renders).
- **Cold archive:** NAS (`/Volumes/NAS/3d_print/_cold_archive/<date>/`) — the full current
  7.6 G snapshot + regenerated outputs over time. The "give me last month's STL back" net.

### Website integration — mostly free

hotchkiss.io is Rust/Axum + SQLite + HTMX, self-hosted with push-to-deploy. It ALREADY
has a Three.js STL viewer (markdown `![](x.stl)` → 3D), on-the-fly AVIF resizing, a
`/projects` page over child content-pages, and an admin edit UI — plus its own SPEC
"Phase 15: hand-curate 5–10 prints". A 3D project = a `content_pages` row under `projects`
+ `attachments`. Integration is PRODUCING good artifacts and getting them in, not changing
the site. **Auth:** the site uses passkeys today; chotchki is fine adding API keys (or
similar) so `fab publish` can post safely — that's the leaning for the publish mechanism.

### Deployment (resolved — Phase 18 spike, 2026-07-03)

Both candidate modes were spiked to WORKING artifacts in a day; evidence + port notes in
[deploy-spike-notes](docs/deploy-spike-notes.md). Web-first is the sequencing decision, not a
religion — native is proven and parked, not dead.

- **Web (primary):** the slicer ships as a wasm bundle hotchkiss.io bakes into a special page —
  standalone full-page app, client-only, ZERO outputs stored server-side (a visitor's model
  never leaves their browser; that line goes ON the page). Stack held up under fire: Bevy 0.19
  + feathers render on wasm32-unknown-unknown (bevy#22620 did not reproduce, WebGL2 AND
  WebGPU), and the Manifold kernel runs in Chrome via manifold-csg `unstable-wasm-uu` +
  wasm-bindgen — 387 KB of kernel in a Bevy-dominated bundle. STL-upload-first; openscad-wasm
  (.scad in the browser) stays a stretch with a known GPL calculus. Artifact contract + tag
  `web-v*` release pipeline + seconds-scale dev loop: [web-bundle](docs/web-bundle.md).
- **Native (parked):** cargo-packager emits a working .app+DMG and NSIS from one config —
  proven locally, workflows kept manual-dispatch only. The signing bill is known ($99/yr Apple
  is unavoidable post-Sequoia; Windows ships unsigned via winget): [packaging](docs/packaging.md).
- **crates.io (opportunistic):** the `fab-scad` name is free and the package verifies at
  103 KiB — publishing claims the name; `cargo install` becomes the from-source channel.

## Constraints & risks

- **Data loss is the cardinal sin.** Cold-archive lands BEFORE any deletion; pruning is
  gated on per-project validation. `import()` intermediates make naive deletion dangerous —
  the manifest DAG is what makes pruning safe.
- **The slicer is a redesign, not a faithful port.** Slab + connectors REPLACES the nested
  jigsaw — so the test is FUNCTIONAL (pieces fit the bed, connectors mate with clearance,
  every feature prints support-free, reassembly matches original dimensions), not a diff
  against the old STL.
- **Placement + orientation is the hard part, not the cut**, and it's shared across
  connector types — which is exactly why the GUI earns its keep.
- **Slop must be tuned.** Connector clearances are printer/material-dependent — test
  coupons before full prints, or parts won't fit / bolts won't bite.
- **Heat-set pocket specs must be right.** BOSL2 has none; harvest from existing projects
  + the insert maker's spec.
- **Bambu 3mf embedding is an unknown.** Baking settings/pauses into the 3mf may or may not
  be cleanly doable — investigate before promising one-click.
- **BOSL2 pin may strand a few projects.** Per-project `[bosl2] pin` override is the escape
  hatch; the inventory pass sizes the problem first.
- **The long tail.** 62 projects, many one-offs. Incremental migration is deliberate; the
  backlog may stay a backlog. Don't let it block "backed up".

## Resolved (round 2)

- Tool name → `fab` (repo `fab-scad`, MIT). · Repo topology → `fab-scad` owns it; designs
  in `scad-models` submodule; toolchain (`libs/`, `scad-lib`) at `fab-scad` level. · BOSL2
  pin → track latest TAG, currently v2.0.746. · Pilots → shoe_holder, keyboard_tent,
  nail_polish_holder. · GUI → build it, EARLY (Phase 5). · Cold archive → already done
  manually to the NAS.

## Open sub-decisions (resolve in-phase, not now)

- **CC BY-NC-SA 4.0 on `scad-models`** — how to actually apply/structure it (LICENSE, per-file
  headers, README terms) before flipping the repo public. Until then it stays private.
- **`fab publish` auth** — API key on hotchkiss.io is the leaning (passkeys for humans).
- **GUI toolkit + the OpenSCAD wrap** — which stack (egui / three-d / bevy / tauri+three.js),
  and how it drives OpenSCAD (CLI render, geometry round-trip, self-rendered preview).
  Discovered by the Phase 3 spike + Phase 5 GUI, not decided here.
- **How far Bambu 3mf embedding can go** — plate/material/pause baked into the 3mf may or
  may not be cleanly doable; investigate.

## Success criteria

- Repo clones small and fast from public GitHub; source fully off-site.
- Fresh checkout + `fab render` reproduces a project's outputs with no fiddling, on a
  pinned BOSL2 that won't move under me.
- Multi-cut slicing is linear/parallel (not 2^N), pieces fit the bed, connectors mate, and
  reassembly matches the original — `import()` intermediates gone on those projects.
- A sliced project exports as separate objects in a Bambu-ready 3mf (magnet pause where
  needed).
- At least one project LIVE on hotchkiss.io/projects, published via `fab`.
- Day-to-day, the workflow is LOW mental burden: focus a project, reuse shared parts,
  generate output.
- Nothing deleted that wasn't first archived and validated.
