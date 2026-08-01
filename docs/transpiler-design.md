# AR: the build-time transpiler — what it must be, and what would kill it

Status: DESIGN, nothing built. Written 2026-07-26 against the AO measurements.

The bet: compile a set of OpenSCAD libraries to Rust at fab-scad BUILD time, and retire both the
hand-written intrinsics and the Cranelift JIT into it. This doc is the case for and against, the
constraints that are already known (they are not guesses — every one is a bug we already shipped and
fixed), and the acceptance contract.

## Why, honestly

Two justifications survive scrutiny, and one popular one does not.

**1. Tier collapse — the real prize.** Today: interpreter everywhere, intrinsics everywhere, JIT on
desktop ONLY, because the browser cannot JIT in-sandbox. That asymmetry is architectural, not a
tuning problem: the web build is permanently on the slow tier. AOT compilation erases it, because the
compiling already happened. wasm gets the fast tier for free. Nothing else on the roadmap does that.

**2. Deleting the intrinsics.** ~55 hand-written Rust functions, each pinned to a BOSL2 definition by
an AST fingerprint, each needing a guard for every way the library could shadow or rebind what it
depends on. Phase AN is a list of the ways that goes wrong. A transpiler GENERATES that layer from
the library source, so it stops being maintained by hand.

**3. Raw desktop speed — DO NOT lead with this.** L.4 measured the JIT at 34-334x per CALL and
NET-NEUTRAL-TO-SLOWER on real models (corner_brace 252→277ms), because geometry dominated and the
numeric tail was light. A transpiler that only makes eval faster inherits that ceiling exactly. See
"The ceiling" below — the honest answer is model-dependent and sometimes tiny.

## The ceiling (AO.14, measured)

A transpiler acts on EVAL. It cannot touch the geometry kernel. So the eval fraction of a render is a
HARD upper bound on what it can win, per model. `fab render --engine scad-rs` now reports the split.

Measured on real models — the spread is the finding, not any single number:

| model | total | eval | geo | eval share |
|---|---|---|---|---|
| bowtie | 62 ms | 61 | 0 | 98% |
| ams_stackfix | 95 ms | 85 | 10 | 89% |
| ashtray | 778 ms | 138 | 639 | 18% |

So "how much would a transpiler buy" has no single answer. On an eval-bound model a perfect
transpiler approaches a 10x whole-render win; on `ashtray` it is capped at 18% NO MATTER WHAT. Any
pitch that quotes one model is cherry-picking, in either direction.

**I raised a caveat here and then tested it, and it did not hold.** The worry was that `eval` is not
all interpretation: `GeoNode::Leaf(Mesh)` is documented as "a tessellated primitive", so fab-lang
tessellates during eval and hands Manifold a mesh. If tessellation were a large part of `eval`, the
column would overstate AR's ceiling badly, since tessellation is already compiled Rust doing trig.

Two synthetic A/Bs settle it. Hold interpretation fixed, vary facets:

| `$fn` | eval | geo |
|---|---|---|
| 8 | 3 ms | 3 ms |
| 32 | 1 ms | 309 ms |
| 128 | 8 ms | 7025 ms |
| 512 | 34 ms | 21735 ms |

Hold geometry fixed (one sphere), vary interpretation:

| calls | eval | geo |
|---|---|---|
| 100 | 1 ms | 0 |
| 1 000 | 3 ms | 0 |
| 10 000 | 16 ms | 0 |
| 100 000 | 136 ms | 0 |

`eval` is LINEAR in call count (1 → 136 ms over 1000x) and nearly flat in facet count (3 → 34 ms over
64x), while `geo` is where facets land (3 → 21735 ms). Tessellation is indeed inside `eval` and it is
CHEAP — roughly 31 ms for ~5.2M triangles. So for any model that is not pathologically
high-facet-low-logic, **`eval` is a fair proxy for the interpretation work a transpiler addresses**,
and the ceiling table above can be read at face value.

Recording the failed caveat rather than deleting it: the concern was legitimate, the code comment
that prompted it was accurate, and the conclusion was still wrong. Cheap to test, so it got tested.

### The corpus distribution (50 models)

    n=50   min 0%   p25 23%   MEDIAN 60%   p75 90%   max 100%   mean 55%
    28 of 50 models are >50% eval

