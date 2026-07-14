# SPEC — R1: the tracer boolean (fab-manifold), the go/no-go

R0 proved the mesh spine reproduces the C++ kernel bit-for-bit on properties (K.0 green). R1 is the
real bet: port Manifold's robustness core — the mesh boolean — and prove `cube ∪ cube` is
boolean-residual-clean `< 1e-5` vs the C++ kernel. If it isn't, we STOP. Everything before this was
scaffolding FOR this.

This scope comes from a deep read of the actually-linked v3.5.1 pipeline (`boolean3` / `boolean_result`
/ `face_op` / `edge_op` / `polygon` / `collider` / `sort` / `shared`). **~4,030 NCLOC of real port
work**, not counting the tests.

## The pipeline (what a union actually does)

A boolean in Manifold is five stages, NO exact arithmetic anywhere — plain f64 + a tracked epsilon +
an operation-dependent SYMBOLIC PERTURBATION (`Shadows()`) that resolves exact-coordinate ties. That
perturbation model IS the robustness; a "more correct" (Shewchuk/exact) predicate makes DIFFERENT
choices at ties and breaks the differential. We port it verbatim.

| stage | file | ~NCLOC | role |
|---|---|---|---|
| broad phase | collider.h + sort.cpp | 320 | find candidate intersecting edge×face pairs (BVH) + Morton geometry sort |
| **intersection core** | boolean3.cpp/.h | 680 | the shadow-kernel cascade (Shadow01→Kernel02→Kernel11→Kernel12) + winding numbers → the four tables `xv12_/xv21_/w03_/w30_`. THE robustness core. |
| assembly | boolean_result.cpp | 650 | windings→inclusion counts, remap/duplicate verts, thread intersection verts into edges, emit the output halfedge polygon soup |
| retriangulation | face_op.cpp | 230 | `Face2Tri`: cut faces (arbitrary polygons) → triangles; re-pair halfedges |
| 2D triangulator | polygon.cpp | 800 | ear-clip/monotone retriangulation of `>4`-gon cut faces |
| cleanup | edge_op.cpp | 750 | `SimplifyTopology` — edge-collapse / degenerate removal |
| shared vocab | shared.h | 600 | Halfedge(s), TriRef, TmpEdge, the perturbation primitives, faceNormal/vertNormal |

## The strategy: offset-first, then coincident

The single biggest de-risking move. `Shadows()` is `p == q ? dir < 0 : p < q` on RAW doubles. The
`dir` (sign) input is a sum of tracked normal components — the perturbation.

- **Axis-aligned `cube ∪ cube`** shares integer coordinates, so `p == q` fires CONSTANTLY → nearly
  every retain/discard decision routes through the perturbation, which means `faceNormal_` and
  `vertNormal_` (the angle-weighted `acos` pseudonormal — the known [[libm-transcendental-divergence]]
  hazard) MUST be byte-identical.
- **An offset `cube ∪ cube`** (diagonal translation, general position) has NO coincident coordinates →
  `p == q` never fires → the normals are NEVER CONSULTED. The core algorithm runs; the perturbation is
  inert.

So R1 splits: prove the CORE pipeline on the offset case first (GATE-A), where the whole normals +
libm-acos hazard is switched off. Only then harden the perturbation for the coincident case (GATE-B),
pulling the determinism-phase `libm`-crate adoption forward for `CalculateVertNormals`. If GATE-A
(the easy case) isn't clean, the core is wrong and we STOP without ever touching the normal hazard.

## What DEFERS (not needed for the tracer)

- **`edge_op` entirely (~750 NCLOC).** The union calls `SimplifyTopology` ONCE, and `IsManifold()`
  already passes BEFORE it runs (`boolean_result.cpp:944`). Every mutation it makes moves geometry by
  `≤ tolerance_` (~1e-12·bbox for a unit cube) or exactly zero. For a residual-`<1e-5` union RESULT it
  is a no-op. Port it in R2 when the nasty-model corpus needs decimation.
- **The LBVH collider.** A serial brute-force O(n·leaves) box-overlap scan emits the identical
  candidate pairs. Port the Karras radix-tree BVH later (behind a flag, differential-tested to emit the
  same set).
- **2D holes / keyhole path in polygon.cpp.** An offset `cube ∪ cube` carves L-shaped (notched, no
  interior hole) cut faces → the simple-polygon ear-clip runs; `tree2d` + `CutKeyhole` defer.
