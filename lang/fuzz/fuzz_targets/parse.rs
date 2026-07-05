//! Fuzz the parser: ANY input bytes → `parse` RETURNS (Ok or a typed Err), never panics, hangs, or
//! overflows the stack — and the resulting AST DROPS without overflowing either (the non-recursive
//! `Drop` handles whatever deep-left-chain the fuzzer discovers). This is the "bytes → no panic, no
//! hang" doctrine (SPEC), running from the first parser commit.
//!
//! Deliberately parse-ONLY: a deep-left-chain (`1+1+…`) parses fine (iteratively) but would overflow
//! the RECURSIVE pretty-printer, so a print-roundtrip oracle here would false-positive on a known,
//! bounded printer limitation rather than a real parser bug.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        // Drop of the returned Program (on Ok) exercises the non-recursive teardown.
        let _ = fab_lang::parse(src);
    }
});
