# SPEC: manifold-rs — the geometry kernel in Rust

2026-07-14. Scoped from a 6-front investigation ([[manifold-rs-feasibility]] recon → this). `[OPEN]`
marks a decision that's chotchki's to make before/early in the port. This is a SCOPE, not the port —
it bounds the work, picks the load-bearing approaches, and defines the smallest thing that proves the
bet before we sink months into it.

## The bet

Reimplement Manifold's geometry kernel in pure Rust so fab-scad drops the C++ / emsdk / wasm-cxx-shim
toolchain ENTIRELY and owns the three things a binding structurally cannot fix: **determinism,
parallelism, and math portability**. The measured target is ~11K NCLOC of real port work (14.9K
upstream Manifold, minus ~2.4K skipped features, minus the parallel layer we replace rather than
port), and the WHOLE bet lives in a ~4.4K "robustness core" (`impl` + `polygon` + `boolean3` +
`boolean_result` + `face_op` + `edge_op`). A 95%-right version passes cubes and fails the nasty corpus
— there is no partial credit here.

What it unlocks: **bit-identical output native == wasm** — the fab-scad determinism doctrine (same
source → same bytes, every platform) that the C++ kernel STRUCTURALLY CANNOT satisfy (two known core
defects — a non-total-order comparator + a global mesh-id counter — [[onetbb-wasm-determinism]]); one
language + LTO; the C++ toolchain gone; and possibly `Solid: Send` for free (the `!Send` hazard is a
`csg_tree` lazy-mutation artifact that immutable Rust leaves dissolve — [[manifold-kernel-threading]]).

## The bounded surface

The ENTIRE port target is `src/kernel.rs` — a closed set. `backend.rs` and everything downstream reach
geometry through the `GeometryBackend` trait, never Manifold directly; `lang/`, the differential
harness, and the corpus tests name "Manifold" only in comments. So `kernel.rs` IS the complete spec of
what has to exist. ~24 wrapper methods over ~46 binding entry points; most trivial, ~6 hit the core.

**3D — PORT.** Primitives (`cube`/`sphere`/`cylinder`, pure generators). Booleans ★ (`union`/
`difference`/`intersection`/`batch_union`/`batch_hull`/`minkowski_sum` — the core; minkowski validated
by volume-residual, not bit-exact). Transforms (`translate`/`rotate` degrees-Euler-XYZ/`transform`
3×4). Queries (`volume` **MUST be SIGNED**, `surface_area`, `genus`, `num_tri`/`num_vert`, `is_empty`,
`bounding_box`, `status` the validity checker). Mesh IO (`from_mesh_f64`/`to_mesh_f64` — the stride
3=xyz / 7=xyz+RGBA sentinel is load-bearing; NEVER f32-downcast; `set_properties`). Slicing
(`split_by_plane` preferred over `trim_by_plane`, `decompose`, `slice_to_cross_section`, `project`,
`slice_at_z`). 2D→3D (`extrude_with_options`, `revolve`, `CrossSection::extrude`).

**2D — PORT** (via a 2D boolean engine, see Dependencies): booleans, `from_polygons` (Positive fill) +
even-odd variant, `offset` (Round/Miter/Square; chamfer→jtSquare, miter_limit 2.0), `area`,
`to_polygons`, `transform` (2×3), `hull_simple_polygon` (critical path — the teardrop/wedge).

**DON'T PORT** (grep-confirmed unused, ~2.4K NCLOC deleted): `refine`/subdivision, `smooth`, `sdf`/
marching-cubes, `warp`, `compose`/`as_original`, `calculate_normals`, curvature, `MinGap`, the C-API,
and Manifold's `mesh_fixes` (fab-scad does its own exact-bits weld in `kernel.rs`).

**Two subtle semantics a naive port silently breaks — bake into the corpus DAY ONE:**
1. Variable-width vertex properties threaded through booleans with LINEAR INTERPOLATION at cut verts
   (color survives a union — the [[bosl2-color-this-test-target]] gate).
