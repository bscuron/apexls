# Fuzz targets

`fuzz/Cargo.toml` + `fuzz_targets/parser_no_panic.rs` are hand-written
(matching `cargo-fuzz init`'s standard layout) but **not run or verified
locally** -- this repo's primary dev environment is Windows, where native
`cargo-fuzz`/libFuzzer support is unreliable, and no nightly toolchain is
installed. Run this via CI (Linux) or WSL:

```
cargo install cargo-fuzz
mkdir -p fuzz/corpus/parser_no_panic
find tests/corpus/npsp -name '*.cls' -exec cp {} fuzz/corpus/parser_no_panic/ \;
cargo +nightly fuzz run parser_no_panic
```

Status of the targets named in the original roadmap:

- `parser_no_panic` — done (above). Arbitrary bytes must never
  panic/infinite-loop any of the three parser entry points.
- `lexer_no_panic` — not yet stood up; the lexer's round-trip test against
  the full NPSP corpus (`crates/apex-lexer/tests/roundtrip.rs`) has been
  the primary source of confidence so far, but a dedicated fuzz target
  would still catch panics on inputs no real-world file happens to
  contain.
- `parser_roundtrip` — not yet stood up as a *fuzz* target; covered today
  by `crates/apex-parser/tests/roundtrip.rs` (extracted NPSP fragments,
  not fuzzer-mutated input) and the proptest-based parenthesization
  metamorphic test.
- `differential_fuzz` — blocked on the `oracle/` harness (adapters not
  written yet); out of scope until that exists.
