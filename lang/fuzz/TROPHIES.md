# Fuzz trophies

Every crash the fuzzer finds gets a row here — the doctrine (SPEC) is "bytes → no panic, no hang,"
so any crash is a real defect, and the fix isn't done until the reproducer is a committed regression
test. This log IS the paper trail: what broke, what class it was, where the regression test lives.

## How a trophy is processed

1. The nightly `fuzz` job (or a local `cargo +nightly fuzz run <target>`) crashes → libFuzzer writes
   the input to `lang/fuzz/artifacts/<target>/crash-<hash>`.
2. Reproduce + minimize:
   `cargo +nightly fuzz run <target> lang/fuzz/artifacts/<target>/crash-<hash>` then
   `cargo +nightly fuzz tmin <target> <crash-file>`.
3. Fix the defect in `lang/src/`.
4. Pin it: add the (minimized) input as a case in `parser_corpus::never_panics_on_adversarial_input`
   (or the lexer's equivalent), so the class can never regress silently.
5. Add the reduced input to the seed corpus (`lang/fuzz/corpus/<target>/`) and log the row below.

## Log

_None yet._ The parser survived its first campaign (1.82M executions, ~59k/sec, zero crashes) — the
`MAX_DEPTH` guards + the non-recursive `Drop` hold the "no panic, no hang, no overflow" line. Rows
land here as the nightly campaign accrues runtime.

| Date | Target | Class | Reproducer (minimized) | Regression test | Fix |
|------|--------|-------|------------------------|-----------------|-----|
| 2026-07-09 | eval | timeout (bounded-slow, NOT a memory bug) | `x=[for(i=[0:9e9])0];` — a range whose length is capped at `RANGE_MAX` (10M), so eval builds a 10-million-element list in >10s | _pending_ | _pending — needs a GLOBAL eval budget (Q.5); a single 10M comprehension is under any per-range cap yet still 10s, and a low global cap would break real high-`$fn` geometry, so the limit is a design call, not a mechanical fix_ |
| 2026-07-09 | jit_diff | OOM in the JIT COMPILER (NOT a divergence) | a ~2KB non-numeric body (`debug_tetra`-style: a wide nested list studded with 60-digit literals like `666…663`) — `compile_function` allocates hugely ANALYZING it before declining the non-numeric body | _pending_ | harness-guarded (skip inputs >4KB OR with a ≥12-digit literal run — no real code has either); ROOT fix is a compile-complexity budget in `fab-jit` so `compile_function` rejects a pathological body cheaply instead of OOMing (a JIT-side facet of Q.5) |
| **2026-07-09** | **jit_diff** | **REAL DIVERGENCE — JIT ≠ interpreter (doctrine #36), the first CORRECTNESS bug the campaign found** | **`function f(s) = (-s)*(-s);` at `s = NaN`: interp = `0xfff8000000000000`, JIT = `0x7ff8000000000000` — both NaN, different SIGN BIT.** Minimized: needs BOTH the negate AND the multiply (`s*s`, `-s`, `-(s*s)` all MATCH). Root: the JIT's `fmul` (Cranelift→ARM) canonicalizes a signed NaN to the default sign-clear NaN, while the interpreter's `f64::mul` PRESERVES the operand NaN's sign — the `-s` sets a sign bit that only the multiply reveals. | _pending_ (repro `gen`/corpus below) | **task filed — fix the JIT/interp NaN-payload divergence; a determinism-phase decision on the canonical-NaN convention (which tier wins), then make both agree. Relates to [[libm-transcendental-divergence]].** NaN quarantined out of the jit_diff sample battery meanwhile so the campaign keeps hunting OTHER classes. |

**Note on the first eval findings:** the eval campaign surfaced the SAME class in two flavors within ~15 min — a *timeout* (`[0:9e9]` → a 10M-element list in >10s) and an *OOM* (`[0:3.9e83]` with large elements → >2GB RSS). Both TERMINATE (the `RANGE_MAX` guard bounds iteration count) — they're not memory-unsafety, the "no panic/overflow" line held — but a single comprehension can still burn 10s or 2GB. `-fork=1 -ignore_timeouts=1 -ignore_ooms=1` did NOT keep the campaign alive (the RSS hard-limit aborts regardless), which confirms the point: this is a MISSING-BUDGET problem, not a fuzzer-flag problem. The eval campaign is therefore PARKED pending **Q.5 (a global eval iteration/memory budget)** — a real design call, since a low cap would break legitimate high-`$fn` geometry (millions of vertices).

**The same class hit jit_diff too** — but on the JIT COMPILE side (a pathological body OOMs `compile_function` before it declines), not the eval side. Two cheap harness guards (skip inputs >4KB or with a ≥12-digit literal run) keep jit_diff + the nightly CI job alive; the root fix is a compile-complexity budget in `fab-jit` (the Q.5 facet noted in the table). **The bottom line across the whole campaign:** no panics/overflows (the memory-safety line held), but jit_diff DID find a REAL interp≠JIT divergence — the NaN-sign-through-`fmul` bug above, a doctrine-#36 violation the bounded `fast_eq_jit` proptest missed. That's the differential fuzzer earning its keep: the property it exists to enforce (bit-identity across tiers) had a genuine gap, and any-input fuzzing surfaced it where a fixed sample battery didn't. Everything else was resource-exhaustion (→ Q.5). The K.3 generator sidesteps the resource class by construction (bounded ranges + literal sizes) and its differential label caught 0 mismatches on the 50k corpus only because none of those 12.6k JIT-eligible bodies happened to hit the `(-NaN)²` shape — it's a rare pattern, which is exactly why fuzzing (not sampling) found it.
