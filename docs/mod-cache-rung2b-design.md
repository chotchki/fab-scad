# Module-memo rung 2b: read-set-precise `$`-context keys

Status: DESIGN (BU.8, decided 2026-07-16) — builds on the shipped rung 2a
(`lang/src/eval/mod_cache.rs`, J.5.2a). Sequenced after the BU.4.6 kernel parallelization lands;
feeds the cache-default-ON decision and the BU.7 cache-leverage measurement (PLAN.md).

## The problem, in one sentence

Rung 2a keys a module call on the FULL reaching `$`-context (all ~42 BOSL2 `$`-vars, bit-exact), so
any caller that mints a `$`-var the child never reads — BOSL2's distributors set `$idx`/`$pos` per
copy in `xcopies()`/`ycopies()`/`move_copies()` — changes the key on EVERY copy and the memo misses
N times on N identical children. Measured ceiling (`mod_redundancy`): slice_parts is 99.4% redundant
in (module, params) but only ~42% with the full-`$`-ctx key. The ~57 points in between are exactly
the vars-minted-but-never-read gap; rung 2b's job is to key on what the call actually READS.

## The invariant (chotchki, 2026-07-16)

Read sets propagate LEAVES-UP with writes as masks:

```
escaping_reads(node) = (⋃ escaping_reads(children) − $-binds(node)) ∪ own_reads(node)
```

A read of `$x` escapes a call iff it resolves ABOVE the call's entry context; a `$x = …` bind
between the read and the boundary kills it. One OpenSCAD-specific sharpening: `$`-assignments are
scope-hoisted (last-assignment-wins, I.2.7), so the kill is SCOPE-LEVEL ("is `$x` bound anywhere in
this scope"), not sequential dataflow — simpler than the classic ordered gen/kill.

## Two substrates for the walk — verdict: dynamic first

**Static (AST leaves-up at load):** compute per-module escaping-read sets once, with a call-graph
fixpoint. Sound by over-approximation (a conditional read counts as a read — a wider key is never a
wrong hit, it just misses more). The costs are all real work before the first improved hit:
- a `$`-read table for every BUILTIN (`circle()` reads `$fn`/`$fa`/`$fs` INSIDE the evaluator —
  no AST mentions them), curated forever as builtins land;
- the fixpoint over recursion + modules called through function values;
- `children()` forces conservatism at definitions (the 2a fence sidesteps it, but any wider rung
  reopens it).

**Dynamic (capture at first execution, verify on probe):** instrument the ONE choke point every
`$`-read already flows through — `Scope::lookup_opt`'s `$`-branch walking the `dynamic_parent`
chain (`lang/src/eval/scope.rs`) — and record, per cached call, which `$`-vars resolved ABOVE the
call's entry frame. Complete BY CONSTRUCTION (no builtin table: the builtin's own lookup goes
through the same chain), exact per-trace, and it self-maintains as BOSL2 evolves.

Dynamic wins the first implementation: the choke point makes completeness free, the purity fences
it needs already exist (below), and the recorded sets are tiny in practice (`$fn`/`$fa`/`$fs`,
occasionally `$idx`). The static walk keeps a SECOND life later as a prefilter (a
statically-`$`-clean module skips tracking entirely) and as an EXPLAIN artifact — per-module read
sets inspectable next to the intrinsic plan (O.3). Not built now.

## The dynamic design

### Soundness argument (why observed reads are enough)

The classic incremental-computation argument (Adapton/salsa lineage): every branch decision inside
the call is ITSELF a read. If a later context agrees bit-exactly on every recorded (var, value)
pair, the evaluation provably takes the SAME trace — same branches, same reads, same `Geo`. An
observed read set is only unsound if some influence bypasses recording; the fences below close each
bypass class.

### Capture stack — the leaves-up walk, performed at runtime

Maintain a stack of ACTIVE captures (one per cached-call frame currently evaluating), each holding
its entry-frame identity. When `lookup_opt` resolves a `$`-name, it already walks child→parent —
record `(name, value_bits)` into EVERY active capture whose entry frame the walk crossed before
resolving. That single rule implements the whole invariant:

- **gen:** the deepest call records the read, and so does every enclosing capture whose boundary
  the resolution escaped — the reads propagate up.
- **kill:** a `$`-bind inside a call means later reads resolve BELOW that boundary — the walk never
  crosses, nothing is recorded. The scope chain masks writes with zero extra machinery.