Against the rule registered in advance — median >50% means the speed case stands on its own — the
answer is **YES at 60%**, but not overwhelmingly, and the spread matters more than the median. Over
half the corpus is eval-dominated and would see most of a transpiler's win; a quarter sits at 23% or
below and would see almost none. `desk_design` is 0% eval; `bowtie` is 100%.

The defensible claim is therefore: **a transpiler addresses the majority of render time on the
majority of models, and is near-useless on a substantial minority.** That is a real case, and it is
not the "10x everywhere" a headline would prefer.

## Why the generated corpus is the WRONG instrument here

AO's heavy generated lane shows our lead SHRINKING with the dial (4.13x → 1.20x → 1.08x at dials
2/8/16) while real BOSL2 models show it GROWING with size (2.6x → 3.9x → 10.7x). Both numbers are
now trusted; they disagree because they measure different halves. Generated programs are
geometry-primitive-heavy and eval-light; BOSL2 builds geometry THROUGH the language.

Consequence for AR: do not evaluate the transpiler on the generated corpus as it stands, and do not
"make the geometry bigger" to strengthen it — that scales the half a transpiler cannot address and
dilutes the half it can. AR needs an EVAL-weighted generator, which is AR.3's surface (below).

## The constraints are already known — every one is a shipped bug

A transpiler is a compiler, and it freezes MORE ahead of time than the JIT did. Every AN finding is
therefore a requirement, not history. The unifying statement: **the compiler must read the same
program the interpreter reads.**

| AN | what moved at runtime that the compiler had frozen |
|---|---|
| AN.10 | a PARAMETER shadows a dep's name and redirects that call; the native had resolved it statically |
| AN.11 / AN.17 | a dep's constant must be read from the DEP's island, not the caller's — and the guard has to REACH entries whose only constants come from a dep (it did not, for 11 of them) |
| AN.5 | a root assignment must NOT override a `use`d library's own constant inside that library |
| AN.1 | a duplicate parameter name resolves first-declared, not last |
| AN.2 | a positional arg takes the lowest UNFILLED slot, not the next counter value |
| AN.3 | an unfilled defaultless param is `undef`, and must not fall through to a like-named global |

AN.11/AN.17 is the cautionary one: the check was correct and its PLACEMENT was wrong, so it was
unreachable for exactly the entries that needed it, and it shipped as "sound by construction" for
weeks. A transpiler will have more such surfaces, not fewer.

## The acceptance contract — RETARGET, do not delete

When the JIT goes, its test suite must not go with it. Every one of these asserts "the compiled tier
agrees with the interpreter", which is precisely the transpiler's contract:

- `fast_eq_jit` — tier equality on values
- `jit_diff` — fuzzed differential
- `jit_dispatch_diff` — DISPATCH-level, the shape that catches the AN.1-AN.3 binding family. A
  positional-binding harness structurally cannot see those; this one can
- `corpus_diff` — the BOSL2 corpus at the pin

Plus doctrine #36's NaN carve-out: compare with `fab_lang::tier_eq`, never raw `to_bits()`.

## AR.3: the library surface (the piece everything else needs)

Today the generator picks from `const BUILTINS: &[(&str, usize)]` — name and ARITY only. That cannot
describe a library, so it cannot fuzz one, and it cannot generate a named-argument call at all. On
builtins that would not matter much (names are ignored there — see the sketch below), but BOSL2 is
USER-defined code, where names DO bind and where the whole AN.14 diagnostic family lives.

What is needed is a declaration carrying, per function/module: its name, and per parameter a NAME and
a DOMAIN (number / vector / path / VNF / region / bool). Three consumers, ONE declaration:

1. the advanced fuzzer (AO) — generates calls that type-check and therefore do work
2. transpiled-library fuzzing (AR.1) — the same generator, pointed at a transpiled library
3. the dispatch registry itself — `intrinsics::Entry {name, reference, consts, consts_v, deps,
   builtins, func}` is ALREADY a hand-maintained surface manifest of this kind. The transpiler should
   GENERATE it.

DOMAINS are load-bearing, not decoration. A call with wrong-typed arguments returns `undef` almost
immediately: it costs nothing, renders nothing, times as ~0. A surface carrying only (name, arity)
produces a corpus that measures ERROR HANDLING while looking like it measures geometry — and the
failure is invisible, because the programs still run, still agree with the oracle, and still report a
ratio. That is the same shape as every blind-channel bug this project has hit: not wrong,
unfalsifiable.

## What would kill this bet

State these now, so the answer is not negotiated later:

