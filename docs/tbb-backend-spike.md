# W.3.9 spike — provide our own Manifold parallelism backend?

**Verdict: DON'T build the TBB shim. The motivation (determinism) is unreachable by a parallelism
backend, and the remaining wins (drop the oneTBB dep) don't justify the real cost. Status quo holds —
native oneTBB, single-threaded wasm. The one actionable finding is a Manifold-core comparator bug worth
an upstream PR; `manifold-rs` (own the kernel) remains the only path to the prizes the shim can't buy.**

Researched 2026-07-13 via a 6-agent fan-out → synthesis → adversarial verify, all source-grounded
against `target/release/build/manifold-csg-sys-*/out/manifold-src` + `manifold-csg-sys-3.5.103`.

## The premise it overturned

We went in believing "own the TBB backend" buys DETERMINISM (bit-identical output → unblocks the R.2
geometry differential) + one-impl + drop-the-dep. The determinism half is a **mirage**, for two
independently-verified reasons:

1. **R.2's cross-platform gap is 100% floating point, not parallelism.** Native Par already equals Seq
   bit-for-bit — not luck: every geometry-path reduction uses order-INDEPENDENT operators (`la::min/max`,
   bool-and, integer sum; `properties.cpp:356-392`), and the FP-associativity-sensitive sums
   (Volume/SurfaceArea) are a *serial* Kahan loop (`properties.cpp:283-292`) whose scalar output never
   feeds back into vertices. The native↔wasm delta that actually blocks R.2 is transcendental libm
   (~1 ULP, [[libm-transcendental-divergence]]) + predicate rounding (manifold#666). No backend touches
   that. **R.2 = the libm-crate swap + robust predicates, full stop.**
2. **The same-platform residual (garage_door: 3 STL hashes, identical volume+genus) is a Manifold-core
   comparator bug a fixed-order backend cannot reach.** `boolean_result.cpp:263` runs a `parallel_for`
   whose threads concurrently `push_back` into a `concurrent_map`; the per-key vectors are then
   `std::stable_sort`-ed with `EdgePos::operator<` (`boolean_result.cpp:197-200`) — which tiebreaks only
   on `(edgePos, collisionId)` and **ignores `vert`/`isStart`, so it is NOT a strict total order.**
   `stable_sort` then preserves the *concurrent-append race order* for equal keys. Coplanar contact
   (a garage door in a frame) makes many ties → byte-different edge pairing, same volume+genus. A
   fixed-CHUNK backend still races the intra-key appends. Fixing it needs either serializing the boolean
   kernel (defeats the point) or **patching the comparator (a core edit that breaks the zero-patch
   promise).** Secondary unreachable source: `meshIDCounter_` is a process-global `atomic` (`impl.cpp:91`)
   whose assignment order across our coarse shard-pool ops is nondeterministic.

**Corollary — the tempting cheap experiment is confounded.** Running garage_door under `MANIFOLD_PAR=OFF`
makes the append serial+ordered → it goes deterministic → reads as "fixed-order parallel fixes it." It
does NOT. The decisive test is to **patch `EdgePos::operator<` to a total order on the existing
oneTBB-Par build and re-hash** — if that stabilizes it, the comparator IS the cause (→ upstream PR),
not the backend.

## How nasty is the shim, really

Cheaper to inject than feared, but bigger + subtler to implement than the "≈300-LOC namespace shim"
first suggested.

- **Injection is clean (no Manifold patch).** `cmake/manifoldDeps.cmake:41-84` already has a
  non-builtin-TBB branch: build `-DMANIFOLD_PAR=ON -DMANIFOLD_USE_BUILTIN_TBB=OFF` + a fake
  `TBBConfig.cmake` pointing at our shim's headers/lib. Only the *sys crate's* `build/build.rs:60-62`
  (which forces builtin TBB) changes — our wrapper, low churn. A drop-in `tbb::` NAMESPACE shim
  (keep `MANIFOLD_PAR=1`) covers the 8 raw `tbb::` leak sites for free; `MANIFOLD_PAR==2` is rejected
  (`utils.h:25/31` hard-`#error`s anything but 1/-1, and the leaks aren't routed through parallel.h).
- **But the surface is a real C++ library, not a trivial shim:**
  - **A working concurrent ordered map is MANDATORY, not omittable.** `boolean_result.cpp:249-250` gates
    the *entire parallel boolean intersection path* (the hottest CSG kernel) on
    `__has_include(<tbb/concurrent_map.h>)` — omit it and the hot kernel *silently runs sequential*. So
    the shim must ship `concurrent_map` + `concurrent_unordered_map` — concurrent-mutation containers,
    exactly the hard class. (Plus a `face_op.cpp:284` `__has_include(<tbb/tbb.h>)` trap that may force one
    tiny patch.)
  - **The pool must be work-stealing / nesting-safe, or it DEADLOCKS.** `this_task_arena::isolate` at 11
    sites exists because Manifold *nests* parallel regions; a naive fixed-thread blocking-join pool blocks
    all N threads in an outer join with none left for the inner `parallel_for`. TBB gives nested
    composability + work-stealing for free; a hand-rolled pool re-earns both.
  - The easy 80%: `parallel_invoke`/`task_group` (= fork-join), `blocked_range`/`split`, `combinable`
    (Vec by stable worker-slot), `parallel_for`, the arena/isolate/affinity stubs. `parallel_scan`'s
    2-pass Body protocol is the one fiddly-but-bounded primitive (determinism free — all scan operands
    are integer, so any decomposition == serial exactly).
- **wasm ≈ adopting oneTBB-wasm anyway, plus a strategic regression.** Fine-grained wasm parallelism
  needs SharedArrayBuffer + `+atomics,+bulk-memory` (our message-passing Worker transport shares *nothing*
  — separate address spaces). "Provide our own" only moves thread-*spawn* into Rust; you still recompile
  manifold + Clipper2 + libc++ shared-memory-consistent with a locked allocator — the emscripten-pthreads
  machinery. And threads are scoped OUT of wasm-cxx-shim (maintainer points at emscripten), so the wasm
  arm re-drags the emsdk toolchain we deliberately rejected for wasm32-unknown-unknown/Bevy. COOP/COEP is
  already done; that's not the hard part.

## Decision — shim vs. own it (the deciding factor)

The shim loses both ways: **nastier than hoped** (a concurrent-mutation map + a work-stealing nesting-safe
pool, not a namespace veneer) **and buys less** (determinism struck entirely; no LTO — still C++ across an
FFI barrier; wasm ≈ oneTBB-wasm + an emsdk regression). Its only surviving win is dropping the oneTBB dep
on native — which doesn't justify owning a concurrent map + scheduler.

`manifold-rs` (own the kernel in Rust, [[manifold-rs-feasibility]]) is the only path to the prizes the
shim can't deliver: a total-order comparator *we* control (real determinism), a deterministic nesting-safe
pool, whole-pipeline LTO, leaving C++/emsdk entirely. This spike is its recon — the primitive map + the
`boolean_result.cpp:197` comparator finding feed it directly.

## Actions

1. **Don't build the TBB shim.** Strike "determinism" and "unblocks R.2" from its rationale entirely.
2. **Free upstream win:** the garage_door nondeterminism has a named cause (`EdgePos::operator<`
   non-total-order). Confirm via the comparator patch on the real Par build (NOT PAR=OFF — confounded),
   then file a Manifold PR (a total-order tiebreak). Fixes it for everyone, no backend, no ownership.
3. **R.2** stays its own thing: libm crate + robust predicates.
4. **Status quo:** native oneTBB (works, fast), single-threaded wasm (Manifold's own recommendation — the
   parallel-wasm config is README-flagged for "potential memory corruption").
5. **`manifold-rs`** carries the north-star payoff; greenlight per its backlog entry.

_(One research agent — the ffi/build-seam dimension — failed its schema retry cap; its ground was
covered by the synthesis's injection-point analysis + the adversarial verify's Attack 5, both of which
independently reached the clean `-DMANIFOLD_USE_BUILTIN_TBB=OFF` finding.)_