2. The signed-volume + `decompose` cavity contract — an enclosed void is a separate INVERTED shell
   (negative signed volume); `components()` partitions on `volume() >= 0`. We JUST fixed a bug here
   (W.4) — the port must preserve this exactly.

## Pillar 1 — wasm-safe DETERMINISTIC parallelism `[the crux]`

**Decision: `rayon` + `wasm-bindgen-rayon`, behind a thin manifold-rs-owned `par::` module** mirroring
Manifold's `parallel.h` (~24 primitives). It's the only candidate that gives BOTH the wasm story
(maintained; COOP/COEP already set; first-class single-thread fallback) AND nesting-safe work-stealing
— and Manifold NESTS (`BatchBoolean` task-group → each `SimpleBoolean` runs data-parallel inside), so a
fixed-partition blocking-join pool would DEADLOCK. Rejected: a hand-rolled pool (re-earns work-stealing
= rebuilds rayon), chili (wasm not first-class).

Determinism is NOT free from rayon — it's achievable BY CONSTRUCTION, and `par::` is the ONLY
parallelism door in the crate (clippy-ban direct `rayon::iter` in-kernel):
- disjoint-write ops → indexed `par_iter().collect()` (deterministic for free; ~80% of the surface).
- reductions → **type-gated by a `CommutativeAssociative` marker trait**: a non-associative float
  reduce WON'T COMPILE. Float-add that feeds geometry goes through `par::reduce_serial` (fixed-order
  Kahan, matching Manifold's own serial Volume/Area loops — NEVER "optimize" these into a parallel
  reduce).
- scans → hand-rolled fixed-block-size (block = f(n), NOT thread count), integer operands → bit-
  identical across thread count and platform.
- `stable_sort` → total-order comparator enforced by an `Ord` bound (the EdgePos fix, see Risks);
  radix for ints.

**Guarantee:** native-Par == native-Seq == wasm, bit-for-bit — every primitive is integer / index /
total-order / min-max, none touches FP associativity. **Ship serial-wasm FIRST** (bit-identical to
native anyway; determinism is by-construction), threads behind the nightly flag later.

## Pillar 2 — portable deterministic math

**Transcendentals: adopt `libm`** (pure-Rust MUSL port), behind a single `kernel/src/mathf.rs` seam
with a CI deny-lint forbidding `f64::sin`-etc outside it. The kernel's own transcendental traffic is
TINY — fab-lang tessellates primitives and hands the kernel finished vertex soup, so facet trig is not
the kernel's job; the real surface is `rotate` (sin/cos), `offset` round-joins (sin/cos/atan2), plus
the parity set. Reuse fab-lang's degree-trig with exact-quadrant SNAPPING (`sin(180°)==0`,
`acos(-0.5)==120`) so lang-side and kernel-side are ONE math dialect ([[libm-transcendental-divergence]]).
Keep `sqrt`/`floor`/`ceil`/`round`/`trunc` as hardware `f64::` (IEEE-exact; routing through libm is a
pointless perf hit). We need REPRODUCIBILITY, not correct-rounding — reject CRlibm.

**Exact predicates: DON'T adopt one. Port Manifold's tolerance model VERBATIM.** The load-bearing
reframe from the investigation: **Manifold uses NO exact arithmetic and NO Shewchuk** — it runs plain
`f64` and buys robustness via (a) a tracked per-mesh epsilon (`EpsilonFromScale`), (b) OPERATION-
DEPENDENT symbolic perturbation of exactly-equal ties (+normal for union, −normal for difference/
intersection), (c) "never ask the same question two ways", (d) a centered-shoelace `SignedArea`
(subtract a reference vert to kill catastrophic cancellation — write it as explicit `*` and `-`,
NEVER `mul_add`). Adopting `robust`/`geometry-predicates` would be a DIFFERENT ALGORITHM that makes
different topological choices at ties — both valid manifolds, but it BREAKS the C++ bit-differential
oracle on every coplanar/degenerate case. Those crates stay in the back pocket for a hypothetical
future oracle-free kernel only.