- **hit-merge:** when an INNER cached call HITS, its body never executes, but its stored read set
  must still propagate — replay each (name, value) of the hit entry as a synthetic resolution from
  the current scope, recording into enclosing captures exactly as a real read would. (The values
  are the ones the probe just verified, so the replay is exact.)

The boundary test is cheap: the walk compares each crossed frame against the capture stack's entry
frames (`Rc` pointer identity) — and it only runs on the `$`-branch of `lookup_opt`, a handful of
frames deep (the specials split from L.2.7 keeps that chain short).

### Entry shape + probe

Per 2a base key (body ptr, home frame, params bit-exact) — the `$`-ctx content LEAVES the key —
store a small vec of entries:

```
(read_set: Vec<(Rc<str>, ValueBits)>, geo: Geo)
```

- **Probe:** for each entry, resolve each recorded name in the CURRENT scope and compare bits;
  first full match wins. Cost O(entries × |read_set|), both small; cap entries per base key
  (branch-divergent traces produce a few entries, a runaway produces eviction, not growth).
- **Fast path:** rung 2a's `dyn_ctx` pointer identity (`Scope::dyn_ctx`, minted fresh on every
  `$`-bind) short-circuits verification — `Rc::ptr_eq` on the stored ctx means the reaching context
  is IDENTICAL by construction, skip the per-var compare.
- **Absence is a value:** `lookup_opt` distinguishes UNBOUND from bound-to-`undef` (the OpenSCAD
  warning split). The read set records UNBOUND as its own marker — a context that later BINDS the
  var must miss.

### What stays from rung 2a (the fences)

- **`$children == 0`** — unchanged. 2b widens KEY precision, not the children hazard; a call
  rendering call-site children still isn't a pure function of any key we can build here.
- **Purity snapshot** — unchanged, shared with the N.2c eval cache: store only if
  (messages.len, rand_stream.draws, closures.len, impure_reads) moved nothing (echo/assert,
  seedless `rands`, `parent_module` all fenced transitively).
- **Bit-exact values** — the shared `hash_value_bits`/`value_bits_eq` walker (`+0 ≠ -0`,
  `NaN == NaN`).

### Read-path inventory (implementation gate)

Completeness rests on EVERY dynamic-chain read routing through recording. Known readers to audit at
implementation: `lookup_opt`'s `$`-branch, `special_f64s` ($fn/$fa/$fs resolution — walks the chain
itself), and each `specials()` caller (it reads EVERYTHING — inside a capture that's a
record-the-world event; today's callers are the 2a key builder and the `eval_children` `$`-overlay
(L.2.8p), both outside the fenced body, but the audit must prove that stays true). A chain-reader
added later without recording is the ONE way this design silently breaks — the audit list lives in
the module doc and the differential (below) is the tripwire.

## Costs + the off-switch

Recording is a set-insert per ESCAPING `$`-read (most reads resolve locally and record nothing);
probing is a few bit-compares. Same posture as N.2c: bit-identical to cache-off by construction,
A/B differential as the gate, and the program-level auto-off (N.2c.2.2) reusable unchanged if a
low-redundancy model turns probe cost net-negative. Default-ON still ALSO waits on N.2c.3 (the
deep-recursion probe-cost pathology) — that is a cost-of-missing problem, orthogonal to this
hit-rate work, and it is NOT fixed here.

## Validation

- **Correctness:** the J.5.3 cache-on==off differential (bit-compare, both orders) over the full
  corpus (901) + the models tree; the fuzz eval target runs with the cache forced on for a soak.
- **Hit rate:** re-run `mod_redundancy` — target is the measured ceiling (~99% in (module, params)
  on slice_parts, from 42%).
- **The motivating case:** an xcopies microbench — `xcopies(n=50) screw_hole(...)` must evaluate
  the child ONCE (49 hits), verified by cache counters; same for `ycopies`/`grid_copies`.
- **Perf:** slice_parts + under_sink_guide wall time, cache on vs off, plus the BU.6 harness delta
  report for the models tree.

## Out of scope (and where it lives)

- Kernel-level `GeoNode → Solid` caching (P.2) — keys the child BELOW the `Transform` node, so the
  transform variation xcopies produces is handled structurally there; this doc is the evaluator
  layer only.
- CSG optimizer rungs (canonicalization, union reassociation, CSE past Rc-sharing) — BU.7's
  layer-decomposed redundancy measurement decides whether the residual after 2b + P.2 justifies
  building any of it.
- Widening past the `$children == 0` fence (rung 2c, if ever) — needs a children-identity key
  component; not designed.
