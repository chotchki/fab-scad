//! Fuzz the lexer: ANY input bytes → `lex` RETURNS (Ok token stream or a typed Err), never panics or
//! hangs. The lexer is the first line of the "bytes → no panic, no hang" doctrine (SPEC) — it steps
//! `char` by `char` over valid UTF-8, so the fuzzer probes the escape/number/comment/unicode edges.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        let _ = fab_lang::lex(src);
    }
});