- **Every parallel path** (`tbb::combinable`, `MANIFOLD_PAR`) — serial only, per the SPEC.
- **`Slice`/`Project`** (2D outputs), smoothing/refine, `CompactProps`.

## The risk register (why this could fail the go/no-go)

All HIGH-severity, all from the deep read:

1. **FMA contraction — the #1 hazard.** Every `a*b + c` in `Interpolate`/`Intersect`/`dot`/`cross`/
   `CCW`/`determinant2x2` is a separate rounded multiply then add. Manifold builds `-ffp-contract=off`
   (confirmed `CMakeLists.txt:234-237`); Rust never auto-contracts, so plain ops match — but we must
   NEVER call `f64::mul_add` (already clippy-banned) and never enable fast-math. A single FMA flips a
   value in the last ULP → can flip an inclusion at a tie → a TOPOLOGY change, not a `<1e-5` residual.
2. **Perturbation tie-break semantics.** `Shadows` (`p==q ? dir<0 : p<q`, NaN→false), `withSign`
   (sign by `expandP`; union = `expandP=true` → `+v`), `CCW` (`area²·4 ≤ base²·tol²` — the literal `4`,
   the `≤`, the squared form are all load-bearing). Any "cleaner" predicate diverges.
3. **`faceNormal_`/`vertNormal_` byte-identity** (coincident case only — GATE-B). These ARE the
   perturbation vectors. `vertNormal` is angle-weighted (`acos` summed over the halfedge ring in
   `ForVert` order) → adopt the `libm` crate here.
4. **Container iteration order is load-bearing.** `AddNewEdgeVerts`/`AppendPartialEdges`/`AppendNewEdges`
   iterate `std::map`s and assign halfedge slots as they go → **BTreeMap, never HashMap**.
   `AssembleHalfedges` uses a `std::multimap` whose equal-key match is FIFO insertion order (libstdc++)
   — must replicate. `EdgePos::operator<` tiebreaks on exact-f64 `==` of a `dot` — summation order and
   no-FMA both matter.
5. **Morton sort stability.** 30-bit codes collide heavily on a symmetric cube; C++ `stable_sort` keeps
   pre-sort index order at ties. Unstable sort or different pre-sort input → different index space →
   different perturbation outcomes. (Deferrable for the offset tracer — see M.1.1.)

**The oracle tolerates its own nondeterminism.** The boolean-residual metric (`vol((A−B)∪(B−A))/vol(A)`)
is triangulation-INDEPENDENT (G.3.7), so even if the linked C++ build runs parallel and its mesh varies
run-to-run, the residual is stable. A serial `MANIFOLD_PAR=NONE` oracle is a DEBUGGING aid (to
snapshot-compare the four intermediate tables), not a gate requirement.

## Data structures fab-manifold still needs

Beyond R0's `Vec3`/`Mat3x4`/`Box3`/`Mesh`: `vec2` + `vec4`; the value-style `Halfedge{start,end,pair,
prop}` (distinct from the stored triple) + `TriRef` + `TmpEdge`; `Intersections{p1q2, x12, v12}` (this
version dropped the old `SparseIndices` API here); mutable `Halfedges` accessors (setters, `push_back`,
the `ForVert` orbit iterator); `DisjointSets` union-find (rank + lower-index tie rule); `stable_sort` +
`Permute`; a serial brute-force `Collisions`; and the perturbation-input precompute (`faceNormal_`,
`SetEpsilon`/`tolerance`, and — GATE-B — `vertNormal_`).

## The two gates

- **GATE-A (offset go/no-go, end of M.1.3):** offset `cube ∪ cube` residual-clean `< 1e-5` vs C++,
  `IsManifold` + exact genus + analytic volume. CLEAN → the intersection→assembly→retriangulation core
  is proven (the hard 80%). NOT clean → diagnose against the snapshotted intermediate tables, or STOP.
- **GATE-B / thesis (end of M.1.6):** the structure-aware fuzzer (one `#[derive(Arbitrary)]` CSG-tree
  generator, up to 100 random transformed cubes, union) residual-clean `< 1e-5` AND `IsManifold` /
  exact-genus / analytic-Volume over the fuzzer + `polygon_fuzz`, 1h ASan-clean, `intermediate_checks`
  on. CLEAN → **THESIS PROVEN** (the rest is execution risk). NOT clean → STOP at R1.
