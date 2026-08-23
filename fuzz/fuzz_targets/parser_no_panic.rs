//! Never panics or hangs on arbitrary bytes -- that's the entire property
//! under test. Exercises all three entry points, since each has its own
//! grammar dispatch, speculative-parse/rollback paths, and recovery
//! logic that could in principle diverge.
//!
//! Requires nightly + `cargo-fuzz` (`cargo install cargo-fuzz`), neither
//! of which is set up in this repo's primary (Windows) dev environment --
//! native Windows libFuzzer support is unreliable, so this is meant to
//! run via CI or WSL/Linux, not necessarily on a contributor's machine.
//! Seed `fuzz/corpus/parser_no_panic/` from `tests/corpus/` real-world
//! files before a real run, so mutation starts from syntactically-close
//! input rather than pure noise:
//!
//!   mkdir -p fuzz/corpus/parser_no_panic
//!   find tests/corpus/npsp -name '*.cls' -exec cp {} fuzz/corpus/parser_no_panic/ \;
//!   cargo +nightly fuzz run parser_no_panic

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let _ = apex_parser::parse_expression(src);
    let _ = apex_parser::parse_statement(src);
    let _ = apex_parser::parse_block(src);
});
