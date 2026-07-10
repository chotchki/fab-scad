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

**Note on the first eval findings:** the eval campaign surfaced the SAME class in two flavors within ~15 min — a *timeout* (`[0:9e9]` → a 10M-element list in >10s) and an *OOM* (`[0:3.9e83]` with large elements → >2GB RSS). Both TERMINATE (the `RANGE_MAX` guard bounds iteration count) — they're not memory-unsafety, the "no panic/overflow" line held — but a single comprehension can still burn 10s or 2GB. `-fork=1 -ignore_timeouts=1 -ignore_ooms=1` did NOT keep the campaign alive (the RSS hard-limit aborts regardless), which confirms the point: this is a MISSING-BUDGET problem, not a fuzzer-flag problem. The eval campaign is therefore PARKED pending **Q.5 (a global eval iteration/memory budget)** — a real design call, since a low cap would break legitimate high-`$fn` geometry (millions of vertices). jit_diff (numeric-function-only, so it never builds these comprehensions) keeps running clean and carries the memory-safety + bit-identity hunt. The K.3 generator sidesteps the class by construction (it bounds range magnitudes).