- **the corpus median eval share is small.** Then the speed pitch is dead and AR must be argued on
  tier-collapse + maintenance alone. It might still be worth it; it must not be SOLD on speed.
  ANSWERED, 50 models: median 60%, so the speed case CLEARS the bar that was set in advance — but
  only just, and a quarter of the corpus sits at 23% or below. This bullet is retired as a killer and
  demoted to a scoping constraint: AR is a majority-of-models win, not a universal one.
- **transpiled output cannot be proven equal to the interpreter.** The AN family says equality is
  subtle and the failure mode is silent. If the retargeted suites cannot be made to pass, the tier is
  not shippable at any speed — a tier whose contract is "agrees with the interpreter" cannot have a
  case where it silently does not.
- **build-time cost is unacceptable.** Compiling libraries at fab-scad build time lands on every
  contributor's `cargo build`. Fat LTO already roughly doubles link time (P.1.5.1) and was therefore
  confined to `[profile.dist]`; a transpiler pass has the same politics and needs the same answer up
  front.

## AR.3 sketch: two manifests, one source read

`intrinsics::Entry` is already a surface declaration — it just answers a different question than the
fuzzer asks:

- **`Entry` is the DISPATCH manifest**: may this native implementation be used here? Hence `reference`
  (fingerprinted), `consts`/`consts_v` (the guard), `deps`, `builtins` (shadowing).
- **What AR.3 adds is the CALL manifest**: how do I construct a call that WORKS? That needs the one
  thing `Entry` has no reason to carry — parameter names and their domains.

A transpiler reads the library once and can emit both. Today the first is hand-written and the second
does not exist; the generator makes do with `(&str, usize)`.

**Names bind for USER functions and are IGNORED for builtins — verified, not assumed.** This shapes
the whole design, so it was tested against the oracle first:

```openscad
pow(exp = 3, base = 2)  ->  9     // NOT 8: names ignored, bound positionally (3^2, not 2^3)
sin(bogus = 30)         ->  0.5   // a nonsense parameter name is accepted and bound positionally
pow(2, exp = 3)         ->  8     // mixed positional + named
```

fab reproduces all three exactly. So a builtin's parameter names are DECORATIVE: emitting them is
still worth doing (it checks that we ignore them the same way upstream does — `pow(exp=3, base=2)`
returning 9 is a conformance fact worth a corpus case), but no AN.14 diagnostic can fire there.
Named-argument BINDING, and therefore the whole AN.1/AN.2/AN.3/AN.14 family, exists only on
user-defined functions and modules — which is what BOSL2 is. The surface must mark which it is, or a
generator will happily produce named builtin calls, see them bind positionally, and conclude the
named-arg path is covered when it has never been exercised.

```rust
/// One callable a library hosts, as DECLARED rather than rediscovered by every consumer.
pub struct Decl {
    pub name:   &'static str,
    pub kind:   Kind,               // Function | Module — modules take children, functions do not
    /// Do parameter NAMES bind? False for builtins (verified above), true for user/library
    /// functions. The AN.14 diagnostic family is unreachable when this is false.
    pub names_bind: bool,
    pub params: &'static [Param],
}

pub struct Param {
    /// Load-bearing: without it a corpus can only make POSITIONAL calls, and AN.14's whole
    /// diagnostic family (`argument "a" supplied more than once`, `variable "x" not specified as
    /// parameter`) is unreachable by construction.
    pub name:     &'static str,
    pub domain:   Domain,
    /// Defaultless params are AN.3's case: unfilled must be `undef`, and must NOT fall through to a
    /// like-named global. A generator that never omits an argument cannot catch that regression.
    pub required: bool,
}

pub enum Domain { Num, Bool, Str, Vec2, Vec3, VecN, Path, Region, Vnf, Any }

