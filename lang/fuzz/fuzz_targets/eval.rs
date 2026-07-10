//! Fuzz the EVALUATOR: ANY input bytes → parse → eval → geometry → mesh RETURNS (Ok or a typed Err),
//! never panics, hangs, or overflows the stack. Where `parse` fuzzes bytes → AST, this fuzzes the whole
//! tier BELOW it — the value machinery (`ops`/comprehensions/`Scope`/closures), the module + geometry
//! tree, AND the two iterative `Drop` paths (`ValueList` and the geo tree) that M.1 rewrote to be
//! heap-bounded. Under cargo-fuzz's ASan this is the memory-safety checker miri can't be for the parts
//! it CAN'T reach on this crate (there's no `unsafe` in `fab-lang`, but ASan + a fuzzer still catch
//! leaks, and the "no panic / no overflow / no hang" doctrine is the real gate here).
//!
//! Hermetic: fuzzer-generated source has no `include`/`use`, so no filesystem is touched. A program that
//! DOES name an include just fails to resolve (a typed Err), not a crash. Iteration is capped in the
//! evaluator itself (`RANGE_MAX`, `MAX_DEPTH`); pair with a libFuzzer `-timeout` for the rare slow unit.
//! Seed from the `parse` corpus (`cargo fuzz run eval -- lang/fuzz/corpus/parse`) so it starts on the
//! parseable programs the parser campaign already discovered.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        // Full pipeline. Drop of the returned Mesh/Geo (on Ok) and of every intermediate value tree
        // exercises the non-recursive teardown on whatever deep nesting the fuzzer finds.
        let _ = fab_lang::evaluate(src);
    }
});
