# `minkowski()` on Manifold — design

Status: **design** (J.4.4). No code yet. This is the research + design deliverable; implementation is a
separate, phased task gated on the decisions in [§8](#8-open-decisions).

Sources are primary and adversarially verified (a 108-agent deep-research sweep, 2026-07-07) — GitHub issue
threads read via the API, algorithm complexity from the peer-reviewed literature. Citations in
[§9](#9-sources).

---

## 1. The problem, and why it's genuinely hard

`minkowski(){ A; B; }` is the Minkowski sum `A ⊕ B = { a + b : a ∈ A, b ∈ B }`. On a triangle-mesh CSG
kernel it is the one classical CSG op with no clean primitive, and the state of the art confirms it — this
isn't us being slow to a solved problem:

- **OpenSCAD** implements it as *convex-decompose both operands → pairwise convex Minkowski sums → union
  all*. The decomposition runs on **CGAL Nef polyhedra** (`convex_decomposition_3`), which is exact but
  slow AND crash-prone: a decade of bug reports (#1097, #1455, #4623, #6359) show CGAL's Nef decomposition
  asserting/crashing on concave input, some exceptions uncatchable (the Nef destructor faults during stack
  unwinding). ochafik, who wrote OpenSCAD's Manifold integration: *"the CGAL Nef-based convex component
  decomposition we use is quick to crash with random models."*
- **OpenSCAD's Manifold backend still farms minkowski to CGAL.** The blocker (kintel, OpenSCAD lead,
  #6297, 2025-10-19): *"The convex decomposition doesn't have a corresponding operator in Manifold, so we
  currently use CGAL to perform it."* Only the final *union* runs on Manifold. The proposed fix — "implement
  all or part of Minkowski in Manifold, convex decomposition being the core missing feature" — is still open
  and tagged for a future release.
- **The complexity is intrinsic, not an artifact.** Convex⊕convex output is `Θ(nm)` facets in 3D (tight
  bound `4mn − 9m − 9n + 26`, Fogel–Halperin–Weibel). Non-convex⊕non-convex is `Θ(n³m³) = O(n⁶)` — and the
  **union of the pieces is the dominant cost** (the decomposition and per-pair hulls are cheap; materializing
  the `O(n⁶)` output is not). This is why concave minkowski hangs/OOMs everywhere, including our L.2.7
  timeouts.

## 2. The reframe that unlocks it: volume-residual validation, not bit-exact

The reason minkowski "hasn't ported" cleanly is that a mesh-based sum is **topologically different** from
CGAL's Nef result even when geometrically identical — so you can't diff meshes vertex-for-vertex. Our
differential harness already has the right tier: `differ.rs::boolean_residual = sym_diff_ratio(scad, oracle)`
(the symmetric-difference / XOR volume ratio) plus `diff_within(scad, max_residual)`. That validates
**shape** regardless of topology.

This splits our two hard requirements cleanly and is the crux of the whole design:

| Requirement | For minkowski | Consequence |
|---|---|---|
| **Oracle match** | RELAXED to volume-residual `< ε` | a topology-different-but-shape-correct sum passes; we don't need CGAL's exact Nef mesh |
| **Cross-platform determinism** (doctrine #36) | **STILL required** | our own output must be bit-identical every platform — this is the real discriminator (see §5) |

The residual relaxes the *oracle* bar but not the *determinism* bar. That's what keeps the good algorithm
(deterministic Manifold hull+union) and rules out the easy escape hatch (randomized convex decomposition).

## 3. The algorithm — Manifold's own tiered approach (PR #666)

Manifold shipped native `MinkowskiSum`/`MinkowskiDifference` in **PR #666 (zalo, merged 2026-01-17)**, and
it is *exactly* the design we'd reach independently. The shipped `src/minkowski.cpp` has **three tiers** (the
tier gate is a convexity test on each operand):

1. **Convex ⊕ Convex** — one convex hull of *all pairwise vertex sums*. No face iteration.
   `A ⊕ B = conv({ a + b : a ∈ V(A), b ∈ V(B) })` (standard theorem: `conv(A) ⊕ conv(B) = conv(A ⊕ B)`).
   Cost: hull of `|V(A)|·|V(B)|` points, `O(nm log nm)`. Exact.
2. **Convex ⊕ Non-convex** — **the rounding case**, and the one that matters. Iterate faces of the
   **non-convex operand only**; for each face `F` (3 verts), hull `F ⊕ (whole convex operand)`; batch-union
   all. That's `O(#faces_of_nonconvex)` hulls, **not** `O(nm)`. `minkowski(){ arbitrary_shape; sphere(r); }`
   — round/offset a shape by a sphere or box — lands here. Manifold uses `BATCH_SIZE = 1000`.
3. **Non-convex ⊕ Non-convex** — brute force: for every `(faceA, faceB)` pair, hull the 9 vertex-sum
   combos, batch-union with periodic reduction (`REDUCE_THRESHOLD = 200` to fight the union OOM). This is the
   `O(n·m)` hull blowup + the `O(n⁶)` union. zalo's own disclosed drawbacks: *"a bit slower than the general
   technique… runs out of memory when performing the final union (especially Non-Convex/Non-Convex)."*

The maintainers are explicit that **tier 2 is effectively the whole real-world need**. Lalish (Manifold
owner): *"Have you ever seen it used with anything but a sphere?"* thehans (OpenSCAD): *"not really, just
variations on sphere; e.g. geodesics."* pca006132: *"we will not be able to support general minkowski, but I
doubt if people really need that."* This is our prioritization, straight from the people who own the kernel.

## 4. Our toolkit maps 1:1

We have every primitive tiers 1–3 need, in `src/kernel.rs`:

- `Solid::to_indexed()` / `tris()` — mesh vertex/triangle access (feeds hulls + the convexity test).
- `Solid::batch_hull(&[Solid])` — convex hull over a vertex set (hull of pairwise sums; per-face hulls).
- `Solid::batch_union(&[Solid])` — the accumulate-and-merge union (with our own periodic reduction).
- `Solid::translate` / `transform` — position a probe copy per vertex.
- 2D: `Section` (Clipper2) — see §6.

Tier-1 hull-of-points: `batch_hull` hulls the union of the operands' vertices, so we either build a
degenerate point-solid or (cleaner) sum vertex lists and hull once. A `hull_points` helper is the one small
primitive to add.

**Convexity test** (the tier gate): a mesh is convex iff every vertex lies on the non-positive side of every
face plane (equivalently, the mesh equals its own convex hull — `sym_diff_ratio(m, hull(m)) ≈ 0`). The
latter reuses machinery we already have and is robust; the per-face-plane test is faster. Either is
deterministic.

## 5. Determinism — the real risk, and where it bites

This is the load-bearing concern for us, and the research surfaced a direct warning:

- **Manifold is non-deterministic BY DESIGN** (parallel reduction order). pca006132: *"We have
  non-determinism by design. We can force determinism by setting `ManifoldParams().deterministic = true`."*
  Our `manifold3d` 0.3.1 crate does not expose that setter — **action item: confirm the flag is reachable
  (bind it, or upgrade the crate) and that our existing booleans already run deterministic**, else our whole
  geometry tier has a latent cross-platform hole, not just minkowski.
- **Even with the flag, tier 3 broke cross-platform.** PR #666's non-convex⊕non-convex CI test failed on
  GitHub's Mac and Windows runners (passed on Linux) — *"triangulations not coming out CCW"* — from a
  boolean-algorithm bug on self-intersecting **"kissing union"** coincident geometry (adjacent per-face
  hulls share faces exactly). zalo ended up **commenting the test out** pending an elalish boolean fix. This
  is the same failure family as OpenSCAD's #6297 root cause (two cubes sharing an edge → epsilon-valid mesh
  duplicates a vertex → self-intersection).
- **Tiers 1 and 2 are far safer.** Tier 1 is a single hull (no coincident-face union). Tier 2's union is of
  per-face swept hulls over a *convex* probe — much less kissing-geometry stress than tier 3's face-pair
  grid. These are the tiers we'd ship first; tier 3 is exactly the one with an open cross-platform
  determinism bug upstream.

Net: determinism is achievable for tiers 1–2 on a deterministic-configured Manifold, and it's the reason we
do **not** reach for approximate convex decomposition — V-HACD / coACD are randomized/threaded and
non-deterministic, so even if a decomposition passed the volume-residual it would drift platform-to-platform,
violating the doctrine. Manifold's team reached the same conclusion (they rejected 3D convex decomposition as
"not a practical approach" and pursued direct offsetting instead).

## 6. 2D is nearly free

We bundle Clipper2 via `Section`/CrossSection. Clipper2 ships `MinkowskiSum`/`MinkowskiDiff`. If we bind them
through `Section`, 2D minkowski (`minkowski()` over 2D children) is close to a delegate-and-done — 2D
convex⊕convex is `O(n+m)` and Clipper2 handles the general polygon case. This is a small, high-value,
low-risk slice worth doing early and independently of the 3D work.

## 7. Recommended design

Match Manifold's tiering, ship it in risk order, validate by residual:

- **Phase A — `hull_points` + convexity test + tier 1 (convex⊕convex).** Smallest, exact, no union-degeneracy
  risk. Pins the residual harness on a case with a known answer.
- **Phase B — tier 2 (convex⊕non-convex), the rounding fast path.** Per-non-convex-face `hull(F ⊕ convexB)`,
  batch-union with our own periodic reduction. This is ~all real usage. Order the operands so the *convex*
  one is the probe; if neither is convex, fall to Phase D. Validate `minkowski(){ shape; sphere($fn=…); }`
  against the oracle by `boolean_residual`.
- **Phase C — 2D via Clipper2.** Independent of A/B; bind `Section::minkowski`.
- **Phase D — tier 3 (non-convex⊕non-convex).** GATED on the cross-platform determinism story (the kissing-
  union CCW bug). Options, in order of preference: (i) prove Manifold's `deterministic=true` + a fixed
  boolean handles our kissing unions bit-identically across the CI matrix; (ii) ship it behind a flag with a
  LOUD "non-convex⊕non-convex minkowski: cross-platform determinism unverified" note; (iii) keep the current
  LOUD `Unimplemented` for this one case only. Never a silent wrong mesh.

Everywhere: `$fn`/`$fa`/`$fs` drive the sphere/probe tessellation (already ours), so the probe is
deterministic; the sum inherits that.

## 8. Open decisions

1. **`ManifoldParams().deterministic`** — is it already set for our existing booleans? If not, that's a
   pre-existing doctrine gap to close *before* minkowski (it affects every parallel Manifold op, not just
   this). First thing to verify.
2. **Tier-3 policy** — ship-behind-flag vs keep-LOUD-unimplemented until the upstream kissing-union boolean
   bug is confirmed fixed and cross-platform-verified. Recommend: keep tier 3 LOUD until (1) is proven, land
   tiers 1/2/2D first (which is ~all real usage + `test_cyl`).
3. **Residual tolerance** for the minkowski differential — what `max_residual` counts as "matches the
   oracle"? Needs a value calibrated on tier-1/2 cases where we know the exact answer.
4. **`test_cyl`'s actual trigger** — it reaches minkowski through `cyl()` internals (`circum`/`chamfer`);
   confirm which tier it needs (likely 2). It may pass on tiers 1/2 alone.

## 9. Sources

- OpenSCAD blocker + pipeline: openscad/openscad#6297 (kintel, 2025-10-19), PR#4533 (ochafik, 2023).
- Manifold stance + native impl: elalish/manifold#415 (convex-decomp feature request, rejected), **#666
  (zalo, MinkowskiSum, merged 2026-01-17)**, #192 (mesh offsetting, open).
- Determinism: manifold#666 CI thread (`ManifoldParams().deterministic = true`; Mac/Windows CCW failures).
- Complexity: Fogel–Halperin (CAD 39:929, 2007), Fogel–Halperin–Weibel (DCG 42:654, 2009, tight `4mn−9m−9n+26`),
  Das–Sarvottamananda (DAM 2024, worst-case-optimal `O(nm)`), Hachenberger (Algorithmica 2009, `Θ(n³m³)`).
- Convex identity: Delos–Teissandier (arXiv:1412.2564), `conv(A)⊕conv(B)=conv(A⊕B)`.