pub trait LibrarySurface {
    fn decls(&self) -> &'static [Decl];
}
```

What the generator does with it, and why each matters:

- pick a `Decl`, fill each `Param` from its `Domain` → the call actually computes something. This is
  the AR.4 trap made structural: wrong-typed args return `undef` in ~0 time, so a domain-blind corpus
  measures error handling while looking like it measures work.
- sometimes pass by NAME, sometimes positionally, sometimes MIX → reaches AN.2 (positional takes the
  lowest unfilled slot, not the next counter). Only meaningful where `names_bind`; on a builtin the
  same call shape instead checks that we ignore the name exactly as upstream does.
- sometimes pass the same name TWICE, sometimes omit a `required` one → reaches AN.14 and AN.3.
- `Kind::Module` gets children generated; `Kind::Function` does not.

Sequencing note: builtins become surface #1 by rewriting `const BUILTINS: &[(&str, usize)]` as
`&[Decl]`, which is mechanical and immediately upgrades today's generator. A transpiled library is
then surface #2 with NO new generation machinery — same trait, same call builder. That is the whole
argument for declaring the surface before writing the transpiler rather than after: do it in the
other order and the call generator gets written twice.

### What shipped (AR.3/AR.4, `gen/src/lib.rs`) — three deltas from the sketch above

1. **`Decl` grew a `ret: Domain`.** Without a return domain every argument bottoms out at a literal
   after one hop; with it, calls COMPOSE (`asin(sin(…))` — `sin` declares `ret: Unit`, `asin` wants
   `Unit`), and composition is where eval work comes from. The scalar domains nest one way
   (`Unit`/`Pos`/`Deg` stand in for `Num`, any scalar is a usable angle), never the other.
2. **Domains are GENERATION domains, not acceptance claims.** They answer "what argument makes this
   call compute", which is NARROWER than what the builtin tolerates: `len` accepts anything and
   measures only sized values, so it declares `VecN`; `asin` accepts any number and is NaN outside
   `[-1, 1]`, so it declares `Unit`; `search`'s key is pinned to `Num` because a string key over a
   non-string column ABORTS the oracle (`docs/openscad-search-crash.md`) — a pin that outlives
   nothing: cheap's arbitrary arguments still cover the whole mismatch space, in the lane built to
   handle crashes. The shipped enum is the builtin subset (`Num Pos Unit Deg Bool Str Vec3 VecN List
   Table Any`); BOSL2's `Path`/`Region`/`Vnf` arrive with the transpiled surface.
3. **The contract is a TEST, not a convention:** `every_declared_call_computes` generates every
   decl's call from its domains and asserts the result is never `undef` — the "did no work" tell.
   If a decl's domains drift into uselessness, that fails by name instead of the heavy lane silently
   going back to measuring error handling.

---

# The shape, settled 2026-07-28

Everything above was written before the transpiler had ever been pointed at a library. It has now,
and four things changed — three because chotchki decided them, one because measuring beat guessing.

## Measure first: 47.3% of BOSL2 already compiled

Before any of the design below, the v0 emitter was run over every top-level function in the pinned
BOSL2. **632 of 1335 compiled** — with no baked constants and no widening at all. String literals
(`Value::Str` was simply never emittable) took that to **742, or 55.5%**, in about an hour of work.

That number reframes the phase. The remaining half is not open-ended research, it is four named
bands with counts attached:

| declines | band |
|---|---|
| 375 | unbaked library constants — `_EPSILON` 131, `UP` 106, `CENTER` 70, `_NO_ARG` 43 |
| 73 | computed callee (a function value in call position) |
| 73 | non-contiguous named args to a sibling |
| 40 | function literals + call-through-a-local-binding |
| ~20 | C-style comprehension, `echo`, and a tail |

The histogram is FIRST-DECLINE-WINS, so every count is a lower bound — clearing a band re-runs its
survivors against whatever they hit next, which is why the constant band GREW when strings landed.

It is a ratchet, not a report (`bosl2_codegen_coverage_holds_its_floor`). An emitter that quietly
stops handling a construct does not fail; it falls back to interpreting, and every other test still
passes. Only a number that must not drop can see that. Same reasoning as AR.3.3's domain floor.

## The transpiler is a proc macro; the library crate is its output

chotchki: *"I'm kinda expecting fab-lib-bosl2 to be a huge proc macro when all is said and done"* —
then, clarifying — *"errr the transpiler to be a huge proc macro, fab-lib-bosl2 will be the output of
a huge `fab_transpile!("BOSL2/std.scad")`"*, and *"it may need to be an array"*.

So `fab-lib-bosl2` is a crate whose entire content is one macro invocation over an array of ROOTS:

```rust
fab_transpile!(["BOSL2/std.scad", "BOSL2/gears.scad"]);
```

This costs nothing to keep open **provided `fab-lib` stays a pure function from library-source to
Rust-source, with no opinion on delivery**. Then checked-in generation and a proc macro are the same
transpiler with a different consumer, and switching between them is not a rewrite. Ship checked-in
first (reviewable, no build-time transpiler dependency), convert once the emitter stops moving.

The reviewability objection against a macro turns out not to bite: the diff worth reading when BOSL2
bumps is the SUBMODULE's, and the regression that matters — functions falling out of coverage — is
caught by the ratchet, which is a test rather than an artifact.

What does gate the conversion, and must be measured rather than argued, is **expansion cost**.
Parsing 85K lines of OpenSCAD and handing rustc ~1.7 MB of tokens would run on every clean build of
every consumer, where checked-in output costs only the rustc half. Second wrinkle: a proc macro is
not re-run when a file it reads changes unless that file is `include_str!`d, so the macro needs an
explicit list of the 56 `.scad` files rather than a glob — which is fine, and arguably better, since
a silently-dropped library file is the failure mode where a missing input costs a PART, not an error.

## A root is not a directory, and `include` means far more than one file

The first library read globbed `*.scad` from a directory. That describes a program nobody has.

`std.scad` reaches **30 of BOSL2's 56 files**; gears, screws, threading, nurbs and the rest are
OPT-IN roots a user includes separately. And BOSL2's opt-in files do not include `std.scad` back —
`gears.scad` has no includes at all, it simply assumes std is already there. So the closures
**compose rather than nest**, which is exactly why the macro takes an array.

Measured, both units kept:

| unit | functions | constants | files |
|---|---|---|---|
| `std.scad` closure | 934 | 106 | 32 |
| `gears.scad` closure | 39 | 0 | 1 |
| whole directory | 1329 | 169 | 56 |

`include` versus `use` is honored: `include` splices a file whole, `use` imports its modules and
functions and deliberately NOT its variables. Getting that backwards would let the constant band bake
a value the consumer's program never binds — the native would answer with a number where the
interpreter says `undef`.

## The registry carries PROVENANCE

chotchki: *"the registry needs to know what root emitted what, so when a downstream consumer includes
it, it gets what it asked for, which make include more than just itself"*.

So a declaration is not just `name → decl`. Each ROOT keeps its own closure, keyed by the path a
consumer writes in its `include` line. This is not bookkeeping — it is what makes the surface
answerable, and it is wrong in both directions without it. A generated program that includes only
`std.scad` and calls `spur_gear` is broken; one restricted to the handful of names `std.scad` itself
declares misses 934 functions it should be exercising. And a missing function costs a silently-absent
PART rather than an error, which is the hardest failure to notice.

## One trait, three kinds of declaration

chotchki: *"the fab-lib-bosl2 library should be exporting the implementation of a library surface
trait that includes constants/functions/modules, that trait is what the fuzzer/fab-gui/etc should be
consuming"*.

Today there are THREE overlapping descriptions of the same thing, maintained separately:

- `fab_gen::Decl` — the fuzzer's view, already carrying `Kind::{Function, Module}`
- `fab_lang::SurfaceFn` — derived from references at runtime, then `Box::leak`ed to get `'static`
- `intrinsics::Entry` — the dispatch view

