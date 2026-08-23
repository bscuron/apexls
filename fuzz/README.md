# Fuzz targets

Not yet initialized with `cargo-fuzz` (requires a nightly toolchain). Once
the lexer exists, run `cargo install cargo-fuzz && cargo fuzz init` from
this directory and add targets matching the roadmap:

- `lexer_no_panic` — arbitrary bytes never panic/infinite-loop the lexer
- `parser_no_panic` — arbitrary bytes never panic/infinite-loop the parser
- `parser_roundtrip` — `render(parse(x)) == x` for generated/mutated inputs
- `differential_fuzz` — mutate corpus seeds, compare apexls vs. oracle
  accept/reject via `oracle/`

Seed the corpus (`fuzz/corpus/`) from `tests/corpus/` real-world files
rather than pure noise, so mutation starts from syntactically-close inputs.
