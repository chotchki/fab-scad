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