These collapse into one trait in **fab-lang**, because every other crate already depends on it and a
generated library crate must not depend on the fuzzer. The runtime derivation and its leak both
disappear: a generated library declares itself at build time instead of being read back at startup.

Two lists on the trait, deliberately NOT one list with an `Option`:

- `callables()` — pure DECLARATION: name, kind, params with names, domains and required-ness
- `natives()` — IMPLEMENTATION: reference, fingerprint, guard sets, function pointer

They are different lengths on purpose. BOSL2 declares 1335 functions and we compile 742. Folding them
together would assert those are the same set, which is precisely the drift this phase exists to kill.

> AS BUILT (AR.26.1, 2026-07-31): the second method is `rows()`, returning `registry::Rows` — the row
> sets a consumer accumulates into a `registry::Registry`. And a row carries its VERBATIM REFERENCE,
> not a fingerprint: the registry parses it and hashes it with our own parser, so the gate hash is
> never asserted by the row author. That is the difference between trusting a library and checking
> it, and it is why `surface::Fingerprint` needs no public constructor. Count is 1260 of 1329
> compiled as of AR.27, not 742 — and the live figure is the FLOOR const in `fab_lib::emit`'s
> coverage ratchet, never a number in prose. This document stays as the dated design record it says it is; the live contract is
> `lang/src/registry.rs` and `lang/src/surface.rs`.

Declaring MODULES buys something before a single module is transpiled: the fuzzer can generate calls
against BOSL2's 416-module surface as soon as it is declared. That coverage is not gated on the
question of whether modules ever get natives.

