# Spike: in-process Manifold kernel (Phase 10)

**Verdict: GO.** A Rust Manifold binding does everything fab's geometry layer needs, in-process,
and the slicing hot path comes back ~70× faster per piece. The move isn't "replace OpenSCAD" — it's
"stop using OpenSCAD as the geometry KERNEL while keeping it as the SCAD front-door." That split is
the whole idea, and the spike says it holds.

## Why this came up

OpenSCAD plays two roles for fab today: it's the LANGUAGE (we generate `.scad` text, we lean on
BOSL2 for `screw_hole`/`onion`/`teardrop`, and the user's models ARE scad) and it's the KERNEL (the
Manifold library, under the hood). All the roundtrip pain is the kernel role — a process spawn per
piece render, stringly-typed codegen (the opposite of "strong typing is testing-before-the-test"),
STL parsed back out. The language role we can't drop: every project chotchki owns is scad input.

So: keep OpenSCAD as the SCAD→mesh front door (render the user model ONCE, cache the mesh), and do
all slicing + connector CSG in-process in Manifold, typed in Rust. The slicer/connector *logic* is
already Rust (`onion_axis`, feasibility, orientation, placement) — only the final geometry EMISSION
is scad strings. We're swapping the emission layer, not rewriting the brain.

## What the spike proved (manifold3d 0.3.1 → manifold-csg-sys 3.5.102, the C API)

Ran against the real `models/Underdesk/Underdesk.scad` (7970 facets, 3987 verts).

- **10.1 — the binding is real.** Builds + links on macOS (cmake 4.3, clang 21). `cube`/`sphere`,
  `union`/`difference`/`intersection`/`batch_*`, `translate`/`rotate`/`transform`, `split_by_plane`
  (both halves), `trim_by_plane` (half-space clip), `status()` for validity, and mesh in/out
  (`from_meshgl`, `to_mesh_f64`). Booleans returned valid manifolds with the right triangle counts;
  STL export round-trips.
- **10.2 — real geometry imports clean.** Raw STL triangle-soup is (correctly) rejected
  `NotManifold`. An exact-BITS vertex weld — OpenSCAD emits bit-identical coords for shared verts —
  reconstructs the indexed solid EXACTLY (3987 verts / 7970 tris, matching OpenSCAD's own counts),
  `status = OK`, ~12 ms release for the whole model. And because fab keeps OpenSCAD as the front
  door, every input mesh is guaranteed-manifold by construction — the robustness risk is designed
  away, not gambled on.
- **10.3 — slicing matches, and it's the payoff.** In-process slab slice vs the OpenSCAD slice of
  the same cut: middle slab 1872 tris (Manifold) vs 1868 (OpenSCAD) — a 4-triangle cut-face
  triangulation difference — bbox identical to 0.01 mm. Latency:

  | path | per piece |
  |---|---|
  | OpenSCAD spawn (spawn + re-import + render, every re-slice) | **277 ms** |
  | in-process (import paid once at ~12 ms, then just the split) | **~4 ms** |

  That ~70× is the difference between the current spawn-storm and the instant background rebuild the
  reactive DAG is supposed to feel like. The import cost amortizes across a whole editing session
  instead of being paid per spawn.
- **10.4 — multi-object 3mf is first-class.** The `threemf` crate writes N pieces as N separate
  objects on one plate (verified by read-back AND raw zip/XML: 2 objects / 2 items / 2 meshes). No
  `--enable=lazy-union` trick to babysit — separation is native.

## Bonus: the floater bug evaporates

The two-axis onion floater (fixed in scad this session by confining the peg to `children()`) is a
foot-gun of scad's union-outside-intersection structure. In the Manifold port each piece is BUILT
from its own cell — peg unioned into the below-piece, socket diffed from the above-piece, both
intersected with the typed cell — so the leak can't exist. One class of bug retired by construction.

## Proposed Track C (needs sign-off before it's a phase)

Keep OpenSCAD as the SCAD front-door; move the kernel in-process. Rough shape:

1. **`fab-geo` module** wrapping manifold3d: STL import+weld, slab slice (`split_by_plane`),
   connector solids, boolean assembly, STL + 3mf export.
2. **Port the slicer** — slab extraction is `split_by_plane`; a multi-axis piece is the intersection
   of its per-axis slabs (floater-free by construction).
3. **Port the connectors** — onion peg/socket as Manifold solids (sphere + cap at `ang`), bolt
   clearance as cylinders + counterbore (NO BOSL2 threads needed — heat-set inserts, so it's just
   negative cylinders). `onion_axis`/feasibility already Rust.
4. **Wire the reactive DAG to the cached input mesh** — re-slice becomes in-process, no spawn.
5. **Keep the OpenSCAD path as fallback + parity oracle** (golden mesh compare, PLAN 8.4).

Carried risks/unknowns: connector-solid fidelity vs BOSL2 `onion()` (replicate the teardrop cap
angle), print-orientation transforms (have `rotate` + a 12-float `transform`), and the Bambu 3mf
settings question (6.5) is orthogonal to this. None look blocking.

Spike code lives in scratch (`manifold_spike`), not the repo — it was a throwaway to answer go/no-go.
