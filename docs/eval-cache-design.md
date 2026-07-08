# Eval-memo cache (N.2c) — design

Status: REVIEWED (v2), 2026-07-08 — a 4-lens adversarial review ran against the v1 draft; its blockers +
majors are folded in below. The N.1 profile says the interpreter is ~57% allocation; the redundancy probe
(`docs/models-profile.md`) says **82–92% of user-function calls are exact repeats** (the honest, closure-safe
ceiling — see the review note). This memoizes those calls so the repeat is a hash lookup, not a re-evaluation
— deleting the majority of the function-call eval AND the allocation riding on it. It is correctness-CRITICAL:
a wrong hit is a silently wrong geometry, so this doc exists to get the fences right before a line is written.

## Review outcome — what the v1 draft got wrong

The skeleton (Apply hook, delta purity fence, exact-`FullKey` over a hashed bucket map, per-program lifetime)
is sound; four things in the v1 draft would have produced a broken build, and — the cross-cutting theme — the
A/B differential gate is STRUCTURALLY BLIND to the wrong-hit classes (they're deterministic-wrong: bit-identical
every run, so #36 never trips; they only show as on≠off IF the corpus happens to exercise that exact shape). So
these are closed pre-build, not deferred to the gate:

- **B1 (blocker) — the key omitted the closure's captured ENV.** `Task::Apply` is the shared hook for named
  fns AND closures; a closure shares its body AST with its siblings but captures a distinct env, so
  `(body, args, $ctx)` collides `adder(1)` and `adder(2)` → wrong mesh. FIX: add `base.frame` Rc-ptr to the
  key (stable for named fns → no hit loss; distinct per capture → forced miss, never wrong). The redundancy
  probe used the same env-less key, so its first ceiling was inflated — RE-MEASURED with the env in the key:
  the ceiling drops only ~0–2 pts (82–92% floor holds), so the cache is still worth building.
- **B2 (blocker) — the naive `specials()` key IS the L.2.7 bottleneck in a cache costume.** It allocs a
  BTreeMap + ~42 String clones and walks the (deep, post-L.2.7) dynamic chain per call — O(depth), O(N²) on
  linear recursion. Benchmarking the go/no-go on THAT key would measure a slowdown and mislead. FIX: maintain
  an O(1) per-frame `$`-context id (a rolling hash / interned id updated at `call_frame` creation, read with
  zero walk) and land it BEFORE the A/B gate. (Args still hash directly — cheap for 4/5 models.)
- **Majors folded in:** (1) `parent_module`/`module_stack`-reading functions marked uncacheable by a static
  AST pre-scan at ctx-build (NOT deferred to the gate — it can't catch this). (2) The fence side-channel list
  was incomplete: evaluating a function literal pushes to `ctx.closures` (mints a `closure_id`) — snapshot
  `closures.len()` alongside `(messages, draws)` and treat growth as impure (simplest safe choice).
  (`file_needs`/`module_depth`/`module_stack`/`children_stack` are unreachable-to-WRITE from function-expr
  eval — stated, not guarded.) (3) A `Value::Function` ARG hashes/compares on `(closure_id, self_name)`, never
  `Value::==` (which has no Function arm → would never match → silently un-caches the whole higher-order
  slice). (4) Error-path safety comes from `eval_with_global`'s expression loop dropping all tasks on `?` —
  `CacheStore` is a normal task modeled on `TraceReturn` (peek `values.last()`), NOT a `geo_stack` CleanupTask
  (those run on the error path and would cache a leftover value). (5) Bounded LRU in v1, sized from the
  concentration curve (10 keys = 22–61% of calls), NOT unbounded (OOM + it cancels the drop-cost saving). (6)
  Fence-stress fixtures (seedless-rands fn, echo-recursion, `parent_module` at two depths) + run the cache-ON
  build through the CROSS-PLATFORM determinism CI, since on==off on one box doesn't prove #36.
- **Minors:** cost-weight the memo decision (skip caching a cheap body behind a 936-elem key —
  `estimated-body-cost > key-cost`); two-pass compare (store a content hash, compare before element-wise eq;
  Rc-ptr fast-path for shared list args); fixed-seed bucket hasher (run-reproducible, not `RandomState`); the
  measured redundancy is COUNT-weighted so the real net-positive ceiling sits below it (a hit prunes the body
  subtree, never the already-evaluated arg subtrees).

## Scope — what gets memoized

USER-FUNCTION calls only (not modules, not builtins). Builtins are already <1% (N.1) and some are impure
(`rands`); modules produce geometry via side-effecting instantiation, a different cache (J.5/P.2, geometry
nodes). The eval-allocation cost the probe measured is user-function evaluation, and that's the target.

The hook is `Task::Apply` — the point where a call's argument values are on the stack and its body is about to
evaluate. That's where we have everything the key needs.

## The key

A memoized result is a pure function of `(which function, its arguments, the $-vars its body reads)`. The key:

- **fn identity** — the body `Expr` pointer (`std::ptr::from_ref(body) as usize`). Stable and unique per
  definition within a run; never leaves the process, so its non-determinism across runs is irrelevant.
- **arg values** — the bound argument `Value`s in parameter order (`vals` at `Apply`).
- **reaching $-context** — the `$`-vars in scope at the call. SAFE FALLBACK: ALL reaching `$`-vars
  (`caller.specials()`), which the probe measured floors the hit-rate at 84%. The tighter key (only the
  `$`-vars the body actually reads) recovers the gap to ~96% but needs read-set analysis — a LATER refinement,
  not v1.

### Collision safety — EXACT comparison, never hash-only

A 64-bit hash over ~360 K keys has a ~1e-9 birthday collision chance — and a cache collision is not a crash, it
is a WRONG MESH that reproduces bit-for-bit every run (worse than a crash: invisible). So the cache never
trusts a hash alone. Structure: `HashMap<u64, Vec<(FullKey, Value)>>` — the `u64` buckets, and a hit requires
an EXACT `FullKey` match (fn ptr + every arg + every reaching `$`-var). Buckets are ~1 entry; the linear scan
is free.

`FullKey` equality and hashing are BIT-EXACT (`f64::to_bits`), NOT `Value`'s `==`: `+0.0`/`-0.0` are distinct
keys and `NaN` equals itself, because `f(+0)` and `f(-0)` can diverge (`1/x`). Stricter than `==` → fewer hits,
never a wrong one. Storing a `FullKey` clones the args + specials — cheap `Rc` bumps for lists/strings, `f64`
copies for numbers.

## The purity fence — only memoize a call with NO side effects

A call is cacheable only if evaluating it is observably pure. For FUNCTION eval the only mutable side-channels
are:

- **`ctx.messages`** — `echo`, plus warnings ("Ignoring unknown variable", range-cap, …). Order-sensitive
  (I.5 pins echo text vs the oracle), so a memoized call that skips a re-echo would DIVERGE.
- **`ctx.rand_stream`** — seedless `rands()` advances one global MT19937 (L.2.8c). Skip a draw and every
  subsequent draw shifts.

Detection is a DELTA, not static analysis: at an `Apply` MISS, snapshot `(messages.len(), rand_stream.draws())`;
evaluate the body; when its value lands, if BOTH are unchanged the subtree was pure → store `key → value`, else
DON'T cache (it re-runs, re-emitting its effects, every time). This is per-KEY and precise: a function that
echoes only for `arg > 5` caches its `arg = 3` key and never its `arg = 10` key. It propagates up nested
memoized calls for free — an impure inner call bumps the counter, so its (memoized) caller also sees a delta and
declines. `rand_stream` needs a monotonic `draws: u64` counter added (it has none today).

Mechanically this mirrors `TraceReturn`: push `Task::CacheStore { key, pre_msg, pre_draws }` BEFORE the body
eval so it fires (LIFO) once the result is on the stack; it peeks `values.last()`, checks the deltas, inserts.

### The one known fence GAP — `parent_module`

`parent_module(n)`/`$parent_modules` READ the module-instantiation stack without mutating it, so a function
that calls `parent_module` depends on state NOT in the key and produces NO delta — a theoretical wrong-cache.
It is near-nonexistent in functions (it's a module-introspection builtin; BOSL2's `deprecate()` uses it inside
MODULES, and `$parent_modules` is a `$`-var already IN the key). v1 accepts the gap and relies on the
differential gate below to catch it; if the corpus ever diverges, the fix is to mark functions whose subtree
references `parent_module` uncacheable (a static pre-scan).

## Lifetime, bounding

Per-PROGRAM: the cache lives in `Ctx`, dies with it. No cross-program/on-disk tier (that's later). v1 is
UNBOUNDED (cleared per program) — simplest, and the concentration (10 keys = 25–61% of calls) means a bounded
LRU would capture most of it anyway; add LRU only if per-program memory balloons (distinct keys ran 4 K–360 K;
`under_sink_guide`'s 936-elem keys are the memory watch). Bounding is a knob, not a correctness property.

## Determinism & the validation gate (the real safety net)

The cache MUST NOT change output bits. Two independent guards:

1. **A/B, on vs off.** A flag (`FAB_EVAL_CACHE=0` to disable, like M.3's `FAB_GEO_DRIVER`) runs the WHOLE
   corpus + models differential with the cache ON and OFF and asserts bit-identical output. This is the M.3
   pattern and it is the ultimate backstop: ANY fence hole (the `parent_module` gap, a missed side-channel, a
   key that's not actually complete) surfaces here as a divergence BEFORE it ships. The cache does not land
   until on == off across the whole corpus.
2. **fast==slow stays.** The cache is pure memoization of the interpreter, so the existing fast==slow proofs
   still bind; the cache is just the interpreter with a lookup table.

## The key-COST question (measure, don't pre-optimize)

Computing the key on every `Apply` means `caller.specials()` (a `BTreeMap` alloc + a walk of the — post-L.2.7,
DEEP — dynamic chain) plus hashing the args. That could eat the savings. Per chotchki's call (2026-07-08): v1
uses the NAIVE `specials()` key, then the cache's own N.1-style profile decides. IF `specials()`/key-hash shows
as a hotspot, the fix is a precomputed per-frame `$`-context identity (a rolling hash or an interned
`$`-context id maintained at frame creation, O(1) to read) and/or a lazily-cached content-hash on heavy list
`Value`s (`under_sink_guide`'s 936-elem args). Not built until the profile demands it.

## Build order (reviewed — the `$`-context id lands BEFORE the gate)

1. `RandStream::draws()` monotonic counter. ✅ (done)
2. `Scope::frame_id()` (captured-env identity for B1). ✅ (done — also now in the probe key)
3. **O(1) `$`-context id per frame (B2)** — a rolling hash / interned id maintained at `call_frame`/`bind`,
   read with zero walk. This is the valid-baseline prerequisite; the naive `specials()` key is never shipped
   OR benchmarked.
4. Static pre-scan at ctx-build: mark functions whose subtree references `parent_module`/`module_stack`
   uncacheable (major #1).
5. `eval_cache.rs`: `FullKey { body_ptr, env_ptr, arg-hash+bits, $ctx-id }` + bit-exact hash/eq + a bounded
   LRU bucket map with a fixed-seed hasher; `Cache` in `Ctx` behind the `FAB_EVAL_CACHE` flag.
6. `Task::CacheStore` (a normal `eval_with_global` task modeled on `TraceReturn` — peek `values.last()`, NOT a
   `geo_stack` CleanupTask); the `Apply` probe: miss → snapshot `(messages.len, draws, closures.len)` + push
   store task; hit → push value, skip body. Build the key from `&vals` before the bind loop consumes them;
   drop the cache `Ref` before the match.
7. Fence-stress fixtures (seedless-rands fn, echo-recursion, `parent_module` at two depths) + A/B differential:
   corpus + models, cache on == off, bit-identical, RUN THROUGH THE CROSS-PLATFORM CI. Gate.
8. Profile the cache; cost-weight the memo decision + add the two-pass content-hash compare if the key-hash
   shows.
