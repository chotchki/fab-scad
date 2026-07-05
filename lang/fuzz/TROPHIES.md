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
| — | — | — | — | — | — |