**Determinism rules** (Rust's defaults are already safe — the job is a lint that KEEPS them safe):
never `mul_add` (FMA contracts to 1 rounding on aarch64, 2 on wasm — the biggest hazard, and Rust does
NOT auto-contract); no fast-math / `reassoc` / `fadd_fast`; never touch the FP control register
(FTZ/denormals). x87 excess-precision is a non-issue (we ship no 32-bit x86). The predicate layer
needs ZERO new deps — pure `f64` inherits determinism from these rules. native == wasm is bit-exact;
manifold-rs vs C++ is TOLERANCE-exact via the existing eps-ladder in `differ.rs` (two different
equalities, both satisfied).

## Dependencies

| Dep | Decision | Why |
| --- | --- | --- |
| **2D boolean engine** `[OPEN]` | **The linked Manifold v3.5.1 USES Clipper2** (chotchki was right; the scoping agent mis-read a newer upstream master). `cross_section.cpp` `#include "clipper2/clipper.h"`, `cliptype_of_op` → `C2::ClipType::{Union,Difference,Intersection}`, offset via `clipper.offset.h`; the build links `libClipper2.a`. CrossSection is a thin re-export of Clipper2. (`tree2d.cpp` is only `polygon.cpp`'s triangulation helper, NOT a 2D boolean.) So the real choice: **(a) PORT Clipper2** (~10–15K, bit-faithful, and ALREADY deterministic — Clipper2 works in scaled INTEGER coords, no float-assoc hazard) vs **(b) adopt `i_overlay`** (pure-Rust, integer→deterministic, far smaller) validated by AREA-residual — algorithm-independent, exactly the way minkowski is validated by volume-residual, NOT bit-differential. **Recommend (b):** 2D determinism is by integer-coords either way, so porting all of Clipper2 just to bit-match an oracle we can instead AREA-match is poor ROI. Verify i_overlay's robustness on the offset/round-join path first. |
| **libm** | Adopt (pinned) | Pillar 2. |
| **Predicates crate** | NONE | Pillar 2 — port the tolerance model. |
| **Parallel** | rayon + wasm-bindgen-rayon | Pillar 1. |
| **Vec / linalg** | glam-f64 or hand-rolled | Mechanical; must match rounding (no FMA) or the differential drifts 1 ULP. |

**RESOLVED (2026-07-14):** the linked version is `manifold-csg-sys 3.5.103` → Manifold **v3.5.1**, and
its CrossSection is **Clipper2** (verified in the built `manifold-src`, not a clone). So the 2D C++
oracle is Clipper2's output; whichever engine we pick (i_overlay recommended), it validates by 2D
area-residual against Clipper2-via-Manifold. If we ever want a bit-exact 2D oracle we'd port Clipper2
itself — the recommendation is not to.

## Phasing

Serial-only through R3 so the C++ reference stays exactly comparable; parallel integrates AFTER a
correct serial reference exists. Rough total ~25–39 engineer-weeks (~7–9 months solo) — but **R0+R1 is
the go/no-go and should be funded as its OWN decision** (below).

| Phase | Scope | ~LOC | Test gate |
| --- | --- | --- | --- |
| **R0 — infra + mesh spine** | L0 infra (vec/utils/iters/hashtable/disjoint-sets/svd/sort) + collider BVH + `Impl` (halfedge, `CreateHalfedges`, MeshGL↔Impl, `IsManifold`). Stand up the differential harness + `intermediate_checks`. NO booleans. **Spike `par::` + the rayon⟷Bevy-wasm coexistence check HERE.** | ~2.4K | **K.0:** build `Impl` from cube soup, round-trip MeshGL, `Volume_rust == Volume_cpp` on IDENTICAL buffers (the instrument-calibration that breaks invariant-circularity), pass `IsManifold`. |
| **R1 — tracer boolean (union, serial)** ★ | boolean3 + boolean_result + face_op + robust polygon.cpp. `cube ∪ cube`. Port `polygon_fuzz` + `manifold_fuzz`. | ~2.7K | **GO/NO-GO:** union of random transformed cubes is boolean-residual-clean vs C++ (<1e-5) AND passes IsManifold/Volume/genus over the structure-aware fuzzer, `polygon_fuzz` 1h ASan-clean. **If not clean, STOP — the thesis is unproven.** |
| **R2 — full robustness core** | difference/intersection (fall out of boolean3's op param) + edge_op cleanup + polygon hardening + the nasty-model corpus. | ~1.1K | **K.5 (acceptance set):** `boolean_test` + `boolean_complex_test` + all 17 nasty `.obj` (self_intersect/Havocglass/Cray) → `Status==NoError`, 0 divergence, 24h ASan campaign 0 trophies. |
| **R3 — 3D completion** | constructors (+Decompose), manifold.cpp (split/trim/slice/project), Volume/Area/Genus, csg_tree FLATTENED (~200 LOC eager — dissolves `!Send`), quickhull, minkowski, transforms, color/set_properties. | ~2.6K | fab-scad's ENTIRE `Solid` surface green vs C++ on the `models/` sweep (~55 real projects). `properties_test` = the BOSL2 color gate. |
| **R4 — deterministic parallel** | swap `par::` in for the serial reference; total-order comparators, fixed-shape reductions, deterministic ids. | ~600 | **K.D:** bit-identical Seq==Par==wasm on the full corpus, run1==run2. The pillar-1 proof C++ CAN'T pass. |
| **R5 — 2D subsystem** | 2D boolean engine (i_overlay recommended, else port Clipper2) + offset + the CrossSection wrapper + the 2D↔3D bridges (extrude/revolve caps), reusing the 3D triangulator. | ~1–2K glue (engine is a dep, not ported) | **K.6:** `cross_section_test` ported, 2D **area-residual** <1e-5 vs Clipper2-via-Manifold, offset area-by-area vs OpenSCAD (the 78.2548 canary). |
| **R6 — libm verify** | verify the libm+predicate discipline (established R0) throughout. | — | native==wasm bit-for-bit, full corpus. |
| **R.X — cut C++** | freeze `oracle_goldens.json` (vol/area/genus/bbox/status) + own byte-exact `mesh_snapshots/`, flip to golden-mode, `--no-default-features` off `kernel`. | — | suite green with C++ GONE. **The finish line.** |

## Test / oracle plan — three axes

Three relations, three tools, kept SEPARATE (C++ can gate *is-it-the-right-solid* but never
*is-it-deterministic*):
- **Oracle A — C++ differential** (semantic, tolerance, SCAFFOLD). Extend `differ::Driver` into a
  `KernelDriver` trait with `RustKernel` + `CppKernel` (manifold3d behind the `kernel` feature, skips
  cleanly when absent). Metric reused from G.3.7: **boolean-residual** `vol((A−B)∪(B−A))/vol(A) < 1e-5`
  — triangulation-INDEPENDENT, so it's immune to exactly the C++ nondeterminism that motivates the
  port. Backstop: vol/area <1e-7 rel, genus EXACT, bbox exact, component count exact.
- **Oracle B — manifold invariants** (structural, REFERENCE-FREE, PERMANENT). Port `test.h` to a
  `check` module; `intermediate_checks` calls it after every internal op (ON in test/fuzz, OFF in
  release). Survives C++ removal. Circularity broken by K.0.
- **Axis C — determinism** (bit-identity, EXACT, PERMANENT). manifold-rs vs ITSELF: run1==run2 +
  native==wasm32 CI matrix; hash vertex/index buffers by bits, scalars via `fab_lang::tier_eq`
  (NaN-is-a-class — never raw `to_bits`; a canonicalizing kernel trips `(-x)*(-x)→x*x` like the JIT did
  — [[nan-class-tier-equality]]).

**Port tests FIRST, dependency-ordered** (you can't assert `Volume==12` until Volume is trustworthy):
instruments → polygon → primitives → boolean core → CSG fuzzer (proptest fast-gate + cargo-fuzz/ASan
continuous, ONE `#[derive(Arbitrary)]` generator consumed both ways) → nasty corpus → 2D → determinism
→ cut. Seed the fuzzer from the 17 `.obj` + fab-scad's own `models/`. Kani for BOUNDED proofs only
(comparator totality, halfedge push/pop discipline, sweep termination) — never whole-boolean
([[verification-tier-tooling]]).

## Top risks

1. **Symbolic-perturbation fidelity** — the crux, untestable in isolation (only shows on degenerate/
   coplanar inputs), and a "more correct" predicate that DISAGREES with C++ is a FAILURE against the
   oracle. *De-risk:* port `polygon_fuzz`/`manifold_fuzz`/the nasty corpus FIRST — the same tests
   Manifold uses answer "did I get perturbation right". Don't "improve" the tolerance logic.
2. **The tracer (R1) doesn't come out bit-clean vs C++** → the whole thesis is unproven. *De-risk:*
   this IS the go/no-go — fund R0+R1 as a standalone spike with an explicit STOP.
3. **EdgePos-class comparators reintroduce the exact nondeterminism bug** (a partial order that "looks
   total"). Both known C++ defects are ALREADY partially fixed in the fork we port; the port COMPLETES
   them (full total order down to source index). *De-risk:* property-test every topology comparator
   for antisymmetry + transitivity + TOTALITY; assert Par==Seq bit-identical.
4. **2D version ambiguity + boolean2 port cost** (see Dependencies `[OPEN]`). *De-risk:* verify the
   linked version first; i_overlay stays the documented hedge; sequence 2D AFTER polygon.cpp lands.
5. **rayon ⟷ Bevy coexistence on wasm** (UNKNOWN — kernel pool + Bevy's wasm task pools + the Worker
   transport share one module). *De-risk:* spike in R0 week 1, gated by the kernel living in its own
   off-main-thread Worker; threaded-wasm blocked until it passes; serial-wasm ships regardless.

## The tracer bullet (the go/no-go)

**R0 + R1: the mesh spine + a serial `union`, bit-differential-clean vs C++ on the structure-aware
fuzzer.** The smallest end-to-end milestone that exercises the WHOLE approach — the halfedge `Impl`,
the hard boolean reassembly (most of the 4.4K core), the double oracle, and the libm/no-FMA discipline,
all SERIAL. It touches every load-bearing risk except parallelism (by-construction, phased later).

**"Done" =** union of random transformed cubes (`manifold_fuzz` domain, `intermediate_checks=true`,
`Status==NoError` after each op) is boolean-residual-clean vs the linked C++ kernel (<1e-5) AND passes
IsManifold + exact-genus + analytic-Volume, with `polygon_fuzz` 1h ASan-clean. If R1 is clean, the
"own the kernel" thesis is PROVEN and the remaining ~5 months are execution risk, not thesis risk. If
it isn't, we stop at R1.

## `[OPEN]` — decisions for chotchki

1. **Time-box the tracer, or fund the whole 7–9 months?** Recommend: commit only to R0+R1 (~2–3 months)
   with an explicit go/no-go, THEN decide. The bet is falsifiable cheaply — don't pre-commit the tail.
2. **Threaded-wasm nightly lock-in (`-Zbuild-std` + `+atomics`)?** Recommend serial-wasm first (bit-
   identical to native; the only cost of deferring threads is wasm PERF, zero correctness cost — no
   regression vs today's single-threaded wasm).
3. **Do we WANT `Solid: Send`?** csg_tree flattening dissolves `!Send` for free, but
   [[manifold-kernel-threading]] deliberately crosses threads with mesh data, not Solids. Take the
   simplification and revisit the threading doctrine, or keep `!Send` as a guardrail?
4. **2D: adopt `i_overlay` + area-oracle, or port Clipper2 for bit-faithfulness?** (Version check
   RESOLVED — linked v3.5.1 = Clipper2, so this is a real fork, not the earlier "boolean2".) Recommend
   i_overlay: 2D determinism is by integer-coords either way, and area-residual validates it exactly the
   way volume-residual validates minkowski — porting all of Clipper2 to bit-match is poor ROI. Verify
   i_overlay's offset/round-join robustness first.
5. **C++ retirement — DELETE, or keep it CI-only-linkable as a permanent oracle?** Freeze goldens then
   cut, vs keep `manifold3d` behind an off-by-default `oracle` feature for future regression diffs (at
   the cost of not fully dropping the C++ toolchain from CI). Which finish line?