## The registry accumulates, and is passed in

The dependency inversion is forced: `fab-lib-bosl2` depends on `fab-lang`, so `fab-lang` cannot
depend on it, so `intrinsics::REGISTRY` — a static that dispatch reads directly — has no home.

Two shapes were on the table: thread `Config` to every dispatch site, or install libraries once into
a global. chotchki picked neither: *"I think its reasonable for those modules to be expect to be
given a registry of loaded stuff that keeps getting built up"* — an ACCUMULATING registry the
consumer builds and passes in.

That is better than either, for a reason neither option weighed: **it composes**. BOSL2 plus
machineblocks plus a user's own library is the normal case, and both alternatives quietly assumed
exactly one library. It also kills the global-init ordering hazard outright instead of documenting
around it.

`Config::intrinsics` stays as the per-evaluation toggle — AR.2's differential has to turn natives off
WITHOUT changing which libraries are present, so the two are not the same knob.

The hard part is not the type. `table()`, `anchor_fp`, `entry_by_name` and `classify` are all reached
from call sites with no `Config` in hand, and `table()` in particular is a process-lifetime `OnceLock`
keyed on nothing — sound only while there is exactly one immutable registry in the process. Per-registry
caching, or none.

## The emission ABI is named, and it is small

Generated code used to reach `crate::eval::{ops, builtins, …}` and `super::{bosl_assert, native_rt}`,
which resolved only because `generated.rs` happened to live inside `eval`. It now names
`fab_lang::rt` and nothing else — **10 functions and 3 types**, all pure value algebra with no
evaluator context to thread.

Enforced by scanning the emitted TEXT, not by trusting the emitter: paths are decided in ~25 separate
format strings, and one of them reverting is a one-character diff the compiler accepts right up until
the file moves.

`extern crate self as fab_lang` makes that path resolve inside fab-lang too, so moving `generated.rs`
into its own crate cannot change a byte of what it moves. A move that rewrites what it moves is a
move nobody can diff.

Adding to `rt` is a deliberate act. Every addition widens what a generated native can do without
going through the value algebra, and the bit-identity argument rests entirely on it not doing that.

## What the library read found that 55 hand-written references could not

- **Zero drift.** All 52 registry entries that BOSL2 declares still fingerprint-match their
  transcribed reference. First real evidence for the maintenance thesis.
- **BOSL2 declares three names twice** at column 0 in one file: `_sort_vectors` (the known last-wins
  trap that already cost a bug), plus `_get_cp` and `_list_shape_recurse` — both same-arity, so
  almost certainly upstream dead code. A colliding name is REFUSED, not resolved: which body a user
  gets depends on their include graph, not ours.
- **An analyzer bug.** A C-style comprehension's update clause BINDS, it does not only reassign —
  `_dp_distance_array` introduces `newrow` there and reads it from two later update bindings. Walking
  update as plain args reported that loop variable as a free GLOBAL read; 19 phantom free names
  collapsed to 6. No hand reference uses that form.
- **An upstream bug.** `gear_shorten_skew` reads `helical` while declaring `helical1`/`helical2`, so
  it returns `undef`. Five functions read names nothing declares, which also means those names still
  need a GUARD rather than a bake — a user can define the missing name at top level and change what
  the function means.

## Still open

- **Modules.** "A fully working copy of BOSL2" — the 1335 functions, or the 416 modules too? Every
  existing intrinsic is a pure function, so the deletion of the JIT and the intrinsics is satisfiable
  on FUNCTION parity alone. Modules need `children()`, the `$`-var dynamic chain and the attachment
  stack, which is evaluator context the emission model deliberately has none of. Recommendation:
  functions first, modules as their own phase.
- **The fallback island does not scale.** AR.10's decline-to-interpreter path interprets a
  `FALLBACK_SOURCES` string holding the batch's verbatim references, cached per THREAD keyed on the
  string's `(ptr, len)`. At 14 functions that is 4 KB nobody notices; at library scale it is all 85K
  lines of BOSL2, so the first depth-exceeded call on each worker pays a full library parse and every
  thread then holds its own AST. Three ways out and they are not equivalent: split per generated
  module (bounded, but a declining function's deps can live in another file), share one parse behind a
  lock (contention on the path that is already the slow one), or drop the embedded copy and interpret
  against the USER's island (free, but gives up the guarantee that interpreted bindings equal the
  bakes bit-for-bit, which is the whole reason the copy is embedded).
